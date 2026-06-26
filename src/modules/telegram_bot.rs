use chrono::{DateTime, Duration, Timelike, Utc};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::config::Config;
use crate::config::RuntimeState;
use crate::core::error::SniperResult;
use crate::modules::db::{PnlSummary, TradeJournal};

#[derive(BotCommands, Clone, Debug)]
#[command(rename_rule = "lowercase")]
enum Command {
    Start,
    Status,
    Overview,
    Health,
    Reportnow,
    Balance,
    Positions,
    Pnl { period: String },
    Journal { n: usize },
    Pause,
    Resume,
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
    Logs,
    Help,
}

pub struct TelegramBot {
    bot: Bot,
    state: RuntimeState,
    journal: Arc<Mutex<TradeJournal>>,
    authorized_users: Vec<i64>,
    start_time: DateTime<Utc>,
    db_path: String,
    log_path: String,
    ws_endpoint: String,
    http_labels: Vec<String>,
    paper_mode: bool,
    latest_chat_id: Arc<RwLock<Option<ChatId>>>,
}

impl TelegramBot {
    pub fn new(config: &Config, state: RuntimeState, journal: TradeJournal) -> Self {
        Self {
            bot: Bot::new(&config.telegram.bot_token),
            state,
            journal: Arc::new(Mutex::new(journal)),
            authorized_users: config.telegram.authorized_user_ids.clone(),
            start_time: Utc::now(),
            db_path: config.database.path.clone(),
            log_path: "logs/bot.log".to_string(),
            ws_endpoint: config.rpc.ws_endpoint.clone(),
            http_labels: config.rpc.http_endpoints.keys().cloned().collect(),
            paper_mode: config.trading.paper_mode,
            latest_chat_id: Arc::new(RwLock::new(None)),
        }
    }

    fn is_auth(&self, uid: i64) -> bool {
        self.authorized_users.contains(&uid)
    }

    async fn remember_chat_id(&self, chat_id: ChatId) {
        let mut latest = self.latest_chat_id.write().await;
        *latest = Some(chat_id);
    }

    async fn send_text(&self, chat_id: ChatId, text: String) -> SniperResult<()> {
        self.bot.send_message(chat_id, text).await?;
        Ok(())
    }

    fn format_uptime(&self) -> String {
        let secs = (Utc::now() - self.start_time).num_seconds().max(0);
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        let s = secs % 60;
        format!("{}h {}m {}s", h, m, s)
    }

    fn summary_for_period(&self, label: &str, since: DateTime<Utc>) -> SniperResult<PnlSummary> {
        let journal = self.journal.lock().unwrap();
        let _ = label;
        journal.get_pnl_summary(since)
    }

    fn render_pnl(&self, label: &str, s: &PnlSummary) -> String {
        format!(
            "📈 {}\nTrades: {}\nWin rate: {:.2}%\nP&L: {:.4} SOL\nAvg P&L: {:.2}%\nBest: {:.4} SOL\nWorst: {:.4} SOL\nFees: {:.4} SOL\nTips: {:.4} SOL",
            label,
            s.total_trades,
            s.win_rate() * 100.0,
            s.total_pnl_sol,
            s.avg_pnl_pct,
            s.best_trade_sol,
            s.worst_trade_sol,
            s.total_fees_sol,
            s.total_tips_sol,
        )
    }

    fn tail_logs(&self, max_lines: usize) -> String {
        let path = Path::new(&self.log_path);
        match fs::read_to_string(path) {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                if lines.is_empty() {
                    return "📜 Log file is empty".to_string();
                }
                let start = lines.len().saturating_sub(max_lines);
                format!("📜 Last {} log lines:\n{}", lines.len() - start, lines[start..].join("\n"))
            }
            Err(_) => format!("📜 Log file not found at {}", self.log_path),
        }
    }

    fn render_overview(&self) -> String {
        let today = self.summary_for_period(
            "today",
            Utc::now().date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc(),
        );
        let week = self.summary_for_period("week", Utc::now() - Duration::weeks(1));

        let today_text = match today {
            Ok(s) => format!("Today: {} trades | {:.4} SOL", s.total_trades, s.total_pnl_sol),
            Err(e) => format!("Today: error ({})", e),
        };
        let week_text = match week {
            Ok(s) => format!("Week: {} trades | {:.4} SOL", s.total_trades, s.total_pnl_sol),
            Err(e) => format!("Week: error ({})", e),
        };

        format!(
            "🎯 pf-sniper overview\nState: {}\nMode: {}\nUptime: {}\nDB: {}\nWSS: {}\nRPCs: {}\n{}\n{}",
            if self.state.is_paused() { "PAUSED" } else { "RUNNING" },
            if self.paper_mode { "PAPER" } else { "LIVE" },
            self.format_uptime(),
            self.db_path,
            if self.ws_endpoint.is_empty() { "not set" } else { "configured" },
            if self.http_labels.is_empty() { "none".to_string() } else { self.http_labels.join(", ") },
            today_text,
            week_text,
        )
    }

    fn render_health(&self) -> String {
        let log_exists = Path::new(&self.log_path).exists();
        let db_exists = Path::new(&self.db_path).exists();
        format!(
            "🩺 Health\nBot: {}\nMode: {}\nDB file: {}\nLog file: {}\nAuthorized users: {}\nWSS: {}\nRPC count: {}",
            if self.state.is_paused() { "Paused" } else { "Active" },
            if self.paper_mode { "Paper" } else { "Live" },
            if db_exists { "present" } else { "missing" },
            if log_exists { "present" } else { "missing" },
            self.authorized_users.len(),
            if self.ws_endpoint.is_empty() { "missing" } else { "configured" },
            self.http_labels.len(),
        )
    }

    fn render_positions(&self) -> String {
        "📈 Positions\nOpen positions: 0\nTracked positions are not yet wired into Telegram view in this scaffold build.".to_string()
    }

    fn render_balance(&self) -> String {
        format!(
            "💰 Balance\nMode: {}\nWallet balance integration is not implemented in this scaffold build.",
            if self.paper_mode { "PAPER" } else { "LIVE" }
        )
    }

    async fn send_overview_report(&self, reason: &str) -> SniperResult<()> {
        let chat_id = { *self.latest_chat_id.read().await };
        if let Some(chat_id) = chat_id {
            let text = format!("⏱ {} report\n\n{}", reason, self.render_overview());
            self.send_text(chat_id, text).await?;
        }
        Ok(())
    }

    pub fn spawn_reporter(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut last_hour_sent: Option<u32> = None;
            let mut last_day_sent: Option<String> = None;

            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                let now = Utc::now();
                let hour_key = now.hour();
                let day_key = now.format("%Y-%m-%d").to_string();

                if now.minute() == 0 && last_hour_sent != Some(hour_key) {
                    if let Err(e) = self.send_overview_report("Hourly").await {
                        warn!("Hourly report failed: {}", e);
                    }
                    last_hour_sent = Some(hour_key);
                }

                if now.hour() == 0 && now.minute() == 0 && last_day_sent.as_deref() != Some(&day_key) {
                    if let Err(e) = self.send_overview_report("Daily").await {
                        warn!("Daily report failed: {}", e);
                    }
                    last_day_sent = Some(day_key);
                }
            }
        });
    }

    async fn dispatch(&self, msg: Message, cmd: Command) -> SniperResult<()> {
        let uid = msg.from().map(|u| u.id.0 as i64).unwrap_or(0);
        if !self.is_auth(uid) {
            self.bot.send_message(msg.chat.id, "⛔ Unauthorized").await?;
            return Ok(());
        }

        self.remember_chat_id(msg.chat.id).await;

        match cmd {
            Command::Start => {
                self.send_text(
                    msg.chat.id,
                    format!(
                        "🎯 pf-sniper v0.1.0\n{}\n\nUse /overview, /status, /health, /pnl, /journal, /logs",
                        self.render_overview()
                    ),
                )
                .await?;
            }
            Command::Status => {
                self.send_text(
                    msg.chat.id,
                    format!(
                        "📊 Status\nState: {}\nMode: {}\nUptime: {}\nPaused: {}\nDB: {}\nWSS: {}",
                        if self.state.is_paused() { "PAUSED" } else { "RUNNING" },
                        if self.paper_mode { "PAPER" } else { "LIVE" },
                        self.format_uptime(),
                        self.state.is_paused(),
                        self.db_path,
                        if self.ws_endpoint.is_empty() { "missing" } else { "configured" },
                    ),
                )
                .await?;
            }
            Command::Overview => {
                self.send_text(msg.chat.id, self.render_overview()).await?;
            }
            Command::Health => {
                self.send_text(msg.chat.id, self.render_health()).await?;
            }
            Command::Reportnow => {
                self.send_overview_report("Manual").await?;
                self.send_text(msg.chat.id, "✅ Manual report sent".to_string()).await?;
            }
            Command::Balance => {
                self.send_text(msg.chat.id, self.render_balance()).await?;
            }
            Command::Positions => {
                self.send_text(msg.chat.id, self.render_positions()).await?;
            }
            Command::Pnl { period } => {
                let label = period.to_lowercase();
                let since = match label.as_str() {
                    "today" => Utc::now().date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc(),
                    "week" => Utc::now() - Duration::weeks(1),
                    "month" => Utc::now() - Duration::weeks(4),
                    "all" => Utc::now() - Duration::days(3650),
                    _ => Utc::now() - Duration::days(3650),
                };
                let s = self.summary_for_period(&label, since)?;
                self.send_text(msg.chat.id, self.render_pnl(&label, &s)).await?;
            }
            Command::Journal { n } => {
                let trades = {
                    let journal = self.journal.lock().unwrap();
                    journal.get_recent_trades(n)?
                };
                let text = if trades.is_empty() {
                    "📓 Journal\nNo closed trades yet".to_string()
                } else {
                    let body = trades
                        .iter()
                        .take(n.min(10))
                        .enumerate()
                        .map(|(i, x)| {
                            format!(
                                "{}. {} | pnl {:.4} SOL | score {} | reason {:?}",
                                i + 1,
                                x.token_mint,
                                x.net_pnl_sol,
                                x.entry_score,
                                x.exit_reason
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!("📓 Recent closed trades\n{}", body)
                };
                self.send_text(msg.chat.id, text).await?;
            }
            Command::Pause => {
                self.state.pause();
                self.send_text(msg.chat.id, "⏸ Bot paused. Existing management only.".to_string())
                    .await?;
            }
            Command::Resume => {
                self.state.resume();
                self.send_text(msg.chat.id, "▶ Bot resumed. New entries allowed.".to_string())
                    .await?;
            }
            Command::Forcesell { token } => {
                warn!("/forcesell {}", token);
                self.send_text(
                    msg.chat.id,
                    format!("⚠ Force-sell requested for {}\nExecution wiring is scaffold-only right now.", token),
                )
                .await?;
            }
            Command::Setwallet { label } => {
                info!("Wallet → {}", label);
                self.send_text(
                    msg.chat.id,
                    format!("🔑 Active wallet request received: {}\nWallet switching is not fully wired yet.", label),
                )
                .await?;
            }
            Command::Listwallets => {
                self.send_text(
                    msg.chat.id,
                    "🔑 Wallet labels listing is not wired yet in this scaffold build.".to_string(),
                )
                .await?;
            }
            Command::Setrpc { label, url } => {
                info!("RPC {}→{}", label, url);
                self.send_text(
                    msg.chat.id,
                    format!("🌐 RPC update received\nLabel: {}\nURL: {}\nConfig persistence is not wired yet.", label, url),
                )
                .await?;
            }
            Command::Setwss { url } => {
                info!("WSS→{}", url);
                self.send_text(
                    msg.chat.id,
                    format!("🔌 WSS update received\n{}\nRuntime switching is not wired yet.", url),
                )
                .await?;
            }
            Command::Listrpc => {
                let body = if self.http_labels.is_empty() {
                    "No HTTP RPC endpoints configured".to_string()
                } else {
                    self.http_labels
                        .iter()
                        .map(|x| format!("• {}", x))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                self.send_text(msg.chat.id, format!("🌐 RPC endpoints\n{}", body)).await?;
            }
            Command::Setrisk { param, value } => {
                info!("Risk {}={}", param, value);
                self.send_text(
                    msg.chat.id,
                    format!("⚙ Risk update received\n{} = {}\nPersistence is not wired yet.", param, value),
                )
                .await?;
            }
            Command::Setfilter { param, value } => {
                info!("Filter {}={}", param, value);
                self.send_text(
                    msg.chat.id,
                    format!("⚙ Filter update received\n{} = {}\nPersistence is not wired yet.", param, value),
                )
                .await?;
            }
            Command::Retrain => {
                self.send_text(
                    msg.chat.id,
                    "🧠 Retraining requested\nTrigger is acknowledged, but trainer orchestration is not wired to Telegram yet.".to_string(),
                )
                .await?;
            }
            Command::Abtest { action } => {
                self.send_text(
                    msg.chat.id,
                    format!("🔬 A/B test command received\nAction: {}", action),
                )
                .await?;
            }
            Command::Logs => {
                self.send_text(msg.chat.id, self.tail_logs(20)).await?;
            }
            Command::Help => {
                self.send_text(msg.chat.id, Command::descriptions().to_string()).await?;
            }
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
