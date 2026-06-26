//! Holder Concentration Analyzer — top-10 holder concentration / Gini calc.
//!
//! Measures how concentrated a token's supply is among the largest holders.
//! A highly concentrated holder set (dev + 2 wallets owning >50% of supply)
//! signals potential rug risk: coordinated dumping.
//!
//! Uses the Gini coefficient as a formal inequality metric:
//!   G = 1 - (2/(n-1)) * (Σ(i * x_i) / Σx_i)
//! where x_i are sorted holder balances.

use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use tracing::debug;

use crate::core::error::{SniperError, SniperResult};
use crate::config::FilterConfig;

/// Result of a holder concentration analysis
#[derive(Debug, Clone)]
pub struct HolderAnalysis {
    pub total_supply: u64,
    pub top_10_holders: Vec<(Pubkey, u64)>,
    pub top_10_concentration: f64,    // fraction of total supply held by top 10
    pub gini_coefficient: f64,        // 0 = perfect equality, 1 = perfect inequality
    pub unique_holder_count: u64,
    pub passes_filter: bool,
}

pub struct HolderConcentrationAnalyzer {
    config: FilterConfig,
}

impl HolderConcentrationAnalyzer {
    pub fn new(config: FilterConfig) -> Self {
        Self { config }
    }

    /// Analyze holder concentration for a token
    /// 
    /// NOTE: In production, this queries the Solana RPC for all token accounts
    /// owned by the SPL token program with the given mint. For now, this takes
    /// the holder map as input (would be populated by the ws_listener + RPC polling).
    pub fn analyze(&self, holders: &HashMap<Pubkey, u64>) -> SniperResult<HolderAnalysis> {
        if holders.is_empty() {
            return Err(SniperError::Unknown { 
                msg: "Cannot analyze empty holder map".into() 
            });
        }

        let total_supply: u64 = holders.values().sum();
        let unique_holder_count = holders.len() as u64;

        // Sort holders by balance descending
        let mut sorted: Vec<(Pubkey, u64)> = holders.iter()
            .map(|(&k, &v)| (k, v))
            .collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));

        // Top 10 concentration
        let top_10_count = 10.min(sorted.len());
        let top_10_supply: u64 = sorted.iter().take(top_10_count).map(|(_, balance)| balance).sum();
        let top_10_concentration = top_10_supply as f64 / total_supply as f64;

        // Gini coefficient calculation
        let gini = Self::compute_gini(&sorted, total_supply);

        let top_10: Vec<(Pubkey, u64)> = sorted.into_iter().take(top_10_count).collect();

        let passes_filter = top_10_concentration <= self.config.max_dev_holder_concentration
            && gini <= self.config.max_gini_coefficient;

        debug!(
            "Holder analysis: top10={:.2}%, Gini={:.3}, unique={}, passes={}",
            top_10_concentration * 100.0,
            gini,
            unique_holder_count,
            passes_filter
        );

        Ok(HolderAnalysis {
            total_supply,
            top_10_holders: top_10,
            top_10_concentration,
            gini_coefficient: gini,
            unique_holder_count,
            passes_filter,
        })
    }

    /// Compute the Gini coefficient from a sorted list of (holder, balance) pairs.
    ///
    /// The Gini coefficient is computed as:
    ///   G = (Σ_i Σ_j |x_i - x_j|) / (2 * n * Σx)
    /// 
    /// For large n, we use the sorted formula:
    ///   G = 1 - (2/(n-1)) * (Σ(i * x_i) / Σx_i)
    /// where i is 1-indexed rank in ascending order.
    fn compute_gini(sorted: &[(Pubkey, u64)], total_supply: u64) -> f64 {
        let n = sorted.len();
        if n <= 1 {
            return 1.0; // Single holder = maximum inequality
        }

        let mut sum_ix = 0.0_f64;
        let mut total = 0.0_f64;

        // Sort ascending for the Gini formula (sorted is currently descending)
        let mut asc = sorted.to_vec();
        asc.sort_by(|a, b| a.1.cmp(&b.1));

        for (i, (_, balance)) in asc.iter().enumerate() {
            sum_ix += (i as f64 + 1.0) * (*balance as f64);
            total += *balance as f64;
        }

        if total == 0.0 {
            return 0.0;
        }

        let gini = 1.0 - (2.0 / (n as f64 - 1.0)) * (sum_ix / total);
        gini.max(0.0).min(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FilterConfig;

    fn default_config() -> FilterConfig {
        FilterConfig {
            min_entry_score: 65,
            max_dev_holder_concentration: 0.15,
            max_gini_coefficient: 0.60,
            min_wash_trade_unique_ratio: 0.40,
            entry_delay_seconds: 3,
            max_bundled_buy_cluster_size: 3,
            min_trades_for_analysis: 10,
        }
    }

    #[test]
    fn test_equal_holders_gini() {
        let analyzer = HolderConcentrationAnalyzer::new(default_config());
        let mut holders = HashMap::new();
        for i in 0..10 {
            holders.insert(Pubkey::new_unique(), 100);
        }
        let analysis = analyzer.analyze(&holders).unwrap();
        assert!(analysis.gini_coefficient < 0.1, "Equal holders should have near-zero Gini");
    }

    #[test]
    fn test_concentrated_holders_gini() {
        let analyzer = HolderConcentrationAnalyzer::new(default_config());
        let mut holders = HashMap::new();
        holders.insert(Pubkey::new_unique(), 900);
        for i in 0..9 {
            holders.insert(Pubkey::new_unique(), 10);
        }
        let analysis = analyzer.analyze(&holders).unwrap();
        assert!(analysis.gini_coefficient > 0.5, "Concentrated holders should have high Gini");
    }
}
