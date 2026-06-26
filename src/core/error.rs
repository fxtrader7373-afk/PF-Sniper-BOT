//! Error types for pf-sniper
//!
//! Every error carries semantic meaning: we distinguish between
//! recoverable failures (rate limits, transient network issues)
//! and fatal errors (keystore corruption, invalid signatures).

use thiserror::Error;

#[derive(Error, Debug)]
pub enum SniperError {
    #[error("RPC error: {source}")]
    RpcError {
        #[from]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("Rate limited (HTTP 429) on endpoint: {endpoint}")]
    RateLimited { endpoint: String },

    #[error("Transaction submission failed: {msg}, signature: {sig:?}")]
    TxSubmissionFailed { msg: String, sig: Option<String> },

    #[error("Bundle rejected by Jito Block Engine: {reason}")]
    BundleRejected { reason: String },

    #[error("Simulation failed: {msg}")]
    SimulationFailed { msg: String },

    #[error("Decode error: {msg}")]
    DecodeError { msg: String },

    #[error("Keystore error: {msg}")]
    KeystoreError { msg: String },

    #[error("Database error: {source}")]
    DatabaseError {
        #[from]
        source: rusqlite::Error,
    },

    #[error("Configuration error: {msg}")]
    ConfigError { msg: String },

    #[error("Telegram bot error: {msg}")]
    TelegramError { msg: String },

    #[error("TUI error: {msg}")]
    TuiError { msg: String },

    #[error("Backtest error: {msg}")]
    BacktestError { msg: String },

    #[error("Anomaly detected: {signal_name} drifted by {kl_divergence:.2}")]
    AnomalyDetected {
        signal_name: String,
        kl_divergence: f64,
    },

    #[error("Circuit breaker triggered after {count} consecutive losses")]
    CircuitBreaker { count: usize },

    #[error("Entry score {score} below minimum threshold {min}")]
    ScoreBelowThreshold { score: u8, min: u8 },

    #[error("Position size {size} exceeds maximum {max}")]
    PositionSizeExceeded { size: u64, max: u64 },

    #[error("Max concurrent positions ({max}) reached")]
    MaxConcurrentPositions { max: usize },

    #[error("Unknown error: {msg}")]
    Unknown { msg: String },
}

pub type SniperResult<T> = Result<T, SniperError>;

impl From<serde_json::Error> for SniperError {
    fn from(e: serde_json::Error) -> Self {
        SniperError::DecodeError { msg: e.to_string() }
    }
}

impl From<solana_sdk::pubkey::ParsePubkeyError> for SniperError {
    fn from(e: solana_sdk::pubkey::ParsePubkeyError) -> Self {
        SniperError::DecodeError { msg: format!("Invalid pubkey: {}", e) }
    }
}

impl From<std::io::Error> for SniperError {
    fn from(e: std::io::Error) -> Self {
        SniperError::RpcError { source: Box::new(e) }
    }
}

impl From<tokio::task::JoinError> for SniperError {
    fn from(e: tokio::task::JoinError) -> Self {
        SniperError::Unknown { msg: format!("Tokio task join error: {}", e) }
    }
}
