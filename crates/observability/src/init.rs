use crate::metrics::MetricsCollector;
use crate::trace::TracerProvider;
use tracing_subscriber::EnvFilter;

pub struct ObservabilityConfig {
    pub service_name: String,
    pub log_level: String,
    pub log_format: String,
    pub enable_otlp: bool,
    pub otlp_endpoint: Option<String>,
    pub enable_prometheus: bool,
    pub prometheus_listen_addr: Option<String>,
}

pub fn init_observability(config: &ObservabilityConfig) -> common::Result<MetricsCollector> {
    // Build env filter
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.log_level));

    // Build tracer provider if OTLP enabled
    let tracer = if config.enable_otlp {
        let endpoint = config.otlp_endpoint.as_deref().unwrap_or("http://localhost:4317");
        TracerProvider::build_otlp(endpoint, &config.service_name).ok()
    } else {
        None
    };

    // Init tracing subscriber
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let registry = tracing_subscriber::registry().with(env_filter);

    match config.log_format.as_str() {
        "json" => {
            let fmt_layer = tracing_subscriber::fmt::layer().json();
            if let Some(tracer) = tracer {
                let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
                registry.with(fmt_layer).with(otel_layer).init();
            } else {
                registry.with(fmt_layer).init();
            }
        }
        _ => {
            let fmt_layer = tracing_subscriber::fmt::layer();
            if let Some(tracer) = tracer {
                let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
                registry.with(fmt_layer).with(otel_layer).init();
            } else {
                registry.with(fmt_layer).init();
            }
        }
    }

    tracing::info!(
        service = %config.service_name,
        level = %config.log_level,
        format = %config.log_format,
        "observability initialized"
    );

    MetricsCollector::new()
}
