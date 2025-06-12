use anyhow::Result;
use clap::{Parser, Subcommand};
use pump_kmonitor::kline::KLineManager;
use pump_kmonitor::{logger, pump, web};
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
    /// Start the monitoring service (WebSocket connection to Pump.fun)
    Monitor,
    /// Start the web service (HTTP API and web interface)
    Web {
        #[arg(long, default_value = "8080")]
        port: u16,
    },
}

#[tokio::main]
pub async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();
    logger::init(true);

    match cli.command {
        Commands::Monitor => {
            println!("🔍 Starting monitoring service...");
            start_monitor_service().await?;
        }
        Commands::Web { port } => {
            println!("🌐 Starting web service...");
            start_web_service(port).await?;
        }
    }

    Ok(())
}

async fn start_monitor_service() -> Result<()> {
    let websocket_endpoint = std::env::var("RPC_WEBSOCKET_ENDPOINT")
        .expect("RPC_WEBSOCKET_ENDPOINT environment variable is required");

    // Create KLineManager for monitoring service
    let kline_manager = Arc::new(Mutex::new(
        KLineManager::new()
            .await
            .expect("Failed to connect to Redis"),
    ));

    println!("📡 Connecting to WebSocket: {}", websocket_endpoint);
    
    // Start WebSocket monitoring (this will run indefinitely)
    pump::connect_websocket(&websocket_endpoint, kline_manager).await
}

async fn start_web_service(port: u16) -> Result<()> {
    // Create KLineManager for web service
    let kline_manager = Arc::new(Mutex::new(
        KLineManager::new()
            .await
            .expect("Failed to connect to Redis"),
    ));

    println!("🌐 Web interface will be available at http://localhost:{}", port);
    
    // Start web server (this will run indefinitely)
    web::start_web_server(kline_manager, port).await
}
