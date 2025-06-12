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
    Monitor {
        #[arg(long, default_value = "8080")]
        web_port: u16,
    },
}

#[tokio::main]
pub async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();
    logger::init(true);

    match cli.command {
        Commands::Monitor { web_port } => {
            println!("Monitoring started...");
            
            // Load environment variables
            let _ = dotenvy::dotenv();
            
            // Use environment variable if available, otherwise use CLI argument
            let final_web_port = std::env::var("WEB_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(web_port);
            
            println!("Web interface will be available at http://localhost:{}", final_web_port);

            let websocket_endpoint = std::env::var("RPC_WEBSOCKET_ENDPOINT")
                .expect("RPC_WEBSOCKET_ENDPOINT environment variable is required");

            // Create a global KLineManager using environment configuration
            let kline_manager = Arc::new(Mutex::new(
                KLineManager::new()
                    .await
                    .expect("Failed to connect to Redis"),
            ));

            // Start web server in background
            let web_kline_manager = Arc::clone(&kline_manager);
            tokio::spawn(async move {
                if let Err(e) = web::start_web_server(web_kline_manager, final_web_port).await {
                    eprintln!("Web server error: {}", e);
                }
            });

            // Start WebSocket monitoring
            pump::connect_websocket(&websocket_endpoint, Arc::clone(&kline_manager)).await?;
        }
    }

    Ok(())
}
