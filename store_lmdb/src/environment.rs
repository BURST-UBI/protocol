//! LMDB environment setup.

use std::path::Path;

/// Wraps the LMDB environment and all database handles.
pub struct LmdbEnvironment {
    // The heed Env will be stored here once implemented.
    _path: std::path::PathBuf,
}

impl LmdbEnvironment {
    /// Open or create an LMDB environment at the given path.
    pub fn open(_path: &Path, _max_dbs: u32, _map_size: usize) -> Result<Self, super::LmdbError> {
        todo!("open heed::Env with the given parameters")
    }
}
