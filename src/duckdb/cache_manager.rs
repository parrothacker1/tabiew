use crate::AppResult;
use anyhow::anyhow;
use bytesize::ByteSize;
use lru::LruCache;
use sha2::{Digest, Sha256};
use std::{
    num::NonZeroUsize,
    path::{Path, PathBuf},
    time::SystemTime,
};

pub struct CacheManager {
    cache_dir: PathBuf,
    cache_size: Option<ByteSize>,
    lru_file_cache: LruCache<PathBuf, SystemTime>,
}

impl CacheManager {
    pub fn new<P: AsRef<Path>>(
        cache_dir: P,
        cache_size: Option<ByteSize>,
        lru_capacity: usize,
    ) -> AppResult<Self> {
        let mut lru_cache =
            LruCache::<PathBuf, SystemTime>::new(NonZeroUsize::new(lru_capacity).unwrap());
        build_db_lru(&mut lru_cache, cache_dir.as_ref(), cache_size)?;
        Ok(Self {
            cache_dir: cache_dir.as_ref().to_path_buf(),
            cache_size,
            lru_file_cache: lru_cache,
        })
    }
    pub fn create_new_db(&mut self, file_path: &Path) -> AppResult<PathBuf> {
        if self.cache_size.is_some() {
            self.lru_cache_eviction(file_path)?;
        }
        let duckdb_file_name = hash_file_path(&self.cache_dir, file_path)?;
        if let Some((evicted_file, _)) = self
            .lru_file_cache
            .push(duckdb_file_name.to_path_buf(), SystemTime::now())
            && evicted_file != duckdb_file_name
        {
            let _ = delete_file(&evicted_file);
        }
        Ok(duckdb_file_name)
    }
    /// `Ok(None)` when the file isn't cached.
    pub fn check_existing_cache_db(&mut self, file_path: &Path) -> AppResult<Option<PathBuf>> {
        let hashed_file = hash_file_path(&self.cache_dir, file_path)?;
        if !std::fs::exists(&hashed_file).map_err(|e| anyhow!(e))? {
            return Ok(None);
        }
        self.lru_file_cache
            .push(hashed_file.clone(), SystemTime::now());
        Ok(Some(hashed_file))
    }
    pub fn delete_db(&mut self, file_path: &Path) -> AppResult<()> {
        let hashed_file = hash_file_path(&self.cache_dir, file_path)?;
        delete_file(&hashed_file)?;
        let _ = self.lru_file_cache.pop(&hashed_file);
        Ok(())
    }
    fn lru_cache_eviction(&mut self, file_path: &Path) -> AppResult<()> {
        let file_size = ByteSize(
            file_path
                .metadata()
                .map_err(|e| anyhow!("Failed to get metadata of file: {}", e))?
                .len(),
        );
        if let Some(cache_size) = self.cache_size {
            if file_size >= cache_size {
                return Err(anyhow!("File size is bigger than the cache size"));
            }
            while cache_size
                .as_u64()
                .saturating_sub(self.current_cache_usage()?.as_u64())
                < file_size.as_u64()
                && let Some((evicted, _)) = self.lru_file_cache.pop_lru()
            {
                if delete_file(&evicted).is_err() {
                    break;
                }
            }
        }
        Ok(())
    }
    fn current_cache_usage(&self) -> AppResult<ByteSize> {
        let mut total = 0;
        for entry in std::fs::read_dir(&self.cache_dir)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if metadata.is_file() && (name.ends_with(".duckdb") || name.ends_with(".duckdb.wal")) {
                total += metadata.len();
            }
        }
        Ok(ByteSize(total))
    }
}

fn delete_file(file_path: &Path) -> AppResult<()> {
    std::fs::remove_file(file_path)
        .map_err(|e| anyhow!("Failed to delete file {:?}: {}", file_path, e))?;
    let wal_path = file_path.with_extension("duckdb.wal");
    match std::fs::remove_file(&wal_path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(anyhow!("Failed to delete file {:?}: {}", wal_path, e))
        }
        _ => Ok(()),
    }
}

fn hash_file_path(cache_dir: &Path, file_path: &Path) -> AppResult<PathBuf> {
    let last_modified_time = std::fs::metadata(file_path)
        .map_err(|e| anyhow!("Failed to get metadata of the file: {}", e))?
        .modified()
        .map_err(|e| anyhow!("Failed to get modified time of the file: {}", e))?
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let file_path_bytes = file_path.as_os_str().as_encoded_bytes();
    let mut sha_hasher = Sha256::new();
    sha_hasher.update(file_path_bytes);
    sha_hasher.update(last_modified_time.as_nanos().to_le_bytes());
    let hash_res = sha_hasher.finalize();
    let hex: String = hash_res.iter().map(|b| format!("{b:02x}")).collect();
    let duckdb_file_name = format!("{hex}.duckdb");
    Ok(cache_dir.join(duckdb_file_name))
}

struct CacheEntry {
    path: PathBuf,
    size: ByteSize,
    accessed: SystemTime,
}

// TODO: what if some other tabiew instance is reading a duckdb file and this tries to delete it ?
fn build_db_lru(
    cache: &mut LruCache<PathBuf, SystemTime>,
    cache_dir: &Path,
    cache_size: Option<ByteSize>,
) -> AppResult<()> {
    std::fs::create_dir_all(cache_dir)
        .map_err(|e| anyhow!("Failed to create the cache directory: {}", e))?;
    let mut total_size = ByteSize(0);
    let entries = std::fs::read_dir(cache_dir)
        .map_err(|e| anyhow!("Failed to read the cache directory: {}", e))?;
    let mut sorted_entries = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| anyhow!("Failed to read entry: {}", e))?;
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "duckdb") {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|e| anyhow!("Failed to get the metadata of entry: {}", e))?;
        if !metadata.is_file() {
            continue;
        }
        let accessed_time = metadata
            .accessed()
            .map_err(|e| anyhow!("Failed to get accessed time of entry: {}", e))?;
        sorted_entries.push(CacheEntry {
            path,
            size: ByteSize(metadata.len()),
            accessed: accessed_time,
        });
    }
    sorted_entries.sort_by_key(|e| std::cmp::Reverse(e.accessed));
    let mut delete_index = 0;
    let cache_cap = cache.cap().get();
    for entry in &sorted_entries {
        if delete_index >= cache_cap {
            break;
        }
        if let Some(cache_size) = cache_size {
            if entry.size + total_size > cache_size {
                break;
            }
            total_size = entry.size + total_size;
        }
        delete_index += 1;
    }
    for entry in sorted_entries[..delete_index].iter().rev() {
        cache.push(entry.path.clone(), entry.accessed);
    }
    delete_files(&sorted_entries[delete_index..]);
    Ok(())
}

fn delete_files(files: &[CacheEntry]) {
    for entry in files {
        let _ = delete_file(&entry.path);
    }
}
