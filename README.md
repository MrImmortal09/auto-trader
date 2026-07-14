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

### 4. Deploy frontend on Vercel + backend on a VM

This repo now supports a split deployment model:

- Backend on a VM or bare-metal host
- Frontend on Vercel

For the backend host:

```bash
cd auto-trader
cargo build --release -p server
cd kotak-bridge && npm install
```

Then run the backend from the repo root so the SQLite database, Telegram session file, and `kotak-bridge/` folder all stay in a stable relative layout:

```bash
cd auto-trader
./target/release/server
```

Recommended VM notes:

- Open TCP port `8080`, or put Nginx/Caddy in front and expose HTTPS on `443`
- Keep `trades.db` and `session.json` on persistent disk
- Install Node.js on the VM because the Rust backend launches `kotak-bridge/index.js`

For the Vercel frontend:

- Deploy the `frontend/` directory
- On first load, enter your backend URL or IP in the `Server URL or IP:PORT` field in the Kotak login panel
- The frontend stores that server address in browser storage and a cookie, so you do not need to re-enter it every time

Examples:

- `http://34.93.xx.xx:8080`
- `https://api.example.com`

---

## Environment Variables

The backend currently reads environment variables from the process environment.
It does not automatically load a `.env` file, so either:

- export the variables in your shell before starting the server, or
- define them in a `systemd` unit with `Environment=` or `EnvironmentFile=`

These are the supported variables:

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

Example shell startup:

```bash
export DATABASE_URL=sqlite:///home/ubuntu/auto-trader/trades.db
export TELEGRAM_API_ID=12345678
export TELEGRAM_API_HASH=abcdef1234567890abcdef1234567890
export TELEGRAM_CHAT_IDS=-1001234567890,-1009876543210

cd ~/auto-trader
./target/release/server
```

---

## Deployment Guide

This section assumes:

- backend on a GCP Ubuntu/Debian VM
- frontend on Vercel
- repo uploaded with the same folder layout

### 1. Prepare the GCP VM

Create an e2-micro VM and allow:

- SSH from your admin IP
- TCP `8080` if you want to expose the Rust server directly
- TCP `80` and `443` if you will use Nginx/Caddy as a reverse proxy

Recommended baseline packages:

```bash
sudo apt update
sudo apt install -y build-essential pkg-config libssl-dev curl git nginx
curl https://sh.rustup.rs -sSf | sh -s -- -y
curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -
sudo apt install -y nodejs
source "$HOME/.cargo/env"
node -v
npm -v
cargo -V
```

### 2. Upload the repo and install dependencies

```bash
cd ~
git clone <repo-url> auto-trader
cd auto-trader

cargo build --release -p server
cd kotak-bridge
npm install
cd ..
```

Important runtime note:

- run the backend from the repo root so `trades.db`, `session.json`, `frontend/dist`, and `kotak-bridge/` all resolve correctly

### 3. Build the frontend for Vercel

If you are deploying from GitHub, point Vercel at the `frontend/` directory.

Vercel settings:

- Framework preset: `Vite`
- Root directory: `frontend`
- Build command: `pnpm build`
- Output directory: `dist`

No frontend environment variable is required for the backend URL because the UI now asks for it and stores it in browser storage and a cookie.

### 4. Start the backend manually once

Export only the variables you actually need. Example:

```bash
cd ~/auto-trader
export DATABASE_URL=sqlite:///home/ubuntu/auto-trader/trades.db
export PAPER=true
export MAX_TRADE_INR=10000
export BROKERAGE=20
export TELEGRAM_API_ID=12345678
export TELEGRAM_API_HASH=abcdef1234567890abcdef1234567890
export TELEGRAM_CHAT_IDS=-1001234567890

./target/release/server
```

Expected behavior:

- server binds to `0.0.0.0:8080`
- Kotak bridge starts only after valid Kotak login tokens exist
- Telegram auth state is stored in `session.json`

### 5. Put the backend under systemd

Create a dedicated environment file:

```bash
sudo mkdir -p /etc/auto-trader
sudo tee /etc/auto-trader/server.env >/dev/null <<'EOF'
DATABASE_URL=sqlite:///home/ubuntu/auto-trader/trades.db
PAPER=true
MAX_TRADE_INR=10000
BROKERAGE=20
TELEGRAM_API_ID=12345678
TELEGRAM_API_HASH=abcdef1234567890abcdef1234567890
TELEGRAM_CHAT_IDS=-1001234567890,-1009876543210
KOTAK_SCRIPS=nse_cm|11536&nse_cm|1594
EOF
```

Create the service file:

```bash
sudo tee /etc/systemd/system/auto-trader.service >/dev/null <<'EOF'
[Unit]
Description=Auto Trader backend
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=ubuntu
WorkingDirectory=/home/ubuntu/auto-trader
EnvironmentFile=/etc/auto-trader/server.env
ExecStart=/home/ubuntu/auto-trader/target/release/server
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
```

Enable and start it:

```bash
sudo systemctl daemon-reload
sudo systemctl enable auto-trader
sudo systemctl start auto-trader
sudo systemctl status auto-trader
journalctl -u auto-trader -f
```

### 6. Optional: expose backend through Nginx

Directly exposing `:8080` works, but a reverse proxy is cleaner and lets you add TLS.

Example Nginx site:

```nginx
server {
   listen 80;
   server_name api.example.com;

   location / {
      proxy_pass http://127.0.0.1:8080;
      proxy_http_version 1.1;
      proxy_set_header Host $host;
      proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
      proxy_set_header X-Forwarded-Proto $scheme;
      proxy_set_header Connection '';
      proxy_buffering off;
   }
}
```

Then enable it:

```bash
sudo tee /etc/nginx/sites-available/auto-trader >/dev/null <<'EOF'
server {
   listen 80;
   server_name api.example.com;

   location / {
      proxy_pass http://127.0.0.1:8080;
      proxy_http_version 1.1;
      proxy_set_header Host $host;
      proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
      proxy_set_header X-Forwarded-Proto $scheme;
      proxy_set_header Connection '';
      proxy_buffering off;
   }
}
EOF

sudo ln -s /etc/nginx/sites-available/auto-trader /etc/nginx/sites-enabled/auto-trader
sudo nginx -t
sudo systemctl reload nginx
```

If you have a domain, add HTTPS with Certbot afterward.

### 7. Connect the Vercel frontend to the VM

After the Vercel site is live:

1. Open the frontend in the browser.
2. In the Kotak login panel, enter your backend address in `Server URL or IP:PORT`.
3. Use either `http://PUBLIC_IP:8080` or your reverse-proxied domain such as `https://api.example.com`.
4. Continue using the UI normally; all API calls, SSE logs, and downloads will use that stored backend URL.

Examples:

- `http://34.93.xx.xx:8080`
- `https://api.example.com`

### 8. First live login flow after deployment

For Telegram:

1. Open the frontend.
2. Enter Telegram API ID, API hash, and phone number.
3. Request the login code.
4. Submit the OTP and 2FA password if prompted.
5. Select the chats to monitor.

For Kotak:

1. Enter the backend server URL once.
2. Enter Kotak access token, mobile number, UCC, TOTP, and MPIN.
3. Click `Connect`.
4. On success, the backend fetches the scrip master and starts the Node bridge.

### 9. Smoke-test checklist

Before considering the deployment ready, verify:

- `curl http://127.0.0.1:8080/api/settings` returns JSON on the VM
- `systemctl status auto-trader` shows the service is healthy
- the Vercel frontend can load settings and portfolio data
- live logs connect through `/api/logs/stream`
- Telegram chat selection works
- Kotak login succeeds and scrip master download works

### 10. Important persistence notes

These files should remain on persistent disk on the VM:

- `trades.db`
- `session.json`
- the built server binary under `target/release/server`
- `kotak-bridge/node_modules/`

If you rebuild or redeploy, keep the same working directory or copy over the database and session files.

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

The current implementation stores the Telegram session in `session.json` in the working directory.

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
