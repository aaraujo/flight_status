use crate::error::FlightSearchError;
use crate::metrics::{inc_flight_status_error, inc_flight_status_success};
use chrono::{Duration, NaiveDate, Utc};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::Deserialize;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::env;
use tracing::{debug, error, info, instrument};

const DATE_FORMAT: &str = "%Y-%m-%d";

/// API parameters provided by model
#[derive(Debug, Deserialize, Default)]
pub struct FlightSearchArgs {
    source: String,
    destination: String,
    departure_date: Option<String>,
    return_date: Option<String>,
    service: Option<String>,
    adults: Option<u8>,
    currency: Option<String>,
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
}

#[derive(Debug, Serialize, Default)]
struct SkyscannerLocation {
    sky_id: String,
    entity_id: String,
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
                    "source": { "type": "string", "description": "Source airport code or city name (e.g., 'BOM' or 'Mumbai')" },
                    "destination": { "type": "string", "description": "Destination airport code or city name (e.g., 'DEL' or 'Delhi')" },
                    "departure_date": { "type": "string", "description": "Departure flight date in 'YYYY-MM-DD' format" },
                    "return_date": { "type": "string", "description": "Return flight date in 'YYYY-MM-DD' format" },
                    "service": { "type": "string", "description": "Class of service", "enum": ["economy", "premium_economy", "business"] },
                    "adults": { "type": "integer", "description": "Number of adults (over 12 years old)" },
                    "currency": { "type": "string", "description": "Currency code (e.g., 'USD')" }
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
        let departure_date = args.departure_date.unwrap_or_else(|| {
            let date = Utc::now() + Duration::days(30);
            date.format(DATE_FORMAT).to_string()
        });
        let service = args.service.unwrap_or_else(|| "economy".to_string());
        let adults = args.adults.unwrap_or(1);
        let children = 0; // Not in args yet
        let infants = 0; // Not in args yet
        let currency = args.currency.unwrap_or_else(|| "USD".to_string());
        let market = "US".to_string();
        // For roundtrip, use 7 days after departure date if only one date is provided
        let in_date = departure_date.clone();
        let return_date = args.return_date.unwrap_or_else(|| {
            let dep_date = NaiveDate::parse_from_str(departure_date.as_str(), DATE_FORMAT)
                .expect("Unable to parse departure_date");
            let return_date = dep_date + Duration::days(7);
            return_date.format(DATE_FORMAT).to_string()
        });
        let out_date = return_date.clone();
        // Resolve source and destination to skyId/entityId
        let source_loc = resolve_skyscanner_location(&api_key, &args.source).await?;
        let dest_loc = resolve_skyscanner_location(&api_key, &args.destination).await?;
        // Build Skyscanner query params
        let mut query_params = HashMap::new();
        query_params.insert("inDate", in_date.clone());
        query_params.insert("outDate", out_date.clone());
        query_params.insert("origin", source_loc.sky_id.clone());
        query_params.insert("originId", source_loc.entity_id.clone());
        query_params.insert("destination", dest_loc.sky_id.clone());
        query_params.insert("destinationId", dest_loc.entity_id.clone());
        query_params.insert("cabinClass", service.clone());
        query_params.insert("adults", adults.to_string());
        query_params.insert("children", children.to_string());
        query_params.insert("infants", infants.to_string());
        query_params.insert("market", market.clone());
        query_params.insert("currency", currency.clone());
        info!(
            "Calling Skyscanner flights/roundtrip/list API with: {:?}",
            query_params
        );
        let client = reqwest::Client::new();
        let response = client
            .get("https://skyscanner89.p.rapidapi.com/flights/roundtrip/list")
            .headers({
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert(
                    "X-RapidAPI-Host",
                    "skyscanner89.p.rapidapi.com".parse().unwrap(),
                );
                headers.insert("X-RapidAPI-Key", api_key.parse().unwrap());
                headers
            })
            .query(&query_params)
            .send()
            .await
            .map_err(|e| FlightSearchError::HttpRequestFailed(e.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| FlightSearchError::HttpRequestFailed(e.to_string()))?;
        if !status.is_success() {
            error!(
                "Skyscanner API call failed with status {}: response: {}",
                status, text
            );
            let error =
                FlightSearchError::ApiError(format!("Status: {}, Response: {}", status, text));
            inc_flight_status_error(status.as_u16() as u64, &error);
            return Err(error);
        }
        // Parse Skyscanner response and map to FlightOption(s)
        let data: Value = serde_json::from_str(&text)
            .map_err(|e| FlightSearchError::HttpRequestFailed(e.to_string()))?;
        debug!("Parsed Skyscanner response: {:?}", data);
        let mut flight_options = Vec::new();
        // Support both wrapped and unwrapped responses
        let itineraries = data
            .get("itineraries")
            .or_else(|| data.get("data").and_then(|d| d.get("itineraries")));
        if let Some(itineraries) = itineraries {
            if let Some(buckets) = itineraries.get("buckets").and_then(|b| b.as_array()) {
                'outer: for bucket in buckets {
                    if let Some(items) = bucket.get("items").and_then(|i| i.as_array()) {
                        for item in items {
                            // Extract airline name (first marketing carrier of first leg)
                            let airline = item
                                .get("legs")
                                .and_then(|legs| legs.as_array())
                                .and_then(|legs| legs.first())
                                .and_then(|leg| leg.get("carriers"))
                                .and_then(|carriers| carriers.get("marketing"))
                                .and_then(|marketing| marketing.as_array())
                                .and_then(|arr| arr.first())
                                .and_then(|carrier| carrier.get("name"))
                                .and_then(|n| n.as_str())
                                .unwrap_or("Unknown Airline")
                                .to_string();
                            let flight_number = item
                                .get("legs")
                                .and_then(|legs| legs.as_array())
                                .and_then(|legs| legs.first())
                                .and_then(|leg| leg.get("segments"))
                                .and_then(|segments| segments.as_array())
                                .and_then(|segment| segment.first())
                                .and_then(|leg| leg.get("flightNumber"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            // Departure and arrival from first leg
                            let departure = item
                                .get("legs")
                                .and_then(|legs| legs.as_array())
                                .and_then(|legs| legs.first())
                                .and_then(|leg| leg.get("departure"))
                                .and_then(|d| d.as_str())
                                .unwrap_or("")
                                .to_string();
                            let arrival = item
                                .get("legs")
                                .and_then(|legs| legs.as_array())
                                .and_then(|legs| legs.first())
                                .and_then(|leg| leg.get("arrival"))
                                .and_then(|a| a.as_str())
                                .unwrap_or("")
                                .to_string();
                            // Duration from first leg
                            let duration = item
                                .get("legs")
                                .and_then(|legs| legs.as_array())
                                .and_then(|legs| legs.first())
                                .and_then(|leg| leg.get("durationInMinutes"))
                                .and_then(|d| d.as_u64())
                                .map(|mins| format!("{} hours {} minutes", mins / 60, mins % 60))
                                .unwrap_or_else(|| "Unknown duration".to_string());
                            // Stops from first leg
                            let stops = item
                                .get("legs")
                                .and_then(|legs| legs.as_array())
                                .and_then(|legs| legs.first())
                                .and_then(|leg| leg.get("stopCount"))
                                .and_then(|s| s.as_u64())
                                .unwrap_or(0) as usize;
                            // Price: use pricingOptions[0].price.amount or price.raw
                            let price = item
                                .get("pricingOptions")
                                .and_then(|po| po.as_array())
                                .and_then(|arr| arr.first())
                                .and_then(|opt| opt.get("price"))
                                .and_then(|p| p.get("amount"))
                                .and_then(|a| a.as_f64())
                                .or_else(|| {
                                    item.get("price")
                                        .and_then(|p| p.get("raw"))
                                        .and_then(|a| a.as_f64())
                                })
                                .unwrap_or(0.0);
                            // Currency: use pricingOptions[0].price.currencyCode or fallback to USD
                            let currency = item
                                .get("pricingOptions")
                                .and_then(|po| po.as_array())
                                .and_then(|arr| arr.first())
                                .and_then(|opt| opt.get("price"))
                                .and_then(|p| p.get("currencyCode"))
                                .and_then(|c| c.as_str())
                                .or_else(|| {
                                    item.get("price")
                                        .and_then(|p| p.get("currency"))
                                        .and_then(|c| c.as_str())
                                })
                                .unwrap_or(&currency)
                                .to_string();
                            // Only push if price is nonzero
                            if price > 0.0 {
                                flight_options.push(FlightOption {
                                    airline,
                                    flight_number,
                                    departure,
                                    arrival,
                                    duration,
                                    stops,
                                    price,
                                    currency,
                                });
                            }
                            if flight_options.len() >= 5 {
                                break 'outer;
                            }
                        }
                    }
                }
            }
        }
        if flight_options.is_empty() {
            return Ok("No flights found for the given criteria.".to_string());
        }
        // Generate response for LLM
        let mut output = String::new();
        output.push_str("Here are some flight options:\n\n");
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
        }
        inc_flight_status_success();
        Ok(output)
    }
}

#[instrument(name = "resolve_skyscanner_location")]
async fn resolve_skyscanner_location(
    api_key: &str,
    query: &str,
) -> Result<SkyscannerLocation, FlightSearchError> {
    let client = reqwest::Client::new();
    let response = client
        .get("https://skyscanner89.p.rapidapi.com/flights/auto-complete")
        .headers({
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                "X-RapidAPI-Host",
                "skyscanner89.p.rapidapi.com".parse().unwrap(),
            );
            headers.insert("X-RapidAPI-Key", api_key.parse().unwrap());
            headers
        })
        .query(&[("query", query)])
        .send()
        .await
        .map_err(|e| FlightSearchError::HttpRequestFailed(e.to_string()))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| FlightSearchError::HttpRequestFailed(e.to_string()))?;
    if !status.is_success() {
        return Err(FlightSearchError::ApiError(format!(
            "Auto-complete failed: {}: {}",
            status, text
        )));
    }
    let data: Value = serde_json::from_str(&text)
        .map_err(|e| FlightSearchError::HttpRequestFailed(e.to_string()))?;
    // Use inputSuggest array per schema
    if let Some(suggestions) = data.get("inputSuggest").and_then(|d| d.as_array()) {
        for item in suggestions {
            if let Some(nav) = item.get("navigation") {
                if let Some(params) = nav.get("relevantFlightParams") {
                    if let (Some(sky_id), Some(entity_id)) = (
                        params.get("skyId").and_then(|v| v.as_str()),
                        params.get("entityId").and_then(|v| v.as_str()),
                    ) {
                        return Ok(SkyscannerLocation {
                            sky_id: sky_id.to_string(),
                            entity_id: entity_id.to_string(),
                        });
                    }
                }
            }
        }
    }
    Err(FlightSearchError::InvalidResponse(
        "No valid airport found in auto-complete response".to_string(),
    ))
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
