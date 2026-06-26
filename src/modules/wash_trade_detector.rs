//! Wash Trade Detector — unique-wallet vs trade-count ratio.
//!
//! Wash trading inflates trade volume without organic holder interest.
//! This module detects wash trading by computing:
//!   wash_ratio = unique_trading_wallets / total_trade_count
//!
//! A low ratio (< 0.40) signals that the same wallets are trading repeatedly,
//! suggesting wash activity rather than genuine market interest.

use std::collections::{HashMap, HashSet};
use solana_sdk::pubkey::Pubkey;
use tracing::debug;

use crate::config::FilterConfig;

/// A single trade record (from on-chain logs or RPC polling)
#[derive(Debug, Clone)]
pub struct TradeRecord {
    pub wallet: Pubkey,
    pub slot: u64,
    pub amount: u64,
    pub is_buy: bool,
    pub timestamp: i64,
}

/// Result of wash-trade analysis
#[derive(Debug, Clone)]
pub struct WashTradeAnalysis {
    pub unique_wallets: usize,
    pub total_trades: usize,
    pub unique_ratio: f64,
    pub passes_filter: bool,
}

pub struct WashTradeDetector {
    config: FilterConfig,
    trades: Vec<TradeRecord>,
}

impl WashTradeDetector {
    pub fn new(config: FilterConfig) -> Self {
        Self {
            config,
            trades: Vec::new(),
        }
    }

    /// Add a trade record for analysis
    pub fn add_trade(&mut self, trade: TradeRecord) {
        self.trades.push(trade);
    }

    /// Clear trade history (call after analysis or periodic reset)
    pub fn clear(&mut self) {
        self.trades.clear();
    }

    /// Run wash-trade analysis on accumulated trades
    pub fn analyze(&self) -> WashTradeAnalysis {
        let total_trades = self.trades.len();

        if total_trades == 0 {
            return WashTradeAnalysis {
                unique_wallets: 0,
                total_trades: 0,
                unique_ratio: 1.0,
                passes_filter: true,
            };
        }

        let unique_wallets: HashSet<&Pubkey> = self.trades.iter()
            .map(|t| &t.wallet)
            .collect();

        let unique_count = unique_wallets.len();
        let unique_ratio = unique_count as f64 / total_trades as f64;
        let passes_filter = unique_ratio >= self.config.min_wash_trade_unique_ratio;

        debug!(
            "Wash trade analysis: unique={}, total={}, ratio={:.3}, passes={}",
            unique_count, total_trades, unique_ratio, passes_filter
        );

        WashTradeAnalysis {
            unique_wallets: unique_count,
            total_trades,
            unique_ratio,
            passes_filter,
        }
    }

    /// Compute per-wallet trade frequency (for deeper wash detection)
    pub fn wallet_frequency(&self) -> HashMap<Pubkey, usize> {
        let mut freq: HashMap<Pubkey, usize> = HashMap::new();

        for trade in &self.trades {
            *freq.entry(trade.wallet).or_insert(0) += 1;
        }

        freq
    }
}
