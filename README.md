# Auto Trader — Algorithmic Options OMS

A single-user algorithmic trading platform for Indian equity options.  
Signals are scraped from Telegram groups (MTProto userbot), executed via the Kotak Neo API, and monitored through a React dashboard.

---

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│  Telegram Groups  (3rd-party, no bot access)            │
└──────────────────────────┬──────────────────────────────┘
                           │ MTProto (grammers-client)
                           ▼
┌─────────────────────────────────────────────────────────┐
│  telegram_ingester   regex signal parser                 │
│  → TradeSignal → broadcast channel                      │
└──────────────────────────┬──────────────────────────────┘
                           │
                           ▼
┌─────────────────────────────────────────────────────────┐
│  trading_engine   stateful OMS (50ms tick)              │
│  WaitingForEntry → Active → Target1Hit → Closed         │
│  FeeCalculator (STT, SEBI, stamp, GST)                  │
│  → DbWriteMessage → mpsc channel                        │
└──────────────┬────────────────────────┬─────────────────┘
               │                        │
               ▼                        ▼
┌──────────────────────┐   ┌────────────────────────────┐
│  kotak_client        │   │  SQLite (WAL)              │
│  REST: login +       │   │  wallet / paper_trades /   │
│  place_live_order    │   │  system_logs /             │
│  WebSocket: HSM LTP  │   │  trading_config            │
│  feed (mlhsm.*)      │   └────────────────────────────┘
└──────────────────────┘               │
                                       ▼
                           ┌───────────────────────┐
                           │  server  (Axum :8080) │
                           │  GET  /api/portfolio  │
                           │  GET  /api/settings   │
                           │  POST /api/settings   │
                           │  POST /api/webhook/.. │
                           │  GET  /api/logs/stream│
                           └───────────┬───────────┘
                                       │ SSE + REST
                                       ▼
                           ┌───────────────────────┐
                           │  frontend  (Vite/React)│
                           │  Settings bar          │
                           │  P&L chart (recharts)  │
                           │  Trade table           │
                           │  Live log terminal     │
                           └───────────────────────┘
```

---

## Prerequisites

| Tool | Minimum version | Install |
|---|---|---|
| Rust + Cargo | stable ≥ 1.80 | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| Node.js | ≥ 20 | [nodejs.org](https://nodejs.org) |
| pnpm | ≥ 9 | `npm i -g pnpm` |
| SQLite | any | shipped with macOS/Linux |

---

## Quick Start

### 1. Clone & build the backend

```bash
git clone <repo-url> auto-trader
cd auto-trader

# type-check all 5 crates
cargo check

# run the server (SQLite auto-created at trades.db)
cargo run -p server
```

The server listens on **`http://0.0.0.0:8080`**.

### 2. Run the frontend dev server

```bash
cd frontend
pnpm install
pnpm dev          # http://localhost:5173 — proxies /api → :8080
```

Open **`http://localhost:5173`** in your browser.

### 3. Production frontend build

```bash
cd frontend
pnpm build        # outputs to frontend/dist/
```

The Axum server already serves `../frontend/dist` as a static fallback, so after building you can access the dashboard directly at `http://localhost:8080`.

---

## Environment Variables

Create a `.env` file in the project root or export these before starting the server:

```dotenv
# ── SQLite ───────────────────────────────────────────────
DATABASE_URL=sqlite://trades.db          # default

# ── Trading engine ───────────────────────────────────────
PAPER=true                               # true = paper mode (default)
MAX_TRADE_INR=10000                      # max capital per trade
BROKERAGE=20                             # flat brokerage per order leg (₹)

# ── Telegram MTProto ingester (optional) ─────────────────
# Get these from https://my.telegram.org → API Development Tools
TELEGRAM_API_ID=12345678
TELEGRAM_API_HASH=abcdef1234567890abcdef1234567890
# Comma-separated chat IDs to listen to (use a negative number for groups)
TELEGRAM_CHAT_IDS=-1001234567890,-1009876543210

# ── Kotak Neo WebSocket (set after login) ────────────────
# These are obtained by calling kotak_client::KotakClient::login() at startup
KOTAK_AUTH_TOKEN=eyJhbGci...
KOTAK_SID=xxxx-xxxx-xxxx-xxxx
# Scrips to subscribe (pSymbol from scrip master, & separated)
KOTAK_SCRIPS=nse_cm|11536&nse_cm|1594
```

---

## First-Time Kotak Login

The Kotak session tokens (`KOTAK_AUTH_TOKEN`, `KOTAK_SID`) expire daily.  
Run the login helper once per session before starting the server:

```bash
# Example (adapt to your setup — or integrate into a startup script)
cargo run --example kotak_login   # TODO: add this example
```

Until then, the WebSocket market-data feed will silently fail to connect (the position monitor still works in paper mode using `entry_price` as the assumed LTP).

---

## First-Time Telegram Login

On the **first run** with `TELEGRAM_API_ID` set, the ingester will prompt you interactively:

```
Telegram phone number (e.g. +91XXXXXXXXXX): +91XXXXXXXXXX
Login code (sent to your Telegram app): 12345
```

A session file `session.db` is created alongside the binary and reused automatically on subsequent runs.  
If two-factor authentication is enabled you will also be asked for your 2FA password.

---

## Signal Format

The Telegram parser recognises messages like:

```
BUY BHEL 425 CE ABOVE 8.25
TARGET :- 9.50 / 11.50
SL :- 5
JULY EXPIRY
```

| Field | Example | Notes |
|---|---|---|
| Action | `BUY` / `SELL` | Required |
| Instrument | `BHEL` | Underlying name |
| Strike | `425` | Options only |
| Option type | `CE` / `PE` | Options only |
| Entry condition | `ABOVE` / `BELOW` | LTP trigger |
| Entry price | `8.25` | Trigger price |
| Targets | `9.50 / 11.50` | `/`-separated, ordered |
| Stop loss | `SL :- 5` | Initial SL |
| Expiry | `JULY EXPIRY` | Optional |

Equity signals (no strike/type) are also supported:
```
BUY RELIANCE ABOVE 2500
TGT 2600 / 2700
SL 2420
```

---

## Position Lifecycle

```
WaitingForEntry ──LTP crosses entry──▶ Active
                                           │
                    ┌── SL hit ────────────┤
                    ▼                      ├── Target 1 (full exit) ──▶ Closed
                 Closed                   │
                    ▲                      └── Target 1 (partial) ──▶ Target1Hit
                    │                                                      │
                    └──── SL (trailed) or Target 2 hit ───────────────────┘
```

On `Target1Hit`:
- Sells `target_1_exit_pct %` of the position
- Trails the SL to `(avg_buy_price + target_2) / 2`

---

## Dashboard

| Section | URL | Description |
|---|---|---|
| Settings bar | top | Edit brokerage, max trade, targets, PAPER/LIVE toggle. Changes persist to SQLite immediately. |
| P&L chart | middle | Recharts line chart of cumulative realised P&L. |
| Trade table | middle | Full history with gross, charges breakdown, and per-trade P&L. |
| Log terminal | bottom | Live SSE stream of engine events (entry, SL hit, target hit, config changes). |

---

## API Endpoints

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/portfolio` | Wallet balance + last 100 trades |
| `GET` | `/api/settings` | Current `TradingConfig` as JSON |
| `POST` | `/api/settings` | Update `TradingConfig` (persisted to DB + in-memory) |
| `POST` | `/api/webhook/telegram` | Inject a `TradeSignal` JSON manually |
| `GET` | `/api/logs/stream` | SSE stream of engine log events |

---

## Project Structure

```
auto-trader/
├── Cargo.toml                  workspace root
├── backend/
│   ├── shared_domain/          domain types (TradeSignal, TradingConfig, etc.)
│   ├── kotak_client/           Kotak Neo REST + WebSocket client
│   ├── telegram_ingester/      MTProto userbot + regex signal parser
│   ├── trading_engine/         stateful OMS + fee calculator
│   └── server/                 Axum HTTP server + SQLite writer
├── frontend/                   Vite + React 19 + Tailwind v4 dashboard
└── kotak-api-docs/             local copy of official Kotak API docs
```

---

## Fee Calculations (Paper Mode)

All paper trades compute Kotak Neo charges automatically:

| Charge | Rule |
|---|---|
| Brokerage | Flat ₹20 per leg (configurable) |
| SEBI fee | 0.0001% of turnover |
| Exchange fee (NSE) | 0.00297% of turnover |
| STT (options) | 0.05% on SELL side only |
| STT (equity intraday) | 0.025% on SELL side only |
| Stamp duty | 0.003% on BUY side only |
| GST | 18% × (brokerage + SEBI + exchange fee) |
