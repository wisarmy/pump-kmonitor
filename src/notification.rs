use crate::strategy::StrategyAlert;
use anyhow::Result;
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
}

impl NotificationManager {
    /// 创建新的通知管理器
    pub fn new() -> Self {
        let script_path = std::env::var("NOTIFICATION_SCRIPT_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                // 默认脚本路径
                PathBuf::from("./scripts/notify.sh")
            });

        let enabled = std::env::var("NOTIFICATION_ENABLED")
            .unwrap_or_else(|_| "true".to_string())
            .parse()
            .unwrap_or(true);

        Self {
            script_path,
            enabled,
        }
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
            "🚨 策略告警\n\
            📍 Token: {}\n\
            🔍 策略: {}\n\
            📊 详情: {}\n\
            ⏰ 时间: {}\n\
            📈 K线数量: {}\n\
            🔗 [GMGN](https://gmgn.ai/sol/token/{})",
            alert.mint,
            alert.strategy_name,
            alert.message,
            chrono::DateTime::from_timestamp(alert.timestamp, 0)
                .map(|dt| chrono::DateTime::<chrono::Local>::from(dt).format("%Y-%m-%d %H:%M:%S").to_string())
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
}
