//! The ONNX session, behind a trait so the tests can drive the service with a
//! fake one and no weights on disk.
//!
//! THE MODEL IS THE DAEMON'S, EXACTLY. Every vector this service returns lands
//! in a tenant's `message_vecs` beside vectors that tenant's daemon computed
//! locally before the switch, and a query vector from here is compared against
//! both. So the session is built the way `squelch-core/src/embed/fastembed_impl.rs`
//! builds it for its default model: the same fastembed variant, the same
//! `max_length`, fastembed's own defaults for everything else (graph
//! optimisation, intra-op threads, pooling, normalisation). Anything set here
//! that the daemon does not set is a way for the two to drift.
//!
//! Which variant: the daemon resolves `protocol::MODEL_CODE` through
//! `resolve_model`'s exact-code path, which lands on
//! [`EmbeddingModel::BGESmallENV15`] (fp32, `onnx/model.onnx`) and never on
//! `BGESmallENV15Q`, the int8 build fastembed ships under a near-identical
//! code. This file names the variant directly rather than repeating the
//! lookup, and a test pins that the variant's code, dimension and file are the
//! ones the protocol and the daemon's resolver agree on.
//!
//! NO DOWNLOAD, EVER. The image bakes the weights at build time and the pod
//! has no egress, so a missing file must be a refusal to start with the file
//! named, not a fetch that hangs until a timeout and then says something about
//! DNS. [`load_fastembed`] asks the hf-hub cache for every file the session
//! reads before fastembed is called, which is the same lookup fastembed makes
//! first: with every file present it never reaches the network.

use std::path::{Path, PathBuf};

use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

use crate::protocol;

/// One forward pass. `&mut` because fastembed's `embed` is, and because the
/// one inference thread owns the session outright.
pub trait Session: Send {
    /// `vectors[i]` for `texts[i]`. The coalescer checks the count and every
    /// width, so an implementation does not need to.
    fn embed(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, SessionError>;
}

/// Inference failed. Deliberately carries nothing: an error from the tokenizer
/// or the runtime is formatted by code that has the text in hand, and nothing
/// derived from a text may reach a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionError;

/// Builds the session, on the inference thread. A string error because it is
/// a startup failure, about files and model tables, and goes to the log once
/// before the process exits.
pub type Loader = Box<dyn FnOnce() -> Result<Box<dyn Session>, String> + Send>;

/// The fastembed variant. See the module doc for why this one.
pub fn model() -> EmbeddingModel {
    EmbeddingModel::BGESmallENV15
}

/// The four tokenizer files fastembed reads beside the model file
/// (`load_tokenizer_hf_hub`, fastembed 7.5 `src/common.rs`). Not exported by
/// fastembed, so restated here, and restated again in `Dockerfile.embed`, which
/// fetches them; a test reads that Dockerfile and fails if the two disagree.
pub const TOKENIZER_FILES: [&str; 4] = [
    "tokenizer.json",
    "config.json",
    "special_tokens_map.json",
    "tokenizer_config.json",
];

/// The model as fastembed describes it: (model_code, dim, every file the
/// session reads, in the repo's own paths).
pub fn model_files() -> Result<(String, usize, Vec<String>), String> {
    let model = model();
    let info = TextEmbedding::get_model_info(&model).map_err(|e| format!("model info: {e}"))?;
    let mut files = vec![info.model_file.clone()];
    files.extend(info.additional_files.iter().cloned());
    files.extend(TOKENIZER_FILES.iter().map(|f| (*f).to_string()));
    Ok((info.model_code.clone(), info.dim, files))
}

/// Refuse a model that is not the protocol's. Cheap, and run at startup,
/// because a fastembed bump that renumbers or re-points the variant would
/// otherwise serve 384 floats in the wrong space without a word.
pub fn check_model() -> Result<Vec<String>, String> {
    let (code, dim, files) = model_files()?;
    if code != protocol::MODEL_CODE {
        return Err(format!(
            "fastembed's {:?} is model_code {code}, but the protocol pins {}",
            model(),
            protocol::MODEL_CODE
        ));
    }
    if dim != protocol::DIMS {
        return Err(format!(
            "model {code} has dim {dim}, but the protocol pins {}",
            protocol::DIMS
        ));
    }
    Ok(files)
}

/// Which of `files` the hf-hub cache at `cache_dir` cannot resolve for the
/// protocol's model. Asks the snapshot pointer, the lookup fastembed itself
/// makes, rather than the directory listing (squelch-core's
/// `model_appears_cached` has the history of why a listing lies).
pub fn missing_files(cache_dir: &Path, files: &[String]) -> Vec<String> {
    let repo = hf_hub::Cache::new(cache_dir.to_path_buf()).model(protocol::MODEL_CODE.to_string());
    files
        .iter()
        .filter(|f| repo.get(f).is_none())
        .cloned()
        .collect()
}

/// Build the real session from the baked weights in `cache_dir`.
pub fn load_fastembed(cache_dir: PathBuf) -> Result<Box<dyn Session>, String> {
    // fastembed's downloader reads HF_HOME in preference to the cache dir it
    // is handed, so a stray HF_HOME would point it at an empty directory and
    // turn the check below into a lie.
    if std::env::var_os("HF_HOME").is_some() {
        return Err(
            "HF_HOME is set; unset it. The weights directory is SQUELCH_EMBED_MODEL_DIR, \
             and fastembed would otherwise look in HF_HOME instead"
                .to_string(),
        );
    }
    let files = check_model()?;
    let missing = missing_files(&cache_dir, &files);
    if !missing.is_empty() {
        return Err(format!(
            "weights for {} are not in {}: missing {}. They are baked into the image at \
             build time (Dockerfile.embed) and this service never downloads",
            protocol::MODEL_CODE,
            cache_dir.display(),
            missing.join(", ")
        ));
    }

    // The daemon's options, and nothing else: model, cache dir, max_length.
    // Download progress off because there is no download.
    let opts = TextInitOptions::new(model())
        .with_cache_dir(cache_dir)
        .with_show_download_progress(false)
        .with_max_length(protocol::MAX_TOKENS);
    let embedding = TextEmbedding::try_new(opts).map_err(|e| format!("fastembed init: {e}"))?;
    Ok(Box::new(FastSession(embedding)))
}

struct FastSession(TextEmbedding);

impl Session for FastSession {
    fn embed(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, SessionError> {
        // `None` is the daemon's batch size too, and at most eight texts
        // arrive here, so this is one ORT run whatever fastembed's default.
        self.0.embed(texts, None).map_err(|_| SessionError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The variant this service loads is the one the daemon's resolver picks
    /// for `protocol::MODEL_CODE`: same code, same width, same weights file.
    /// (squelch-core's `default_code_resolves_to_fp32` pins the daemon half.)
    #[test]
    fn the_variant_is_the_protocols_fp32_build() {
        let (code, dim, files) = model_files().unwrap();
        assert_eq!(code, protocol::MODEL_CODE);
        assert_eq!(dim, protocol::DIMS);
        assert_eq!(
            files[0], "onnx/model.onnx",
            "the fp32 file, not the int8 one"
        );
        assert!(check_model().is_ok());
    }

    fn seed(cache_dir: &Path, files: &[String]) {
        let repo = cache_dir.join(format!(
            "models--{}",
            protocol::MODEL_CODE.replace('/', "--")
        ));
        std::fs::create_dir_all(repo.join("refs")).unwrap();
        std::fs::write(repo.join("refs").join("main"), b"c0ffee").unwrap();
        for f in files {
            let p = repo.join("snapshots").join("c0ffee").join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"x").unwrap();
        }
    }

    /// An empty weights dir is a startup refusal naming every missing file,
    /// and the check runs before fastembed could try a download.
    #[test]
    fn absent_weights_refuse_to_load_and_name_the_files() {
        let dir =
            std::env::temp_dir().join(format!("squelch-embed-weights-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let files = check_model().unwrap();

        assert_eq!(missing_files(&dir, &files), files);
        let err = match load_fastembed(dir.clone()) {
            Ok(_) => panic!("loaded with no weights"),
            Err(e) => e,
        };
        if std::env::var_os("HF_HOME").is_none() {
            assert!(err.contains("never downloads"), "{err}");
            assert!(err.contains("onnx/model.onnx"), "{err}");
        }

        // All but one present: exactly that one is named.
        seed(&dir, &files[1..]);
        assert_eq!(missing_files(&dir, &files), vec![files[0].clone()]);
        seed(&dir, &files);
        assert!(missing_files(&dir, &files).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `Dockerfile.embed` fetches the weights with curl, not with this binary
    /// (the image cross-compiles, and a cross-built binary cannot run in the
    /// build). So the file list lives in two places, and this is what keeps
    /// them one list.
    #[test]
    fn the_dockerfile_bakes_every_file_the_session_reads() {
        let dockerfile = include_str!("../../Dockerfile.embed");
        assert!(dockerfile.contains(protocol::MODEL_CODE));
        for f in check_model().unwrap() {
            assert!(
                dockerfile.contains(&f),
                "Dockerfile.embed does not fetch {f}"
            );
        }
    }

    /// The real model, end to end. Needs the weights on disk, so it is
    /// ignored by default:
    ///
    /// ```sh
    /// SQUELCH_EMBED_MODEL_DIR=/path/to/cache cargo test -p squelch-embed -- --ignored
    /// ```
    ///
    /// The cache is any hf-hub layout holding the fp32 build, for instance a
    /// daemon's `~/.local/share/squelch/models`. It builds a second session
    /// the way squelch-core does for its default model and requires the
    /// service's vectors to be bit-identical to it.
    #[test]
    #[ignore = "needs the model weights on disk (SQUELCH_EMBED_MODEL_DIR)"]
    fn real_model_matches_the_daemons_session() {
        let dir = PathBuf::from(
            std::env::var("SQUELCH_EMBED_MODEL_DIR").expect("set SQUELCH_EMBED_MODEL_DIR"),
        );
        let mut service = load_fastembed(dir.clone()).unwrap();
        let mut daemon = TextEmbedding::try_new(
            TextInitOptions::new(EmbeddingModel::BGESmallENV15)
                .with_cache_dir(dir)
                .with_show_download_progress(false)
                .with_max_length(256),
        )
        .unwrap();
        let texts: Vec<String> = vec![
            "Your order has shipped\n\nTracking number 1Z999".into(),
            "Lunch on Thursday?".into(),
            "x".repeat(5000),
        ];
        let a = service.embed(&texts).unwrap();
        let b = daemon.embed(&texts, None).unwrap();
        assert_eq!(a.len(), 3);
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.len(), protocol::DIMS);
            assert_eq!(x, y, "service and daemon sessions disagree");
            let norm: f32 = x.iter().map(|v| v * v).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 1e-3, "not normalised: {norm}");
        }
    }
}
