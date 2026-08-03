# Live Trading — Task Handover

> **Read this first.** This document lets a new model/engineer take over the
> "add real live trading" project for `auto-trader` with zero prior context.
> It is self-contained but points to two companion files:
>
> - **Design/plan (authoritative):** [docs/live-trading-plan.md](./live-trading-plan.md)
> - **Working notes (terse):** `/memories/session/live-trading-plan.md` (agent session memory)

---

## 1. Mission

The trading engine was **paper-only**. We are adding **real live order execution**
through the Kotak Neo API, rolled out in **8 verifiable phases**. Everything is
gated behind a `mode` flag (`"PAPER"` / `"LIVE"`) stored in `trading_config`.
**Paper behaviour must remain byte-for-byte unchanged** at every step.

**Current status: all 8 phases DONE and compiling. Nothing has been run against
the real broker yet.**

### CRITICAL SAFETY CAVEAT (read before touching `mode`)
The LIVE round trip (startup reconciliation → funds check → entry → protective
stop → targets → forced exits → manual close → recording) is implemented, but
**it has never been run against the real broker.** Nothing in it has been
observed placing an actual order, and `mp` (market protection) has only been
confirmed from the docs, never from a fill. Do not flip `mode` to `LIVE` until
the tiny manual test order in §7 has verified `mp` semantics end to end.

---

## 2. The original problem (how this started)
1. Daily reports showed wrong PnL for open/partial positions → fixed (realized-only PnL).
2. Options expiring today were never auto-squared-off → added 15:10 IST square-off.
3. **Discovery:** `place_live_order` in the Kotak client was **never called
   anywhere**; the `mode` flag was displayed but never gated execution — i.e.
   even "LIVE" mode simulated. That discovery spawned this project.

---

## 3. Repository facts

- **Workspace root:** `/Users/oms/Coding/auto-trader` (Cargo workspace).
- **Backend crates** (under `backend/`): `server` (Axum), `kotak_client`,
  `shared_domain`, `telegram_ingester`, `trading_engine`.
- **DB:** sqlx + SQLite (WAL), single writer task. Table of interest:
  `paper_trades` (all fills, paper & live), `trading_config`, `open_positions`
  (JSON snapshot of live `MonitoredPosition`s), `kotak_session`.
- **Frontend:** React 18 + TS + Vite + Tailwind, single file
  [frontend/src/App.tsx](../frontend/src/App.tsx).
- **kotak-bridge:** Node.js websocket bridge, spawned as a child of the Rust server.

### Build / verify (per [AGENTS.md](../AGENTS.md) — MANDATORY after edits)
```bash
# after ANY Rust change:
cd /Users/oms/Coding/auto-trader && cargo build -p server
# after ANY frontend change:
cd /Users/oms/Coding/auto-trader/frontend && pnpm run build
```
There is one **pre-existing** harmless warning (`updated_at` never read in
`db.rs`); ignore it. Any *other* warning/error is yours to fix.

### Deployment context
- GCP: `gcloud compute ssh --zone "us-east1-d" "trader-1" --project "trader-502418"`
- Binary lives at `~/auto-trader/backend/server`, run natively inside **tmux pane `0:0`**
  (not systemd). Restart: `tmux send-keys -t 0:0 "cd ~/auto-trader/backend && ./server" C-m`.
- **PATH CONTRACT:** the binary MUST run from `backend/` — it resolves
  `../frontend/dist` and `../kotak-bridge` relatively.
- Kotak API docs are in `kotak-api-docs/`.

---

## 4. Locked decisions (do not re-litigate — user already approved)

| # | Decision |
|---|----------|
| 1 | Rollout is **phased**; verify each phase with a build. |
| 2 | Order types: **MARKET** for forced exits, **Limit** for targets, **SL-M** for stops. Product code **NRML**. |
| 3 | ~~Exits are native SL-M + Limit OCO.~~ **SUPERSEDED.** Only the **stop is native** (one SL-M sell covering the whole holding). Targets are executed by the engine as market sells. See "Revised decisions" below. |
| 4 | ~~Entry is a native resting order.~~ **SUPERSEDED.** The engine watches the LTP and sends a **market buy** at the trigger. See "Revised decisions" below. |
| 5 | Fills: use **real fill price** from the Positions API (`buyAmt/flBuyQty`, `sellAmt/flSellQty`). Charges: keep the **FeeCalculator estimate** (Kotak exposes no per-order charges). |
| 6 | **Market protection (`mp`)** is **asymmetric**: guard on entry, `mp=0` on protective exits (stop-loss, 15:10 square-off) so they always fill. |
| 7 | Safety features: minimal (no kill-switch / limits requested). |
| 8 | Storage: reuse the `paper_trades` table + a new `mode` column. |
| 9 | Restart: **reconcile** against Kotak Positions/Orders on startup in LIVE mode. |

### Revised decisions (supersede rows 3 and 4 above)

Rows 3 and 4 were reversed by the user after review. The reason for both is the
same and it is the single most important constraint in this project:

> **The account carries no margin. It never sells or shorts options or futures,
> and never buys futures.** Every sell must be covered by stock we already hold.

That makes "resting SL-M for the full quantity **plus** a resting Limit target
for a slice" unacceptable — it commits ~150% of the holding, and if the target
fills first the stop is briefly large enough to open a naked short.

| # | Revised decision |
|---|------------------|
| 3a | **Protection is one native `SL-M` sell for the full held quantity**, `mp = 0`. There is **no native target order**. `target_order_id` is retained on `MonitoredPosition` for snapshot compatibility but is never set. |
| 3b | **Target 1** is engine-driven with strict ordering: **first** shrink the stop to the runner quantity (and trail its trigger), **then** market-sell the slice. `keep + slice == held`, so both legs can fill simultaneously without going short. If the shrink fails, nothing is sold and the whole position is squared off. |
| 3c | **Target 2 / all forced exits** cancel the resting stop **first**, then market-sell. If the cancel fails, **no sell is sent at all**. |
| 3d | **Target-1 slice rounds UP to a whole lot** (3 lots at 50% → sell 2, run 1). A single-lot position therefore exits in full at target 1. |
| 4a | **Entry:** the engine watches the LTP (`ABOVE` → `ltp >= entry`, `BELOW` → `ltp <= entry`, the same trigger points PAPER uses) and sends a **market buy** with `mp = entry_market_protection`. `entry_order_id` guards against re-placing on the next 50 ms tick. |
| 4b | **Partial entry fill** → cancel the remainder, run the position on what filled. |
| 4c | **Rejected entry** → loud error, position dropped, never retried. |
| 10 | **Any failure to establish or maintain protection → loud ERROR + immediate market square-off.** Being flat is always acceptable; being open without a stop is not. |
| 11 | **Every price sent to the broker is rounded DOWN to a tick multiple** (`round_down_tick`) — stops, trailed stops, everything. |
| 12 | **15:29 IST entry cutoff:** no new entry is taken, and every position still waiting for its trigger is dropped. Applies to **both** PAPER and LIVE. |
| 13 | Fills are believed **only** from the **order book** (`fldQty` / `avgPrc`), never from our own LTP. |

### Confirmed fact: `mp` unit = **PERCENT**
Per Kotak's official page
(<https://www.kotakneo.com/support/what-is-market-price-protection-mpp/>):
MPP is a **percentage**, **default 5%**, range **0–20%**. User chose **5%** as the
entry default (stored in `TradingConfig.entry_market_protection`).
- **Open verification:** Kotak's *Strategy Bot* uses a ₹0.50 band at `mp=0%`.
  Whether the **raw Neo Trade API** applies that same floor at `mp=0` is **still
  unverified** — confirm with one tiny live order before trusting `mp=0` exits.

---

## 5. Kotak Neo API reference (used by this project)

All order calls: `POST`, `Content-Type: application/x-www-form-urlencoded`, single
form field `jData` = stringified JSON. Headers: `Authorization` (access token),
`Sid`, `Auth`, `neo-fin-key: neotradeapi`, optional `X-Forwarded-For`.

| Op | Endpoint | jData fields |
|----|----------|--------------|
| Place | `{base}/quick/order/rule/ms/place` | `am,dq,es,mp,pc,pf,pr,pt,qt,rt,tp,ts,tt` |
| Modify | `{base}/quick/order/vr/modify` | place fields + `no` (nOrdNo) |
| Cancel | `{base}/quick/order/cancel` | `on` (nOrdNo), `am`, `ts` (AMO only) |
| Positions | `GET {base}/quick/user/positions` | returns `data[]`: `trdSym, sym, prod, exSeg, buyAmt, sellAmt, flBuyQty, flSellQty, optTp, expDt` (amounts/qtys are **strings**, current-day only, **no charges**) |
| Order book | `GET {base}/quick/user/orders` | returns `data[]`: `nOrdNo, ordSt, trdSym, trnsTp, prcTp, qty, fldQty, avgPrc, rejRsn`. Fields arrive as **strings or numbers** depending on the row — `KotakOrder` normalises both. An **empty book comes back as `Not_Ok` / "no data"**, which `get_order_book()` maps to `Ok(vec![])`. |

`pt` values: `L` / `MKT` / `SL` / `SL-M`. `tt`: `B` / `S`. `pr="0"` for market.
`tp` = trigger for SL/SL-M.

---

## 6. Phase status & exact remaining work

Each phase ends with `cargo build` (+ `pnpm run build` if FE touched).

### ✅ Phase 1 — Kotak client primitives (DONE)
File: [backend/kotak_client/src/client.rs](../backend/kotak_client/src/client.rs), [lib.rs](../backend/kotak_client/src/lib.rs), [error.rs](../backend/kotak_client/src/error.rs).
- Added `modify_order(&self, order, order_no)`, `cancel_order(&self, order_no, trading_symbol)`,
  `get_positions(&self) -> Vec<KotakPosition>`.
- New pub `KotakPosition` struct (string fields) + helpers `buy_qty()`, `sell_qty()`,
  `net_qty()`, `avg_buy_price()`, `avg_sell_price()` (div-by-zero guarded).
- New `KotakError::ApiError { status_code, message }`.
- Reused existing header/session conventions from `place_live_order`.

### ✅ Phase 2 — Domain & schema plumbing (DONE)
Files: [shared_domain/src/lib.rs](../backend/shared_domain/src/lib.rs), [server/src/db.rs](../backend/server/src/db.rs).
- `MonitoredPosition` +`entry_order_id` / `sl_order_id` / `target_order_id: Option<String>` (`#[serde(default)]`).
- `DbWriteMessage::Trade` +`mode: String`; `send_trade(...)` in monitor takes `mode: &str` (both call sites pass `&cfg.mode`).
- `paper_trades` +`mode` column (migration `ALTER TABLE ... DEFAULT 'PAPER'`), writer records it.
- **NOTE:** `routes/positions.rs` `close_position_handler` does its own `paper_trades`
  INSERT and currently omits `mode` (relies on the column DEFAULT). Wire it in Phase 7.

### ✅ Phase 3 — LIVE entry path (DONE, then reworked in Phase 4)
Files: [trading_engine/src/monitor.rs](../backend/trading_engine/src/monitor.rs), [server/src/main.rs](../backend/server/src/main.rs), [shared_domain/src/lib.rs](../backend/shared_domain/src/lib.rs), [server/src/db.rs](../backend/server/src/db.rs), [server/src/routes/settings.rs](../backend/server/src/routes/settings.rs), [frontend/src/App.tsx](../frontend/src/App.tsx).
- `TradingConfig.entry_market_protection: f64` (default 5.0) end-to-end (schema, load/save, FE "Entry MP %" input).
- `start_position_monitor` now takes `kotak: Arc<Mutex<Option<KotakClient>>>` (passed from `main.rs`).
- `compute_entry_qty()` survives. **`build_entry_order()` and `reconcile_live_entry_fills()` were
  deleted** in Phase 4 — resting entry orders and Positions-API fill detection are both gone,
  replaced by LTP-triggered market buys settled from the order book (revised decisions 4a and 13).

### ✅ Phase 4 — LIVE exits (DONE)
Files: [trading_engine/src/monitor.rs](../backend/trading_engine/src/monitor.rs), [shared_domain/src/lib.rs](../backend/shared_domain/src/lib.rs), [kotak_client/src/client.rs](../backend/kotak_client/src/client.rs).

LIVE no longer shares the PAPER two-pass loop at all. The tick branch routes on
`cfg.mode`, and LIVE runs `live_tick()` end to end — **the PAPER path is
unreachable in LIVE and vice versa**, which is what keeps paper behaviour safe.

`live_tick()` has four stages, and no positions lock is held across a broker call:
1. **`reconcile_live_orders()`** (every `LIVE_POLL_INTERVAL`, 2 s) — settles the
   entry / stop / pending-exit legs against `get_order_book()`.
2. **`decide_live()`** — pure, read-lock only, returns at most **one** `LiveAction`
   per position. Ordering: forced exit → protection → targets. It returns nothing
   while an engine-initiated exit is in flight, which is what bounds
   `resting stop qty + in-flight sell qty <= held`.
3. **`exec_live_action()`** — performs the broker calls and writes results back
   through short-lived `with_position()` locks.
4. Drops `Closed` positions, cancelling any order id they still carry.

- New client method **`get_order_book()`** + `KotakOrder` (string-or-number tolerant,
  with `is_complete` / `is_rejected` / `is_cancelled` / `is_terminal`).
- New `MonitoredPosition` fields, all `#[serde(default)]` so **no DB wipe is needed**:
  `sl_order_qty`, `sl_order_trigger`, `pending_exit_order_id`, `pending_exit_qty`,
  `pending_exit_reason`, `entry_cancel_sent`, `exit_attempts`, `live_halt`.
- Failure policy: every failure is a `loud_error()` (tracing + an ERROR row in the UI).
  A failed stop placement or stop modify → immediate market square-off. Failed exits are
  counted in `exit_attempts`, spaced to the poll cadence, and after `MAX_EXIT_ATTEMPTS`
  (3) the position is `live_halt`ed — no further orders, manual intervention required.
- `EXIT_AT <price>` is **advisory in LIVE**: we exit at market and record the real fill.
- Unit tests cover `round_down_tick` and `tgt1_slice_qty` (`cargo test -p trading_engine`).

**Two latent bugs fixed here (both flagged and user-approved):**
- `resolved_order` is no longer overwritten with the full-size entry order, so it stays a
  pristine **one-lot template** and `lot_size_of()` is correct.
- `EXIT_AT` / `ENTRY_CHANGED_ERROR` / opposite-signal no longer set a `WaitingForEntry`
  position straight to `Closed` (which in LIVE would abandon an in-flight entry order).
  They set `force_exit`, and each mode closes it through its own path — LIVE via
  `AbandonEntry` (cancel, then close), PAPER via the new `PosAction::Expire`.

### ✅ Phase 5 — LIVE expiry square-off @ 15:10 (DONE)
- `is_expiry_squareoff_due()` is checked in `decide_live()` and produces the same
  `ExitAll` action as any other forced exit: cancel the resting stop, then market-sell
  with `mp = 0`. No separate code path.
- The related **15:29 entry cutoff** (`is_entry_cutoff_passed()`) is wired into **both**
  modes — `AbandonEntry` in LIVE, `PosAction::Expire` in PAPER.

### ✅ Phase 6 — Startup reconciliation (DONE)
File: [trading_engine/src/monitor.rs](../backend/trading_engine/src/monitor.rs) — `reconcile_on_startup()`.

Runs **once**, on the first LIVE tick that has a Kotak session, guarded by a
`startup_reconciled` flag in `start_position_monitor`. Deferring it to the tick
loop (rather than doing it in `main.rs`) means it still runs when the user logs
in *after* the monitor starts, or flips PAPER → LIVE mid-day.

Order of operations:
1. `reconcile_live_orders()` first, so a fill that landed while we were down is
   recorded as a real trade instead of being inferred from a quantity gap below.
2. `get_positions()` + `get_order_book()`. **If either call fails, reconciliation
   aborts with a loud error and does not touch state** — acting on a partial view
   is worse than not acting.
3. Per tracked position:
   - `WaitingForEntry`: an `entry_order_id` that is absent from today's book is a
     leftover from a previous session → cleared, so the trigger can fire again.
   - Open (`Active` / `Target1Hit`): broker `net_qty` is authoritative. Zero →
     loud error + close locally. Different → loud error + adopt the broker's number.
   - **Stop adoption** (the reason this phase exists). Count the non-terminal sell
     orders resting on that symbol: **0** → forget the stale `sl_order_id` so the
     decision pass places a fresh stop; **1** → adopt its id, remaining qty and
     `trigger()` (loud error if we did not already know about it), and let the
     normal decision pass modify it if the size or level is wrong; **>1** → loud
     error and `live_halt` the position, because two resting sells against one
     holding is exactly the oversell this account cannot carry.
4. Broker exposure and resting sells with no matching tracked position are
   **reported, never touched** — an unexplained order may be a manual one, and
   cancelling on a guess is worse than leaving it.

- **Verify:** `cargo build`; restart with an open live position.

### ✅ Pre-entry funds check (added with Phase 6)
File: [trading_engine/src/monitor.rs](../backend/trading_engine/src/monitor.rs), [kotak_client/src/client.rs](../backend/kotak_client/src/client.rs).
- New client method **`get_limits()`** → `KotakLimits { net, margin_used, collateral_value }`
  (`POST {base}/quick/user/limits`, `jData={"seg":"ALL","exch":"ALL","prod":"ALL"}`).
- `LiveAction::PlaceEntry` now carries `{ qty, ltp }`, and `exec_live_action` checks
  `limits.net >= qty * ltp + FUNDS_BUFFER_INR` before sending the buy.
  **`FUNDS_BUFFER_INR = 500.0`** — a ₹10,000 order needs ₹10,500 available.
- **A limits call that fails is treated as insufficient funds**: the entry is dropped
  with a loud error. Missing a signal is recoverable; buying blind is not.

### ✅ Phase 7 — Manual close (LIVE) (DONE)
Files: [server/src/routes/positions.rs](../backend/server/src/routes/positions.rs), [frontend/src/App.tsx](../frontend/src/App.tsx).
- `close_position_handler` in LIVE **does not book anything itself**. It sets
  `force_exit = "CLOSED_VIA_FRONTEND"` and returns **`202 Accepted`**; the monitor cancels the
  resting stop, market-sells, and records the trade at the price that actually filled. Booking
  it in the handler would have written a fake fill and orphaned a real position.
- `delete_position_handler` in LIVE returns **`409 Conflict`** for a position that still holds
  quantity or tracks any broker order — forgetting it would leave real, unmonitored exposure
  and the engine would then cancel its stop as an orphan.
- The PAPER `paper_trades` INSERT now binds `mode` explicitly (the Phase-2 note is resolved).
- Frontend: `closeOngoingTrade` keeps the row on a `202` (the exit is not filled yet), and
  `cancelTrade` now checks `res.ok` instead of optimistically removing the row.

### ✅ Phase 8 — Recording & reports (DONE)
Files: [server/src/routes/portfolio.rs](../backend/server/src/routes/portfolio.rs), [frontend/src/App.tsx](../frontend/src/App.tsx).
- Write side was already complete: both LIVE `send_trade()` call sites
  (`record_live_exit`, the entry-fill branch of `reconcile_live_orders`) pass `"LIVE"`
  with the **real** fill price and `FeeCalculator` charges; PAPER and the manual-close
  handler pass `"PAPER"`.
- Read side: `PaperTrade` (Rust and TS) gained `mode`, and `GET /api/portfolio` selects it.
- UI: an amber **LIVE** badge on the trades table row, on the reports signal group, and
  on each execution in the trade-details modal. **Only LIVE is badged** — absence means
  paper, which keeps the historical paper rows quiet and makes real money stand out.
- **Verify:** `cargo build` + `pnpm run build`. Both pass.

---

## 7. Before ever running LIVE (operational checklist)
1. Fund the account with enough headroom for the intended order size **plus the
   ₹500 buffer** — the engine refuses to enter otherwise, and it also refuses if the
   limits call itself fails.
2. Place **one tiny live test order** (1 lot) to confirm: `mp` percent semantics, that
   `mp=0` market exits actually fill, that a **partially-filled market buy** ends up
   `cancelled` (not stuck `open`) in the order book, and that `fldQty`/`avgPrc` map as expected.
3. Confirm an `SL-M` sell is **accepted at `mp=0`** and that its trigger is honoured on the
   tick grid we round to.
4. Only then flip `mode` to `LIVE` via the settings UI.

---

## 8. Conventions & gotchas learned
- **Kotak numbers are strings** in JSON responses — parse defensively (guard div-by-zero).
  The **order book** is worse: the same field can be a string on one row and a number on the
  next, which is why `KotakOrder` deserialises everything through `de_loose_string`.
- Keep the **PAPER path untouched**. As of Phase 4 the two modes no longer share the tick body
  at all — LIVE returns early into `live_tick()`. Do not reintroduce `if cfg.mode == "LIVE"`
  branches inside the paper passes.
- **Never hold a lock across a Kotak network call.** Snapshot needed data under the lock, drop it,
  do I/O, then re-acquire to apply. `decide_live` / `exec_live_action` is the reference pattern;
  `with_position()` is the short-lived write-back helper.
- The monitor is a **single task**, and `tokio::select!` polls only one branch at a time, so a
  `live_tick()` in progress cannot interleave with signal handling or with another tick. The
  sequencing inside `exec_live_action` relies on that.
- Entry semantics: `ABOVE entry_price` → trigger when `ltp >= entry`; `BELOW` → `ltp <= entry`.
  Both send a **market buy** in LIVE; the words no longer describe an order type.
- `net_value` in `paper_trades`: BUY = cash-out (price×qty + fees), SELL = cash-in (price×qty − fees),
  computed by `FeeCalculator` in [backend/trading_engine/src/fees.rs](../backend/trading_engine/src/fees.rs).
- Expiry strings are stored uppercase like `26-JUL-2026` (`%d-%b-%Y`; chrono parses `%b` case-insensitively).
- `OrderType` serde renames: `Limit`→`L`, `Market`→`MKT`, `StopLoss`→`SL`, `StopLossMarket`→`SL-M`.
  `TransactionType`: `Buy`→`B`, `Sell`→`S`. `ProductCode::Nrml`→`NRML`.

---

## 9. Handover summary (one paragraph)
We are converting `auto-trader` from paper-only to real live trading via Kotak Neo, in 8 phases,
all gated by a `mode` flag. **All 8 phases are now done and compiling**; the PAPER path is
unchanged except for the shared 15:29 entry cutoff. The LIVE design changed materially partway
through (see "Revised decisions" in §4): entries are **LTP-triggered market buys**, protection is a
**single native SL-M for the full holding**, and targets are **engine-driven market sells** —
because the account has no margin and must never place a sell that exceeds what it holds.
An entry is also refused unless the account shows the order value **plus a ₹500 buffer**.
**Nothing here has ever touched the real broker.** The remaining work is operational, not
code: run the §7 checklist, above all the tiny test order that confirms `mp` semantics, before
flipping `mode` to `LIVE`.
Full design is in [docs/live-trading-plan.md](./live-trading-plan.md).
