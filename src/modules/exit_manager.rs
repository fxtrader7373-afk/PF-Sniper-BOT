use chrono::{DateTime, Duration, Utc};
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use tracing::{info, warn};
use crate::core::error::SniperResult;
use crate::core::types::{Position, PositionStatus};
use crate::config::RiskConfig;

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
pub enum ExitReason { StopLoss, TakeProfit { level: u8 }, Timeout, BondingCurveGraduated, PoolRugDetected, ManualOverride }

pub struct ExitManager {
    risk_config: RiskConfig,
    max_hold_duration: Duration,
    positions: HashMap<Pubkey, Position>,
    tp1_hit: HashMap<Pubkey, bool>,
}

impl ExitManager {
    pub fn new(risk_config: RiskConfig, max_hold_minutes: u64) -> Self {
        Self { risk_config, max_hold_duration: Duration::minutes(max_hold_minutes as i64), positions: HashMap::new(), tp1_hit: HashMap::new() }
    }

    pub fn track_position(&mut self, position: Position) {
        let mint = position.token_mint;
        let entry_price = position.entry_price;
        self.positions.insert(mint, position);
        self.tp1_hit.insert(mint, false);
        info!("Tracking position: {} at entry price {}", mint, entry_price);
    }

    pub fn untrack_position(&mut self, token_mint: &Pubkey) {
        self.positions.remove(token_mint);
        self.tp1_hit.remove(token_mint);
    }

    pub async fn check_exits(&mut self, current_prices: &HashMap<Pubkey, f64>) -> Vec<ExitEvent> {
        let mut exit_events = Vec::new();
        for (&mint, position) in self.positions.iter_mut() {
            let current_price = match current_prices.get(&mint) { Some(&p) => p, None => continue };
            position.current_price = current_price;
            position.updated_at = Utc::now();
            let pnl_pct = if position.entry_price > 0.0 { (current_price - position.entry_price) / position.entry_price } else { 0.0 };
            position.unrealized_pnl = pnl_pct * position.entry_amount_sol;

            let stop_loss_price = position.entry_price * (1.0 - self.risk_config.stop_loss_pct);
            if current_price <= stop_loss_price {
                exit_events.push(ExitEvent { token_mint: mint, exit_reason: ExitReason::StopLoss, sell_amount_pct: 1.0, exit_price: current_price, expected_sol_output: position.current_sol_value, bonding_curve: Pubkey::default(), associated_bonding_curve: Pubkey::default(), timestamp: Utc::now() });
                position.status = PositionStatus::StoppedOut;
                warn!("STOP-LOSS triggered for {} at {}", mint, current_price);
                continue;
            }

            let tp1_price = position.entry_price * (1.0 + self.risk_config.take_profit_1_pct);
            if current_price >= tp1_price && !self.tp1_hit.get(&mint).copied().unwrap_or(false) {
                self.tp1_hit.insert(mint, true);
                exit_events.push(ExitEvent { token_mint: mint, exit_reason: ExitReason::TakeProfit { level: 1 }, sell_amount_pct: 0.5, exit_price: current_price, expected_sol_output: position.current_sol_value * 0.5, bonding_curve: Pubkey::default(), associated_bonding_curve: Pubkey::default(), timestamp: Utc::now() });
                position.status = PositionStatus::TakeProfit { level: 1 };
                continue;
            }

            let tp2_price = position.entry_price * (1.0 + self.risk_config.take_profit_2_pct);
            if current_price >= tp2_price && self.tp1_hit.get(&mint).copied().unwrap_or(false) {
                exit_events.push(ExitEvent { token_mint: mint, exit_reason: ExitReason::TakeProfit { level: 2 }, sell_amount_pct: 1.0, exit_price: current_price, expected_sol_output: position.current_sol_value, bonding_curve: Pubkey::default(), associated_bonding_curve: Pubkey::default(), timestamp: Utc::now() });
                position.status = PositionStatus::Closed;
                continue;
            }

            if Utc::now().signed_duration_since(position.created_at) > self.max_hold_duration {
                exit_events.push(ExitEvent { token_mint: mint, exit_reason: ExitReason::Timeout, sell_amount_pct: 1.0, exit_price: current_price, expected_sol_output: position.current_sol_value, bonding_curve: Pubkey::default(), associated_bonding_curve: Pubkey::default(), timestamp: Utc::now() });
                position.status = PositionStatus::Closed;
            }
        }
        exit_events
    }

    pub fn force_sell(&mut self, token_mint: &Pubkey) -> Option<ExitEvent> {
        let position = self.positions.get(token_mint)?;
        let event = ExitEvent { token_mint: *token_mint, exit_reason: ExitReason::ManualOverride, sell_amount_pct: 1.0, exit_price: position.current_price, expected_sol_output: position.current_sol_value, bonding_curve: Pubkey::default(), associated_bonding_curve: Pubkey::default(), timestamp: Utc::now() };
        self.untrack_position(token_mint);
        warn!("MANUAL OVERRIDE /forcesell for {}", token_mint);
        Some(event)
    }

    pub fn tracked_positions(&self) -> Vec<&Position> { self.positions.values().collect() }
    pub fn open_position_count(&self) -> usize {
        self.positions.values().filter(|p| matches!(p.status, PositionStatus::Open | PositionStatus::PartiallyClosed { .. })).count()
    }
}
