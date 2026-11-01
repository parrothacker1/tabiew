use crate::AppResult;
use anyhow::anyhow;
use bytesize::ByteSize;
use lru::LruCache;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    time::SystemTime,
};

pub struct CacheManager {
    cache_dir: PathBuf,
    cache_size: Option<ByteSize>,
    lru_file_cache: LruCache<PathBuf, Option<File>>,
}

impl CacheManager {
    pub fn new<P: AsRef<Path>>(
        cache_dir: P,
        cache_size: Option<ByteSize>,
        lru_capacity: usize,
    ) -> AppResult<Self> {
        let mut lru_cache: LruCache<PathBuf, Option<File>>;
        if lru_capacity == 0 {
            lru_cache = LruCache::unbounded();
        } else {
            lru_cache = LruCache::new(NonZeroUsize::new(lru_capacity).unwrap());
        }
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
        let lock = lock_db(&duckdb_file_name)?;
        self.track(&duckdb_file_name, lock);
        Ok(duckdb_file_name)
    }
    pub fn check_existing_cache_db(&mut self, file_path: &Path) -> AppResult<Option<PathBuf>> {
        let hashed_file = hash_file_path(&self.cache_dir, file_path)?;
        if self
            .lru_file_cache
            .peek(&hashed_file)
            .is_some_and(Option::is_some)
        {
            self.lru_file_cache.promote(&hashed_file);
            return Ok(Some(hashed_file));
        }
        let lock = lock_db(&hashed_file)?;
        if !std::fs::exists(&hashed_file).map_err(|e| anyhow!(e))? {
            return Ok(None);
        }
        self.track(&hashed_file, lock);
        Ok(Some(hashed_file))
    }
    pub fn delete_db(&mut self, file_path: &Path) -> AppResult<()> {
        let hashed_file = hash_file_path(&self.cache_dir, file_path)?;
        self.lru_file_cache.pop(&hashed_file);
        delete_file(&hashed_file)
    }
    pub fn release_db(&mut self, file_path: &Path) {
        if let Some(lock) = self.lru_file_cache.peek_mut(file_path) {
            *lock = None;
        }
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
            let mut in_use = Vec::new();
            let result = loop {
                let free_space = match self.current_cache_usage() {
                    Ok(usage) => cache_size.as_u64().saturating_sub(usage.as_u64()),
                    Err(e) => break Err(e),
                };
                if free_space >= file_size.as_u64() {
                    break Ok(());
                }
                let Some((evicted, lock)) = self.lru_file_cache.pop_lru() else {
                    break Err(anyhow!(
                        "Can't allocate {} in the cache, the remaining cached files are in use",
                        file_size
                    ));
                };
                drop(lock);
                if delete_file(&evicted).is_err() {
                    in_use.push(evicted);
                }
            };
            for path in in_use.into_iter().rev() {
                self.lru_file_cache.push(path.clone(), None);
                self.lru_file_cache.demote(&path);
            }
            return result;
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
    fn track(&mut self, file_path: &Path, lock: File) {
        if let Some((evicted, evicted_lock)) = self
            .lru_file_cache
            .push(file_path.to_path_buf(), Some(lock))
            && evicted != file_path
        {
            drop(evicted_lock);
            let _ = delete_file(&evicted);
        }
    }
}

fn lock_db(file_path: &Path) -> AppResult<File> {
    let lock = File::create(file_path.with_extension("duckdb.lock"))?;
    lock.lock_shared()?;
    Ok(lock)
}

fn delete_file(file_path: &Path) -> AppResult<()> {
    let file_lock = std::fs::File::create(&file_path.with_extension("duckdb.lock"))?;
    if let Err(std::fs::TryLockError::WouldBlock) = file_lock.try_lock() {
        return Err(anyhow!("File {:?} is in use", file_path));
    }
    std::fs::remove_file(file_path)
        .map_err(|e| anyhow!("Failed to delete file {:?}: {}", file_path, e))?;
    match std::fs::remove_file(file_path.with_extension("duckdb.wal")) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(anyhow!(
            "Failed to delete file {:?}: {}",
            file_path.with_extension("duckdb.wal"),
            e
        )),
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

fn build_db_lru(
    cache: &mut LruCache<PathBuf, Option<File>>,
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
        cache.push(entry.path.clone(), None);
    }
    delete_files(&sorted_entries[delete_index..]);
    Ok(())
}

fn delete_files(files: &[CacheEntry]) {
    for entry in files {
        let _ = delete_file(&entry.path);
    }
}
