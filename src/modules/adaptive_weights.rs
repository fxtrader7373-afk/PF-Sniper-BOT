use std::collections::HashMap;
use tracing::info;
use crate::core::error::SniperResult;

#[derive(Debug, Clone)]
pub struct WeightUpdate {
    pub weights: HashMap<String, f64>,
    pub accuracy: f64,
    pub num_trades: usize,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

pub struct AdaptiveWeights {
    min_trades: usize,
    db_path: String,
}

impl AdaptiveWeights {
    pub fn new(db_path: String, min_trades: usize) -> Self {
        Self { min_trades: min_trades.max(150), db_path }
    }

    pub fn retrain(&self) -> SniperResult<WeightUpdate> {
        info!("Adaptive weights: need {} trades to retrain (ML disabled for stability)", self.min_trades);
        // Stub: return current heuristic weights
        let mut w = HashMap::new();
        w.insert("dev_reputation".into(), 0.15);
        w.insert("holder_concentration".into(), 0.15);
        w.insert("gini_coefficient".into(), 0.10);
        w.insert("wash_trade_ratio".into(), 0.15);
        w.insert("bundled_buy_penalty".into(), 0.15);
        w.insert("entry_timing".into(), 0.15);
        w.insert("liquidity".into(), 0.15);
        Ok(WeightUpdate { weights: w, accuracy: 0.0, num_trades: 0, timestamp: chrono::Utc::now() })
    }
}
