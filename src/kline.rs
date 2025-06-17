use crate::redis_helper;
use chrono::{Local, TimeZone, Timelike};
use redis::AsyncCommands;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KLineData {
    pub timestamp: i64,       // Timestamp representing the start time of this K-line
    pub open: String,         // Opening price (stored as String for Decimal)
    pub high: String,         // Highest price
    pub low: String,          // Lowest price
    pub close: String,        // Closing price
    pub volume_sol: String,   // Trading volume (SOL)
    pub volume_token: String, // Trading volume (Token)
    pub net_flow_sol: String, // Net flow (buy - sell) in SOL
    pub last_update: u64,     // Last update timestamp (seconds)
}

pub struct KLineManager {
    idle_timeout: Duration,
}

impl KLineManager {
    pub async fn new() -> anyhow::Result<Self> {
        // Load .env file if it exists
        let _ = dotenvy::dotenv();

        // Get K-line timeout from environment variable
        let timeout_secs = std::env::var("KLINE_TIMEOUT_SECS")
            .unwrap_or_else(|_| "60".to_string())
            .parse()
            .unwrap_or(60);

        Ok(Self {
            idle_timeout: Duration::from_secs(timeout_secs),
        })
    }

    pub async fn with_redis_url(_redis_url: &str, timeout_secs: u64) -> anyhow::Result<Self> {
        // Redis URL is ignored since we use the global pool
        warn!("with_redis_url is deprecated, using global Redis pool instead");

        Ok(Self {
            idle_timeout: Duration::from_secs(timeout_secs),
        })
    }

    // Get the minute timestamp that a given timestamp belongs to
    fn get_minute_timestamp(timestamp: i64) -> i64 {
        // Convert timestamp to DateTime, then set seconds and nanoseconds to 0 to get the whole minute
        if let Some(dt) = Local.timestamp_opt(timestamp, 0).single() {
            let dt_without_seconds = dt.with_second(0).unwrap().with_nanosecond(0).unwrap();
            dt_without_seconds.timestamp()
        } else {
            // If timestamp is invalid, return original timestamp
            timestamp
        }
    }

    // Generate Redis key for mint last activity
    fn get_mint_activity_key(mint: &str) -> String {
        format!("mint_activity:{}", mint)
    }

    // Generate Redis key
    fn get_kline_key(mint: &str, timestamp: i64) -> String {
        format!("kline:{}:{}", mint, timestamp)
    }

    fn get_mint_pattern(mint: &str) -> String {
        format!("kline:{}:*", mint)
    }

    // Add trading data
    pub async fn add_trade(
        &self,
        mint: &str,
        timestamp: i64,
        price: Decimal,
        sol_volume: Decimal,
        token_volume: Decimal,
        is_buy: bool,
    ) -> anyhow::Result<()> {
        let minute_ts = Self::get_minute_timestamp(timestamp);
        let key = Self::get_kline_key(mint, minute_ts);
        let current_time = chrono::Utc::now().timestamp() as u64;

        let mut con = redis_helper::get_connection().await?;

        // Check if K-line for this minute already exists
        let existing: Option<String> = con.get(&key).await?;

        let kline = if let Some(existing_data) = existing {
            // Update existing K-line
            let mut kline: KLineData = serde_json::from_str(&existing_data)?;

            let price_str = price.to_string();
            let high_decimal: Decimal = kline.high.parse().unwrap_or(Decimal::ZERO);
            let low_decimal: Decimal = kline.low.parse().unwrap_or(price);
            let vol_sol_decimal: Decimal = kline.volume_sol.parse().unwrap_or(Decimal::ZERO);
            let vol_token_decimal: Decimal = kline.volume_token.parse().unwrap_or(Decimal::ZERO);
            let net_flow_decimal: Decimal = kline.net_flow_sol.parse().unwrap_or(Decimal::ZERO);

            // Update highest and lowest prices
            if price > high_decimal {
                kline.high = price_str.clone();
            }
            if price < low_decimal {
                kline.low = price_str.clone();
            }

            // Update closing price
            kline.close = price_str.clone();
            kline.last_update = current_time;

            // Accumulate trading volume
            kline.volume_sol = (vol_sol_decimal + sol_volume).to_string();
            kline.volume_token = (vol_token_decimal + token_volume).to_string();

            // Update net flow (buy is positive, sell is negative)
            let flow_change = if is_buy { sol_volume } else { -sol_volume };
            kline.net_flow_sol = (net_flow_decimal + flow_change).to_string();

            // Check if low price is zero and fix it
            if kline.low == "0" {
                warn!(
                    "⚠️ Low price for mint {} is zero, setting to current price: {}, timestamp: {}",
                    mint, price, timestamp
                );
            }

            // Check if high price has increased more than 1000% compared to open price
            let open_decimal: Decimal = kline.open.parse().unwrap_or(Decimal::ZERO);
            if open_decimal > Decimal::ZERO {
                let high_decimal_current: Decimal = kline.high.parse().unwrap_or(Decimal::ZERO);
                let price_increase = (high_decimal_current - open_decimal) / open_decimal;
                let thousand_percent = Decimal::new(100, 0); // 10000% = 100.0

                if price_increase > thousand_percent {
                    let increase_percentage = price_increase * Decimal::new(100, 0);
                    warn!(
                        "⚠️ Mint {} surged {:.2}% - Open: {}, High: {}, Current: {}, timestamp: {}",
                        mint, increase_percentage, kline.open, kline.high, price, timestamp
                    );
                }
            }

            kline
        } else {
            // Create new K-line
            let initial_net_flow = if is_buy { sol_volume } else { -sol_volume };
            KLineData {
                timestamp: minute_ts,
                open: price.to_string(),
                high: price.to_string(),
                low: price.to_string(),
                close: price.to_string(),
                volume_sol: sol_volume.to_string(),
                volume_token: token_volume.to_string(),
                net_flow_sol: initial_net_flow.to_string(),
                last_update: current_time,
            }
        };

        // Save to Redis without expiration time (we handle cleanup manually)
        let kline_json = serde_json::to_string(&kline)?;
        let _: () = con.set(&key, kline_json).await?;

        // Update mint's last activity time
        let activity_key = Self::get_mint_activity_key(mint);
        let _: () = con.set(&activity_key, current_time).await?;

        Ok(())
    }

    // Check and delete all K-lines for inactive mints
    pub async fn cleanup_idle_klines(&self) -> anyhow::Result<()> {
        let mut con = redis_helper::get_connection().await?;
        let current_time = chrono::Utc::now().timestamp() as u64;

        // Get all mint activity keys
        let activity_pattern = "mint_activity:*";
        let activity_keys: Vec<String> = con.keys(activity_pattern).await?;

        for activity_key in activity_keys {
            // Get the last activity time for this mint
            if let Ok(Some(last_activity_str)) =
                con.get::<&str, Option<String>>(&activity_key).await
            {
                if let Ok(last_activity) = last_activity_str.parse::<u64>() {
                    // Check if this mint is inactive
                    if current_time - last_activity > self.idle_timeout.as_secs() {
                        // Extract mint address from the activity key
                        let mint = activity_key.strip_prefix("mint_activity:").unwrap_or("");

                        if !mint.is_empty() {
                            // Find and delete all K-lines for this inactive mint
                            let kline_pattern = Self::get_mint_pattern(mint);
                            let kline_keys: Vec<String> = con.keys(&kline_pattern).await?;

                            if !kline_keys.is_empty() {
                                info!(
                                    "🗑️ Mint {} inactive for {} seconds, deleting {} K-lines",
                                    mint,
                                    current_time - last_activity,
                                    kline_keys.len()
                                );

                                // Delete all K-lines for this mint
                                for key in &kline_keys {
                                    let _: () = con.del(key).await?;
                                }

                                // Also delete the activity tracking key
                                let _: () = con.del(&activity_key).await?;
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    // Get all K-line data for the specified mint
    pub async fn get_klines_for_mint(
        &self,
        mint: &str,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<KLineData>> {
        let mut con = redis_helper::get_connection().await?;
        let pattern = Self::get_mint_pattern(mint);

        let keys: Vec<String> = con.keys(&pattern).await?;
        let mut klines = Vec::new();

        for key in keys {
            if let Ok(Some(data)) = con.get::<&str, Option<String>>(&key).await {
                if let Ok(kline) = serde_json::from_str::<KLineData>(&data) {
                    klines.push(kline);
                }
            }
        }

        // Sort by timestamp
        klines.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

        // If limit is specified, only return the latest N entries
        if let Some(limit) = limit {
            if klines.len() > limit {
                klines = klines.into_iter().rev().take(limit).rev().collect();
            }
        }

        Ok(klines)
    }

    // Get the latest K-line data (grouped by mint)
    pub async fn get_latest_klines(
        &self,
        limit_per_mint: usize,
    ) -> anyhow::Result<Vec<(String, KLineData)>> {
        let mut con = redis_helper::get_connection().await?;
        let keys: Vec<String> = con.keys("kline:*:*").await?;

        use std::collections::HashMap;
        let mut mint_klines: HashMap<String, Vec<KLineData>> = HashMap::new();

        for key in keys {
            if let Ok(Some(data)) = con.get::<&str, Option<String>>(&key).await {
                if let Ok(kline) = serde_json::from_str::<KLineData>(&data) {
                    // Extract mint from key
                    let parts: Vec<&str> = key.split(':').collect();
                    if parts.len() >= 2 {
                        let mint = parts[1].to_string();
                        mint_klines.entry(mint).or_insert_with(Vec::new).push(kline);
                    }
                }
            }
        }

        let mut result = Vec::new();
        for (mint, mut klines) in mint_klines {
            // Sort by timestamp, take the latest ones
            klines.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
            klines.truncate(limit_per_mint);

            for kline in klines {
                result.push((mint.clone(), kline));
            }
        }

        Ok(result)
    }

    // Get statistics
    pub async fn get_stats(&self) -> anyhow::Result<(usize, usize)> {
        let mut con = redis_helper::get_connection().await?;
        let keys: Vec<String> = con.keys("kline:*:*").await?;

        let mut mints = std::collections::HashSet::new();
        for key in &keys {
            let parts: Vec<&str> = key.split(':').collect();
            if parts.len() >= 2 {
                mints.insert(parts[1].to_string());
            }
        }

        Ok((mints.len(), keys.len()))
    }

    // Get active mint statistics
    pub async fn get_active_mints(&self) -> anyhow::Result<Vec<(String, u64)>> {
        let mut con = redis_helper::get_connection().await?;
        let activity_keys: Vec<String> = con.keys("mint_activity:*").await?;

        let mut active_mints = Vec::new();
        for activity_key in activity_keys {
            if let Ok(Some(last_activity_str)) =
                con.get::<&str, Option<String>>(&activity_key).await
            {
                if let Ok(last_activity) = last_activity_str.parse::<u64>() {
                    let mint = activity_key
                        .strip_prefix("mint_activity:")
                        .unwrap_or("")
                        .to_string();
                    if !mint.is_empty() {
                        active_mints.push((mint, last_activity));
                    }
                }
            }
        }

        // Sort by last activity time (most recent first)
        active_mints.sort_by(|a, b| b.1.cmp(&a.1));

        Ok(active_mints)
    }
}
