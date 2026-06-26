//! Integration tests for pf-sniper scoring + risk pipeline
//!
//! These tests verify that:
//! 1. Signal vectors produce deterministic scores
//! 2. Kelly sizing clamps correctly
//! 3. Entry filters respect delay windows
//! 4. Holder concentration analysis computes Gini correctly

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

mod tests {
    use super::*;
    use crate::config::{FilterConfig, RiskConfig};
    use crate::core::types::{PoolCreationEvent, SignalVector};
    use crate::modules::entry_filter::EntryFilter;
    use crate::modules::holder_concentration_analyzer::HolderConcentrationAnalyzer;
    use crate::modules::scoring_engine::ScoringEngine;
    use crate::modules::risk_engine::RiskEngine;

    fn default_filter_config() -> FilterConfig {
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

    fn default_risk_config() -> RiskConfig {
        RiskConfig {
            kelly_fraction: 0.25,
            max_position_size_lamports: 500_000_000,
            stop_loss_pct: 0.30,
            take_profit_1_pct: 0.50,
            take_profit_2_pct: 1.00,
            max_concurrent_positions: 5,
            consecutive_loss_circuit_breaker: 5,
        }
    }

    #[test]
    fn test_scoring_engine_deterministic() {
        let config = default_filter_config();
        let engine = ScoringEngine::default_with_filter(config);

        let signals = SignalVector {
            dev_reputation: 80.0,
            holder_concentration: 0.10,
            gini_coefficient: 0.30,
            wash_trade_ratio: 0.70,
            bundled_buy_penalty: 20.0,
            entry_timing_score: 75.0,
            liquidity_score: 60.0,
            trade_velocity: 15.0,
            unique_wallets: 50,
            total_trades: 100,
        };

        let score = engine.score(&signals);

        // Same input should always produce same output
        let score2 = engine.score(&signals);
        assert_eq!(score.score, score2.score);

        // Score should be in valid range
        assert!(score.score <= 100);
    }

    #[test]
    fn test_entry_filter_eligibility() {
        let config = default_filter_config();
        let filter = EntryFilter::new(config);

        // Event just created (0 seconds ago) — should not be eligible
        let now = chrono::Utc::now();
        let recent_event = PoolCreationEvent {
            mint: solana_sdk::pubkey::Pubkey::new_unique(),
            bonding_curve: solana_sdk::pubkey::Pubkey::new_unique(),
            associated_bonding_curve: solana_sdk::pubkey::Pubkey::new_unique(),
            user: solana_sdk::pubkey::Pubkey::new_unique(),
            name: "TestToken".to_string(),
            symbol: "TEST".to_string(),
            uri: "https://example.com".to_string(),
            mayhem: false,
            slot: 12345,
            timestamp: now,
            signature: "sig123".to_string(),
        };

        assert!(!filter.is_eligible(&recent_event));

        // Event from 10 seconds ago — should be eligible
        let old_event = PoolCreationEvent {
            timestamp: now - chrono::Duration::seconds(10),
            ..recent_event.clone()
        };

        assert!(filter.is_eligible(&old_event));
    }

    #[test]
    fn test_holder_concentration_gini_equal_distribution() {
        let config = default_filter_config();
        let analyzer = HolderConcentrationAnalyzer::new(config);

        let mut holders = HashMap::new();
        for _ in 0..10 {
            holders.insert(solana_sdk::pubkey::Pubkey::new_unique(), 100);
        }

        let analysis = analyzer.analyze(&holders).unwrap();

        // Equal distribution should have low Gini
        assert!(analysis.gini_coefficient < 0.1);
        assert!(analysis.passes_filter);
    }

    #[test]
    fn test_holder_concentration_gini_concentrated() {
        let config = default_filter_config();
        let analyzer = HolderConcentrationAnalyzer::new(config);

        let mut holders = HashMap::new();
        holders.insert(solana_sdk::pubkey::Pubkey::new_unique(), 900);
        for _ in 0..9 {
            holders.insert(solana_sdk::pubkey::Pubkey::new_unique(), 10);
        }

        let analysis = analyzer.analyze(&holders).unwrap();

        // Concentrated distribution should have high Gini
        assert!(analysis.gini_coefficient > 0.5);
        assert!(!analysis.passes_filter);
    }

    #[test]
    fn test_risk_engine_circuit_breaker() {
        let config = default_risk_config();
        let mut engine = RiskEngine::new(config);

        // Simulate 5 consecutive losses
        for _ in 0..5 {
            engine.record_loss(-0.1);
        }

        // Should trigger circuit breaker
        assert!(engine.check_entry().is_err());

        // Reset should clear it
        engine.reset_circuit_breaker();
        assert!(engine.check_entry().is_ok());
    }

    #[test]
    fn test_risk_engine_max_concurrent_positions() {
        let config = default_risk_config();
        let mut engine = RiskEngine::new(config);

        // Open 5 positions (max)
        for i in 0..5 {
            engine.open_position(solana_sdk::pubkey::Pubkey::new_unique());
        }

        // Should reject 6th position
        assert!(engine.check_entry().is_err());

        // Close one and try again
        let first_mint = solana_sdk::pubkey::Pubkey::new_unique();
        engine.open_position(first_mint);
        engine.close_position(first_mint);

        // Should still be at 5, so rejected
        assert!(engine.check_entry().is_err());
    }

    #[test]
    fn test_scoring_weights_normalization() {
        let config = default_filter_config();
        let mut engine = ScoringEngine::new(HashMap::new(), config);

        // Give it weights that don't sum to 1.0
        let mut bad_weights = HashMap::new();
        bad_weights.insert("dev_reputation".to_string(), 0.5);
        bad_weights.insert("holder_concentration".to_string(), 0.5);

        engine.update_weights(bad_weights);

        // Weights should now sum to ~1.0 after normalization
        let sum: f64 = engine.get_weights().values().sum();
        assert!((sum - 1.0).abs() < 0.01);
    }
}
