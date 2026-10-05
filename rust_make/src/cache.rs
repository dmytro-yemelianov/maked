use crate::hash::{Sha256, sha256_file, to_hex};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct CacheConfig {
    pub enabled: bool,
    pub cache_dir: PathBuf,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cache_dir: PathBuf::from(".maked_cache"),
        }
    }
}

pub struct ContentAddressableCache {
    pub config: CacheConfig,
}

impl ContentAddressableCache {
    pub fn new(config: CacheConfig) -> Self {
        if config.enabled {
            let _ = fs::create_dir_all(&config.cache_dir);
            let _ = fs::create_dir_all(config.cache_dir.join("cas"));
        }
        Self { config }
    }

    /// Computes the content-addressable cache key:
    /// Key = SHA256( target_name + "\n" + recipe_commands + "\n" + sorted(prereq_name:prereq_hash) )
    pub fn compute_cache_key(target: &str, commands: &[String], prereqs: &[String]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(target.as_bytes());
        hasher.update(b"\n");

        for cmd in commands {
            hasher.update(cmd.as_bytes());
            hasher.update(b"\n");
        }

        // Sort prerequisites to ensure deterministic hashing regardless of declared order
        let mut sorted_prereqs = prereqs.to_vec();
        sorted_prereqs.sort();

        for p in sorted_prereqs {
            hasher.update(p.as_bytes());
            hasher.update(b":");
            if let Ok(h) = sha256_file(&p) {
                hasher.update(&h);
            } else {
                hasher.update(b"missing");
            }
            hasher.update(b"\n");
        }

        to_hex(&hasher.finalize())
    }

    /// Look up artifact in CAS; if present, restore to destination path and return true
    pub fn restore_artifact(&self, key: &str, dest_path: &str) -> bool {
        if !self.config.enabled {
            return false;
        }
        let cas_path = self.config.cache_dir.join("cas").join(key);
        if cas_path.exists() {
            if let Some(parent) = Path::new(dest_path).parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = fs::create_dir_all(parent);
                }
            }
            if fs::copy(&cas_path, dest_path).is_ok() {
                return true;
            }
        }
        false
    }

    /// Store a built artifact in CAS under the given key
    pub fn store_artifact(&self, key: &str, src_path: &str) -> bool {
        if !self.config.enabled {
            return false;
        }
        let src = Path::new(src_path);
        if !src.exists() {
            return false;
        }
        let cas_dir = self.config.cache_dir.join("cas");
        let cas_path = cas_dir.join(key);
        let _ = fs::create_dir_all(&cas_dir);
        fs::copy(src, &cas_path).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cas_cache_restore_and_keying() {
        let temp_dir = std::env::temp_dir().join(format!("maked_test_cas_{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);

        let cache = ContentAddressableCache::new(CacheConfig {
            enabled: true,
            cache_dir: temp_dir.clone(),
        });

        let target = temp_dir.join("output.o");
        let target_str = target.to_str().unwrap();

        // Create dummy artifact
        let _ = fs::create_dir_all(&temp_dir);
        fs::write(&target, b"compiled object code").unwrap();

        let cmds = vec!["gcc -c src.c -o output.o".to_string()];
        let prereqs = vec![];
        let key = ContentAddressableCache::compute_cache_key(target_str, &cmds, &prereqs);

        // Store into CAS
        assert!(cache.store_artifact(&key, target_str));

        // Delete artifact from workspace
        fs::remove_file(&target).unwrap();
        assert!(!target.exists());

        // Restore from CAS
        assert!(cache.restore_artifact(&key, target_str));
        assert!(target.exists());
        assert_eq!(fs::read(&target).unwrap(), b"compiled object code");

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
