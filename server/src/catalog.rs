use std::collections::HashMap;
use std::sync::RwLock;

use anyhow::{Result, anyhow};
use datafusion::arrow::datatypes::SchemaRef;

#[derive(Debug)]
pub struct CatalogManager {
    databases: RwLock<HashMap<String, DatabaseMeta>>,
}

#[derive(Debug, Clone)]
pub struct DatabaseMeta {
    pub name: String,
    pub tables: HashMap<String, TableMeta>,
}

#[derive(Debug, Clone)]
pub struct TableMeta {
    pub name: String,
    pub database: String,
    pub schema: SchemaRef,
}

impl CatalogManager {
    pub fn new() -> Self {
        let mut databases = HashMap::new();
        databases.insert(
            "default".to_string(),
            DatabaseMeta {
                name: "default".to_string(),
                tables: HashMap::new(),
            },
        );

        Self {
            databases: RwLock::new(databases),
        }
    }

    pub fn create_table(
        &self,
        database: &str,
        table: &str,
        schema: SchemaRef,
    ) -> Result<TableMeta> {
        let mut databases = self
            .databases
            .write()
            .map_err(|_| anyhow!("catalog write lock poisoned"))?;
        let database_meta = databases
            .entry(database.to_string())
            .or_insert_with(|| DatabaseMeta {
                name: database.to_string(),
                tables: HashMap::new(),
            });

        if database_meta.tables.contains_key(table) {
            return Err(anyhow!("table already exists: {database}.{table}"));
        }

        let table_meta = TableMeta {
            name: table.to_string(),
            database: database.to_string(),
            schema,
        };
        database_meta
            .tables
            .insert(table.to_string(), table_meta.clone());
        Ok(table_meta)
    }

    pub fn table(&self, database: &str, table: &str) -> Option<TableMeta> {
        let databases = self.databases.read().ok()?;
        databases.get(database)?.tables.get(table).cloned()
    }

    pub fn drop_table(&self, database: &str, table: &str) -> Result<TableMeta> {
        let mut databases = self
            .databases
            .write()
            .map_err(|_| anyhow!("catalog write lock poisoned"))?;
        let database_meta = databases
            .get_mut(database)
            .ok_or_else(|| anyhow!("database not found: {database}"))?;

        database_meta
            .tables
            .remove(table)
            .ok_or_else(|| anyhow!("table not found: {database}.{table}"))
    }
}
