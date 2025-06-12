use crate::kline::{KLineData, KLineManager};
use crate::notification::NotificationManager;
use anyhow::Result;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyAlert {
    pub mint: String,
    pub strategy_name: String,
    pub message: String,
    pub timestamp: i64,
    pub klines: Vec<KLineData>,
}

#[derive(Debug, Clone)]
pub struct ConsecutiveRisingPattern {
    /// 连续上涨K线数量
    pub consecutive_count: usize,
    /// 是否要求递增涨幅
    pub require_increasing_gains: bool,
    /// 最小涨幅阈值 (百分比)
    pub min_gain_threshold: Decimal,
}

impl Default for ConsecutiveRisingPattern {
    fn default() -> Self {
        Self {
            consecutive_count: 4,
            require_increasing_gains: true,
            min_gain_threshold: Decimal::new(1, 2), // 1%
        }
    }
}

pub struct StrategyEngine {
    kline_manager: Arc<Mutex<KLineManager>>,
    notification_manager: NotificationManager,
    /// 存储每个mint最近检查的K线数据，避免重复检查
    last_checked: HashMap<String, u64>,
}

impl StrategyEngine {
    pub fn new(
        kline_manager: Arc<Mutex<KLineManager>>,
        notification_manager: NotificationManager,
    ) -> Self {
        Self {
            kline_manager,
            notification_manager,
            last_checked: HashMap::new(),
        }
    }

    /// 运行策略检测
    pub async fn run_strategy_check(&mut self) -> Result<()> {
        info!("🔍 开始运行策略检测...");

        // 获取所有活跃的mint
        let active_mints = {
            let manager = self.kline_manager.lock().await;
            manager.get_active_mints().await?
        };

        info!("📊 发现 {} 个活跃 mint", active_mints.len());

        for (mint, last_activity) in active_mints {
            // 检查是否需要检测这个mint（避免重复检测相同的数据）
            if let Some(&last_check) = self.last_checked.get(&mint) {
                if last_activity <= last_check {
                    continue;
                }
            }

            // 获取该mint的K线数据
            let klines = {
                let manager = self.kline_manager.lock().await;
                manager.get_klines_for_mint(&mint, Some(10)).await?
            };

            // 检测连续上涨模式
            if let Some(alert) = self.check_consecutive_rising_pattern(&mint, &klines) {
                info!("🚨 策略触发: {} - {}", alert.strategy_name, alert.message);
                
                // 发送通知
                if let Err(e) = self.notification_manager.send_notification(&alert).await {
                    warn!("❌ 通知发送失败: {}", e);
                } else {
                    info!("✅ 通知发送成功");
                }
            }

            // 更新最后检查时间
            self.last_checked.insert(mint, last_activity);
        }

        Ok(())
    }

    /// 检测连续上涨模式
    fn check_consecutive_rising_pattern(
        &self,
        mint: &str,
        klines: &[KLineData],
    ) -> Option<StrategyAlert> {
        let pattern = ConsecutiveRisingPattern::default();

        // 检查K线数量是否足够
        if klines.len() < pattern.consecutive_count {
            return None;
        }

        // 按时间戳排序，确保顺序正确
        let mut sorted_klines = klines.to_vec();
        sorted_klines.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

        // 取最近的N根K线
        let recent_klines = &sorted_klines[sorted_klines.len() - pattern.consecutive_count..];

        // 检查是否连续上涨
        let mut gains = Vec::new();
        let mut is_consecutive_rising = true;

        for i in 0..recent_klines.len() {
            let kline = &recent_klines[i];
            
            // 解析开盘价和收盘价
            let open_price = match kline.open.parse::<Decimal>() {
                Ok(price) => price,
                Err(_) => continue,
            };
            
            let close_price = match kline.close.parse::<Decimal>() {
                Ok(price) => price,
                Err(_) => continue,
            };

            // 检查是否为阳线（收盘价 > 开盘价）
            if close_price <= open_price {
                is_consecutive_rising = false;
                break;
            }

            // 计算实体涨幅（只考虑实体部分）
            let gain = if open_price > Decimal::ZERO {
                (close_price - open_price) / open_price * Decimal::new(100, 0)
            } else {
                Decimal::ZERO
            };

            // 检查是否满足最小涨幅要求
            if gain < pattern.min_gain_threshold {
                is_consecutive_rising = false;
                break;
            }

            gains.push(gain);
        }

        if !is_consecutive_rising {
            return None;
        }

        // 如果要求递增涨幅，检查每根K线的涨幅是否递增
        if pattern.require_increasing_gains {
            for i in 1..gains.len() {
                if gains[i] <= gains[i - 1] {
                    return None;
                }
            }
        }

        // 构造告警消息
        let total_gain: Decimal = gains.iter().sum();
        let gain_sequence: Vec<String> = gains.iter().map(|g| format!("{:.2}%", g)).collect();

        let message = format!(
            "发现连续{}根递增上涨K线！总涨幅: {:.2}%, 涨幅序列: [{}]",
            pattern.consecutive_count,
            total_gain,
            gain_sequence.join(", ")
        );

        Some(StrategyAlert {
            mint: mint.to_string(),
            strategy_name: "连续递增上涨模式".to_string(),
            message,
            timestamp: chrono::Local::now().timestamp(),
            klines: recent_klines.to_vec(),
        })
    }

    /// 持续运行策略检测
    pub async fn run_continuous_check(&mut self, interval_secs: u64) -> Result<()> {
        info!("🔄 开始持续策略检测，检测间隔: {}秒", interval_secs);
        
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(interval_secs));
        
        loop {
            interval.tick().await;
            
            if let Err(e) = self.run_strategy_check().await {
                warn!("❌ 策略检测出错: {}", e);
            }
        }
    }
}
