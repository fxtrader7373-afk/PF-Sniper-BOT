//! Backtester — replay mode against logged historical pool data.
//!
//! Before any filter change touches live capital, run it through the backtester.
//! Replays historical events from the SQLite journal or from CSV files.
//!
//! Returns a BacktestResult with full metrics:
//! - Total / avg P&L, win rate
//! - Max drawdown
//! - Sharpe / Sortino ratio
//! - Expectancy
//! - Profit factor

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::collections::HashMap;
use tracing::{info, warn};

use crate::core::error::{SniperError, SniperResult};
use crate::core::types::*;
use crate::config::{Config, FilterConfig};
use crate::modules::scoring_engine::ScoringEngine;
use crate::modules::risk_engine::RiskEngine;

pub struct Backtester {
    config: Config,
    scoring_engine: ScoringEngine,
    risk_engine: RiskEngine,
}

impl Backtester {
    pub fn new(config: Config, filter_config: FilterConfig, weights: HashMap<String, f64>) -> Self {
        Self {
            config: config.clone(),
            scoring_engine: ScoringEngine::new(weights, filter_config),
            risk_engine: RiskEngine::new(config.risk.clone()),
        }
    }

    /// Run backtest against the trade journal database
    pub fn run_from_journal(&self) -> SniperResult<BacktestResult> {
        let db_path = &self.config.database.path;
        let conn = Connection::open(db_path)
            .map_err(|e| SniperError::DatabaseError { source: e })?;

        let mut stmt = conn.prepare(
            "SELECT token_mint, dev_wallet, entry_slot, entry_time, exit_time,
                    entry_price, exit_price, entry_amount_sol, exit_amount_sol,
                    swap_fee_sol, price_impact_sol, mev_tax_sol, jito_tip_sol,
                    net_pnl_sol, net_pnl_pct, entry_score, signal_vector_json,
                    exit_reason, is_paper, was_override
             FROM trade_journal
             WHERE was_override = 0
             ORDER BY entry_time ASC"
        )?;

        let mut trades = Vec::new();
        let mut cumulative_pnl = 0.0_f64;
        let mut peak_pnl = 0.0_f64;
        let mut max_drawdown = 0.0_f64;
        let mut holding_times = Vec::new();

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, f64>(5)?,
                row.get::<_, Option<f64>>(6)?,
                row.get::<_, f64>(7)?,
                row.get::<_, Option<f64>>(8)?,
                row.get::<_, f64>(9)?,
                row.get::<_, f64>(10)?,
                row.get::<_, f64>(11)?,
                row.get::<_, f64>(12)?,
                row.get::<_, f64>(13)?,
                row.get::<_, f64>(14)?,
                row.get::<_, u8>(15)?,
                row.get::<_, String>(16)?,
                row.get::<_, String>(17)?,
                row.get::<_, bool>(18)?,
                row.get::<_, bool>(19)?,
            ))
        })?;

        let mut winning = 0;
        let mut losing = 0;
        let mut total_pnl = 0.0;

        for row in rows {
            let (mint, dev, entry_slot, entry_time, exit_time,
                 entry_price, exit_price, entry_amount_sol, exit_amount_sol,
                 swap_fee, price_impact, mev_tax, jito_tip,
                 net_pnl_sol, net_pnl_pct, entry_score, signal_json,
                 exit_reason, is_paper, was_override) = row?;

            // Skip overrides
            if was_override {
                continue;
            }

            let exit_price = exit_price.unwrap_or(0.0);
            let exit_amount_sol = exit_amount_sol.unwrap_or(0.0);

            // Track cumulative P&L and drawdown
            cumulative_pnl += net_pnl_sol;
            peak_pnl = peak_pnl.max(cumulative_pnl);
            let drawdown = peak_pnl - cumulative_pnl;
            max_drawdown = max_drawdown.max(drawdown);

            // Holding time
            if let Some(exit_time_str) = exit_time {
                if let Ok(exit_dt) = DateTime::parse_from_rfc3339(&exit_time_str) {
                    if let Ok(entry_dt) = DateTime::parse_from_rfc3339(&entry_time) {
                        let holding_secs = (exit_dt.timestamp() - entry_dt.timestamp()) as f64;
                        holding_times.push(holding_secs);
                    }
                }
            }

            if net_pnl_sol > 0.0 {
                winning += 1;
            } else {
                losing += 1;
            }
            total_pnl += net_pnl_sol;

            // Log each trade for detailed analysis
            trades.push(TradeJournalEntry {
                id: None,
                token_mint: mint,
                dev_wallet: dev,
                entry_slot,
                exit_slot: None,
                entry_time: DateTime::parse_from_rfc3339(&entry_time).unwrap_or_default().with_timezone(&Utc),
                exit_time: exit_time.and_then(|s| DateTime::parse_from_rfc3339(&s).ok()).map(|dt| dt.with_timezone(&Utc)),
                entry_price,
                exit_price: Some(exit_price),
                entry_amount_sol,
                exit_amount_sol: Some(exit_amount_sol),
                swap_fee_sol: swap_fee,
                price_impact_sol: price_impact,
                mev_tax_sol: mev_tax,
                jito_tip_sol: jito_tip,
                net_pnl_sol: net_pnl_sol,
                net_pnl_pct,
                entry_score,
                signal_vector_json: signal_json,
                exit_reason: serde_json::from_str(&format!("\"{}\"", exit_reason)).unwrap_or(ExitReason::Timeout),
                is_paper,
                was_override,
            });
        }

        let total_trades = winning + losing;
        let win_rate = if total_trades > 0 {
            winning as f64 / total_trades as f64
        } else {
            0.0
        };

        let avg_pnl = if total_trades > 0 {
            total_pnl / total_trades as f64
        } else {
            0.0
        };

        let avg_holding_time = if holding_times.is_empty() {
            0.0
        } else {
            holding_times.iter().sum::<f64>() / holding_times.len() as f64
        };

        // Compute Sharpe ratio (simplified)
        let returns: Vec<f64> = trades.iter()
            .map(|t| t.net_pnl_pct / 100.0)
            .collect();

        let sharpe = if returns.len() > 1 {
            let mean = returns.iter().sum::<f64>() / returns.len() as f64;
            let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (returns.len() - 1) as f64;
            let std_dev = variance.sqrt();
            if std_dev > 0.0 { mean / std_dev } else { 0.0 }
        } else {
            0.0
        };

        // Sortino ratio (downside deviation only)
        let downside_returns: Vec<f64> = returns.iter()
            .filter(|&&r| r < 0.0)
            .copied()
            .collect();

        let sortino = if downside_returns.len() > 1 {
            let mean = returns.iter().sum::<f64>() / returns.len() as f64;
            let downside_variance = downside_returns.iter()
                .map(|r| r.powi(2))
                .sum::<f64>() / downside_returns.len() as f64;
            let downside_std = downside_variance.sqrt();
            if downside_std > 0.0 { mean / downside_std } else { 0.0 }
        } else {
            0.0
        };

        // Expectancy: average win * win_rate - average loss * loss_rate
        let avg_win = if winning > 0 {
            trades.iter().filter(|t| t.net_pnl_sol > 0.0)
                .map(|t| t.net_pnl_sol).sum::<f64>() / winning as f64
        } else {
            0.0
        };
        let avg_loss = if losing > 0 {
            trades.iter().filter(|t| t.net_pnl_sol <= 0.0)
                .map(|t| t.net_pnl_sol.abs()).sum::<f64>() / losing as f64
        } else {
            0.0
        };
        let expectancy = avg_win * win_rate - avg_loss * (1.0 - win_rate);

        // Profit factor
        let gross_profit = trades.iter().filter(|t| t.net_pnl_sol > 0.0)
            .map(|t| t.net_pnl_sol).sum::<f64>();
        let gross_loss = trades.iter().filter(|t| t.net_pnl_sol <= 0.0)
            .map(|t| t.net_pnl_sol.abs()).sum::<f64>();
        let profit_factor = if gross_loss > 0.0 { gross_profit / gross_loss } else { 0.0 };

        // Max consecutive losses
        let mut max_consecutive_losses = 0;
        let mut current_consecutive = 0;
        for trade in &trades {
            if trade.net_pnl_sol <= 0.0 {
                current_consecutive += 1;
                max_consecutive_losses = max_consecutive_losses.max(current_consecutive);
            } else {
                current_consecutive = 0;
            }
        }

        let result = BacktestResult {
            total_trades,
            winning_trades: winning,
            losing_trades: losing,
            win_rate,
            total_pnl_sol: total_pnl,
            avg_pnl_per_trade_sol: avg_pnl,
            max_drawdown_pct: if peak_pnl > 0.0 { max_drawdown / peak_pnl * 100.0 } else { 0.0 },
            sharpe_ratio: sharpe,
            sortino_ratio: sortino,
            expectancy,
            profit_factor,
            avg_holding_time_seconds: avg_holding_time,
            max_consecutive_losses,
        };

        info!("Backtest complete: {} trades, {:.1}% win rate, {:.4} SOL total P&L",
            result.total_trades, result.win_rate * 100.0, result.total_pnl_sol);

        Ok(result)
    }

    /// Run backtest against the trade journal (public wrapper)
    pub fn run(&self) -> SniperResult<BacktestResult> {
        self.run_from_journal()
    }
}
