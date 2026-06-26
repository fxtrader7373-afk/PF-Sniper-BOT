use chrono::Utc;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

mod config;
mod core;
mod modules;
mod utils;

use config::{generate_default_config, Config, RuntimeState};
use core::error::{SniperError, SniperResult};
use core::types::*;
use modules::ab_test_harness::AbTestHarness;
use modules::adaptive_weights::AdaptiveWeights;
use modules::anomaly_alerts::AnomalyAlerts;
use modules::bundled_buy_detector::BundledBuyDetector;
use modules::db::TradeJournal;
use modules::dev_wallet_reputation::DevWalletReputation;
use modules::entry_filter::EntryFilter;
use modules::exit_manager::ExitManager;
use modules::holder_concentration_analyzer::HolderConcentrationAnalyzer;
use modules::risk_engine::RiskEngine;
use modules::rpc_provider::RpcManager;
use modules::scoring_engine::ScoringEngine;
use modules::telegram_bot::TelegramBot;
use modules::wash_trade_detector::WashTradeDetector;
use modules::ws_listener::WsListener;

pub struct AppContext {
    pub config: Config,
    pub runtime_state: RuntimeState,
    pub rpc_manager: RpcManager,
    pub ws_listener: Option<WsListener>,
    pub event_receiver: Option<tokio::sync::mpsc::Receiver<PoolCreationEvent>>,
    pub entry_filter: EntryFilter,
    pub dev_reputation: DevWalletReputation,
    pub holder_analyzer: HolderConcentrationAnalyzer,
    pub wash_detector: WashTradeDetector,
    pub bundle_detector: BundledBuyDetector,
    pub scoring_engine: ScoringEngine,
    pub adaptive_weights: AdaptiveWeights,
    pub ab_test: AbTestHarness,
    pub risk_engine: RiskEngine,
    pub exit_manager: ExitManager,
    pub journal: TradeJournal,
    pub anomaly_alerts: AnomalyAlerts,
    pub tui_state: Arc<RwLock<AppState>>,
}

pub struct AppState {
    pub positions: Vec<Position>,
    pub pnl_today: f64,
    pub pnl_week: f64,
    pub pnl_all: f64,
    pub win_rate: f64,
    pub total_trades: usize,
    pub paper_mode: bool,
    pub bot_paused: bool,
    pub uptime_secs: u64,
    pub last_signal: Option<EntryScore>,
    pub open_positions: usize,
    pub active_wallet: String,
    pub rpc_latency_ms: u64,
    pub ws_connected: bool,
    pub db_path: String,
    pub last_event: String,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            positions: Vec::new(),
            pnl_today: 0.0,
            pnl_week: 0.0,
            pnl_all: 0.0,
            win_rate: 0.0,
            total_trades: 0,
            paper_mode: true,
            bot_paused: false,
            uptime_secs: 0,
            last_signal: None,
            open_positions: 0,
            active_wallet: String::from("None"),
            rpc_latency_ms: 0,
            ws_connected: false,
            db_path: String::from("pf_sniper.db"),
            last_event: String::from("Waiting for events"),
        }
    }
}

pub struct CliArgs {
    pub config_path: PathBuf,
    pub run_tui: bool,
    pub dry_run: bool,
    pub backtest: bool,
    pub retrain: bool,
}

impl CliArgs {
    pub fn parse() -> Self {
        let args: Vec<String> = std::env::args().collect();
        let mut config_path = PathBuf::from("config.toml");
        let (mut run_tui, mut dry_run, mut backtest, mut retrain) = (false, false, false, false);
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--tui" | "-t" => run_tui = true,
                "--dry-run" | "-d" => dry_run = true,
                "--backtest" | "-b" => backtest = true,
                "--retrain" | "-r" => retrain = true,
                "--config" | "-c" => {
                    if i + 1 < args.len() {
                        config_path = PathBuf::from(&args[i + 1]);
                        i += 1;
                    }
                }
                "--help" | "-h" => {
                    println!("pf-sniper v0.1.0\nUsage: pf-sniper [--config PATH] [--dry-run] [--tui] [--backtest] [--retrain]");
                    std::process::exit(0);
                }
                other => {
                    config_path = PathBuf::from(other);
                }
            }
            i += 1;
        }
        Self {
            config_path,
            run_tui,
            dry_run,
            backtest,
            retrain,
        }
    }
}

pub async fn init_app(cli: &CliArgs) -> SniperResult<AppContext> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(true)
        .init();
    info!("=== pf-sniper v0.1.0 ===");

    let mut config = if cli.config_path.exists() {
        Config::from_file(&cli.config_path)
            .map_err(|e| SniperError::ConfigError { msg: e.to_string() })?
    } else {
        generate_default_config(&cli.config_path)
            .map_err(|e| SniperError::ConfigError { msg: e.to_string() })?;
        Config::from_file(&cli.config_path)
            .map_err(|e| SniperError::ConfigError { msg: e.to_string() })?
    };

    if cli.dry_run {
        config.trading.paper_mode = true;
        warn!("DRY-RUN MODE forced");
    }
    config
        .validate()
        .map_err(|e| SniperError::ConfigError { msg: e.to_string() })?;

    let runtime_state = RuntimeState::new(config.filters.clone(), config.risk.clone());
    let rpc_manager = RpcManager::new();
    for (label, url) in &config.rpc.http_endpoints {
        rpc_manager.add_provider(label, url).await;
    }
    if config.rpc.http_endpoints.is_empty() {
        warn!("No RPC endpoints configured");
    }

    let (ws_listener, event_receiver) = if !config.rpc.ws_endpoint.is_empty() {
        let (l, r) = WsListener::new(config.rpc.ws_endpoint.clone());
        (Some(l), Some(r))
    } else {
        (None, None)
    };

    let entry_filter = EntryFilter::new(config.filters.clone());
    let dev_reputation = DevWalletReputation::new(config.database.path.clone());
    let holder_analyzer = HolderConcentrationAnalyzer::new(config.filters.clone());
    let wash_detector = WashTradeDetector::new(config.filters.clone());
    let bundle_detector = BundledBuyDetector::new(config.filters.clone());
    let scoring_engine = ScoringEngine::default_with_filter(config.filters.clone());
    let adaptive_weights = AdaptiveWeights::new(config.database.path.clone(), 150);
    let ab_test = AbTestHarness::new(scoring_engine.get_weights().clone(), config.filters.clone());
    let risk_engine = RiskEngine::new(config.risk.clone());
    let exit_manager = ExitManager::new(config.risk.clone(), 60);
    let journal = TradeJournal::open(std::path::Path::new(&config.database.path))?;
    let anomaly_alerts = AnomalyAlerts::from_training_data(&[]);
    let tui_state = Arc::new(RwLock::new(AppState::new()));

    {
        let mut state = tui_state.write().await;
        state.paper_mode = config.trading.paper_mode;
        state.bot_paused = runtime_state.is_paused();
        state.db_path = config.database.path.clone();
        state.ws_connected = !config.rpc.ws_endpoint.is_empty();
        state.last_event = "Initialization complete".to_string();
    }

    info!("All modules initialized");
    Ok(AppContext {
        config,
        runtime_state,
        rpc_manager,
        ws_listener,
        event_receiver,
        entry_filter,
        dev_reputation,
        holder_analyzer,
        wash_detector,
        bundle_detector,
        scoring_engine,
        adaptive_weights,
        ab_test,
        risk_engine,
        exit_manager,
        journal,
        anomaly_alerts,
        tui_state,
    })
}

pub async fn run_event_loop(ctx: &mut AppContext) -> SniperResult<()> {
    info!("Starting main event loop");
    if let Some(ref ws) = ctx.ws_listener {
        ws.start().await?;
        let mut state = ctx.tui_state.write().await;
        state.ws_connected = true;
    }

    if let Some(mut receiver) = ctx.event_receiver.take() {
        loop {
            tokio::select! {
                Some(event) = receiver.recv() => { process_pool_event(ctx, event).await; }
                _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => { run_maintenance(ctx).await; }
            }
        }
    } else {
        warn!("No event receiver — idle mode");
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            run_maintenance(ctx).await;
        }
    }
}

async fn process_pool_event(ctx: &mut AppContext, event: PoolCreationEvent) {
    info!("New pool: {} ({}) slot={}", event.symbol, event.mint, event.slot);
    {
        let mut state = ctx.tui_state.write().await;
        state.last_event = format!("Pool {} ({}) slot={}", event.symbol, event.mint, event.slot);
    }
    if !ctx.entry_filter.is_eligible(&event) {
        info!("Pool {} not eligible yet", event.mint);
        return;
    }
    if ctx.runtime_state.is_paused() {
        info!("Bot paused — skipping {}", event.mint);
        return;
    }
    if let Err(e) = ctx.risk_engine.check_entry() {
        warn!("Risk rejected {}: {}", event.mint, e);
        return;
    }
    let _ = ctx.dev_reputation.lookup(&event.user).await;
    let signals = SignalVector::zero();
    let _alerts = ctx.anomaly_alerts.observe(&signals);
    let entry_score = ctx.scoring_engine.score(&signals);
    {
        let mut state = ctx.tui_state.write().await;
        state.last_signal = Some(entry_score.clone());
    }
    info!("Score for {}: {}/100", event.mint, entry_score.score);
    if entry_score.score < ctx.config.filters.min_entry_score {
        return;
    }
    let size = ctx.risk_engine.kelly_position_size(1.0);
    info!(
        "[{}] Buy {:.4} SOL of {}",
        if ctx.config.trading.paper_mode { "PAPER" } else { "LIVE" },
        size,
        event.mint
    );
}

async fn run_maintenance(ctx: &mut AppContext) {
    let today = ctx
        .journal
        .get_pnl_summary(Utc::now().date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc())
        .ok();
    let week = ctx.journal.get_pnl_summary(Utc::now() - chrono::Duration::weeks(1)).ok();
    let all = ctx.journal.get_pnl_summary(Utc::now() - chrono::Duration::days(3650)).ok();
    let rpc_health = ctx.rpc_manager.health_report().await;
    let latency = rpc_health.iter().map(|x| x.latency_ms).min().unwrap_or(0);

    let mut state = ctx.tui_state.write().await;
    state.uptime_secs += 60;
    state.paper_mode = ctx.config.trading.paper_mode;
    state.bot_paused = ctx.runtime_state.is_paused();
    state.open_positions = ctx.exit_manager.open_position_count();
    state.pnl_today = today.as_ref().map(|x| x.total_pnl_sol).unwrap_or(0.0);
    state.pnl_week = week.as_ref().map(|x| x.total_pnl_sol).unwrap_or(0.0);
    state.pnl_all = all.as_ref().map(|x| x.total_pnl_sol).unwrap_or(0.0);
    state.total_trades = all.as_ref().map(|x| x.total_trades as usize).unwrap_or(0);
    state.win_rate = all.as_ref().map(|x| x.win_rate() * 100.0).unwrap_or(0.0);
    state.rpc_latency_ms = latency;
}

pub fn run_backtest(ctx: &AppContext) -> SniperResult<()> {
    let bt = modules::backtester::Backtester::new(
        ctx.config.clone(),
        ctx.config.filters.clone(),
        ctx.scoring_engine.get_weights().clone(),
    );
    match bt.run() {
        Ok(r) => {
            println!(
                "\n=== BACKTEST ===\nTrades: {}\nWin Rate: {:.2}%\nP&L: {:.4} SOL\nSharpe: {:.3}\n==============\n",
                r.total_trades,
                r.win_rate * 100.0,
                r.total_pnl_sol,
                r.sharpe_ratio
            );
            Ok(())
        }
        Err(e) => {
            warn!("Backtest failed: {}", e);
            Err(e)
        }
    }
}

pub fn retrain_weights(ctx: &mut AppContext) -> SniperResult<()> {
    match ctx.adaptive_weights.retrain() {
        Ok(u) => {
            info!("Retrained: acc={:.2}%", u.accuracy * 100.0);
            ctx.ab_test.start_shadow_test(u.weights.clone());
            Ok(())
        }
        Err(e) => {
            warn!("Retrain failed: {}", e);
            Err(e)
        }
    }
}

async fn run_tui_loop(state: Arc<RwLock<AppState>>) -> SniperResult<()> {
    loop {
        {
            let s = state.read().await;
            print!("\x1B[2J\x1B[1;1H");
            println!("==================== pf-sniper TUI ====================");
            println!("Mode           : {}", if s.paper_mode { "PAPER" } else { "LIVE" });
            println!("State          : {}", if s.bot_paused { "PAUSED" } else { "RUNNING" });
            println!("Uptime         : {} sec", s.uptime_secs);
            println!("WS Connected   : {}", s.ws_connected);
            println!("DB             : {}", s.db_path);
            println!("RPC Latency    : {} ms", s.rpc_latency_ms);
            println!("Open Positions : {}", s.open_positions);
            println!("Total Trades   : {}", s.total_trades);
            println!("Win Rate       : {:.2}%", s.win_rate);
            println!("P&L Today      : {:.4} SOL", s.pnl_today);
            println!("P&L Week       : {:.4} SOL", s.pnl_week);
            println!("P&L All        : {:.4} SOL", s.pnl_all);
            println!("Last Event     : {}", s.last_event);
            if let Some(score) = &s.last_signal {
                println!("Last Score     : {}/100", score.score);
            } else {
                println!("Last Score     : none");
            }
            println!("=======================================================");
            println!("Press Ctrl+C to exit");
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

#[tokio::main]
async fn main() -> SniperResult<()> {
    let cli = CliArgs::parse();
    let mut ctx = init_app(&cli).await?;

    if cli.backtest {
        return run_backtest(&ctx);
    }
    if cli.retrain {
        return retrain_weights(&mut ctx);
    }

    if !ctx.config.telegram.bot_token.is_empty() {
        let telegram_journal = TradeJournal::open(std::path::Path::new(&ctx.config.database.path))?;
        let telegram_bot = Arc::new(TelegramBot::new(
            &ctx.config,
            ctx.runtime_state.clone(),
            telegram_journal,
        ));

        telegram_bot.clone().spawn_reporter();
        tokio::spawn(async move {
            if let Err(e) = telegram_bot.start().await {
                warn!("Telegram bot stopped: {}", e);
            }
        });
    } else {
        warn!("Telegram bot token missing; telegram bot not started");
    }

    if cli.run_tui {
        let tui_state = ctx.tui_state.clone();
        tokio::spawn(async move {
            if let Err(e) = run_tui_loop(tui_state).await {
                warn!("TUI loop stopped: {}", e);
            }
        });
    }

    run_event_loop(&mut ctx).await
}
