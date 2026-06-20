use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// A background thread that calls `flush_fn` every `interval`, stoppable via Drop.
///
/// Sleeps in small slices so `Drop` reacts quickly (within the slice duration).
pub struct FlushWorker {
    handle: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

impl FlushWorker {
    pub fn start<F>(interval: Duration, flush_fn: F) -> Self
    where
        F: Fn() + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let handle = thread::spawn(move || {
            while !stop_clone.load(Ordering::SeqCst) {
                flush_fn();
                // Sleep in small slices so Drop reacts quickly.
                let deadline = Instant::now() + interval;
                while Instant::now() < deadline {
                    if stop_clone.load(Ordering::SeqCst) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            }
        });
        Self {
            handle: Some(handle),
            stop,
        }
    }
}

impl Drop for FlushWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
