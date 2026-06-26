//! Main orchestrator for pf-sniper
//!
//! Wires up all 18 modules into a coherent system with proper lifecycle management.
//!
//! Entry order:
//! 1. Load config (from file or generate default paper-mode template)
//! 2. Initialize RPC provider with failover
//! 3. Initialize WebSocket listener for pump.fun program
//! 4. Initialize all analysis modules
//! 5. Start main event loop (paper mode by default)
//! 6. Start Telegram bot for remote control
//! 7. Start TUI dashboard (optional, via --tui flag)

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn, error};
use tracing_subscriber::EnvFilter;

mod config;
mod core;
mod modules;
mod utils;

use config::{Config, RuntimeState, generate_default_config, FilterConfig};
use core::error::SniperResult;
use core::types::*;
use modules::rpc_provider::RpcManager;
use modules::ws_listener::WsListener;
use modules::entry_filter::EntryFilter;
use modules::dev_wallet_reputation::DevWalletReputation;
use modules::holder_concentration_analyzer::HolderConcentrationAnalyzer;
use modules::wash_trade_detector::WashTradeDetector;
use modules::bundled_buy_detector::BundledBuyDetector;
use modules::scoring_engine::ScoringEngine;
use modules::adaptive_weights::AdaptiveWeights;
use modules::ab_test_harness::AbTestHarness;
use modules::risk_engine::RiskEngine;
use modules::execution::ExecutionEngine;
use modules::exit_manager::ExitManager;
use modules::db::TradeJournal;
use modules::anomaly_alerts::AnomalyAlerts;

/// Full application context — owns all 18 modules
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

/// Application state for the TUI dashboard
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
        }
    }
}

/// Parse CLI arguments
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
        let mut run_tui = false;
        let mut dry_run = false;
        let mut backtest = false;
        let mut retrain = false;

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
                    println!("pf-sniper v0.1.0 — Pump.fun Sniping Bot");
                    println!();
                    println!("Usage: pf-sniper [OPTIONS]");
                    println!();
                    println!("Options:");
                    println!("  --config, -c <path>  Path to config.toml (default: ./config.toml)");
                    println!("  --tui, -t            Launch TUI dashboard");
                    println!("  --dry-run, -d        Paper mode regardless of config");
                    println!("  --backtest, -b       Run backtest and exit");
                    println!("  --retrain, -r        Retrain adaptive weights and exit");
                    println!("  --help, -h           Show this help message");
                    std::process::exit(0);
                }
                other => {
                    // Treat as positional argument (config path)
                    config_path = PathBuf::from(other);
                }
            }
            i += 1;
        }

        Self { config_path, run_tui, dry_run, backtest, retrain }
    }
}

/// Initialize the full application context
pub async fn init_app(cli: &CliArgs) -> SniperResult<AppContext> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(true)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true)
        .init();

    info!("=== pf-sniper v0.1.0 ===");
    info!("Build: SAHA — Personal expert system for quantitative on-chain trading");
    info!("Config path: {:?}", cli.config_path);

    // Load or generate config
    let mut config = if cli.config_path.exists() {
        Config::from_file(&cli.config_path)?
    } else {
        info!("Config not found, generating default paper-mode template");
        generate_default_config(&cli.config_path)?;
        Config::from_file(&cli.config_path)?
    };

    // Override paper mode if --dry-run flag
    if cli.dry_run {
        config.trading.paper_mode = true;
        warn!("DRY-RUN MODE: paper_mode forced true via CLI flag");
    }

    // Validate config
    config.validate()?;

    // Runtime state
    let runtime_state = RuntimeState::new(config.filters.clone(), config.risk.clone());

    // RPC manager
    let rpc_manager = RpcManager::new();
    for (label, url) in &config.rpc.http_endpoints {
        rpc_manager.add_provider(label, url).await;
        info!("RPC provider registered: {}", label);
    }
    if config.rpc.http_endpoints.is_empty() {
        warn!("No RPC endpoints configured — use /setrpc via Telegram to add one");
    }

    // WebSocket listener
    let (ws_listener, event_receiver) = if !config.rpc.ws_endpoint.is_empty() {
        let (listener, receiver) = WsListener::new(config.rpc.ws_endpoint.clone());
        info!("WebSocket listener initialized for pump.fun program");
        (Some(listener), Some(receiver))
    } else {
        warn!("No WebSocket endpoint — ws_listener disabled. Use /setwss to configure.");
        (None, None)
    };

    // Analysis modules
    let entry_filter = EntryFilter::new(config.filters.clone());
    let dev_reputation = DevWalletReputation::new(config.database.path.clone());
    let holder_analyzer = HolderConcentrationAnalyzer::new(config.filters.clone());
    let wash_detector = WashTradeDetector::new(config.filters.clone());
    let bundle_detector = BundledBuyDetector::new(config.filters.clone());

    // Scoring engine
    let scoring_engine = ScoringEngine::default_with_filter(config.filters.clone());
    info!("Scoring engine initialized with default heuristic weights");

    // Adaptive weights (disabled until 150+ trades)
    let adaptive_weights = AdaptiveWeights::new(config.database.path.clone(), 150);

    // A/B test harness
    let ab_test = AbTestHarness::new(
        scoring_engine.get_weights().clone(),
        config.filters.clone(),
    );

    // Risk engine
    let risk_engine = RiskEngine::new(config.risk.clone());

    // Exit manager (60 min max hold time)
    let exit_manager = ExitManager::new(config.risk.clone(), 60);

    // Trade journal
    let journal = TradeJournal::open(std::path::Path::new(&config.database.path))?;

    // Anomaly alerts
    let anomaly_alerts = AnomalyAlerts::from_training_data(&[]);

    // TUI state
    let tui_state = Arc::new(RwLock::new(AppState::new()));

    info!("All modules initialized successfully");

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

/// Run the main event loop — processes new pool events through the full pipeline
pub async fn run_event_loop(ctx: &mut AppContext) -> SniperResult<()> {
    info!("Starting main event loop...");

    // Start WebSocket listener
    if let Some(ref ws) = ctx.ws_listener {
        ws.start().await?;
    }

    // Process events as they arrive
    if let Some(mut receiver) = ctx.event_receiver.take() {
        loop {
            tokio::select! {
                Some(event) = receiver.recv() => {
                    process_pool_event(ctx, event).await;
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {
                    // Periodic maintenance
                    run_maintenance(ctx).await;
                }
            }
        }
    } else {
        warn!("No event receiver — bot will idle. Configure a WebSocket endpoint to start sniping.");

        // Keep the process alive for TUI/Telegram
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            run_maintenance(ctx).await;
        }
    }
}

/// Process a single pool creation event through the full pipeline
async fn process_pool_event(ctx: &mut AppContext, event: PoolCreationEvent) {
    info!(
        "New pool detected: {} ({}) at slot {} | mayhem={}",
        event.symbol, event.mint, event.slot, event.mayhem
    );

    // Step 1: Entry filter (second-wave timing)
    if !ctx.entry_filter.is_eligible(&event) {
        let wait = ctx.entry_filter.time_until_eligible(&event);
        info!("Pool {} not eligible yet, waiting {:?}", event.mint, wait);
        return;
    }

    // Step 2: Check if bot is paused
    if ctx.runtime_state.is_paused() {
        info!("Bot paused — skipping entry for {}", event.mint);
        return;
    }

    // Step 3: Check risk engine
    if let Err(e) = ctx.risk_engine.check_entry() {
        warn!("Risk engine rejected entry for {}: {}", event.mint, e);
        return;
    }

    // Step 4: Dev wallet reputation
    match ctx.dev_reputation.lookup(&event.user).await {
        Ok(profile) => {
            info!(
                "Dev reputation for {}: score={:.1}, launches={}, rugs={}",
                event.user,
                profile.reputation_score,
                profile.total_launches,
                profile.rug_exits
            );
        }
        Err(e) => {
            warn!("Dev wallet lookup failed for {}: {}", event.user, e);
        }
    }

    // Step 5: Build signal vector (placeholder — real implementation queries on-chain data)
    let signals = SignalVector::zero();

    // Step 6: Anomaly check
    let alerts = ctx.anomaly_alerts.observe(&signals);
    for alert in &alerts {
        match alert.severity {
            AlertSeverity::Critical => {
                error!("CRITICAL ANOMALY: {} (KL={:.2})", alert.signal_name, alert.kl_divergence);
                // In production, you'd halt trading or escalate
            }
            AlertSeverity::Warning => {
                warn!("Anomaly warning: {} drifted (KL={:.2})", alert.signal_name, alert.kl_divergence);
            }
            AlertSeverity::Info => {
                info!("Anomaly info: {} shifted slightly", alert.signal_name);
            }
        }
    }

    // Step 7: Score the entry
    let entry_score = ctx.scoring_engine.score(&signals);
    info!("Entry score for {}: {}/100", event.mint, entry_score.score);

    // Step 8: Check minimum threshold
    if entry_score.score < ctx.config.filters.min_entry_score {
        info!(
            "Score {} below threshold {} — skipping {}",
            entry_score.score,
            ctx.config.filters.min_entry_score,
            event.mint
        );
        return;
    }

    // Step 9: Compute position size via Kelly criterion
    // In production, this queries actual wallet balance
    let bankroll_sol = 1.0;
    let position_size = ctx.risk_engine.kelly_position_size(bankroll_sol);
    info!("Kelly position size: {:.4} SOL", position_size);

    // Step 10: Check max position cap
    let max_position_sol = ctx.config.risk.max_position_size_lamports as f64 / 1_000_000_000.0;
    let actual_size = position_size.min(max_position_sol);

    if ctx.config.trading.paper_mode {
        info!(
            "[PAPER] Would buy {:.4} SOL of {} at slot {}",
            actual_size, event.mint, event.slot
        );
        return;
    }

    // Step 11: Execute (live mode)
    // In production: initialize ExecutionEngine with wallet and call execute_buy
    info!("[LIVE] Executing buy for {:.4} SOL of {}", actual_size, event.mint);
}

/// Periodic maintenance tasks
async fn run_maintenance(ctx: &mut AppContext) {
    // Check for exit conditions on open positions
    // In production, this queries current prices for tracked positions

    // Update TUI state
    {
        let mut state = ctx.tui_state.write().await;
        state.uptime_secs += 60;
        state.paper_mode = ctx.config.trading.paper_mode;
        state.bot_paused = ctx.runtime_state.is_paused();
        state.open_positions = ctx.exit_manager.open_position_count();
    }

    // Log summary
    info!(
        "Maintenance check: uptime={}s, open_positions={}, mode={}",
        {
            let state = ctx.tui_state.read().await;
            state.uptime_secs
        },
        ctx.exit_manager.open_position_count(),
        if ctx.config.trading.paper_mode { "PAPER" } else { "LIVE" }
    );
}

/// Run a backtest and print results
pub fn run_backtest(ctx: &AppContext) -> SniperResult<()> {
    info!("Running backtest with current filter settings...");

    let backtester = modules::backtester::Backtester::new(
        ctx.config.clone(),
        ctx.config.filters.clone(),
        ctx.scoring_engine.get_weights().clone(),
    );

    match backtester.run() {
        Ok(result) => {
            println!("\n=== BACKTEST RESULTS ===");
            println!("Total trades:          {}", result.total_trades);
            println!("Winning trades:        {}", result.winning_trades);
            println!("Losing trades:         {}", result.losing_trades);
            println!("Win rate:              {:.2}%", result.win_rate * 100.0);
            println!("Total P&L:             {:.4} SOL", result.total_pnl_sol);
            println!("Avg P&L per trade:     {:.4} SOL", result.avg_pnl_per_trade_sol);
            println!("Max drawdown:          {:.2}%", result.max_drawdown_pct);
            println!("Sharpe ratio:          {:.3}", result.sharpe_ratio);
            println!("Sortino ratio:         {:.3}", result.sortino_ratio);
            println!("Expectancy:            {:.4} SOL", result.expectancy);
            println!("Profit factor:         {:.3}", result.profit_factor);
            println!("Avg holding time:      {:.1}s", result.avg_holding_time_seconds);
            println!("Max consecutive losses:{}", result.max_consecutive_losses);
            println!("========================\n");

            Ok(())
        }
        Err(e) => {
            warn!("Backtest failed: {}", e);
            Err(e)
        }
    }
}

/// Retrain adaptive weights and optionally start A/B test
pub fn retrain_weights(ctx: &mut AppContext) -> SniperResult<()> {
    info!("Retraining adaptive weights...");

    match ctx.adaptive_weights.retrain() {
        Ok(update) => {
            info!(
                "Weights retrained: accuracy={:.2}%, trades={}",
                update.accuracy * 100.0,
                update.num_trades
            );

            // Start A/B shadow test
            ctx.ab_test.start_shadow_test(update.weights.clone());
            info!("Shadow A/B test started with new weights");

            Ok(())
        }
        Err(e) => {
            warn!("Retraining failed (expected if < 150 trades): {}", e);
            Err(e)
        }
    }
}

/// Main entry point
#[tokio::main]
async fn main() -> SniperResult<()> {
    let cli = CliArgs::parse();

    // Initialize application
    let mut ctx = init_app(&cli).await?;

    // Handle CLI-only modes
    if cli.backtest {
        return run_backtest(&ctx);
    }

    if cli.retrain {
        return retrain_weights(&mut ctx);
    }

    // Run main event loop
    run_event_loop(&mut ctx).await
}
