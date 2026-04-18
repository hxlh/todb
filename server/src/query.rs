use anyhow::Result;
use datafusion::arrow::array::Array;
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::arrow::util::display::array_value_to_string;
use serde::Serialize;
use serde_json::{Map, Number, Value};
use sql::{create_session_context, udf::VersionInfo, udf::register_version_udf};

use crate::version::BuildVersion;

#[derive(Debug, Serialize, PartialEq)]
pub struct QuerySchemaField {
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: String,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct QueryResponse {
    pub schema: Vec<QuerySchemaField>,
    pub data: Vec<Map<String, Value>>,
}

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

pub struct QueryEngine {
    ctx: datafusion::execution::context::SessionContext,
}

impl QueryEngine {
    pub fn new(build_version: BuildVersion) -> Result<Self> {
        let ctx = create_session_context();
        register_version_udf(
            &ctx,
            VersionInfo {
                commit_short: build_version.commit_short,
                build_time: build_version.build_time,
            },
        )?;
        Ok(Self { ctx })
    }
}

pub async fn execute_query(engine: &QueryEngine, sql: &str) -> Result<QueryResponse, QueryError> {
    let dataframe = engine.ctx.sql(sql).await.map_err(anyhow::Error::from)?;
    let batches = dataframe.collect().await.map_err(anyhow::Error::from)?;
    Ok(record_batches_to_response(&batches)?)
}

fn record_batches_to_response(batches: &[RecordBatch]) -> Result<QueryResponse> {
    let Some(first_batch) = batches.first() else {
        return Ok(QueryResponse {
            schema: Vec::new(),
            data: Vec::new(),
        });
    };

    let schema = first_batch
        .schema()
        .fields()
        .iter()
        .map(|field| QuerySchemaField {
            name: field.name().to_string(),
            data_type: field.data_type().to_string(),
        })
        .collect::<Vec<_>>();

    let mut data = Vec::new();

    for batch in batches {
        let field_names = batch
            .schema()
            .fields()
            .iter()
            .map(|field| field.name().to_string())
            .collect::<Vec<_>>();

        for row_index in 0..batch.num_rows() {
            let mut row = Map::new();
            for (column_index, column_name) in field_names.iter().enumerate() {
                let column = batch.column(column_index);
                row.insert(
                    column_name.clone(),
                    array_value_to_json(column.as_ref(), row_index)?,
                );
            }
            data.push(row);
        }
    }

    Ok(QueryResponse { schema, data })
}

fn array_value_to_json(array: &dyn Array, row_index: usize) -> Result<Value> {
    if array.is_null(row_index) {
        return Ok(Value::Null);
    }

    let rendered = array_value_to_string(array, row_index).map_err(anyhow::Error::from)?;
    Ok(rendered_value_to_json(&rendered))
}

fn rendered_value_to_json(rendered: &str) -> Value {
    if let Ok(boolean) = rendered.parse::<bool>() {
        return Value::Bool(boolean);
    }

    if let Ok(integer) = rendered.parse::<i64>() {
        return Value::Number(Number::from(integer));
    }

    if let Ok(unsigned) = rendered.parse::<u64>() {
        return Value::Number(Number::from(unsigned));
    }

    if let Ok(float) = rendered.parse::<f64>()
        && let Some(number) = Number::from_f64(float)
    {
        return Value::Number(number);
    }

    Value::String(rendered.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{QueryEngine, execute_query};
    use crate::version::BuildVersion;

    #[tokio::test]
    async fn execute_query_accepts_select_version() {
        let engine = QueryEngine::new(BuildVersion {
            commit_short: "abc1234".to_string(),
            build_time: "20260418215711".to_string(),
        })
        .expect("create query engine");

        let response = execute_query(&engine, "select version() as version")
            .await
            .expect("execute query");

        assert_eq!(response.schema.len(), 1);
        assert_eq!(response.schema[0].name, "version");
        assert_eq!(response.schema[0].data_type, "Utf8");
        assert_eq!(
            response.data,
            vec![json!({"version": "abc1234-20260418215711"})
                .as_object()
                .unwrap()
                .clone()]
        );
    }

    #[tokio::test]
    async fn execute_query_supports_general_projection_output() {
        let engine = QueryEngine::new(BuildVersion {
            commit_short: "abc1234".to_string(),
            build_time: "20260418215711".to_string(),
        })
        .expect("create query engine");

        let response = execute_query(&engine, "select 'Alice' as name, 30 as age")
            .await
            .expect("execute query");

        assert_eq!(response.schema.len(), 2);
        assert_eq!(response.schema[0].name, "name");
        assert_eq!(response.schema[0].data_type, "Utf8");
        assert_eq!(response.schema[1].name, "age");
        assert_eq!(response.schema[1].data_type, "Int64");
        assert_eq!(
            response.data,
            vec![json!({"name": "Alice", "age": 30})
                .as_object()
                .unwrap()
                .clone()]
        );
    }
}
