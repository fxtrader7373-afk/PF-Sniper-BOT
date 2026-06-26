//! Bundled Buy Detector — same-slot wallet clustering.
//!
//! Detects when multiple buys from different wallets land in the same slot,
//! which strongly suggests coordinated bot activity (bundled buys).
//! This is a primary signal of manipulation: the dev or a syndicate
//! is distributing supply across controlled wallets to fake organic demand.

use std::collections::HashMap;
use solana_sdk::pubkey::Pubkey;
use tracing::debug;

use crate::config::FilterConfig;

/// A buy event captured from on-chain logs
#[derive(Debug, Clone)]
pub struct BuyEvent {
    pub wallet: Pubkey,
    pub slot: u64,
    pub amount_sol: u64,
    pub timestamp: i64,
}

/// Result of bundled-buy cluster analysis
#[derive(Debug, Clone)]
pub struct BundledBuyAnalysis {
    pub slot: u64,
    pub unique_wallets: usize,
    pub total_buys: usize,
    pub cluster_size: usize,
    pub total_sol: u64,
    pub passes_filter: bool,
}

pub struct BundledBuyDetector {
    config: FilterConfig,
    events: Vec<BuyEvent>,
}

impl BundledBuyDetector {
    pub fn new(config: FilterConfig) -> Self {
        Self {
            config,
            events: Vec::new(),
        }
    }

    /// Add a buy event
    pub fn add_event(&mut self, event: BuyEvent) {
        self.events.push(event);
    }

    /// Analyze bundled-buy clusters in the current event window
    pub fn analyze(&self) -> Vec<BundledBuyAnalysis> {
        // Group events by slot
        let mut slot_groups: HashMap<u64, Vec<&BuyEvent>> = HashMap::new();
        for event in &self.events {
            slot_groups.entry(event.slot).or_default().push(event);
        }

        let mut results = Vec::new();

        for (slot, events) in slot_groups {
            let total_buys = events.len();
            let unique_wallets: std::collections::HashSet<&Pubkey> = events.iter()
                .map(|e| &e.wallet)
                .collect();

            let cluster_size = unique_wallets.len();
            let total_sol: u64 = events.iter().map(|e| e.amount_sol).sum();

            // A cluster is suspicious if multiple wallets bought in the same slot
            let passes_filter = cluster_size <= self.config.max_bundled_buy_cluster_size;

            results.push(BundledBuyAnalysis {
                slot,
                unique_wallets: unique_wallets.len(),
                total_buys,
                cluster_size,
                total_sol,
                passes_filter,
            });
        }

        // Sort by cluster_size descending (largest clusters first)
        results.sort_by(|a, b| b.cluster_size.cmp(&a.cluster_size));

        debug!("Bundled buy analysis: found {} clusters", results.len());

        results
    }

    /// Clear events (call after analysis or periodic reset)
    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// Returns the maximum cluster size found
    pub fn max_cluster_size(&self) -> usize {
        let analyses = self.analyze();
        analyses.iter().map(|a| a.cluster_size).max().unwrap_or(0)
    }

    /// Returns whether any cluster exceeds the configured threshold
    pub fn has_suspicious_clusters(&self) -> bool {
        let analyses = self.analyze();
        analyses.iter().any(|a| !a.passes_filter)
    }
}
