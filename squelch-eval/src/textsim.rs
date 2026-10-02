//! Text similarity for the `*_sim` metrics: the cosine of two embeddings
//! from the daemon's own on-box model, cached on disk per dataset so a second
//! `report` embeds nothing it has seen.
//!
//! The cache is trusted only when its header names the same model and width;
//! a corrupt or foreign cache is discarded whole, never partly read.
use crate::score::TextSimilarity;
use anyhow::{Context, Result, bail};
use squelch_core::embed::Embedder;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

pub const CACHE_FILE: &str = "embeddings.jsonl";
/// Texts per embedder call.
const BATCH: usize = 64;

/// Stable across runs and Rust versions (the std hasher is neither).
pub fn key(model: &str, text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in model.bytes().chain([0]).chain(text.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let dot: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum();
    let norm = |v: &[f32]| v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    let denominator = norm(a) * norm(b);
    if denominator == 0.0 {
        0.0
    } else {
        (dot / denominator).clamp(0.0, 1.0)
    }
}

/// A first pass that only collects the texts a scoring pass would compare,
/// so they can be embedded in batches before the real pass.
#[derive(Default)]
pub struct RecordingSimilarity {
    pub texts: RefCell<BTreeSet<String>>,
}

impl TextSimilarity for RecordingSimilarity {
    fn similarity(&self, a: &str, b: &str) -> f64 {
        let mut texts = self.texts.borrow_mut();
        texts.insert(a.to_owned());
        texts.insert(b.to_owned());
        0.0
    }
}

pub struct EmbeddingSimilarity {
    embedder: Box<dyn Embedder>,
    model: String,
    path: PathBuf,
    vectors: RefCell<HashMap<u64, Vec<f32>>>,
    /// What happened to the on-disk cache when it was opened.
    pub cache_note: String,
    failures: RefCell<usize>,
}

impl EmbeddingSimilarity {
    /// Load the cache at `path` if it is this model's; otherwise start empty.
    pub fn open(path: &Path, model: &str, embedder: Box<dyn Embedder>) -> Self {
        let (vectors, cache_note) = match load(path, model, embedder.dims()) {
            Ok(Some(vectors)) => {
                let note = format!("{} cached", vectors.len());
                (vectors, note)
            }
            Ok(None) => (HashMap::new(), "no cache yet".to_owned()),
            Err(error) => (HashMap::new(), format!("cache discarded: {error}")),
        };
        Self {
            embedder,
            model: model.to_owned(),
            path: path.to_path_buf(),
            vectors: RefCell::new(vectors),
            cache_note,
            failures: RefCell::new(0),
        }
    }

    /// Embed every text not yet cached, in batches. Returns how many.
    pub fn prefetch(&self, texts: impl IntoIterator<Item = String>) -> Result<usize> {
        let missing: Vec<String> = {
            let vectors = self.vectors.borrow();
            texts
                .into_iter()
                .filter(|t| !vectors.contains_key(&key(&self.model, t)))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        };
        for chunk in missing.chunks(BATCH) {
            let embedded = self
                .embedder
                .embed_batch(chunk)
                .map_err(|e| anyhow::anyhow!("embedding failed: {e}"))?;
            let mut vectors = self.vectors.borrow_mut();
            for (text, vector) in chunk.iter().zip(embedded) {
                vectors.insert(key(&self.model, text), vector);
            }
        }
        Ok(missing.len())
    }

    /// Write the cache (0600): a header line, then one vector per line.
    pub fn save(&self) -> Result<()> {
        let mut out =
            serde_json::json!({"model": self.model, "dims": self.embedder.dims()}).to_string();
        out.push('\n');
        let vectors = self.vectors.borrow();
        let mut keys: Vec<&u64> = vectors.keys().collect();
        keys.sort();
        for k in keys {
            out.push_str(
                &serde_json::json!({"k": format!("{k:016x}"), "v": vectors[k]}).to_string(),
            );
            out.push('\n');
        }
        crate::private_write(&self.path, out.as_bytes())
    }

    pub fn cached(&self) -> usize {
        self.vectors.borrow().len()
    }

    /// Texts that could not be embedded on demand (scored 0).
    pub fn failures(&self) -> usize {
        *self.failures.borrow()
    }

    fn vector(&self, text: &str) -> Option<Vec<f32>> {
        let k = key(&self.model, text);
        if let Some(vector) = self.vectors.borrow().get(&k) {
            return Some(vector.clone());
        }
        match self.embedder.embed(text) {
            Ok(vector) => {
                self.vectors.borrow_mut().insert(k, vector.clone());
                Some(vector)
            }
            Err(_) => {
                *self.failures.borrow_mut() += 1;
                None
            }
        }
    }
}

impl TextSimilarity for EmbeddingSimilarity {
    fn similarity(&self, a: &str, b: &str) -> f64 {
        match (self.vector(a), self.vector(b)) {
            (Some(a), Some(b)) => cosine(&a, &b),
            _ => 0.0,
        }
    }
}

/// `Ok(None)` when there is no cache file; `Err` when there is one and it
/// cannot be trusted.
fn load(path: &Path, model: &str, dims: usize) -> Result<Option<HashMap<u64, Vec<f32>>>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    let mut lines = text.lines();
    let header: serde_json::Value =
        serde_json::from_str(lines.next().context("empty cache")?).context("unreadable header")?;
    if header["model"] != serde_json::json!(model) || header["dims"] != serde_json::json!(dims) {
        bail!(
            "built by {} ({} dims), not {model} ({dims} dims)",
            header["model"],
            header["dims"]
        );
    }
    let mut vectors = HashMap::new();
    for (index, line) in lines.enumerate() {
        let entry: serde_json::Value =
            serde_json::from_str(line).with_context(|| format!("line {}", index + 2))?;
        let k = entry["k"]
            .as_str()
            .and_then(|k| u64::from_str_radix(k, 16).ok())
            .with_context(|| format!("line {} key", index + 2))?;
        let v: Vec<f32> = serde_json::from_value(entry["v"].clone())
            .with_context(|| format!("line {} vector", index + 2))?;
        if v.len() != dims {
            bail!("line {} has {} dims", index + 2, v.len());
        }
        vectors.insert(k, v);
    }
    Ok(Some(vectors))
}

/// The daemon's embedder, if its weights are already on this machine. Never
/// downloads: an eval report should not fetch a model as a side effect.
pub fn production() -> Result<(Box<dyn Embedder>, String)> {
    let settings = squelch_core::config::EmbedConfig::default().settings();
    let has_weights = std::fs::read_dir(&settings.cache_dir)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false);
    if !has_weights {
        bail!(
            "no embedding weights at {} (the daemon downloads them on first run)",
            settings.cache_dir.display()
        );
    }
    let embedder = squelch_core::embed::FastEmbedder::new(&settings)
        .map_err(|e| anyhow::anyhow!("the embedding model would not load: {e}"))?;
    Ok((Box::new(embedder), settings.model_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use squelch_core::embed::StubEmbedder;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counts embedder calls through to a stub.
    struct Counting(StubEmbedder, Arc<AtomicUsize>);
    impl Embedder for Counting {
        fn embed(&self, text: &str) -> squelch_core::error::Result<Vec<f32>> {
            self.1.fetch_add(1, Ordering::SeqCst);
            self.0.embed(text)
        }
        fn dims(&self) -> usize {
            self.0.dims()
        }
    }

    fn counting(dims: usize) -> (Box<dyn Embedder>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Box::new(Counting(StubEmbedder::new(dims), calls.clone())),
            calls,
        )
    }

    #[test]
    fn the_cache_is_reused_and_only_for_its_own_model() {
        let dir = crate::fixture::temp_dir("embeddings");
        let path = dir.join(CACHE_FILE);
        let (embedder, calls) = counting(32);
        let sim = EmbeddingSimilarity::open(&path, "model-a", embedder);
        assert_eq!(sim.cache_note, "no cache yet");
        assert_eq!(
            sim.prefetch(["pay the bill".into(), "bill payment due".into()])
                .unwrap(),
            2
        );
        let first = sim.similarity("pay the bill", "bill payment due");
        assert!(first > 0.0 && first < 1.0);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "prefetched, not embedded again"
        );
        sim.save().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        // Hit: a new process embeds nothing it has seen.
        let (embedder, calls) = counting(32);
        let again = EmbeddingSimilarity::open(&path, "model-a", embedder);
        assert_eq!(again.cache_note, "2 cached");
        assert_eq!(again.prefetch(["pay the bill".into()]).unwrap(), 0);
        assert_eq!(again.similarity("pay the bill", "bill payment due"), first);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        // Miss: a new text is embedded once.
        again.similarity("pay the bill", "something new");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Another model's cache is discarded whole.
        let (embedder, calls) = counting(32);
        let other = EmbeddingSimilarity::open(&path, "model-b", embedder);
        assert!(
            other.cache_note.starts_with("cache discarded"),
            "{}",
            other.cache_note
        );
        assert_eq!(other.cached(), 0);
        other.similarity("pay the bill", "bill payment due");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // So is one with the wrong width, and a corrupt one.
        let (embedder, _) = counting(16);
        assert!(
            EmbeddingSimilarity::open(&path, "model-a", embedder)
                .cache_note
                .starts_with("cache discarded")
        );
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{text}{{\"k\": \"00ff\", \"v\": [0.5]}}\n")).unwrap();
        let (embedder, _) = counting(32);
        let short = EmbeddingSimilarity::open(&path, "model-a", embedder);
        assert!(short.cache_note.contains("dims"), "{}", short.cache_note);
        std::fs::write(&path, format!("{text}{{\"k\": \"zz\", \"v\": []}}\n")).unwrap();
        let (embedder, _) = counting(32);
        let corrupt = EmbeddingSimilarity::open(&path, "model-a", embedder);
        assert!(
            corrupt.cache_note.starts_with("cache discarded"),
            "{}",
            corrupt.cache_note
        );
        assert_eq!(corrupt.cached(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cosine_and_keys() {
        assert_eq!(cosine(&[1.0, 0.0], &[1.0, 0.0]), 1.0);
        assert_eq!(cosine(&[1.0, 0.0], &[-1.0, 0.0]), 0.0, "clamped");
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
        assert_ne!(key("m", "a"), key("n", "a"));
        assert_ne!(key("m", "a"), key("m", "b"));
        assert_ne!(
            key("ma", ""),
            key("m", "a"),
            "the separator keeps fields apart"
        );
    }
}
