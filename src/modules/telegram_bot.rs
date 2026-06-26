use chrono::{DateTime, Duration, Utc};
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;
use tracing::{info, warn};
use crate::config::Config;
use crate::core::error::SniperResult;
use crate::config::RuntimeState;
use crate::modules::db::TradeJournal;

#[derive(BotCommands, Clone, Debug)]
#[command(rename_rule = "lowercase")]
enum Command {
    Start, Status, Balance, Positions,
    Pnl { period: String },
    Journal { n: usize },
    Pause, Resume,
    Forcesell { token: String },
    Setwallet { label: String },
    Listwallets,
    #[command(parse_with = "split")]
    Setrpc { label: String, url: String },
    Setwss { url: String },
    Listrpc,
    #[command(parse_with = "split")]
    Setrisk { param: String, value: String },
    #[command(parse_with = "split")]
    Setfilter { param: String, value: String },
    Retrain,
    Abtest { action: String },
    Logs, Help,
}

pub struct TelegramBot {
    bot: Bot,
    state: RuntimeState,
    journal: TradeJournal,
    authorized_users: Vec<i64>,
    start_time: DateTime<Utc>,
}

impl TelegramBot {
    pub fn new(config: &Config, state: RuntimeState, journal: TradeJournal) -> Self {
        Self { bot: Bot::new(&config.telegram.bot_token), state, journal,
            authorized_users: config.telegram.authorized_user_ids.clone(), start_time: Utc::now() }
    }

    fn is_auth(&self, uid: i64) -> bool { self.authorized_users.contains(&uid) }

    async fn dispatch(&self, msg: Message, cmd: Command) -> SniperResult<()> {
        let uid = msg.from().map(|u| u.id.0 as i64).unwrap_or(0);
        if !self.is_auth(uid) { self.bot.send_message(msg.chat.id, "⛔").await?; return Ok(()); }
        match cmd {
            Command::Start => { self.bot.send_message(msg.chat.id, "🎯 pf-sniper v0.1.0").await?; }
            Command::Status => { let u = (Utc::now()-self.start_time).num_seconds();
                self.bot.send_message(msg.chat.id, format!("State: {}\nUptime: {}s", if self.state.is_paused(){"PAUSED"}else{"RUNNING"}, u)).await?; }
            Command::Balance => { self.bot.send_message(msg.chat.id, "💰 Paper mode").await?; }
            Command::Positions => { self.bot.send_message(msg.chat.id, "📈 None").await?; }
            Command::Pnl { period } => {
                let since = match period.as_str() { "today"=>Utc::now().date_naive().and_hms_opt(0,0,0).unwrap().and_utc(),
                    "week"=>Utc::now()-Duration::weeks(1), "month"=>Utc::now()-Duration::weeks(4), _=>Utc::now()-Duration::days(3650) };
                let s = self.journal.get_pnl_summary(since)?;
                self.bot.send_message(msg.chat.id, format!("📈 {}:\nTrades: {}\nP&L: {:.4} SOL", period, s.total_trades, s.total_pnl_sol)).await?;
            }
            Command::Journal { n } => {
                let trades = self.journal.get_recent_trades(n)?;
                let t = if trades.is_empty() { "None".into() } else { trades.iter().take(5).map(|x| format!("{} {:.4} SOL", x.token_mint, x.net_pnl_sol)).collect::<Vec<_>>().join("\n") };
                self.bot.send_message(msg.chat.id, format!("📓\n{}", t)).await?;
            }
            Command::Pause => { self.state.pause(); self.bot.send_message(msg.chat.id, "⏸").await?; }
            Command::Resume => { self.state.resume(); self.bot.send_message(msg.chat.id, "▶").await?; }
            Command::Forcesell { token } => { warn!("/forcesell {}", token); self.bot.send_message(msg.chat.id, format!("⚠️ {}", token)).await?; }
            Command::Setwallet { label } => { info!("Wallet → {}", label); self.bot.send_message(msg.chat.id, format!("🔑 {}", label)).await?; }
            Command::Listwallets => { self.bot.send_message(msg.chat.id, "🔑 None").await?; }
            Command::Setrpc { label, url } => { info!("RPC {}→{}", label, url); self.bot.send_message(msg.chat.id, format!("🌐 {}→{}", label, url)).await?; }
            Command::Setwss { url } => { info!("WSS→{}", url); self.bot.send_message(msg.chat.id, format!("🔌 {}", url)).await?; }
            Command::Listrpc => { self.bot.send_message(msg.chat.id, "🌐 None").await?; }
            Command::Setrisk { param, value } => { info!("Risk {}={}", param, value); self.bot.send_message(msg.chat.id, format!("⚙️ {}={}", param, value)).await?; }
            Command::Setfilter { param, value } => { info!("Filter {}={}", param, value); self.bot.send_message(msg.chat.id, format!("⚙️ {}={}", param, value)).await?; }
            Command::Retrain => { self.bot.send_message(msg.chat.id, "🧠 Retraining...").await?; }
            Command::Abtest { action } => { self.bot.send_message(msg.chat.id, format!("🔬 A/B {}", action)).await?; }
            Command::Logs => { self.bot.send_message(msg.chat.id, "📜 Check logs").await?; }
            Command::Help => { self.bot.send_message(msg.chat.id, Command::descriptions().to_string()).await?; }
        }
        Ok(())
    }

    pub async fn start(self: Arc<Self>) -> SniperResult<()> {
        info!("Starting Telegram bot...");
        let bot = self.bot.clone();
        Command::repl(bot, move |_bot: Bot, msg: Message, cmd: Command| {
            let this = self.clone();
            async move {
                if let Err(e) = this.dispatch(msg, cmd).await {
                    warn!("Command dispatch failed: {}", e);
                }
                Ok(())
            }
        })
        .await;
        Ok(())
    }
}
