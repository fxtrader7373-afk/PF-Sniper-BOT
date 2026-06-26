//! Terminal UI — ratatui dashboard for live monitoring.
//!
//! Displays:
//! - Active positions with unrealized P&L
//! - Total realized P&L (today / week / all-time)
//! - Win rate and expectancy
//! - Recent trades from journal
//! - Bot state (running/paused, paper/live)
//! - Signal vector heatmap for latest scored entry

use chrono::Utc;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Span, Spans},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs},
    Terminal,
};
use std::io;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

use crate::core::error::SniperResult;
use crate::core::types::*;
use crate::modules::db::PnlSummary;

/// App state for the TUI
pub struct AppState {
    pub positions: Vec<Position>,
    pub pnl_summary: Option<PnlSummary>,
    pub bot_paused: bool,
    pub paper_mode: bool,
    pub uptime_seconds: u64,
    pub win_rate: f64,
    pub expectancy: f64,
    pub recent_signals: Vec<EntryScore>,
    pub active_tab: usize,
    pub tab_titles: Vec<&'static str>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            positions: Vec::new(),
            pnl_summary: None,
            bot_paused: false,
            paper_mode: true,
            uptime_seconds: 0,
            win_rate: 0.0,
            expectancy: 0.0,
            recent_signals: Vec::new(),
            active_tab: 0,
            tab_titles: vec!["Positions", "P&L", "Signals", "Journal", "Settings"],
        }
    }
}

/// Run the TUI dashboard
pub fn run_tui(state: Arc<RwLock<AppState>>) -> SniperResult<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    loop {
        terminal.draw(|f| ui(f, &state))?;

        if event::poll(std::time::Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => {
                        disable_raw_mode()?;
                        execute!(
                            terminal.backend_mut(),
                            LeaveAlternateScreen,
                            DisableMouseCapture
                        )?;
                        terminal.show_cursor()?;
                        info!("TUI exited");
                        return Ok(());
                    }
                    KeyCode::Left => {
                        let mut s = state.blocking_write();
                        if s.active_tab > 0 {
                            s.active_tab -= 1;
                        }
                    }
                    KeyCode::Right => {
                        let mut s = state.blocking_write();
                        if s.active_tab < s.tab_titles.len() - 1 {
                            s.active_tab += 1;
                        }
                    }
                    _ => {}
                }
            }
        }

        // Update uptime
        let mut s = state.blocking_write();
        s.uptime_seconds += 1;
    }
}

/// Render the UI
fn ui(f: &mut ratatui::Frame, state: &Arc<RwLock<AppState>>) {
    let s = state.blocking_read();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Length(3), // Tabs
            Constraint::Min(10),   // Main content
            Constraint::Length(3), // Footer
        ])
        .split(f.size());

    // Header
    let header_text = format!(
        "pf-sniper v0.1.0 | {} | {} | Uptime: {}s",
        if s.paper_mode { "PAPER" } else { "LIVE" },
        if s.bot_paused { "PAUSED" } else { "RUNNING" },
        s.uptime_seconds
    );
    let header = Paragraph::new(header_text)
        .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL).title("Header"));
    f.render_widget(header, chunks[0]);

    // Tabs
    let tabs = Tabs::new(
        s.tab_titles.iter().map(|t| Spans::from(Span::raw(*t)))
    )
    .select(s.active_tab)
    .style(Style::default().fg(Color::White))
    .highlight_style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )
    .block(Block::default().borders(Borders::ALL).title("Tabs"));
    f.render_widget(tabs, chunks[1]);

    // Main content (tab-specific)
    match s.active_tab {
        0 => render_positions_tab(f, chunks[2], &s),
        1 => render_pnl_tab(f, chunks[2], &s),
        2 => render_signals_tab(f, chunks[2], &s),
        3 => render_journal_tab(f, chunks[2], &s),
        4 => render_settings_tab(f, chunks[2], &s),
        _ => {}
    }

    // Footer
    let footer = Paragraph::new("←/→: switch tabs | q/Esc: quit")
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL).title("Help"));
    f.render_widget(footer, chunks[3]);
}

fn render_positions_tab(f: &mut ratatui::Frame, area: Rect, state: &AppState) {
    if state.positions.is_empty() {
        let text = Paragraph::new("No open positions")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(text, area);
        return;
    }

    let header = ["Token", "Entry $", "Current $", "P&L SOL", "P&L %", "Status"];
    let rows: Vec<Row> = state.positions.iter().map(|p| {
        let pnl_pct = if p.entry_price > 0.0 {
            (p.current_price - p.entry_price) / p.entry_price * 100.0
        } else {
            0.0
        };
        let pnl_color = if pnl_pct >= 0.0 { Color::Green } else { Color::Red };

        Row::new(vec![
            Cell::from(p.token_mint.to_string()),
            Cell::from(format!("{:.6}", p.entry_price)),
            Cell::from(format!("{:.6}", p.current_price)),
            Cell::from(Span::styled(format!("{:.4}", p.unrealized_pnl), Style::default().fg(pnl_color))),
            Cell::from(Span::styled(format!("{:+.2}%", pnl_pct), Style::default().fg(pnl_color))),
            Cell::from(format!("{:?}", p.status)),
        ])
    }).collect();

    let widths = [
        Constraint::Length(10),
        Constraint::Length(12),
        Constraint::Length(12),
        Constraint::Length(12),
        Constraint::Length(10),
        Constraint::Length(20),
    ];

    let table = Table::new(rows)
        .header(Row::new(header).style(Style::default().fg(Color::Yellow)))
        .widths(&widths)
        .block(Block::default().borders(Borders::ALL).title("Open Positions"));

    f.render_widget(table, area);
}

fn render_pnl_tab(f: &mut ratatui::Frame, area: Rect, state: &AppState) {
    let text = match &state.pnl_summary {
        Some(summary) => {
            format!(
                "Total Trades: {}\n\
                 Win Rate: {:.2}%\n\
                 Total P&L: {:.4} SOL\n\
                 Avg P&L: {:.2}%\n\
                 Best Trade: {:.4} SOL\n\
                 Worst Trade: {:.4} SOL\n\
                 Expectancy: {:.4} SOL\n\
                 Profit Factor: {:.2}",
                summary.total_trades,
                summary.win_rate() * 100.0,
                summary.total_pnl_sol,
                summary.avg_pnl_pct,
                summary.best_trade_sol,
                summary.worst_trade_sol,
                state.expectancy,
                summary.profit_factor(),
            )
        }
        None => "No P&L data available".to_string(),
    };

    let paragraph = Paragraph::new(text)
        .style(Style::default().fg(Color::White))
        .block(Block::default().borders(Borders::ALL).title("P&L Summary"));

    f.render_widget(paragraph, area);
}

fn render_signals_tab(f: &mut ratatui::Frame, area: Rect, state: &AppState) {
    if state.recent_signals.is_empty() {
        let text = Paragraph::new("No signal data yet")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(text, area);
        return;
    }

    let latest = &state.recent_signals[0];
    let text = format!(
        "Entry Score: {}/100\n\n\
         Signal Vector:\n\
         Dev Reputation: {:.1}/100\n\
         Holder Concentration: {:.2}\n\
         Gini Coefficient: {:.2}\n\
         Wash Trade Ratio: {:.2}\n\
         Bundle Penalty: {:.1}/100\n\
         Entry Timing: {:.1}/100\n\
         Liquidity: {:.1}/100\n\
         Trade Velocity: {:.1}/min\n\
         Unique Wallets: {}\n\
         Total Trades: {}",
        latest.score,
        latest.signal_vector.dev_reputation,
        latest.signal_vector.holder_concentration,
        latest.signal_vector.gini_coefficient,
        latest.signal_vector.wash_trade_ratio,
        latest.signal_vector.bundled_buy_penalty,
        latest.signal_vector.entry_timing_score,
        latest.signal_vector.liquidity_score,
        latest.signal_vector.trade_velocity,
        latest.signal_vector.unique_wallets,
        latest.signal_vector.total_trades,
    );

    let paragraph = Paragraph::new(text)
        .style(Style::default().fg(Color::White))
        .block(Block::default().borders(Borders::ALL).title("Latest Signal Vector"));

    f.render_widget(paragraph, area);
}

fn render_journal_tab(f: &mut ratatui::Frame, area: Rect, state: &AppState) {
    let text = "Journal tab — displays last N closed trades from SQLite";
    let paragraph = Paragraph::new(text)
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL).title("Trade Journal"));
    f.render_widget(paragraph, area);
}

fn render_settings_tab(f: &mut ratatui::Frame, area: Rect, state: &AppState) {
    let text = format!(
        "Mode: {}\n\
         Bot State: {}\n\
         Use /setrisk and /setfilter via Telegram to adjust parameters",
        if state.paper_mode { "PAPER" } else { "LIVE" },
        if state.bot_paused { "PAUSED" } else { "RUNNING" },
    );

    let paragraph = Paragraph::new(text)
        .style(Style::default().fg(Color::White))
        .block(Block::default().borders(Borders::ALL).title("Settings"));

    f.render_widget(paragraph, area);
}
