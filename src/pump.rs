use anyhow::Result;
use base64::{Engine as _, engine::general_purpose};
use rust_decimal::Decimal;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::constant::PUMP_PROGRAM;
use crate::kline::KLineManager;
use crate::websocket::WebSocketMonitor;

#[derive(Debug)]
pub struct TradeEvent {
    pub signature: String,
    pub slot: u64,
    pub success: bool,
    pub mint: String,
    pub user: String,
    pub sol_amount: u64,
    pub token_amount: u64,
    pub is_buy: bool,
    pub timestamp: i64,
    pub virtual_sol_reserves: u64,
    pub virtual_token_reserves: u64,
    pub real_sol_reserves: u64,
    pub real_token_reserves: u64,
}

#[derive(Debug)]
pub struct TradeDetails {
    pub sol_amount_formatted: Decimal,
    pub token_amount_formatted: Decimal,
    pub virtual_sol_formatted: Decimal,
    pub virtual_token_formatted: Decimal,
    pub real_sol_formatted: Decimal,
    pub real_token_formatted: Decimal,
    pub price: Decimal,
    pub market_cap: Decimal,
}

pub async fn connect_websocket(
    rpc_ws_endpoint: &str,
    kline_manager: Arc<Mutex<KLineManager>>,
) -> Result<()> {
    let monitor = WebSocketMonitor::new(
        rpc_ws_endpoint.to_string(),
        kline_manager,
        vec![PUMP_PROGRAM.to_string()],
        "PUMP".to_string(),
    );

    monitor
        .start(
            |response: &Value, kline_manager: Arc<Mutex<KLineManager>>| {
                let response = response.clone();
                async move { handle_pump_message(&response, kline_manager).await }
            },
        )
        .await
}

pub async fn handle_pump_message(
    response: &Value,
    kline_manager: Arc<Mutex<KLineManager>>,
) -> Result<()> {
    if let Some(trade_events) = parse_trade_event(response) {
        debug!("Parsed PUMP trade events: {:#?}", trade_events);

        for trade_event in trade_events {
            if let Err(e) = process_trade_event(trade_event, kline_manager.clone()).await {
                error!("Failed to process PUMP trade event: {}", e);
            }
        }
    } else {
        // Check if contains Pump instruction but parsing failed
        if contains_pump_instruction(response) {
            debug!("Contains Pump instruction but parsing failed");
        }
    }
    Ok(())
}

pub async fn process_trade_event(
    trade_event: TradeEvent,
    kline_manager: Arc<Mutex<KLineManager>>,
) -> Result<()> {
    if let Some(details) = calculate_trade_details(&trade_event) {
        // Skip trades with zero or invalid prices to prevent "low": "0" issues
        if details.price.is_zero() {
            warn!("Skipping trade with zero price for mint {:#?}", trade_event);
            return Ok(());
        }
        // Skip micro transactions to keep K-lines clean
        let min_sol_amount = std::env::var("MIN_SOL_AMOUNT_PUMP")
            .unwrap_or_else(|_| "0.01".to_string())
            .parse::<Decimal>()
            .unwrap_or_else(|_| Decimal::new(1, 2)); // default 0.01 SOL

        // Log configuration on first use (using static to avoid repeated logs)
        use std::sync::Once;
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            info!("📊 Pump配置 - 最小SOL金额: {}", min_sol_amount);
        });
        if details.sol_amount_formatted < min_sol_amount {
            debug!(
                "Skipping micro transaction: SOL={}, mint={}",
                details.sol_amount_formatted, trade_event.mint
            );
            return Ok(());
        }

        // Update K-line data
        let mint_clone = trade_event.mint.clone();
        let timestamp = trade_event.timestamp;
        let price = details.price;
        let sol_amount = details.sol_amount_formatted;
        let token_amount = details.token_amount_formatted;

        let is_buy = trade_event.is_buy;
        tokio::spawn(async move {
            let manager = kline_manager.lock().await;
            if let Err(e) = manager
                .add_trade(
                    &mint_clone,
                    timestamp,
                    price,
                    sol_amount,
                    token_amount,
                    is_buy,
                    false,
                )
                .await
            {
                error!("K-line update failed: {}", e);
            }
        });

        info!(
            "{} {} [PUMP]: signature= {}, mint= {}, user= {}, SOL= {:.6}, tokens= {:.2}, price= {:.9}, market_cap= {:.2}, success= {}, time= {}",
            if trade_event.is_buy { "🟢" } else { "🔴" },
            if trade_event.is_buy { "Buy" } else { "Sell" },
            trade_event.signature,
            trade_event.mint,
            trade_event.user,
            details.sol_amount_formatted,
            details.token_amount_formatted,
            details.price,
            details.market_cap,
            trade_event.success,
            // Convert timestamp to readable format
            chrono::DateTime::from_timestamp(trade_event.timestamp, 0)
                .map(|dt| dt
                    .with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string())
                .unwrap_or_else(|| "Invalid timestamp".to_string())
        );
    } else {
        info!("🟡 Trade detected: {:#?}", trade_event);
    }
    Ok(())
}

pub fn parse_trade_event(response: &Value) -> Option<Vec<TradeEvent>> {
    // Extract params.result
    let result = response.get("params")?.get("result")?;

    let value = result.get("value")?;
    let context = result.get("context")?;

    // Get basic information
    let signature = value.get("signature")?.as_str()?.to_string();
    let slot = context.get("slot")?.as_u64()?;
    let success = value.get("err").map(|v| v.is_null()).unwrap_or(false);

    // Check if logs contain Buy/Sell instructions
    let logs = value.get("logs")?.as_array()?;
    let has_pump_instruction = logs.iter().any(|log| {
        if let Some(log_str) = log.as_str() {
            log_str.contains("Program log: Instruction: Buy")
                || log_str.contains("Program log: Instruction: Sell")
        } else {
            false
        }
    });

    if !has_pump_instruction {
        return None;
    }

    // Check for failed instructions
    let has_failed_instruction = logs.iter().any(|log| {
        if let Some(log_str) = log.as_str() {
            log_str.contains("failed")
        } else {
            false
        }
    });

    if has_failed_instruction {
        debug!("Transaction contains failed instruction, ignoring");
        return None; // Ignore transactions with failed instructions
    }

    let mut events = vec![];
    let mut current_is_buy = false;

    // Extract and parse program data that comes AFTER Buy/Sell instruction
    for log in logs {
        if let Some(log_str) = log.as_str() {
            // Check if this is a Buy/Sell instruction
            if log_str.contains("Program log: Instruction: Buy") {
                current_is_buy = true;
                debug!("Found PUMP Buy instruction in logs");
            } else if log_str.contains("Program log: Instruction: Sell") {
                current_is_buy = false;
                debug!("Found PUMP Sell instruction in logs");
            }

            // Parse program data
            if log_str.starts_with("Program data: ") {
                if let Some(data_str) = log_str.strip_prefix("Program data: ") {
                    debug!(
                        "Found program data: {}",
                        &data_str[..std::cmp::min(100, data_str.len())]
                    );
                    if let Some(trade_data) = decode_and_parse_program_data(data_str) {
                        let trade_event = TradeEvent {
                            signature: signature.clone(),
                            slot,
                            success,
                            mint: trade_data.0,
                            user: trade_data.1,
                            sol_amount: trade_data.2,
                            token_amount: trade_data.3,
                            is_buy: current_is_buy,
                            timestamp: trade_data.4,
                            virtual_sol_reserves: trade_data.5,
                            virtual_token_reserves: trade_data.6,
                            real_sol_reserves: trade_data.7,
                            real_token_reserves: trade_data.8,
                        };
                        events.push(trade_event);
                    }
                }
            }
        }
    }

    if events.is_empty() {
        None
    } else {
        Some(events)
    }
}

fn decode_and_parse_program_data(
    program_data: &str,
) -> Option<(String, String, u64, u64, i64, u64, u64, u64, u64)> {
    // Decode base64 data
    let decoded = general_purpose::STANDARD.decode(program_data).ok()?;

    if decoded.len() < 129 {
        return None;
    }

    // Skip first 8 bytes of event identifier
    let _event_type = &decoded[..8];

    // From 8th byte is mint address (32 bytes)
    let mint_bytes = &decoded[8..40];
    let mint = bs58::encode(mint_bytes).into_string();

    let mut pos = 40;

    // Read sol_amount (8 bytes)
    let sol_amount = {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&decoded[pos..pos + 8]);
        u64::from_le_bytes(bytes)
    };
    pos += 8;

    // Read token_amount (8 bytes)
    let token_amount = {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&decoded[pos..pos + 8]);
        u64::from_le_bytes(bytes)
    };
    pos += 8;

    // Skip is_buy (1 byte) - we handle this in the caller
    pos += 1;

    // Read user address (32 bytes)
    let user_bytes = &decoded[pos..pos + 32];
    let user = bs58::encode(user_bytes).into_string();
    pos += 32;

    // Read timestamp (8 bytes)
    let timestamp = {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&decoded[pos..pos + 8]);
        u64::from_le_bytes(bytes) as i64
    };
    pos += 8;

    // Read virtual_sol_reserves (8 bytes)
    let virtual_sol_reserves = {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&decoded[pos..pos + 8]);
        u64::from_le_bytes(bytes)
    };
    pos += 8;

    // Read virtual_token_reserves (8 bytes)
    let virtual_token_reserves = {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&decoded[pos..pos + 8]);
        u64::from_le_bytes(bytes)
    };
    pos += 8;

    // Read real_sol_reserves (8 bytes)
    let real_sol_reserves = {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&decoded[pos..pos + 8]);
        u64::from_le_bytes(bytes)
    };
    pos += 8;

    // Read real_token_reserves (8 bytes)
    let real_token_reserves = {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&decoded[pos..pos + 8]);
        u64::from_le_bytes(bytes)
    };

    debug!(
        "Decoded TradeEvent: sol_amount={}, token_amount={}, user={}, timestamp={}",
        sol_amount, token_amount, user, timestamp
    );

    Some((
        mint,
        user,
        sol_amount,
        token_amount,
        timestamp,
        virtual_sol_reserves,
        virtual_token_reserves,
        real_sol_reserves,
        real_token_reserves,
    ))
}

pub fn calculate_trade_details(trade_event: &TradeEvent) -> Option<TradeDetails> {
    // Use Decimal for precise calculations
    let sol_divisor = Decimal::new(1_000_000_000, 0); // 10^9 for SOL
    let token_divisor = Decimal::new(1_000_000, 0); // 10^6 for tokens
    let total_supply = Decimal::new(1_000_000_000, 0); // 1B tokens total supply

    let sol_amount_formatted = Decimal::from(trade_event.sol_amount) / sol_divisor;
    let token_amount_formatted = Decimal::from(trade_event.token_amount) / token_divisor;
    let virtual_sol_formatted = Decimal::from(trade_event.virtual_sol_reserves) / sol_divisor;
    let virtual_token_formatted = Decimal::from(trade_event.virtual_token_reserves) / token_divisor;
    let real_sol_formatted = Decimal::from(trade_event.real_sol_reserves) / sol_divisor;
    let real_token_formatted = Decimal::from(trade_event.real_token_reserves) / token_divisor;

    // Calculate price (SOL per token)
    let price = if !token_amount_formatted.is_zero() {
        sol_amount_formatted / token_amount_formatted
    } else {
        Decimal::ZERO
    };

    // Calculate market cap (assuming total supply of 1B tokens)
    let market_cap = price * total_supply;

    Some(TradeDetails {
        sol_amount_formatted,
        token_amount_formatted,
        virtual_sol_formatted,
        virtual_token_formatted,
        real_sol_formatted,
        real_token_formatted,
        price,
        market_cap,
    })
}

pub fn contains_pump_instruction(response: &Value) -> bool {
    if let Some(logs) = response
        .get("params")
        .and_then(|p| p.get("result"))
        .and_then(|r| r.get("value"))
        .and_then(|v| v.get("logs"))
        .and_then(|l| l.as_array())
    {
        return logs.iter().any(|log| {
            if let Some(log_str) = log.as_str() {
                log_str.contains("Program log: Instruction: Buy")
                    || log_str.contains("Program log: Instruction: Sell")
            } else {
                false
            }
        });
    }
    false
}
