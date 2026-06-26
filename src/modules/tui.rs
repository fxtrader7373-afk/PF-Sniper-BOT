use crossterm::{event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode}, execute, terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen}};
use ratatui::{backend::CrosstermBackend, layout::{Constraint, Direction, Layout, Rect}, style::{Color, Modifier, Style}, text::{Span, Line}, widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs}, Terminal};
use std::io;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;
use crate::core::error::SniperResult;
use crate::core::types::{EntryScore, Position};
use crate::modules::db::PnlSummary;

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
        Self { positions: Vec::new(), pnl_summary: None, bot_paused: false, paper_mode: true,
            uptime_seconds: 0, win_rate: 0.0, expectancy: 0.0, recent_signals: Vec::new(),
            active_tab: 0, tab_titles: vec!["Positions", "P&L", "Signals", "Journal", "Settings"] }
    }
}

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
                        execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
                        terminal.show_cursor()?;
                        info!("TUI exited");
                        return Ok(());
                    }
                    KeyCode::Left => { let mut s = state.blocking_write(); if s.active_tab > 0 { s.active_tab -= 1; } }
                    KeyCode::Right => { let mut s = state.blocking_write(); if s.active_tab < s.tab_titles.len()-1 { s.active_tab += 1; } }
                    _ => {}
                }
            }
        }
        let mut s = state.blocking_write(); s.uptime_seconds += 1;
    }
}

fn ui(f: &mut ratatui::Frame, state: &Arc<RwLock<AppState>>) {
    let s = state.blocking_read();
    let chunks = Layout::default().direction(Direction::Vertical).margin(1)
        .constraints([Constraint::Length(3), Constraint::Length(3), Constraint::Min(10), Constraint::Length(3)]).split(f.size());
    let header = Paragraph::new(format!("pf-sniper | {} | {} | {}s",
        if s.paper_mode {"PAPER"} else {"LIVE"}, if s.bot_paused {"PAUSED"} else {"RUNNING"}, s.uptime_seconds))
        .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)).block(Block::default().borders(Borders::ALL).title("Header"));
    f.render_widget(header, chunks[0]);
    let tabs = Tabs::new(s.tab_titles.iter().map(|t| Line::from(Span::raw(*t))))
        .select(s.active_tab).style(Style::default().fg(Color::White))
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL).title("Tabs"));
    f.render_widget(tabs, chunks[1]);
    match s.active_tab {
        0 => render_pos(f, chunks[2], &s), 1 => render_pnl(f, chunks[2], &s),
        2 => render_sig(f, chunks[2], &s), _ => render_set(f, chunks[2], &s),
    }
    f.render_widget(Paragraph::new("q: quit | ←→: tabs").style(Style::default().fg(Color::DarkGray)).block(Block::default().borders(Borders::ALL).title("Help")), chunks[3]);
}

fn render_pos(f: &mut ratatui::Frame, area: Rect, state: &AppState) {
    if state.positions.is_empty() { f.render_widget(Paragraph::new("No positions").style(Style::default().fg(Color::DarkGray)), area); return; }
    let rows: Vec<Row> = state.positions.iter().map(|p| {
        let pct = if p.entry_price>0.0 {(p.current_price-p.entry_price)/p.entry_price*100.0} else {0.0};
        let c = if pct>=0.0 {Color::Green} else {Color::Red};
        Row::new(vec![Cell::from(p.token_mint.to_string()), Cell::from(format!("{:.6}",p.entry_price)),
            Cell::from(format!("{:.6}",p.current_price)),
            Cell::from(Span::styled(format!("{:.4}",p.unrealized_pnl), Style::default().fg(c))),
            Cell::from(Span::styled(format!("{:+.1}%",pct), Style::default().fg(c))),
            Cell::from(format!("{:?}",p.status))])
    }).collect();
    f.render_widget(Table::new(rows, [Constraint::Length(10),Constraint::Length(10),Constraint::Length(10),Constraint::Length(10),Constraint::Length(8),Constraint::Length(15)])
        .header(Row::new(["Token","Entry","Now","P&L SOL","P&L%","Status"]).style(Style::default().fg(Color::Yellow)))
        .block(Block::default().borders(Borders::ALL).title("Positions")), area);
}
fn render_pnl(f: &mut ratatui::Frame, area: Rect, state: &AppState) {
    let t = match &state.pnl_summary { Some(s) => format!("Trades: {}\nP&L: {:.4} SOL\nWin: {:.1}%", s.total_trades, s.total_pnl_sol, s.win_rate()*100.0), None => "No data".into() };
    f.render_widget(Paragraph::new(t).block(Block::default().borders(Borders::ALL).title("P&L")), area);
}
fn render_sig(f: &mut ratatui::Frame, area: Rect, state: &AppState) {
    let t = if let Some(l) = state.recent_signals.first() { format!("Score: {}/100\nGini: {:.2}", l.score, l.signal_vector.gini_coefficient) } else { "No signals".into() };
    f.render_widget(Paragraph::new(t).block(Block::default().borders(Borders::ALL).title("Signals")), area);
}
fn render_set(f: &mut ratatui::Frame, area: Rect, state: &AppState) {
    f.render_widget(Paragraph::new(format!("Mode: {}\nPaused: {}", if state.paper_mode {"PAPER"} else {"LIVE"}, state.bot_paused)).block(Block::default().borders(Borders::ALL).title("Settings")), area);
}
