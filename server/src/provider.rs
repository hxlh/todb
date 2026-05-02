use std::any::Any;
use std::fmt;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use datafusion::arrow::datatypes::SchemaRef;
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::catalog::{Session, TableProvider};
use datafusion::common::{SchemaExt, not_impl_err, plan_err};
use datafusion::error::Result as DfResult;
use datafusion::execution::TaskContext;
use datafusion::logical_expr::TableType;
use datafusion::logical_expr::dml::InsertOp;
use datafusion::physical_plan::insert::{DataSink, DataSinkExec};
use datafusion::physical_plan::memory::MemoryExec;
use datafusion::physical_plan::metrics::MetricsSet;
use datafusion::physical_plan::{
    DisplayAs, DisplayFormatType, ExecutionPlan, SendableRecordBatchStream,
};
use futures::StreamExt;

pub struct TodbTableProvider {
    schema: SchemaRef,
    batches: Arc<RwLock<Vec<RecordBatch>>>,
}

impl TodbTableProvider {
    pub fn new(schema: SchemaRef) -> Self {
        Self {
            schema,
            batches: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn new_with_data(schema: SchemaRef, batches: Vec<RecordBatch>) -> Self {
        Self {
            schema,
            batches: Arc::new(RwLock::new(batches)),
        }
    }

    pub fn row_count(&self) -> usize {
        self.batches
            .read()
            .map(|b| b.iter().map(|batch| batch.num_rows()).sum())
            .unwrap_or(0)
    }
}

impl fmt::Debug for TodbTableProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TodbTableProvider")
            .field("schema", &self.schema)
            .finish()
    }
}

#[async_trait]
impl TableProvider for TodbTableProvider {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    async fn scan(
        &self,
        _state: &dyn Session,
        projection: Option<&Vec<usize>>,
        _filters: &[datafusion::logical_expr::Expr],
        _limit: Option<usize>,
    ) -> DfResult<Arc<dyn ExecutionPlan>> {
        let batches = self.batches.read().map_err(|e| {
            datafusion::error::DataFusionError::Execution(format!("lock poisoned: {e}"))
        })?;

        let projected_schema = match projection {
            Some(projection) => {
                let fields: Vec<_> = projection
                    .iter()
                    .map(|&i| self.schema.field(i).clone())
                    .collect();
                Arc::new(datafusion::arrow::datatypes::Schema::new(fields))
            }
            None => self.schema.clone(),
        };

        let projected_batches: Vec<RecordBatch> = batches
            .iter()
            .map(|batch| match projection {
                Some(projection) => batch.project(projection),
                None => Ok(batch.clone()),
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Arc::new(MemoryExec::try_new(
            &[projected_batches],
            projected_schema,
            None,
        )?))
    }

    async fn insert_into(
        &self,
        _state: &dyn Session,
        input: Arc<dyn ExecutionPlan>,
        insert_op: InsertOp,
    ) -> DfResult<Arc<dyn ExecutionPlan>> {
        if !self
            .schema()
            .logically_equivalent_names_and_types(&input.schema())
        {
            return plan_err!(
                "Insert schema mismatch: expected {:?}, got {:?}",
                self.schema()
                    .fields()
                    .iter()
                    .map(|f| f.data_type())
                    .collect::<Vec<_>>(),
                input
                    .schema()
                    .fields()
                    .iter()
                    .map(|f| f.data_type())
                    .collect::<Vec<_>>()
            );
        }
        if insert_op != InsertOp::Append {
            return not_impl_err!("{insert_op} not supported for TodbTableProvider");
        }

        let sink = Arc::new(TodbSink {
            batches: self.batches.clone(),
        });

        Ok(Arc::new(DataSinkExec::new(
            input,
            sink,
            Arc::clone(&self.schema),
            None,
        )))
    }
}

struct TodbSink {
    batches: Arc<RwLock<Vec<RecordBatch>>>,
}

impl fmt::Debug for TodbSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TodbSink").finish()
    }
}

impl DisplayAs for TodbSink {
    fn fmt_as(&self, _t: DisplayFormatType, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TodbSink")
    }
}

#[async_trait]
impl DataSink for TodbSink {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn metrics(&self) -> Option<MetricsSet> {
        None
    }

    async fn write_all(
        &self,
        mut data: SendableRecordBatchStream,
        _context: &Arc<TaskContext>,
    ) -> DfResult<u64> {
        let mut new_batches = Vec::new();
        let mut row_count = 0u64;
        while let Some(batch) = data.next().await.transpose()? {
            row_count += batch.num_rows() as u64;
            new_batches.push(batch);
        }

        let mut batches = self.batches.write().map_err(|e| {
            datafusion::error::DataFusionError::Execution(format!("lock poisoned: {e}"))
        })?;
        batches.append(&mut new_batches);

        Ok(row_count)
    }
}
