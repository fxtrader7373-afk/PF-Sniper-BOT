//! Adaptive Weights — logistic regression retrained from real trade outcomes.
//!
//! This module continuously improves the scoring_engine weights by:
//! 1. Collecting labeled training data (signal vector + binary outcome)
//! 2. Fitting a logistic regression model
//! 3. Updating scoring_engine weights when the model is statistically significant
//!
//! Constraint: does not retrain meaningfully below ~150-200 closed trades.
//! Until then, it runs on fixed heuristic weights.

use linfa::traits::Fit;
use linfa::dataset::Dataset;
use linfa_logistic::LogisticRegression;
use ndarray::{Array1, Array2};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{info, warn};

use crate::core::error::{SniperError, SniperResult};
use crate::core::types::{SignalVector, TradeJournalEntry};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeightUpdate {
    pub weights: HashMap<String, f64>,
    pub accuracy: f64,
    pub num_trades: usize,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

pub struct AdaptiveWeights {
    min_trades: usize,
    db_path: String,
    feature_names: Vec<String>,
}

impl AdaptiveWeights {
    pub fn new(db_path: String, min_trades: usize) -> Self {
        Self {
            min_trades: min_trades.max(150), // Enforce minimum
            db_path,
            feature_names: vec![
                "dev_reputation".to_string(),
                "holder_concentration".to_string(),
                "gini_coefficient".to_string(),
                "wash_trade_ratio".to_string(),
                "bundled_buy_penalty".to_string(),
                "entry_timing".to_string(),
                "liquidity".to_string(),
            ],
        }
    }

    /// Retrain weights from the trade journal
    pub fn retrain(&self) -> SniperResult<WeightUpdate> {
        let conn = Connection::open(&self.db_path)
            .map_err(|e| SniperError::DatabaseError { source: e })?;

        let data = self.load_training_data(&conn)?;

        if data.len() < self.min_trades {
            warn!(
                "Insufficient trades for retraining: {} < {}",
                data.len(), self.min_trades
            );
            return Err(SniperError::Unknown {
                msg: format!("Need at least {} trades, have {}", self.min_trades, data.len()),
            });
        }

        let (x, y) = self.build_feature_matrix(&data)?;
        let dataset = Dataset::new(x, y);

        let model = LogisticRegression::default()
            .fit(&dataset)
            .map_err(|e| SniperError::Unknown {
                msg: format!("Logistic regression fit failed: {:?}", e),
            })?;

        // Extract learned weights
        let weights: HashMap<String, f64> = self.feature_names.iter()
            .zip(model.coef().iter())
            .map(|(name, &w)| (name.clone(), w))
            .collect();

        // Compute accuracy on training data
        let predictions = model.predict(&dataset);
        let accuracy = predictions.iter()
            .zip(dataset.targets().iter())
            .filter(|(pred, actual)| pred == actual)
            .count() as f64 / dataset.nsamples() as f64;

        info!("Adaptive weights retrained: accuracy={:.2}%, trades={}", accuracy * 100.0, data.len());

        Ok(WeightUpdate {
            weights,
            accuracy,
            num_trades: data.len(),
            timestamp: chrono::Utc::now(),
        })
    }

    /// Load training data from the trade journal
    fn load_training_data(&self, conn: &Connection) -> SniperResult<Vec<(SignalVector, bool)>> {
        let mut stmt = conn.prepare(
            "SELECT signal_vector_json, net_pnl_pct
             FROM trade_journal
             WHERE was_override = 0
             ORDER BY exit_time DESC
             LIMIT 1000"
        )?;

        let mut data = Vec::new();

        let rows = stmt.query_map([], |row| {
            let signal_json: String = row.get(0)?;
            let pnl_pct: f64 = row.get(1)?;
            Ok((signal_json, pnl_pct))
        })?;

        for row_result in rows {
            let (signal_json, pnl_pct) = row_result?;
            let signals: SignalVector = serde_json::from_str(&signal_json)?;
            let is_profitable = pnl_pct > 0.0;
            data.push((signals, is_profitable));
        }

        Ok(data)
    }

    /// Build feature matrix X and target vector y
    fn build_feature_matrix(
        &self,
        data: &[(SignalVector, bool)],
    ) -> SniperResult<(Array2<f64>, Array1<bool>)> {
        let n = data.len();
        let p = self.feature_names.len();

        let mut x = Array2::zeros((n, p));
        let mut y = Array1::zeros(n);

        for (i, (signals, is_profitable)) in data.iter().enumerate() {
            x[[i, 0]] = signals.dev_reputation / 100.0;
            x[[i, 1]] = signals.holder_concentration;
            x[[i, 2]] = signals.gini_coefficient;
            x[[i, 3]] = signals.wash_trade_ratio;
            x[[i, 4]] = signals.bundled_buy_penalty / 100.0;
            x[[i, 5]] = signals.entry_timing_score / 100.0;
            x[[i, 6]] = signals.liquidity_score / 100.0;
            y[i] = *is_profitable;
        }

        Ok((x, y))
    }
}
