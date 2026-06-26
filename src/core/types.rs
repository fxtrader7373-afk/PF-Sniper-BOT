//! Core domain types for pf-sniper.
//!
//! These are the primitives that flow through every module:
//! new-pool events, trade signals, position state, scoring vectors, P&L metrics.
//! Precision here matters — a mis-modeled field silently corrupts the scoring engine,
//! the risk engine, and the adaptive weights trainer.

use serde::{Deserialize, Serialize};
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use chrono::{DateTime, Utc};

// ── Program IDs (constants, not config — these are protocol-level truths) ──

/// Pump.fun bonding-curve program (v1 & v2)
/// Source: [2](https://docs.chainstack.com/docs/solana-listening-to-pumpfun-token-mint-using-only-logssubscribe)
pub const PUMP_FUN_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

/// PumpSwap AMM program (migration target)
/// Source: [6](https://docs.bitquery.io/docs/blockchain/Solana/Pumpfun/pump-swap-api/)
pub const PUMPSWAP_PROGRAM_ID: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";

/// Jito tip account
pub const JITO_TIP_ACCOUNT: &str = "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5";

// ── New-Pool / Token-Creation Events ─────────────────────────────────────────

/// Raw event emitted by the pump.fun program on new token creation.
/// Decoded from logsSubscribe Program data: lines.
///
/// Source for decode method: [2](https://docs.chainstack.com/docs/solana-listening-to-pumpfun-token-mint-using-only-logssubscribe)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolCreationEvent {
    /// Token mint address
    pub mint: Pubkey,
    /// Bonding curve PDA
    pub bonding_curve: Pubkey,
    /// Associated bonding curve account
    pub associated_bonding_curve: Pubkey,
    /// Creator / dev wallet
    pub user: Pubkey,
    /// Token name (from metadata)
    pub name: String,
    /// Token symbol
    pub symbol: String,
    /// Metadata URI (usually arweave)
    pub uri: String,
    /// Whether this is a mayhem-mode token
    pub mayhem: bool,
    /// Slot at which the create instruction landed
    pub slot: u64,
    /// Timestamp of block containing the create instruction
    pub timestamp: DateTime<Utc>,
    /// Transaction signature
    pub signature: String,
}

// ── Trade Signal Vector ──────────────────────────────────────────────────────

/// The composite signal vector fed into the scoring_engine.
/// Every field is computed by an independent analysis module and
/// represents a dimension in the multi-signal decision space.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalVector {
    /// Dev wallet reputation score (0-100, higher = cleaner history)
    pub dev_reputation: f64,
    /// Top-10 holder concentration (0.0-1.0, lower = more distributed)
    pub holder_concentration: f64,
    /// Gini coefficient of holder distribution (0.0-1.0)
    pub gini_coefficient: f64,
    /// Wash-trade unique-wallet ratio (0.0-1.0, higher = more organic)
    pub wash_trade_ratio: f64,
    /// Bundled-buy cluster size penalty (0-100, higher = more suspicious)
    pub bundled_buy_penalty: f64,
    /// Second-wave timing score (0-100, higher = better entry timing)
    pub entry_timing_score: f64,
    /// Liquidity depth score (0-100)
    pub liquidity_score: f64,
    /// Trade velocity (trades per minute in first N minutes)
    pub trade_velocity: f64,
    /// Unique wallet count in observation window
    pub unique_wallets: u64,
    /// Total trade count in observation window
    pub total_trades: u64,
}

impl SignalVector {
    /// Returns a zero vector (default-uninformed prior)
    pub fn zero() -> Self {
        Self {
            dev_reputation: 50.0,
            holder_concentration: 0.5,
            gini_coefficient: 0.5,
            wash_trade_ratio: 0.5,
            bundled_buy_penalty: 50.0,
            entry_timing_score: 50.0,
            liquidity_score: 50.0,
            trade_velocity: 0.0,
            unique_wallets: 0,
            total_trades: 0,
        }
    }
}

// ── Composite Score ──────────────────────────────────────────────────────────

/// Final entry score from the scoring_engine (0-100).
/// A weighted combination of SignalVector fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryScore {
    pub score: u8,
    pub weights: HashMap<String, f64>,
    pub signal_vector: SignalVector,
    pub timestamp: DateTime<Utc>,
    pub is_paper: bool,
}

// ── Position State ───────────────────────────────────────────────────────────

/// A tracked position (open, partially closed, or fully closed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub token_mint: Pubkey,
    pub entry_slot: u64,
    pub entry_price: f64,
    pub entry_amount_sol: f64,
    pub entry_tx_signature: Option<String>,
    pub current_price: f64,
    pub current_sol_value: f64,
    pub unrealized_pnl: f64,
    pub realized_pnl: f64,
    pub token_amount: f64,
    pub status: PositionStatus,
    pub stop_loss_price: f64,
    pub take_profit_levels: Vec<f64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PositionStatus {
    Open,
    PartiallyClosed { remaining_pct: f64 },
    Closed,
    StoppedOut,
    TakeProfit { level: u8 },
    OverrideForcedSell, // /forcesell from Telegram — excluded from training
}

// ── Trade Journal Entry ──────────────────────────────────────────────────────

/// One row in the SQLite trade journal.
/// This is the training data for adaptive_weights.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeJournalEntry {
    pub id: Option<i64>,
    pub token_mint: String,
    pub dev_wallet: String,
    pub entry_slot: u64,
    pub exit_slot: Option<u64>,
    pub entry_time: DateTime<Utc>,
    pub exit_time: Option<DateTime<Utc>>,
    pub entry_price: f64,
    pub exit_price: Option<f64>,
    pub entry_amount_sol: f64,
    pub exit_amount_sol: Option<f64>,
    pub swap_fee_sol: f64,
    pub price_impact_sol: f64,
    pub mev_tax_sol: f64,
    pub jito_tip_sol: f64,
    pub net_pnl_sol: f64,
    pub net_pnl_pct: f64,
    pub entry_score: u8,
    pub signal_vector_json: String,
    pub exit_reason: ExitReason,
    pub is_paper: bool,
    pub was_override: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ExitReason {
    StopLoss,
    TakeProfit { level: u8 },
    ManualOverride,
    BondingCurveGraduated, // migrated to Raydium/PumpSwap
    PoolRugDetected,
    Timeout,
}

// ── Wallet Keystore Metadata ─────────────────────────────────────────────────

/// A labeled reference to an encrypted wallet keystore file.
/// The actual key material NEVER leaves the encrypted file on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletKeystoreMeta {
    pub label: String,
    pub encrypted_file_path: String,
    pub pubkey: Pubkey,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

// ── RPC Endpoint Health ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcHealth {
    pub label: String,
    pub url: String,
    pub latency_ms: u64,
    pub last_429_at: Option<DateTime<Utc>>,
    pub consecutive_429s: u32,
    pub is_healthy: bool,
    pub last_check: DateTime<Utc>,
}

// ── Backtest Result ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestResult {
    pub total_trades: usize,
    pub winning_trades: usize,
    pub losing_trades: usize,
    pub win_rate: f64,
    pub total_pnl_sol: f64,
    pub avg_pnl_per_trade_sol: f64,
    pub max_drawdown_pct: f64,
    pub sharpe_ratio: f64,
    pub sortino_ratio: f64,
    pub expectancy: f64,
    pub profit_factor: f64,
    pub avg_holding_time_seconds: f64,
    pub max_consecutive_losses: usize,
}

// ── Anomaly Alert ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyAlert {
    pub signal_name: String,
    pub expected_mean: f64,
    pub expected_std: f64,
    pub observed_mean: f64,
    pub observed_std: f64,
    pub kl_divergence: f64,
    pub severity: AlertSeverity,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
}

// ── A/B Test State ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbTestState {
    pub is_active: bool,
    pub shadow_weights: HashMap<String, f64>,
    pub live_weights: HashMap<String, f64>,
    pub shadow_trades: usize,
    pub live_trades: usize,
    pub shadow_expectancy: f64,
    pub live_expectancy: f64,
    pub started_at: DateTime<Utc>,
}

impl AbTestState {
    pub fn new(live_weights: HashMap<String, f64>) -> Self {
        Self {
            is_active: false,
            shadow_weights: live_weights.clone(),
            live_weights,
            shadow_trades: 0,
            live_trades: 0,
            shadow_expectancy: 0.0,
            live_expectancy: 0.0,
            started_at: Utc::now(),
        }
    }
}
