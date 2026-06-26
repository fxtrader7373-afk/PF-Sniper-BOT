//! Exit Manager — automated stop-loss + partial take-profit ladder.
//!
//! No manual override path except the emergency /forcesell Telegram command,
//! which is explicitly logged and excluded from adaptive_weights training.
//!
//! Ladder structure:
//! 1. Stop-loss: sell 100% if price drops below (entry_price * (1 - stop_loss_pct))
//! 2. Take-profit level 1: sell 50% if price reaches (entry_price * (1 + tp_1_pct))
//! 3. Take-profit level 2: sell remaining 50% if price reaches (entry_price * (1 + tp_2_pct))
//! 4. Timeout: sell 100% if position has been open longer than max_hold_time

use chrono::{DateTime, Duration, Utc};
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use tracing::{info, warn};

use crate::core::error::SniperResult;
use crate::core::types::{Position, PositionStatus};
use crate::config::RiskConfig;
use crate::modules::execution::ExecutionEngine;

/// Exit event emitted when a position should be closed
#[derive(Debug, Clone)]
pub struct ExitEvent {
    pub token_mint: Pubkey,
    pub exit_reason: ExitReason,
    pub sell_amount_pct: f64,
    pub exit_price: f64,
    pub expected_sol_output: f64,
    pub bonding_curve: Pubkey,
    pub associated_bonding_curve: Pubkey,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub enum ExitReason {
    StopLoss,
    TakeProfit { level: u8 },
    Timeout,
    BondingCurveGraduated,
    PoolRugDetected,
    ManualOverride,
}

pub struct ExitManager {
    risk_config: RiskConfig,
    max_hold_duration: Duration,
    positions: HashMap<Pubkey, Position>,
    tp1_hit: HashMap<Pubkey, bool>,
}

impl ExitManager {
    pub fn new(risk_config: RiskConfig, max_hold_minutes: u64) -> Self {
        Self {
            risk_config,
            max_hold_duration: Duration::minutes(max_hold_minutes as i64),
            positions: HashMap::new(),
            tp1_hit: HashMap::new(),
        }
    }

    /// Track a new position
    pub fn track_position(&mut self, position: Position) {
        self.positions.insert(position.token_mint, position);
        self.tp1_hit.insert(position.token_mint, false);
        info!("Tracking position: {} at entry price {}", position.token_mint, position.entry_price);
    }

    /// Stop tracking a position (after full exit)
    pub fn untrack_position(&mut self, token_mint: &Pubkey) {
        self.positions.remove(token_mint);
        self.tp1_hit.remove(token_mint);
    }

    /// Check all tracked positions for exit conditions
    /// Returns a list of exit events that should be executed
    pub async fn check_exits(
        &mut self,
        current_prices: &HashMap<Pubkey, f64>,
    ) -> Vec<ExitEvent> {
        let mut exit_events = Vec::new();

        for (&mint, position) in self.positions.iter_mut() {
            let current_price = match current_prices.get(&mint) {
                Some(&price) => price,
                None => continue, // No price data yet
            };

            position.current_price = current_price;
            position.updated_at = Utc::now();

            // Update unrealized P&L
            let pnl_pct = if position.entry_price > 0.0 {
                (current_price - position.entry_price) / position.entry_price
            } else {
                0.0
            };
            position.unrealized_pnl = pnl_pct * position.entry_amount_sol;

            // Check exit conditions in priority order

            // 1. Stop-loss check
            let stop_loss_price = position.entry_price * (1.0 - self.risk_config.stop_loss_pct);
            if current_price <= stop_loss_price {
                exit_events.push(ExitEvent {
                    token_mint: mint,
                    exit_reason: ExitReason::StopLoss,
                    sell_amount_pct: 1.0,
                    exit_price: current_price,
                    expected_sol_output: position.current_sol_value,
                    bonding_curve: Pubkey::default(),       // Filled by caller from on-chain data
                    associated_bonding_curve: Pubkey::default(),
                    timestamp: Utc::now(),
                });
                position.status = PositionStatus::StoppedOut;
                warn!("STOP-LOSS triggered for {} at price {}", mint, current_price);
                continue;
            }

            // 2. Take-profit level 1 check
            let tp1_price = position.entry_price * (1.0 + self.risk_config.take_profit_1_pct);
            if current_price >= tp1_price && !self.tp1_hit.get(&mint).copied().unwrap_or(false) {
                self.tp1_hit.insert(mint, true);
                exit_events.push(ExitEvent {
                    token_mint: mint,
                    exit_reason: ExitReason::TakeProfit { level: 1 },
                    sell_amount_pct: 0.5,
                    exit_price: current_price,
                    expected_sol_output: position.current_sol_value * 0.5,
                    bonding_curve: Pubkey::default(),
                    associated_bonding_curve: Pubkey::default(),
                    timestamp: Utc::now(),
                });
                position.status = PositionStatus::TakeProfit { level: 1 };
                info!("TAKE-PROFIT level 1 triggered for {} at price {}", mint, current_price);
                continue;
            }

            // 3. Take-profit level 2 check (only if level 1 already hit)
            let tp2_price = position.entry_price * (1.0 + self.risk_config.take_profit_2_pct);
            if current_price >= tp2_price && self.tp1_hit.get(&mint).copied().unwrap_or(false) {
                exit_events.push(ExitEvent {
                    token_mint: mint,
                    exit_reason: ExitReason::TakeProfit { level: 2 },
                    sell_amount_pct: 1.0, // sell remaining
                    exit_price: current_price,
                    expected_sol_output: position.current_sol_value,
                    bonding_curve: Pubkey::default(),
                    associated_bonding_curve: Pubkey::default(),
                    timestamp: Utc::now(),
                });
                position.status = PositionStatus::Closed;
                info!("TAKE-PROFIT level 2 triggered for {} at price {}", mint, current_price);
                continue;
            }

            // 4. Timeout check
            let elapsed = Utc::now().signed_duration_since(position.created_at);
            if elapsed > self.max_hold_duration {
                exit_events.push(ExitEvent {
                    token_mint: mint,
                    exit_reason: ExitReason::Timeout,
                    sell_amount_pct: 1.0,
                    exit_price: current_price,
                    expected_sol_output: position.current_sol_value,
                    bonding_curve: Pubkey::default(),
                    associated_bonding_curve: Pubkey::default(),
                    timestamp: Utc::now(),
                });
                position.status = PositionStatus::Closed;
                warn!("TIMEOUT exit for {} after {}", mint, elapsed);
            }
        }

        exit_events
    }

    /// Execute all exit events through the execution engine
    pub async fn execute_exits(
        &mut self,
        exit_events: Vec<ExitEvent>,
        execution: &ExecutionEngine,
    ) -> SniperResult<Vec<String>> {
        let mut signatures = Vec::new();

        for event in exit_events {
            let position = match self.positions.get(&event.token_mint) {
                Some(p) => p.clone(),
                None => continue,
            };

            let token_amount = (position.token_amount * event.sell_amount_pct) as u64;

            match execution.execute_sell(
                event.token_mint,
                event.bonding_curve,
                event.associated_bonding_curve,
                token_amount,
                event.expected_sol_output,
            ).await {
                Ok(Some(sig)) => {
                    signatures.push(sig);
                    info!("Exit executed: {} for {}% at price {}",
                        event.token_mint, event.sell_amount_pct * 100.0, event.exit_price);

                    // If fully exited, untrack
                    if event.sell_amount_pct >= 1.0 {
                        self.untrack_position(&event.token_mint);
                    }
                }
                Ok(None) => {
                    // Paper mode — no actual execution
                    info!("[PAPER] Would execute exit: {} for {}%", event.token_mint, event.sell_amount_pct * 100.0);
                }
                Err(e) => {
                    warn!("Exit execution failed for {}: {}", event.token_mint, e);
                }
            }
        }

        Ok(signatures)
    }

    /// Force-sell a position (manual override via /forcesell)
    /// This is explicitly logged and excluded from training
    pub fn force_sell(&mut self, token_mint: &Pubkey) -> Option<ExitEvent> {
        let position = self.positions.get(token_mint)?;

        let event = ExitEvent {
            token_mint: *token_mint,
            exit_reason: ExitReason::ManualOverride,
            sell_amount_pct: 1.0,
            exit_price: position.current_price,
            expected_sol_output: position.current_sol_value,
            bonding_curve: Pubkey::default(),
            associated_bonding_curve: Pubkey::default(),
            timestamp: Utc::now(),
        };

        self.untrack_position(token_mint);
        warn!("MANUAL OVERRIDE /forcesell for {}", token_mint);
        Some(event)
    }

    /// Get all currently tracked positions
    pub fn tracked_positions(&self) -> Vec<&Position> {
        self.positions.values().collect()
    }

    /// Get count of open positions
    pub fn open_position_count(&self) -> usize {
        self.positions.values()
            .filter(|p| matches!(p.status, PositionStatus::Open | PositionStatus::PartiallyClosed { .. }))
            .count()
    }
}
