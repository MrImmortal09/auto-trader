# Live Trading Implementation Plan

> Status: **Design approved — implementing in phases.**
> Owner: engineering. Last updated: 2026-07-27.

This document is the single source of truth for turning the auto-trader from a
paper-only simulator into a system that places **real** orders on Kotak Neo,
gated by the `mode` (`LIVE` / `PAPER`) flag.

---

## 1. Background — why this work exists

The engine is **paper-only today**. Verified findings:

- `place_live_order` ([backend/kotak_client/src/client.rs](../backend/kotak_client/src/client.rs)) is the **only** order-placement
  function and is **never called anywhere** in the codebase.
- The position monitor (`start_position_monitor`,
  [backend/trading_engine/src/monitor.rs](../backend/trading_engine/src/monitor.rs)) is spawned **without a Kotak client**
  ([backend/server/src/main.rs](../backend/server/src/main.rs)). Its Pass-2 "execution" only calls the
  `FeeCalculator` simulator and writes to the `paper_trades` table.
- The `mode` = `"LIVE"` / `"PAPER"` flag is stored, loaded and displayed but
  **never gates execution**. Even in `LIVE` mode, everything is simulated.

Consequence: our recent changes (realized-PnL report; expiry square-off at
15:10) live in the single shared path, but the expiry square-off only
**simulates** a sell in live — it never reaches Kotak.

The goal of this plan is to make **entries, stop-loss, targets, manual close,
and the expiry square-off** place and manage real exchange orders in `LIVE`
mode, while keeping `PAPER` mode behaviour exactly as it is today.

---

## 2. Locked decisions

| Topic | Decision |
|-------|----------|
| Rollout | Phased; verify (`cargo build` + paper test) each phase before the next. |
| Order type | **Market** for entries and forced exits; **Limit** for targets. |
| Product code | **NRML** (carry-forward). Makes the 15:10 expiry square-off mandatory. |
| Exit orders | **Native** exchange orders: **SL-M** (stop-loss) + **Limit** (target), engine-managed **OCO**. (BO/CO discontinued Apr 2026.) |
| Entry orders | **Native**, resting on the exchange at `entry_price`. `ABOVE` → **SL-M buy** (stop, trigger = entry). `BELOW` → **Limit buy** at entry. |
| Fill price | Real, from the **Positions API** (`buyAmt/flBuyQty`, `sellAmt/flSellQty`). |
| Charges | **Estimated** via `FeeCalculator` (Kotak's documented APIs expose no per-order charges). |
| Market protection (`mp`) | **Asymmetric**: guard % on **entry** (default **5%**, Kotak default); `mp = 0` on **stop-loss** & **15:10 square-off** (protective exits must always fill). Unit CONFIRMED = percent (0–20). |
| Safety controls | Minimal (no kill-switch / rate / max-loss limits requested). |
| Storage | Same `paper_trades` table + new **`mode`** column (`LIVE` / `PAPER`). |
| Restart | **Reconcile** in-memory positions/orders against Kotak on startup in `LIVE` mode. |

---

## 3. Market protection (`mp`) — policy

A market order fills against whatever is resting in the book. In thin books
(common for **stock / non-index options**) that can be far from the last trade.
`mp` caps how far from a reference price a market order may execute; beyond the
band the excess does not fill. `mp = 0` disables protection (fills at any
price).

- **Unit CONFIRMED = percentage.** Per Kotak's official support page
  (<https://www.kotakneo.com/support/what-is-market-price-protection-mpp/>):
  MPP is a **percent**, **default 5%**, adjustable **0%–20%**. It ensures an
  order executes only within a set price range relative to the order price.
- Caveat (Kotak Strategy-Bot behaviour): at `mp = 0%`, orders are placed as
  limit orders with a **₹0.50** minimum protection band. Whether the **raw Neo
  Trade API** applies that same ₹0.50 floor at `mp = 0` is still **to be
  verified with one tiny live order** before relying on `mp = 0` for exits.
- **Entry** (SL-M / Limit buy): a guard value protects against chasing a spike;
  worst case is a *missed* entry — safe. Default to Kotak's own **5%**.
- **Stop-loss (SL-M sell) and 15:10 square-off (MKT sell)**: these are
  protective exits; they **must** fill. A tight `mp` here can leave us stuck in
  a losing/expiring position. → `mp = 0`.
- The entry guard value will be a **configurable `TradingConfig` field**
  (`entry_market_protection`, default `5.0`), added in Phase 3.


---

## 4. Kotak Neo API reference (used by this plan)

All order calls: `Content-Type: application/x-www-form-urlencoded`, single
`jData` field (stringified JSON). Headers: `Authorization` (access token),
`Sid`, `Auth`, `neo-fin-key: neotradeapi`, optional `X-Forwarded-For`.

| Purpose | Method + path | Key `jData` fields |
|---------|---------------|--------------------|
| Place | `POST {base}/quick/order/rule/ms/place` | `am,dq,es,mp,pc,pf,pr,pt,qt,rt,tp,ts,tt` |
| Modify | `POST {base}/quick/order/vr/modify` | place fields + `no` (nOrdNo) |
| Cancel | `POST {base}/quick/order/cancel` | `on` (nOrdNo), `am`, `ts` (req. for AMO) |
| Positions | `GET {base}/quick/user/positions` | — (returns `data[]`) |

Field values: `pt` = `L` (Limit) / `MKT` (Market) / `SL` / `SL-M`;
`tt` = `B` / `S`; `pc` = `NRML` / `CNC` / `MIS`; `pr` = `"0"` for market;
`tp` = trigger price for SL / SL-M.

Order response: `{ "nOrdNo": "...", "stat": "Ok", "stCode": 200 }`;
error `{ "stat": "Not_Ok", "emsg": "...", "stCode": <code> }`.

Positions response `data[]` object (amounts/qtys are **strings**):
`trdSym, sym, prod, exSeg, qty, buyAmt, sellAmt, flBuyQty, flSellQty, lotSz,
optTp, expDt, posFlg, sqrFlg`. Current trading day only. **No charges field.**

> Doc ambiguity: the Modify field *table* lists `vd` for validity while the
> Modify *example* reuses the place-style `rt`. We mirror the working place
> payload (`rt`) + `no`, and revisit if the broker rejects it.

---

## 5. Data-model & schema changes

**`shared_domain::MonitoredPosition`** — add (all `#[serde(default)]`):

- `entry_order_id: Option<String>` — Kotak `nOrdNo` of the resting entry order.
- `sl_order_id: Option<String>` — `nOrdNo` of the live SL-M sell.
- `target_order_id: Option<String>` — `nOrdNo` of the live target limit sell.

**`paper_trades` table** — add column `mode TEXT NOT NULL DEFAULT 'PAPER'`
(migration via `ALTER TABLE ... ADD COLUMN`, matching the existing additive
migration style in [backend/server/src/db.rs](../backend/server/src/db.rs)).

**`TradingConfig`** — add `entry_market_protection` (entry `mp` guard; unit TBD,
default small). Wired in Phase 3.

---

## 6. Fills & fees strategy

- **Fill price**: after an order is confirmed filled, read the real average
  price from the Positions API: `avg_buy = buyAmt / flBuyQty`,
  `avg_sell = sellAmt / flSellQty` (guard divide-by-zero).
- **Fill detection**: **poll** Positions / order state on the existing monitor
  cadence (throttled) — documented and reliable. A push-based Neo order feed
  (`hsi`, see [kotak-api-docs/neo-websocket.md](../kotak-api-docs/neo-websocket.md)) may exist; if confirmed it can
  replace polling later.
- **Charges**: keep `FeeCalculator` estimates for the statutory breakdown; the
  documented APIs do not return per-order charges. Recorded PnL therefore uses
  **real fill price + estimated charges**.

---

## 7. Restart reconciliation (LIVE)

On startup in `LIVE` mode, before the monitor acts:

1. Fetch `get_positions()` (source of truth for what we actually hold).
2. For each restored in-memory `MonitoredPosition`, reconcile `executed_qty` /
   `avg_buy_price` against the exchange; drop/close positions the exchange does
   not show.
3. Verify each stored `sl_order_id` / `target_order_id` still exists (via order
   state); re-place missing protective orders or cancel orphans.

This prevents orphaned or duplicated live orders after a crash/redeploy.

---

## 8. Phased build

Each phase ends with `cargo build` (backend) and, where relevant,
`pnpm run build` (frontend), per [AGENTS.md](../AGENTS.md). Phases 1–2 add code without
changing runtime behaviour.

### Phase 1 — Kotak client primitives (additive, no behaviour change)
Files: [backend/kotak_client/src/client.rs](../backend/kotak_client/src/client.rs), [backend/kotak_client/src/lib.rs](../backend/kotak_client/src/lib.rs).
- `modify_order(&self, order: &OrderRequest, order_no: &str) -> Result<ExecutionResult, KotakError>` → `POST /quick/order/vr/modify`.
- `cancel_order(&self, order_no: &str, trading_symbol: Option<&str>) -> Result<ExecutionResult, KotakError>` → `POST /quick/order/cancel`.
- `get_positions(&self) -> Result<Vec<KotakPosition>, KotakError>` → `GET /quick/user/positions`.
- New public `KotakPosition` struct (parsed fields + `avg_buy_price()` /
  `avg_sell_price()` / `net_qty()` helpers).
- Reuse existing `KotakOrderResponse`, `KotakError`, header/session conventions.
- **Verify**: `cargo build -p kotak_client`. Nothing calls these yet.

### Phase 2 — Domain & schema plumbing (additive)
Files: [backend/shared_domain/src/lib.rs](../backend/shared_domain/src/lib.rs), [backend/server/src/db.rs](../backend/server/src/db.rs).
- Add the three `*_order_id` fields to `MonitoredPosition`.
- Add `mode` column migration + include `mode` in the paper-trade insert /
  `DbWriteMessage`.
- Add `entry_market_protection` to `TradingConfig` (default; settings load/save).
- **Verify**: `cargo build`; existing paper flow unchanged.

### Phase 3 — LIVE entry path
Files: [backend/trading_engine/src/monitor.rs](../backend/trading_engine/src/monitor.rs), [backend/server/src/main.rs](../backend/server/src/main.rs).
- Pass the `Arc<Mutex<Option<KotakClient>>>` into `start_position_monitor`.
- In `LIVE` mode: place the native entry order (SL-M buy for `ABOVE`, Limit buy
  for `BELOW`) with the entry `mp` guard; detect fill (poll `get_positions`);
  transition to `Active` with the real fill price.
- Ask user for the entry `mp` value + confirm unit with a tiny test order.
- **Verify**: `cargo build`; paper mode unchanged; live entry dry-run.

### Phase 4 — LIVE native OCO exits + trailing
File: [backend/trading_engine/src/monitor.rs](../backend/trading_engine/src/monitor.rs).
- On entry fill, place **SL-M sell** (trigger = stop_loss, `mp = 0`) + **Limit
  sell** (target₁).
- Target₁ fill (partial TGT1): `modify_order` SL-M qty down, trail trigger,
  place TGT2 limit. Stop-loss fill: `cancel_order` the target. OCO invariant:
  exactly one protective + one target live per open qty.
- **Verify**: `cargo build`; live OCO dry-run.

### Phase 5 — LIVE expiry square-off @ 15:10
File: [backend/trading_engine/src/monitor.rs](../backend/trading_engine/src/monitor.rs).
- At the existing 15:10 IST cutoff on expiry day: `cancel_order` the resting
  SL-M + target, then **MKT sell** the remaining qty (`mp = 0`).
- **Verify**: `cargo build`.

### Phase 6 — Startup reconciliation
File: [backend/server/src/main.rs](../backend/server/src/main.rs) (+ helper in monitor/client).
- Implement §7. **Verify**: `cargo build`; restart with an open live position.

### Phase 7 — Manual close (LIVE)
File: [backend/server/src/routes/positions.rs](../backend/server/src/routes/positions.rs).
- `close_position_handler`: in `LIVE`, cancel resting exits + place a live MKT
  sell instead of simulating. **Verify**: `cargo build`.

### Phase 8 — Recording & reports
Files: [backend/server/src/db.rs](../backend/server/src/db.rs), [backend/server/src/routes/portfolio.rs](../backend/server/src/routes/portfolio.rs), [frontend/src/App.tsx](../frontend/src/App.tsx).
- Persist real fill price + estimated charges + `mode`; surface `mode` in
  reports. **Verify**: `cargo build` + `pnpm run build`.

---

## 9. Edge cases & risks

- **Order rejection** (insufficient margin, price-band, freeze qty): surface an
  error log + mark the position; do not silently retry into a loop.
- **Partial fills**: reconcile `executed_qty` from Positions; size protective
  orders to the actually-held qty.
- **NSE freeze quantity**: large orders may need splitting; note but low
  priority for current lot sizes.
- **`mp` unit unknown**: confirm with one tiny live order before trusting it.
- **Soft vs native SL**: native SL-M survives a server/websocket outage (chosen);
  still depends on the exchange accepting the trigger.
- **Idempotency on restart**: never re-place an order that already exists (see
  §7 reconciliation).
- **Server CWD contract**: binary must run from `backend/` (paths
  `../frontend/dist`, `../kotak-bridge`).

---

## 10. Open items to confirm

- Entry `mp` guard value + unit (Phase 3, with a tiny live test order).
- Whether a push-based Neo order feed (`hsi`) is available (else keep polling).
- `BELOW`-entry as Limit buy vs stop — confirm against real signals.
- Modify payload `rt` vs `vd` — confirm broker acceptance.
