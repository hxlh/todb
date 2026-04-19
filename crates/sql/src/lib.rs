use datafusion::execution::context::SessionContext;

pub mod catalog;
pub mod udf;

pub fn create_session_context() -> SessionContext {
    SessionContext::new()
}
