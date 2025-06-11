use anyhow::Result;
use clap::{Parser, Subcommand};
use pump_kmonitor::{logger, pump};

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
            let websocket_endpoint = std::env::var("RPC_WEBSOCKET_ENDPOINT")
                .expect("RPC_WEBSOCKET_ENDPOINT environment variable is required");
            pump::connect_websocket(&websocket_endpoint).await?;
        }
    }

    Ok(())
}
