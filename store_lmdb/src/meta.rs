//! LMDB implementation of MetaStore.

use std::sync::Arc;

use heed::types::Bytes;
use heed::{Database, Env};

use burst_store::meta::MetaStore;
use burst_store::StoreError;

use crate::LmdbError;

const SCHEMA_VERSION_KEY: &[u8] = b"schema_version";

pub struct LmdbMetaStore {
    pub(crate) env: Arc<Env>,
    pub(crate) meta_db: Database<Bytes, Bytes>,
}

impl MetaStore for LmdbMetaStore {
    fn put_meta(&self, key: &str, value: &[u8]) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.meta_db
            .put(&mut wtxn, key.as_bytes(), value)
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_meta(&self, key: &str) -> Result<Vec<u8>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let val = self
            .meta_db
            .get(&rtxn, key.as_bytes())
            .map_err(LmdbError::from)?
            .ok_or_else(|| LmdbError::NotFound(format!("meta key '{}'", key)))?;
        Ok(val.to_vec())
    }

    fn delete_meta(&self, key: &str) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.meta_db
            .delete(&mut wtxn, key.as_bytes())
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_schema_version(&self) -> Result<u32, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let val = self
            .meta_db
            .get(&rtxn, SCHEMA_VERSION_KEY)
            .map_err(LmdbError::from)?;
        match val {
            Some(bytes) if bytes.len() == 4 => {
                let arr: [u8; 4] = bytes.try_into().expect("checked length");
                Ok(u32::from_le_bytes(arr))
            }
            Some(_) => Err(LmdbError::Serialization(
                "schema_version has unexpected byte length".to_string(),
            ))?,
            None => Ok(0),
        }
    }

    fn set_schema_version(&self, version: u32) -> Result<(), StoreError> {
        let bytes = version.to_le_bytes();
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.meta_db
            .put(&mut wtxn, SCHEMA_VERSION_KEY, &bytes)
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }
}
