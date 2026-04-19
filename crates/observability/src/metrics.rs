use prometheus::{HistogramOpts, HistogramVec, IntCounter, IntGauge, Registry};

pub struct MetricsCollector {
    pub registry: Registry,
    pub rpc_duration: HistogramVec,
    pub rpc_requests_total: IntCounter,
    pub rpc_active: IntGauge,
    pub memory_allocated: IntGauge,
}

impl MetricsCollector {
    pub fn new() -> common::Result<Self> {
        let registry = Registry::new();

        let rpc_duration = HistogramVec::new(
            HistogramOpts::new("todb_rpc_duration_seconds", "RPC request duration")
                .buckets(prometheus::exponential_buckets(0.001, 2.0, 16).unwrap()),
            &["method", "status"],
        )
        .map_err(|e| common::Error::Known {
            code: common::ErrorCode::Internal,
            message: format!("failed to create rpc_duration metric: {}", e),
        })?;

        let rpc_requests_total =
            IntCounter::new("todb_rpc_requests_total", "Total number of RPC requests").map_err(
                |e| common::Error::Known {
                    code: common::ErrorCode::Internal,
                    message: format!("failed to create rpc_requests_total metric: {}", e),
                },
            )?;

        let rpc_active = IntGauge::new("todb_rpc_active", "Number of active RPC requests")
            .map_err(|e| common::Error::Known {
                code: common::ErrorCode::Internal,
                message: format!("failed to create rpc_active metric: {}", e),
            })?;

        let memory_allocated =
            IntGauge::new("todb_memory_allocated_bytes", "Total allocated memory").map_err(
                |e| common::Error::Known {
                    code: common::ErrorCode::Internal,
                    message: format!("failed to create memory_allocated metric: {}", e),
                },
            )?;

        registry
            .register(Box::new(rpc_duration.clone()))
            .map_err(|e| common::Error::Known {
                code: common::ErrorCode::Internal,
                message: format!("failed to register rpc_duration: {}", e),
            })?;
        registry
            .register(Box::new(rpc_requests_total.clone()))
            .map_err(|e| common::Error::Known {
                code: common::ErrorCode::Internal,
                message: format!("failed to register rpc_requests_total: {}", e),
            })?;
        registry
            .register(Box::new(rpc_active.clone()))
            .map_err(|e| common::Error::Known {
                code: common::ErrorCode::Internal,
                message: format!("failed to register rpc_active: {}", e),
            })?;
        registry
            .register(Box::new(memory_allocated.clone()))
            .map_err(|e| common::Error::Known {
                code: common::ErrorCode::Internal,
                message: format!("failed to register memory_allocated: {}", e),
            })?;

        Ok(Self {
            registry,
            rpc_duration,
            rpc_requests_total,
            rpc_active,
            memory_allocated,
        })
    }
}
