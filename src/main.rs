mod error;
mod flight_search_tool;
mod metrics;
mod otel;

use dotenv::dotenv;
use flight_search_tool::FlightSearchTool;
use rig::agent::Agent;
use rig::completion::Prompt;
use rig::providers::openai;
use rig::providers::openai::completion::CompletionModel;
use tracing::{info, instrument};

#[instrument(skip(agent))]
async fn search_flights(
    agent: &Agent<CompletionModel>,
    query: &str,
) -> Result<String, anyhow::Error> {
    info!("Searching for flights with query: {}", query);
    let response = agent.prompt(query).await?;
    info!("Received flight search response");
    Ok(response)
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    dotenv().ok();

    // OTEL graceful shutdown on success or error exit 
    let _otel_guard = otel::init_otel()?;

    info!("Starting flight agent");

    let openai_client = openai::Client::from_env();

    // Wire up model to flight search tool
    let agent = openai_client
        .agent("gpt-4.1")
        .preamble(
            "You are a helpful assistant that can search for flights between two airports for users.",
        )
        .tool(FlightSearchTool)
        .build();

    let response = search_flights(
        &agent,
        "Find me flights from Austin to Barcelona on May 25th 2025.",
    )
    .await?;

    println!("Agent response:\n{}", response);
    info!("Shutting down flight agent");
    Ok(())
}
