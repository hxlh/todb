use tonic::service::Interceptor;
use tonic::Status;

#[derive(Clone)]
pub struct TraceInterceptor;

const TRACE_ID_HEADER: &str = "x-todb-trace-id";

impl Interceptor for TraceInterceptor {
    fn call(&mut self, request: tonic::Request<()>) -> Result<tonic::Request<()>, Status> {
        let trace_id = request
            .metadata()
            .get(TRACE_ID_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        tracing::trace!(
            trace_id = %trace_id,
            "rpc request received"
        );

        Ok(request)
    }
}
