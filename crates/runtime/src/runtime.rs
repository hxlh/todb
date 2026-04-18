use std::future::Future;
use tokio::runtime::Handle;
use tokio::task::JoinHandle;
use tracing::Instrument;

pub struct AsyncRuntime {
    rt: tokio::runtime::Runtime,
}

impl AsyncRuntime {
    pub fn new(_worker_threads: usize) -> common::Result<Self> {
        let rt = tokio::runtime::Runtime::new().map_err(|e| common::Error::Known {
            code: common::ErrorCode::Internal,
            message: format!("failed to create tokio runtime: {}", e),
        })?;
        Ok(Self { rt })
    }

    pub fn handle(&self) -> &Handle {
        self.rt.handle()
    }

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.rt.block_on(future)
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.rt.spawn(future)
    }

    pub fn spawn_with_span<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.rt.spawn(future.instrument(tracing::Span::current()))
    }
}
