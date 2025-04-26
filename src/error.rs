use thiserror::Error;

#[derive(Debug, Error)]
pub enum FlightSearchError {
    #[error("HTTP request failed: {0}")]
    HttpRequestFailed(String),
    #[error("Invalid response: {0}")]
    InvalidResponse(String),
    #[error("API error: {0}")]
    ApiError(String),
    #[error("Missing API key")]
    MissingApiKey,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let http_error = FlightSearchError::HttpRequestFailed("connection failed".to_string());
        assert_eq!(
            http_error.to_string(),
            "HTTP request failed: connection failed"
        );

        let invalid_response = FlightSearchError::InvalidResponse("malformed JSON".to_string());
        assert_eq!(
            invalid_response.to_string(),
            "Invalid response: malformed JSON"
        );

        let api_error = FlightSearchError::ApiError("rate limit exceeded".to_string());
        assert_eq!(api_error.to_string(), "API error: rate limit exceeded");

        let missing_key = FlightSearchError::MissingApiKey;
        assert_eq!(missing_key.to_string(), "Missing API key");
    }
}
