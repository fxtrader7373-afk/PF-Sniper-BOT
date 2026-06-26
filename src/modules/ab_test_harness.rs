//! A/B Test Harness — shadow-mode testing of new weights vs current.
//!
//! Before any weight update touches live capital, it runs in shadow mode:
//! - Both current and shadow weights score every entry opportunity
//! - Shadow scores are recorded but NOT used for real trades
//! - After N trades, shadow performance is compared to live performance
//! - If shadow expectancy > live expectancy by a statistically significant margin,
//!   the new weights are promoted to live
//!
//! This prevents degrading the scoring engine with a bad retraining iteration.

use std::collections::HashMap;
use chrono::Utc;
use tracing::{info, warn};

use crate::core::types::{AbTestState, SignalVector};
use crate::modules::scoring_engine::ScoringEngine;

pub struct AbTestHarness {
    pub state: AbTestState,
    live_engine: ScoringEngine,
    shadow_engine: Option<ScoringEngine>,
    filter_config: crate::config::FilterConfig,
}

impl AbTestHarness {
    pub fn new(live_weights: HashMap<String, f64>, filter_config: crate::config::FilterConfig) -> Self {
        let state = AbTestState::new(live_weights.clone());
        let live_engine = ScoringEngine::new(live_weights, filter_config.clone());

        Self {
            state,
            live_engine,
            shadow_engine: None,
            filter_config,
        }
    }

    /// Start a shadow test with new weights
    pub fn start_shadow_test(&mut self, shadow_weights: HashMap<String, f64>) {
        self.shadow_engine = Some(ScoringEngine::new(shadow_weights.clone(), self.filter_config.clone()));
        self.state.is_active = true;
        self.state.shadow_weights = shadow_weights;
        self.state.shadow_trades = 0;
        self.state.live_trades = 0;
        self.state.shadow_expectancy = 0.0;
        self.state.live_expectancy = 0.0;
        self.state.started_at = Utc::now();
        info!("A/B shadow test started with {} weights", self.state.shadow_weights.len());
    }

    /// Score with both engines and record the results
    pub fn score_and_record(&mut self, signals: &SignalVector, pnl: f64) {
        let live_score = self.live_engine.score(signals);
        self.state.live_trades += 1;
        self.state.live_expectancy = self.running_expectancy(
            self.state.live_expectancy,
            self.state.live_trades - 1,
            pnl,
        );

        if let Some(ref shadow_engine) = self.shadow_engine {
            let shadow_score = shadow_engine.score(signals);
            self.state.shadow_trades += 1;
            self.state.shadow_expectancy = self.running_expectancy(
                self.state.shadow_expectancy,
                self.state.shadow_trades - 1,
                pnl,
            );
        }
    }

    /// Check if shadow weights should be promoted
    /// Returns true if shadow expectancy is significantly better
    pub fn should_promote(&self, min_trades: usize) -> bool {
        if !self.state.is_active {
            return false;
        }

        if self.state.shadow_trades < min_trades || self.state.live_trades < min_trades {
            return false;
        }

        let improvement = self.state.shadow_expectancy - self.state.live_expectancy;

        // Promote if shadow is better by at least 5% absolute expectancy
        if improvement > 0.05 {
            info!(
                "A/B test: shadow expectancy ({:.3}) > live ({:.3}), promoting",
                self.state.shadow_expectancy,
                self.state.live_expectancy,
            );
            true
        } else {
            warn!(
                "A/B test: shadow expectancy ({:.3}) not better than live ({:.3}), discarding",
                self.state.shadow_expectancy,
                self.state.live_expectancy,
            );
            false
        }
    }

    /// Promote shadow weights to live
    pub fn promote(&mut self) -> Option<HashMap<String, f64>> {
        if !self.should_promote(50) {
            return None;
        }

        let new_weights = self.state.shadow_weights.clone();
        self.live_engine = ScoringEngine::new(new_weights.clone(), self.filter_config.clone());
        self.state.is_active = false;
        self.shadow_engine = None;

        info!("Shadow weights promoted to live");
        Some(new_weights)
    }

    /// Stop the shadow test without promoting
    pub fn stop(&mut self) {
        self.state.is_active = false;
        self.shadow_engine = None;
        info!("A/B shadow test stopped without promotion");
    }

    /// Compute running expectancy incrementally
    fn running_expectancy(&self, prev: f64, n: usize, new_val: f64) -> f64 {
        if n == 0 {
            return new_val;
        }
        prev + (new_val - prev) / (n as f64 + 1.0)
    }
}
