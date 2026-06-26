use rusqlite::Connection;
use tracing::info;
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
    pub fn new(config: Config, filter_config: FilterConfig, weights: std::collections::HashMap<String, f64>) -> Self {
        Self { config: config.clone(), scoring_engine: ScoringEngine::new(weights, filter_config), risk_engine: RiskEngine::new(config.risk.clone()) }
    }

    pub fn run(&self) -> SniperResult<BacktestResult> {
        let conn = Connection::open(&self.config.database.path).map_err(|e| SniperError::DatabaseError { msg: e.to_string() })?;
        let mut stmt = conn.prepare(
            "SELECT token_mint, dev_wallet, entry_slot, entry_time, exit_time,
                    entry_price, exit_price, entry_amount_sol, exit_amount_sol,
                    swap_fee_sol, price_impact_sol, mev_tax_sol, jito_tip_sol,
                    net_pnl_sol, net_pnl_pct, entry_score, signal_vector_json,
                    exit_reason, is_paper, was_override
             FROM trade_journal WHERE was_override = 0 ORDER BY entry_time ASC"
        ).map_err(|e| SniperError::DatabaseError { msg: e.to_string() })?;

        let mut trades = Vec::new();
        let mut cumulative_pnl = 0.0_f64;
        let mut peak_pnl = 0.0_f64;
        let mut max_drawdown = 0.0_f64;
        let mut holding_times = Vec::new();
        let mut winning = 0;
        let mut losing = 0;
        let mut total_pnl = 0.0;

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?, row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?, row.get::<_, f64>(5)?,
                row.get::<_, Option<f64>>(6)?, row.get::<_, f64>(7)?,
                row.get::<_, Option<f64>>(8)?, row.get::<_, f64>(9)?,
                row.get::<_, f64>(10)?, row.get::<_, f64>(11)?,
                row.get::<_, f64>(12)?, row.get::<_, f64>(13)?,
                row.get::<_, f64>(14)?, row.get::<_, u8>(15)?,
                row.get::<_, String>(16)?, row.get::<_, String>(17)?,
                row.get::<_, bool>(18)?, row.get::<_, bool>(19)?,
            ))
        }).map_err(|e| SniperError::DatabaseError { msg: e.to_string() })?;

        for row_result in rows {
            let (mint, dev, entry_slot, entry_time, exit_time_opt,
                 entry_price, exit_price, entry_amount_sol, exit_amount_sol,
                 swap_fee, price_impact, mev_tax, jito_tip,
                 net_pnl_sol, net_pnl_pct, entry_score, signal_json,
                 exit_reason, is_paper, _was_override) = row_result.map_err(|e| SniperError::DatabaseError { msg: e.to_string() })?;

            let exit_price = exit_price.unwrap_or(0.0);
            let exit_amount_sol = exit_amount_sol.unwrap_or(0.0);

            cumulative_pnl += net_pnl_sol;
            peak_pnl = peak_pnl.max(cumulative_pnl);
            max_drawdown = max_drawdown.max(peak_pnl - cumulative_pnl);

            if let Some(ref ets) = exit_time_opt {
                if let Ok(exit_dt) = chrono::DateTime::parse_from_rfc3339(ets) {
                    if let Ok(entry_dt) = chrono::DateTime::parse_from_rfc3339(&entry_time) {
                        holding_times.push((exit_dt.timestamp() - entry_dt.timestamp()) as f64);
                    }
                }
            }
      if net_pnl_sol > 0.0 { winning += 1; } else { losing += 1; }
            total_pnl += net_pnl_sol;

            trades.push(TradeJournalEntry {
                id: None, token_mint: mint, dev_wallet: dev, entry_slot,
                exit_slot: None,
                entry_time: chrono::DateTime::parse_from_rfc3339(&entry_time).unwrap_or_default().with_timezone(&chrono::Utc),
                exit_time: exit_time_opt.as_deref().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|dt| dt.with_timezone(&chrono::Utc)),
                entry_price, exit_price: Some(exit_price), entry_amount_sol,
                exit_amount_sol: Some(exit_amount_sol), swap_fee_sol: swap_fee,
                price_impact_sol: price_impact, mev_tax_sol: mev_tax, jito_tip_sol: jito_tip,
                net_pnl_sol, net_pnl_pct, entry_score, signal_vector_json: signal_json,
                exit_reason: serde_json::from_str(&format!("\"{}\"", exit_reason)).unwrap_or(ExitReason::Timeout),
                is_paper, was_override: false,
            });
        }

        let total_trades = (winning + losing) as usize;
        let win_rate = if total_trades > 0 { winning as f64 / total_trades as f64 } else { 0.0 };
        let avg_pnl = if total_trades > 0 { total_pnl / total_trades as f64 } else { 0.0 };
        let avg_hold = if holding_times.is_empty() { 0.0 } else { holding_times.iter().sum::<f64>() / holding_times.len() as f64 };

        let returns: Vec<f64> = trades.iter().map(|t| t.net_pnl_pct / 100.0).collect();
        let sharpe = if returns.len() > 1 {
            let m = returns.iter().sum::<f64>() / returns.len() as f64;
            let v = returns.iter().map(|r| (r - m).powi(2)).sum::<f64>() / (returns.len() - 1) as f64;
            let s = v.sqrt(); if s > 0.0 { m / s } else { 0.0 }
        } else { 0.0 };

        let downside: Vec<f64> = returns.iter().filter(|&&r| r < 0.0).copied().collect();
        let sortino = if downside.len() > 1 {
            let m = returns.iter().sum::<f64>() / returns.len() as f64;
            let dv = downside.iter().map(|r| r.powi(2)).sum::<f64>() / downside.len() as f64;
            let ds = dv.sqrt(); if ds > 0.0 { m / ds } else { 0.0 }
        } else { 0.0 };

        let avg_win = if winning > 0 { trades.iter().filter(|t| t.net_pnl_sol > 0.0).map(|t| t.net_pnl_sol).sum::<f64>() / winning as f64 } else { 0.0 };
        let avg_loss = if losing > 0 { trades.iter().filter(|t| t.net_pnl_sol <= 0.0).map(|t| t.net_pnl_sol.abs()).sum::<f64>() / losing as f64 } else { 0.0 };
        let expectancy = avg_win * win_rate - avg_loss * (1.0 - win_rate);
        let gross_profit = trades.iter().filter(|t| t.net_pnl_sol > 0.0).map(|t| t.net_pnl_sol).sum::<f64>();
        let gross_loss = trades.iter().filter(|t| t.net_pnl_sol <= 0.0).map(|t| t.net_pnl_sol.abs()).sum::<f64>();
        let profit_factor = if gross_loss > 0.0 { gross_profit / gross_loss } else { 0.0 };

        let mut max_consec = 0;
        let mut cur_consec = 0;
        for t in &trades { if t.net_pnl_sol <= 0.0 { cur_consec += 1; max_consec = max_consec.max(cur_consec); } else { cur_consec = 0; } }


Ok(BacktestResult {
            total_trades, winning_trades: winning as usize, losing_trades: losing as usize,
            win_rate, total_pnl_sol: total_pnl, avg_pnl_per_trade_sol: avg_pnl,
            max_drawdown_pct: if peak_pnl > 0.0 { max_drawdown / peak_pnl * 100.0 } else { 0.0 },
            sharpe_ratio: sharpe, sortino_ratio: sortino, expectancy, profit_factor,
            avg_holding_time_seconds: avg_hold, max_consecutive_losses: max_consec,
        })
    }
}
