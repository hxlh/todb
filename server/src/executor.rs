use anyhow::Result;
use datafusion::arrow::datatypes::SchemaRef;
use datafusion::arrow::record_batch::RecordBatch;

use crate::client_session::ClientSession;
use crate::engine::EngineState;

pub enum ExecutionResult {
    Query {
        schema: SchemaRef,
        batches: Vec<RecordBatch>,
    },
    AffectedRows {
        rows: u64,
    },
    CommandComplete {
        tag: String,
    },
}

pub struct QueryExecutor;

impl QueryExecutor {
    pub async fn execute(
        _engine: &EngineState,
        _session: &ClientSession,
        _sql: &str,
    ) -> Result<ExecutionResult> {
        anyhow::bail!("not implemented")
    }
}
