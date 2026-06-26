//! Configuration management for pf-sniper
//!
//! All RPC endpoints, wallet keystores, risk parameters, and filter thresholds
//! are loaded from config files or updated dynamically via Telegram commands.
//! Nothing is ever hardcoded.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Master configuration loaded from `config.toml`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub trading: TradingConfig,
    pub rpc: RpcConfig,
    pub risk: RiskConfig,
    pub filters: FilterConfig,
    pub telegram: TelegramConfig,
    pub database: DatabaseConfig,
    pub backtest: BacktestConfig,
}

/// Trading engine parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingConfig {
    /// Paper mode: true = simulate only, false = real capital at risk
    #[serde(default = "default_true")]
    pub paper_mode: bool,

    /// Default buy amount in lamports (0.1 SOL = 100_000_000)
    #[serde(default = "default_buy_amount")]
    pub default_buy_amount_lamports: u64,

    /// Active wallet label (references encrypted keystore)
    pub active_wallet_label: Option<String>,

    /// Jito tip in lamports
    #[serde(default = "default_jito_tip")]
    pub jito_tip_lamports: u64,

    /// Max compute units for transaction
    #[serde(default = "default_compute_units")]
    pub compute_unit_limit: u32,

    /// Priority fee in micro-lamports
    #[serde(default = "default_priority_fee")]
    pub priority_fee_micro_lamports: u64,
}

/// RPC endpoint configuration with failover
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcConfig {
    /// Named RPC endpoints (HTTP)
    pub http_endpoints: HashMap<String, String>,

    /// WebSocket endpoint for subscriptions
    pub ws_endpoint: String,

    /// Geyser gRPC endpoint (optional, for lower latency)
    pub geyser_endpoint: Option<String>,

    /// Active HTTP endpoint label
    pub active_http_label: Option<String>,
}

/// Risk management parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    /// Kelly fraction for position sizing (0.0 to 1.0)
    #[serde(default = "default_kelly_fraction")]
    pub kelly_fraction: f64,

    /// Maximum position size in lamports
    #[serde(default = "default_max_position")]
    pub max_position_size_lamports: u64,

    /// Stop-loss percentage (as fraction, e.g. 0.30 = 30%)
    #[serde(default = "default_stop_loss")]
    pub stop_loss_pct: f64,

    /// Take-profit percentage for first ladder rung
    #[serde(default = "default_take_profit_1")]
    pub take_profit_1_pct: f64,

    /// Take-profit percentage for second ladder rung
    #[serde(default = "default_take_profit_2")]
    pub take_profit_2_pct: f64,

    /// Maximum concurrent open positions
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_positions: usize,

    /// Consecutive loss threshold to trigger circuit breaker
    #[serde(default = "default_consecutive_losses")]
    pub consecutive_loss_circuit_breaker: usize,
}

/// Scoring engine filter thresholds
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterConfig {
    /// Minimum entry score (0-100) required to take position
    #[serde(default = "default_min_score")]
    pub min_entry_score: u8,

    /// Maximum dev wallet holder concentration (as fraction)
    #[serde(default = "default_max_dev_concentration")]
    pub max_dev_holder_concentration: f64,

    /// Maximum Gini coefficient for holder distribution
    #[serde(default = "default_max_gini")]
    pub max_gini_coefficient: f64,

    /// Wash trade ratio threshold (unique wallets / trade count)
    #[serde(default = "default_wash_trade_ratio")]
    pub min_wash_trade_unique_ratio: f64,

    /// Minimum seconds after pool creation to enter ("second wave" logic)
    #[serde(default = "default_entry_delay")]
    pub entry_delay_seconds: u64,

    /// Maximum bundled-buy cluster size to tolerate
    #[serde(default = "default_max_bundle_cluster")]
    pub max_bundled_buy_cluster_size: usize,

    /// Minimum total trade count before analyzing holder concentration
    #[serde(default = "default_min_trades_for_analysis")]
    pub min_trades_for_analysis: u64,
}

/// Telegram bot configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    /// Bot token (loaded at runtime, NEVER in source)
    pub bot_token: String,

    /// Authorized user IDs who can control the bot
    pub authorized_user_ids: Vec<i64>,
}

/// Database configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// Path to SQLite database file
    pub path: String,
}

/// Backtester configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestConfig {
    /// Path to historical data for replay
    pub data_path: String,

    /// Whether to run backtest on startup
    #[serde(default)]
    pub run_on_startup: bool,
}

/// Runtime state that can be modified via Telegram commands
#[derive(Debug, Clone)]
pub struct RuntimeState {
    pub bot_paused: Arc<AtomicBool>,
    pub active_wallet_label: Arc<RwLock<Option<String>>>,
    pub filters: Arc<RwLock<FilterConfig>>,
    pub risk: Arc<RwLock<RiskConfig>>,
}

impl RuntimeState {
    pub fn new(filters: FilterConfig, risk: RiskConfig) -> Self {
        Self {
            bot_paused: Arc::new(AtomicBool::new(false)),
            active_wallet_label: Arc::new(RwLock::new(None)),
            filters: Arc::new(RwLock::new(filters)),
            risk: Arc::new(RwLock::new(risk)),
        }
    }

    pub fn is_paused(&self) -> bool {
        self.bot_paused.load(Ordering::Relaxed)
    }

    pub fn pause(&self) {
        self.bot_paused.store(true, Ordering::Relaxed);
        info!("Bot PAUSED — managing existing exits only");
    }

    pub fn resume(&self) {
        self.bot_paused.store(false, Ordering::Relaxed);
        info!("Bot RESUMED — new entries allowed");
    }
}

// ── Default values ──────────────────────────────────────────────────

fn default_true() -> bool { true }
fn default_buy_amount() -> u64 { 100_000_000 } // 0.1 SOL
fn default_jito_tip() -> u64 { 500_000 }       // 0.0005 SOL
fn default_compute_units() -> u32 { 200_000 }
fn default_priority_fee() -> u64 { 1000 }
fn default_kelly_fraction() -> f64 { 0.25 }
fn default_max_position() -> u64 { 500_000_000 } // 0.5 SOL
fn default_stop_loss() -> f64 { 0.30 }
fn default_take_profit_1() -> f64 { 0.50 }
fn default_take_profit_2() -> f64 { 1.00 }
fn default_max_concurrent() -> usize { 5 }
fn default_consecutive_losses() -> usize { 5 }
fn default_min_score() -> u8 { 65 }
fn default_max_dev_concentration() -> f64 { 0.15 }
fn default_max_gini() -> f64 { 0.60 }
fn default_wash_trade_ratio() -> f64 { 0.40 }
fn default_entry_delay() -> u64 { 3 }
fn default_max_bundle_cluster() -> usize { 3 }
fn default_min_trades_for_analysis() -> u64 { 10 }

impl Config {
    /// Load configuration from a TOML file
    pub fn from_file(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        config.validate()?;
        Ok(config)
    }

    /// Create a default config for first-run paper mode
    pub fn default_paper() -> Self {
        Self {
            trading: TradingConfig {
                paper_mode: true,
                default_buy_amount_lamports: default_buy_amount(),
                active_wallet_label: None,
                jito_tip_lamports: default_jito_tip(),
                compute_unit_limit: default_compute_units(),
                priority_fee_micro_lamports: default_priority_fee(),
            },
            rpc: RpcConfig {
                http_endpoints: HashMap::new(),
                ws_endpoint: String::new(),
                geyser_endpoint: None,
                active_http_label: None,
            },
            risk: RiskConfig {
                kelly_fraction: default_kelly_fraction(),
                max_position_size_lamports: default_max_position(),
                stop_loss_pct: default_stop_loss(),
                take_profit_1_pct: default_take_profit_1(),
                take_profit_2_pct: default_take_profit_2(),
                max_concurrent_positions: default_max_concurrent(),
                consecutive_loss_circuit_breaker: default_consecutive_losses(),
            },
            filters: FilterConfig {
                min_entry_score: default_min_score(),
                max_dev_holder_concentration: default_max_dev_concentration(),
                max_gini_coefficient: default_max_gini(),
                min_wash_trade_unique_ratio: default_wash_trade_ratio(),
                entry_delay_seconds: default_entry_delay(),
                max_bundled_buy_cluster_size: default_max_bundle_cluster(),
                min_trades_for_analysis: default_min_trades_for_analysis(),
            },
            telegram: TelegramConfig {
                bot_token: String::new(),
                authorized_user_ids: vec![],
            },
            database: DatabaseConfig {
                path: "pf_sniper.db".to_string(),
            },
            backtest: BacktestConfig {
                data_path: "backtest_data".to_string(),
                run_on_startup: false,
            },
        }
    }

    /// Validate config invariants
    pub fn validate(&self) -> Result<(), Box<dyn std::error::Error>> {
        if self.trading.paper_mode {
            warn!("Running in PAPER MODE — no real capital at risk");
        }

        if self.rpc.http_endpoints.is_empty() {
            warn!("No HTTP RPC endpoints configured — bot will not start until RPC is set via /setrpc");
        }

        if self.rpc.ws_endpoint.is_empty() {
            warn!("No WebSocket endpoint configured — ws_listener cannot subscribe without WSS");
        }

        if self.telegram.bot_token.is_empty() {
            warn!("No Telegram bot token configured — commands won't work until token is set");
        }

        if !(0.0..=1.0).contains(&self.risk.kelly_fraction) {
            return Err("kelly_fraction must be between 0.0 and 1.0".into());
        }

        if self.risk.stop_loss_pct <= 0.0 || self.risk.stop_loss_pct >= 1.0 {
            return Err("stop_loss_pct must be between 0.0 and 1.0".into());
        }

        if self.filters.min_entry_score > 100 {
            return Err("min_entry_score must be between 0 and 100".into());
        }

        Ok(())
    }

    /// Write current config back to disk
    pub fn to_file(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let content = toml::to_string_pretty(self)?;
        fs::write(path, content)?;
        info!("Config written to {:?}", path);
        Ok(())
    }
}

/// Generate a default config.toml template file at the given path
pub fn generate_default_config(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::default_paper();
    config.to_file(path)?;
    info!("Default config generated at {:?}", path);
    info!("EDIT THIS FILE before running — fill in RPC endpoints, Telegram token, and wallet keystore path");
    Ok(())
}
