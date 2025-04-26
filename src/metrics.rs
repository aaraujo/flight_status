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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flight_status_success_once_lock() {
        // Test that flight_status_success() returns the same instance across multiple calls
        let counter1 = flight_status_success();
        let counter2 = flight_status_success();
        assert!(std::ptr::eq(counter1, counter2));
    }

    #[test]
    fn test_flight_status_error_once_lock() {
        // Test that flight_status_error() returns the same instance across multiple calls
        let counter1 = flight_status_error();
        let counter2 = flight_status_error();
        assert!(std::ptr::eq(counter1, counter2));
    }

    #[test]
    fn test_metrics_increment() {
        // Test that metrics can be incremented
        // Note: This test doesn't verify the actual metric values
        // as that would require a running OpenTelemetry collector
        inc_flight_status_success();
        inc_flight_status_error(
            404,
            &FlightSearchError::HttpRequestFailed("test".to_string()),
        );
    }
}
