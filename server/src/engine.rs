use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use datafusion::catalog::{CatalogProvider, CatalogProviderList, SchemaProvider, TableProvider};
use datafusion::execution::context::{SessionConfig, SessionContext};
use datafusion::execution::runtime_env::RuntimeEnv;
use datafusion::execution::session_state::SessionStateBuilder;
use sql::catalog::{build_record_batch, load_system_table_defs};

use crate::catalog_provider::{TodbCatalogProvider, TodbCatalogProviderList};
use crate::provider::TodbTableProvider;
use crate::schema_provider::TodbSchemaProvider;
use crate::version::BuildVersion;

pub const DEFAULT_CATALOG: &str = "todb";
pub const DEFAULT_SCHEMA: &str = "public";

#[derive(Debug)]
pub struct EngineState {
    pub catalog_list: Arc<dyn CatalogProviderList>,
    pub runtime_env: Arc<RuntimeEnv>,
    pub build_version: BuildVersion,
}

impl EngineState {
    pub fn new(build_version: BuildVersion) -> Result<Self> {
        let schema_provider = Arc::new(TodbSchemaProvider::new());

        let system_table_dir = default_system_table_dir();
        let system_tables = load_system_tables(&system_table_dir)?;
        for (name, table) in system_tables {
            schema_provider.register_system_table(name, table);
        }

        let default_catalog = Arc::new(TodbCatalogProvider::new());
        default_catalog.register_schema(DEFAULT_SCHEMA, schema_provider)?;

        let catalog_list = Arc::new(TodbCatalogProviderList::new());
        catalog_list.register_catalog(DEFAULT_CATALOG.to_string(), default_catalog);

        Ok(Self {
            catalog_list,
            runtime_env: Arc::new(RuntimeEnv::default()),
            build_version,
        })
    }

    pub fn schema_provider(&self) -> Arc<dyn SchemaProvider> {
        let catalog = self
            .catalog_list
            .catalog(DEFAULT_CATALOG)
            .expect("default catalog exists");
        catalog
            .schema(DEFAULT_SCHEMA)
            .expect("default schema exists")
    }

    pub fn session_context(&self) -> Result<SessionContext> {
        let config = SessionConfig::new()
            .with_information_schema(true)
            .set_str("datafusion.catalog.default_catalog", DEFAULT_CATALOG)
            .set_str("datafusion.catalog.default_schema", DEFAULT_SCHEMA)
            .set_bool(
                "datafusion.catalog.create_default_catalog_and_schema",
                false,
            )
            .set_str("datafusion.sql_parser.dialect", "postgres");

        let state = SessionStateBuilder::new()
            .with_config(config)
            .with_runtime_env(self.runtime_env.clone())
            .with_catalog_list(self.catalog_list.clone())
            .with_default_features()
            .build();

        let ctx = SessionContext::new_with_state(state);

        sql::udf::register_version_udf(
            &ctx,
            sql::udf::VersionInfo {
                commit_short: self.build_version.commit_short.clone(),
                build_time: self.build_version.build_time.clone(),
            },
        )?;

        Ok(ctx)
    }
}

fn default_system_table_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("bootstrap/system_tables.yaml")
}

fn load_system_tables(dir: &Path) -> Result<Vec<(String, Arc<dyn TableProvider>)>> {
    let defs = load_system_table_defs(dir)?;
    let mut tables = Vec::with_capacity(defs.len());

    for def in defs {
        let name = def.name.clone();
        let batch = build_record_batch(&def)?;
        let schema = batch.schema();
        let provider = Arc::new(TodbTableProvider::new_with_data(schema, vec![batch]))
            as Arc<dyn TableProvider>;
        tables.push((name, provider));
    }

    Ok(tables)
}
