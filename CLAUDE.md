# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Safety rules (read before writing any code)

This project places **real orders on a live Kotak Neo brokerage account** — real money is at stake. These rules come from `AGENTS.md` and apply to every change in this repo, not just order-placement code:

- **Ask before writing code** if any part of a design is unclear. A wrong assumption here isn't a refactor you can redo later — it can lose money. Build the parts that are already settled while waiting on an answer, rather than blocking on everything.
- Don't treat previously written-down decisions as settled just because they're written down — several have been reversed after review. Re-confirm anything that changes order behaviour.
- When forced to make a judgment call, pick the option that errs toward **not trading** / **being flat**, and say so explicitly.
- **Never sell more than we hold.** The account carries no margin — it only buys options, never shorts/sells options or futures, never buys futures. Invariant: `resting stop qty + in-flight sell qty <= executed_qty` must hold at all times. Cancel/shrink a resting stop *before* sending another sell; if the cancel/shrink fails, send no sell at all.
- If a change requires a SQLite schema change that isn't backward compatible, tell the user the DB may need clearing.

## Commands

### Backend (Rust workspace, root `Cargo.toml`)

```bash
cargo check                       # type-check all 5 workspace crates
cargo build                       # debug build
cargo build --release -p server   # release build of just the server (what gets deployed)
cargo run -p server                # run the server locally (SQLite auto-created at ./trades.db)
cargo test -p telegram_ingester    # run one crate's tests (also: kotak_client, trading_engine)
cargo test -p telegram_ingester parses_basic_signal   # run a single test by name
```

Run the server from the **repo root**, not from `backend/`: it resolves `trades.db`, `session.json`, `../frontend/dist`, and `kotak-bridge/` relative to the working directory.

**Always run `cargo build` (or `cargo check`) after any Rust change to verify it compiles before finishing a task.**

### Frontend (`frontend/`)

```bash
pnpm install
pnpm dev            # Vite dev server on :5173, proxies /api -> :8080
pnpm run build       # tsc -b && vite build -> frontend/dist (served by the Axum backend as a static fallback)
pnpm lint
```

**Always run `pnpm run build` in `frontend/` after any frontend change** to catch type/build errors.

### kotak-bridge (`kotak-bridge/`, Node.js)

```bash
cd kotak-bridge && npm install
```

Not run directly — the Rust backend spawns `bash -c "node index.js"` itself (see Architecture).

## Architecture

Single-user algorithmic options trading platform. Signals come from Telegram, get parsed and turned into managed positions by a Rust state machine, and are executed against the Kotak Neo brokerage API. A React dashboard observes everything over REST + SSE.

```
Telegram groups --(MTProto/grammers)--> telegram_ingester (regex parser)
                                              |  TradeSignal (broadcast channel)
                                              v
                                        trading_engine (50ms tick position monitor)
                                        WaitingForEntry -> Active -> Target1Hit -> Closed
                                              |                     |
                                    DbWriteMessage (mpsc)     kotak_client (place_live_order,
                                              |                HSM LTP WebSocket via Node bridge)
                                              v
                                        SQLite (WAL): wallet / paper_trades / system_logs /
                                                       trading_config / open_positions / kotak_session
                                              ^
                                              |
                                        server (Axum :8080) -- REST + SSE --> frontend (Vite/React)
```

### Workspace crates (`backend/`, members of root `Cargo.toml`)

- **`shared_domain`** — shared types (`TradeSignal`, `TradingConfig`, `MonitoredPosition`, `DbWriteMessage`, `ExecutionResult`, `OrderRequest`) and IST time helpers (`now_ist`, `is_market_open`, `today_market_close_ist`, …). Every date/time decision in the backend is normalized to IST (`UTC+05:30`) via these helpers regardless of the server's actual timezone — always use them instead of `chrono::Utc::now()` directly.
- **`kotak_client`** — Kotak Neo REST client (two-step login: TOTP → MPIN validate, in `client.rs`) plus the HSM market-data WebSocket. The WebSocket is **not** a native Rust connection: `websocket.rs` locates the `kotak-bridge/` directory (relative to CWD or `CARGO_MANIFEST_DIR`) and spawns `node index.js` as a child process, talking to it over line-delimited JSON on stdin/stdout. `bin/live_probe.rs` is a standalone connectivity probe.
- **`telegram_ingester`** — MTProto userbot (grammers) + regex-based signal parser (`parser.rs`, has unit tests) + step-by-step login/session manager (`manager.rs`) for the frontend's Telegram login flow. Session persisted to `session.json`.
- **`trading_engine`** — the OMS core. `monitor.rs` (~2400 lines) is `start_position_monitor`, a 50ms-tick loop driving the position state machine described in README's Position Lifecycle. `fees.rs` computes Kotak's exact charge breakdown (brokerage, STT, SEBI, stamp duty, GST) for paper-mode P&L. `scrip_master.rs` parses/stores the daily Kotak scrip master CSV (`ScripStore`) used to resolve instrument name + strike + expiry to a tradeable symbol.
- **`server`** — Axum HTTP server. `main.rs` wires everything together at startup (DB init, restore Kotak session from DB if still valid for today, restore open positions, spawn the position monitor, spawn a daily 09:10 IST scrip-master refresh task, spawn a daily 15:40 IST Kotak session-clear task). `db.rs` owns schema/migrations (`ensure_column` for additive migrations) and a single-writer `db_writer` task that serializes all SQLite writes (avoids "database is locked"). `routes/` holds one file per concern (`auth_kotak`, `auth_telegram`, `auth_passkey`, `settings`, `portfolio`, `positions`, `health`).

Note: there is a second, unused `shared_domain/Cargo.toml` at the repo root (outside `backend/`) — it is **not** a workspace member; the real crate is `backend/shared_domain`.

### Auth model

- Single shared **passkey** (`PASSKEY` env var) gates the whole app; `POST /api/auth/verify-passkey` exchanges it for an HMAC-signed, JWT-like bearer token (`AUTH_SECRET` env var signs it) with an `exp` claim. `auth_middleware` in `main.rs` checks this token on every `/api/*` route except `/api/auth/verify-passkey` and `/api/health` (and accepts it via `?token=` query param for the SSE log stream, since `EventSource` can't set headers).
- This is unrelated to the **Kotak** login (mobile/UCC/TOTP/MPIN, against Kotak's own API) and the **Telegram** login (phone/OTP/2FA, against Telegram's MTProto API) — both are separate, per-broker/per-source auth flows exposed under `/api/auth/kotak/*` and `/api/auth/telegram/*`.

### Frontend (`frontend/`, Vite + React 19 + Tailwind v4)

`src/App.tsx` is the shell; `src/components/` holds the dashboard panels (settings bar, portfolio, log terminal, login panels for Kotak/Telegram, reports); `src/screens/` holds full routed views (trade analytics, portfolio performance); `src/lib/api.ts` wraps backend calls and `src/lib/auth.ts` manages the passkey bearer token in `localStorage`. The backend URL itself is stored client-side (`localStorage` + cookie) rather than baked in at build time, since the frontend (Vercel) and backend (VM) are typically deployed separately — see README's split-deployment guide.

### Deployment

Production backend runs on a GCP VM (`gcloud compute ssh --zone "us-east1-d" "trader-1" --project "trader-502418"`), natively (not systemd) inside `tmux` session `0`, pane `0:0`. The binary lives at `~/auto-trader/backend/server`; restarting it means `tmux send-keys -t 0:0 "cd ~/auto-trader/backend && ./server" C-m`. See `README.md` for the full VM + Vercel split-deployment guide, systemd unit alternative, and Nginx reverse-proxy config.

Kotak API reference docs are checked into `kotak-api-docs/`.
