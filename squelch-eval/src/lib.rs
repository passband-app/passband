//! Offline triage evaluation: replay historical mail through the production
//! agent with a candidate model and compare it to the stored decisions.
//!
//! PRIVACY: a dataset is the owner's own mail. It lives under
//! [`default_root`] (dirs 0700, files 0600) and never inside a repository.
pub mod asof;
pub mod case;
pub mod export;
#[cfg(test)]
mod fixture;
pub mod pricing;
pub mod report;
pub mod run;
pub mod score;
pub mod textsim;

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// `~/.local/share/squelch/evals`.
pub fn default_root() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".local/share/squelch/evals"),
        None => PathBuf::from("squelch-evals"),
    }
}

/// Create `dir` (and parents) and restrict it to the owner.
pub fn private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    set_mode(dir, 0o700)
}

/// Write `bytes` to `path` readable only by the owner.
pub fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))?;
    set_mode(path, 0o600)
}

pub fn set_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .with_context(|| format!("chmod {}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

/// Refuse a dataset location inside a git working tree: the data is personal
/// mail and must never be one `git add` away from a commit.
pub fn refuse_repo_path(path: &Path) -> Result<()> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    // Walk the nearest existing ancestor upward; the leaf may not exist yet.
    let mut probe = Some(absolute.as_path());
    while let Some(dir) = probe {
        if let Ok(real) = dir.canonicalize() {
            let mut up = Some(real.as_path());
            while let Some(candidate) = up {
                if candidate.join(".git").exists() {
                    bail!(
                        "refusing to write eval data inside the repository at {}; use a path outside it (default {})",
                        candidate.display(),
                        default_root().display()
                    );
                }
                up = candidate.parent();
            }
            return Ok(());
        }
        probe = dir.parent();
    }
    Ok(())
}

/// splitmix64: a tiny, seedable PRNG for sampling and bootstrap. Deterministic
/// across platforms, which is what makes a seeded sample reproducible.
#[derive(Debug, Clone)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `0..n` (n > 0), by rejection so there is no modulo bias.
    pub fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0, "empty range");
        let zone = u64::MAX - u64::MAX % n;
        loop {
            let value = self.next_u64();
            if value < zone {
                return value % n;
            }
        }
    }
    /// Fisher-Yates.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i as u64 + 1) as usize;
            items.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix_matches_reference_and_is_seeded() {
        // Reference values for seed 1234567 from the splitmix64 paper's C code.
        let mut rng = SplitMix64::new(1234567);
        assert_eq!(rng.next_u64(), 6457827717110365317);
        assert_eq!(rng.next_u64(), 3203168211198807973);
        let mut a = (0..50).collect::<Vec<_>>();
        let mut b = a.clone();
        SplitMix64::new(7).shuffle(&mut a);
        SplitMix64::new(7).shuffle(&mut b);
        assert_eq!(a, b);
        assert_ne!(a, (0..50).collect::<Vec<_>>());
    }

    #[test]
    fn dataset_paths_inside_a_repository_are_refused() {
        let repo = std::env::temp_dir().join(format!("squelch-eval-repo-{}", std::process::id()));
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert!(refuse_repo_path(&repo.join("evals/new")).is_err());
        let outside = std::env::temp_dir().join(format!("squelch-eval-out-{}", std::process::id()));
        assert!(refuse_repo_path(&outside).is_ok());
        std::fs::remove_dir_all(repo).unwrap();
    }
}
