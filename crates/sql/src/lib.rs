use datafusion::execution::context::SessionContext;

pub mod udf;

pub fn create_session_context() -> SessionContext {
    SessionContext::new()
}
