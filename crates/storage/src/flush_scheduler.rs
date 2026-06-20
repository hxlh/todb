use std::time::Duration;

use crate::flush_worker::FlushWorker;

/// Schedules cross-shard flush. Wraps a [`FlushWorker`] that periodically
/// invokes `flush_all` (supplied by `LsmEngine` to flush all its shards).
/// The worker thread stops when this is dropped.
pub struct FlushScheduler {
    _worker: FlushWorker,
}

impl FlushScheduler {
    /// Call `flush_all` every `interval`. `flush_all` typically iterates the
    /// engine's shards and calls `flush_oldest_imm` on each.
    pub fn start<F>(interval: Duration, flush_all: F) -> Self
    where
        F: Fn() + Send + 'static,
    {
        Self {
            _worker: FlushWorker::start(interval, flush_all),
        }
    }
}
