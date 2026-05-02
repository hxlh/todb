use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use datafusion::catalog::{SchemaProvider, TableProvider};
use datafusion::error::{DataFusionError, Result};

use crate::provider::TodbTableProvider;

pub struct TodbSchemaProvider {
    tables: RwLock<HashMap<String, Arc<dyn TableProvider>>>,
    system_tables: RwLock<HashMap<String, Arc<dyn TableProvider>>>,
}

impl TodbSchemaProvider {
    pub fn new() -> Self {
        Self {
            tables: RwLock::new(HashMap::new()),
            system_tables: RwLock::new(HashMap::new()),
        }
    }

    pub fn register_system_table(&self, name: impl Into<String>, table: Arc<dyn TableProvider>) {
        self.system_tables
            .write()
            .expect("system_tables lock")
            .insert(name.into(), table);
    }
}

impl Default for TodbSchemaProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for TodbSchemaProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TodbSchemaProvider")
            .field("user_tables", &self.tables.read().expect("lock").len())
            .field(
                "system_tables",
                &self.system_tables.read().expect("lock").len(),
            )
            .finish()
    }
}

#[async_trait]
impl SchemaProvider for TodbSchemaProvider {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn table_names(&self) -> Vec<String> {
        self.tables.read().expect("lock").keys().cloned().collect()
    }

    async fn table(&self, name: &str) -> Result<Option<Arc<dyn TableProvider>>> {
        if let Some(t) = self.tables.read().expect("lock").get(name) {
            return Ok(Some(t.clone()));
        }
        if let Some(t) = self.system_tables.read().expect("lock").get(name) {
            return Ok(Some(t.clone()));
        }
        Ok(None)
    }

    fn register_table(
        &self,
        name: String,
        table: Arc<dyn TableProvider>,
    ) -> Result<Option<Arc<dyn TableProvider>>> {
        let mut tables = self.tables.write().expect("lock");
        if tables.contains_key(&name) {
            return Err(DataFusionError::Execution(format!(
                "table already exists: {name}"
            )));
        }
        let table = Arc::new(TodbTableProvider::new(table.schema())) as Arc<dyn TableProvider>;
        Ok(tables.insert(name, table))
    }

    fn deregister_table(&self, name: &str) -> Result<Option<Arc<dyn TableProvider>>> {
        Ok(self.tables.write().expect("lock").remove(name))
    }

    fn table_exist(&self, name: &str) -> bool {
        self.tables.read().expect("lock").contains_key(name)
            || self.system_tables.read().expect("lock").contains_key(name)
    }
}
