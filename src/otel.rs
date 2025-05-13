use anyhow::anyhow;
use opentelemetry::global;
use opentelemetry::metrics::Meter;
use opentelemetry::propagation::TextMapCompositePropagator;
use opentelemetry::trace::TracerProvider;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::{BatchLogProcessor, SdkLoggerProvider};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::propagation::{BaggagePropagator, TraceContextPropagator};
use opentelemetry_sdk::trace::{BatchSpanProcessor, SdkTracerProvider};
use std::env;
use std::sync::OnceLock;
use std::time::Duration;
use tracing::subscriber;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;

/// Initialize OpenTelemetry and return a guard that ensures proper cleanup
pub fn init_otel() -> Result<OtelGuard, anyhow::Error> {
    let providers = OtelProviders::init()?;
    Ok(OtelGuard { providers })
}

/// Creates or returns metric generator
pub fn get_meter() -> &'static Meter {
    static METER: OnceLock<Meter> = OnceLock::new();
    METER.get_or_init(|| global::meter(get_service().as_str()))
}

/// Guard that ensures OpenTelemetry providers are properly shut down
pub struct OtelGuard {
    providers: OtelProviders,
}

/// Calls `providers.shutdown()` on success of failure
impl Drop for OtelGuard {
    fn drop(&mut self) {
        if let Err(e) = self.providers.shutdown() {
            eprintln!("Error during OpenTelemetry shutdown: {}", e);
        }
    }
}

/// Wraps OTEL log, trace, and metric providers
struct OtelProviders {
    pub log_provider: SdkLoggerProvider,
    pub trace_provider: SdkTracerProvider,
    pub meter_provider: SdkMeterProvider,
}

/// Manages initialization and shutdown for OTEL providers.
impl OtelProviders {
    fn init() -> Result<OtelProviders, anyhow::Error> {
        let log_provider = init_logs()?;

        // Create a new OpenTelemetryTracingBridge using the LoggerProvider.
        let otel_layer = OpenTelemetryTracingBridge::new(&log_provider);
        let filter_otel = EnvFilter::new("info")
            .add_directive("hyper=off".parse()?)
            .add_directive("tonic=off".parse()?)
            .add_directive("rig-core=off".parse()?)
            .add_directive("reqwest=off".parse()?);
        let log_layer = otel_layer.with_filter(filter_otel);
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_thread_names(true)
            .with_filter(EnvFilter::new("info").add_directive("opentelemetry=info".parse()?));

        let trace_provider = init_traces()?;
        // Create a new OpenTelemetryTracingBridge using the TracerProvider.
        let tracing_layer = OpenTelemetryLayer::new(trace_provider.tracer(get_service().as_str()));
        let tracing_layer = tracing_layer
            .with_filter(EnvFilter::new("info").add_directive("opentelemetry=info".parse()?));

        let subscriber = tracing_subscriber::registry()
            .with(log_layer)
            .with(tracing_layer)
            .with(fmt_layer);
        subscriber::set_global_default(subscriber)?;

        let meter_provider = init_metrics()?;

        Ok(OtelProviders {
            trace_provider,
            log_provider,
            meter_provider,
        })
    }

    fn shutdown(&self) -> Result<(), anyhow::Error> {
        // Collect all shutdown errors
        let mut shutdown_errors = Vec::new();
        if let Err(e) = self.log_provider.shutdown() {
            shutdown_errors.push(format!("Shutdown log provider failed: {}", e));
        }
        if let Err(e) = self.trace_provider.shutdown() {
            shutdown_errors.push(format!("Shutdown trace provider failed: {}", e));
        }
        if let Err(e) = self.meter_provider.shutdown() {
            shutdown_errors.push(format!("Shutdown meter provider failed: {}", e));
        }
        // Return an error if any shutdown failed
        if !shutdown_errors.is_empty() {
            return Err(anyhow!(format!(
                "Failed to shutdown providers:{}",
                shutdown_errors.join("\n")
            )));
        }
        Ok(())
    }
}

fn get_service() -> &'static String {
    static SERVICE: OnceLock<String> = OnceLock::new();
    SERVICE.get_or_init(|| env::var("OTEL_SERVICE_NAME").unwrap_or("otel-service".to_owned()))
}

fn get_resource() -> Resource {
    static RESOURCE: OnceLock<Resource> = OnceLock::new();
    RESOURCE
        .get_or_init(|| {
            Resource::builder()
                .with_service_name(get_service().as_str())
                .build()
        })
        .clone()
}

fn init_traces() -> Result<SdkTracerProvider, anyhow::Error> {
    let baggage_propagator = BaggagePropagator::new();
    let trace_context_propagator = TraceContextPropagator::new();
    let composite_propagator = TextMapCompositePropagator::new(vec![
        Box::new(baggage_propagator),
        Box::new(trace_context_propagator),
    ]);
    global::set_text_map_propagator(composite_propagator);

    let otlp_endpoint = env::var("OTEL_EXPORTER_OTLP_ENDPOINT");

    // Build the trace provider with the appropriate exporter
    let batch_config = opentelemetry_sdk::trace::BatchConfigBuilder::default()
        .with_max_queue_size(1000)
        .with_scheduled_delay(Duration::from_secs(1))
        .with_max_export_batch_size(100)
        .build();
    let provider = if otlp_endpoint.is_ok() {
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(otlp_endpoint?)
            .build()
            .expect("Failed to create span exporter");
        SdkTracerProvider::builder()
            .with_span_processor(BatchSpanProcessor::new(exporter, batch_config))
            .with_resource(get_resource())
            .build()
    } else {
        // Setup tracer provider with stdout exporter
        // that prints the spans to stdout.
        let exporter = opentelemetry_stdout::SpanExporter::default();
        SdkTracerProvider::builder()
            .with_span_processor(BatchSpanProcessor::new(exporter, batch_config))
            .with_resource(get_resource())
            .build()
    };

    global::set_tracer_provider(provider.clone());
    Ok(provider)
}

fn init_metrics() -> Result<SdkMeterProvider, anyhow::Error> {
    let otlp_endpoint = env::var("OTEL_EXPORTER_OTLP_ENDPOINT");
    let provider = if otlp_endpoint.is_ok() {
        let exporter = opentelemetry_otlp::MetricExporter::builder()
            .with_tonic()
            .with_endpoint(otlp_endpoint?)
            .build()
            .expect("Failed to create metric exporter");
        SdkMeterProvider::builder()
            .with_reader(
                PeriodicReader::builder(exporter)
                    .with_interval(Duration::from_secs(1))
                    .build(),
            )
            .with_resource(get_resource())
            .build()
    } else {
        let exporter = opentelemetry_stdout::MetricExporter::builder().build();
        SdkMeterProvider::builder()
            .with_reader(
                PeriodicReader::builder(exporter)
                    .with_interval(Duration::from_secs(1))
                    .build(),
            )
            .with_resource(get_resource())
            .build()
    };
    global::set_meter_provider(provider.clone());
    Ok(provider)
}

fn init_logs() -> Result<SdkLoggerProvider, anyhow::Error> {
    let otlp_endpoint = env::var("OTEL_EXPORTER_OTLP_ENDPOINT");

    // Build the logger provider with the appropriate exporter
    let batch_processor = if otlp_endpoint.is_ok() {
        // Setup logger provider with OTLP exporter using gRPC
        let otlp_exporter = opentelemetry_otlp::LogExporter::builder()
            .with_tonic()
            .with_endpoint(otlp_endpoint?) // Adjust as needed
            .build()
            .expect("Failed to build OTLP log exporter");
        BatchLogProcessor::builder(otlp_exporter).build()
    } else {
        // Setup logger provider with stdout exporter that prints to stdout.
        BatchLogProcessor::builder(opentelemetry_stdout::LogExporter::default()).build()
    };

    // Create a logger provider with the batch processor
    let provider = SdkLoggerProvider::builder()
        .with_log_processor(batch_processor)
        .with_resource(get_resource())
        .build();
    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_service_once_lock() {
        // Test that get_service() returns the same instance across multiple calls
        let service1 = get_service();
        let service2 = get_service();
        assert!(std::ptr::eq(service1, service2));
    }

    #[test]
    fn test_get_meter_once_lock() {
        // Test that get_meter() returns the same instance across multiple calls
        let meter1 = get_meter();
        let meter2 = get_meter();
        assert!(std::ptr::eq(meter1, meter2));
    }
}
