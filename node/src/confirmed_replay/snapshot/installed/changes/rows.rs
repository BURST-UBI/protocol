use super::*;
use crate::confirmed_replay::scratch_records::{ScratchRecords, ScratchRecordsBuilder};

#[derive(serde::Serialize, serde::Deserialize)]
struct Row {
    table: UndoTable,
    key: Vec<u8>,
    value: Vec<u8>,
}

pub(super) struct ProjectionRows {
    records: ScratchRecords<Row>,
    builder: Option<ScratchRecordsBuilder<Row>>,
    count: usize,
}

fn identity(table: UndoTable, key: &[u8]) -> Result<[u8; 32], StoreError> {
    let encoded = bincode::serialize(&(table, key)).map_err(invalid)?;
    Ok(burst_crypto::blake2b_256(&encoded))
}

impl ProjectionRows {
    pub(super) fn new(directory: &std::path::Path) -> Result<Self, StoreError> {
        Ok(Self {
            records: ScratchRecords::new(directory)?,
            builder: None,
            count: 0,
        })
    }

    pub(super) fn bulk(directory: &std::path::Path) -> Result<Self, StoreError> {
        let mut rows = Self::new(directory)?;
        // Rows::put charges logical key/value bytes. The builder also counts
        // serialized table and vector headers, but excludes index and RAM costs.
        rows.builder = Some(ScratchRecordsBuilder::new(
            directory,
            MAX_ROWS,
            (MAX_BYTES + 32 * MAX_ROWS) as u64,
        )?);
        Ok(rows)
    }

    pub(super) fn finish_bulk(mut self) -> Result<Self, StoreError> {
        if let Some(builder) = self.builder.take() {
            self.records = builder.finish()?;
        }
        Ok(self)
    }

    pub(super) fn len(&self) -> usize {
        self.count
    }

    pub(super) fn get(&self, table: UndoTable, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        if self.builder.is_some() {
            return Err(invalid("replay projection index is unfinished"));
        }
        let row = self.records.get(&identity(table, key)?)?;
        row.map(|row| {
            if row.table != table || row.key != key {
                return Err(invalid("replay projection key differs from its index"));
            }
            Ok(row.value)
        })
        .transpose()
    }

    pub(super) fn insert(
        &mut self,
        table: UndoTable,
        key: &[u8],
        value: Vec<u8>,
    ) -> Result<(), StoreError> {
        if self.builder.is_none() && self.get(table, key)?.is_some() {
            return Err(invalid("duplicate replay projection row"));
        }
        let row = Row {
            table,
            key: key.to_vec(),
            value,
        };
        if let Some(builder) = &mut self.builder {
            builder.push(identity(table, key)?, &row)?;
        } else if self.records.put(identity(table, key)?, &row)? {
            return Err(invalid("replay projection row changed during insertion"));
        }
        self.count += 1;
        Ok(())
    }

    pub(super) fn visit(
        &self,
        mut visit: impl FnMut(UndoTable, Vec<u8>, Vec<u8>) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        if self.builder.is_some() {
            return Err(invalid("replay projection index is unfinished"));
        }
        let mut count = 0usize;
        self.records.visit(|key, row| {
            if identity(row.table, &row.key)? != key {
                return Err(invalid("replay projection row identity changed"));
            }
            count += 1;
            visit(row.table, row.key, row.value)
        })?;
        if count != self.count {
            return Err(invalid("replay projection inventory changed"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_projection_matches_incremental_and_requires_complete_unique_index() {
        let directory = tempfile::tempdir().unwrap();
        let mut bulk = ProjectionRows::bulk(directory.path()).unwrap();
        let mut incremental = ProjectionRows::new(directory.path()).unwrap();
        for index in (0u32..1024).rev() {
            for table in [UndoTable::Accounts, UndoTable::Frontiers] {
                let key = index.to_be_bytes();
                let value = vec![(index % 251) as u8; 37];
                bulk.insert(table, &key, value.clone()).unwrap();
                incremental.insert(table, &key, value).unwrap();
            }
        }
        assert!(bulk.get(UndoTable::Accounts, &0u32.to_be_bytes()).is_err());
        assert!(bulk.visit(|_, _, _| Ok(())).is_err());
        let bulk = bulk.finish_bulk().unwrap();
        assert_eq!(bulk.len(), incremental.len());
        let mut visited = 0;
        bulk.visit(|table, key, value| {
            assert_eq!(incremental.get(table, &key)?, Some(value));
            visited += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(visited, 2048);
        assert!(bulk.get(UndoTable::Accounts, b"missing").unwrap().is_none());
        bulk.records.truncate_for_test();
        assert!(bulk.visit(|_, _, _| Ok(())).is_err());
        let mut duplicate = ProjectionRows::bulk(directory.path()).unwrap();
        duplicate.insert(UndoTable::Meta, b"same", vec![1]).unwrap();
        duplicate.insert(UndoTable::Meta, b"same", vec![2]).unwrap();
        assert!(duplicate.finish_bulk().is_err());
    }

    #[test]
    fn projection_rows_preserve_table_identity_and_reject_corrupt_storage() {
        let directory = tempfile::tempdir().unwrap();
        let mut rows = ProjectionRows::new(directory.path()).unwrap();
        rows.insert(UndoTable::Meta, b"same", vec![1]).unwrap();
        rows.insert(UndoTable::BrnMeta, b"same", vec![2]).unwrap();
        assert!(rows.insert(UndoTable::Meta, b"same", vec![3]).is_err());
        assert_eq!(rows.get(UndoTable::Meta, b"same").unwrap(), Some(vec![1]));
        assert_eq!(rows.get(UndoTable::Meta, b"absent").unwrap(), None);
        let mut actual = std::collections::BTreeMap::new();
        rows.visit(|table, key, value| {
            actual.insert((table, key), value);
            Ok(())
        })
        .unwrap();
        assert_eq!(actual.len(), 2);
        assert_eq!(actual[&(UndoTable::BrnMeta, b"same".to_vec())], vec![2]);
        rows.records
            .put(
                identity(UndoTable::Meta, b"same").unwrap(),
                &Row {
                    table: UndoTable::Meta,
                    key: b"foreign".to_vec(),
                    value: vec![1],
                },
            )
            .unwrap();
        assert!(rows.get(UndoTable::Meta, b"same").is_err());
        assert!(rows.visit(|_, _, _| Ok(())).is_err());
        rows.records.truncate_for_test();
        assert!(rows.get(UndoTable::Meta, b"same").is_err());
        assert!(rows.visit(|_, _, _| Ok(())).is_err());
    }
}
