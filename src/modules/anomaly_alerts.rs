//! Anomaly Alerts — flags when live signal distributions drift from training data.
//!
//! Uses KL divergence to measure the distance between:
//! 1. The historical distribution of a signal (when adaptive_weights was trained)
//! 2. The current live distribution of the same signal
//!
//! If KL divergence exceeds a threshold, an alert is raised.
//! This prevents the scoring engine from making decisions on data it hasn't
//! been trained to understand (distribution shift / concept drift).

use chrono::Utc;
use std::collections::HashMap;
use tracing::{warn, info};

use crate::core::types::{AnomalyAlert, AlertSeverity, SignalVector};

/// Stores the historical distribution parameters for each signal
#[derive(Debug, Clone)]
pub struct SignalDistribution {
    pub mean: f64,
    pub std: f64,
    pub count: u64,
}

pub struct AnomalyAlerts {
    /// Historical distribution of each signal (from training data)
    historical: HashMap<String, SignalDistribution>,
    /// KL divergence threshold for Warning
    warning_threshold: f64,
    /// KL divergence threshold for Critical
    critical_threshold: f64,
    /// Running stats for live signals (updated per event)
    live_stats: HashMap<String, LiveStats>,
    /// Alerted signals (to avoid spamming)
    alerted: HashMap<String, u64>,
}

#[derive(Debug, Clone)]
struct LiveStats {
    sum: f64,
    sum_sq: f64,
    count: u64,
}

impl LiveStats {
    fn new() -> Self {
        Self { sum: 0.0, sum_sq: 0.0, count: 0 }
    }

    fn mean(&self) -> f64 {
        if self.count == 0 { return 0.0; }
        self.sum / self.count as f64
    }

    fn std(&self) -> f64 {
        if self.count < 2 { return 0.0; }
        let mean = self.mean();
        let variance = (self.sum_sq / self.count as f64) - (mean * mean);
        variance.max(0.0).sqrt()
    }
}

impl AnomalyAlerts {
    pub fn new(
        historical: HashMap<String, SignalDistribution>,
        warning_threshold: f64,
        critical_threshold: f64,
    ) -> Self {
        Self {
            historical,
            warning_threshold,
            critical_threshold,
            live_stats: HashMap::new(),
            alerted: HashMap::new(),
        }
    }

    /// Initialize from a set of historical signal vectors (training data)
    pub fn from_training_data(signals: &[SignalVector]) -> Self {
        let mut historical = HashMap::new();

        if signals.is_empty() {
            // Default uninformed priors
            historical.insert("dev_reputation".to_string(), SignalDistribution { mean: 50.0, std: 25.0, count: 0 });
            historical.insert("holder_concentration".to_string(), SignalDistribution { mean: 0.5, std: 0.2, count: 0 });
            historical.insert("gini_coefficient".to_string(), SignalDistribution { mean: 0.5, std: 0.2, count: 0 });
            historical.insert("wash_trade_ratio".to_string(), SignalDistribution { mean: 0.5, std: 0.2, count: 0 });
            historical.insert("bundled_buy_penalty".to_string(), SignalDistribution { mean: 50.0, std: 25.0, count: 0 });
            historical.insert("entry_timing_score".to_string(), SignalDistribution { mean: 50.0, std: 25.0, count: 0 });
            historical.insert("liquidity_score".to_string(), SignalDistribution { mean: 50.0, std: 25.0, count: 0 });
        } else {
            // Compute empirical distributions from training data
            for signal in signals {
                Self::update_distribution(&mut historical, "dev_reputation", signal.dev_reputation);
                Self::update_distribution(&mut historical, "holder_concentration", signal.holder_concentration);
                Self::update_distribution(&mut historical, "gini_coefficient", signal.gini_coefficient);
                Self::update_distribution(&mut historical, "wash_trade_ratio", signal.wash_trade_ratio);
                Self::update_distribution(&mut historical, "bundled_buy_penalty", signal.bundled_buy_penalty);
                Self::update_distribution(&mut historical, "entry_timing_score", signal.entry_timing_score);
                Self::update_distribution(&mut historical, "liquidity_score", signal.liquidity_score);
            }
        }

        Self {
            historical,
            warning_threshold: 0.5,
            critical_threshold: 1.5,
            live_stats: HashMap::new(),
            alerted: HashMap::new(),
        }
    }

    /// Update a signal distribution with a new observation
    fn update_distribution(
        dists: &mut HashMap<String, SignalDistribution>,
        name: &str,
        value: f64,
    ) {
        let entry = dists.entry(name.to_string()).or_insert(SignalDistribution {
            mean: value,
            std: 0.0,
            count: 0,
        });

        let count = entry.count;
        entry.count += 1;
        let delta = value - entry.mean;
        entry.mean += delta / entry.count as f64;
        let delta2 = value - entry.mean;
        if count > 0 {
            entry.std = (entry.std.powi(2) * count as f64 + delta * delta2) / (entry.count as f64);
            entry.std = entry.std.sqrt();
        }
    }

    /// Record a live observation and check for anomalies
    pub fn observe(&mut self, signal: &SignalVector) -> Vec<AnomalyAlert> {
        let mut alerts = Vec::new();

        // Update live stats
        self.update_live_stats("dev_reputation", signal.dev_reputation);
        self.update_live_stats("holder_concentration", signal.holder_concentration);
        self.update_live_stats("gini_coefficient", signal.gini_coefficient);
        self.update_live_stats("wash_trade_ratio", signal.wash_trade_ratio);
        self.update_live_stats("bundled_buy_penalty", signal.bundled_buy_penalty);
        self.update_live_stats("entry_timing_score", signal.entry_timing_score);
        self.update_live_stats("liquidity_score", signal.liquidity_score);

        // Check KL divergence for each signal
        let signal_names = vec![
            "dev_reputation", "holder_concentration", "gini_coefficient",
            "wash_trade_ratio", "bundled_buy_penalty", "entry_timing_score", "liquidity_score",
        ];

        for name in signal_names {
            if let (Some(hist), Some(live)) = (self.historical.get(name), self.live_stats.get(name)) {
                if live.count < 30 {
                    continue; // Need enough live data to estimate distribution
                }

                let kl = self.compute_kl_divergence(hist, live);

                // Check if we've already alerted on this signal recently
                let cooldown = self.alerted.get(name).copied().unwrap_or(0);
                let now = Utc::now().timestamp() as u64;
                if now - cooldown < 300 {
                    continue; // 5-minute cooldown between alerts for same signal
                }

                if kl > self.critical_threshold {
                    let alert = AnomalyAlert {
                        signal_name: name.to_string(),
                        expected_mean: hist.mean,
                        expected_std: hist.std,
                        observed_mean: live.mean(),
                        observed_std: live.std(),
                        kl_divergence: kl,
                        severity: AlertSeverity::Critical,
                        timestamp: Utc::now(),
                    };
                    alerts.push(alert);
                    self.alerted.insert(name.to_string(), now);
                    warn!("CRITICAL ANOMALY: {} drifted significantly (KL={:.2})", name, kl);
                } else if kl > self.warning_threshold {
                    let alert = AnomalyAlert {
                        signal_name: name.to_string(),
                        expected_mean: hist.mean,
                        expected_std: hist.std,
                        observed_mean: live.mean(),
                        observed_std: live.std(),
                        kl_divergence: kl,
                        severity: AlertSeverity::Warning,
                        timestamp: Utc::now(),
                    };
                    alerts.push(alert);
                    self.alerted.insert(name.to_string(), now);
                    info!("WARNING: {} distribution shifted (KL={:.2})", name, kl);
                }
            }
        }

        alerts
    }

    /// Update live stats for a single signal
    fn update_live_stats(&mut self, name: &str, value: f64) {
        let stats = self.live_stats.entry(name.to_string()).or_insert_with(LiveStats::new);
        stats.sum += value;
        stats.sum_sq += value * value;
        stats.count += 1;
    }

    /// Compute KL divergence between two normal distributions
    /// KL(P || Q) = log(σ_Q / σ_P) + (σ_P² + (μ_P - μ_Q)²) / (2σ_Q²) - 1/2
    fn compute_kl_divergence(&self, hist: &SignalDistribution, live: &LiveStats) -> f64 {
        let sigma_p = live.std().max(1e-6); // Avoid division by zero
        let sigma_q = hist.std.max(1e-6);
        let mu_p = live.mean();
        let mu_q = hist.mean;

        let kl = (sigma_q / sigma_p).ln()
            + (sigma_p.powi(2) + (mu_p - mu_q).powi(2)) / (2.0 * sigma_q.powi(2))
            - 0.5;

        kl.max(0.0)
    }
}
