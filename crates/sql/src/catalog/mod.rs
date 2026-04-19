use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use datafusion::arrow::array::{
    ArrayRef, BooleanArray, Float64Array, Int64Array, StringArray, UInt64Array,
};
use datafusion::arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::catalog::SchemaProvider;
use datafusion::catalog_common::MemorySchemaProvider;
use datafusion::datasource::MemTable;
use datafusion::execution::context::SessionContext;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct SystemTableDef {
    pub name: String,
    pub columns: Vec<ColumnDef>,
    #[serde(default)]
    pub rows: Vec<Vec<serde_yaml::Value>>,
}

#[derive(Debug, Deserialize)]
pub struct ColumnDef {
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: String,
    #[serde(default)]
    pub nullable: bool,
}

#[derive(Debug, Deserialize)]
struct SystemTableSet {
    tables: Vec<SystemTableDef>,
}

pub fn load_system_table_defs(path: &Path) -> Result<Vec<SystemTableDef>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("read system tables file: {}", path.display()))?;
    let table_set: SystemTableSet =
        serde_yaml::from_str(&content).with_context(|| format!("parse {}", path.display()))?;
    Ok(table_set.tables)
}

fn arrow_type(name: &str) -> Result<DataType> {
    match name {
        "UInt64" => Ok(DataType::UInt64),
        "Int64" => Ok(DataType::Int64),
        "Float64" => Ok(DataType::Float64),
        "Utf8" => Ok(DataType::Utf8),
        "Boolean" => Ok(DataType::Boolean),
        other => bail!("unsupported type: {other}"),
    }
}

fn build_schema(columns: &[ColumnDef]) -> Result<SchemaRef> {
    let fields: Result<Vec<Field>> = columns
        .iter()
        .map(|c| {
            let dt = arrow_type(&c.data_type)?;
            Ok(Field::new(&c.name, dt, c.nullable))
        })
        .collect();
    Ok(Arc::new(Schema::new(fields?)))
}

fn value_to_array(values: &[serde_yaml::Value], data_type: &DataType) -> Result<ArrayRef> {
    match data_type {
        DataType::UInt64 => {
            let arr: Vec<Option<u64>> = values
                .iter()
                .map(|v| if v.is_null() { None } else { v.as_u64() })
                .collect();
            Ok(Arc::new(UInt64Array::from(arr)))
        }
        DataType::Int64 => {
            let arr: Vec<Option<i64>> = values
                .iter()
                .map(|v| if v.is_null() { None } else { v.as_i64() })
                .collect();
            Ok(Arc::new(Int64Array::from(arr)))
        }
        DataType::Float64 => {
            let arr: Vec<Option<f64>> = values
                .iter()
                .map(|v| if v.is_null() { None } else { v.as_f64() })
                .collect();
            Ok(Arc::new(Float64Array::from(arr)))
        }
        DataType::Utf8 => {
            let arr: Vec<Option<&str>> = values
                .iter()
                .map(|v| if v.is_null() { None } else { v.as_str() })
                .collect();
            Ok(Arc::new(StringArray::from(arr)))
        }
        DataType::Boolean => {
            let arr: Vec<Option<bool>> = values
                .iter()
                .map(|v| if v.is_null() { None } else { v.as_bool() })
                .collect();
            Ok(Arc::new(BooleanArray::from(arr)))
        }
        _ => bail!("unsupported type: {data_type}"),
    }
}

pub fn build_record_batch(def: &SystemTableDef) -> Result<RecordBatch> {
    let schema = build_schema(&def.columns)?;
    let row_count = def.rows.len();
    let mut columns: Vec<ArrayRef> = Vec::with_capacity(def.columns.len());

    for (col_idx, col_def) in def.columns.iter().enumerate() {
        let dt = arrow_type(&col_def.data_type)?;
        let values: Vec<serde_yaml::Value> = if row_count == 0 {
            Vec::new()
        } else {
            def.rows
                .iter()
                .map(|row| row.get(col_idx).cloned().unwrap_or(serde_yaml::Value::Null))
                .collect()
        };
        columns.push(value_to_array(&values, &dt)?);
    }

    RecordBatch::try_new(schema, columns).map_err(Into::into)
}

pub fn register_system_catalog(ctx: &SessionContext, table_path: &Path) -> Result<()> {
    let defs = load_system_table_defs(table_path)?;
    let schema = Arc::new(MemorySchemaProvider::new());

    for def in &defs {
        let batch = build_record_batch(def)?;
        let schema_ref = batch.schema();
        let table = MemTable::try_new(schema_ref, vec![vec![batch]])?;
        schema.register_table(def.name.clone(), Arc::new(table))?;
    }

    let default_catalog_name = ctx.state().config_options().catalog.default_catalog.clone();
    let catalog = ctx
        .catalog(&default_catalog_name)
        .context("default catalog not found")?;
    catalog.register_schema("system", schema)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_parse_single_yaml() {
        let yaml = r#"
name: test_table
columns:
  - name: id
    type: UInt64
    nullable: false
  - name: label
    type: Utf8
    nullable: true
rows:
  - [1, "hello"]
  - [2, null]
"#;
        let def: SystemTableDef = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(def.name, "test_table");
        assert_eq!(def.columns.len(), 2);
        assert_eq!(def.columns[0].name, "id");
        assert_eq!(def.columns[0].data_type, "UInt64");
        assert!(!def.columns[0].nullable);
        assert_eq!(def.columns[1].name, "label");
        assert!(def.columns[1].nullable);
        assert_eq!(def.rows.len(), 2);
    }

    #[test]
    fn test_load_from_single_yaml_file_preserves_table_order() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
tables:
  - name: second
    columns:
      - name: id
        type: UInt64
        nullable: false
    rows:
      - [2]
  - name: first
    columns:
      - name: id
        type: UInt64
        nullable: false
    rows:
      - [1]
"#;
        let path = dir.path().join("system_tables.yaml");
        fs::File::create(&path)
            .unwrap()
            .write_all(yaml.as_bytes())
            .unwrap();

        let defs = load_system_table_defs(&path).unwrap();
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].name, "second");
        assert_eq!(defs[1].name, "first");
    }

    #[test]
    fn test_build_batch_matches_yaml() {
        let yaml = r#"
name: demo
columns:
  - name: val
    type: UInt64
    nullable: false
  - name: flag
    type: Boolean
    nullable: true
rows:
  - [10, true]
  - [20, false]
  - [30, null]
"#;
        let def: SystemTableDef = serde_yaml::from_str(yaml).unwrap();
        let batch = build_record_batch(&def).unwrap();
        assert_eq!(batch.num_columns(), 2);
        assert_eq!(batch.num_rows(), 3);
        assert_eq!(batch.schema().field(0).name(), "val");
        assert_eq!(batch.schema().field(1).name(), "flag");
    }
}
