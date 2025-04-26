use crate::error::FlightSearchError;
use crate::otel;
use opentelemetry::KeyValue;
use opentelemetry::metrics::Counter;
use std::sync::OnceLock;

pub fn inc_flight_status_success() {
    flight_status_success().add(1, &[])
}

pub fn inc_flight_status_error(status: u64, error: &FlightSearchError) {
    let kind = match error {
        FlightSearchError::HttpRequestFailed(_) => "HttpRequestFailed",
        FlightSearchError::InvalidResponse(_) => "InvalidResponse",
        FlightSearchError::ApiError(_) => "ApiError",
        FlightSearchError::MissingApiKey => "MissingApiKey",
    };
    let attributes = vec![
        KeyValue::new("status", status.to_string()),
        KeyValue::new("kind", kind.to_string()),
    ];
    flight_status_error().add(1, &attributes)
}

fn flight_status_success() -> &'static Counter<u64> {
    static COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
    COUNTER.get_or_init(|| {
        let meter = otel::get_meter();
        meter
            .u64_counter("flight_status_success")
            .with_description("Number of successful flight search executions")
            .build()
    })
}

fn flight_status_error() -> &'static Counter<u64> {
    static COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
    COUNTER.get_or_init(|| {
        let meter = otel::get_meter();
        meter
            .u64_counter("flight_status_error")
            .with_description("Number of failed flight search executions")
            .build()
    })
}
