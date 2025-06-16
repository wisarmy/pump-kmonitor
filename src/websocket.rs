use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

use crate::kline::KLineManager;

pub struct WebSocketMonitor {
    pub endpoint: String,
    pub kline_manager: Arc<Mutex<KLineManager>>,
    pub program_addresses: Vec<String>,
    pub monitor_name: String,
}

impl WebSocketMonitor {
    pub fn new(
        endpoint: String,
        kline_manager: Arc<Mutex<KLineManager>>,
        program_addresses: Vec<String>,
        monitor_name: String,
    ) -> Self {
        Self {
            endpoint,
            kline_manager,
            program_addresses,
            monitor_name,
        }
    }

    pub async fn start<F>(&self, message_handler: F) -> Result<()>
    where
        F: Fn(&Value, Arc<Mutex<KLineManager>>) -> Result<()> + Send + Sync + 'static,
    {
        let mut reconnect_attempts = 0;
        const MAX_RECONNECT_ATTEMPTS: u32 = 10;
        const INITIAL_RECONNECT_DELAY: u64 = 5; // seconds

        loop {
            match self.connect_internal(&message_handler).await {
                Ok(_) => {
                    // Reset reconnection counter on successful connection
                    reconnect_attempts = 0;
                    info!(
                        "{} WebSocket connection restored successfully",
                        self.monitor_name
                    );
                }
                Err(e) => {
                    reconnect_attempts += 1;
                    error!(
                        "{} WebSocket connection failed (attempt {}/{}): {}",
                        self.monitor_name, reconnect_attempts, MAX_RECONNECT_ATTEMPTS, e
                    );

                    if reconnect_attempts >= MAX_RECONNECT_ATTEMPTS {
                        error!("Max reconnection attempts reached. Giving up.");
                        return Err(e);
                    }

                    // Exponential backoff: 5s, 10s, 20s, 40s, etc. (max 5 minutes)
                    let delay = std::cmp::min(
                        INITIAL_RECONNECT_DELAY * 2_u64.pow(reconnect_attempts - 1),
                        300, // max 5 minutes
                    );

                    warn!("Attempting to reconnect in {} seconds...", delay);
                    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                }
            }
        }
    }

    async fn connect_internal<F>(&self, message_handler: &F) -> Result<()>
    where
        F: Fn(&Value, Arc<Mutex<KLineManager>>) -> Result<()> + Send + Sync + 'static,
    {
        info!(
            "Connecting to {} WebSocket server: {}",
            self.monitor_name, self.endpoint
        );

        let (ws_stream, _) = connect_async(&self.endpoint)
            .await
            .context("Failed to connect to WebSocket server")?;

        info!("Connected to {} WebSocket server", self.monitor_name);

        let (write, mut read) = ws_stream.split();

        // Start periodic cleanup task for K-line data
        let kline_manager_clone = Arc::clone(&self.kline_manager);
        let monitor_name = self.monitor_name.clone();
        let kline_check_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                let manager = kline_manager_clone.lock().await;
                if let Err(e) = manager.cleanup_idle_klines().await {
                    error!("{} K-line cleanup failed: {}", monitor_name, e);
                }
            }
        });

        // Wrap write in Arc<Mutex<>> for sharing between tasks
        let write_arc = Arc::new(Mutex::new(write));

        // Send subscription request
        let logs_subscribe = json!({
              "jsonrpc": "2.0",
              "id": 1,
              "method": "logsSubscribe",
              "params": [
                {
                  "mentions": self.program_addresses
                },
                {
                  "commitment": "confirmed"
                }
              ]
        });

        let msg = Message::text(logs_subscribe.to_string());
        {
            let mut writer = write_arc.lock().await;
            if let Err(e) = writer.send(msg).await {
                kline_check_task.abort();
                return Err(anyhow::anyhow!(
                    "Failed to send {} subscription message: {}",
                    self.monitor_name,
                    e
                ));
            }
        }

        info!(
            "{} Subscription request sent successfully",
            self.monitor_name
        );

        // Start ping task to keep connection alive
        let write_clone = Arc::clone(&write_arc);
        let monitor_name_clone = self.monitor_name.clone();
        let ping_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                let mut writer = write_clone.lock().await;
                if let Err(e) = writer.send(Message::Ping(vec![].into())).await {
                    error!("Failed to send ping: {}", e);
                    break;
                }
                debug!("Ping sent to keep {} connection alive", monitor_name_clone);
            }
        });

        // Main message processing loop
        while let Some(message) = read.next().await {
            match message {
                Ok(Message::Text(text)) => {
                    let response: serde_json::Value = serde_json::from_str(&text).unwrap();

                    // check if response contains "method" field
                    if let Some(method) = response.get("method") {
                        if method == "logsNotification" {
                            if let Err(e) =
                                message_handler(&response, Arc::clone(&self.kline_manager))
                            {
                                debug!("{} message handling failed: {}", self.monitor_name, e);
                            }
                        }
                    } else {
                        debug!(
                            "Received {} subscription response: {:#?}",
                            self.monitor_name, response
                        );
                    }
                }
                Ok(Message::Pong(_)) => {
                    debug!("Received pong from {} server", self.monitor_name);
                }
                Ok(Message::Ping(data)) => {
                    debug!(
                        "Received ping from {} server, sending pong",
                        self.monitor_name
                    );
                    let mut writer = write_arc.lock().await;
                    if let Err(e) = writer.send(Message::Pong(data)).await {
                        error!("Failed to send pong: {}", e);
                        break;
                    }
                }
                Ok(Message::Close(close)) => {
                    info!(
                        "{} Connection closed by server: {:?}",
                        self.monitor_name, close
                    );
                    break;
                }
                Ok(Message::Binary(_)) => {
                    debug!("Received binary message (ignored)");
                }
                Ok(Message::Frame(_)) => {
                    debug!("Received frame message (ignored)");
                }
                Err(e) => {
                    error!("Error receiving {} message: {:?}", self.monitor_name, e);
                    break;
                }
            }
        }

        // Clean up tasks when connection closes
        kline_check_task.abort();
        ping_task.abort();

        Err(anyhow::anyhow!(
            "{} WebSocket connection closed",
            self.monitor_name
        ))
    }
}
