use std::{env, sync::Arc};

use anyhow::Result;
use rand::seq::IndexedRandom;
use solana_client::rpc_client::RpcClient;
use tracing::debug;

pub mod constant;
pub mod kline;
pub mod logger;
pub mod notification;
pub mod pump;
pub mod strategy;
pub mod web;

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

pub fn get_rpc_client() -> Result<Arc<RpcClient>> {
    let random_url = get_random_rpc_url()?;
    let client = RpcClient::new(random_url);
    return Ok(Arc::new(client));
}
