use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;

use async_trait::async_trait;
use datafusion::arrow::array::*;
use datafusion::arrow::datatypes::*;
use datafusion::arrow::record_batch::RecordBatch;
use futures::sink::Sink;
use futures::stream::{Stream, StreamExt};
use pgwire::api::ClientInfo;
use pgwire::api::ClientPortalStore;
use pgwire::api::PgWireServerHandlers;
use pgwire::api::auth::ServerParameterProvider;
use pgwire::api::auth::noop::NoopStartupHandler;
use pgwire::api::auth::StartupHandler;
use pgwire::api::query::SimpleQueryHandler;
use pgwire::api::results::{FieldFormat, FieldInfo, QueryResponse, Response, Tag};
use pgwire::error::{PgWireError, PgWireResult};
use pgwire::messages::PgWireBackendMessage;
use pgwire::messages::PgWireFrontendMessage;
use pgwire::api::Type;

use crate::client_session::ClientSession;
use crate::engine::EngineState;
use crate::executor::{ExecutionResult, QueryExecutor};

pub struct TodbHandlers {
    engine: Arc<EngineState>,
}

impl TodbHandlers {
    pub fn new(engine: Arc<EngineState>) -> Self {
        Self { engine }
    }
}

impl PgWireServerHandlers for TodbHandlers {
    fn simple_query_handler(&self) -> Arc<impl SimpleQueryHandler> {
        Arc::new(TodbQueryHandler {
            engine: self.engine.clone(),
        })
    }

    fn startup_handler(&self) -> Arc<impl StartupHandler> {
        Arc::new(TodbStartupHandler)
    }
}

#[derive(Debug, Clone)]
struct TodbStartupHandler;

#[async_trait]
impl NoopStartupHandler for TodbStartupHandler {}

#[derive(Debug)]
struct TodbParameterProvider;

impl ServerParameterProvider for TodbParameterProvider {
    fn server_parameters<C>(&self, _client: &C) -> Option<HashMap<String, String>>
    where
        C: ClientInfo,
    {
        let mut params = HashMap::new();
        params.insert("server_version".to_string(), "16.0-todb".to_string());
        params.insert("server_encoding".to_string(), "UTF8".to_string());
        params.insert("client_encoding".to_string(), "UTF8".to_string());
        params.insert("DateStyle".to_string(), "ISO YMD".to_string());
        params.insert("integer_datetimes".to_string(), "on".to_string());
        Some(params)
    }
}

#[derive(Debug)]
struct TodbQueryHandler {
    engine: Arc<EngineState>,
}

#[async_trait]
impl SimpleQueryHandler for TodbQueryHandler {
    async fn do_query<C>(
        &self,
        _client: &mut C,
        query: &str,
    ) -> PgWireResult<Vec<Response>>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let session = ClientSession::new("default");

        match QueryExecutor::execute(&self.engine, &session, query).await {
            Ok(ExecutionResult::Query { schema, batches }) => {
                let fields = arrow_schema_to_field_infos(&schema);
                let fields_arc = Arc::new(fields);
                let rows = batches_to_data_rows(batches, fields_arc.clone());
                Ok(vec![Response::Query(QueryResponse::new(
                    fields_arc,
                    rows,
                ))])
            }
            Ok(ExecutionResult::AffectedRows { rows }) => {
                Ok(vec![Response::Execution(
                    Tag::new("INSERT").with_rows(rows as usize),
                )])
            }
            Ok(ExecutionResult::CommandComplete { tag }) => {
                Ok(vec![Response::Execution(Tag::new(&tag))])
            }
            Err(e) => {
                let error_info = pgwire::error::ErrorInfo::new(
                    "ERROR".to_string(),
                    "42000".to_string(),
                    e.to_string(),
                );
                Ok(vec![Response::Error(Box::new(error_info))])
            }
        }
    }
}

fn arrow_schema_to_field_infos(schema: &Schema) -> Vec<FieldInfo> {
    schema
        .fields()
        .iter()
        .map(|field| {
            let pg_type = arrow_type_to_pg(&field.data_type());
            FieldInfo::new(
                field.name().clone(),
                None,
                None,
                pg_type,
                FieldFormat::Text,
            )
        })
        .collect()
}

fn arrow_type_to_pg(dt: &DataType) -> pgwire::api::Type {
    use pgwire::api::Type;
    match dt {
        DataType::Int8 | DataType::Int16 => Type::INT2,
        DataType::Int32 => Type::INT4,
        DataType::Int64 => Type::INT8,
        DataType::UInt8 | DataType::UInt16 => Type::INT2,
        DataType::UInt32 => Type::INT4,
        DataType::UInt64 => Type::INT8,
        DataType::Float32 => Type::FLOAT4,
        DataType::Float64 => Type::FLOAT8,
        DataType::Utf8 | DataType::LargeUtf8 => Type::TEXT,
        DataType::Boolean => Type::BOOL,
        DataType::Date32 | DataType::Date64 => Type::DATE,
        _ => Type::TEXT,
    }
}

fn batches_to_data_rows(
    batches: Vec<RecordBatch>,
    schema: Arc<Vec<FieldInfo>>,
) -> std::pin::Pin<Box<dyn Stream<Item = PgWireResult<pgwire::messages::data::DataRow>> + Send>> {
    let mut data_rows = Vec::new();
    let mut encoder = pgwire::api::results::DataRowEncoder::new(schema);

    for batch in &batches {
        for row_idx in 0..batch.num_rows() {
            for col_idx in 0..batch.num_columns() {
                let col = batch.column(col_idx);
                let val = if col.is_null(row_idx) {
                    None::<String>
                } else {
                    Some(column_to_string(col, row_idx))
                };
                match val {
                    Some(ref v) => {
                        let _ = encoder.encode_field_with_type_and_format(
                            v,
                            &pgwire::api::Type::TEXT,
                            pgwire::api::results::FieldFormat::Text,
                            &pgwire::types::format::FormatOptions::default(),
                        );
                    }
                    None => {
                        let _ = encoder.encode_field_with_type_and_format(
                            &Option::<String>::None,
                            &pgwire::api::Type::TEXT,
                            pgwire::api::results::FieldFormat::Text,
                            &pgwire::types::format::FormatOptions::default(),
                        );
                    }
                }
            }
            let row = encoder.take_row();
            data_rows.push(Ok(row));
        }
    }

    Box::pin(futures::stream::iter(data_rows))
}

fn column_to_string(array: &ArrayRef, row: usize) -> String {
    if array.is_null(row) {
        return String::new();
    }
    match array.as_ref() {
        a if a.as_any().is::<Int8Array>() => {
            a.as_any().downcast_ref::<Int8Array>().unwrap().value(row).to_string()
        }
        a if a.as_any().is::<Int16Array>() => {
            a.as_any().downcast_ref::<Int16Array>().unwrap().value(row).to_string()
        }
        a if a.as_any().is::<Int32Array>() => {
            a.as_any().downcast_ref::<Int32Array>().unwrap().value(row).to_string()
        }
        a if a.as_any().is::<Int64Array>() => {
            a.as_any().downcast_ref::<Int64Array>().unwrap().value(row).to_string()
        }
        a if a.as_any().is::<UInt8Array>() => {
            a.as_any().downcast_ref::<UInt8Array>().unwrap().value(row).to_string()
        }
        a if a.as_any().is::<UInt16Array>() => {
            a.as_any().downcast_ref::<UInt16Array>().unwrap().value(row).to_string()
        }
        a if a.as_any().is::<UInt32Array>() => {
            a.as_any().downcast_ref::<UInt32Array>().unwrap().value(row).to_string()
        }
        a if a.as_any().is::<UInt64Array>() => {
            a.as_any().downcast_ref::<UInt64Array>().unwrap().value(row).to_string()
        }
        a if a.as_any().is::<Float32Array>() => {
            a.as_any().downcast_ref::<Float32Array>().unwrap().value(row).to_string()
        }
        a if a.as_any().is::<Float64Array>() => {
            a.as_any().downcast_ref::<Float64Array>().unwrap().value(row).to_string()
        }
        a if a.as_any().is::<StringArray>() => {
            a.as_any().downcast_ref::<StringArray>().unwrap().value(row).to_string()
        }
        a if a.as_any().is::<BooleanArray>() => {
            let v = a.as_any().downcast_ref::<BooleanArray>().unwrap().value(row);
            if v { "t" } else { "f" }.to_string()
        }
        _ => "NULL".to_string(),
    }
}

