use thiserror::Error;

#[derive(Error, Debug)]
pub enum SniperError {
    #[error("RPC error: {msg}")]
    RpcError { msg: String },
    #[error("Rate limited: {endpoint}")]
    RateLimited { endpoint: String },
    #[error("Tx failed: {msg}")]
    TxSubmissionFailed { msg: String, sig: Option<String> },
    #[error("Bundle rejected: {reason}")]
    BundleRejected { reason: String },
    #[error("Decode error: {msg}")]
    DecodeError { msg: String },
    #[error("Keystore error: {msg}")]
    KeystoreError { msg: String },
    #[error("Database error: {msg}")]
    DatabaseError { msg: String },
    #[error("Config error: {msg}")]
    ConfigError { msg: String },
    #[error("Telegram error: {msg}")]
    TelegramError { msg: String },
    #[error("TUI error: {msg}")]
    TuiError { msg: String },
    #[error("Backtest error: {msg}")]
    BacktestError { msg: String },
    #[error("Anomaly: {signal}")]
    AnomalyDetected { signal: String },
    #[error("Circuit breaker: {count} losses")]
    CircuitBreaker { count: usize },
    #[error("Score {score} < {min}")]
    ScoreBelowThreshold { score: u8, min: u8 },
    #[error("Size {size} > {max}")]
    PositionSizeExceeded { size: u64, max: u64 },
    #[error("Max positions ({max})")]
    MaxConcurrentPositions { max: usize },
    #[error("Unknown: {msg}")]
    Unknown { msg: String },
}
pub type SniperResult<T> = Result<T, SniperError>;
impl From<std::io::Error> for SniperError { fn from(e: std::io::Error) -> Self { SniperError::RpcError { msg: e.to_string() } } }
impl From<serde_json::Error> for SniperError { fn from(e: serde_json::Error) -> Self { SniperError::DecodeError { msg: e.to_string() } } }
impl From<solana_sdk::pubkey::ParsePubkeyError> for SniperError { fn from(e: solana_sdk::pubkey::ParsePubkeyError) -> Self { SniperError::DecodeError { msg: e.to_string() } } }
impl From<rusqlite::Error> for SniperError { fn from(e: rusqlite::Error) -> Self { SniperError::DatabaseError { msg: e.to_string() } } }
impl From<Box<dyn std::error::Error + Send + Sync>> for SniperError { fn from(e: Box<dyn std::error::Error + Send + Sync>) -> Self { SniperError::Unknown { msg: e.to_string() } } }
impl From<teloxide::RequestError> for SniperError { fn from(e: teloxide::RequestError) -> Self { SniperError::TelegramError { msg: e.to_string() } } }
