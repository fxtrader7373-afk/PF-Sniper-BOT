//! SQLite Database — trade journal, signal vectors, P&L tracking.
//!
//! Schema:
//!   trade_journal: every entry/exit, fees, slippage, P&L, full signal vector at entry time
//!   positions: currently tracked positions with live state
//!   dev_wallet_history: cross-pool reputation cache
//!   config_changes: audit log of all parameter adjustments via Telegram
//!
//! This journal is the training data for adaptive_weights retraining.

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Result as SqliteResult, params};
use solana_sdk::pubkey::Pubkey;
use std::path::Path;
use tracing::{info, warn};

use crate::core::error::{SniperError, SniperResult};
use crate::core::types::*;

pub struct TradeJournal {
    conn: Connection,
}

impl TradeJournal {
    /// Open or create the trade journal database
    pub fn open(path: &Path) -> SniperResult<Self> {
        let conn = Connection::open(path)
            .map_err(|e| SniperError::DatabaseError { msg: e.to_string() })?;

        // Create tables if they don't exist
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS trade_journal (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                token_mint TEXT NOT NULL,
                dev_wallet TEXT NOT NULL,
                entry_slot INTEGER NOT NULL,
                exit_slot INTEGER,
                entry_time TEXT NOT NULL,
                exit_time TEXT,
                entry_price REAL NOT NULL,
                exit_price REAL,
                entry_amount_sol REAL NOT NULL,
                exit_amount_sol REAL,
                swap_fee_sol REAL NOT NULL DEFAULT 0.0,
                price_impact_sol REAL NOT NULL DEFAULT 0.0,
                mev_tax_sol REAL NOT NULL DEFAULT 0.0,
                jito_tip_sol REAL NOT NULL DEFAULT 0.0,
                net_pnl_sol REAL NOT NULL DEFAULT 0.0,
                net_pnl_pct REAL NOT NULL DEFAULT 0.0,
                entry_score INTEGER NOT NULL,
                signal_vector_json TEXT NOT NULL,
                exit_reason TEXT NOT NULL DEFAULT 'Timeout',
                is_paper INTEGER NOT NULL DEFAULT 1,
                was_override INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS positions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                token_mint TEXT NOT NULL UNIQUE,
                entry_slot INTEGER NOT NULL,
                entry_price REAL NOT NULL,
                entry_amount_sol REAL NOT NULL,
                entry_tx_signature TEXT,
                current_price REAL NOT NULL DEFAULT 0.0,
                current_sol_value REAL NOT NULL DEFAULT 0.0,
                unrealized_pnl REAL NOT NULL DEFAULT 0.0,
                realized_pnl REAL NOT NULL DEFAULT 0.0,
                token_amount REAL NOT NULL,
                status TEXT NOT NULL DEFAULT 'Open',
                stop_loss_price REAL NOT NULL,
                take_profit_levels TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS dev_wallet_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                wallet TEXT NOT NULL,
                token_mint TEXT NOT NULL,
                exit_reason TEXT NOT NULL,
                net_pnl_pct REAL NOT NULL,
                entry_time TEXT NOT NULL,
                UNIQUE(wallet, token_mint)
            );
            CREATE TABLE IF NOT EXISTS config_changes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                param_name TEXT NOT NULL,
                old_value TEXT NOT NULL,
                new_value TEXT NOT NULL,
                changed_at TEXT NOT NULL,
                changed_by TEXT NOT NULL DEFAULT 'telegram'
            );
            CREATE TABLE IF NOT EXISTS anomaly_alerts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                signal_name TEXT NOT NULL,
                expected_mean REAL NOT NULL,
                expected_std REAL NOT NULL,
                observed_mean REAL NOT NULL,
                observed_std REAL NOT NULL,
                kl_divergence REAL NOT NULL,
                severity TEXT NOT NULL,
                timestamp TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS ab_test_results (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                test_name TEXT NOT NULL,
                started_at TEXT NOT NULL,
                ended_at TEXT,
                shadow_weights TEXT,
                live_weights TEXT,
                shadow_expectancy REAL,
                live_expectancy REAL,
                shadow_trades INTEGER,
                live_trades INTEGER,
                promoted INTEGER NOT NULL DEFAULT 0
            );"
        ).map_err(|e| SniperError::DatabaseError { msg: e.to_string() })?;

        info!("Database opened at {:?}", path);
        Ok(Self { conn })
    }

    /// Record a new trade entry in the journal
    pub fn record_entry(&self, entry: &TradeJournalEntry) -> SniperResult<i64> {
        self.conn.execute(
            "INSERT INTO trade_journal (
                token_mint, dev_wallet, entry_slot, entry_time,
                entry_price, entry_amount_sol, entry_score,
                signal_vector_json, is_paper, was_override
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                entry.token_mint,
                entry.dev_wallet,
                entry.entry_slot,
                entry.entry_time.to_rfc3339(),
                entry.entry_price,
                entry.entry_amount_sol,
                entry.entry_score,
                entry.signal_vector_json,
                if entry.is_paper { 1 } else { 0 },
                if entry.was_override { 1 } else { 0 },
            ],
        ).map_err(|e| SniperError::DatabaseError { msg: e.to_string() })?;

        let id = self.conn.last_insert_rowid();
        info!("Trade entry recorded: id={} token={}", id, entry.token_mint);
        Ok(id)
    }

    /// Record a trade exit (update existing entry)
    pub fn record_exit(&self, trade_id: i64, entry: &TradeJournalEntry) -> SniperResult<()> {
        self.conn.execute(
            "UPDATE trade_journal SET
                exit_slot = ?2,
                exit_time = ?3,
                exit_price = ?4,
                exit_amount_sol = ?5,
                swap_fee_sol = ?6,
                price_impact_sol = ?7,
                mev_tax_sol = ?8,
                jito_tip_sol = ?9,
                net_pnl_sol = ?10,
                net_pnl_pct = ?11,
                exit_reason = ?12
            WHERE id = ?1",
            params![
                trade_id,
                entry.exit_slot.unwrap_or(0),
                entry.exit_time.map(|t| t.to_rfc3339()),
                entry.exit_price.unwrap_or(0.0),
                entry.exit_amount_sol.unwrap_or(0.0),
                entry.swap_fee_sol,
                entry.price_impact_sol,
                entry.mev_tax_sol,
                entry.jito_tip_sol,
                entry.net_pnl_sol,
                entry.net_pnl_pct,
                format!("{:?}", entry.exit_reason),
            ],
        ).map_err(|e| SniperError::DatabaseError { msg: e.to_string() })?;

        // Also record in dev_wallet_history
        self.conn.execute(
            "INSERT OR IGNORE INTO dev_wallet_history (wallet, token_mint, exit_reason, net_pnl_pct, entry_time)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                entry.dev_wallet,
                entry.token_mint,
                format!("{:?}", entry.exit_reason),
                entry.net_pnl_pct,
                entry.entry_time.to_rfc3339(),
            ],
        ).map_err(|e| SniperError::DatabaseError { msg: e.to_string() })?;

        info!("Trade exit recorded: id={} pnl={:.4} SOL", trade_id, entry.net_pnl_sol);
        Ok(())
    }

    /// Get the last N closed trades from the journal
    pub fn get_recent_trades(&self, n: usize) -> SniperResult<Vec<TradeJournalEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, token_mint, dev_wallet, entry_slot, exit_slot,
                    entry_time, exit_time, entry_price, exit_price,
                    entry_amount_sol, exit_amount_sol, swap_fee_sol,
                    price_impact_sol, mev_tax_sol, jito_tip_sol,
                    net_pnl_sol, net_pnl_pct, entry_score,
                    signal_vector_json, exit_reason, is_paper, was_override
             FROM trade_journal
             WHERE exit_time IS NOT NULL
             ORDER BY exit_time DESC
             LIMIT ?1"
        ).map_err(|e| SniperError::DatabaseError { msg: e.to_string() })?;

        let trades = stmt.query_map([n], |row| {
            Ok(TradeJournalEntry {
                id: row.get(0)?,
                token_mint: row.get(1)?,
                dev_wallet: row.get(2)?,
                entry_slot: row.get(3)?,
                exit_slot: row.get(4)?,
                entry_time: DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?).unwrap().with_timezone(&Utc),
                exit_time: row.get::<_, Option<String>>(6)?.and_then(|s| DateTime::parse_from_rfc3339(&s).ok()).map(|dt| dt.with_timezone(&Utc)),
                entry_price: row.get(7)?,
                exit_price: row.get(8)?,
                entry_amount_sol: row.get(9)?,
                exit_amount_sol: row.get(10)?,
                swap_fee_sol: row.get(11)?,
                price_impact_sol: row.get(12)?,
                mev_tax_sol: row.get(13)?,
                jito_tip_sol: row.get(14)?,
                net_pnl_sol: row.get(15)?,
                net_pnl_pct: row.get(16)?,
                entry_score: row.get(17)?,
                signal_vector_json: row.get(18)?,
                exit_reason: serde_json::from_str(&format!("\"{}\"", row.get::<_, String>(19)?)).unwrap_or(ExitReason::Timeout),
                is_paper: row.get::<_, i32>(20)? != 0,
                was_override: row.get::<_, i32>(21)? != 0,
            })
        }).map_err(|e| SniperError::DatabaseError { msg: e.to_string() })?;

        trades.collect::<Result<Vec<_>, _>>()
            .map_err(|e| SniperError::DatabaseError { msg: e.to_string() })
    }

    /// Compute P&L summary for a time period
    pub fn get_pnl_summary(&self, since: DateTime<Utc>) -> SniperResult<PnlSummary> {
        let since_str = since.to_rfc3339();

        let mut stmt = self.conn.prepare(
            "SELECT COUNT(*),
                    SUM(net_pnl_sol),
                    AVG(net_pnl_pct),
                    SUM(CASE WHEN net_pnl_sol > 0 THEN 1 ELSE 0 END),
                    SUM(CASE WHEN net_pnl_sol <= 0 THEN 1 ELSE 0 END),
                    MAX(net_pnl_sol),
                    MIN(net_pnl_sol),
                    SUM(swap_fee_sol),
                    SUM(price_impact_sol),
                    SUM(mev_tax_sol),
                    SUM(jito_tip_sol)
             FROM trade_journal
             WHERE exit_time >= ?1 AND was_override = 0"
        ).map_err(|e| SniperError::DatabaseError { msg: e.to_string() })?;

        let summary: PnlSummary = stmt.query_row([since_str], |row| {
            Ok(PnlSummary {
                total_trades: row.get(0)?,
                total_pnl_sol: row.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                avg_pnl_pct: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                winning_trades: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                losing_trades: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                best_trade_sol: row.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                worst_trade_sol: row.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
                total_fees_sol: row.get::<_, Option<f64>>(7)?.unwrap_or(0.0) + row.get::<_, Option<f64>>(8)?.unwrap_or(0.0) + row.get::<_, Option<f64>>(9)?.unwrap_or(0.0),
                total_tips_sol: row.get::<_, Option<f64>>(10)?.unwrap_or(0.0),
            })
        }).map_err(|e| SniperError::DatabaseError { msg: e.to_string() })?;

        Ok(summary)
    }

    /// Record a configuration change (audit trail)
    pub fn record_config_change(&self, param: &str, old_value: &str, new_value: &str, changed_by: &str) -> SniperResult<()> {
        self.conn.execute(
            "INSERT INTO config_changes (param_name, old_value, new_value, changed_at, changed_by)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![param, old_value, new_value, Utc::now().to_rfc3339(), changed_by],
        ).map_err(|e| SniperError::DatabaseError { msg: e.to_string() })?;

        info!("Config change recorded: {} = {} → {} (by {})", param, old_value, new_value, changed_by);
        Ok(())
    }

    /// Record an anomaly alert
    pub fn record_anomaly(&self, alert: &AnomalyAlert) -> SniperResult<()> {
        self.conn.execute(
            "INSERT INTO anomaly_alerts (signal_name, expected_mean, expected_std, observed_mean, observed_std, kl_divergence, severity, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                alert.signal_name,
                alert.expected_mean,
                alert.expected_std,
                alert.observed_mean,
                alert.observed_std,
                alert.kl_divergence,
                format!("{:?}", alert.severity),
                alert.timestamp.to_rfc3339(),
            ],
        ).map_err(|e| SniperError::DatabaseError { msg: e.to_string() })?;

        Ok(())
    }

    /// Record an A/B test result
    pub fn record_ab_test(&self, test: &AbTestState, promoted: bool) -> SniperResult<()> {
        self.conn.execute(
            "INSERT INTO ab_test_results (test_name, started_at, ended_at, shadow_weights, live_weights, shadow_expectancy, live_expectancy, shadow_trades, live_trades, promoted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                "weight_update",
                test.started_at.to_rfc3339(),
                Utc::now().to_rfc3339(),
                serde_json::to_string(&test.shadow_weights).unwrap_or_default(),
                serde_json::to_string(&test.live_weights).unwrap_or_default(),
                test.shadow_expectancy,
                test.live_expectancy,
                test.shadow_trades,
                test.live_trades,
                if promoted { 1 } else { 0 },
            ],
        ).map_err(|e| SniperError::DatabaseError { msg: e.to_string() })?;

        Ok(())
    }
}

/// P&L summary for a time period
#[derive(Debug, Clone)]
pub struct PnlSummary {
    pub total_trades: i64,
    pub total_pnl_sol: f64,
    pub avg_pnl_pct: f64,
    pub winning_trades: i64,
    pub losing_trades: i64,
    pub best_trade_sol: f64,
    pub worst_trade_sol: f64,
    pub total_fees_sol: f64,
    pub total_tips_sol: f64,
}

impl PnlSummary {
    pub fn win_rate(&self) -> f64 {
        if self.total_trades == 0 {
            return 0.0;
        }
        self.winning_trades as f64 / self.total_trades as f64
    }

    pub fn profit_factor(&self) -> f64 {
        if self.total_fees_sol == 0.0 {
            return 0.0;
        }
        self.total_pnl_sol / self.total_fees_sol
    }
}
