use crate::error::FlightSearchError;
use crate::metrics::{inc_flight_status_error, inc_flight_status_success};
use chrono::{Duration, Utc};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::env;
use tracing::{debug, error, info, instrument};

/// API parameters provided by model
#[derive(Debug, Deserialize, Default)]
pub struct FlightSearchArgs {
    source: String,
    destination: String,
    date: Option<String>,
    sort: Option<String>,
    service: Option<String>,
    itinerary_type: Option<String>,
    adults: Option<u8>,
    seniors: Option<u8>,
    currency: Option<String>,
    nearby: Option<String>,
    nonstop: Option<String>,
}

/// Structured response provided to model
pub struct FlightOption {
    pub airline: String,
    pub flight_number: String,
    pub departure: String,
    pub arrival: String,
    pub duration: String,
    pub stops: usize,
    pub price: f64,
    pub currency: String,
    pub booking_url: String,
}

#[derive(Debug)]
pub struct FlightSearchTool;

impl Tool for FlightSearchTool {
    const NAME: &'static str = "search_flights";
    type Error = FlightSearchError;
    type Args = FlightSearchArgs;
    type Output = String;

    async fn definition(&self, _param: String) -> ToolDefinition {
        ToolDefinition {
            name: "search_flights".to_string(),
            description: "Search for flights between two airports".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "source": { "type": "string", "description": "Source airport code (e.g., 'BOM')" },
                    "destination": { "type": "string", "description": "Destination airport code (e.g., 'DEL')" },
                    "date": { "type": "string", "description": "Flight date in 'YYYY-MM-DD' format" },
                    "sort": { "type": "string", "description": "Sort order for results", "enum": ["ML_BEST_VALUE", "PRICE", "DURATION", "EARLIEST_OUTBOUND_DEPARTURE", "EARLIEST_OUTBOUND_ARRIVAL", "LATEST_OUTBOUND_DEPARTURE", "LATEST_OUTBOUND_ARRIVAL"] },
                    "service": { "type": "string", "description": "Class of service", "enum": ["ECONOMY", "PREMIUM_ECONOMY", "BUSINESS", "FIRST"] },
                    "itinerary_type": { "type": "string", "description": "Itinerary type", "enum": ["ONE_WAY", "ROUND_TRIP"] },
                    "adults": { "type": "integer", "description": "Number of adults" },
                    "seniors": { "type": "integer", "description": "Number of seniors" },
                    "currency": { "type": "string", "description": "Currency code (e.g., 'USD')" },
                    "nearby": { "type": "string", "description": "Include nearby airports", "enum": ["yes", "no"] },
                    "nonstop": { "type": "string", "description": "Show only nonstop flights", "enum": ["yes", "no"] },
                },
                "required": ["source", "destination"]
            }),
        }
    }

    #[instrument(name = "call_flight_search_tool")]
    async fn call(&self, args: FlightSearchArgs) -> Result<String, FlightSearchError> {
        // Use the RapidAPI key from an environment variable
        let api_key = env::var("RAPIDAPI_KEY").map_err(|_| FlightSearchError::MissingApiKey)?;

        // Set default values if not provided
        let date = args.date.unwrap_or_else(|| {
            let date = Utc::now() + Duration::days(30);
            date.format("%Y-%m-%d").to_string()
        });

        let sort = args.sort.unwrap_or_else(|| "ML_BEST_VALUE".to_string());
        let service = args.service.unwrap_or_else(|| "ECONOMY".to_string());
        let itinerary_type = args.itinerary_type.unwrap_or_else(|| "ONE_WAY".to_string());
        let adults = args.adults.unwrap_or(1);
        let seniors = args.seniors.unwrap_or(0);
        let currency = args.currency.unwrap_or_else(|| "USD".to_string());
        let nearby = args.nearby.unwrap_or_else(|| "no".to_string());
        let nonstop = args.nonstop.unwrap_or_else(|| "no".to_string());

        // Build URL query parameters
        let mut query_params = HashMap::new();
        query_params.insert("date", date); // valid?
        query_params.insert("sourceAirportCode", args.source);
        query_params.insert("destinationAirportCode", args.destination);
        query_params.insert("itineraryType", itinerary_type);
        query_params.insert("sortOrder", sort);
        query_params.insert("numAdults", adults.to_string());
        query_params.insert("numSeniors", seniors.to_string());
        query_params.insert("classOfService", service);
        query_params.insert("pageNumber", "1".to_string());
        query_params.insert("nearby", nearby);
        query_params.insert("nonstop", nonstop);
        query_params.insert("currencyCode", currency.clone());

        info!("Calling flight search API with: {:?}", query_params);

        let client = reqwest::Client::new();
        let response = client
            .get("https://tripadvisor16.p.rapidapi.com/api/v1/flights/searchFlights")
            .headers({
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert(
                    "X-RapidAPI-Host",
                    "tripadvisor16.p.rapidapi.com".parse().unwrap(),
                );
                headers.insert("X-RapidAPI-Key", api_key.parse().unwrap());
                headers
            })
            .query(&query_params)
            .send()
            .await
            .map_err(|e| FlightSearchError::HttpRequestFailed(e.to_string()))?;

        // Get the status code before consuming `response`
        let status = response.status();

        // Read the response text (this consumes `response`)
        let text = response
            .text()
            .await
            .map_err(|e| FlightSearchError::HttpRequestFailed(e.to_string()))?;

        // Log the raw API response for debugging
        debug!("Raw API response:\n{}", text);

        // Check if the response is an error
        if !status.is_success() {
            error!("API call failed with status {}: response: {}", status, text);
            let error =
                FlightSearchError::ApiError(format!("Status: {}, Response: {}", status, text));
            inc_flight_status_error(status.as_u16() as u64, &error);
            return Err(error);
        }

        // Parse the response JSON
        let data: Value = serde_json::from_str(&text)
            .map_err(|e| FlightSearchError::HttpRequestFailed(e.to_string()))?;
        debug!("Received response: {:?}", data);

        // Check for API errors in the JSON response
        if let Some(error) = data.get("error") {
            let error_message = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            error!("API response contained an error: {}", error_message);
            let error = FlightSearchError::ApiError(error_message.to_string());
            inc_flight_status_error(status.as_u16() as u64, &error);
            return Err(error);
        }

        let empty_leg = json!({});

        // Extract flight options
        let mut flight_options = Vec::new();

        // Check if 'data' contains 'flights' array
        if let Some(flights) = data
            .get("data")
            .and_then(|d| d.get("flights"))
            .and_then(|f| f.as_array())
        {
            // Iterate over flight entries, taking the first 5
            for flight in flights.iter().take(5) {
                // Extract flight segments
                if let Some(segments) = flight
                    .get("segments")
                    .and_then(|s| s.as_array())
                    .and_then(|s| s.first())
                {
                    // Extract legs from the first segment
                    if let Some(legs) = segments.get("legs").and_then(|l| l.as_array()) {
                        let first_leg = legs.first().unwrap_or(&empty_leg);
                        let last_leg = legs.last().unwrap_or(&empty_leg);

                        // Extract airline name
                        let airline = first_leg
                            .get("marketingCarrier")
                            .and_then(|mc| mc.get("displayName"))
                            .and_then(|dn| dn.as_str())
                            .unwrap_or("Unknown")
                            .to_string();

                        // Extract flight number
                        let flight_number = format!(
                            "{}{}",
                            first_leg
                                .get("marketingCarrierCode")
                                .and_then(|c| c.as_str())
                                .unwrap_or(""),
                            first_leg
                                .get("flightNumber")
                                .and_then(|n| n.as_str())
                                .unwrap_or("")
                        );

                        // Extract departure and arrival times
                        let departure = first_leg
                            .get("departureDateTime")
                            .and_then(|dt| dt.as_str())
                            .unwrap_or("")
                            .to_string();

                        let arrival = last_leg
                            .get("arrivalDateTime")
                            .and_then(|dt| dt.as_str())
                            .unwrap_or("")
                            .to_string();

                        // Parse departure time or fallback to current UTC time
                        let departure_time = chrono::DateTime::parse_from_rfc3339(&departure)
                            .map(|dt| dt.with_timezone(&Utc))
                            .unwrap_or_else(|_| chrono::Utc::now());

                        // Parse arrival time or fallback to current UTC time
                        let arrival_time = chrono::DateTime::parse_from_rfc3339(&arrival)
                            .map(|dt| dt.with_timezone(&Utc))
                            .unwrap_or_else(|_| chrono::Utc::now());

                        // Calculate flight duration
                        let duration = arrival_time - departure_time;
                        let hours = duration.num_hours();
                        let minutes = duration.num_minutes() % 60;
                        let duration_str = format!("{} hours {} minutes", hours, minutes);

                        // Determine number of stops
                        let stops = if legs.len() > 1 { legs.len() - 1 } else { 0 };

                        // Extract purchase links array for price information
                        let purchase_links = flight
                            .get("purchaseLinks")
                            .and_then(|pl| pl.as_array())
                            .map(|v| v.as_slice())
                            .unwrap_or(&[]);

                        // Find the best price from purchase links
                        let best_price = purchase_links.iter().min_by_key(|p| {
                            p.get("totalPrice")
                                .and_then(|tp| tp.as_f64())
                                .unwrap_or(f64::MAX) as u64
                        });

                        // Extract pricing and booking URL if available
                        if let Some(best_price) = best_price {
                            let total_price = best_price
                                .get("totalPrice")
                                .and_then(|tp| tp.as_f64())
                                .unwrap_or(0.0);
                            let booking_url = best_price
                                .get("url")
                                .and_then(|u| u.as_str())
                                .unwrap_or("")
                                .to_string();

                            // Skip flights with price 0.0
                            if total_price == 0.0 {
                                continue;
                            }

                            // Append extracted flight options to flight_options vector
                            flight_options.push(FlightOption {
                                airline,
                                flight_number,
                                departure,
                                arrival,
                                duration: duration_str,
                                stops,
                                price: total_price,
                                currency: currency.clone(),
                                booking_url,
                            });
                        }
                    }
                }
            }
        } else {
            // Return an error if response structure is invalid
            error!("Invalid response structure: {:?}", data);
            let error_message =
                format!("Status: {}, Response: {:?}, Data: {:?}", status, text, data);
            let error = FlightSearchError::InvalidResponse(error_message.to_string());
            inc_flight_status_error(status.as_u16() as u64, &error);
            return Err(error);
        }

        // Format flight_options into a readable string
        // Check if there are any flight options
        if flight_options.is_empty() {
            return Ok("No flights found for the given criteria.".to_string());
        }

        // Initialize the output string
        let mut output = String::new();
        output.push_str("Here are some flight options:\n\n");

        // Iterate over each flight option and format the details
        for (i, option) in flight_options.iter().enumerate() {
            output.push_str(&format!("{}. **Airline**: {}\n", i + 1, option.airline));
            output.push_str(&format!(
                "   - **Flight Number**: {}\n",
                option.flight_number
            ));
            output.push_str(&format!("   - **Departure**: {}\n", option.departure));
            output.push_str(&format!("   - **Arrival**: {}\n", option.arrival));
            output.push_str(&format!("   - **Duration**: {}\n", option.duration));
            output.push_str(&format!(
                "   - **Stops**: {}\n",
                if option.stops == 0 {
                    "Non-stop".to_string()
                } else {
                    format!("{} stop(s)", option.stops)
                }
            ));
            output.push_str(&format!(
                "   - **Price**: {:.2} {}\n",
                option.price, option.currency
            ));
            output.push_str(&format!("   - **Booking URL**: {}\n\n", option.booking_url));
        }

        // Return the formatted flight options
        inc_flight_status_success();
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn cleanup_test_env() {
        unsafe { env::remove_var("RAPIDAPI_KEY") };
    }

    #[test]
    fn test_flight_search_args_validation() {
        let tool = FlightSearchTool;

        // Test with empty source
        let args = FlightSearchArgs {
            source: "".to_string(),
            destination: "DEL".to_string(),
            ..Default::default()
        };
        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.call(args));
        assert!(result.is_err());

        // Test with empty destination
        let args = FlightSearchArgs {
            source: "BOM".to_string(),
            destination: "".to_string(),
            ..Default::default()
        };
        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.call(args));
        assert!(result.is_err());
    }

    #[test]
    fn test_flight_search_tool_definition() {
        let tool = FlightSearchTool;
        let definition = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.definition("test".to_string()));

        assert_eq!(definition.name, "search_flights");
        assert!(definition.description.contains("Search for flights"));
        assert!(definition.parameters.to_string().contains("source"));
        assert!(definition.parameters.to_string().contains("destination"));
    }

    #[test]
    fn test_missing_api_key_error() {
        cleanup_test_env(); // Ensure no API key is set
        let tool = FlightSearchTool;
        let args = FlightSearchArgs {
            source: "BOM".to_string(),
            destination: "DEL".to_string(),
            ..Default::default()
        };

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.call(args));

        assert!(result.is_err());
    }
}
