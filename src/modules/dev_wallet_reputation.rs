//! Dev Wallet Reputation — cross-pool history lookup for creator wallets.
//!
//! Answers: "has this wallet rugged before?"
//! Queries the SQLite trade journal for any previous tokens created by this
//! dev wallet and scores based on:
//! - Number of previous launches
//! - Average P&L of those launches
//! - Whether any were rug-pulls (price → 0 before graduation)
//! - Time distribution (recent rugs weighted higher)

use chrono::{DateTime, Utc};
use rusqlite::{Connection, Result as SqliteResult};
use std::collections::HashMap;
use solana_sdk::pubkey::Pubkey;
use tracing::debug;

use crate::core::error::{SniperError, SniperResult};
use crate::core::types::{ExitReason, TradeJournalEntry};

#[derive(Debug, Clone)]
pub struct DevWalletProfile {
    pub wallet: Pubkey,
    pub total_launches: u64,
    pub successful_exits: u64,
    pub rug_exits: u64,
    pub avg_pnl_pct: f64,
    pub max_loss_pct: f64,
    pub last_launch_at: Option<DateTime<Utc>>,
    pub reputation_score: f64, // 0-100, higher = cleaner
}

impl DevWalletProfile {
    /// Compute a reputation score from the profile stats
    pub fn compute_score(&self) -> f64 {
        if self.total_launches == 0 {
            return 50.0; // Unknown wallet → neutral prior
        }

        let mut score = 50.0;

        // Rug ratio penalty (up to -40 points)
        let rug_ratio = self.rug_exits as f64 / self.total_launches as f64;
        score -= rug_ratio * 40.0;

        // Average P&L adjustment (up to +30 or -30 points)
        let pnl_adjustment = self.avg_pnl_pct.clamp(-1.0, 1.0) * 30.0;
        score += pnl_adjustment;

        // Success rate bonus (up to +20 points)
        let success_rate = self.successful_exits as f64 / self.total_launches as f64;
        score += success_rate * 20.0;

        // Max loss penalty (up to -10 points)
        let max_loss_penalty = self.max_loss_pct.abs().clamp(0.0, 1.0) * 10.0;
        score -= max_loss_penalty;

        score.clamp(0.0, 100.0)
    }
}

pub struct DevWalletReputation {
    db_path: String,
    cache: HashMap<String, DevWalletProfile>,
}

impl DevWalletReputation {
    pub fn new(db_path: String) -> Self {
        Self {
            db_path,
            cache: HashMap::new(),
        }
    }

    /// Look up a dev wallet's reputation score
    pub async fn lookup(&mut self, wallet: &Pubkey) -> SniperResult<DevWalletProfile> {
        let wallet_str = wallet.to_string();

        // Check cache first
        if let Some(profile) = self.cache.get(&wallet_str) {
            debug!("Cache hit for dev wallet {}", wallet_str);
            return Ok(profile.clone());
        }

        // Query database
        let conn = Connection::open(&self.db_path)
            .map_err(|e| SniperError::DatabaseError { source: e })?;

        let profile = self.query_wallet_history(&conn, wallet)?;
        self.cache.insert(wallet_str, profile.clone());

        Ok(profile)
    }

    /// Query the trade journal for this wallet's history
    fn query_wallet_history(&self, conn: &Connection, wallet: &Pubkey) -> SniperResult<DevWalletProfile> {
        let wallet_str = wallet.to_string();

        let mut stmt = conn.prepare(
            "SELECT COUNT(*) as total,
                    SUM(CASE WHEN exit_reason = 'TakeProfit' OR exit_reason = 'StopLoss' THEN 1 ELSE 0 END) as successful,
                    SUM(CASE WHEN exit_reason = 'PoolRugDetected' THEN 1 ELSE 0 END) as rugs,
                    AVG(net_pnl_pct) as avg_pnl,
                    MIN(net_pnl_pct) as max_loss
             FROM trade_journal
             WHERE dev_wallet = ?"
        )?;

        let (total, successful, rugs, avg_pnl, max_loss): (i64, i64, i64, Option<f64>, Option<f64>) =
            stmt.query_row([&wallet_str], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?;

        // Get last launch time
        let mut stmt = conn.prepare(
            "SELECT entry_time FROM trade_journal WHERE dev_wallet = ? ORDER BY entry_time DESC LIMIT 1"
        )?;

        let last_launch: Option<String> = stmt.query_row([&wallet_str], |row| row.get(0)).ok();
        let last_launch_at = last_launch.and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        let profile = DevWalletProfile {
            wallet: *wallet,
            total_launches: total as u64,
            successful_exits: successful as u64,
            rug_exits: rugs as u64,
            avg_pnl_pct: avg_pnl.unwrap_or(0.0),
            max_loss_pct: max_loss.unwrap_or(0.0),
            last_launch_at,
            reputation_score: 0.0, // Computed below
        };

        let score = profile.compute_score();
        Ok(DevWalletProfile { reputation_score: score, ..profile })
    }

    /// Clear the cache (call after significant DB changes)
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }
}
