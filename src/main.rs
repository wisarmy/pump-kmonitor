use anyhow::Result;
use clap::{Parser, Subcommand};
use pump_kmonitor::kline::KLineManager;
use pump_kmonitor::{logger, pump};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Monitor {},
}

#[tokio::main]
pub async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();
    logger::init(true);

    match cli.command {
        Commands::Monitor {} => {
            println!("Monitoring started...");

            // Load environment variables
            let _ = dotenvy::dotenv();

            let websocket_endpoint = std::env::var("RPC_WEBSOCKET_ENDPOINT")
                .expect("RPC_WEBSOCKET_ENDPOINT environment variable is required");

            // Create a global KLineManager using environment configuration
            let kline_manager = Arc::new(Mutex::new(
                KLineManager::new()
                    .await
                    .expect("Failed to connect to Redis"),
            ));

            pump::connect_websocket(&websocket_endpoint, Arc::clone(&kline_manager)).await?;
        }
    }

    Ok(())
}
