//! Module index — all 18 core modules
//!
//! Each module is independently testable before integration.
//! Build order: config → core types → analysis modules → execution → interfaces

pub mod rpc_provider;
pub mod ws_listener;
pub mod entry_filter;
pub mod dev_wallet_reputation;
pub mod holder_concentration_analyzer;
pub mod wash_trade_detector;
pub mod bundled_buy_detector;
pub mod scoring_engine;
pub mod adaptive_weights;
pub mod ab_test_harness;
pub mod risk_engine;
pub mod execution;
pub mod exit_manager;
pub mod db;
pub mod telegram_bot;
pub mod tui;
pub mod backtester;
pub mod anomaly_alerts;
