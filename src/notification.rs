use crate::strategy::StrategyAlert;
use anyhow::Result;
use redis::{AsyncCommands, Client as RedisClient};
use serde_json;
use std::path::PathBuf;
use std::process::Command;
use tracing::{error, info, warn};

#[derive(Debug, Clone)]
pub struct NotificationManager {
    /// 通知脚本路径
    script_path: PathBuf,
    /// 是否启用通知
    enabled: bool,
    /// Redis客户端，用于存储通知记录
    redis_client: RedisClient,
    /// 通知冷却时间（秒）
    notification_cooldown_seconds: u64,
}

impl NotificationManager {
    /// 创建新的通知管理器
    pub fn new() -> Result<Self> {
        let script_path = std::env::var("NOTIFICATION_SCRIPT_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                // 默认脚本路径
                PathBuf::from("./scripts/notify.sh")
            });

        let enabled = std::env::var("NOTIFICATION_ENABLED")
            .unwrap_or_else(|_| "true".to_string())
            .parse::<bool>()
            .unwrap_or(true);

        // 从环境变量读取通知冷却时间，默认为600秒（10分钟）
        let notification_cooldown_seconds = std::env::var("NOTIFICATION_COOLDOWN_SECONDS")
            .unwrap_or_else(|_| "600".to_string())
            .parse::<u64>()
            .unwrap_or(600);

        // 连接Redis
        let redis_url =
            std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/".to_string());
        let redis_client = RedisClient::open(redis_url)?;

        info!(
            "📱 通知管理器初始化完成 - 脚本路径: {:?}, 启用状态: {}, 冷却时间: {}秒",
            script_path, enabled, notification_cooldown_seconds
        );

        Ok(Self {
            script_path,
            enabled,
            redis_client,
            notification_cooldown_seconds,
        })
    }

    /// 发送通知
    pub async fn send_notification(&self, alert: &StrategyAlert) -> Result<()> {
        if !self.enabled {
            info!("📢 通知已禁用，跳过发送");
            return Ok(());
        }

        if !self.script_path.exists() {
            warn!("⚠️ 通知脚本不存在: {:?}", self.script_path);
            return Ok(());
        }

        // 检查是否在5分钟内已经通知过该代币
        if self.should_skip_duplicate_notification(&alert.mint).await? {
            info!(
                "🔄 代币 {} 在{}s内已通知过，跳过重复通知",
                &alert.mint, self.notification_cooldown_seconds,
            );
            return Ok(());
        }

        // 准备通知数据
        let notification_data = serde_json::json!({
            "type": "strategy_alert",
            "alert": alert,
            "formatted_message": self.format_alert_message(alert)
        });

        // 执行通知脚本
        let output = Command::new("bash")
            .arg(&self.script_path)
            .arg(notification_data.to_string())
            .output();

        match output {
            Ok(result) => {
                if result.status.success() {
                    info!("✅ 通知脚本执行成功");
                    if !result.stdout.is_empty() {
                        info!("📤 脚本输出: {}", String::from_utf8_lossy(&result.stdout));
                    }

                    // 通知成功后，记录到Redis中，避免重复通知
                    if let Err(e) = self.record_notification(&alert.mint).await {
                        warn!("⚠️ 记录通知状态失败: {}", e);
                    }
                } else {
                    let stderr = String::from_utf8_lossy(&result.stderr);
                    error!("❌ 通知脚本执行失败: {}", stderr);
                    return Err(anyhow::anyhow!("通知脚本执行失败: {}", stderr));
                }
            }
            Err(e) => {
                error!("❌ 执行通知脚本时出错: {}", e);
                return Err(anyhow::anyhow!("执行通知脚本时出错: {}", e));
            }
        }

        Ok(())
    }

    /// 格式化告警消息
    fn format_alert_message(&self, alert: &StrategyAlert) -> String {
        format!(
            "## 🚀连续上涨📈
- 🚨 策略告警
- 📍 Token: {}
- 🔍 策略: {}
- 📊 详情: {}
- ⏰ 时间: {}
- 📈 K线数量: {}
- 🔗 [GMGN](https://gmgn.ai/sol/token/{})",
            alert.mint,
            alert.strategy_name,
            alert.message,
            chrono::DateTime::from_timestamp(alert.timestamp, 0)
                .map(|dt| chrono::DateTime::<chrono::Local>::from(dt)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string())
                .unwrap_or_else(|| "未知时间".to_string()),
            alert.klines.len(),
            alert.mint
        )
    }

    /// 检查通知脚本是否可执行
    pub fn check_script_availability(&self) -> bool {
        if !self.enabled {
            return true; // 如果禁用了通知，则认为"可用"
        }

        self.script_path.exists() && self.script_path.is_file()
    }

    /// 获取脚本路径
    pub fn get_script_path(&self) -> &PathBuf {
        &self.script_path
    }

    /// 是否启用通知
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// 检查是否应该跳过重复通知（5分钟内已通知过）
    async fn should_skip_duplicate_notification(&self, mint: &str) -> Result<bool> {
        let mut conn = self.redis_client.get_multiplexed_async_connection().await?;
        let key = format!("notification:{}:recent", mint);

        // 检查键是否存在
        let exists: bool = conn.exists(&key).await?;
        Ok(exists)
    }

    /// 记录通知状态（设置可配置的冷却时间）
    async fn record_notification(&self, mint: &str) -> Result<()> {
        let mut conn = self.redis_client.get_multiplexed_async_connection().await?;
        let key = format!("notification:{}:recent", mint);
        let timestamp = chrono::Local::now().timestamp();

        // 设置键值，使用可配置的冷却时间
        let _: () = conn
            .set_ex(&key, timestamp, self.notification_cooldown_seconds)
            .await?;

        info!(
            "📝 记录通知状态: {} (冷却时间: {}秒)",
            mint, self.notification_cooldown_seconds
        );

        Ok(())
    }
}
