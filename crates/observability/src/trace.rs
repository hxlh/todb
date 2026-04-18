use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;

pub struct TracerProvider;

impl TracerProvider {
    pub fn build_otlp(endpoint: &str, service_name: &str) -> common::Result<opentelemetry_sdk::trace::Tracer> {
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()
            .map_err(|e| common::Error::Known {
                code: common::ErrorCode::Config,
                message: format!("failed to build OTLP exporter: {}", e),
            })?;

        let provider = opentelemetry_sdk::trace::TracerProvider::builder()
            .with_simple_exporter(exporter)
            .with_resource(opentelemetry_sdk::Resource::new(vec![
                opentelemetry::KeyValue::new("service.name", service_name.to_string()),
            ]))
            .build();

        Ok(provider.tracer("todb"))
    }
}
