//! Risk Engine — Kelly-fractional position sizing, max concurrent positions,
//! consecutive-loss circuit breaker.
//!
//! This is the capital protection layer. Every entry request passes through
//! here before execution. The engine enforces:
//! 1. Position sizing via fractional Kelly criterion
//! 2. Maximum concurrent open positions
//! 3. Consecutive-loss circuit breaker
//! 4. Hard cap on maximum position size in lamports
//!
//! Kelly Criterion: f* = (bp - q) / b
//! where: b = win_loss_ratio, p = win_rate, q = 1-p
//! Then: position_size = f* * bankroll * kelly_fraction

use chrono::Utc;
use std::collections::HashSet;
use solana_sdk::pubkey::Pubkey;
use tracing::{info, warn};

use crate::core::error::{SniperError, SniperResult};
use crate::config::{RiskConfig, RuntimeState};

pub struct RiskEngine {
    config: RiskConfig,
    open_positions: HashSet<Pubkey>,
    consecutive_losses: usize,
    total_trades: usize,
    winning_trades: usize,
    total_won_sol: f64,
    total_lost_sol: f64,
}

impl RiskEngine {
    pub fn new(config: RiskConfig) -> Self {
        Self {
            config,
            open_positions: HashSet::new(),
            consecutive_losses: 0,
            total_trades: 0,
            winning_trades: 0,
            total_won_sol: 0.0,
            total_lost_sol: 0.0,
        }
    }

    /// Check if a new position is allowed
    pub fn check_entry(&self) -> SniperResult<()> {
        // Check concurrent position limit
        if self.open_positions.len() >= self.config.max_concurrent_positions {
            return Err(SniperError::MaxConcurrentPositions {
                max: self.config.max_concurrent_positions,
            });
        }

        // Check consecutive loss circuit breaker
        if self.consecutive_losses >= self.config.consecutive_loss_circuit_breaker {
            return Err(SniperError::CircuitBreaker {
                count: self.consecutive_losses,
            });
        }

        Ok(())
    }

    /// Compute position size using fractional Kelly criterion
    pub fn kelly_position_size(&self, bankroll_sol: f64) -> f64 {
        // Avoid division by zero or degenerate cases
        if self.total_trades < 5 {
            // Insufficient data — use fixed small fraction (25% of max position)
            return self.config.max_position_size_lamports as f64 * 0.25 / 1_000_000_000.0;
        }

        let win_rate = self.winning_trades as f64 / self.total_trades as f64;
        let loss_rate = 1.0 - win_rate;

        // Average win and loss sizes
        let avg_win = if self.winning_trades > 0 {
            self.total_won_sol / self.winning_trades as f64
        } else {
            0.0
        };
        let avg_loss = if self.total_trades > self.winning_trades {
            self.total_lost_sol / (self.total_trades - self.winning_trades) as f64
        } else {
            0.0
        };

        if avg_loss == 0.0 || avg_win == 0.0 {
            // No loss data yet — use conservative fixed size
            return self.config.max_position_size_lamports as f64 * 0.25 / 1_000_000_000.0;
        }

        let win_loss_ratio = avg_win / avg_loss;

        // Full Kelly: f* = (bp - q) / b
        let kelly_full = (win_loss_ratio * win_rate - loss_rate) / win_loss_ratio;

        // Fractional Kelly
        let kelly_fractional = kelly_full * self.config.kelly_fraction;

        // Clamp to [0, 1]
        let kelly_safe = kelly_fractional.clamp(0.0, 1.0);

        // Position size = Kelly fraction * bankroll
        let position_size = kelly_safe * bankroll_sol;

        // Hard cap at max_position_size
        let max_position_sol = self.config.max_position_size_lamports as f64 / 1_000_000_000.0;
        let position_capped = position_size.min(max_position_sol);

        info!(
            "Kelly sizing: win_rate={:.2}, avg_win={:.4}, avg_loss={:.4}, kelly={:.3}, size={:.4} SOL",
            win_rate, avg_win, avg_loss, kelly_safe, position_capped
        );

        position_capped
    }

    /// Record a winning trade
    pub fn record_win(&mut self, pnl_sol: f64) {
        self.winning_trades += 1;
        self.total_trades += 1;
        self.consecutive_losses = 0;
        self.total_won_sol += pnl_sol;
        info!("Recorded win: +{:.4} SOL (consecutive losses reset)", pnl_sol);
    }

    /// Record a losing trade
    pub fn record_loss(&mut self, pnl_sol: f64) {
        self.total_trades += 1;
        self.consecutive_losses += 1;
        self.total_lost_sol += pnl_sol.abs();
        warn!("Recorded loss: {:.4} SOL (consecutive losses: {})", pnl_sol.abs(), self.consecutive_losses);

        if self.consecutive_losses >= self.config.consecutive_loss_circuit_breaker {
            warn!(
                "CIRCUIT BREAKER TRIGGERED: {} consecutive losses reached threshold of {}",
                self.consecutive_losses,
                self.config.consecutive_loss_circuit_breaker
            );
        }
    }

    /// Open a new position (tracked for max concurrent limit)
    pub fn open_position(&mut self, token_mint: Pubkey) {
        self.open_positions.insert(token_mint);
    }

    /// Close a position (remove from tracking)
    pub fn close_position(&mut self, token_mint: Pubkey) {
        self.open_positions.remove(&token_mint);
    }

    /// Get current open position count
    pub fn open_position_count(&self) -> usize {
        self.open_positions.len()
    }

    /// Reset circuit breaker (manual override for emergencies)
    pub fn reset_circuit_breaker(&mut self) {
        self.consecutive_losses = 0;
        info!("Circuit breaker manually reset");
    }
}
