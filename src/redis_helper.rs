use anyhow::{Context, Result};
use redis::{AsyncCommands, Client, RedisResult, aio::ConnectionManager};
use std::sync::Arc;
use tokio::sync::{Mutex, OnceCell};
use tracing::{debug, info};

/// Global Redis connection pool
static REDIS_POOL: OnceCell<Arc<Mutex<ConnectionManager>>> = OnceCell::const_new();

/// Initialize the global Redis connection pool
pub async fn init_pool() -> Result<()> {
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/".to_string());

    info!("Initializing Redis connection pool: {}", redis_url);

    let client = Client::open(redis_url.as_str()).context("Failed to create Redis client")?;
    let connection_manager = client
        .get_connection_manager()
        .await
        .context("Failed to create Redis connection manager")?;

    REDIS_POOL
        .set(Arc::new(Mutex::new(connection_manager)))
        .map_err(|_| anyhow::anyhow!("Redis pool already initialized"))?;

    info!("Successfully initialized Redis connection pool");
    Ok(())
}

/// Get a Redis connection from the global pool
pub async fn get_connection() -> Result<tokio::sync::MutexGuard<'static, ConnectionManager>> {
    let pool = REDIS_POOL
        .get()
        .ok_or_else(|| anyhow::anyhow!("Redis pool not initialized. Call init_pool() first."))?;
    Ok(pool.lock().await)
}

/// Set key-value pair
pub async fn set<K, V>(key: K, value: V) -> Result<()>
where
    K: redis::ToRedisArgs + std::fmt::Debug + Send + Sync,
    V: redis::ToRedisArgs + std::fmt::Debug + Send + Sync,
{
    debug!("Setting key: {:?}", key);
    let mut conn = get_connection().await?;
    let _: () = conn
        .set(&key, &value)
        .await
        .with_context(|| format!("Failed to set key: {:?}", key))?;
    Ok(())
}

/// Get value by key
pub async fn get<K, V>(key: K) -> Result<Option<V>>
where
    K: redis::ToRedisArgs + std::fmt::Debug + Send + Sync,
    V: redis::FromRedisValue,
{
    debug!("Getting key: {:?}", key);
    let mut conn = get_connection().await?;
    let result: RedisResult<V> = conn.get(&key).await;

    match result {
        Ok(value) => Ok(Some(value)),
        Err(e) => {
            if e.kind() == redis::ErrorKind::TypeError {
                debug!("Key not found: {:?}", key);
                Ok(None)
            } else {
                Err(anyhow::anyhow!("Failed to get key {:?}: {}", key, e))
            }
        }
    }
}

/// Set key-value pair with expiration time in seconds
pub async fn setex<K, V>(key: K, value: V, seconds: u64) -> Result<()>
where
    K: redis::ToRedisArgs + std::fmt::Debug + Send + Sync,
    V: redis::ToRedisArgs + std::fmt::Debug + Send + Sync,
{
    debug!(
        "Setting key with expiration: {:?}, expires in: {}s",
        key, seconds
    );
    let mut conn = get_connection().await?;
    let _: () = conn
        .set_ex(&key, &value, seconds)
        .await
        .with_context(|| format!("Failed to set key with expiration: {:?}", key))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_redis_operations() {
        if init_pool().await.is_err() {
            println!("Skipping test: Redis server not available");
            return;
        }

        // Test basic operations
        set("test_key", "test_value").await.unwrap();
        let value: Option<String> = get("test_key").await.unwrap();
        assert_eq!(value, Some("test_value".to_string()));

        // Test expiration
        setex("temp_key", "temp_value", 60).await.unwrap();
        let temp_value: Option<String> = get("temp_key").await.unwrap();
        assert_eq!(temp_value, Some("temp_value".to_string()));
    }
}
