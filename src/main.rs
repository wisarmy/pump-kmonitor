use anyhow::Result;
use clap::{Parser, Subcommand};
use pump_kmonitor::kline::KLineManager;
use pump_kmonitor::notification::NotificationManager;
use pump_kmonitor::strategy::StrategyEngine;
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
    /// Run strategy detection
    Strategy {
        /// Run strategy detection once and exit
        #[arg(long)]
        once: bool,
        /// Interval in seconds for continuous detection (default: 30)
        #[arg(long, default_value = "10")]
        interval: u64,
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
        Commands::Strategy { once, interval } => {
            println!("🎯 Starting strategy detection...");
            start_strategy_service(once, interval).await?;
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

async fn start_strategy_service(once: bool, interval: u64) -> Result<()> {
    // Create KLineManager for strategy service
    let kline_manager = Arc::new(Mutex::new(
        KLineManager::new()
            .await
            .expect("Failed to connect to Redis"),
    ));

    // Create notification manager
    let notification_manager = NotificationManager::new();

    // Check if notification script is available
    if notification_manager.is_enabled() && !notification_manager.check_script_availability() {
        println!("⚠️  通知脚本不存在: {:?}", notification_manager.get_script_path());
        println!("💡 请确保通知脚本存在并有执行权限");
    } else if notification_manager.is_enabled() {
        println!("✅ 通知功能已启用，脚本路径: {:?}", notification_manager.get_script_path());
    } else {
        println!("ℹ️  通知功能已禁用");
    }

    // Create strategy engine
    let mut strategy_engine = StrategyEngine::new(kline_manager, notification_manager);

    if once {
        println!("🔍 执行一次性策略检测...");
        strategy_engine.run_strategy_check().await?;
        println!("✅ 策略检测完成");
    } else {
        println!("🔄 启动持续策略检测，间隔: {}秒", interval);
        strategy_engine.run_continuous_check(interval).await?;
    }

    Ok(())
}
