use std::sync::Arc;

use anyhow::Result;
use datafusion::execution::runtime_env::RuntimeEnv;

use crate::catalog::CatalogManager;
use crate::store::TableStore;
use crate::version::BuildVersion;

#[derive(Debug)]
pub struct EngineState {
    pub catalog: CatalogManager,
    pub store: TableStore,
    pub runtime_env: Arc<RuntimeEnv>,
    pub build_version: BuildVersion,
}

impl EngineState {
    pub fn new(build_version: BuildVersion) -> Result<Self> {
        Ok(Self {
            catalog: CatalogManager::new(),
            store: TableStore::new(),
            runtime_env: Arc::new(RuntimeEnv::default()),
            build_version,
        })
    }
}
