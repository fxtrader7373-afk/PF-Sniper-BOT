# pf-sniper — Pump.fun Sniping Bot

**Built by SAHA** — Personal expert system for quantitative on-chain trading.

---

## Architecture Overview

```
ws_listener (logsSubscribe)
    │
    ▼
entry_filter (second-wave timing)
    │
    ├── dev_wallet_reputation ─┐
    ├── holder_concentration ───┤
    ├── wash_trade_detector ───┼─► scoring_engine (0-100 score)
    ├── bundled_buy_detector ──┤                      │
    └──────────────────────────┘                      ▼
                                              risk_engine (Kelly sizing)
                                                  │
                                                  ▼
                                          execution (Jito bundle → direct RPC)
                                                  │
                                                  ▼
                                          exit_manager (SL/TP ladder)
                                                  │
                                                  ▼
                                          db (trade journal)
                                                  │
                                    ┌─────────────┼──────────────┐
                                    ▼             ▼              ▼
                             adaptive_weights  telegram_bot      tui
                             ab_test_harness   backtester    anomaly_alerts
```

## 18 Core Modules

| # | Module | Function |
|---|--------|----------|
| 1 | `rpc_provider` | Trait-based RPC abstraction with auto-failover on HTTP 429 |
| 2 | `ws_listener` | logsSubscribe on pump.fun program ID for real-time new-pool detection |
| 3 | `entry_filter` | Second-wave timing logic — skips unwinnable first-2-block bot races |
| 4 | `dev_wallet_reputation` | Cross-pool history lookup: has this creator rugged before? |
| 5 | `holder_concentration_analyzer` | Top-10 holder concentration + Gini coefficient calculation |
| 6 | `wash_trade_detector` | Unique-wallet vs trade-count ratio to detect wash trading |
| 7 | `bundled_buy_detector` | Same-slot wallet clustering detection |
| 8 | `scoring_engine` | Composite 0-100 entry score from weighted signal vector |
| 9 | `adaptive_weights` | Logistic regression retrained from real trade outcomes |
| 10 | `ab_test_harness` | Shadow-mode testing of new weights before live promotion |
| 11 | `risk_engine` | Kelly-fractional position sizing + consecutive-loss circuit breaker |
| 12 | `execution` | Jito bundle first, direct RPC fallback, actual-vs-expected slippage logging |
| 13 | `exit_manager` | Automated stop-loss + partial take-profit ladder (no manual override) |
| 14 | `db` | SQLite trade journal — full signal vector at entry time for training |
| 15 | `telegram_bot` | Full remote control (see command table below) |
| 16 | `tui` | ratatui dashboard: positions, P&L, win rate, expectancy, signal heatmap |
| 17 | `backtester` | Replay mode against historical data before any filter change touches capital |
| 18 | `anomaly_alerts` | KL divergence detection when live signal distributions drift from training |

## Telegram Command Set

| Command | Function |
|---|---|
| `/start` | Initialize, show main menu |
| `/status` | Uptime, bot state, active position count |
| `/balance` | Current SOL + tracked token holdings |
| `/positions` | Open positions with live P&L |
| `/pnl today\|week\|month\|all` | P&L summary by period |
| `/journal n` | Last n closed trades from DB |
| `/pause` | Stop new entries, keep managing existing exits |
| `/resume` | Resume new entries |
| `/forcesell <token>` | Manual emergency close (excluded from adaptive_weights training) |
| `/setwallet <label>` | Switch active trading wallet by label |
| `/listwallets` | Show configured wallet labels |
| `/setrpc <label> <url>` | Update/add an RPC endpoint |
| `/setwss <url>` | Update WSS endpoint |
| `/listrpc` | Show configured endpoints + live latency/health |
| `/setrisk <param> <value>` | Adjust Kelly fraction, max position size, stop-loss, take-profit |
| `/setfilter <param> <value>` | Adjust scoring_engine thresholds |
| `/retrain` | Manually trigger adaptive_weights retraining job |
| `/abtest on\|off` | Toggle shadow A/B testing |
| `/logs` | Tail recent warnings/errors |
| `/help` | Full command list |

## Non-Negotiable Build Constraints

- **Paper mode required** before any module touches live capital
- **No scoring_engine or filter change goes live** without backtester validation
- **adaptive_weights** does not retrain meaningfully below ~150-200 closed trades — runs on fixed heuristic weights until then
- **All API keys/RPC URLs** configurable via Telegram or config file, never hardcoded
- **Wallet keys** loaded from encrypted local file at boot, referenced only by label over Telegram — raw keys are NEVER accepted via Telegram messages

## Setup

```bash
# 1. Copy and edit config
cp config.sample.toml config.toml
# Edit config.toml with your RPC endpoints, Telegram token, wallet labels

# 2. Build (paper mode by default)
cargo build --release

# 3. Run
RUST_LOG=info ./target/release/pf-sniper config.toml

# 4. (Optional) Run with TUI dashboard
cargo run -- --tui config.toml
```

## Key Program IDs

| Program | Address |
|---------|---------|
| Pump.fun | `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P` |
| PumpSwap AMM | `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA` |
| Jito Tip | `96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5` |

## Research Sources

- [Pump.fun API — Bitquery docs](https://docs.bitquery.io/docs/blockchain/Solana/Pumpfun/Pump-Fun-API/)
- [Chainstack — logsSubscribe for pump.fun](https://docs.chainstack.com/docs/solana-listening-to-pumpfun-token-mint-using-only-logssubscribe)
- [Jito Bundles — Rust crate](https://crates.io/crates/jito-bundle)
- [Jito Explained — Chainstack 2026](https://chainstack.com/jito-explained-bundles-tips-mev-solana/)
- [Solana WebSocket Subscriptions — QuickNode](https://www.quicknode.com/guides/solana-development/getting-started/how-to-create-websocket-subscriptions-to-solana-blockchain-using-typescript)
