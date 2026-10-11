//! A pool of read-only catalog connections.

use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use medianest_catalog::Db;

use crate::Result;

/// Read-only connections, opened on demand and reused.
#[derive(Debug)]
pub struct ReaderPool {
    path: PathBuf,
    idle: Mutex<Vec<Db>>,
}

impl ReaderPool {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            idle: Mutex::new(Vec::new()),
        }
    }

    /// Runs `query` on a read-only connection. Concurrent callers get
    /// separate connections, so reads never wait for each other or for writes.
    pub fn with<R>(&self, query: impl FnOnce(&Db) -> medianest_catalog::Result<R>) -> Result<R> {
        let db = self
            .idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop();
        let db = match db {
            Some(db) => db,
            None => Db::open_read_only(&self.path)?,
        };
        let result = query(&db);
        let mut idle = self.idle.lock().unwrap_or_else(PoisonError::into_inner);
        if idle.len() < 8 {
            idle.push(db);
        }
        Ok(result?)
    }

    /// Closes idle connections, for example before a restore.
    pub(crate) fn clear(&self) {
        self.idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}
