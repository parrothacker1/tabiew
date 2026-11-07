mod cache;
mod config;
mod file_db;
mod ingester;
pub mod reader;
mod session;
mod helper;

use cache::CacheManager;
use config::Config;
use session::Session;

pub use cache::{CacheMetadata, CacheStatus};
pub use config::Config as DuckDBConfig;

pub struct DuckDB {
    cache_manager: CacheManager,
    config: Config,
    session: Session,
}

impl DuckDB {
    pub fn new() {}
}
