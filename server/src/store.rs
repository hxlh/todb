use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::{Result, anyhow};
use datafusion::arrow::datatypes::SchemaRef;
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::datasource::MemTable;

pub type TableId = String;

#[derive(Debug)]
pub struct TableStore {
    tables: RwLock<HashMap<TableId, Arc<InMemoryTableData>>>,
}

#[derive(Debug)]
pub struct InMemoryTableData {
    schema: SchemaRef,
    batches: RwLock<Vec<RecordBatch>>,
}

impl TableStore {
    pub fn new() -> Self {
        Self {
            tables: RwLock::new(HashMap::new()),
        }
    }

    pub fn create_table(&self, table_id: impl Into<TableId>, schema: SchemaRef) -> Result<()> {
        let table_id = table_id.into();
        let mut tables = self
            .tables
            .write()
            .map_err(|_| anyhow!("table store write lock poisoned"))?;

        if tables.contains_key(&table_id) {
            return Err(anyhow!("table already exists: {table_id}"));
        }

        tables.insert(
            table_id,
            Arc::new(InMemoryTableData {
                schema,
                batches: RwLock::new(Vec::new()),
            }),
        );
        Ok(())
    }

    pub fn append(&self, table_id: &str, batch: RecordBatch) -> Result<()> {
        let table = self
            .table(table_id)
            .ok_or_else(|| anyhow!("table not found: {table_id}"))?;
        table.append(batch)
    }

    pub fn drop_table(&self, table_id: impl AsRef<str>) -> Result<()> {
        let table_id = table_id.as_ref().to_string();
        let mut tables = self
            .tables
            .write()
            .map_err(|_| anyhow!("table store write lock poisoned"))?;
        tables
            .remove(&table_id)
            .ok_or_else(|| anyhow!("table not found: {table_id}"))?;
        Ok(())
    }

    pub fn table(&self, table_id: &str) -> Option<Arc<InMemoryTableData>> {
        let tables = self.tables.read().ok()?;
        tables.get(table_id).cloned()
    }
}

impl InMemoryTableData {
    pub fn append(&self, batch: RecordBatch) -> Result<()> {
        if !self.schema.contains(&batch.schema()) {
            return Err(anyhow!("batch schema mismatch"));
        }

        self.batches
            .write()
            .map_err(|_| anyhow!("table batch write lock poisoned"))?
            .push(batch);
        Ok(())
    }

    pub fn replace_batches(&self, new_batches: Vec<RecordBatch>) -> Result<()> {
        *self
            .batches
            .write()
            .map_err(|_| anyhow!("table batch write lock poisoned"))? = new_batches;
        Ok(())
    }

    pub fn row_count(&self) -> usize {
        self.batches
            .read()
            .map(|batches| batches.iter().map(RecordBatch::num_rows).sum())
            .unwrap_or(0)
    }

    pub fn mem_table(&self) -> Result<Arc<MemTable>> {
        let partitions = vec![self
            .batches
            .read()
            .map_err(|_| anyhow!("table batch read lock poisoned"))?
            .clone()];
        Ok(Arc::new(MemTable::try_new(self.schema.clone(), partitions)?))
    }

    pub fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    pub fn batches(&self) -> Result<Vec<RecordBatch>> {
        Ok(self
            .batches
            .read()
            .map_err(|_| anyhow!("table batch read lock poisoned"))?
            .clone())
    }
}
