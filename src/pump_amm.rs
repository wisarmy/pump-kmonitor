use anyhow::Result;
use base64::{Engine as _, engine::general_purpose};
use rust_decimal::Decimal;
use serde_json::Value;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::constant::PUMP_AMM_PROGRAM;
use crate::kline::KLineManager;
use crate::websocket::WebSocketMonitor;
use crate::{get_rpc_client, redis_helper};

#[derive(Debug, Clone)]
pub struct AmmPoolData {
    pub base_token_mint: String,
    pub quote_token_mint: String,
}

impl AmmPoolData {
    pub fn get_mint(&self) -> Option<String> {
        let wsol = spl_token::native_mint::id().to_string();
        if self.base_token_mint == wsol {
            Some(self.quote_token_mint.clone())
        } else if self.quote_token_mint == wsol {
            Some(self.base_token_mint.clone())
        } else {
            None
        }
    }
}

#[derive(Debug)]
pub struct AmmTradeEvent {
    pub signature: String,
    pub slot: u64,
    pub success: bool,
    pub pool: String,
    pub user: String,
    pub token_amount: u64,
    pub sol_amount: u64,
    pub is_buy: bool,
    pub timestamp: i64,
    pub pool_base_token_reserves: u64,
    pub pool_quote_token_reserves: u64,
    pub lp_fee: u64,
    pub protocol_fee: u64,
    pub coin_creator_fee: u64,
}

#[derive(Debug)]
pub struct AmmTradeDetails {
    pub sol_amount_formatted: Decimal,
    pub token_amount_formatted: Decimal,
    pub pool_base_formatted: Decimal,
    pub pool_quote_formatted: Decimal,
    pub lp_fee_formatted: Decimal,
    pub protocol_fee_formatted: Decimal,
    pub creator_fee_formatted: Decimal,
    pub price: Decimal,
}

pub async fn connect_websocket(
    rpc_ws_endpoint: &str,
    kline_manager: Arc<Mutex<KLineManager>>,
) -> Result<()> {
    let monitor = WebSocketMonitor::new(
        rpc_ws_endpoint.to_string(),
        kline_manager,
        vec![PUMP_AMM_PROGRAM.to_string()],
        "AMM".to_string(),
    );

    monitor
        .start(
            |response: &Value, kline_manager: Arc<Mutex<KLineManager>>| {
                let response = response.clone();
                async move { handle_amm_message(&response, kline_manager).await }
            },
        )
        .await
}

pub async fn handle_amm_message(
    response: &Value,
    kline_manager: Arc<Mutex<KLineManager>>,
) -> Result<()> {
    debug!("Processing AMM message: {:#?}", response);

    if let Some(amm_trade_event) = parse_amm_trade_event(response) {
        if let Some(details) = calculate_amm_trade_details(&amm_trade_event) {
            // Skip trades with zero or invalid prices to prevent "low": "0" issues
            if details.price.is_zero() {
                warn!(
                    "Skipping AMM trade with zero price for pool {:#?}",
                    amm_trade_event
                );
                return Ok(());
            }
            // Skip micro transactions (less than 0.01 SOL) to keep K-lines clean
            let min_sol_amount = Decimal::new(1, 2); // 0.01 SOL
            if details.sol_amount_formatted < min_sol_amount {
                debug!(
                    "Skipping micro AMM transaction: SOL={}, pool={}",
                    details.sol_amount_formatted, amm_trade_event.pool
                );
                return Ok(());
            }

            // For AMM trades, we'll use the pool address as the "mint" for K-line tracking
            let pool_clone = amm_trade_event.pool.clone();
            let timestamp = amm_trade_event.timestamp;
            let price = details.price;
            let sol_amount = details.sol_amount_formatted;
            let token_amount = details.token_amount_formatted;

            // get pool data
            let pool_data = get_amm_pool_cached(Pubkey::from_str(&pool_clone)?).await?;
            let mint = pool_data
                .get_mint()
                .ok_or_else(|| anyhow::anyhow!("Failed to get mint from pool data"))?;
            let mint_clone = mint.clone();

            tokio::spawn(async move {
                let manager = kline_manager.lock().await;
                if let Err(e) = manager
                    .add_trade(&mint_clone, timestamp, price, sol_amount, token_amount)
                    .await
                {
                    error!("K-line update failed: {}", e);
                }
            });

            info!(
                "{} {} [AMM]: signature= {}, pool= {}, mint= {}, user= {}, SOL= {:.6}, tokens= {:.2}, price= {:.9}, lp_fee= {:.6}, protocol_fee= {:.6}, creator_fee= {:.6}, success= {}, time= {}",
                if amm_trade_event.is_buy {
                    "🟢"
                } else {
                    "🔴"
                },
                if amm_trade_event.is_buy {
                    "Buy"
                } else {
                    "Sell"
                },
                amm_trade_event.signature,
                amm_trade_event.pool,
                mint,
                amm_trade_event.user,
                details.sol_amount_formatted,
                details.token_amount_formatted,
                details.price,
                details.lp_fee_formatted,
                details.protocol_fee_formatted,
                details.creator_fee_formatted,
                amm_trade_event.success,
                // Convert timestamp to readable format
                chrono::DateTime::from_timestamp(amm_trade_event.timestamp, 0)
                    .map(|dt| dt
                        .with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string())
                    .unwrap_or_else(|| "Invalid timestamp".to_string())
            );
        } else {
            info!("🟡 AMM Trade detected: {:#?}", amm_trade_event);
        }
    } else {
        // Check if contains AMM instruction but parsing failed
        if contains_amm_instruction(response) {
            debug!("Contains AMM instruction but parsing failed");
        } else {
            debug!("No AMM instruction found in message");

            // Print some logs for debugging
            if let Some(logs) = response
                .get("params")
                .and_then(|p| p.get("result"))
                .and_then(|r| r.get("value"))
                .and_then(|v| v.get("logs"))
                .and_then(|l| l.as_array())
            {
                let amm_logs: Vec<_> = logs
                    .iter()
                    .filter_map(|log| log.as_str())
                    .filter(|log_str| {
                        log_str.contains("pAMMBay6")
                            || log_str.contains("Instruction: Buy")
                            || log_str.contains("Instruction: Sell")
                            || log_str.starts_with("Program data:")
                    })
                    .collect();

                if !amm_logs.is_empty() {
                    debug!("Relevant AMM logs found: {:#?}", amm_logs);
                }
            }
        }
    }
    Ok(())
}

pub fn parse_amm_trade_event(response: &Value) -> Option<AmmTradeEvent> {
    // Extract params.result
    let result = response.get("params")?.get("result")?;
    let value = result.get("value")?;
    let context = result.get("context")?;

    // Get basic information
    let signature = value.get("signature")?.as_str()?.to_string();
    let slot = context.get("slot")?.as_u64()?;
    let success = value.get("err").map(|v| v.is_null()).unwrap_or(false);

    debug!(
        "AMM transaction - signature: {}, success: {}",
        signature, success
    );

    // Check if logs contain AMM Buy/Sell instructions
    let logs = value.get("logs")?.as_array()?;
    let has_amm_program = logs.iter().any(|log| {
        if let Some(log_str) = log.as_str() {
            log_str.contains(&format!("Program {} invoke", PUMP_AMM_PROGRAM))
        } else {
            false
        }
    });

    // Determine buy/sell from logs
    let mut is_buy_transaction = None;
    for log in logs {
        if let Some(log_str) = log.as_str() {
            if log_str.contains("failed") {
                return None; // Ignore failed transactions
            }
            if log_str.contains("Program log: Instruction: Buy") {
                is_buy_transaction = Some(true);
                break;
            } else if log_str.contains("Program log: Instruction: Sell") {
                is_buy_transaction = Some(false);
                break;
            }
        }
    }

    let has_program_data = logs.iter().any(|log| {
        if let Some(log_str) = log.as_str() {
            log_str.starts_with("Program data: ")
        } else {
            false
        }
    });

    debug!(
        "AMM check - has_amm_program: {}, is_buy: {:?}, has_program_data: {}",
        has_amm_program, is_buy_transaction, has_program_data
    );

    if !has_amm_program {
        debug!("Missing AMM program in logs");
        return None;
    }

    if is_buy_transaction.is_none() {
        debug!("Missing Buy/Sell instruction in logs");
        return None;
    }

    if !has_program_data {
        debug!("Missing program data in logs");
        return None;
    }

    let is_buy = is_buy_transaction.unwrap();

    // Extract and parse program data - look for any program data in AMM context
    for log in logs {
        if let Some(log_str) = log.as_str() {
            if log_str.starts_with("Program data: ") {
                if let Some(data_str) = log_str.strip_prefix("Program data: ") {
                    debug!(
                        "Found program data: {}",
                        &data_str[..std::cmp::min(100, data_str.len())]
                    );
                    if let Some(trade_data) = decode_and_parse_amm_program_data(data_str) {
                        return Some(AmmTradeEvent {
                            signature,
                            slot,
                            success,
                            pool: trade_data.0,
                            user: trade_data.1,
                            token_amount: trade_data.2,
                            sol_amount: trade_data.3,
                            is_buy: is_buy,
                            timestamp: trade_data.4,
                            pool_base_token_reserves: trade_data.5,
                            pool_quote_token_reserves: trade_data.6,
                            lp_fee: trade_data.7,
                            protocol_fee: trade_data.8,
                            coin_creator_fee: trade_data.9,
                        });
                    }
                }
            }
        }
    }

    None
}

pub fn decode_and_parse_amm_program_data(
    program_data: &str,
) -> Option<(String, String, u64, u64, i64, u64, u64, u64, u64, u64)> {
    // Decode base64 data
    let decoded = general_purpose::STANDARD.decode(program_data).ok()?;
    debug!("Decoded AMM program data length: {}", decoded.len());

    if decoded.len() < 200 {
        warn!("AMM program data too short: {} bytes", decoded.len());
        return None;
    }

    // Skip first 8 bytes of event identifier
    let _event_type = &decoded[..8];
    let mut pos = 8;

    // Read timestamp (8 bytes)
    let timestamp = {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&decoded[pos..pos + 8]);
        u64::from_le_bytes(bytes) as i64
    };
    pos += 8;

    // Read second field (8 bytes) - either baseAmountOut (buy) or baseAmountIn (sell)
    let base_amount = {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&decoded[pos..pos + 8]);
        u64::from_le_bytes(bytes)
    };
    pos += 8;

    // Read third field (8 bytes) - either maxQuoteAmountIn (buy) or minQuoteAmountOut (sell)
    let quote_limit = {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&decoded[pos..pos + 8]);
        u64::from_le_bytes(bytes)
    };
    pos += 8;

    // Skip userBaseTokenReserves and userQuoteTokenReserves (16 bytes)
    pos += 16;

    // Read poolBaseTokenReserves (8 bytes)
    let pool_base_token_reserves = {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&decoded[pos..pos + 8]);
        u64::from_le_bytes(bytes)
    };
    pos += 8;

    // Read poolQuoteTokenReserves (8 bytes)
    let pool_quote_token_reserves = {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&decoded[pos..pos + 8]);
        u64::from_le_bytes(bytes)
    };
    pos += 8;

    // Read actual amount field (8 bytes) - either quoteAmountIn (buy) or quoteAmountOut (sell)
    let quote_amount = {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&decoded[pos..pos + 8]);
        u64::from_le_bytes(bytes)
    };
    let (sol_amount, token_amount) = if pool_base_token_reserves > pool_quote_token_reserves {
        (quote_amount, base_amount)
    } else {
        (base_amount, quote_amount)
    };

    pos += 8;

    // Skip lpFeeBasisPoints (8 bytes)
    pos += 8;

    // Read lpFee (8 bytes)
    let lp_fee = {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&decoded[pos..pos + 8]);
        u64::from_le_bytes(bytes)
    };
    pos += 8;

    // Skip protocolFeeBasisPoints (8 bytes)
    pos += 8;

    // Read protocolFee (8 bytes)
    let protocol_fee = {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&decoded[pos..pos + 8]);
        u64::from_le_bytes(bytes)
    };
    pos += 8;

    // Skip intermediate fields (16 bytes)
    pos += 16;

    // Read pool address (32 bytes)
    let pool_bytes = &decoded[pos..pos + 32];
    let pool = bs58::encode(pool_bytes).into_string();
    pos += 32;

    // Read user address (32 bytes)
    let user_bytes = &decoded[pos..pos + 32];
    let user = bs58::encode(user_bytes).into_string();
    pos += 32;

    // Skip userBaseTokenAccount (32 bytes)
    pos += 32;

    // Skip userQuoteTokenAccount (32 bytes)
    pos += 32;

    // Skip protocolFeeRecipient (32 bytes)
    pos += 32;

    // Skip protocolFeeRecipientTokenAccount (32 bytes)
    pos += 32;

    // Skip coinCreator (32 bytes)
    pos += 32;

    // Skip coinCreatorFeeBasisPoints (8 bytes)
    pos += 8;

    // Read coinCreatorFee (8 bytes)
    let coin_creator_fee = if pos + 8 <= decoded.len() {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&decoded[pos..pos + 8]);
        u64::from_le_bytes(bytes)
    } else {
        0
    };

    debug!(
        "Decoded AMM TradeEvent: pool={}, user={}, token_amount={}, sol_amount={}, timestamp={}, quote_limit={}",
        pool, user, token_amount, sol_amount, timestamp, quote_limit
    );

    Some((
        pool,
        user,
        token_amount,
        sol_amount,
        timestamp,
        pool_base_token_reserves,
        pool_quote_token_reserves,
        lp_fee,
        protocol_fee,
        coin_creator_fee,
    ))
}

pub fn calculate_amm_trade_details(amm_trade_event: &AmmTradeEvent) -> Option<AmmTradeDetails> {
    // Use Decimal for precise calculations
    let sol_divisor = Decimal::new(1_000_000_000, 0); // 10^9 for SOL
    let token_divisor = Decimal::new(1_000_000, 0); // 10^6 for tokens

    let token_amount_formatted = Decimal::from(amm_trade_event.token_amount) / token_divisor;
    let sol_amount_formatted = Decimal::from(amm_trade_event.sol_amount) / sol_divisor;
    let pool_base_formatted =
        Decimal::from(amm_trade_event.pool_base_token_reserves) / token_divisor;
    let pool_quote_formatted =
        Decimal::from(amm_trade_event.pool_quote_token_reserves) / sol_divisor;
    let lp_fee_formatted = Decimal::from(amm_trade_event.lp_fee) / sol_divisor;
    let protocol_fee_formatted = Decimal::from(amm_trade_event.protocol_fee) / sol_divisor;
    let creator_fee_formatted = Decimal::from(amm_trade_event.coin_creator_fee) / sol_divisor;

    // Calculate price (SOL per token)
    let price = if !token_amount_formatted.is_zero() {
        sol_amount_formatted / token_amount_formatted
    } else {
        Decimal::ZERO
    };

    Some(AmmTradeDetails {
        sol_amount_formatted,
        token_amount_formatted,
        pool_base_formatted,
        pool_quote_formatted,
        lp_fee_formatted,
        protocol_fee_formatted,
        creator_fee_formatted,
        price,
    })
}

pub fn contains_amm_instruction(response: &Value) -> bool {
    if let Some(logs) = response
        .get("params")
        .and_then(|p| p.get("result"))
        .and_then(|r| r.get("value"))
        .and_then(|v| v.get("logs"))
        .and_then(|l| l.as_array())
    {
        let has_amm_program = logs.iter().any(|log| {
            if let Some(log_str) = log.as_str() {
                log_str.contains(&format!("Program {} invoke", PUMP_AMM_PROGRAM))
            } else {
                false
            }
        });

        let has_instruction = logs.iter().any(|log| {
            if let Some(log_str) = log.as_str() {
                log_str.contains("Program log: Instruction: Buy")
                    || log_str.contains("Program log: Instruction: Sell")
            } else {
                false
            }
        });

        debug!(
            "contains_amm_instruction check - has_amm_program: {}, has_instruction: {}",
            has_amm_program, has_instruction
        );

        return has_amm_program && has_instruction;
    }
    debug!("contains_amm_instruction - no logs found");
    false
}

fn get_pool_key(pool: &str) -> String {
    format!("pool:{}", pool)
}

async fn get_amm_pool_cached(pool: Pubkey) -> Result<AmmPoolData> {
    // Use Redis connection pool instead of creating new connection
    if let Some(data) = redis_helper::get::<_, String>(get_pool_key(&pool.to_string())).await? {
        debug!("Cache hit for pool {}", pool);
        let parts: Vec<&str> = data.split(',').collect();
        if parts.len() == 2 {
            return Ok(AmmPoolData {
                base_token_mint: parts[0].to_string(),
                quote_token_mint: parts[1].to_string(),
            });
        }
    }
    debug!("Cache miss for pool {}", pool);
    let pool_data = get_amm_pool(pool)?;

    // Cache the result with 1 hour expiration
    redis_helper::setex(
        get_pool_key(&pool.to_string()),
        format!(
            "{},{}",
            pool_data.base_token_mint, pool_data.quote_token_mint
        ),
        3600, // 1 hour
    )
    .await?;
    Ok(pool_data)
}

fn get_amm_pool(pool: Pubkey) -> Result<AmmPoolData> {
    let client = get_rpc_client().unwrap();
    let data = client.get_account_data(&pool)?;

    if data.len() < 200 {
        return Err(anyhow::anyhow!("Pool data too short: {} bytes", data.len()));
    }

    let mut pos = 0;

    // Skip discriminator (8 bytes)
    pos += 8;

    // Skip some bytes
    pos += 35;

    // Read base token mint (32 bytes)
    let base_token_mint = bs58::encode(&data[pos..pos + 32]).into_string();
    pos += 32;

    // Read quote token mint (32 bytes)
    let quote_token_mint = bs58::encode(&data[pos..pos + 32]).into_string();

    Ok(AmmPoolData {
        base_token_mint,
        quote_token_mint,
    })
}
