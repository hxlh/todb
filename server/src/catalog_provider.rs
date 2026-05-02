use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, RwLock};

use datafusion::catalog::{CatalogProvider, CatalogProviderList, SchemaProvider};
use datafusion::error::Result;

pub struct TodbCatalogProvider {
    schemas: RwLock<HashMap<String, Arc<dyn SchemaProvider>>>,
}

impl TodbCatalogProvider {
    pub fn new() -> Self {
        Self {
            schemas: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for TodbCatalogProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for TodbCatalogProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TodbCatalogProvider")
            .field("schema_count", &self.schemas.read().expect("lock").len())
            .finish()
    }
}

impl CatalogProvider for TodbCatalogProvider {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn schema_names(&self) -> Vec<String> {
        self.schemas.read().expect("lock").keys().cloned().collect()
    }

    fn schema(&self, name: &str) -> Option<Arc<dyn SchemaProvider>> {
        self.schemas.read().expect("lock").get(name).cloned()
    }

    fn register_schema(
        &self,
        name: &str,
        schema: Arc<dyn SchemaProvider>,
    ) -> Result<Option<Arc<dyn SchemaProvider>>> {
        Ok(self
            .schemas
            .write()
            .expect("lock")
            .insert(name.to_string(), schema))
    }

    fn deregister_schema(
        &self,
        name: &str,
        _cascade: bool,
    ) -> Result<Option<Arc<dyn SchemaProvider>>> {
        Ok(self.schemas.write().expect("lock").remove(name))
    }
}


pub struct TodbCatalogProviderList {
    catalogs: RwLock<HashMap<String, Arc<dyn CatalogProvider>>>,
}

impl TodbCatalogProviderList {
    pub fn new() -> Self {
        Self {
            catalogs: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for TodbCatalogProviderList {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for TodbCatalogProviderList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TodbCatalogProviderList")
            .field("catalog_count", &self.catalogs.read().expect("lock").len())
            .finish()
    }
}

impl CatalogProviderList for TodbCatalogProviderList {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn register_catalog(
        &self,
        name: String,
        catalog: Arc<dyn CatalogProvider>,
    ) -> Option<Arc<dyn CatalogProvider>> {
        self.catalogs.write().expect("lock").insert(name, catalog)
    }

    fn catalog_names(&self) -> Vec<String> {
        self.catalogs
            .read()
            .expect("lock")
            .keys()
            .cloned()
            .collect()
    }

    fn catalog(&self, name: &str) -> Option<Arc<dyn CatalogProvider>> {
        self.catalogs.read().expect("lock").get(name).cloned()
    }
}
