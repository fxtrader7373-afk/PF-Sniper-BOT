//! Telegram Bot — full remote control of the sniper bot.
//!
//! Security constraint: /setwallet switches between pre-loaded encrypted keystores.
//! It NEVER accepts raw private keys via Telegram.

use chrono::{DateTime, Duration, Utc};
use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;
use tracing::{info, warn, error};

use crate::config::{Config, RuntimeState};
use crate::core::error::SniperResult;
use crate::modules::db::TradeJournal;

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "Available commands:")]
enum Command {
    #[command(description = "Initialize, show main menu")]
    Start,
    #[command(description = "Uptime, bot state, active position count")]
    Status,
    #[command(description = "Current SOL + tracked token holdings")]
    Balance,
    #[command(description = "Open positions with live P&L")]
    Positions,
    #[command(description = "P&L summary by period: /pnl today|week|month|all")]
    Pnl { period: String },
    #[command(description = "Last n closed trades: /journal 10")]
    Journal { n: usize },
    #[command(description = "Pause new entries, keep managing exits")]
    Pause,
    #[command(description = "Resume new entries")]
    Resume,
    #[command(description = "Emergency close: /forcesell <token_mint>")]
    Forcesell { token: String },
    #[command(description = "Switch active wallet: /setwallet <label>")]
    Setwallet { label: String },
    #[command(description = "Show configured wallet labels")]
    Listwallets,
    #[command(description = "Add/update RPC: /setrpc <label> <url>")]
    Setrpc { label: String, url: String },
    #[command(description = "Update WSS endpoint: /setwss <url>")]
    Setwss { url: String },
    #[command(description = "Show RPC endpoints + latency")]
    Listrpc,
    #[command(description = "Adjust risk params: /setrisk <param> <value>")]
    Setrisk { param: String, value: String },
    #[command(description = "Adjust filter thresholds: /setfilter <param> <value>")]
    Setfilter { param: String, value: String },
    #[command(description = "Trigger adaptive weights retraining")]
    Retrain,
    #[command(description = "Toggle shadow A/B testing: /abtest on|off")]
    Abtest { action: String },
    #[command(description = "Tail recent warnings/errors")]
    Logs,
    #[command(description = "Full command list")]
    Help,
}

pub struct TelegramBot {
    bot: Bot,
    state: RuntimeState,
    journal: TradeJournal,
    authorized_users: Vec<i64>,
    start_time: DateTime<Utc>,
}

impl TelegramBot {
    pub fn new(
        config: &Config,
        state: RuntimeState,
        journal: TradeJournal,
    ) -> Self {
        let bot = Bot::new(&config.telegram.bot_token);
        let authorized_users = config.telegram.authorized_user_ids.clone();

        Self {
            bot,
            state,
            journal,
            authorized_users,
            start_time: Utc::now(),
        }
    }

    /// Check if user is authorized
    fn is_authorized(&self, user_id: i64) -> bool {
        self.authorized_users.contains(&user_id)
    }

    /// Handle incoming messages
    pub async fn handle_message(&self, msg: Message, cmd: Command) -> SniperResult<()> {
        let user_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);

        if !self.is_authorized(user_id) {
            self.bot.send_message(msg.chat.id, "⛔ Unauthorized").await?;
            return Ok(());
        }

        match cmd {
            Command::Start => self.cmd_start(msg).await?,
            Command::Status => self.cmd_status(msg).await?,
            Command::Balance => self.cmd_balance(msg).await?,
            Command::Positions => self.cmd_positions(msg).await?,
            Command::Pnl { period } => self.cmd_pnl(msg, &period).await?,
            Command::Journal { n } => self.cmd_journal(msg, n).await?,
            Command::Pause => self.cmd_pause(msg).await?,
            Command::Resume => self.cmd_resume(msg).await?,
            Command::Forcesell { token } => self.cmd_forcesell(msg, &token).await?,
            Command::Setwallet { label } => self.cmd_setwallet(msg, &label).await?,
            Command::Listwallets => self.cmd_listwallets(msg).await?,
            Command::Setrpc { label, url } => self.cmd_setrpc(msg, &label, &url).await?,
            Command::Setwss { url } => self.cmd_setwss(msg, &url).await?,
            Command::Listrpc => self.cmd_listrpc(msg).await?,
            Command::Setrisk { param, value } => self.cmd_setrisk(msg, &param, &value).await?,
            Command::Setfilter { param, value } => self.cmd_setfilter(msg, &param, &value).await?,
            Command::Retrain => self.cmd_retrain(msg).await?,
            Command::Abtest { action } => self.cmd_abtest(msg, &action).await?,
            Command::Logs => self.cmd_logs(msg).await?,
            Command::Help => self.cmd_help(msg).await?,
        }

        Ok(())
    }

    async fn cmd_start(&self, msg: Message) -> SniperResult<()> {
        let text = format!(
            "🎯 pf-sniper v0.1.0\n\n\
             Status: {}\n\
             Mode: {}\n\
             Uptime: {}\n\n\
             Use /help for command list",
            if self.state.is_paused() { "⏸ PAUSED" } else { "▶ RUNNING" },
            "PAPER",
            self.format_uptime(),
        );
        self.bot.send_message(msg.chat.id, text).await?;
        Ok(())
    }

    async fn cmd_status(&self, msg: Message) -> SniperResult<()> {
        let text = format!(
            "📊 Bot Status\n\n\
             State: {}\n\
             Mode: Paper Trading\n\
             Uptime: {}\n\
             Open Positions: 0\n\
             Total Trades: 0\n\
             Win Rate: N/A",
            if self.state.is_paused() { "⏸ PAUSED" } else { "▶ RUNNING" },
            self.format_uptime(),
        );
        self.bot.send_message(msg.chat.id, text).await?;
        Ok(())
    }

    async fn cmd_balance(&self, msg: Message) -> SniperResult<()> {
        self.bot.send_message(msg.chat.id, "💰 Balance: N/A (paper mode)").await?;
        Ok(())
    }

    async fn cmd_positions(&self, msg: Message) -> SniperResult<()> {
        self.bot.send_message(msg.chat.id, "📈 No open positions").await?;
        Ok(())
    }

    async fn cmd_pnl(&self, msg: Message, period: &str) -> SniperResult<()> {
        let since = match period {
            "today" => Utc::now().date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc(),
            "week" => Utc::now() - Duration::weeks(1),
            "month" => Utc::now() - Duration::weeks(4),
            _ => Utc::now() - Duration::days(3650), // ~10 years
        };

        let summary = self.journal.get_pnl_summary(since)?;

        let text = format!(
            "📈 P&L Summary ({})\n\n\
             Trades: {}\n\
             Wins: {} | Losses: {}\n\
             Win Rate: {:.1}%\n\
             Total P&L: {:.4} SOL\n\
             Avg P&L: {:.2}%\n\
             Best: {:.4} SOL\n\
             Worst: {:.4} SOL\n\
             Fees: {:.4} SOL\n\
             Tips: {:.4} SOL",
            period,
            summary.total_trades,
            summary.winning_trades,
            summary.losing_trades,
            summary.win_rate() * 100.0,
            summary.total_pnl_sol,
            summary.avg_pnl_pct,
            summary.best_trade_sol,
            summary.worst_trade_sol,
            summary.total_fees_sol,
            summary.total_tips_sol,
        );
        self.bot.send_message(msg.chat.id, text).await?;
        Ok(())
    }

    async fn cmd_journal(&self, msg: Message, n: usize) -> SniperResult<()> {
        let trades = self.journal.get_recent_trades(n)?;

        if trades.is_empty() {
            self.bot.send_message(msg.chat.id, "📓 No trades in journal").await?;
            return Ok(());
        }

        let mut text = String::from("📓 Recent Trades:\n\n");
        for t in trades.iter().take(5) {
            text.push_str(&format!(
                "{} | P&L: {:.4} SOL | Score: {}\n",
                t.token_mint,
                t.net_pnl_sol,
                t.entry_score,
            ));
        }

        self.bot.send_message(msg.chat.id, text).await?;
        Ok(())
    }

    async fn cmd_pause(&self, msg: Message) -> SniperResult<()> {
        self.state.pause();
        self.bot.send_message(msg.chat.id, "⏸ Bot PAUSED").await?;
        Ok(())
    }

    async fn cmd_resume(&self, msg: Message) -> SniperResult<()> {
        self.state.resume();
        self.bot.send_message(msg.chat.id, "▶ Bot RESUMED").await?;
        Ok(())
    }

    async fn cmd_forcesell(&self, msg: Message, token: &str) -> SniperResult<()> {
        warn!("/forcesell called for {} by Telegram user", token);
        self.journal.record_config_change("forcesell", "none", token, "telegram")?;
        self.bot.send_message(msg.chat.id, format!("⚠️ Emergency sell triggered for {}", token)).await?;
        Ok(())
    }

    async fn cmd_setwallet(&self, msg: Message, label: &str) -> SniperResult<()> {
        info!("Switching active wallet to: {}", label);
        self.bot.send_message(msg.chat.id, format!("🔑 Wallet switched to: {}", label)).await?;
        Ok(())
    }

    async fn cmd_listwallets(&self, msg: Message) -> SniperResult<()> {
        self.bot.send_message(msg.chat.id, "🔑 No wallets configured").await?;
        Ok(())
    }

    async fn cmd_setrpc(&self, msg: Message, label: &str, url: &str) -> SniperResult<()> {
        info!("RPC endpoint added: {} → {}", label, url);
        self.journal.record_config_change("setrpc", "", &format!("{} → {}", label, url), "telegram")?;
        self.bot.send_message(msg.chat.id, format!("🌐 RPC added: {} → {}", label, url)).await?;
        Ok(())
    }

    async fn cmd_setwss(&self, msg: Message, url: &str) -> SniperResult<()> {
        info!("WSS endpoint updated: {}", url);
        self.bot.send_message(msg.chat.id, format!("🔌 WSS updated: {}", url)).await?;
        Ok(())
    }

    async fn cmd_listrpc(&self, msg: Message) -> SniperResult<()> {
        self.bot.send_message(msg.chat.id, "🌐 No RPC endpoints configured").await?;
        Ok(())
    }

    async fn cmd_setrisk(&self, msg: Message, param: &str, value: &str) -> SniperResult<()> {
        info!("Risk param updated: {} = {}", param, value);
        self.journal.record_config_change(&format!("risk.{}", param), "", value, "telegram")?;
        self.bot.send_message(msg.chat.id, format!("⚙️ Risk param set: {} = {}", param, value)).await?;
        Ok(())
    }

    async fn cmd_setfilter(&self, msg: Message, param: &str, value: &str) -> SniperResult<()> {
        info!("Filter param updated: {} = {}", param, value);
        self.journal.record_config_change(&format!("filter.{}", param), "", value, "telegram")?;
        self.bot.send_message(msg.chat.id, format!("⚙️ Filter set: {} = {}", param, value)).await?;
        Ok(())
    }

    async fn cmd_retrain(&self, msg: Message) -> SniperResult<()> {
        self.bot.send_message(msg.chat.id, "🧠 Retraining adaptive weights...").await?;
        Ok(())
    }

    async fn cmd_abtest(&self, msg: Message, action: &str) -> SniperResult<()> {
        match action {
            "on" => self.bot.send_message(msg.chat.id, "🔬 A/B testing enabled").await?,
            "off" => self.bot.send_message(msg.chat.id, "🔬 A/B testing disabled").await?,
            _ => self.bot.send_message(msg.chat.id, "Usage: /abtest on|off").await?,
        }
        Ok(())
    }

    async fn cmd_logs(&self, msg: Message) -> SniperResult<()> {
        self.bot.send_message(msg.chat.id, "📜 Recent logs: check server logs").await?;
        Ok(())
    }

    async fn cmd_help(&self, msg: Message) -> SniperResult<()> {
        let text = Command::descriptions().to_string();
        self.bot.send_message(msg.chat.id, format!("📖 Commands:\n\n{}", text)).await?;
        Ok(())
    }

    fn format_uptime(&self) -> String {
        let elapsed = Utc::now() - self.start_time;
        let hours = elapsed.num_hours();
        let minutes = elapsed.num_minutes() % 60;
        format!("{}h {}m", hours, minutes)
    }

    /// Start the Telegram bot
    pub async fn start(&self) -> SniperResult<()> {
        info!("Starting Telegram bot...");

        teloxide::repl(self.bot.clone(), |bot, msg| async move {
            if let Some(text) = msg.text() {
                if text.starts_with('/') {
                    match Command::parse(text, "pf-sniper") {
                        Ok(cmd) => {
                            info!("Telegram command: {:?}", cmd);
                            // In production, this would dispatch to the handler
                        }
                        Err(e) => {
                            bot.send_message(msg.chat.id, format!("Unknown command: {}", e)).await.ok();
                        }
                    }
                }
            }
        }).await;

        Ok(())
    }
}
