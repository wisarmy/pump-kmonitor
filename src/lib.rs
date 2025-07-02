use std::{env, sync::Arc, time::Duration};

use anyhow::Result;
use rand::seq::IndexedRandom;
use solana_client::rpc_client::RpcClient;
use tokio::sync::OnceCell;
use tracing::{debug, warn};

pub mod constant;
pub mod kline;
pub mod logger;
pub mod notification;
pub mod pump;
pub mod pump_amm;
pub mod redis_helper;
pub mod strategy;
pub mod web;
pub mod websocket;

pub fn get_random_rpc_url() -> Result<String> {
    let cluster_urls = env::var("RPC_ENDPOINTS")?
        .split(",")
        .map(|s| s.trim().to_string())
        .collect::<Vec<String>>();
    let random_url = cluster_urls
        .choose(&mut rand::rng())
        .expect("No RPC endpoints configured")
        .clone();

    debug!("Choose rpc: {}", random_url);
    return Ok(random_url);
}

// Global RPC client pool
static RPC_CLIENT_POOL: OnceCell<Vec<Arc<RpcClient>>> = OnceCell::const_new();

// Initialize the RPC client pool with timeout configurations
pub async fn init_rpc_client_pool() -> Result<()> {
    let cluster_urls = env::var("RPC_ENDPOINTS")?
        .split(",")
        .map(|s| s.trim().to_string())
        .collect::<Vec<String>>();

    let clients: Vec<Arc<RpcClient>> = cluster_urls
        .into_iter()
        .map(|url| Arc::new(RpcClient::new(url)))
        .collect();

    RPC_CLIENT_POOL
        .set(clients)
        .map_err(|_| anyhow::anyhow!("Failed to initialize RPC client pool"))?;
    debug!(
        "Initialized RPC client pool with {} clients",
        RPC_CLIENT_POOL.get().unwrap().len()
    );
    Ok(())
}

// Get a random RPC client from the pool with retry logic
pub fn get_rpc_client() -> Result<Arc<RpcClient>> {
    let pool = RPC_CLIENT_POOL
        .get()
        .ok_or_else(|| anyhow::anyhow!("RPC client pool not initialized"))?;
    let client = pool
        .choose(&mut rand::rng())
        .ok_or_else(|| anyhow::anyhow!("No RPC clients available in pool"))?
        .clone();
    Ok(client)
}

// Health check for RPC clients
pub async fn check_rpc_client_health() -> Result<usize> {
    let pool = RPC_CLIENT_POOL
        .get()
        .ok_or_else(|| anyhow::anyhow!("RPC client pool not initialized"))?;

    let mut healthy_count = 0;
    for (i, client) in pool.iter().enumerate() {
        match client.get_health() {
            Ok(_) => {
                healthy_count += 1;
                debug!("RPC client {} is healthy", i);
            }
            Err(e) => {
                warn!("RPC client {} is unhealthy: {}", i, e);
            }
        }
    }

    debug!(
        "RPC client pool health: {}/{} clients healthy",
        healthy_count,
        pool.len()
    );
    Ok(healthy_count)
}

// Get RPC client with retry mechanism for failed requests
pub async fn get_rpc_client_with_retry<T, F>(operation: F, max_retries: u32) -> Result<T>
where
    F: Fn(Arc<RpcClient>) -> Result<T> + Send + Sync,
{
    let mut last_error = None;

    for attempt in 0..=max_retries {
        match get_rpc_client() {
            Ok(client) => {
                match operation(client) {
                    Ok(result) => return Ok(result),
                    Err(e) => {
                        warn!("RPC operation failed on attempt {}: {}", attempt + 1, e);
                        last_error = Some(e);

                        if attempt < max_retries {
                            // Exponential backoff
                            let delay = Duration::from_millis(100 * (2_u64.pow(attempt)));
                            tokio::time::sleep(delay).await;
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Failed to get RPC client on attempt {}: {}", attempt + 1, e);
                last_error = Some(e);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("All retry attempts failed")))
}

// Legacy function for backward compatibility
pub fn get_rpc_client_blocking() -> Result<Arc<RpcClient>> {
    get_rpc_client()
}
