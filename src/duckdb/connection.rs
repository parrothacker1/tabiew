use duckdb::{Connection, ProgressHandle};
use std::path::{Path, PathBuf};

pub enum ConnectionType {
    ConnectionInMem,
    ConnectionFile,
}

pub struct DuckDB {
    conn: Connection,
    conn_type: ConnectionType,
    progress: ProgressHandle,
    file_path: Option<PathBuf>,
}

impl DuckDB {
    pub fn new() -> Self {
        Self {
            conn: Connection::open_in_memory()?,
        }
    }
    pub fn query(&self, query: String) {}
    pub fn rows_from_to(&self, from: i16, to: i16) {}

    pub fn open<P: AsRef<Path>>(&self, path: P) {}
    pub fn close(&self) {}
}
