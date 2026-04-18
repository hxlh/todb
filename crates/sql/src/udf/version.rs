use std::sync::Arc;

use anyhow::Result;
use datafusion::arrow::array::{ArrayRef, StringArray};
use datafusion::arrow::datatypes::DataType;
use datafusion::execution::context::SessionContext;
use datafusion::logical_expr::{ColumnarValue, ScalarUDF, Volatility, create_udf};

#[derive(Debug, Clone)]
pub struct VersionInfo {
    pub commit_short: String,
    pub build_time: String,
}

pub fn register_version_udf(ctx: &SessionContext, version: VersionInfo) -> Result<()> {
    let value = format!("{}-{}", version.commit_short, version.build_time);

    let udf: ScalarUDF = create_udf(
        "version",
        vec![],
        DataType::Utf8,
        Volatility::Immutable,
        Arc::new(move |_args: &[ColumnarValue]| {
            let array: ArrayRef = Arc::new(StringArray::from(vec![value.clone()]));
            Ok(ColumnarValue::Array(array))
        }),
    );

    ctx.register_udf(udf);
    Ok(())
}
