use std::path::PathBuf;

use crate::{AppResult, misc::paths::cache_path};
use bytesize::ByteSize;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DuckDBConfig {
    /// alloted ram for the duckdb to work with.
    alloted_ram: Option<ByteSize>,

    /// the directory in which the cache files will be generated and stored.
    cache_dir: PathBuf,

    /// the maximum cache size.
    /// the system will try to do a LRU eviction on the files based on the age.
    /// if the file goes beyond the given limit then it will show an error
    max_cache_size: Option<ByteSize>,
}

impl DuckDBConfig {
    pub fn ensure_cache_dir_exists(&self) -> AppResult<&PathBuf> {
        std::fs::create_dir_all(&self.cache_dir)?;
        Ok(&self.cache_dir)
    }
}

impl Default for DuckDBConfig {
    fn default() -> Self {
        Self {
            alloted_ram: None,
            cache_dir: cache_path().unwrap_or_else(|_| std::env::temp_dir().join("tabiew")),
            max_cache_size: None,
        }
    }
}
