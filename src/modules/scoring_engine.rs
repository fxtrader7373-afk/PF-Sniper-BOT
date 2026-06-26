//! Scoring Engine — combines all signals into a 0-100 entry score.
//!
//! Uses a weighted linear combination of normalized signal inputs.
//! Weights can be heuristic (initial) or learned (after adaptive_weights retrains).
//!
//! Signal inputs:
//! 1. dev_reputation: 0-100 (higher = cleaner history)
//! 2. holder_concentration: 0.0-1.0 (lower = more distributed) → inverted to 100-scale
//! 3. gini_coefficient: 0.0-1.0 (lower = fairer distribution) → inverted to 100-scale
//! 4. wash_trade_ratio: 0.0-1.0 (higher = more organic)
//! 5. bundled_buy_penalty: 0-100 (higher = more suspicious) → inverted
//! 6. entry_timing_score: 0-100
//! 7. liquidity_score: 0-100

use std::collections::HashMap;
use tracing::debug;

use crate::core::types::{EntryScore, SignalVector};
use crate::config::FilterConfig;

/// Default heuristic weights (before adaptive retraining kicks in)
fn default_weights() -> HashMap<String, f64> {
    let mut w = HashMap::new();
    w.insert("dev_reputation".to_string(), 0.15);
    w.insert("holder_concentration".to_string(), 0.15);
    w.insert("gini_coefficient".to_string(), 0.10);
    w.insert("wash_trade_ratio".to_string(), 0.15);
    w.insert("bundled_buy_penalty".to_string(), 0.15);
    w.insert("entry_timing".to_string(), 0.15);
    w.insert("liquidity".to_string(), 0.15);
    w
}

pub struct ScoringEngine {
    weights: HashMap<String, f64>,
    filter_config: FilterConfig,
}

impl ScoringEngine {
    pub fn new(weights: HashMap<String, f64>, filter_config: FilterConfig) -> Self {
        Self { weights, filter_config }
    }

    /// Create with default heuristic weights
    pub fn default_with_filter(filter_config: FilterConfig) -> Self {
        Self {
            weights: default_weights(),
            filter_config,
        }
    }

    /// Update weights (called by adaptive_weights or ab_test_harness promotion)
    pub fn update_weights(&mut self, new_weights: HashMap<String, f64>) {
        // Validate weights sum to ~1.0
        let sum: f64 = new_weights.values().sum();
        if (sum - 1.0).abs() > 0.01 {
            debug!("Weights sum to {}, normalizing...", sum);
            let mut normalized = HashMap::new();
            for (k, v) in new_weights {
                normalized.insert(k, v / sum);
            }
            self.weights = normalized;
        } else {
            self.weights = new_weights;
        }
    }

    /// Score a signal vector, returning an EntryScore
    pub fn score(&self, signals: &SignalVector) -> EntryScore {
        let mut total = 0.0_f64;

        // dev_reputation: already 0-100
        total += self.weights.get("dev_reputation").unwrap_or(&0.15)
            * signals.dev_reputation;

        // holder_concentration: invert (lower is better) → 100 - (conc * 100)
        let holder_score = (1.0 - signals.holder_concentration) * 100.0;
        total += self.weights.get("holder_concentration").unwrap_or(&0.15)
            * holder_score;

        // gini_coefficient: invert (lower is better) → 100 - (gini * 100)
        let gini_score = (1.0 - signals.gini_coefficient) * 100.0;
        total += self.weights.get("gini_coefficient").unwrap_or(&0.10)
            * gini_score;

        // wash_trade_ratio: already 0-1 → multiply by 100
        total += self.weights.get("wash_trade_ratio").unwrap_or(&0.15)
            * signals.wash_trade_ratio * 100.0;

        // bundled_buy_penalty: invert (lower is better) → 100 - penalty
        let bundle_score = 100.0 - signals.bundled_buy_penalty;
        total += self.weights.get("bundled_buy_penalty").unwrap_or(&0.15)
            * bundle_score;

        // entry_timing_score: already 0-100
        total += self.weights.get("entry_timing").unwrap_or(&0.15)
            * signals.entry_timing_score;

        // liquidity_score: already 0-100
        total += self.weights.get("liquidity").unwrap_or(&0.15)
            * signals.liquidity_score;

        let score = total.round().clamp(0.0, 100.0) as u8;
        let passes_threshold = score >= self.filter_config.min_entry_score;

        debug!(
            "Scoring: raw={:.2}, rounded={}, passes_threshold={}",
            total, score, passes_threshold
        );

        EntryScore {
            score,
            weights: self.weights.clone(),
            signal_vector: signals.clone(),
            timestamp: chrono::Utc::now(),
            is_paper: true, // Default to paper; execution module overrides
        }
    }

    /// Get current weights (for display/telemetry)
    pub fn get_weights(&self) -> &HashMap<String, f64> {
        &self.weights
    }
}
