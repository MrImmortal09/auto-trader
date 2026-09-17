use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use kotak_client::KotakClient;
use shared_domain::{
    DbWriteMessage, MonitoredPosition, TradeSignal, TradeState, TradingConfig,
};
use tokio::sync::{broadcast, mpsc, RwLock};

use crate::fees::{ChargeBreakdown, FeeCalculator};
use serde_json::json;

// ---------------------------------------------------------------------------
// Internal action types — collected in the read pass, applied after
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum PosAction {
    EntryBuy { qty: i32 },
    ExitSell {
        qty: i32,
        reason: String,
        new_sl: Option<f64>,
        exec_price: Option<f64>,
    },
    Cancel { reason: String },
    /// Drop a position that is still waiting for its entry and never will take
    /// one (signal cancelled/changed, or the end-of-day entry cutoff).
    Expire { reason: String },
}

struct Pending {
    idx: usize,
    ltp: f64,
    action: PosAction,
}

// ---------------------------------------------------------------------------
// DB helpers
// ---------------------------------------------------------------------------

async fn send_trade(
    tx: &mpsc::Sender<DbWriteMessage>,
    ticker: &str,
    action: &str,
    qty: i32,
    price: f64,
    c: &ChargeBreakdown,
    signal_id: Option<String>,
    raw_message: Option<String>,
    exit_reason: Option<String>,
    mode: &str,
) {
    let _ = tx.send(DbWriteMessage::Trade {
        ticker: ticker.to_owned(), action: action.to_owned(), qty,
        executed_price: price,
        gross_value: c.gross_value, brokerage: c.brokerage,
        stt_charge: c.stt_charge, sebi_fee: c.sebi_fee,
        stamp_duty: c.stamp_duty, transaction_charge: c.transaction_charge,
        gst: c.gst, net_value: c.net_value,
        signal_id, raw_message, exit_reason,
        mode: mode.to_owned(),
    }).await;
}

async fn send_log(tx: &mpsc::Sender<DbWriteMessage>, log_tx: &broadcast::Sender<String>, level: &'static str, msg: &str) {
    let _ = tx.send(DbWriteMessage::Log {
        level: level.to_owned(),
        message: msg.to_owned(),
    }).await;
    let _ = log_tx.send(msg.to_owned());
}

async fn send_positions_snapshot(
    tx: &mpsc::Sender<DbWriteMessage>,
    positions: &[MonitoredPosition],
) {
    if let Ok(json) = serde_json::to_string(positions) {
        let _ = tx.send(DbWriteMessage::PositionsSnapshot { json }).await;
    }
}

/// True when `now` is at/after 15:38 IST on this position's *resolved*
/// expiry day — two minutes ahead of the 15:40 close, since this account
/// trades options only and nothing is ever meant to carry into settlement.
///
/// Uses `resolved_expiry` (the Scrip Master record `resolved_order` was
/// built from), not `signal.expiry` — the raw parsed field is `None`
/// whenever the incoming message didn't state a date, which `resolve_signal`
/// then defaults to the nearest upcoming expiry; `resolved_expiry` is what
/// that resolution actually landed on. Returns `false` — never due — for a
/// position with no recorded `resolved_expiry` (e.g. adopted from the
/// broker without a Scrip Master lookup) rather than guessing at one.
///
/// Takes `now` rather than calling `shared_domain::now_ist()` itself, the
/// same way `stale_entry_reason` takes a precomputed `entry_cutoff` — so
/// this stays a pure, directly testable function.
fn is_expiry_squareoff_due(pos: &MonitoredPosition, now: chrono::DateTime<chrono::FixedOffset>) -> bool {
    use chrono::Timelike;
    let Some(expiry) = pos.resolved_expiry else { return false; };
    now.date_naive() == expiry
        && (now.hour() > shared_domain::EXPIRY_SQUAREOFF_HOUR
            || (now.hour() == shared_domain::EXPIRY_SQUAREOFF_HOUR
                && now.minute() >= shared_domain::EXPIRY_SQUAREOFF_MINUTE))
}

/// The open/waiting position, if any, that already matches `signal` on
/// contract (instrument/strike/option_type/expiry) *and* entry
/// condition/price. Two different Telegram messages (so the signal_id match
/// in `start_position_monitor` never catches it) can carry the identical
/// call — e.g. a channel resending it with a "BTST" tag — and each used to
/// become its own tracked position while the broker netted both into one
/// line. When their exits later fired together, whichever leg lost the race
/// asked to sell qty the broker no longer had. Matching on entry price/
/// condition too (not just the contract) still lets a genuinely revised call
/// on the same strike through as a new position — only an exact repeat of
/// the contract *and* entry matches here.
///
/// Known gap: this compares the raw parsed fields, not the resolved
/// contract, so two resends that differ only in how the expiry was written
/// (e.g. one omits it and resolves to the nearest expiry, the other spells
/// it out) are not caught. Fixing that needs the expiry resolved before this
/// check runs, which is a bigger reordering left for later — under-matching
/// here is the safer failure mode (a possible duplicate position, which the
/// startup reconciler already flags) versus over-matching and silently
/// refusing a real second trade.
fn find_duplicate_open_position<'a>(
    positions: &'a [MonitoredPosition],
    signal: &TradeSignal,
) -> Option<&'a MonitoredPosition> {
    positions.iter().find(|p| {
        !matches!(p.state, TradeState::Closed)
            && p.signal.instrument_name.eq_ignore_ascii_case(&signal.instrument_name)
            && p.signal.strike == signal.strike
            && p.signal.option_type == signal.option_type
            && p.signal.expiry == signal.expiry
            && p.signal.entry_condition == signal.entry_condition
            && (p.signal.entry_price - signal.entry_price).abs() < f64::EPSILON
    })
}


/// Lots to buy for `instrument_name`: the per-index override if one is set
/// (see `TradingConfig::index_lots_by_symbol`), else `index_lots` for a known
/// index, else `other_lots` for anything else (stock options).
///
/// `0` is a real, meaningful value here — it means "don't auto-trade this
/// class". A bare `index_lots`/`other_lots` of 0 skips every index without an
/// explicit per-symbol override / every stock option; a signal that resolves
/// to 0 is dropped at ingestion (`start_position_monitor`).
fn lots_for_instrument(cfg: &TradingConfig, instrument_name: &str) -> i32 {
    let inst = instrument_name.to_uppercase();
    match shared_domain::INDEX_NAMES.iter().find(|&&idx| inst == idx) {
        Some(&idx) => cfg
            .index_lots_by_symbol
            .get(idx)
            .copied()
            .filter(|&l| l > 0)
            .unwrap_or(cfg.index_lots)
            .max(0),
        None => cfg.other_lots.max(0),
    }
}

/// Compute the entry quantity for a new BUY position (lots × lot size, or a
/// notional-capped multiple for equity). Mirrors the Pass-1 sizing so a native
/// LIVE order is placed for the same qty the paper path would use.
fn compute_entry_qty(
    signal: &TradeSignal,
    lot_size: i32,
    override_qty: Option<i32>,
    cfg: &TradingConfig,
    ltp: Option<f64>,
) -> i32 {
    let lot_size = lot_size.max(1);
    if let Some(override_lots) = override_qty {
        return override_lots * lot_size;
    }
    if signal.option_type.is_some() {
        lots_for_instrument(cfg, &signal.instrument_name) * lot_size
    } else {
        let ltp = ltp.unwrap_or(0.0);
        if ltp <= 0.0 {
            return lot_size;
        }
        let raw_qty = ((cfg.max_trade_amount_inr / ltp).floor() as i32).max(1);
        let mut multiple = (raw_qty / lot_size) * lot_size;
        if multiple == 0 { multiple = lot_size; }
        multiple
    }
}

/// Target-1 level when `ltp` is already at or through it — the move the signal
/// was called on has already happened, so opening now would just book the
/// target on the next tick. `None` when there is still room to target 1, or the
/// signal carries no target at all.
///
/// Both entry paths (PAPER Pass-1 and LIVE `decide_live`) call this at the
/// moment the entry trigger fires and refuse the entry when it returns `Some`.
/// The usual cause is a mis-resolved contract — wrong expiry or strike, so the
/// stream is a different, richer option — or a signal that reached the engine
/// late; either way there is no trade left to make.
fn entry_past_target1(signal: &TradeSignal, ltp: f64) -> Option<f64> {
    let t1 = *signal.targets.first()?;
    (ltp >= t1).then_some(t1)
}

/// The premium window the engine will actually buy `signal` in — `None` when
/// `entry_window_pct` is `0` (the classic "buy as soon as the trigger is
/// crossed" behaviour, which has no far edge to draw).
///
/// With `entry_window_pct > 0` the window is `w = pct/100 * (target1 - entry)`
/// wide (falling back to `pct/100 * entry` when the signal has no usable
/// target-1), sitting on the buy side of the trigger:
///
/// * `ABOVE X` → `[X, X + w]` — take the breakout, never chase past `X + w`
/// * `BELOW X` → `[X - w, X]` — take the breakdown, never chase past `X - w`
///
/// `pct` is clamped to `[0, 100]` on save, so `X + w` never exceeds target 1.
pub fn entry_buy_window(signal: &TradeSignal, cfg: &TradingConfig) -> Option<(f64, f64)> {
    if cfg.entry_window_pct <= 0.0 {
        return None;
    }
    let cond = signal.entry_condition.to_uppercase();
    if cond != "ABOVE" && cond != "BELOW" {
        return None;
    }
    let x = signal.entry_price;
    let base = match signal.targets.first() {
        Some(&t1) if (t1 - x).abs() > 1e-9 => (t1 - x).abs(),
        _ => x.abs(),
    };
    let w = (cfg.entry_window_pct / 100.0) * base;
    Some(if cond == "BELOW" { (x - w, x) } else { (x, x + w) })
}

/// Whether `ltp` is where the engine should buy `signal` right now.
///
/// With `entry_window_pct = 0` this is the classic one-sided check
/// (`ltp >= entry` for `ABOVE`, `ltp <= entry` for `BELOW`); with a window
/// configured it also refuses a price that has run past the far edge — that
/// position keeps waiting for a pull-back. Unknown entry conditions never fire.
fn entry_triggered(signal: &TradeSignal, ltp: f64, cfg: &TradingConfig) -> bool {
    match entry_buy_window(signal, cfg) {
        Some((low, high)) => ltp >= low && ltp <= high,
        None => match signal.entry_condition.to_uppercase().as_str() {
            "ABOVE" => ltp >= signal.entry_price,
            "BELOW" => ltp <= signal.entry_price,
            _ => false,
        },
    }
}

/// Pre-T1 trailing stop (see `TradingConfig::pre_t1_trailing`): feed one LTP
/// observation into `peak_ltp` and, once the peak has covered
/// `pre_t1_trail_arm_pct` % of the entry→target-1 distance, set
/// `current_sl` to `peak - diff * pre_t1_trail_factor` (tick-rounded).
///
/// The SL is allowed to move **both up and down** so that a runtime change to
/// `pre_t1_trail_factor` or `pre_t1_trail_arm_pct` takes effect immediately on
/// the next tick. The only hard floor is the signal's original `stop_loss` —
/// the dynamic level can never go below what the signal itself asked for.
/// Returns `Some(new_sl)` when the stop actually moved. Shared by the PAPER
/// and LIVE paths — protection is a pure software watch in both, so moving
/// the stop is bookkeeping, not an order.
fn pre_t1_trail_update(
    pos: &mut MonitoredPosition,
    ltp: f64,
    cfg: &TradingConfig,
) -> Option<f64> {
    if !cfg.pre_t1_trailing || !matches!(pos.state, TradeState::Active) {
        return None;
    }
    let t1 = *pos.signal.targets.first()?;
    let diff = t1 - pos.avg_buy_price;
    if diff <= 0.0 {
        return None;
    }
    let peak = pos.peak_ltp.map_or(ltp, |p| p.max(ltp));
    pos.peak_ltp = Some(peak);
    if peak < pos.avg_buy_price + diff * cfg.pre_t1_trail_arm_pct / 100.0 {
        return None;
    }
    // Clamp to the signal's original SL as the absolute floor — config
    // changes can loosen the trail but can never expose more than the
    // original stop-loss risk.
    let desired = round_down_tick(
        (peak - diff * cfg.pre_t1_trail_factor).max(pos.signal.stop_loss),
        pos.tick_size,
    );
    if (desired - pos.current_sl).abs() > pos.tick_size * 0.5 {
        pos.current_sl = desired;
        return Some(desired);
    }
    None
}

// ---------------------------------------------------------------------------
// LIVE mode — price safety, sizing, order construction
// ---------------------------------------------------------------------------

/// Order-book poll cadence. Accurate settlement matters more here than reaction
/// speed (the protective stop lives at the broker), so this sits comfortably
/// inside the agreed 5 s ceiling.
const LIVE_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Consecutive failed exit attempts after which the engine stops acting on a
/// position by itself and asks for manual intervention, rather than hammering a
/// rejecting broker (plan §9 — a failure is logged and marked, never retried
/// forever).
const MAX_EXIT_ATTEMPTS: i32 = 3;

/// Cash held back on every entry. The account must show the order value **plus**
/// this much before we buy — ₹10,000 of options needs ₹10,500 available.
///
/// It also absorbs the slippage a market order with `mp` protection can incur,
/// which is why the check is made against the LTP rather than a padded price.
const FUNDS_BUFFER_INR: f64 = 500.0;

/// IST time from which no new entry is taken and every position still waiting
/// for its trigger is dropped — the last minute before the 15:40 close is left
/// alone so a fresh position is never opened as the session ends.
const NO_ENTRY_HOUR: u32 = 15;
const NO_ENTRY_MINUTE: u32 = 39;

/// Attempts made against `get_limits` in a single entry pre-flight before
/// giving up on *this* tick. A transient blip usually clears within a retry or
/// two; if it doesn't, the position stays `WaitingForEntry` and the whole
/// pre-flight is retried on a later tick — nothing is bought and nothing is
/// dropped.
const LIMITS_PREFLIGHT_ATTEMPTS: u32 = 3;
const LIMITS_PREFLIGHT_RETRY_GAP: Duration = Duration::from_millis(400);

/// Minimum spacing between entry pre-flight attempts for one position. Once the
/// trigger has fired the engine wants in promptly, but not at the 50 ms tick
/// rate — this keeps a broker outage from spamming the log with one
/// "could not verify funds" line every tick.
const ENTRY_RETRY_THROTTLE: Duration = Duration::from_secs(5);

/// How long the reconciler keeps hunting the order book for an entry whose send
/// outcome is unknown (`entry_send_uncertain`) before concluding — only ever on
/// a tick where the order-book read itself succeeded — that the order never
/// reached the broker. Comfortably longer than the time Kotak takes to surface
/// a just-placed order in the book, short enough that a genuinely-lost send is
/// retried well before it matters.
const UNCERTAIN_ENTRY_GRACE: Duration = Duration::from_secs(20);

/// True at/after 15:39 IST, for the remainder of the day.
fn is_entry_cutoff_passed() -> bool {
    use chrono::Timelike;
    let now = shared_domain::now_ist();
    let (h, m) = (now.hour(), now.minute());
    h > NO_ENTRY_HOUR || (h == NO_ENTRY_HOUR && m >= NO_ENTRY_MINUTE)
}

/// Why a `WaitingForEntry` position should be abandoned rather than left to
/// watch for its trigger, or `None` if it is still live.
///
/// `entry_cutoff` (today's 15:39 IST bell) only catches a position while the
/// engine is running continuously through that moment. A crash, redeploy, or
/// manual restart that happens to fall before 15:39 lets a never-triggered
/// position survive in the DB as `WaitingForEntry` with nothing to expire it;
/// reloaded on the next day's startup, `entry_cutoff` is false again (it's
/// morning), so it sits watching the LTP feed and can trigger a real entry
/// the moment today's price happens to cross a reference level from a stale
/// — possibly days-old — signal. Comparing `created_at` against today's IST
/// date catches that case too. Positions predating this field (`created_at`
/// empty/unparseable) are treated as stale as well: erring toward not
/// trading rather than guessing their age.
fn stale_entry_reason(pos: &MonitoredPosition, entry_cutoff: bool) -> Option<&'static str> {
    if entry_cutoff {
        return Some("EOD_NO_ENTRY");
    }
    let same_day = chrono::NaiveDateTime::parse_from_str(&pos.created_at, "%Y-%m-%d %H:%M:%S")
        .is_ok_and(|dt| dt.date() == shared_domain::today_ist());
    (!same_day).then_some("STALE_CARRYOVER")
}

/// Whole seconds elapsed since an IST `%Y-%m-%d %H:%M:%S` timestamp, or `None`
/// when the string is missing or unparseable. Negative values (a clock that
/// went backwards) are clamped to 0.
fn secs_since_ist(ts: &Option<String>) -> Option<i64> {
    let parsed = chrono::NaiveDateTime::parse_from_str(ts.as_ref()?, "%Y-%m-%d %H:%M:%S").ok()?;
    Some((shared_domain::now_ist().naive_local() - parsed).num_seconds().max(0))
}

/// Round `price` DOWN to a whole multiple of `tick`.
///
/// Every price the engine sends to the broker goes through this. The exchange
/// rejects orders that are off the tick grid, and rounding down is the agreed
/// default direction for all of them — stops, targets and trailed stops alike.
pub fn round_down_tick(price: f64, tick: f64) -> f64 {
    if !price.is_finite() || tick <= 0.0 {
        return price;
    }
    // The epsilon stops an exact multiple (0.15 / 0.05 = 2.9999999999999996 in
    // binary floating point) from being floored a whole tick too far.
    let steps = (price / tick + 1e-9).floor();
    (steps * tick * 100.0).round() / 100.0
}

/// Lot size for a position. `resolved_order` is kept as a pristine one-lot
/// template, so its quantity *is* the lot size.
fn lot_size_of(pos: &MonitoredPosition) -> i32 {
    pos.resolved_order
        .as_ref()
        .and_then(|o| o.quantity.parse::<i32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(1)
}

/// Quantity to release at target 1: `pct` of the holding, rounded **up** to a
/// whole lot and capped at what we hold.
///
/// Exits have to be whole lots, and the agreed rule is to take the larger slice
/// at target 1 — 3 lots at 50% sells 2 and runs 1 to target 2. A single-lot
/// position therefore exits in full at target 1.
fn tgt1_slice_qty(held: i32, lot_size: i32, pct: f64) -> i32 {
    let lot = lot_size.max(1);
    if held <= lot {
        return held;
    }
    let raw = held as f64 * pct / 100.0;
    let lots = ((raw / lot as f64).ceil() as i32).max(1);
    (lots * lot).min(held)
}

/// Market order (`MKT`). `mp` is Kotak's market-price-protection *percentage*.
fn build_market_order(
    base: &shared_domain::OrderRequest,
    txn: shared_domain::TransactionType,
    qty: i32,
    mp: f64,
) -> shared_domain::OrderRequest {
    use shared_domain::OrderType;
    let mut order = base.clone();
    order.quantity = qty.to_string();
    order.transaction_type = txn;
    order.order_type = OrderType::Market;
    order.price = "0".to_string();
    order.trigger_price = "0".to_string();
    order.market_protection = mp.to_string();
    order
}

/// IOC limit order (`L`, validity `IOC`) at `price`. Used as the fallback when
/// the exchange RMS refuses a plain market order — which it does for the first
/// few minutes after the 09:15 open. `price` must already be tick-rounded.
fn build_ioc_limit_order(
    base: &shared_domain::OrderRequest,
    txn: shared_domain::TransactionType,
    qty: i32,
    price: f64,
) -> shared_domain::OrderRequest {
    use shared_domain::{OrderType, Validity};
    let mut order = base.clone();
    order.quantity = qty.to_string();
    order.transaction_type = txn;
    order.order_type = OrderType::Limit;
    order.price = format!("{price:.2}");
    order.trigger_price = "0".to_string();
    order.market_protection = "0".to_string();
    order.validity = Validity::Ioc;
    order
}

/// The IOC-limit price for an entry retry: a protection band above the touch,
/// never above the entry buy window's far edge (when one is configured), tick-
/// rounded down. `None` if there is no usable reference price.
fn entry_retry_limit_price(
    signal: &shared_domain::TradeSignal,
    cfg: &TradingConfig,
    ltp: f64,
    tick: f64,
) -> Option<f64> {
    if ltp <= 0.0 {
        return None;
    }
    let mut cap = ltp * (1.0 + cfg.entry_market_protection.max(0.0) / 100.0);
    if let Some((_, high)) = entry_buy_window(signal, cfg) {
        cap = cap.min(high);
    }
    let price = round_down_tick(cap, tick);
    (price > 0.0).then_some(price)
}

// ---------------------------------------------------------------------------
// LIVE mode — broker call wrappers
//
// Each takes the Kotak mutex only for the duration of the call. The positions
// lock is never held across any of these.
// ---------------------------------------------------------------------------

type KotakHandle = Arc<tokio::sync::Mutex<Option<KotakClient>>>;

/// Places an order and returns the broker error unflattened, so the entry path
/// can tell a hard rejection (`OrderRejected`) apart from a send that never got
/// a verdict (`is_ambiguous`) or a dead session (`is_session_expired`).
async fn kotak_place(
    kotak: &KotakHandle,
    order: &shared_domain::OrderRequest,
) -> Result<String, kotak_client::KotakError> {
    let guard = kotak.lock().await;
    let client = guard.as_ref().ok_or(kotak_client::KotakError::NotAuthenticated)?;
    client.place_live_order(order).await.map(|e| e.order_id)
}

async fn kotak_limits(
    kotak: &KotakHandle,
) -> Result<kotak_client::KotakLimits, kotak_client::KotakError> {
    let guard = kotak.lock().await;
    let client = guard.as_ref().ok_or(kotak_client::KotakError::NotAuthenticated)?;
    client.get_limits().await
}

async fn kotak_cancel(kotak: &KotakHandle, order_no: &str, trading_symbol: &str) -> Result<(), String> {
    let guard = kotak.lock().await;
    let client = guard.as_ref().ok_or_else(|| "no Kotak session".to_string())?;
    let ts = (!trading_symbol.is_empty()).then_some(trading_symbol);
    client.cancel_order(order_no, ts).await.map(|_| ()).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// LIVE mode — logging & state helpers
// ---------------------------------------------------------------------------

/// Anything that goes wrong on a real-money order is surfaced twice: once in the
/// tracing log and once as an ERROR row the UI shows.
async fn loud_error(
    db_tx: &mpsc::Sender<DbWriteMessage>,
    log_tx: &broadcast::Sender<String>,
    instrument: &str,
    message: &str,
) {
    tracing::error!(instrument = %instrument, "LIVE: {message}");
    let payload = json!({
        "event": "LIVE_ERROR",
        "instrument": instrument,
        "message": message,
    });
    send_log(db_tx, log_tx, "ERROR", &payload.to_string()).await;
}

async fn live_info(
    db_tx: &mpsc::Sender<DbWriteMessage>,
    log_tx: &broadcast::Sender<String>,
    payload: serde_json::Value,
) {
    send_log(db_tx, log_tx, "INFO", &payload.to_string()).await;
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Mutate one position by id under a short-lived write lock.
async fn with_position<F>(positions: &Arc<RwLock<Vec<MonitoredPosition>>>, id: &str, f: F)
where
    F: FnOnce(&mut MonitoredPosition),
{
    let mut g = positions.write().await;
    if let Some(p) = g.iter_mut().find(|p| p.id == id) {
        f(p);
    }
}

/// Clear all record of the resting stop after it has been cancelled or filled.
fn forget_stop(p: &mut MonitoredPosition) {
    p.sl_order_id = None;
    p.sl_order_qty = 0;
    p.sl_order_trigger = 0.0;
}

/// Count a failed exit and halt the position once the cap is reached.
async fn bump_exit_attempts(
    positions: &Arc<RwLock<Vec<MonitoredPosition>>>,
    db_tx: &mpsc::Sender<DbWriteMessage>,
    log_tx: &broadcast::Sender<String>,
    pos_id: &str,
    instrument: &str,
) {
    let mut halted = false;
    {
        let mut g = positions.write().await;
        if let Some(p) = g.iter_mut().find(|p| p.id == pos_id) {
            p.exit_attempts += 1;
            if p.exit_attempts >= MAX_EXIT_ATTEMPTS {
                p.live_halt = Some(format!("exit failed {} times", p.exit_attempts));
                p.force_exit = None;
                halted = true;
            }
        }
    }
    if halted {
        loud_error(db_tx, log_tx, instrument, &format!(
            "giving up after {MAX_EXIT_ATTEMPTS} failed exit attempts — no further orders will be sent for this position, square it off manually"
        )).await;
    }
}

/// How much of `qty` this position can safely ask the broker to sell right
/// now, given what *every other* tracked position on the same broker symbol
/// has already committed to selling.
///
/// More than one tracked position can map to the same broker trading symbol
/// (two signals on one contract). Each position's own resting stop
/// (`sl_order_qty`) and in-flight exit (`pending_exit_qty`) are already
/// tracked in memory and kept current by `reconcile_live_orders`, which runs
/// immediately before every exit decision in the same tick — so summing them
/// here enforces "resting stop qty + in-flight sell qty <= executed_qty"
/// across sibling legs without an extra broker call on the hot exit path,
/// and without depending on a Positions endpoint that has failed on every
/// call for this account before (2026-08-17). Pass 2 of `live_tick` applies
/// exits one at a time, so a sibling leg's exit that goes out first is
/// already reflected here before this one is computed.
///
/// This is a cap on the engine's *own* bookkeeping, not broker truth — it
/// cannot see a quantity sold manually outside the engine that hasn't been
/// reconciled yet. That gap is closed by the periodic broker reconciliation,
/// with the broker's own RMS rejection as the final backstop.
async fn sellable_qty(
    positions: &Arc<RwLock<Vec<MonitoredPosition>>>,
    pos_id: &str,
    trading_symbol: &str,
    qty: i32,
) -> i32 {
    let g = positions.read().await;
    let mut tracked_total = 0i32;
    let mut committed_elsewhere = 0i32;
    for p in g.iter() {
        if matches!(p.state, TradeState::Closed) {
            continue;
        }
        let same_symbol = p
            .resolved_order
            .as_ref()
            .map(|o| o.trading_symbol.trim() == trading_symbol.trim())
            .unwrap_or(false);
        if !same_symbol {
            continue;
        }
        tracked_total += p.executed_qty;
        if p.id != pos_id {
            committed_elsewhere += p.sl_order_qty + p.pending_exit_qty;
        }
    }
    (tracked_total - committed_elsewhere).max(0).min(qty)
}

/// Record a real LIVE sell fill as a trade + log line.
#[allow(clippy::too_many_arguments)]
async fn record_live_sell(
    db_tx: &mpsc::Sender<DbWriteMessage>,
    log_tx: &broadcast::Sender<String>,
    leg: &LiveLeg,
    qty: i32,
    price: f64,
    reason: &str,
    brokerage: f64,
) {
    let fees = FeeCalculator::calculate(qty, price, "SELL", leg.is_options, brokerage);
    let pnl = fees.net_value - leg.avg_buy_price * qty as f64;
    tracing::info!(instrument = %leg.instrument, reason, price, qty, pnl, "LIVE exit filled");
    send_trade(
        db_tx, &leg.instrument, "SELL", qty, price, &fees,
        leg.signal_id.clone(), leg.raw_message.clone(), Some(reason.to_string()), "LIVE",
    ).await;
    live_info(db_tx, log_tx, json!({
        "event": reason,
        "instrument": leg.instrument,
        "price": round2(price),
        "qty": qty,
        "pnl": round2(pnl),
        "mode": "LIVE",
    })).await;
}

// ---------------------------------------------------------------------------
// LIVE mode — order-book reconciliation
// ---------------------------------------------------------------------------

/// Everything the reconciler needs about one position, snapshotted so the
/// positions lock can be released before the broker is called.
struct LiveLeg {
    pos_id: String,
    instrument: String,
    trading_symbol: String,
    is_options: bool,
    signal_id: Option<String>,
    raw_message: Option<String>,
    avg_buy_price: f64,
    entry_price: f64,
    at_target1: bool,
    entry_order_id: Option<String>,
    entry_cancel_sent: bool,
    /// An entry order was sent for this position but never acknowledged — the
    /// reconciler must find it in the book (or prove it isn't there). Only ever
    /// set while `entry_order_id` is `None`.
    entry_send_uncertain: bool,
    entry_uncertain_qty: Option<i32>,
    /// Whole seconds since that send attempt, for the grace-window timeout.
    entry_attempt_secs: Option<i64>,
    /// The entry is being retried as an IOC limit (a prior market send was
    /// RMS-rejected). An IOC limit that comes back cancelled with nothing
    /// filled is a miss to retry, not a signal to drop.
    entry_retry_as_limit: bool,
    sl_order_id: Option<String>,
    pending_exit_order_id: Option<String>,
    pending_exit_qty: i32,
    pending_exit_reason: Option<String>,
}

/// Settle every tracked order against the broker's order book.
///
/// This is the only place LIVE fills are believed: quantities and prices come
/// from `fldQty`/`avgPrc`, never from our own LTP. Returns `true` if any
/// position changed.
async fn reconcile_live_orders(
    positions: &Arc<RwLock<Vec<MonitoredPosition>>>,
    kotak: &KotakHandle,
    db_tx: &mpsc::Sender<DbWriteMessage>,
    log_tx: &broadcast::Sender<String>,
    brokerage: f64,
) -> bool {
    let (legs, claimed_entry_ids): (Vec<LiveLeg>, Vec<String>) = {
        let g = positions.read().await;
        let legs = g.iter()
            .filter(|p| {
                p.entry_order_id.is_some()
                    || p.entry_send_uncertain
                    || p.sl_order_id.is_some()
                    || p.pending_exit_order_id.is_some()
            })
            .map(|p| LiveLeg {
                pos_id: p.id.clone(),
                instrument: p.signal.instrument_name.clone(),
                trading_symbol: p
                    .resolved_order
                    .as_ref()
                    .map(|o| o.trading_symbol.clone())
                    .unwrap_or_default(),
                is_options: p.signal.option_type.is_some(),
                signal_id: p.signal.signal_id.clone(),
                raw_message: p.signal.raw_message.clone(),
                avg_buy_price: p.avg_buy_price,
                entry_price: p.signal.entry_price,
                at_target1: matches!(p.state, TradeState::Target1Hit),
                entry_order_id: p.entry_order_id.clone(),
                entry_cancel_sent: p.entry_cancel_sent,
                entry_send_uncertain: p.entry_send_uncertain && p.entry_order_id.is_none(),
                entry_uncertain_qty: p.entry_uncertain_qty,
                entry_attempt_secs: secs_since_ist(&p.entry_attempt_at),
                entry_retry_as_limit: p.entry_retry_as_limit,
                sl_order_id: p.sl_order_id.clone(),
                pending_exit_order_id: p.pending_exit_order_id.clone(),
                pending_exit_qty: p.pending_exit_qty,
                pending_exit_reason: p.pending_exit_reason.clone(),
            })
            .collect();
        // Every entry order number the engine already owns — the uncertain-entry
        // hunt below must not adopt an order that belongs to another position.
        let claimed = g.iter()
            .filter_map(|p| p.entry_order_id.clone())
            .filter(|s| !s.trim().is_empty())
            .collect();
        (legs, claimed)
    };
    if legs.is_empty() {
        return false;
    }

    let book = {
        let g = kotak.lock().await;
        match g.as_ref() {
            Some(c) => c.get_order_book().await,
            None => return false,
        }
    };
    let book = match book {
        Ok(b) => b,
        Err(e) => {
            // Transient: the next poll retries. Not loud — a flaky poll must not
            // drown out the errors that need a human.
            tracing::warn!("LIVE order-book poll failed: {e}");
            return false;
        }
    };

    let mut mutated = false;

    for leg in &legs {
        // ── Unconfirmed entry send ──────────────────────────────────── //
        // An entry order went out but its broker response was lost, so it may
        // or may not be live. Find the matching order and adopt it (the entry
        // leg below then settles it like any other), or — only once the grace
        // window has passed on a poll where the book actually read cleanly —
        // conclude nothing was placed and hand the position back to decide_live.
        if leg.entry_send_uncertain {
            let want_qty = leg.entry_uncertain_qty.unwrap_or(0);
            let sym = leg.trading_symbol.trim();
            let matches: Vec<&kotak_client::KotakOrder> = if sym.is_empty() {
                Vec::new()
            } else {
                book.iter()
                    .filter(|o| {
                        !o.is_sell()
                            && o.trading_symbol.trim() == sym
                            && !claimed_entry_ids.iter().any(|c| c.trim() == o.order_no.trim())
                            && (want_qty <= 0 || o.ordered_qty() == want_qty)
                    })
                    .collect()
            };

            match matches.as_slice() {
                [ord] => {
                    let adopted = ord.order_no.trim().to_string();
                    let status = ord.status.trim().to_string();
                    with_position(positions, &leg.pos_id, |p| {
                        p.entry_order_id = Some(adopted.clone());
                        p.entry_send_uncertain = false;
                        p.entry_uncertain_qty = None;
                        p.entry_cancel_sent = false;
                    }).await;
                    loud_error(db_tx, log_tx, &leg.instrument, &format!(
                        "found order {adopted} ({status}) for {sym} in the book and adopted it as this position's entry — its placement had been unconfirmed. It settles on the next poll; verify against your broker app"
                    )).await;
                    mutated = true;
                }
                [] => {
                    let waited = leg.entry_attempt_secs.unwrap_or(0);
                    if waited >= UNCERTAIN_ENTRY_GRACE.as_secs() as i64 {
                        with_position(positions, &leg.pos_id, |p| {
                            p.entry_send_uncertain = false;
                            p.entry_uncertain_qty = None;
                        }).await;
                        live_info(db_tx, log_tx, json!({
                            "event": "ENTRY_SEND_UNCONFIRMED_CLEARED",
                            "instrument": leg.instrument,
                            "note": format!("no matching order in the book {waited}s after an unconfirmed entry send — treating it as never placed"),
                            "mode": "LIVE",
                        })).await;
                        mutated = true;
                    }
                    // else: still inside the grace window — Kotak may not have
                    // surfaced the order yet. Wait for the next poll.
                }
                many => {
                    let n = many.len();
                    with_position(positions, &leg.pos_id, |p| {
                        p.entry_send_uncertain = false;
                        p.entry_uncertain_qty = None;
                        p.live_halt = Some("unconfirmed entry send matched multiple broker orders".to_string());
                    }).await;
                    loud_error(db_tx, log_tx, &leg.instrument, &format!(
                        "an unconfirmed entry send for {sym} matches {n} buy orders in the book — cannot tell which is ours, so this position is halted; resolve it at the broker terminal"
                    )).await;
                    mutated = true;
                }
            }
            continue;
        }

        // ── Entry leg ────────────────────────────────────────────────── //
        if let Some(oid) = leg.entry_order_id.clone() {
            if let Some(ord) = book.iter().find(|o| o.order_no.trim() == oid.trim()) {
                if ord.is_terminal() {
                    let filled = ord.filled_qty();
                    let ordered = ord.ordered_qty();
                    let mut avg = ord.avg_price();
                    if filled > 0 {
                        if avg <= 0.0 {
                            avg = leg.entry_price;
                            loud_error(db_tx, log_tx, &leg.instrument, &format!(
                                "entry order {oid} filled {filled} but the broker reported no average price — booking it at the signal entry {avg:.2}, verify against the contract note"
                            )).await;
                        }
                        with_position(positions, &leg.pos_id, |p| {
                            p.executed_qty = filled;
                            p.avg_buy_price = avg;
                            p.state = TradeState::Active;
                            p.entry_order_id = None;
                            p.entry_cancel_sent = false;
                            p.entry_retry_as_limit = false;
                        }).await;

                        let fees = FeeCalculator::calculate(filled, avg, "BUY", leg.is_options, brokerage);
                        tracing::info!(instrument = %leg.instrument, price = avg, qty = filled, "LIVE entry filled");
                        send_trade(
                            db_tx, &leg.instrument, "BUY", filled, avg, &fees,
                            leg.signal_id.clone(), leg.raw_message.clone(),
                            Some("ENTRY".to_string()), "LIVE",
                        ).await;
                        live_info(db_tx, log_tx, json!({
                            "event": "ENTRY",
                            "instrument": leg.instrument,
                            "price": round2(avg),
                            "qty": filled,
                            "net_cost": round2(fees.net_value),
                            "mode": "LIVE",
                        })).await;

                        if ordered > 0 && filled < ordered {
                            loud_error(db_tx, log_tx, &leg.instrument, &format!(
                                "entry order {oid} filled only {filled} of {ordered} — running the position on the filled quantity"
                            )).await;
                        }
                    } else if leg.entry_retry_as_limit && ord.is_cancelled() {
                        // An IOC limit retry that expired unfilled — a miss, not
                        // a failure. Release the id and let decide_live fire the
                        // next limit once the throttle clears (it re-checks the
                        // trigger / buy window first, so a price that has left
                        // the window simply stops the retries).
                        with_position(positions, &leg.pos_id, |p| {
                            p.entry_order_id = None;
                            p.entry_cancel_sent = false;
                        }).await;
                        tracing::info!(
                            instrument = %leg.instrument, %oid,
                            "LIVE entry IOC limit expired unfilled — will retry"
                        );
                    } else {
                        with_position(positions, &leg.pos_id, |p| {
                            p.state = TradeState::Closed;
                            p.entry_order_id = None;
                            p.entry_retry_as_limit = false;
                        }).await;
                        loud_error(db_tx, log_tx, &leg.instrument, &format!(
                            "entry order {oid} came back {} ({}) with nothing filled — signal dropped",
                            ord.status, ord.reject_reason
                        )).await;
                    }
                    mutated = true;
                } else if ord.filled_qty() > 0 && !leg.entry_cancel_sent {
                    // A market buy that only partly filled: keep what we got and
                    // release the rest instead of chasing the price.
                    if let Err(e) = kotak_cancel(kotak, &oid, &leg.trading_symbol).await {
                        loud_error(db_tx, log_tx, &leg.instrument, &format!(
                            "could not cancel the unfilled remainder of entry order {oid}: {e}"
                        )).await;
                    }
                    with_position(positions, &leg.pos_id, |p| p.entry_cancel_sent = true).await;
                    mutated = true;
                }
            }
        }

        // ── Stop-loss leg ────────────────────────────────────────────── //
        if let Some(oid) = leg.sl_order_id.clone() {
            if let Some(ord) = book.iter().find(|o| o.order_no.trim() == oid.trim()) {
                if ord.is_complete() {
                    let filled = ord.filled_qty();
                    let avg = ord.avg_price();
                    if filled > 0 && avg > 0.0 {
                        let reason = if leg.at_target1 { "TRAIL_SL_HIT" } else { "SL_HIT" };
                        record_live_sell(db_tx, log_tx, leg, filled, avg, reason, brokerage).await;
                    } else {
                        loud_error(db_tx, log_tx, &leg.instrument, &format!(
                            "stop-loss order {oid} reports complete but gave no usable fill data (qty {filled}, avg {avg}) — reconcile the position manually"
                        )).await;
                    }
                    with_position(positions, &leg.pos_id, |p| {
                        p.executed_qty = (p.executed_qty - filled).max(0);
                        forget_stop(p);
                        if p.executed_qty <= 0 && p.pending_exit_order_id.is_none() {
                            p.state = TradeState::Closed;
                            p.force_exit = None;
                        }
                    }).await;
                    mutated = true;
                } else if ord.is_rejected() || ord.is_cancelled() {
                    loud_error(db_tx, log_tx, &leg.instrument, &format!(
                        "stop-loss order {oid} is {} at the broker ({}) — the position is unprotected and a new stop will be placed",
                        ord.status, ord.reject_reason
                    )).await;
                    with_position(positions, &leg.pos_id, forget_stop).await;
                    mutated = true;
                }
            }
        }

        // ── Engine-initiated exit leg ────────────────────────────────── //
        if let (Some(oid), Some(reason)) =
            (leg.pending_exit_order_id.clone(), leg.pending_exit_reason.clone())
        {
            if let Some(ord) = book.iter().find(|o| o.order_no.trim() == oid.trim()) {
                if ord.is_terminal() {
                    let filled = ord.filled_qty();
                    let avg = ord.avg_price();
                    // Whether the broker hard-rejected the order (vs. cancelled /
                    // IOC-expired with nothing filled). Cancels are expected when
                    // we use IOC validity — they are not failures, just misses
                    // that the engine retries on the next tick.
                    let was_cancelled = ord.is_cancelled();

                    if filled > 0 && avg > 0.0 {
                        record_live_sell(db_tx, log_tx, leg, filled, avg, &reason, brokerage).await;
                    } else if was_cancelled {
                        // IOC / manual cancel with 0 fill — not an error. Clear
                        // the pending slot; decide_live will re-detect the SL
                        // breach / target hit naturally on the next tick and
                        // fire a new IOC order without any force_exit needed.
                        live_info(db_tx, log_tx, json!({
                            "event": "EXIT_ORDER_UNFILLED",
                            "instrument": leg.instrument,
                            "order_id": oid,
                            "reason": reason,
                            "note": "cancelled (IOC or manual) with 0 fill — retrying next tick",
                            "mode": "LIVE",
                        })).await;
                    } else {
                        // Hard reject from the exchange / RMS — log loudly and
                        // count against the exit-attempt cap.
                        loud_error(db_tx, log_tx, &leg.instrument, &format!(
                            "exit order {oid} ({reason}) came back {} ({}) with nothing sold",
                            ord.status, ord.reject_reason
                        )).await;
                    }

                    let shortfall = leg.pending_exit_qty - filled;
                    let mut now_closed = false;
                    with_position(positions, &leg.pos_id, |p| {
                        p.executed_qty = (p.executed_qty - filled).max(0);
                        p.pending_exit_order_id = None;
                        p.pending_exit_qty = 0;
                        p.pending_exit_reason = None;
                        if p.executed_qty <= 0 {
                            p.state = TradeState::Closed;
                            p.force_exit = None;
                            now_closed = true;
                        } else if shortfall > 0 && !was_cancelled {
                            // Hard reject with qty still outstanding: force an
                            // explicit retry so decide_live acts even if price has
                            // bounced above the trigger. Keep the reason
                            // recognizable as a shortfall retry (append the
                            // suffix once, not on every retry — `reason` here is
                            // fed back in from a previous retry's own
                            // `force_exit`, so appending unconditionally grew
                            // this without bound, and dropping it entirely made
                            // a rejected TGT1_PARTIAL/MANUAL_SELL retry
                            // indistinguishable from a real one once it turned
                            // into a full-quantity ExitAll in decide_live).
                            // `bump_exit_attempts` below applies the shared
                            // retry cap and halts the position once it's reached.
                            p.force_exit = Some(if reason.ends_with("_SHORTFALL") {
                                reason.clone()
                            } else {
                                format!("{reason}_SHORTFALL")
                            });
                        }
                        // For cancels: no force_exit change — decide_live
                        // re-detects the SL breach / target condition on the
                        // next tick and issues a fresh IOC order automatically.
                    }).await;
                    if shortfall > 0 && !was_cancelled {
                        loud_error(db_tx, log_tx, &leg.instrument, &format!(
                            "exit order {oid} ({reason}) filled {filled} of {} — squaring off the remainder",
                            leg.pending_exit_qty
                        )).await;
                        // Don't count this against the retry cap if the fill
                        // just closed the position outright (executed_qty hit
                        // 0) — there's nothing left to retry, so counting it
                        // could halt an already-closed position on its way out.
                        if !now_closed {
                            bump_exit_attempts(positions, db_tx, log_tx, &leg.pos_id, &leg.instrument).await;
                        }
                    }
                    mutated = true;
                }
            }
        }
    }

    mutated
}

// ---------------------------------------------------------------------------
// LIVE mode — startup reconciliation (plan §7)
// ---------------------------------------------------------------------------

/// Rebuild the engine's view from the broker before it acts on anything.
///
/// After a crash or redeploy the stored snapshot can be stale in three ways that
/// all matter: we may think we hold something we do not, we may hold something
/// with no stop, and **there may already be a stop resting that we have
/// forgotten about**. That last one is the dangerous case — placing a second
/// stop would leave two sell orders against one holding, which in a no-margin
/// account means a naked short as soon as both fill.
///
/// The broker is the source of truth throughout: positions decide what we hold,
/// the order book decides what is resting.
///
/// Returns `true` once the order book was read successfully and every tracked
/// position was reconciled against the broker. Also returns `true` in one
/// degraded case: the positions-read failed but nothing is currently tracked,
/// so there was nothing it could have caught — see `positions_verified` in the
/// emitted `LIVE_STARTUP_RECONCILED` event for whether it actually ran.
/// Returns `false` on any other failure — the caller must not let `live_tick`
/// run against unverified state, and must retry this on the next tick rather
/// than treating the failed attempt as done.
async fn reconcile_on_startup(
    positions: &Arc<RwLock<Vec<MonitoredPosition>>>,
    kotak: &KotakHandle,
    db_tx: &mpsc::Sender<DbWriteMessage>,
    log_tx: &broadcast::Sender<String>,
    brokerage: f64,
) -> bool {
    tracing::info!("LIVE startup reconciliation starting");

    // 0. Restart the grace clock on any entry whose send was never confirmed.
    //    Its `entry_attempt_at` is from before the restart, so without this the
    //    reconciler could decide "no matching order, never placed" on the very
    //    first order-book read — before Kotak has had a chance to surface a fill
    //    that landed while we were down — and then let decide_live buy again.
    {
        let now = shared_domain::current_ist_timestamp_string();
        let mut g = positions.write().await;
        for p in g.iter_mut().filter(|p| p.entry_send_uncertain) {
            p.entry_attempt_at = Some(now.clone());
        }
    }

    // 1. Settle whatever we were already tracking first, so a fill that landed
    //    while we were down is recorded as a real trade rather than inferred
    //    from a quantity difference below.
    reconcile_live_orders(positions, kotak, db_tx, log_tx, brokerage).await;

    struct Tracked {
        pos_id: String,
        instrument: String,
        trading_symbol: String,
        is_open: bool,
        executed_qty: i32,
        entry_order_id: Option<String>,
        sl_order_id: Option<String>,
    }
    // Computed before the broker calls: whether there is anything at all for a
    // failed positions-read to have gotten wrong. Held positions are the only
    // reason `get_positions()` truly needs to succeed here — its whole job below
    // is catching drift in what this engine already believes it holds, plus
    // flagging broker-side exposure it doesn't know about.
    let tracked: Vec<Tracked> = {
        let g = positions.read().await;
        g.iter()
            .filter(|p| !matches!(p.state, TradeState::Closed))
            .map(|p| Tracked {
                pos_id: p.id.clone(),
                instrument: p.signal.instrument_name.clone(),
                trading_symbol: p
                    .resolved_order
                    .as_ref()
                    .map(|o| o.trading_symbol.clone())
                    .unwrap_or_default(),
                is_open: matches!(p.state, TradeState::Active | TradeState::Target1Hit),
                executed_qty: p.executed_qty,
                entry_order_id: p.entry_order_id.clone(),
                sl_order_id: p.sl_order_id.clone(),
            })
            .collect()
    };
    let nothing_to_protect = tracked.is_empty();

    // 2. Broker truth.
    let (broker_positions, book) = {
        let guard = kotak.lock().await;
        let Some(client) = guard.as_ref() else {
            tracing::warn!("LIVE startup reconciliation skipped — no Kotak session");
            return false;
        };
        (client.get_positions().await, client.get_order_book().await)
    };
    let mut positions_verified = true;
    let broker_positions = match broker_positions {
        Ok(v) => v,
        // Confirmed (2026-08-17) independent of session/IP/auth — Order Book and
        // Limits succeed on the very same session that Positions fails on, every
        // time, including right after a fresh login. Most likely a broker-side
        // API product/subscription gap, not something a retry fixes. With
        // nothing currently tracked there is nothing this read could have
        // caught, so proceed in a degraded mode rather than block trading
        // entirely. The moment anything is held, `nothing_to_protect` is false
        // and this reverts to fully blocking on the next restart — exactly when
        // it actually has something to protect.
        Err(e) if nothing_to_protect => {
            positions_verified = false;
            loud_error(db_tx, log_tx, "SYSTEM", &format!(
                "startup reconciliation could not read positions ({e}) — proceeding anyway because nothing is currently tracked, so there is nothing to mis-protect; any broker-side holding this engine doesn't already know about will NOT be flagged until positions-read works again"
            )).await;
            Vec::new()
        }
        Err(e) => {
            loud_error(db_tx, log_tx, "SYSTEM", &format!(
                "startup reconciliation could not read positions ({e}) — engine state is unverified against the broker, check the terminal before trading"
            )).await;
            return false;
        }
    };
    let book = match book {
        Ok(v) => v,
        Err(e) => {
            loud_error(db_tx, log_tx, "SYSTEM", &format!(
                "startup reconciliation could not read the order book ({e}) — resting orders are unverified, check the terminal before trading"
            )).await;
            return false;
        }
    };

    // Everything below matches broker rows to positions by trading symbol, so two
    // open positions on the same contract would both claim the same holding and the
    // same resting stop. Rather than guess a split, stand down on all of them.
    let duplicated: Vec<&str> = tracked
        .iter()
        .filter(|t| t.is_open && !t.trading_symbol.is_empty())
        .map(|t| t.trading_symbol.trim())
        .fold(Vec::<(&str, usize)>::new(), |mut acc, sym| {
            match acc.iter_mut().find(|(s, _)| *s == sym) {
                Some(e) => e.1 += 1,
                None => acc.push((sym, 1)),
            }
            acc
        })
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(s, _)| s)
        .collect();

    for t in &tracked {
        if t.is_open && duplicated.contains(&t.trading_symbol.trim()) {
            loud_error(db_tx, log_tx, &t.instrument, &format!(
                "more than one open position is tracked on {} — the engine cannot tell whose quantity or stop is whose, so it will not act on any of them; resolve it at the broker terminal",
                t.trading_symbol
            )).await;
            with_position(positions, &t.pos_id, |p| {
                p.live_halt = Some("duplicate positions on one contract at startup".to_string());
            }).await;
            continue;
        }
        if !t.is_open {
            // Still waiting for an entry. If the stored order id is not in today's
            // book at all it is a leftover from a previous session — clear it so
            // the trigger can fire again instead of waiting on a dead order.
            if let Some(oid) = &t.entry_order_id {
                if !book.iter().any(|o| o.order_no.trim() == oid.trim()) {
                    tracing::warn!(instrument = %t.instrument, %oid, "stale entry order id cleared at startup");
                    with_position(positions, &t.pos_id, |p| {
                        p.entry_order_id = None;
                        p.entry_cancel_sent = false;
                    }).await;
                }
            }
            continue;
        }

        let held_at_broker = broker_positions
            .iter()
            .find(|bp| bp.trading_symbol.trim() == t.trading_symbol.trim())
            .map(|bp| bp.net_qty())
            .unwrap_or(0);

        if held_at_broker <= 0 {
            loud_error(db_tx, log_tx, &t.instrument, &format!(
                "the broker shows no open quantity for {} but the engine thought it held {} — closing it here; check the contract note for the exit that went unrecorded",
                t.trading_symbol, t.executed_qty
            )).await;
            with_position(positions, &t.pos_id, |p| {
                p.executed_qty = 0;
                p.state = TradeState::Closed;
                forget_stop(p);
            }).await;
            continue;
        }

        if held_at_broker != t.executed_qty {
            loud_error(db_tx, log_tx, &t.instrument, &format!(
                "engine held {} of {} but the broker shows {held_at_broker} — adopting the broker's quantity",
                t.executed_qty, t.trading_symbol
            )).await;
            with_position(positions, &t.pos_id, |p| p.executed_qty = held_at_broker).await;
        }

        // Adopt, rather than duplicate, whatever sell is already resting.
        let resting: Vec<&kotak_client::KotakOrder> = book
            .iter()
            .filter(|o| {
                o.is_sell()
                    && !o.is_terminal()
                    && o.trading_symbol.trim() == t.trading_symbol.trim()
            })
            .collect();

        match resting.len() {
            0 => {
                // Nothing protecting the position — clear any stale id so the
                // decision pass places a fresh stop on the next tick.
                with_position(positions, &t.pos_id, forget_stop).await;
            }
            1 => {
                let o = resting[0];
                let order_no = o.order_no.trim().to_string();
                let qty = (o.ordered_qty() - o.filled_qty()).max(0);
                let trigger = o.trigger();
                let already_known = t
                    .sl_order_id
                    .as_deref()
                    .is_some_and(|s| s.trim() == order_no);
                if !already_known {
                    loud_error(db_tx, log_tx, &t.instrument, &format!(
                        "adopting sell order {order_no} ({qty} @ trigger {trigger:.2}) as this position's stop — it was resting at the broker but missing from the engine's snapshot"
                    )).await;
                }
                with_position(positions, &t.pos_id, |p| {
                    p.sl_order_id = Some(order_no.clone());
                    p.sl_order_qty = qty;
                    p.sl_order_trigger = trigger;
                }).await;
                // If the adopted stop is the wrong size or level, the normal
                // decision pass issues a modify — no special handling needed.
            }
            n => {
                loud_error(db_tx, log_tx, &t.instrument, &format!(
                    "{n} sell orders are resting against {} — the engine cannot tell which one is the stop, so it will not act on this position at all; resolve it at the broker terminal",
                    t.trading_symbol
                )).await;
                with_position(positions, &t.pos_id, |p| {
                    p.live_halt = Some(format!("{n} resting sell orders found at startup"));
                }).await;
            }
        }
    }

    // 3. Exposure at the broker that the engine knows nothing about. Reported,
    //    never touched — an order we cannot explain might be a manual one, and
    //    cancelling it on a guess would be worse than leaving it.
    let tracked_symbols: Vec<&str> = tracked
        .iter()
        .map(|t| t.trading_symbol.trim())
        .filter(|s| !s.is_empty())
        .collect();

    for bp in &broker_positions {
        let sym = bp.trading_symbol.trim();
        if bp.net_qty() > 0 && !tracked_symbols.contains(&sym) {
            loud_error(db_tx, log_tx, sym, &format!(
                "the broker holds {} of {sym} which the engine is not tracking — it will not be protected or squared off automatically",
                bp.net_qty()
            )).await;
        }
    }
    for o in book.iter().filter(|o| o.is_sell() && !o.is_terminal()) {
        let sym = o.trading_symbol.trim();
        if !tracked_symbols.contains(&sym) {
            loud_error(db_tx, log_tx, sym, &format!(
                "sell order {} on {sym} is working at the broker with no matching tracked position — left alone, cancel it manually if it is stale",
                o.order_no
            )).await;
        }
    }

    let snapshot = { positions.read().await.clone() };
    send_positions_snapshot(db_tx, &snapshot).await;

    tracing::info!(positions = snapshot.len(), positions_verified, "LIVE startup reconciliation complete");
    live_info(db_tx, log_tx, json!({
        "event": "LIVE_STARTUP_RECONCILED",
        "positions": snapshot.len(),
        "positions_verified": positions_verified,
        "mode": "LIVE",
    })).await;
    true
}

// ---------------------------------------------------------------------------
// LIVE mode — on-demand reconciliation ("Sync with Kotak")
//
// Startup reconciliation above runs once, automatically, and is deliberately
// conservative about it: unambiguous drift it self-corrects, ambiguous drift
// it only logs. This is the on-demand counterpart triggered from the
// dashboard mid-session — same comparison against broker truth, but every
// finding becomes a question with explicit options instead of being silently
// acted on, since by the time someone reaches for this button local state and
// broker truth have likely already been diverging quietly for a while.
// ---------------------------------------------------------------------------

struct ReconcileTracked {
    pos_id: String,
    instrument: String,
    trading_symbol: String,
    is_open: bool,
    executed_qty: i32,
}

/// Read-only comparison against live broker positions. Never mutates
/// anything — every mismatch is returned as a `ReconcileFinding` for the
/// caller to present, not acted on here.
pub async fn preview_reconciliation(
    positions: &Arc<RwLock<Vec<MonitoredPosition>>>,
    kotak: &KotakHandle,
) -> Result<Vec<shared_domain::ReconcileFinding>, String> {
    use shared_domain::{ReconcileAction, ReconcileCategory, ReconcileFinding, ReconcileOption};

    let tracked: Vec<ReconcileTracked> = {
        let g = positions.read().await;
        g.iter()
            .filter(|p| !matches!(p.state, TradeState::Closed))
            .map(|p| ReconcileTracked {
                pos_id: p.id.clone(),
                instrument: p.signal.instrument_name.clone(),
                trading_symbol: p
                    .resolved_order
                    .as_ref()
                    .map(|o| o.trading_symbol.clone())
                    .unwrap_or_default(),
                is_open: matches!(p.state, TradeState::Active | TradeState::Target1Hit),
                executed_qty: p.executed_qty,
            })
            .collect()
    };

    let broker_positions = {
        let guard = kotak.lock().await;
        let Some(client) = guard.as_ref() else {
            return Err("no Kotak session".to_string());
        };
        client.get_positions().await.map_err(|e| e.to_string())?
    };

    let mut findings = Vec::new();

    // Same duplicate rule as startup: two open positions on one broker symbol
    // can't be told apart, so don't guess whose quantity is whose.
    let duplicated: Vec<&str> = tracked
        .iter()
        .filter(|t| t.is_open && !t.trading_symbol.is_empty())
        .map(|t| t.trading_symbol.trim())
        .fold(Vec::<(&str, usize)>::new(), |mut acc, sym| {
            match acc.iter_mut().find(|(s, _)| *s == sym) {
                Some(e) => e.1 += 1,
                None => acc.push((sym, 1)),
            }
            acc
        })
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(s, _)| s)
        .collect();

    for t in &tracked {
        if !t.is_open {
            continue; // WaitingForEntry — nothing bought yet, nothing to compare
        }
        if duplicated.contains(&t.trading_symbol.trim()) {
            findings.push(ReconcileFinding {
                position_id: Some(t.pos_id.clone()),
                trading_symbol: t.trading_symbol.clone(),
                instrument: t.instrument.clone(),
                category: ReconcileCategory::DuplicateAmbiguous,
                engine_qty: t.executed_qty,
                broker_qty: 0,
                broker_avg_price: 0.0,
                message: format!(
                    "More than one tracked position maps to {} at the broker — can't tell which is which. Resolve at the broker terminal.",
                    t.trading_symbol
                ),
                options: vec![],
            });
            continue;
        }

        let broker_row = broker_positions
            .iter()
            .find(|bp| bp.trading_symbol.trim() == t.trading_symbol.trim());
        let broker_qty = broker_row.map(|bp| bp.net_qty()).unwrap_or(0);
        let broker_avg_price = broker_row.map(|bp| bp.avg_buy_price()).unwrap_or(0.0);

        let (category, message, options) = if broker_qty == t.executed_qty {
            (
                ReconcileCategory::Matches,
                format!("{}: {} qty matches at both engine and broker.", t.instrument, t.executed_qty),
                vec![],
            )
        } else if broker_qty <= 0 {
            (
                ReconcileCategory::QtyZero,
                format!(
                    "{}: engine shows {} qty held, broker shows none — looks like it was fully closed outside the app.",
                    t.instrument, t.executed_qty
                ),
                vec![
                    ReconcileOption { action: ReconcileAction::Close, label: "Mark as closed".to_string(), recommended: true },
                    ReconcileOption { action: ReconcileAction::Ignore, label: "Leave as-is".to_string(), recommended: false },
                ],
            )
        } else if broker_qty < t.executed_qty {
            (
                ReconcileCategory::QtyReduced,
                format!(
                    "{}: engine shows {} qty held, broker shows {} — looks like part of it was sold outside the app.",
                    t.instrument, t.executed_qty, broker_qty
                ),
                vec![
                    ReconcileOption { action: ReconcileAction::AdoptQty, label: format!("Adopt broker's quantity ({broker_qty})"), recommended: true },
                    ReconcileOption { action: ReconcileAction::Ignore, label: "Leave as-is".to_string(), recommended: false },
                ],
            )
        } else {
            (
                ReconcileCategory::QtyIncreased,
                format!(
                    "{}: engine shows {} qty held, broker shows {} — looks like more was bought outside the app.",
                    t.instrument, t.executed_qty, broker_qty
                ),
                vec![
                    ReconcileOption { action: ReconcileAction::AdoptQty, label: format!("Adopt broker's quantity ({broker_qty})"), recommended: true },
                    ReconcileOption { action: ReconcileAction::Ignore, label: "Leave as-is".to_string(), recommended: false },
                ],
            )
        };

        findings.push(ReconcileFinding {
            position_id: Some(t.pos_id.clone()),
            trading_symbol: t.trading_symbol.clone(),
            instrument: t.instrument.clone(),
            category,
            engine_qty: t.executed_qty,
            broker_qty,
            broker_avg_price,
            message,
            options,
        });
    }

    // Broker exposure the engine has no record of at all. No one-click
    // "square it off" here — this engine has zero context on why it exists
    // (could be a manual hedge, a different strategy entirely). The one
    // action offered beyond acknowledging it is bringing it under the
    // engine's own SL/target management, but only with a stop-loss and
    // target the user types in themselves — never guessed.
    let tracked_symbols: Vec<&str> = tracked.iter().map(|t| t.trading_symbol.trim()).filter(|s| !s.is_empty()).collect();
    for bp in &broker_positions {
        let sym = bp.trading_symbol.trim();
        if bp.net_qty() > 0 && !tracked_symbols.contains(&sym) {
            findings.push(ReconcileFinding {
                position_id: None,
                trading_symbol: sym.to_string(),
                instrument: bp.symbol.clone(),
                category: ReconcileCategory::UnexplainedExposure,
                engine_qty: 0,
                broker_qty: bp.net_qty(),
                broker_avg_price: bp.avg_buy_price(),
                message: format!("The broker holds {} of {sym} that this app has no record of at all.", bp.net_qty()),
                options: vec![
                    ReconcileOption { action: ReconcileAction::Ignore, label: "Ignore — I'm managing this manually".to_string(), recommended: true },
                    // stop_loss/target/avg_buy_price are placeholders — the
                    // frontend collects the real numbers from the user and
                    // substitutes them into the ReconcileApplyItem it sends.
                    ReconcileOption {
                        action: ReconcileAction::AdoptManual { stop_loss: 0.0, target: 0.0, avg_buy_price: 0.0 },
                        label: "Adopt into my positions (enter SL, target & buy price)".to_string(),
                        recommended: false,
                    },
                ],
            });
        }
    }

    Ok(findings)
}

/// Applies user-confirmed actions from a `preview_reconciliation` report.
/// Re-fetches broker positions fresh rather than trusting a client-echoed
/// quantity, in case time passed between preview and confirmation. Only ever
/// touches the specific positions named.
#[allow(clippy::too_many_arguments)]
pub async fn apply_reconciliation(
    positions: &Arc<RwLock<Vec<MonitoredPosition>>>,
    kotak: &KotakHandle,
    scrip_store: &Arc<RwLock<Option<crate::ScripStore>>>,
    prices: &Arc<DashMap<String, f64>>,
    ws_tx: &Arc<tokio::sync::Mutex<Option<mpsc::UnboundedSender<String>>>>,
    db_tx: &mpsc::Sender<DbWriteMessage>,
    log_tx: &broadcast::Sender<String>,
    items: &[shared_domain::ReconcileApplyItem],
) -> Result<(), String> {
    use shared_domain::ReconcileAction;

    let needs_broker_truth = items.iter().any(|i| {
        matches!(i.action, ReconcileAction::AdoptQty | ReconcileAction::AdoptManual { .. })
    });
    let broker_positions = if needs_broker_truth {
        let guard = kotak.lock().await;
        let Some(client) = guard.as_ref() else {
            return Err("no Kotak session".to_string());
        };
        Some(client.get_positions().await.map_err(|e| e.to_string())?)
    } else {
        None
    };

    for item in items {
        match &item.action {
            ReconcileAction::Ignore => {
                live_info(db_tx, log_tx, json!({
                    "event": "RECONCILE_ACK",
                    "instrument": item.trading_symbol,
                })).await;
            }
            ReconcileAction::Close => {
                let Some(pos_id) = &item.position_id else { continue; };
                with_position(positions, pos_id, |p| {
                    p.executed_qty = 0;
                    p.state = TradeState::Closed;
                    forget_stop(p);
                }).await;
                loud_error(db_tx, log_tx, &item.trading_symbol, &format!(
                    "manually reconciled: {} marked closed (broker shows no open quantity)", item.trading_symbol
                )).await;
            }
            ReconcileAction::AdoptQty => {
                let Some(pos_id) = &item.position_id else { continue; };
                let Some(broker_positions) = &broker_positions else { continue; };
                let broker_qty = broker_positions
                    .iter()
                    .find(|bp| bp.trading_symbol.trim() == item.trading_symbol.trim())
                    .map(|bp| bp.net_qty())
                    .unwrap_or(0);
                let now_closed = broker_qty <= 0;
                with_position(positions, pos_id, |p| {
                    p.executed_qty = broker_qty.max(0);
                    if now_closed {
                        p.state = TradeState::Closed;
                        forget_stop(p);
                    }
                }).await;
                loud_error(db_tx, log_tx, &item.trading_symbol, &format!(
                    "manually reconciled: {} executed_qty adopted from broker ({broker_qty})", item.trading_symbol
                )).await;
            }
            ReconcileAction::AdoptManual { stop_loss, target, avg_buy_price } => {
                adopt_manual(
                    positions, scrip_store, prices, ws_tx, db_tx, log_tx,
                    &broker_positions, &item.trading_symbol, *stop_loss, *target, *avg_buy_price,
                ).await;
            }
        }
    }

    let snapshot = { positions.read().await.clone() };
    send_positions_snapshot(db_tx, &snapshot).await;
    Ok(())
}

/// Builds a brand-new `Active` position from broker exposure the engine had
/// no record of at all (`ReconcileCategory::UnexplainedExposure`), using a
/// user-entered stop-loss and single target — see `ReconcileAction::AdoptManual`.
/// Mirrors how a fresh BUY signal resolves its `OrderRequest`/`ws_scrip_key`
/// in `start_position_monitor`, just keyed off the broker's trading symbol
/// (via `ScripStore::find_by_trading_symbol`) instead of a parsed `TradeSignal`.
#[allow(clippy::too_many_arguments)]
async fn adopt_manual(
    positions: &Arc<RwLock<Vec<MonitoredPosition>>>,
    scrip_store: &Arc<RwLock<Option<crate::ScripStore>>>,
    prices: &Arc<DashMap<String, f64>>,
    ws_tx: &Arc<tokio::sync::Mutex<Option<mpsc::UnboundedSender<String>>>>,
    db_tx: &mpsc::Sender<DbWriteMessage>,
    log_tx: &broadcast::Sender<String>,
    broker_positions: &Option<Vec<kotak_client::KotakPosition>>,
    trading_symbol: &str,
    stop_loss: f64,
    target: f64,
    avg_buy_price_entered: f64,
) {
    // Never double-adopt: a symbol already tracked (open or waiting) has a
    // real position id and belongs to AdoptQty/Close instead.
    let already_tracked = {
        let g = positions.read().await;
        g.iter().any(|p| {
            !matches!(p.state, TradeState::Closed)
                && p.resolved_order.as_ref().map(|o| o.trading_symbol.trim()) == Some(trading_symbol.trim())
        })
    };
    if already_tracked {
        loud_error(db_tx, log_tx, trading_symbol, &format!(
            "manual adopt skipped: {trading_symbol} is already a tracked position"
        )).await;
        return;
    }

    let Some(bp) = broker_positions.as_ref().and_then(|bps| {
        bps.iter().find(|bp| bp.trading_symbol.trim() == trading_symbol.trim())
    }) else {
        loud_error(db_tx, log_tx, trading_symbol, &format!(
            "manual adopt failed: {trading_symbol} no longer shows an open quantity at the broker"
        )).await;
        return;
    };
    let qty = bp.net_qty();
    if qty <= 0 {
        loud_error(db_tx, log_tx, trading_symbol, &format!(
            "manual adopt skipped: {trading_symbol} broker quantity is now zero"
        )).await;
        return;
    }

    let record = {
        let scrip_guard = scrip_store.read().await;
        let Some(store) = scrip_guard.as_ref() else {
            loud_error(db_tx, log_tx, trading_symbol, "manual adopt failed: Scrip Master not loaded").await;
            return;
        };
        let Some(record) = store.find_by_trading_symbol(trading_symbol) else {
            loud_error(db_tx, log_tx, trading_symbol, &format!(
                "manual adopt failed: {trading_symbol} not found in Scrip Master"
            )).await;
            return;
        };
        record
    };

    use shared_domain::{AmoFlag, ExchangeSegment, OrderRequest, OrderType, ProductCode, TradeSignal, TransactionType, Validity};
    let exchange_segment = match record.exchange_segment_code.as_str() {
        "bse_fo" => ExchangeSegment::BseFo,
        "nse_cm" => ExchangeSegment::NseCm,
        _ => ExchangeSegment::NseFo,
    };
    let resolved_order = OrderRequest {
        after_market_order: AmoFlag::No,
        disclosed_quantity: "0".to_string(),
        exchange_segment,
        market_protection: "0".to_string(),
        product_code: ProductCode::Nrml,
        portfolio_flag: "N".to_string(),
        price: "0".to_string(),
        order_type: OrderType::Limit,
        quantity: record.lot_size.to_string(),
        validity: Validity::Day,
        trigger_price: "0".to_string(),
        trading_symbol: record.trading_symbol.clone(),
        transaction_type: TransactionType::Buy,
    };

    // Kotak's figure is a same-day VWAP over every fill of the symbol; when the
    // user knows what this particular lot actually cost, take their number.
    let avg_buy_price = resolved_adopt_avg(avg_buy_price_entered, bp.avg_buy_price());
    let avg_source = if avg_buy_price_entered > 0.0 { "you entered" } else { "broker avg" };
    let sl = round_down_tick(stop_loss, record.tick_size);
    let tgt = round_down_tick(target, record.tick_size);
    let ws_scrip_key = format!("{}|{}", record.exchange_segment_code, record.instrument_token);

    prices.entry(ws_scrip_key.clone()).or_insert(0.0);
    {
        let tx_guard = ws_tx.lock().await;
        if let Some(tx) = tx_guard.as_ref() {
            let _ = tx.send(json!({ "action": "subscribe", "scrips": ws_scrip_key }).to_string());
        }
    }

    let new_pos = MonitoredPosition {
        id: uuid::Uuid::new_v4().to_string(),
        signal: TradeSignal {
            instrument_name: record.symbol_name.clone(),
            strike: Some(record.strike_price),
            option_type: (!record.option_type.is_empty()).then(|| record.option_type.clone()),
            expiry: Some(record.expiry_date.format("%d-%b-%Y").to_string().to_uppercase()),
            action: "BUY".to_string(),
            entry_condition: "ABOVE".to_string(),
            entry_price: avg_buy_price,
            targets: vec![tgt],
            stop_loss: sl,
            source: "MANUAL_RECONCILE".to_string(),
            signal_id: None,
            raw_message: None,
        },
        state: TradeState::Active,
        created_at: shared_domain::current_ist_timestamp_string(),
        current_sl: sl,
        peak_ltp: None,
        next_dynamic_target: None,
        last_dynamic_rung: None,
        dynamic_rung_number: 0,
        manual_sell_qty: None,
        executed_qty: qty,
        avg_buy_price,
        override_qty: None,
        resolved_order: Some(resolved_order),
        resolved_expiry: Some(record.expiry_date),
        ltp: None,
        ws_scrip_key: Some(ws_scrip_key),
        force_exit: None,
        override_exit_price: None,
        tick_size: record.tick_size,
        entry_order_id: None,
        sl_order_id: None,
        sl_order_qty: 0,
        sl_order_trigger: 0.0,
        target_order_id: None,
        pending_exit_order_id: None,
        pending_exit_qty: 0,
        pending_exit_reason: None,
        entry_cancel_sent: false,
        exit_attempts: 0,
        entry_attempts: 0,
        entry_attempt_at: None,
        entry_send_uncertain: false,
        entry_uncertain_qty: None,
        entry_retry_as_limit: false,
        live_halt: None,
        entry_zone: None,
    };

    { positions.write().await.push(new_pos); }

    loud_error(db_tx, log_tx, trading_symbol, &format!(
        "manually adopted {trading_symbol} — qty {qty} @ ₹{avg_buy_price:.2} ({avg_source}), SL ₹{sl:.2}, target ₹{tgt:.2}"
    )).await;
}

/// The entry price for a manually-adopted position: the value the user typed
/// if they gave one (`> 0.0`), otherwise the broker's same-day average.
fn resolved_adopt_avg(user_entered: f64, broker_avg: f64) -> f64 {
    if user_entered > 0.0 { user_entered } else { broker_avg }
}

// ---------------------------------------------------------------------------
// LIVE mode — decision pass
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum LiveAction {
    /// Entry condition met — check funds, then buy `qty`. `ltp` is the price
    /// that triggered it, used to size the funds check. `limit_price` is `None`
    /// for the normal market buy; `Some(p)` when a prior market send was
    /// RMS-rejected and the retry must go in as an IOC limit at `p`.
    PlaceEntry { qty: i32, ltp: f64, limit_price: Option<f64> },
    /// Give up on an entry that will not be taken; cancel it if it is in flight.
    /// `loud` surfaces it as an ERROR the dashboard highlights (the signal
    /// looked wrong) rather than a routine INFO line (e.g. the EOD cutoff).
    AbandonEntry { reason: String, loud: bool },
    /// Target 1: trail the software stop to `new_sl`, then market-sell `slice`.
    /// `next_dynamic_target`/`dynamic_rung` seed the dynamic ladder (see
    /// `TrailDynamic`) when dynamic targeting is on; `None` keeps the
    /// existing fixed-target-2 behaviour for this position. `slice == 0` is a
    /// one-lot runner (`dynamic_targeting_single_lot`): trail only, no sell.
    Target1 { slice: i32, keep: i32, new_sl: f64, next_dynamic_target: Option<f64>, dynamic_rung: Option<f64> },
    /// Dynamic-targeting rung hit: trail the stop and extend the next rung.
    /// `rung_hit` is the price level that was just crossed, recorded as
    /// `last_dynamic_rung` so a later settings change can recompute both
    /// `new_sl` and `next_target` in place. No broker call — this is pure
    /// local bookkeeping, since protection is already a software LTP watch
    /// against `current_sl`.
    TrailDynamic { new_sl: f64, next_target: f64, rung_hit: f64 },
    /// Market-sell everything we hold (stop hit, target 2, forced exit, etc.).
    /// Cancels any resting sell order first if one happens to exist (e.g.
    /// adopted from the broker at startup) — this engine never places one.
    ExitAll { qty: i32, reason: String },
    /// User-requested partial market sell for `qty`, leaving the rest of the
    /// holding running exactly as before (state, current_sl untouched).
    ManualSell { qty: i32 },
}

struct LivePending {
    pos_id: String,
    action: LiveAction,
}

/// Decide the single next LIVE action for one position. Pure and read-only —
/// the caller performs the broker calls with no lock held.
///
/// Ordering is deliberate: forced exits, then protection, then targets. Nothing
/// is decided while an engine-initiated exit is still in flight, which is what
/// keeps `resting stop qty + in-flight sell qty` from ever exceeding what we
/// hold. This account has no margin, so an accidental oversell would be a naked
/// short, not just a bad fill.
fn decide_live(
    pos: &MonitoredPosition,
    ltp_map: &Arc<DashMap<String, f64>>,
    cfg: &TradingConfig,
    entry_cutoff: bool,
    retry_window: bool,
) -> Option<LiveAction> {
    if pos.live_halt.is_some() || pos.resolved_order.is_none() {
        return None;
    }

    let key = pos.ws_scrip_key.as_ref().unwrap_or(&pos.signal.instrument_name);
    let ltp = ltp_map.get(key.as_str()).map(|r| *r).filter(|v| *v > 0.0);

    match pos.state {
        TradeState::Closed => None,

        TradeState::WaitingForEntry => {
            // An entry order was sent but its outcome was never confirmed. The
            // reconciler now owns this position: it is hunting the order book
            // for the fill to adopt, or will clear the flag once it is certain
            // nothing was placed. Send nothing else, and don't let a forced
            // exit / staleness rule (or the kill switch) close it in the
            // meantime — if the order did fill, closing here would drop a real
            // holding. The grace window is seconds, well inside any cutoff; once
            // it resolves to Active the kill switch squares it off there.
            if pos.entry_send_uncertain {
                return None;
            }
            if cfg.kill_switch_active {
                return Some(LiveAction::AbandonEntry {
                    reason: "KILL_SWITCH_ACTIVE".to_string(),
                    loud: false,
                });
            }
            if let Some(reason) = pos.force_exit.clone() {
                return Some(LiveAction::AbandonEntry { reason, loud: false });
            }
            if let Some(reason) = stale_entry_reason(pos, entry_cutoff) {
                return Some(LiveAction::AbandonEntry { reason: reason.to_string(), loud: false });
            }
            if pos.entry_order_id.is_some() {
                // In flight — the order book decides what happened.
                return None;
            }
            // Every send of this entry failed before the broker gave a verdict
            // and every follow-up hunt came up empty. Stop trying.
            if pos.entry_attempts >= shared_domain::MAX_ENTRY_ATTEMPTS {
                return Some(LiveAction::AbandonEntry {
                    reason: format!(
                        "entry order could not be sent after {} attempts (broker unreachable) — signal dropped",
                        shared_domain::MAX_ENTRY_ATTEMPTS
                    ),
                    loud: true,
                });
            }
            let ltp = ltp?;
            if !entry_triggered(&pos.signal, ltp, cfg) {
                return None;
            }
            // The signal's move is already spent — entering here just books
            // target 1 on the next tick. Refuse it loudly instead.
            if let Some(t1) = entry_past_target1(&pos.signal, ltp) {
                return Some(LiveAction::AbandonEntry {
                    reason: format!("entry {ltp:.2} is already at/through target 1 ({t1:.2}) — wrong contract or stale signal, not entering"),
                    loud: true,
                });
            }
            let lot = lot_size_of(pos);
            let qty = compute_entry_qty(&pos.signal, lot, pos.override_qty, cfg, Some(ltp));
            if qty <= 0 || qty % lot != 0 {
                return Some(LiveAction::AbandonEntry {
                    reason: format!("invalid quantity {qty} for lot size {lot}"),
                    loud: false,
                });
            }
            // Space out entry attempts. A pre-flight that just failed (funds
            // unverifiable, ambiguous send) gets time to recover instead of
            // being retried on the very next 50 ms tick.
            if let Some(elapsed) = secs_since_ist(&pos.entry_attempt_at) {
                if elapsed < ENTRY_RETRY_THROTTLE.as_secs() as i64 {
                    return None;
                }
            }
            // A prior market send was RMS-rejected (the post-open `Not_Ok`).
            // Retry as an IOC limit at a protection band above the touch,
            // capped to the buy window. If there is no usable limit price,
            // fall through to another market try rather than stall.
            let limit_price = if pos.entry_retry_as_limit {
                entry_retry_limit_price(&pos.signal, cfg, ltp, pos.tick_size)
            } else {
                None
            };
            Some(LiveAction::PlaceEntry { qty, ltp, limit_price })
        }

        TradeState::Active | TradeState::Target1Hit => {
            if pos.pending_exit_order_id.is_some() || pos.exit_attempts >= MAX_EXIT_ATTEMPTS {
                return None;
            }
            // Space retries out over the poll cadence so a rejecting broker is
            // not hit three times in 150 ms.
            if pos.exit_attempts > 0 && !retry_window {
                return None;
            }
            let held = pos.executed_qty;
            if held <= 0 {
                return None;
            }

            if let Some(reason) = pos.force_exit.clone() {
                return Some(LiveAction::ExitAll { qty: held, reason });
            }
            if is_expiry_squareoff_due(pos, shared_domain::now_ist()) {
                return Some(LiveAction::ExitAll {
                    qty: held,
                    reason: "EXPIRY_SQUAREOFF".to_string(),
                });
            }

            let trigger = round_down_tick(pos.current_sl, pos.tick_size);
            // A stop sitting above the signal's SL while still Active means
            // the pre-T1 trail moved it — label that exit as a trail hit.
            let sl_reason = if matches!(pos.state, TradeState::Target1Hit)
                || pos.current_sl > pos.signal.stop_loss
            {
                "TRAIL_SL_HIT"
            } else {
                "SL_HIT"
            };

            // Protection is a pure software watch — no resting stop order is
            // ever placed at the broker. Kotak's RMS hard-rejects `SL-M` on
            // options outright, and a resting `SL` (limit) risks not filling
            // at all on a fast move, which would be worse than no order —
            // either way, a broker-side order is not the safety net here.
            // Instead: every tick checks LTP against the current stop level
            // directly and fires a market sell for the whole holding the
            // instant it's crossed, with no dependency on any order state.
            if let Some(p) = ltp {
                if p <= trigger {
                    return Some(LiveAction::ExitAll { qty: held, reason: sl_reason.to_string() });
                }
            }

            // A user-requested partial sell, once protection is confirmed
            // intact. Re-clamped against `held` here in case it changed
            // (e.g. a fill landed) between the request and this tick.
            if let Some(qty) = pos.manual_sell_qty {
                let qty = qty.min(held);
                if qty > 0 {
                    return Some(LiveAction::ManualSell { qty });
                }
            }

            let ltp = ltp?;
            match pos.state {
                TradeState::Active => {
                    let t1 = *pos.signal.targets.first()?;
                    if ltp < t1 {
                        return None;
                    }
                    let lot = lot_size_of(pos);
                    // One-lot runner: selling one lot at target 1 would leave
                    // nothing to run, so when opted in the whole holding stays
                    // on as the runner instead (slice 0, no sell at all). Only
                    // when target 1 sits above our fill — otherwise diff <= 0
                    // and the ladder can't climb, so exit in full as before.
                    let keep_whole_lot = cfg.dynamic_targeting
                        && cfg.dynamic_targeting_single_lot
                        && held <= lot
                        && t1 > pos.avg_buy_price;
                    // Dynamic targeting sells exactly one lot at target 1
                    // (not the configured percentage) so there's a runner
                    // left to extend; off, this is the existing behaviour.
                    let slice = if keep_whole_lot {
                        0
                    } else if cfg.dynamic_targeting {
                        lot.min(held)
                    } else {
                        tgt1_slice_qty(held, lot, cfg.target_1_exit_pct)
                    };
                    // Target 2 only ever mattered here as an "is there more
                    // room to run" check — the dynamic path itself only ever
                    // reads target 1. So a single-target signal still ladders
                    // when dynamic targeting is on, instead of always fully
                    // exiting at target 1 regardless of the setting.
                    let has_t2 = pos.signal.targets.len() > 1;
                    if !keep_whole_lot && ((!has_t2 && !cfg.dynamic_targeting) || slice >= held) {
                        Some(LiveAction::ExitAll { qty: held, reason: "TGT1_FULL".to_string() })
                    } else {
                        // diff = distance from entry to target 1. Every rung of
                        // the dynamic ladder (starting with target 1 itself)
                        // follows one rule: trail the stop to
                        // `rung - diff * dynamic_targeting_trail_factor`, and
                        // set the next rung to
                        // `rung + diff * dynamic_targeting_extension_factor`.
                        let diff = t1 - pos.avg_buy_price;
                        Some(LiveAction::Target1 {
                            slice,
                            keep: held - slice,
                            new_sl: round_down_tick(
                                t1 - diff * cfg.dynamic_targeting_trail_factor,
                                pos.tick_size,
                            ),
                            next_dynamic_target: cfg.dynamic_targeting.then(|| {
                                t1 + diff * cfg.dynamic_targeting_extension_factor
                            }),
                            dynamic_rung: cfg.dynamic_targeting.then_some(t1),
                        })
                    }
                }
                TradeState::Target1Hit => {
                    if let Some(next_target) = pos.next_dynamic_target {
                        // Dynamic runner: never exits on a target hit, only
                        // trails and extends. It only ever exits via the
                        // unconditional stop-loss watch above, once price
                        // pulls back through the trailed current_sl.
                        if ltp < next_target {
                            return None;
                        }
                        let t1 = *pos.signal.targets.first()?;
                        let diff = t1 - pos.avg_buy_price;
                        Some(LiveAction::TrailDynamic {
                            new_sl: round_down_tick(
                                next_target - diff * cfg.dynamic_targeting_trail_factor,
                                pos.tick_size,
                            ),
                            next_target: next_target + diff * cfg.dynamic_targeting_extension_factor,
                            rung_hit: next_target,
                        })
                    } else {
                        let t2 = *pos.signal.targets.get(1)?;
                        (ltp >= t2).then(|| LiveAction::ExitAll {
                            qty: held,
                            reason: "TGT2_HIT".to_string(),
                        })
                    }
                }
                _ => None,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// LIVE mode — execution pass
// ---------------------------------------------------------------------------

/// Immutable snapshot of the bits of a position needed to talk to the broker.
struct LiveCtx {
    instrument: String,
    trading_symbol: String,
    base: shared_domain::OrderRequest,
    entry_order_id: Option<String>,
    entry_cancel_sent: bool,
    sl_order_id: Option<String>,
    executed_qty: i32,
    tick_size: f64,
    /// LTP-map key, for pricing an IOC limit fallback when a market order is
    /// refused. `None` only if the position never resolved a websocket key.
    ws_key: Option<String>,
}

/// Cancel any resting stop, then market-sell the whole holding.
///
/// The cancel has to succeed first — normally. A resting stop *plus* a market
/// sell for the same quantity could both fill, which would leave the account
/// short, and this account carries no margin to be short with. But `sl_order_id`
/// can point at an order that is already dead at the broker (this engine no
/// longer places its own, so the only way it's populated is by adopting a
/// resting sell order at startup, which can itself have since filled/been
/// rejected/cancelled) — cancelling a dead order fails with something like
/// "please provide valid order number", which used to block every exit path,
/// including the manual Close button, indefinitely. A dead order can't
/// double-fill against anything, so check the order book before giving up.
#[allow(clippy::too_many_arguments)]
async fn exec_exit_all(
    positions: &Arc<RwLock<Vec<MonitoredPosition>>>,
    kotak: &KotakHandle,
    db_tx: &mpsc::Sender<DbWriteMessage>,
    log_tx: &broadcast::Sender<String>,
    cfg: &TradingConfig,
    ref_ltp: Option<f64>,
    pos_id: &str,
    ctx: &LiveCtx,
    qty: i32,
    reason: &str,
) {
    if qty <= 0 {
        return;
    }

    if let Some(sl_id) = &ctx.sl_order_id {
        if let Err(e) = kotak_cancel(kotak, sl_id, &ctx.trading_symbol).await {
            let still_open = {
                let guard = kotak.lock().await;
                match guard.as_ref() {
                    Some(client) => client
                        .get_order_book()
                        .await
                        .ok()
                        .map(|book| book.iter().any(|o| o.order_no.trim() == sl_id.trim() && !o.is_terminal()))
                        .unwrap_or(true), // couldn't check — stay conservative
                    None => true,
                }
            };
            if still_open {
                loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                    "could not cancel stop-loss {sl_id} before the {reason} exit ({e}) — holding the market sell back so we cannot end up short"
                )).await;
                bump_exit_attempts(positions, db_tx, log_tx, pos_id, &ctx.instrument).await;
                return;
            }
            loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                "stop-loss {sl_id} was already inactive at the broker ({e}) — proceeding with the {reason} market exit"
            )).await;
        }
        with_position(positions, pos_id, forget_stop).await;
    }

    // Never ask the broker to sell more than the engine's own bookkeeping can
    // account for — see `sellable_qty`. This replaces an earlier live
    // Positions read here: that read could come back empty or stale right
    // after a fresh fill (or fail outright — this account's Positions
    // endpoint has failed on every call before, 2026-08-17) and there is no
    // safe way to tell "broker shows nothing because we hold nothing" apart
    // from "broker shows nothing because the read is lagging or broken" —
    // treating both the same way skipped real stop-losses on real holdings.
    // It also held the shared Kotak mutex on every single exit (implicated
    // in the 2026-08-31 freeze). Pure in-memory accounting has none of that:
    // for the common case of one position per symbol it never trims anything
    // (see `sellable_qty`'s doc comment for the residual gap it doesn't
    // cover).
    let safe_qty = sellable_qty(positions, pos_id, &ctx.trading_symbol, qty).await;
    if safe_qty < qty {
        loud_error(db_tx, log_tx, &ctx.instrument, &format!(
            "{reason} exit trimmed from {qty} to {safe_qty} — another tracked leg on {} already has a stop resting or a sell in flight",
            ctx.trading_symbol
        )).await;
    }
    let qty = safe_qty;
    if qty <= 0 {
        loud_error(db_tx, log_tx, &ctx.instrument, &format!(
            "{reason} exit produced no sellable quantity for {} — another tracked leg on this contract already accounts for the full holding; check for a duplicate-position or bookkeeping mismatch",
            ctx.trading_symbol
        )).await;
        bump_exit_attempts(positions, db_tx, log_tx, pos_id, &ctx.instrument).await;
        return;
    }

    match place_exit_sell(kotak, db_tx, log_tx, ctx, cfg, ref_ltp, qty, reason).await {
        Some(order_id) => {
            with_position(positions, pos_id, |p| {
                p.pending_exit_order_id = Some(order_id.clone());
                p.pending_exit_qty = qty;
                p.pending_exit_reason = Some(reason.to_string());
                p.force_exit = None;
                p.override_exit_price = None;
            }).await;
        }
        None => {
            bump_exit_attempts(positions, db_tx, log_tx, pos_id, &ctx.instrument).await;
        }
    }
}

/// Place a protective sell for `qty`: an IOC **market** order first, and — if
/// the exchange RMS hard-rejects it (the generic `Not_Ok` it returns for market
/// orders in the first minutes after the 09:15 open) — one IOC **limit** retry
/// a protection band below the touch. Returns the broker order id on success.
///
/// The limit retry is only attempted after a definitive rejection, never after
/// an ambiguous / session failure, because a rejected order provably never
/// rested and a follow-up cannot double-sell. Logs its own outcome; the caller
/// owns all position-state bookkeeping.
#[allow(clippy::too_many_arguments)]
async fn place_exit_sell(
    kotak: &KotakHandle,
    db_tx: &mpsc::Sender<DbWriteMessage>,
    log_tx: &broadcast::Sender<String>,
    ctx: &LiveCtx,
    cfg: &TradingConfig,
    ref_ltp: Option<f64>,
    qty: i32,
    reason: &str,
) -> Option<String> {
    // IOC so that if Kotak internally converts our MKT to a limit (which it
    // does for F&O options), the order auto-cancels the moment it isn't
    // immediately filled rather than sitting open and tying up margin. The
    // reconciler re-detects the SL condition on the next tick and retries.
    let mut mkt = build_market_order(&ctx.base, shared_domain::TransactionType::Sell, qty, 0.0);
    mkt.validity = shared_domain::Validity::Ioc;
    let rejected = match kotak_place(kotak, &mkt).await {
        Ok(order_id) => {
            tracing::info!(instrument = %ctx.instrument, %order_id, qty, reason, "LIVE market exit placed");
            live_info(db_tx, log_tx, json!({
                "event": "LIVE_EXIT_PLACED",
                "instrument": ctx.instrument,
                "order_id": order_id,
                "qty": qty,
                "reason": reason,
                "mode": "LIVE",
            })).await;
            return Some(order_id);
        }
        Err(e) if e.is_session_expired() || e.is_ambiguous() => {
            loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                "market exit ({reason}) for {qty} was rejected: {e} — the position is unprotected until a new stop is placed"
            )).await;
            return None;
        }
        Err(e) => e,
    };

    // Hard rejection — try an IOC limit a protection band below the touch.
    let Some(ltp) = ref_ltp.filter(|v| *v > 0.0) else {
        loud_error(db_tx, log_tx, &ctx.instrument, &format!(
            "market exit ({reason}) for {qty} was rejected: {rejected} — no live price for a limit fallback, still unprotected"
        )).await;
        return None;
    };
    let floor = round_down_tick(
        ltp * (1.0 - cfg.entry_market_protection.max(0.0) / 100.0),
        ctx.tick_size,
    );
    if floor <= 0.0 {
        loud_error(db_tx, log_tx, &ctx.instrument, &format!(
            "market exit ({reason}) for {qty} was rejected: {rejected} — could not price a limit fallback, still unprotected"
        )).await;
        return None;
    }
    let lim = build_ioc_limit_order(&ctx.base, shared_domain::TransactionType::Sell, qty, floor);
    match kotak_place(kotak, &lim).await {
        Ok(order_id) => {
            loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                "market exit ({reason}) for {qty} was refused ({rejected}) — placed an IOC limit sell at ₹{floor:.2} instead"
            )).await;
            live_info(db_tx, log_tx, json!({
                "event": "LIVE_EXIT_PLACED",
                "instrument": ctx.instrument,
                "order_id": order_id,
                "qty": qty,
                "reason": reason,
                "order_type": "IOC_LIMIT",
                "limit_price": round2(floor),
                "mode": "LIVE",
            })).await;
            Some(order_id)
        }
        Err(e2) => {
            loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                "exit ({reason}) for {qty} rejected as both market ({rejected}) and limit ({e2}) — the position is unprotected"
            )).await;
            None
        }
    }
}

/// Carry out one decided action. No positions lock is held across a broker call.
async fn exec_live_action(
    positions: &Arc<RwLock<Vec<MonitoredPosition>>>,
    kotak: &KotakHandle,
    db_tx: &mpsc::Sender<DbWriteMessage>,
    log_tx: &broadcast::Sender<String>,
    cfg: &TradingConfig,
    ltp_map: &Arc<DashMap<String, f64>>,
    pending: &LivePending,
) -> bool {
    let ctx = {
        let g = positions.read().await;
        let Some(p) = g.iter().find(|p| p.id == pending.pos_id) else { return false; };
        let Some(base) = p.resolved_order.clone() else { return false; };
        LiveCtx {
            instrument: p.signal.instrument_name.clone(),
            trading_symbol: base.trading_symbol.clone(),
            base,
            entry_order_id: p.entry_order_id.clone(),
            entry_cancel_sent: p.entry_cancel_sent,
            sl_order_id: p.sl_order_id.clone(),
            executed_qty: p.executed_qty,
            tick_size: p.tick_size,
            ws_key: p.ws_scrip_key.clone(),
        }
    };

    // Freshest LTP for this position, for pricing an IOC limit fallback.
    let ctx_ltp = ctx
        .ws_key
        .as_ref()
        .and_then(|k| ltp_map.get(k.as_str()).map(|r| *r))
        .filter(|v| *v > 0.0);

    match &pending.action {
        LiveAction::PlaceEntry { qty, ltp, limit_price } => {
            // Stamp the attempt time first, so decide_live's throttle spaces
            // out the next pre-flight even if this one bails out below.
            with_position(positions, &pending.pos_id, |p| {
                p.entry_attempt_at = Some(shared_domain::current_ist_timestamp_string());
            }).await;

            // Pre-flight funds check. We must be able to pay for the order and
            // still have the buffer left over — the account is never run to zero.
            // Size it on the limit price when this is a limit retry (it is the
            // most we could pay), otherwise on the touch.
            let price_for_funds = limit_price.unwrap_or(*ltp);
            let order_value = *qty as f64 * price_for_funds;
            let required = order_value + FUNDS_BUFFER_INR;
            let mut limits_err = None;
            for attempt in 1..=LIMITS_PREFLIGHT_ATTEMPTS {
                match kotak_limits(kotak).await {
                    Ok(limits) => {
                        limits_err = None;
                        if limits.net < required {
                            loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                                "not enough funds for {qty} @ ~₹{ltp:.2}: needs ₹{required:.2} (₹{order_value:.2} order + ₹{FUNDS_BUFFER_INR:.0} buffer) but only ₹{:.2} is available — signal dropped",
                                limits.net
                            )).await;
                            with_position(positions, &pending.pos_id, |p| p.state = TradeState::Closed).await;
                            return true;
                        }
                        break;
                    }
                    Err(e) => {
                        limits_err = Some(e);
                        if attempt < LIMITS_PREFLIGHT_ATTEMPTS {
                            tokio::time::sleep(LIMITS_PREFLIGHT_RETRY_GAP).await;
                        }
                    }
                }
            }
            if let Some(e) = limits_err {
                // Funds could not be verified. Nothing was sent, so nothing is
                // at the broker — keep the position WaitingForEntry and let a
                // later tick retry the whole pre-flight (decide_live throttles
                // it to one attempt per poll). A dead session is surfaced so
                // the server's watchdog re-logs-in; a transient blip just waits.
                let cause = if e.is_session_expired() { "Kotak session expired" } else { "Kotak unreachable" };
                loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                    "could not verify funds before entry ({e}) — {cause}; holding the signal and retrying, nothing bought"
                )).await;
                return true;
            }

            let order = match limit_price {
                Some(lp) => build_ioc_limit_order(
                    &ctx.base, shared_domain::TransactionType::Buy, *qty, *lp,
                ),
                None => build_market_order(
                    &ctx.base, shared_domain::TransactionType::Buy, *qty,
                    cfg.entry_market_protection,
                ),
            };
            match kotak_place(kotak, &order).await {
                Ok(order_id) => {
                    with_position(positions, &pending.pos_id, |p| {
                        p.entry_order_id = Some(order_id.clone());
                        p.entry_cancel_sent = false;
                        p.entry_send_uncertain = false;
                        p.entry_uncertain_qty = None;
                        // A market buy that the broker accepted is one real
                        // attempt. An accepted IOC limit may simply expire
                        // unfilled — that is a miss, not a spent attempt, so it
                        // is not counted here (the reconciler retries it).
                        if limit_price.is_none() {
                            p.entry_attempts += 1;
                        }
                    }).await;
                    match limit_price {
                        Some(lp) => {
                            tracing::info!(instrument = %ctx.instrument, %order_id, qty, limit = lp, "LIVE entry IOC limit buy placed");
                            live_info(db_tx, log_tx, json!({
                                "event": "LIVE_ENTRY_PLACED",
                                "instrument": ctx.instrument,
                                "order_id": order_id,
                                "qty": qty,
                                "order_type": "IOC_LIMIT",
                                "limit_price": round2(*lp),
                                "mode": "LIVE",
                            })).await;
                        }
                        None => {
                            tracing::info!(instrument = %ctx.instrument, %order_id, qty, "LIVE entry market buy placed");
                            live_info(db_tx, log_tx, json!({
                                "event": "LIVE_ENTRY_PLACED",
                                "instrument": ctx.instrument,
                                "order_id": order_id,
                                "qty": qty,
                                "mode": "LIVE",
                            })).await;
                        }
                    }
                }
                Err(e) if e.is_session_expired() => {
                    // HTTP 401/403 — the gateway rejected the request before it
                    // was processed, so the order was provably not placed. Not
                    // counted as a real attempt: leave the position waiting so
                    // it enters once the watchdog restores the session (the
                    // trigger / past-target-1 guards are re-checked each tick).
                    loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                        "entry order not sent — Kotak session expired ({e}); holding the signal until the session is restored, nothing bought"
                    )).await;
                }
                Err(e) if e.is_ambiguous() => {
                    // Connect failure / timeout / unparseable reply — the order
                    // MAY have reached Kotak and filled. Never assume rejection:
                    // mark the position uncertain so the reconciler hunts the
                    // order book for the matching BUY and adopts it (placing the
                    // protective stop), or clears the flag once it is sure
                    // nothing landed.
                    with_position(positions, &pending.pos_id, |p| {
                        p.entry_send_uncertain = true;
                        p.entry_uncertain_qty = Some(*qty);
                        p.entry_attempts += 1;
                    }).await;
                    loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                        "entry order send got no broker verdict ({e}) — it MAY be live at Kotak. Reconciling against the order book before anything else; check your broker app now"
                    )).await;
                }
                Err(e) if limit_price.is_none() => {
                    // A real rejection of the market buy from the exchange / RMS
                    // — the generic post-open `Not_Ok`. Don't drop the signal:
                    // switch this position to IOC-limit retries inside the buy
                    // window. decide_live re-checks the trigger each tick and
                    // stops on its own if the price leaves the window or the
                    // 15:39 cutoff passes.
                    with_position(positions, &pending.pos_id, |p| {
                        p.entry_attempts += 1;
                        p.entry_retry_as_limit = true;
                    }).await;
                    loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                        "entry market buy for {qty} was rejected: {e} — retrying as an IOC limit inside the buy window, nothing bought yet"
                    )).await;
                }
                Err(e) => {
                    // The IOC limit retry was itself hard-rejected. Count it;
                    // give up only once the attempt cap is hit.
                    let mut attempts = shared_domain::MAX_ENTRY_ATTEMPTS;
                    with_position(positions, &pending.pos_id, |p| {
                        p.entry_attempts += 1;
                        attempts = p.entry_attempts;
                    }).await;
                    if attempts >= shared_domain::MAX_ENTRY_ATTEMPTS {
                        loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                            "entry limit buy for {qty} was rejected: {e} — {attempts} attempts, signal dropped"
                        )).await;
                        with_position(positions, &pending.pos_id, |p| p.state = TradeState::Closed).await;
                    } else {
                        loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                            "entry limit buy for {qty} was rejected: {e} — retrying (attempt {attempts})"
                        )).await;
                    }
                }
            }
        }

        LiveAction::AbandonEntry { reason, loud } => {
            if let Some(entry_id) = &ctx.entry_order_id {
                if ctx.entry_cancel_sent {
                    // Already asked once; let the order book say how it ended.
                    return false;
                }
                if let Err(e) = kotak_cancel(kotak, entry_id, &ctx.trading_symbol).await {
                    loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                        "could not cancel in-flight entry order {entry_id} ({reason}): {e} — it may still fill, leaving the position open"
                    )).await;
                    with_position(positions, &pending.pos_id, |p| p.entry_cancel_sent = true).await;
                    return true;
                }
            }
            with_position(positions, &pending.pos_id, |p| {
                p.state = TradeState::Closed;
                p.entry_order_id = None;
                p.entry_retry_as_limit = false;
            }).await;
            if *loud {
                loud_error(db_tx, log_tx, &ctx.instrument, &format!("entry not taken — {reason}")).await;
            } else {
                tracing::info!(instrument = %ctx.instrument, reason, "LIVE entry abandoned");
                live_info(db_tx, log_tx, json!({
                    "event": "ENTRY_ABANDONED",
                    "instrument": ctx.instrument,
                    "reason": reason,
                    "mode": "LIVE",
                })).await;
            }
        }

        LiveAction::Target1 { slice, keep, new_sl, next_dynamic_target, dynamic_rung } => {
            // No resting stop to shrink — protection is a pure software watch
            // (see decide_live). Trail current_sl for the runner first, then
            // sell the slice at market; the next tick's LTP check enforces
            // new_sl on `keep` with no broker-side order involved.
            with_position(positions, &pending.pos_id, |p| {
                p.current_sl = *new_sl;
                p.next_dynamic_target = *next_dynamic_target;
                p.last_dynamic_rung = *dynamic_rung;
                p.dynamic_rung_number = if dynamic_rung.is_some() { 1 } else { 0 };
                p.state = TradeState::Target1Hit;
            }).await;
            live_info(db_tx, log_tx, json!({
                "event": "SL_TRAILED",
                "instrument": ctx.instrument,
                "new_sl": new_sl,
                "qty": keep,
                "mode": "LIVE",
            })).await;

            // One-lot runner: the whole holding stays on the ladder, so there
            // is nothing to sell at target 1. Never send a zero-qty order.
            if *slice <= 0 {
                tracing::info!(instrument = %ctx.instrument, keep, new_sl, "LIVE target 1 hit — keeping the whole lot as a dynamic runner");
                return true;
            }

            // Same cross-leg cap as a full exit (see `sellable_qty`) — a
            // second tracked position on this contract can already have a
            // stop resting or a sell in flight that this slice would stack
            // on top of.
            let slice_qty = sellable_qty(positions, &pending.pos_id, &ctx.trading_symbol, *slice).await;
            if slice_qty < *slice {
                loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                    "target-1 sell trimmed from {slice} to {slice_qty} — another tracked leg on {} already has a stop resting or a sell in flight",
                    ctx.trading_symbol
                )).await;
            }
            if slice_qty <= 0 {
                tracing::info!(instrument = %ctx.instrument, "LIVE target 1 slice skipped — another tracked leg on this contract already accounts for the full holding");
                return true;
            }

            // IOC market, falling back to an IOC limit if the RMS refuses it
            // (Kotak converts F&O market orders to limits anyway; IOC keeps a
            // miss from sitting open and blocking the next exit cycle).
            match place_exit_sell(kotak, db_tx, log_tx, &ctx, cfg, ctx_ltp, slice_qty, "TGT1_PARTIAL").await {
                Some(order_id) => {
                    with_position(positions, &pending.pos_id, |p| {
                        p.pending_exit_order_id = Some(order_id);
                        p.pending_exit_qty = slice_qty;
                        p.pending_exit_reason = Some("TGT1_PARTIAL".to_string());
                    }).await;
                    tracing::info!(instrument = %ctx.instrument, slice_qty, keep, "LIVE target 1 slice placed");
                }
                None => {
                    loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                        "target-1 sell of {slice_qty} could not be placed — still holding the full {} at the trailed stop, squaring off at market",
                        ctx.executed_qty
                    )).await;
                    with_position(positions, &pending.pos_id, |p| {
                        p.force_exit = Some("TGT1_EXIT_FAILED".to_string());
                    }).await;
                }
            }
        }

        LiveAction::TrailDynamic { new_sl, next_target, rung_hit } => {
            with_position(positions, &pending.pos_id, |p| {
                p.current_sl = *new_sl;
                p.next_dynamic_target = Some(*next_target);
                p.last_dynamic_rung = Some(*rung_hit);
                p.dynamic_rung_number += 1;
            }).await;
            tracing::info!(instrument = %ctx.instrument, new_sl, next_target, "LIVE dynamic target extended");
            live_info(db_tx, log_tx, json!({
                "event": "SL_TRAILED",
                "instrument": ctx.instrument,
                "new_sl": new_sl,
                "next_target": next_target,
                "mode": "LIVE",
            })).await;
        }

        LiveAction::ExitAll { qty, reason } => {
            exec_exit_all(positions, kotak, db_tx, log_tx, cfg, ctx_ltp, &pending.pos_id, &ctx, *qty, reason).await;
        }

        LiveAction::ManualSell { qty } => {
            // Clear the request immediately so a slow fill doesn't cause it to
            // re-fire — settlement is picked up the same way as a target-1
            // slice, via pending_exit_order_id in reconcile_live_orders.
            with_position(positions, &pending.pos_id, |p| p.manual_sell_qty = None).await;

            // Same cross-leg cap as any other live sell (see `sellable_qty`).
            let qty = sellable_qty(positions, &pending.pos_id, &ctx.trading_symbol, *qty).await;
            if qty <= 0 {
                loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                    "manual sell skipped — another tracked leg on {} already accounts for the full holding",
                    ctx.trading_symbol
                )).await;
                return true;
            }

            let sell = build_market_order(&ctx.base, shared_domain::TransactionType::Sell, qty, 0.0);
            match kotak_place(kotak, &sell).await {
                Ok(order_id) => {
                    with_position(positions, &pending.pos_id, |p| {
                        p.pending_exit_order_id = Some(order_id.clone());
                        p.pending_exit_qty = qty;
                        p.pending_exit_reason = Some("MANUAL_SELL".to_string());
                    }).await;
                    tracing::info!(instrument = %ctx.instrument, %order_id, qty, "LIVE manual sell placed");
                    live_info(db_tx, log_tx, json!({
                        "event": "LIVE_EXIT_PLACED",
                        "instrument": ctx.instrument,
                        "order_id": order_id,
                        "qty": qty,
                        "reason": "MANUAL_SELL",
                        "mode": "LIVE",
                    })).await;
                }
                Err(e) => {
                    loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                        "manual sell of {qty} was rejected: {e} — still holding the full {}, nothing changed, try again",
                        ctx.executed_qty
                    )).await;
                }
            }
        }
    }

    true
}

/// One LIVE iteration: settle the broker's view, decide, act, then drop anything
/// that has finished.
async fn live_tick(
    positions: &Arc<RwLock<Vec<MonitoredPosition>>>,
    kotak: &KotakHandle,
    db_tx: &mpsc::Sender<DbWriteMessage>,
    log_tx: &broadcast::Sender<String>,
    ltp_map: &Arc<DashMap<String, f64>>,
    cfg: &TradingConfig,
    last_poll: &mut Instant,
) {
    let mut mutated = false;

    let poll_due = last_poll.elapsed() >= LIVE_POLL_INTERVAL;
    if poll_due {
        *last_poll = Instant::now();
        mutated |= reconcile_live_orders(positions, kotak, db_tx, log_tx, cfg.brokerage_per_order).await;
    }

    // ── Pre-T1 trail (bookkeeping only — no broker calls) ────────────── //
    // Protection is a software watch (see decide_live), so ratcheting the
    // stop touches no order; decide_live below reads the updated current_sl
    // in this same tick.
    if cfg.pre_t1_trailing {
        let armed: Vec<(String, f64)> = {
            let mut g = positions.write().await;
            let mut out = Vec::new();
            for p in g.iter_mut() {
                let key = p.ws_scrip_key.as_ref().unwrap_or(&p.signal.instrument_name);
                let Some(ltp) = ltp_map.get(key.as_str()).map(|r| *r).filter(|v| *v > 0.0)
                else {
                    continue;
                };
                let was_at_original = p.current_sl <= p.signal.stop_loss;
                if pre_t1_trail_update(p, ltp, cfg).is_some() {
                    mutated = true;
                    if was_at_original {
                        out.push((p.signal.instrument_name.clone(), p.current_sl));
                    }
                }
            }
            out
        };
        // Only the first ratchet above the signal SL is logged — one line per
        // 50 ms new high would drown the terminal; the dashboard shows the
        // live current_sl and the exit logs the final level.
        for (instrument, new_sl) in armed {
            live_info(db_tx, log_tx, json!({
                "event": "PRE_T1_TRAIL_ARMED",
                "instrument": instrument,
                "new_sl": round2(new_sl),
                "mode": "LIVE",
            })).await;
        }
    }

    // ── Decide (read lock only) ──────────────────────────────────────── //
    let decisions: Vec<LivePending> = {
        let entry_cutoff = is_entry_cutoff_passed();
        let g = positions.read().await;
        g.iter()
            .filter_map(|p| {
                decide_live(p, ltp_map, cfg, entry_cutoff, poll_due)
                    .map(|action| LivePending { pos_id: p.id.clone(), action })
            })
            .collect()
    };

    // ── Act (no positions lock across broker calls) ──────────────────── //
    for pending in &decisions {
        mutated |= exec_live_action(positions, kotak, db_tx, log_tx, cfg, ltp_map, pending).await;
    }

    // ── Drop closed positions, cancelling anything they still track ──── //
    let orphans: Vec<(String, String, String)> = {
        let mut g = positions.write().await;
        let mut out = Vec::new();
        for p in g.iter().filter(|p| matches!(p.state, TradeState::Closed)) {
            let ts = p
                .resolved_order
                .as_ref()
                .map(|o| o.trading_symbol.clone())
                .unwrap_or_default();
            for oid in [&p.entry_order_id, &p.sl_order_id, &p.pending_exit_order_id]
                .into_iter()
                .flatten()
            {
                out.push((oid.clone(), ts.clone(), p.signal.instrument_name.clone()));
            }
        }
        let before = g.len();
        g.retain(|p| !matches!(p.state, TradeState::Closed));
        if g.len() != before {
            mutated = true;
        }
        out
    };
    for (order_id, trading_symbol, instrument) in orphans {
        match kotak_cancel(kotak, &order_id, &trading_symbol).await {
            Ok(()) => {
                live_info(db_tx, log_tx, json!({
                    "event": "ORPHAN_ORDER_CANCELLED",
                    "instrument": instrument,
                    "order_id": order_id,
                    "mode": "LIVE",
                })).await;
            }
            Err(e) => {
                loud_error(db_tx, log_tx, &instrument, &format!(
                    "order {order_id} was still live on a closed position and could not be cancelled: {e} — check the broker terminal"
                )).await;
            }
        }
    }

    if mutated {
        let snapshot = { positions.read().await.clone() };
        send_positions_snapshot(db_tx, &snapshot).await;
    }
}

// ---------------------------------------------------------------------------
// Public position monitor
// ---------------------------------------------------------------------------

/// Stateful OMS loop — 50 ms tick, two-pass state machine.
///
/// State transitions:
/// ```text
/// WaitingForEntry ──entry─▶ Active ──SL──▶ Closed
///                                   └──TGT1 (partial)─▶ Target1Hit ──SL/TGT2──▶ Closed
/// ```
pub async fn start_position_monitor(
    mut signal_rx: broadcast::Receiver<TradeSignal>,
    db_tx: mpsc::Sender<DbWriteMessage>,
    ltp_map: Arc<DashMap<String, f64>>,
    config: Arc<RwLock<TradingConfig>>,
    positions: Arc<RwLock<Vec<MonitoredPosition>>>,
    log_tx: broadcast::Sender<String>,
    scrip_store: Arc<RwLock<Option<crate::scrip_master::ScripStore>>>,
    ws_tx: Arc<tokio::sync::Mutex<Option<mpsc::UnboundedSender<String>>>>,
    kotak: Arc<tokio::sync::Mutex<Option<KotakClient>>>,
) {
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    tracing::info!("Position monitor started");

    let mut last_live_poll = Instant::now() - Duration::from_secs(10);
    let mut startup_reconciled = false;
    // Throttles retries of a *failed* startup reconciliation so they cost the
    // same one poll per LIVE_POLL_INTERVAL as everything else, instead of
    // hammering the broker on every 50 ms engine tick (Kotak's documented
    // limit is 10 req/s across all APIs, and reconciliation alone is 2 calls).
    let mut last_reconcile_attempt = Instant::now() - LIVE_POLL_INTERVAL;

    loop {
        tokio::select! {
            result = signal_rx.recv() => {
                match result {
                    Ok(signal) => {
                        tracing::info!(
                            instrument = %signal.instrument_name,
                            action = %signal.action,
                            entry = signal.entry_price,
                            "Signal queued"
                        );

                        if signal.action == "UPDATE_SL" {
                            if let Some(ref sig_id) = signal.signal_id {
                                let mut write_guard = positions.write().await;
                                let mut updated = false;
                                for p in write_guard.iter_mut() {
                                    if p.signal.signal_id.as_ref() == Some(sig_id) {
                                        p.current_sl = signal.stop_loss;
                                        p.signal.stop_loss = signal.stop_loss;
                                        updated = true;
                                        break;
                                    }
                                }
                                if updated {
                                    drop(write_guard);
                                    let msg = format!(
                                        r#"{{"event":"SIGNAL_UPDATED","instrument":"{}","new_sl":{}}}"#,
                                        "UPDATE", signal.stop_loss
                                    );
                                    send_log(&db_tx, &log_tx, "INFO", &msg).await;
                                    let snapshot = { positions.read().await.clone() };
                                    send_positions_snapshot(&db_tx, &snapshot).await;
                                    tracing::info!(id=?sig_id, "Updated SL via reply");
                                } else {
                                    // Never drop this silently: an unmatched
                                    // SL update means a reply landed on a
                                    // message the engine holds no position
                                    // for, and the trader believes their stop
                                    // moved when nothing did.
                                    drop(write_guard);
                                    let msg = format!(
                                        r#"{{"event":"ERROR","message":"SL update ignored — no open position for the replied-to signal (new_sl {:.2})","instrument":"UPDATE"}}"#,
                                        signal.stop_loss
                                    );
                                    send_log(&db_tx, &log_tx, "ERROR", &msg).await;
                                    tracing::warn!(id=?sig_id, new_sl = signal.stop_loss, "SL update matched no position");
                                }
                            }
                            continue;
                        }

                        if signal.action == "EXIT_AT" {
                            if let Some(ref sig_id) = signal.signal_id {
                                let mut write_guard = positions.write().await;
                                let mut updated = false;
                                for p in write_guard.iter_mut() {
                                    if p.signal.signal_id.as_ref() == Some(sig_id) {
                                        if !matches!(p.state, TradeState::Closed) {
                                            // The price is advisory: PAPER books the
                                            // exit at it, LIVE exits at market and
                                            // records whatever actually filled.
                                            p.override_exit_price = Some(signal.entry_price);
                                            p.force_exit = Some(format!("EXIT_AT_{:.2}", signal.entry_price));
                                            updated = true;
                                            break;
                                        }
                                    }
                                }
                                if updated {
                                    drop(write_guard);
                                    let msg = format!(
                                        r#"{{"event":"SIGNAL_EXITED","instrument":"{}","exit_price":{:.2}}}"#,
                                        "UPDATE", signal.entry_price
                                    );
                                    send_log(&db_tx, &log_tx, "INFO", &msg).await;
                                    let snapshot = { positions.read().await.clone() };
                                    send_positions_snapshot(&db_tx, &snapshot).await;
                                    tracing::info!(id=?sig_id, price=signal.entry_price, "Triggered EXIT_AT via reply");
                                } else {
                                    // Louder than the SL case on purpose: an
                                    // exit that matched nothing means someone
                                    // asked to be flat and is still holding.
                                    drop(write_guard);
                                    let msg = format!(
                                        r#"{{"event":"ERROR","message":"Exit command ignored — no open position for the replied-to signal (exit {:.2}); square off manually if you meant to be flat","instrument":"UPDATE"}}"#,
                                        signal.entry_price
                                    );
                                    send_log(&db_tx, &log_tx, "ERROR", &msg).await;
                                    tracing::error!(id=?sig_id, price=signal.entry_price, "EXIT_AT matched no position");
                                }
                            }
                            continue;
                        }

                        // If the signal has an ID (e.g. edited Telegram message), try to update existing
                        if let Some(ref sig_id) = signal.signal_id {
                            let mut write_guard = positions.write().await;
                            let mut updated = false;
                            for p in write_guard.iter_mut() {
                                if p.signal.signal_id.as_ref() == Some(sig_id) {
                                    if p.signal.entry_price != signal.entry_price || p.signal.entry_condition != signal.entry_condition {
                                        p.force_exit = Some("ENTRY_CHANGED_ERROR".to_string());
                                        updated = true;
                                        break;
                                    }

                                    p.signal.stop_loss = signal.stop_loss;
                                    p.signal.targets = signal.targets.clone();
                                    p.signal.entry_price = signal.entry_price;
                                    
                                    // Update active trailing SL if not hit TGT1 yet
                                    if matches!(p.state, TradeState::WaitingForEntry | TradeState::Active) {
                                        p.current_sl = signal.stop_loss;
                                    }
                                    
                                    // Can't await inside sync context? Wait, we are in an async loop and write_guard is across await point!
                                    // So let's drop guard first.
                                    updated = true;
                                    break;
                                }
                            }
                            if updated {
                                drop(write_guard);
                                let msg = format!(
                                    r#"{{"event":"SIGNAL_UPDATED","instrument":"{}","new_sl":{}}}"#,
                                    signal.instrument_name, signal.stop_loss
                                );
                                send_log(&db_tx, &log_tx, "INFO", &msg).await;
                                let snapshot = { positions.read().await.clone() };
                                send_positions_snapshot(&db_tx, &snapshot).await;
                                tracing::info!(id=?sig_id, "Updated existing signal");
                                continue;
                            }
                        }

                        if signal.action.eq_ignore_ascii_case("BUY") {
                            // A second BUY that exactly repeats a contract +
                            // entry already open or waiting — see
                            // `find_duplicate_open_position`. Never open a
                            // second position for it, but a resend can still
                            // carry a genuinely revised SL/target (the same
                            // channel correcting itself under a new message,
                            // so signal_id doesn't match either) — apply that
                            // in place, the same way an edited-message resend
                            // is applied above, rather than silently dropping
                            // the revision with only a WARN.
                            let dup_id = {
                                let g = positions.read().await;
                                find_duplicate_open_position(&g, &signal).map(|p| p.id.clone())
                            };
                            if let Some(dup_id) = dup_id {
                                let mut revised = false;
                                {
                                    let mut g = positions.write().await;
                                    if let Some(p) = g.iter_mut().find(|p| p.id == dup_id) {
                                        if (p.signal.stop_loss - signal.stop_loss).abs() > f64::EPSILON
                                            || p.signal.targets != signal.targets
                                        {
                                            p.signal.stop_loss = signal.stop_loss;
                                            p.signal.targets = signal.targets.clone();
                                            if matches!(p.state, TradeState::WaitingForEntry | TradeState::Active) {
                                                p.current_sl = signal.stop_loss;
                                            }
                                            revised = true;
                                        }
                                    }
                                }
                                if revised {
                                    let msg = format!(
                                        r#"{{"event":"SIGNAL_UPDATED","instrument":"{}","new_sl":{}}}"#,
                                        signal.instrument_name, signal.stop_loss
                                    );
                                    send_log(&db_tx, &log_tx, "INFO", &msg).await;
                                    let snapshot = { positions.read().await.clone() };
                                    send_positions_snapshot(&db_tx, &snapshot).await;
                                    tracing::info!(
                                        instrument = %signal.instrument_name,
                                        "Resend of an already-tracked contract carried a revised SL/target — applied in place instead of opening a duplicate"
                                    );
                                } else {
                                    let msg = format!(
                                        r#"{{"event":"DUPLICATE_SIGNAL_SKIPPED","message":"Signal skipped — an open or waiting position already exists for this exact contract and entry","instrument":"{}"}}"#,
                                        signal.instrument_name
                                    );
                                    send_log(&db_tx, &log_tx, "WARN", &msg).await;
                                    tracing::warn!(
                                        instrument = %signal.instrument_name,
                                        strike = ?signal.strike,
                                        entry = signal.entry_price,
                                        "Duplicate BUY signal skipped — identical contract + entry already tracked"
                                    );
                                }
                                continue;
                            }

                            // A signal with no target is not tradeable, whatever
                            // it parsed out of — there is nothing to exit on.
                            // Refuse it rather than open a position that can
                            // only ever be closed by the expiry square-off.
                            if signal.targets.is_empty() {
                                let msg = format!(
                                    r#"{{"event":"ERROR","message":"Signal discarded — no target parsed; refusing to open an unprotected position","instrument":"{}"}}"#,
                                    signal.instrument_name
                                );
                                send_log(&db_tx, &log_tx, "ERROR", &msg).await;
                                tracing::error!(
                                    instrument = %signal.instrument_name,
                                    "Signal discarded — missing target"
                                );
                                continue;
                            }

                            // `stop_loss <= 0.0` ("HERO-ZERO" calls — SL :- 0,
                            // trade the full premium with no stop) is allowed
                            // through deliberately: an LTP can never cross 0, so
                            // the software stop never fires, and the position is
                            // protected only by its target(s) and the expiry-day
                            // square-off (`is_expiry_squareoff_due`, 15:38 IST).
                            // Note this can't tell an explicit "SL :- 0" apart
                            // from a message whose SL line just failed to parse
                            // — both default to 0.0 the same way — so log it
                            // loudly rather than silently.
                            if signal.stop_loss <= 0.0 {
                                let msg = format!(
                                    r#"{{"event":"WARN","message":"Signal has no stop-loss (SL :- 0 or unparsed) — opening unprotected, exits only on target or the 15:38 IST expiry square-off","instrument":"{}"}}"#,
                                    signal.instrument_name
                                );
                                send_log(&db_tx, &log_tx, "WARN", &msg).await;
                                tracing::warn!(
                                    instrument = %signal.instrument_name,
                                    "Signal accepted with stop_loss <= 0 — unprotected until target or expiry square-off"
                                );
                            }

                            // Check expiry
                            if let Some(ref expiry_str) = signal.expiry {
                                if let Ok(exp_date) = chrono::NaiveDate::parse_from_str(expiry_str, "%d-%b-%Y") {
                                    let today = shared_domain::today_ist();
                                    if exp_date < today {
                                        let msg = format!(
                                            r#"{{"event":"ERROR","message":"Parsed expiry ({}) is in the past","instrument":"{}"}}"#,
                                            expiry_str, signal.instrument_name
                                        );
                                        send_log(&db_tx, &log_tx, "ERROR", &msg).await;
                                        tracing::error!(
                                            instrument = %signal.instrument_name, 
                                            expiry = %expiry_str, 
                                            "Signal discarded — expiry in past"
                                        );
                                        continue;
                                    }
                                }
                            }

                            // Check kill switch before parsing scrips or opening any new positions
                            if config.read().await.kill_switch_active {
                                let msg = format!(
                                    r#"{{"event":"KILL_SWITCH_BLOCKED","message":"Signal dropped — kill switch is active","instrument":"{}"}}"#,
                                    signal.instrument_name
                                );
                                send_log(&db_tx, &log_tx, "WARN", &msg).await;
                                tracing::warn!(instrument = %signal.instrument_name, "Signal dropped — kill switch is active");
                                continue;
                            }

                            // Lot count set to 0 for this instrument's class means
                            // "don't auto-trade it" — an index with no per-symbol
                            // override, or any stock option when `other_lots` is 0.
                            // Drop the signal here rather than open a position that
                            // can only ever size to 0. Equity signals size off
                            // notional, not lots, so they are exempt.
                            if signal.option_type.is_some() {
                                let auto_lots = { lots_for_instrument(&*config.read().await, &signal.instrument_name) };
                                if auto_lots <= 0 {
                                    let msg = format!(
                                        r#"{{"event":"ERROR","message":"Signal skipped — lot count for {} is 0; raise Index/Other Lots to trade it","instrument":"{}"}}"#,
                                        signal.instrument_name, signal.instrument_name
                                    );
                                    send_log(&db_tx, &log_tx, "ERROR", &msg).await;
                                    tracing::info!(instrument = %signal.instrument_name, "Signal skipped — lot count is 0");
                                    continue;
                                }
                            }

                            // "Already at target 1" is enforced in two places that
                            // actually have a price for the specific option: the
                            // pre-check below (keyed by the resolved scrip) and,
                            // as the real backstop, every tick at the entry
                            // trigger in both PAPER and LIVE. It was previously
                            // checked here against `ltp_map[instrument_name]`,
                            // which is never a key — that map only ever holds
                            // `segment|token` — so the guard never fired.
                            {
                                let scrip_guard = scrip_store.read().await;
                                let mut resolved_order = None;
                                let mut resolved_token = None;
                                let mut resolved_segment_code = None;
                                let mut resolved_tick_size = 0.05;
                                let mut resolved_expiry = None;
                                if let Some(ref store) = *scrip_guard {
                                    if let Some(record) = store.resolve_signal(&signal) {
                                        // Build OrderRequest
                                        use shared_domain::{OrderRequest, AmoFlag, ExchangeSegment, ProductCode, OrderType, Validity, TransactionType};
                                        let qty = record.lot_size.to_string(); // we use lot size by default
                                        resolved_token = Some(record.instrument_token.clone());
                                        resolved_segment_code = Some(record.exchange_segment_code.clone());
                                        resolved_tick_size = record.tick_size;
                                        resolved_expiry = Some(record.expiry_date);
                                        let exchange_segment = match record.exchange_segment_code.as_str() {
                                            "bse_fo" => ExchangeSegment::BseFo,
                                            "nse_cm" => ExchangeSegment::NseCm,
                                            _ => ExchangeSegment::NseFo,
                                        };
                                        resolved_order = Some(OrderRequest {
                                            after_market_order: AmoFlag::No,
                                            disclosed_quantity: "0".to_string(),
                                            exchange_segment,
                                            market_protection: "0".to_string(),
                                            product_code: ProductCode::Nrml,
                                            portfolio_flag: "N".to_string(),
                                            price: "0".to_string(),
                                            order_type: OrderType::Limit,
                                            quantity: qty,
                                            validity: Validity::Day,
                                            trigger_price: "0".to_string(),
                                            trading_symbol: record.trading_symbol.clone(),
                                            transaction_type: TransactionType::Buy,
                                        })
                                    }
                                }

                                if scrip_guard.is_none() {
                                    let msg = format!(
                                        r#"{{"event":"ERROR","message":"Signal discarded — Scrip Master not loaded","instrument":"{}"}}"#,
                                        signal.instrument_name
                                    );
                                    send_log(&db_tx, &log_tx, "ERROR", &msg).await;
                                    tracing::error!(instrument = %signal.instrument_name, "Signal discarded — Scrip Master not loaded");
                                    continue;
                                }

                                if resolved_order.is_none() {
                                    // We have the store but couldn't resolve the order!
                                    let msg = format!(
                                        r#"{{"event":"ERROR","message":"Could not resolve contract in Scrip Master","instrument":"{}"}}"#,
                                        signal.instrument_name
                                    );
                                    send_log(&db_tx, &log_tx, "ERROR", &msg).await;
                                    tracing::error!(instrument = %signal.instrument_name, "Signal discarded — not found in Scrip Master");
                                    continue;
                                }

                                let sl = signal.stop_loss;
                                
                                let mut ws_key = None;
                                if let Some(token) = resolved_token {
                                    let segment_code = resolved_segment_code.unwrap_or_else(|| "nse_fo".to_string());
                                    let ws_scrip = format!("{}|{}", segment_code, token);

                                    // If this exact contract is already streaming
                                    // (a second position on it, or a re-sent
                                    // signal) and its premium is already at/through
                                    // target 1, there is no trade left — drop the
                                    // signal rather than open a position that
                                    // exits on its first tick.
                                    if let Some(px) = ltp_map.get(ws_scrip.as_str()).map(|r| *r).filter(|v| *v > 0.0) {
                                        if let Some(t1) = signal.targets.first().copied() {
                                            if px >= t1 {
                                                let msg = format!(
                                                    r#"{{"event":"ERROR","message":"Signal discarded — already at target 1 ({:.2} ≥ {:.2}), wrong contract or stale signal","instrument":"{}","price":{:.2}}}"#,
                                                    px, t1, signal.instrument_name, px
                                                );
                                                send_log(&db_tx, &log_tx, "ERROR", &msg).await;
                                                tracing::error!(instrument = %signal.instrument_name, price = px, tgt1 = t1, "Signal discarded — already at target 1");
                                                continue;
                                            }
                                        }
                                    }

                                    ltp_map.entry(ws_scrip.clone()).or_insert(0.0);
                                    tracing::info!("Requested live price stream for {}", ws_scrip);
                                    
                                    let tx_guard = ws_tx.lock().await;
                                    if let Some(tx) = tx_guard.as_ref() {
                                        let payload = json!({
                                            "action": "subscribe",
                                            "scrips": ws_scrip
                                        });
                                        let _ = tx.send(payload.to_string());
                                    }
                                    ws_key = Some(ws_scrip);
                                }

                                drop(scrip_guard);

                                // No order is sent here in either mode. The entry
                                // trigger is watched on the LTP feed and, in LIVE,
                                // filled with a market buy at that moment — a
                                // resting stop-buy would sit at the broker with no
                                // way for us to withdraw it in time.

                                let mut pos_guard = positions.write().await;
                                // Automatically exit existing opposite positions for this instrument (e.g., CE trade when PE signal arrives)
                                for p in pos_guard.iter_mut() {
                                    if p.signal.instrument_name.eq_ignore_ascii_case(&signal.instrument_name)
                                        && !matches!(p.state, TradeState::Closed)
                                    {
                                        let is_opposite_option = p.signal.option_type.is_some()
                                            && signal.option_type.is_some()
                                            && p.signal.option_type != signal.option_type;
                                        let is_opposite_action = p.signal.action != signal.action;

                                        if is_opposite_option || is_opposite_action {
                                            tracing::info!(
                                                instrument = %signal.instrument_name,
                                                old_id = %p.id,
                                                old_type = ?p.signal.option_type,
                                                new_type = ?signal.option_type,
                                                "Exiting existing opposite trade due to new signal"
                                            );
                                            p.force_exit = Some("OPPOSITE_SIGNAL_EXIT".to_string());
                                        }
                                    }
                                }

                                pos_guard.push(MonitoredPosition {
                                    id: uuid::Uuid::new_v4().to_string(),
                                    signal,
                                    state: TradeState::WaitingForEntry,
                                    created_at: shared_domain::current_ist_timestamp_string(),
                                    current_sl: sl,
                                    peak_ltp: None,
                                    next_dynamic_target: None,
                                    last_dynamic_rung: None,
                                    dynamic_rung_number: 0,
                                    manual_sell_qty: None,
                                    executed_qty: 0,
                                    avg_buy_price: 0.0,
                                    override_qty: None,
                                    resolved_order,
                                    resolved_expiry,
                                    ltp: None,
                                    ws_scrip_key: ws_key,
                                    force_exit: None,
                                    override_exit_price: None,
                                    tick_size: resolved_tick_size,
                                    entry_order_id: None,
                                    sl_order_id: None,
                                    sl_order_qty: 0,
                                    sl_order_trigger: 0.0,
                                    target_order_id: None,
                                    pending_exit_order_id: None,
                                    pending_exit_qty: 0,
                                    pending_exit_reason: None,
                                    entry_cancel_sent: false,
                                    exit_attempts: 0,
                                    entry_attempts: 0,
                                    entry_attempt_at: None,
                                    entry_send_uncertain: false,
                                    entry_uncertain_qty: None,
                                    entry_retry_as_limit: false,
                                    live_halt: None,
                                    entry_zone: None,
                                });
                                let snapshot = pos_guard.clone();
                                drop(pos_guard);
                                send_positions_snapshot(&db_tx, &snapshot).await;
                            }
                        } else {
                            tracing::warn!(
                                instrument = %signal.instrument_name,
                                "SELL/short signals not yet implemented"
                            );
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Signal receiver lagged — dropped {n}");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::info!("Signal channel closed — monitor exiting");
                        break;
                    }
                }
            }

            _ = tick.tick() => {
                // LIVE runs its own state machine end to end — real orders, real
                // fills, no simulated prices anywhere. PAPER keeps the original
                // two-pass loop below untouched.
                let live_cfg = {
                    let c = config.read().await;
                    (c.mode == "LIVE").then(|| c.clone())
                };
                if let Some(cfg) = live_cfg {
                    // Reconcile against the broker once, on the first LIVE tick that
                    // has a session to talk to. Deferring it until the session exists
                    // means it still runs when the user logs in after the monitor
                    // starts, or flips PAPER → LIVE mid-day.
                    if !startup_reconciled && last_reconcile_attempt.elapsed() >= LIVE_POLL_INTERVAL {
                        last_reconcile_attempt = Instant::now();
                        let has_session = { kotak.lock().await.is_some() };
                        if has_session {
                            startup_reconciled = reconcile_on_startup(&positions, &kotak, &db_tx, &log_tx, cfg.brokerage_per_order).await;
                        }
                    }
                    // Never trade on unverified state: if reconciliation hasn't
                    // succeeded yet (no session, or the broker call failed), skip
                    // live_tick this tick and retry reconciliation on the next one
                    // instead of silently trading blind.
                    if !startup_reconciled { continue; }
                    if positions.read().await.is_empty() { continue; }
                    live_tick(&positions, &kotak, &db_tx, &log_tx, &ltp_map, &cfg, &mut last_live_poll).await;
                    continue;
                }

                let mut pos_guard = positions.write().await;
                if pos_guard.is_empty() { continue; }

                let cfg = config.read().await;
                let entry_cutoff = is_entry_cutoff_passed();
                let mut pending: Vec<Pending> = Vec::new();
                let mut positions_mutated = false;

                // ── Pre-T1 trail (bookkeeping before Pass 1 reads SL) ── //
                if cfg.pre_t1_trailing {
                    for pos in pos_guard.iter_mut() {
                        let key = pos.ws_scrip_key.as_ref().unwrap_or(&pos.signal.instrument_name);
                        let Some(ltp) = ltp_map.get(key.as_str()).map(|r| *r).filter(|v| *v > 0.0)
                        else {
                            continue;
                        };
                        let was_at_original = pos.current_sl <= pos.signal.stop_loss;
                        let instrument = pos.signal.instrument_name.clone();
                        if let Some(new_sl) = pre_t1_trail_update(pos, ltp, &cfg) {
                            positions_mutated = true;
                            // Only the first ratchet above the signal SL is
                            // logged — see the LIVE counterpart in live_tick.
                            if was_at_original {
                                send_log(&db_tx, &log_tx, "INFO", &format!(
                                    r#"{{"event":"PRE_T1_TRAIL_ARMED","instrument":"{instrument}","new_sl":{new_sl:.2}}}"#
                                )).await;
                            }
                        }
                    }
                }

                // ── Pass 1: read-only scan ────────────────────────── //
                for (i, pos) in pos_guard.iter().enumerate() {
                    // Entries that will never be taken are dropped before the LTP
                    // guard below — a cancelled or rewritten signal, or the 15:39
                    // cutoff after which we do not open anything new.
                    if matches!(pos.state, TradeState::WaitingForEntry) {
                        if cfg.kill_switch_active {
                            pending.push(Pending {
                                idx: i,
                                ltp: 0.0,
                                action: PosAction::Expire { reason: "KILL_SWITCH_ACTIVE".to_string() },
                            });
                            continue;
                        }
                        if let Some(reason) = pos.force_exit.clone() {
                            pending.push(Pending { idx: i, ltp: 0.0, action: PosAction::Expire { reason } });
                            continue;
                        }
                        if let Some(reason) = stale_entry_reason(pos, entry_cutoff) {
                            pending.push(Pending {
                                idx: i, ltp: 0.0,
                                action: PosAction::Expire { reason: reason.to_string() },
                            });
                            continue;
                        }
                    }

                    let lookup_key = pos.ws_scrip_key.as_ref().unwrap_or(&pos.signal.instrument_name);
                    // Skip if no price available yet OR if the price is the 0.0
                    // placeholder inserted on startup for carried-over positions.
                    // A real live tick will overwrite 0.0 before we act on anything.
                    let ltp = match ltp_map.get(lookup_key).map(|r| *r) {
                        Some(v) if v > 0.0 => v,
                        _ => continue,
                    };

                    // Expiry-day square-off: at/after 15:38 IST on the option's
                    // resolved expiry day, force-close any still-open position at
                    // market so it is never carried into expiry/settlement.
                    let pa = if matches!(pos.state, TradeState::Active | TradeState::Target1Hit)
                        && pos.force_exit.is_none()
                        && is_expiry_squareoff_due(pos, shared_domain::now_ist())
                    {
                        Some(PosAction::ExitSell {
                            qty: pos.executed_qty,
                            reason: "EXPIRY_SQUAREOFF".to_string(),
                            new_sl: None,
                            exec_price: None,
                        })
                    } else {
                    match pos.state {
                        TradeState::WaitingForEntry => {
                            entry_triggered(&pos.signal, ltp, &cfg).then(|| {
                                // The signal's move is already spent — entering
                                // here just books target 1 on the next tick.
                                // Usually a mis-resolved contract or a stale
                                // signal; refuse it rather than take the trade.
                                if let Some(t1) = entry_past_target1(&pos.signal, ltp) {
                                    return PosAction::Cancel {
                                        reason: format!(
                                            "entry {ltp:.2} is already at/through target 1 ({t1:.2}) — wrong contract or stale signal, not entering"
                                        ),
                                    };
                                }

                                let lot_size = pos
                                    .resolved_order
                                    .as_ref()
                                    .and_then(|o| o.quantity.parse::<i32>().ok())
                                    .filter(|v| *v > 0)
                                    .unwrap_or(1);

                                let qty = compute_entry_qty(&pos.signal, lot_size, pos.override_qty, &cfg, Some(ltp));

                                if qty <= 0 || qty % lot_size != 0 {
                                    PosAction::Cancel {
                                        reason: format!("Invalid quantity {}, must be positive multiple of lot size {}", qty, lot_size)
                                    }
                                } else {
                                    PosAction::EntryBuy { qty }
                                }
                            })
                        }

                        TradeState::Active => {
                            if let Some(ref reason) = pos.force_exit {
                                Some(PosAction::ExitSell {
                                    qty: pos.executed_qty, reason: reason.clone(), new_sl: None, exec_price: pos.override_exit_price,
                                })
                            } else if ltp <= pos.current_sl {
                                // A stop above the signal's SL while still
                                // Active means the pre-T1 trail moved it.
                                let reason = if pos.current_sl > pos.signal.stop_loss {
                                    "TRAIL_SL_HIT"
                                } else {
                                    "SL_HIT"
                                };
                                Some(PosAction::ExitSell {
                                    qty: pos.executed_qty, reason: reason.to_string(), new_sl: None, exec_price: None,
                                })
                            } else if !pos.signal.targets.is_empty() && ltp >= pos.signal.targets[0] {
                                let has_t2 = pos.signal.targets.len() > 1;
                                let lot_size = pos
                                    .resolved_order
                                    .as_ref()
                                    .and_then(|o| o.quantity.parse::<i32>().ok())
                                    .filter(|v| *v > 0)
                                    .unwrap_or(1);

                                let raw_exit_qty = ((pos.executed_qty as f64 * cfg.target_1_exit_pct / 100.0)
                                    .floor() as i32).max(1).min(pos.executed_qty);
                                
                                let mut exit_qty = (raw_exit_qty / lot_size) * lot_size;
                                if exit_qty == 0 && pos.executed_qty >= lot_size {
                                    exit_qty = lot_size;
                                } else if exit_qty == 0 {
                                    exit_qty = pos.executed_qty;
                                }

                                let new_sl = has_t2.then(|| {
                                    let raw = (pos.avg_buy_price + pos.signal.targets[0]) / 2.0;
                                    (raw / pos.tick_size).floor() * pos.tick_size
                                });
                                Some(PosAction::ExitSell {
                                    qty: exit_qty,
                                    reason: (if has_t2 { "TGT1_PARTIAL" } else { "TGT1_FULL" }).to_string(),
                                    new_sl,
                                    exec_price: None,
                                })
                            } else { None }
                        }

                        TradeState::Target1Hit => {
                            if let Some(ref reason) = pos.force_exit {
                                Some(PosAction::ExitSell {
                                    qty: pos.executed_qty, reason: reason.clone(), new_sl: None, exec_price: pos.override_exit_price,
                                })
                            } else if ltp <= pos.current_sl {
                                Some(PosAction::ExitSell {
                                    qty: pos.executed_qty, reason: "TRAIL_SL_HIT".to_string(), new_sl: None, exec_price: None,
                                })
                            } else if pos.signal.targets.len() > 1 && ltp >= pos.signal.targets[1] {
                                Some(PosAction::ExitSell {
                                    qty: pos.executed_qty, reason: "TGT2_HIT".to_string(), new_sl: None, exec_price: None,
                                })
                            } else { None }
                        }

                        TradeState::Closed => None,
                    }
                    };

                    if let Some(a) = pa {
                        pending.push(Pending { idx: i, ltp, action: a });
                    }
                }

                // ── Pass 2: apply + async sends ───────────────────── //
                for pa in pending {
                    let pos = &mut pos_guard[pa.idx];
                    if matches!(pos.state, TradeState::Closed) { continue; }

                    let is_options = pos.signal.option_type.is_some();
                    let instrument = pos.signal.instrument_name.clone();

                    match pa.action {
                        PosAction::EntryBuy { qty } => {
                            let fees = FeeCalculator::calculate(
                                qty, pa.ltp, "BUY", is_options, cfg.brokerage_per_order,
                            );
                            pos.avg_buy_price = pa.ltp;
                            pos.executed_qty   = qty;
                            pos.state          = TradeState::Active;
                            positions_mutated = true;

                            let msg = format!(
                                r#"{{"event":"ENTRY","instrument":"{instrument}","price":{:.2},"qty":{qty},"net_cost":{:.2}}}"#,
                                pa.ltp, fees.net_value
                            );
                            tracing::info!(instrument = %instrument, price = pa.ltp, qty, "Entry executed");
                            send_trade(&db_tx, &instrument, "BUY", qty, pa.ltp, &fees, pos.signal.signal_id.clone(), pos.signal.raw_message.clone(), Some("ENTRY".to_string()), &cfg.mode).await;
                            send_log(&db_tx, &log_tx, "INFO", &msg).await;
                        }

                        PosAction::ExitSell { qty, reason, new_sl, exec_price } => {
                            let price = exec_price.unwrap_or(pa.ltp);
                            let fees = FeeCalculator::calculate(
                                qty, price, "SELL", is_options, cfg.brokerage_per_order,
                            );
                            let pnl = fees.net_value - pos.avg_buy_price * qty as f64;
                            let msg = format!(
                                r#"{{"event":"{reason}","instrument":"{instrument}","price":{:.2},"qty":{qty},"pnl":{pnl:.2}}}"#,
                                price
                            );
                            tracing::info!(instrument = %instrument, reason, price, pnl, "Exit executed");
                            send_trade(&db_tx, &instrument, "SELL", qty, price, &fees, pos.signal.signal_id.clone(), pos.signal.raw_message.clone(), Some(reason.clone()), &cfg.mode).await;
                            send_log(&db_tx, &log_tx, "INFO", &msg).await;

                            pos.executed_qty -= qty;
                            match new_sl {
                                Some(sl) => {
                                    pos.current_sl = sl;
                                    pos.state = TradeState::Target1Hit;
                                    send_log(&db_tx, &log_tx, "INFO", &format!(
                                        r#"{{"event":"SL_TRAILED","instrument":"{instrument}","new_sl":{sl:.2}}}"#
                                    )).await;
                                }
                                None => pos.state = TradeState::Closed,
                            }
                            positions_mutated = true;
                        }

                        PosAction::Cancel { reason } => {
                            pos.state = TradeState::Closed;
                            positions_mutated = true;
                            let msg = format!(
                                r#"{{"event":"ERROR","instrument":"{}","message":"Trade cancelled: {}"}}"#,
                                instrument, reason
                            );
                            tracing::error!(instrument = %instrument, reason = %reason, "Trade cancelled");
                            send_log(&db_tx, &log_tx, "ERROR", &msg).await;
                        }

                        PosAction::Expire { reason } => {
                            pos.state = TradeState::Closed;
                            positions_mutated = true;
                            let msg = format!(
                                r#"{{"event":"ENTRY_ABANDONED","instrument":"{}","reason":"{}"}}"#,
                                instrument, reason
                            );
                            tracing::info!(instrument = %instrument, reason = %reason, "Waiting position dropped");
                            send_log(&db_tx, &log_tx, "INFO", &msg).await;
                        }
                    }
                }

                // ── Pass 3: remove closed ─────────────────────────── //
                let before = pos_guard.len();
                pos_guard.retain(|p| !matches!(p.state, TradeState::Closed));
                let removed = before - pos_guard.len();
                if removed > 0 {
                    positions_mutated = true;
                    tracing::debug!("Removed {removed} closed position(s)");
                }

                if positions_mutated {
                    let snapshot = pos_guard.clone();
                    drop(pos_guard);
                    send_positions_snapshot(&db_tx, &snapshot).await;
                }
            }
        }
    }

    tracing::info!("Position monitor stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position_created_at(created_at: &str) -> MonitoredPosition {
        MonitoredPosition {
            id: "test".to_string(),
            signal: TradeSignal {
                instrument_name: "NIFTY".to_string(),
                strike: Some(25000.0),
                option_type: Some("CE".to_string()),
                expiry: None,
                action: "BUY".to_string(),
                entry_condition: "ABOVE".to_string(),
                entry_price: 120.0,
                targets: vec![140.0, 160.0],
                stop_loss: 100.0,
                source: "test".to_string(),
                signal_id: None,
                raw_message: None,
            },
            state: TradeState::WaitingForEntry,
            created_at: created_at.to_string(),
            current_sl: 100.0,
            peak_ltp: None,
            next_dynamic_target: None,
            last_dynamic_rung: None,
            dynamic_rung_number: 0,
            manual_sell_qty: None,
            executed_qty: 0,
            avg_buy_price: 0.0,
            override_qty: None,
            resolved_order: None,
            resolved_expiry: None,
            ltp: None,
            ws_scrip_key: None,
            force_exit: None,
            override_exit_price: None,
            tick_size: 0.05,
            entry_order_id: None,
            sl_order_id: None,
            sl_order_qty: 0,
            sl_order_trigger: 0.0,
            target_order_id: None,
            pending_exit_order_id: None,
            pending_exit_qty: 0,
            pending_exit_reason: None,
            entry_cancel_sent: false,
            exit_attempts: 0,
            entry_attempts: 0,
            entry_attempt_at: None,
            entry_send_uncertain: false,
            entry_uncertain_qty: None,
            entry_retry_as_limit: false,
            live_halt: None,
            entry_zone: None,
        }
    }

    fn matching_signal() -> TradeSignal {
        // Mirrors position_created_at's signal exactly.
        TradeSignal {
            instrument_name: "NIFTY".to_string(),
            strike: Some(25000.0),
            option_type: Some("CE".to_string()),
            expiry: None,
            action: "BUY".to_string(),
            entry_condition: "ABOVE".to_string(),
            entry_price: 120.0,
            targets: vec![140.0, 160.0],
            stop_loss: 100.0,
            source: "test".to_string(),
            signal_id: Some("different-message-id".to_string()),
            raw_message: None,
        }
    }

    #[test]
    fn duplicate_signal_skipped_when_contract_and_entry_match() {
        // The actual incident: a channel resent the identical call under a
        // second Telegram message a moment later — different signal_id, same
        // contract and entry — and it opened a second real position on top
        // of the first, doubling the broker exposure with no way to tell the
        // two apart afterwards.
        let positions = vec![position_created_at("2026-09-16 15:19:29")];
        assert!(find_duplicate_open_position(&positions, &matching_signal()).is_some());
    }

    #[test]
    fn revised_entry_on_the_same_contract_is_not_a_duplicate() {
        // A genuinely different call on the same strike (new entry price)
        // must still get through — this is a real trade, not a resend.
        let positions = vec![position_created_at("2026-09-17 09:00:00")];
        let mut sig = matching_signal();
        sig.entry_price = 90.0;
        assert!(find_duplicate_open_position(&positions, &sig).is_none());
    }

    #[test]
    fn a_closed_position_does_not_block_a_repeat_signal() {
        // Once the earlier trade is done, a fresh identical call later in
        // the day is a new trade, not a duplicate.
        let mut pos = position_created_at("2026-09-17 09:00:00");
        pos.state = TradeState::Closed;
        assert!(find_duplicate_open_position(&[pos], &matching_signal()).is_none());
    }

    #[test]
    fn different_strike_is_not_a_duplicate() {
        let positions = vec![position_created_at("2026-09-17 09:00:00")];
        let mut sig = matching_signal();
        sig.strike = Some(25100.0);
        assert!(find_duplicate_open_position(&positions, &sig).is_none());
    }

    fn cfg_with_lots(index_lots: i32, other_lots: i32) -> TradingConfig {
        TradingConfig {
            max_trade_amount_inr: 15_000.0,
            index_lots,
            other_lots,
            index_lots_by_symbol: Default::default(),
            mode: "PAPER".into(),
            brokerage_per_order: 20.0,
            target_1_exit_pct: 50.0,
            target_2_exit_pct: 100.0,
            entry_market_protection: 5.0,
            dynamic_targeting: false,
            dynamic_targeting_trail_factor: 0.5,
            dynamic_targeting_extension_factor: 1.0,
            dynamic_targeting_single_lot: false,
            pre_t1_trailing: false,
            pre_t1_trail_arm_pct: 60.0,
            pre_t1_trail_factor: 0.5,
            kill_switch_active: false,
            entry_window_pct: 0.0,
        }
    }

    #[test]
    fn lot_count_of_zero_means_do_not_trade_that_class() {
        // Bare 0 skips the whole class...
        assert_eq!(lots_for_instrument(&cfg_with_lots(0, 0), "NIFTY"), 0);
        assert_eq!(lots_for_instrument(&cfg_with_lots(0, 0), "RELIANCE"), 0);
        // ...but a positive per-index override still wins for that index.
        let mut cfg = cfg_with_lots(0, 0);
        cfg.index_lots_by_symbol.insert("BANKNIFTY".into(), 2);
        assert_eq!(lots_for_instrument(&cfg, "BANKNIFTY"), 2);
        assert_eq!(lots_for_instrument(&cfg, "NIFTY"), 0);
        // A negative from a hand-edited DB row is floored to 0, never negative.
        assert_eq!(lots_for_instrument(&cfg_with_lots(-3, -1), "NIFTY"), 0);
        // Normal positive config is unchanged.
        assert_eq!(lots_for_instrument(&cfg_with_lots(1, 3), "NIFTY"), 1);
        assert_eq!(lots_for_instrument(&cfg_with_lots(1, 3), "TATASTEEL"), 3);
    }

    #[test]
    fn manual_adopt_prefers_the_entered_buy_price_over_the_broker_average() {
        // The user typed a real per-lot fill price — use it verbatim.
        assert_eq!(resolved_adopt_avg(175.50, 182.30), 175.50);
        // Blank / junk falls back to the broker's same-day VWAP.
        assert_eq!(resolved_adopt_avg(0.0, 182.30), 182.30);
        assert_eq!(resolved_adopt_avg(-4.0, 182.30), 182.30);
    }

    #[test]
    fn stale_entry_reason_past_cutoff_always_expires() {
        // 15:39 cutoff wins regardless of created_at, including a position
        // created moments ago today.
        let pos = position_created_at(&shared_domain::current_ist_timestamp_string());
        assert_eq!(stale_entry_reason(&pos, true), Some("EOD_NO_ENTRY"));
    }

    #[test]
    fn stale_entry_reason_same_day_is_not_stale() {
        let pos = position_created_at(&shared_domain::current_ist_timestamp_string());
        assert_eq!(stale_entry_reason(&pos, false), None);
    }

    #[test]
    fn stale_entry_reason_flags_carryover_from_a_previous_day() {
        // A position that survived a restart into a new calendar day must be
        // abandoned rather than left to trigger on a stale reference price.
        let yesterday = shared_domain::today_ist() - chrono::Duration::days(1);
        let created_at = yesterday.and_hms_opt(10, 0, 0).unwrap().format("%Y-%m-%d %H:%M:%S").to_string();
        let pos = position_created_at(&created_at);
        assert_eq!(stale_entry_reason(&pos, false), Some("STALE_CARRYOVER"));
    }

    #[test]
    fn stale_entry_reason_flags_missing_created_at() {
        // Positions persisted before this field existed carry an empty
        // string (serde default) — treat as stale rather than guess their age.
        let pos = position_created_at("");
        assert_eq!(stale_entry_reason(&pos, false), Some("STALE_CARRYOVER"));
    }

    /// A fixed IST instant for `is_expiry_squareoff_due` tests.
    fn ist(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> chrono::DateTime<chrono::FixedOffset> {
        chrono::NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, mi, s)
            .unwrap()
            .and_local_timezone(shared_domain::ist_offset())
            .unwrap()
    }

    #[test]
    fn expiry_squareoff_never_due_without_a_resolved_expiry() {
        // A position adopted from the broker with no Scrip Master lookup
        // (`resolved_expiry: None`) never guesses at an expiry — never due,
        // whatever the clock says.
        let pos = position_created_at("2026-09-17 09:00:00");
        assert!(pos.resolved_expiry.is_none());
        assert!(!is_expiry_squareoff_due(&pos, ist(2026, 9, 17, 15, 50, 0)));
    }

    #[test]
    fn expiry_squareoff_fires_at_1538_on_the_resolved_expiry_day() {
        let mut pos = position_created_at("2026-09-17 09:00:00");
        pos.resolved_expiry = Some(chrono::NaiveDate::from_ymd_opt(2026, 9, 17).unwrap());

        assert!(!is_expiry_squareoff_due(&pos, ist(2026, 9, 17, 15, 37, 59)));
        assert!(is_expiry_squareoff_due(&pos, ist(2026, 9, 17, 15, 38, 0)));
        // Still due right up to (and past) the 15:40 close — this is what
        // forces the exit before the exchange stops accepting orders.
        assert!(is_expiry_squareoff_due(&pos, ist(2026, 9, 17, 15, 40, 0)));
    }

    #[test]
    fn expiry_squareoff_is_inert_on_any_other_day() {
        let mut pos = position_created_at("2026-09-17 09:00:00");
        pos.resolved_expiry = Some(chrono::NaiveDate::from_ymd_opt(2026, 9, 17).unwrap());

        // A day before the resolved expiry, even past 15:38 — not due yet.
        assert!(!is_expiry_squareoff_due(&pos, ist(2026, 9, 16, 15, 50, 0)));
        // A day after — should have already been closed on the actual day;
        // this only guards against ever firing on the wrong day, not against
        // a position that outlived its own expiry.
        assert!(!is_expiry_squareoff_due(&pos, ist(2026, 9, 18, 15, 50, 0)));
    }

    #[test]
    fn entry_past_target1_only_trips_at_or_above_target1() {
        // Guards both entry paths: if the option is already at/through target 1
        // when the entry trigger fires, the move is spent — refuse the trade
        // instead of booking the target on the next tick.
        let mut sig = position_created_at("").signal; // targets [140, 160]
        assert_eq!(entry_past_target1(&sig, 139.99), None);
        assert_eq!(entry_past_target1(&sig, 140.0), Some(140.0));
        assert_eq!(entry_past_target1(&sig, 500.0), Some(140.0));
        // A signal with no target can never trip the guard.
        sig.targets.clear();
        assert_eq!(entry_past_target1(&sig, 9_999.0), None);
    }

    /// A `WaitingForEntry` LIVE position wired for `decide_live`: resolved to a
    /// 75-lot NIFTY CE, entry 120 ABOVE, with the price seeded at 121 so the
    /// trigger fires.
    fn live_waiting_position(ltp_map: &Arc<DashMap<String, f64>>) -> MonitoredPosition {
        use shared_domain::{AmoFlag, ExchangeSegment, OrderRequest, OrderType, ProductCode, TransactionType, Validity};
        let mut pos = position_created_at(&shared_domain::current_ist_timestamp_string());
        pos.ws_scrip_key = Some("nse_fo|999".to_string());
        pos.resolved_order = Some(OrderRequest {
            after_market_order: AmoFlag::No,
            disclosed_quantity: "0".to_string(),
            exchange_segment: ExchangeSegment::NseFo,
            market_protection: "0".to_string(),
            product_code: ProductCode::Nrml,
            portfolio_flag: "N".to_string(),
            price: "0".to_string(),
            order_type: OrderType::Market,
            quantity: "75".to_string(),
            validity: Validity::Day,
            trigger_price: "0".to_string(),
            trading_symbol: "NIFTY25000CE".to_string(),
            transaction_type: TransactionType::Buy,
        });
        ltp_map.insert("nse_fo|999".to_string(), 121.0);
        pos
    }

    #[test]
    fn decide_live_sends_entry_when_triggered_and_unthrottled() {
        let ltp = Arc::new(DashMap::new());
        let pos = live_waiting_position(&ltp);
        assert!(matches!(
            decide_live(&pos, &ltp, &cfg_with_lots(1, 3), false, true),
            Some(LiveAction::PlaceEntry { .. })
        ));
    }

    #[test]
    fn entry_window_off_is_the_classic_one_sided_trigger() {
        let cfg = cfg_with_lots(1, 3); // entry_window_pct defaults to 0
        let mut sig = position_created_at("").signal; // ABOVE 120, targets [140,160]
        assert!(entry_buy_window(&sig, &cfg).is_none());
        assert!(!entry_triggered(&sig, 119.99, &cfg));
        assert!(entry_triggered(&sig, 120.0, &cfg));
        assert!(entry_triggered(&sig, 500.0, &cfg)); // chases any price above
        sig.entry_condition = "BELOW".to_string();
        assert!(entry_triggered(&sig, 120.0, &cfg));
        assert!(entry_triggered(&sig, 1.0, &cfg));
        assert!(!entry_triggered(&sig, 120.01, &cfg));
    }

    #[test]
    fn entry_window_caps_the_chase_and_waits_for_a_pullback() {
        let mut cfg = cfg_with_lots(1, 3);
        cfg.entry_window_pct = 25.0; // 25% of (140 - 120) = 5 wide
        let sig = position_created_at("").signal; // ABOVE 120, target1 140

        assert_eq!(entry_buy_window(&sig, &cfg), Some((120.0, 125.0)));
        assert!(!entry_triggered(&sig, 119.0, &cfg)); // breakout not reached
        assert!(entry_triggered(&sig, 120.0, &cfg));
        assert!(entry_triggered(&sig, 125.0, &cfg));
        assert!(!entry_triggered(&sig, 125.01, &cfg)); // ran past — wait for pullback
        assert!(!entry_triggered(&sig, 131.0, &cfg));
    }

    #[test]
    fn entry_window_mirrors_below_and_falls_back_without_a_target() {
        let mut cfg = cfg_with_lots(1, 3);
        cfg.entry_window_pct = 25.0;

        let mut sig = position_created_at("").signal;
        sig.entry_condition = "BELOW".to_string(); // BELOW 120, target1 140 -> w = 5
        assert_eq!(entry_buy_window(&sig, &cfg), Some((115.0, 120.0)));
        assert!(entry_triggered(&sig, 120.0, &cfg));
        assert!(entry_triggered(&sig, 115.0, &cfg));
        assert!(!entry_triggered(&sig, 114.99, &cfg));
        assert!(!entry_triggered(&sig, 121.0, &cfg));

        // No usable target -> width is a percent of the entry price (120 * 25%).
        sig.entry_condition = "ABOVE".to_string();
        sig.targets.clear();
        assert_eq!(entry_buy_window(&sig, &cfg), Some((120.0, 150.0)));
    }

    #[test]
    fn decide_live_holds_an_unconfirmed_entry_send_for_the_reconciler() {
        // A send with no broker verdict must not be re-sent, and must not be
        // closed by the staleness / forced-exit rules while the reconciler is
        // still hunting the order book for it.
        let ltp = Arc::new(DashMap::new());
        let mut pos = live_waiting_position(&ltp);
        pos.entry_send_uncertain = true;
        pos.force_exit = Some("USER_EXIT".to_string());
        assert!(decide_live(&pos, &ltp, &cfg_with_lots(1, 3), true, true).is_none());
    }

    #[test]
    fn decide_live_throttles_entry_retries() {
        let ltp = Arc::new(DashMap::new());
        let mut pos = live_waiting_position(&ltp);
        // An attempt a moment ago blocks another on this tick.
        pos.entry_attempt_at = Some(shared_domain::current_ist_timestamp_string());
        assert!(decide_live(&pos, &ltp, &cfg_with_lots(1, 3), false, true).is_none());
        // One from well before the throttle window does not.
        let stale = (shared_domain::now_ist() - chrono::Duration::seconds(10))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        pos.entry_attempt_at = Some(stale);
        assert!(matches!(
            decide_live(&pos, &ltp, &cfg_with_lots(1, 3), false, true),
            Some(LiveAction::PlaceEntry { .. })
        ));
    }

    #[test]
    fn decide_live_abandons_an_entry_that_could_never_be_sent() {
        let ltp = Arc::new(DashMap::new());
        let mut pos = live_waiting_position(&ltp);
        pos.entry_attempts = shared_domain::MAX_ENTRY_ATTEMPTS;
        match decide_live(&pos, &ltp, &cfg_with_lots(1, 3), false, true) {
            Some(LiveAction::AbandonEntry { loud, .. }) => assert!(loud),
            other => panic!("expected a loud AbandonEntry, got {other:?}"),
        }
    }

    #[test]
    fn entry_retry_limit_price_caps_at_protection_band_then_buy_window() {
        let mut cfg = cfg_with_lots(1, 3); // entry_market_protection 5.0
        let sig = position_created_at("").signal; // ABOVE 120, target1 140

        // Window off: a protection band above the touch, tick-rounded.
        assert!(entry_buy_window(&sig, &cfg).is_none());
        let p = entry_retry_limit_price(&sig, &cfg, 121.0, 0.05).unwrap();
        assert!((p - 127.05).abs() < 1e-6, "got {p}"); // 121 * 1.05

        // Window on and tighter than the band: the window's far edge wins.
        cfg.entry_window_pct = 25.0; // [120, 125]
        let p = entry_retry_limit_price(&sig, &cfg, 121.0, 0.05).unwrap();
        assert!((p - 125.0).abs() < 1e-6, "got {p}");

        // No usable price -> nothing to place.
        assert_eq!(entry_retry_limit_price(&sig, &cfg, 0.0, 0.05), None);
    }

    #[test]
    fn decide_live_retries_a_rejected_entry_as_a_limit() {
        let ltp = Arc::new(DashMap::new());
        let mut pos = live_waiting_position(&ltp); // ltp 121, entry 120 ABOVE
        let cfg = cfg_with_lots(1, 3);

        // Normal first attempt is a plain market buy.
        match decide_live(&pos, &ltp, &cfg, false, true) {
            Some(LiveAction::PlaceEntry { limit_price, .. }) => assert_eq!(limit_price, None),
            other => panic!("expected a market PlaceEntry, got {other:?}"),
        }

        // After a market rejection the retry carries an IOC limit price.
        pos.entry_retry_as_limit = true;
        match decide_live(&pos, &ltp, &cfg, false, true) {
            Some(LiveAction::PlaceEntry { limit_price: Some(p), .. }) => {
                assert!((p - 127.05).abs() < 1e-6, "got {p}");
            }
            other => panic!("expected a limit PlaceEntry, got {other:?}"),
        }

        // A price that no longer satisfies the trigger stops the retries —
        // decide_live re-checks entry_triggered before every limit send.
        ltp.insert("nse_fo|999".to_string(), 119.0);
        assert!(decide_live(&pos, &ltp, &cfg, false, true).is_none());
    }

    /// A LIVE position holding one 75-lot at avg 120 with LTP at target 1
    /// (140) — diff = 20.
    fn live_one_lot_at_target1(ltp_map: &Arc<DashMap<String, f64>>) -> MonitoredPosition {
        let mut pos = live_waiting_position(ltp_map);
        pos.state = TradeState::Active;
        pos.avg_buy_price = 120.0;
        pos.executed_qty = 75;
        ltp_map.insert("nse_fo|999".to_string(), 140.0);
        pos
    }

    fn dynamic_cfg(single_lot: bool) -> TradingConfig {
        let mut cfg = cfg_with_lots(1, 3);
        cfg.dynamic_targeting = true;
        cfg.dynamic_targeting_single_lot = single_lot;
        cfg
    }

    #[test]
    fn one_lot_exits_in_full_at_target1_unless_single_lot_runner_is_on() {
        let ltp = Arc::new(DashMap::new());
        let pos = live_one_lot_at_target1(&ltp);

        // Default: dynamic targeting sells its one lot, leaving nothing to run.
        match decide_live(&pos, &ltp, &dynamic_cfg(false), false, true) {
            Some(LiveAction::ExitAll { qty, reason }) => {
                assert_eq!(qty, 75);
                assert_eq!(reason, "TGT1_FULL");
            }
            other => panic!("expected TGT1_FULL, got {other:?}"),
        }

        // Opted in: nothing is sold, the whole lot rides the ladder with the
        // stop at 140 - 20*0.5 = 130 and the next rung at 140 + 20*1.0 = 160.
        assert_eq!(
            decide_live(&pos, &ltp, &dynamic_cfg(true), false, true),
            Some(LiveAction::Target1 {
                slice: 0,
                keep: 75,
                new_sl: 130.0,
                next_dynamic_target: Some(160.0),
                dynamic_rung: Some(140.0),
            })
        );

        // The toggle is a sub-option — inert while dynamic targeting is off.
        let mut cfg = dynamic_cfg(true);
        cfg.dynamic_targeting = false;
        assert!(matches!(
            decide_live(&pos, &ltp, &cfg, false, true),
            Some(LiveAction::ExitAll { .. })
        ));
    }

    #[test]
    fn single_lot_runner_leaves_multi_lot_and_bad_fills_alone() {
        let ltp = Arc::new(DashMap::new());

        // Two lots: still sells exactly one lot at target 1, as before.
        let mut pos = live_one_lot_at_target1(&ltp);
        pos.executed_qty = 150;
        match decide_live(&pos, &ltp, &dynamic_cfg(true), false, true) {
            Some(LiveAction::Target1 { slice, keep, .. }) => {
                assert_eq!((slice, keep), (75, 75));
            }
            other => panic!("expected a one-lot Target1 slice, got {other:?}"),
        }

        // Filled at/above target 1 (diff <= 0): the ladder can't climb, so
        // err toward flat and exit in full instead of keeping a runner.
        let mut pos = live_one_lot_at_target1(&ltp);
        pos.avg_buy_price = 140.0;
        assert!(matches!(
            decide_live(&pos, &ltp, &dynamic_cfg(true), false, true),
            Some(LiveAction::ExitAll { .. })
        ));
    }

    #[test]
    fn single_lot_runner_climbs_and_exits_only_on_the_trailed_stop() {
        let ltp = Arc::new(DashMap::new());
        let cfg = dynamic_cfg(true);
        let mut pos = live_one_lot_at_target1(&ltp);
        // State as left by the slice-0 Target1 action.
        pos.state = TradeState::Target1Hit;
        pos.current_sl = 130.0;
        pos.next_dynamic_target = Some(160.0);
        pos.last_dynamic_rung = Some(140.0);
        pos.dynamic_rung_number = 1;

        // Between the stop and the next rung: hold.
        ltp.insert("nse_fo|999".to_string(), 150.0);
        assert!(decide_live(&pos, &ltp, &cfg, false, true).is_none());

        // Next rung: trail to 150, extend to 180 — no sell.
        ltp.insert("nse_fo|999".to_string(), 160.0);
        assert_eq!(
            decide_live(&pos, &ltp, &cfg, false, true),
            Some(LiveAction::TrailDynamic { new_sl: 150.0, next_target: 180.0, rung_hit: 160.0 })
        );

        // Back through the trailed stop: sell the whole lot.
        ltp.insert("nse_fo|999".to_string(), 129.0);
        match decide_live(&pos, &ltp, &cfg, false, true) {
            Some(LiveAction::ExitAll { qty, reason }) => {
                assert_eq!(qty, 75);
                assert_eq!(reason, "TRAIL_SL_HIT");
            }
            other => panic!("expected TRAIL_SL_HIT, got {other:?}"),
        }
    }

    #[test]
    fn secs_since_ist_handles_missing_and_past_timestamps() {
        assert_eq!(secs_since_ist(&None), None);
        assert_eq!(secs_since_ist(&Some("not a date".to_string())), None);
        let ten_ago = (shared_domain::now_ist() - chrono::Duration::seconds(10))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let s = secs_since_ist(&Some(ten_ago)).unwrap();
        assert!((8..=12).contains(&s), "expected ~10s, got {s}");
    }

    /// An Active position from the shared helper: entry/avg 120, targets
    /// [140, 160], SL 100 — so diff = 20, and the default 60 % arm threshold
    /// sits at 132 with a 0.5-factor trail distance of 10.
    fn active_position() -> MonitoredPosition {
        let mut pos = position_created_at(&shared_domain::current_ist_timestamp_string());
        pos.state = TradeState::Active;
        pos.avg_buy_price = 120.0;
        pos.executed_qty = 75;
        pos
    }

    fn pre_t1_cfg() -> TradingConfig {
        let mut cfg = cfg_with_lots(1, 3);
        cfg.pre_t1_trailing = true;
        cfg
    }

    #[test]
    fn pre_t1_trail_is_inert_unless_enabled_and_active() {
        let cfg_off = cfg_with_lots(1, 3);
        let mut pos = active_position();
        assert_eq!(pre_t1_trail_update(&mut pos, 139.0, &cfg_off), None);
        assert_eq!(pos.peak_ltp, None);
        assert_eq!(pos.current_sl, 100.0);

        // Once target 1 has hit, the dynamic ladder / fixed target-2 path
        // owns the stop — this must never touch it again.
        let cfg = pre_t1_cfg();
        let mut pos = active_position();
        pos.state = TradeState::Target1Hit;
        assert_eq!(pre_t1_trail_update(&mut pos, 155.0, &cfg), None);
        assert_eq!(pos.current_sl, 100.0);

        // No target, or an entry at/through target 1 (diff <= 0): nothing to
        // measure progress against.
        let mut pos = active_position();
        pos.signal.targets.clear();
        assert_eq!(pre_t1_trail_update(&mut pos, 139.0, &cfg), None);
        let mut pos = active_position();
        pos.avg_buy_price = 150.0;
        assert_eq!(pre_t1_trail_update(&mut pos, 139.0, &cfg), None);
    }

    #[test]
    fn pre_t1_trail_arms_at_threshold_and_only_ratchets_up() {
        let cfg = pre_t1_cfg();
        let mut pos = active_position();

        // Below the 132 arm level: peak is tracked, stop is untouched — noise
        // near entry keeps the signal's full SL room.
        assert_eq!(pre_t1_trail_update(&mut pos, 131.95, &cfg), None);
        assert_eq!(pos.peak_ltp, Some(131.95));
        assert_eq!(pos.current_sl, 100.0);

        // At 132 it arms: stop = 132 - 10.
        assert_eq!(pre_t1_trail_update(&mut pos, 132.0, &cfg), Some(122.0));
        assert_eq!(pos.current_sl, 122.0);

        // A pullback is not a new peak — the stop never moves down.
        assert_eq!(pre_t1_trail_update(&mut pos, 125.0, &cfg), None);
        assert_eq!(pos.peak_ltp, Some(132.0));
        assert_eq!(pos.current_sl, 122.0);

        // A new high ratchets it further, tick-rounded down onto the grid.
        assert_eq!(pre_t1_trail_update(&mut pos, 138.33, &cfg), Some(128.30));
        assert_eq!(pos.current_sl, 128.30);

        // A signal edit that lowers the stop on an armed position is
        // re-asserted from the retained peak on the very next observation,
        // even one that is not a new high — the higher stop wins.
        pos.current_sl = pos.signal.stop_loss;
        assert_eq!(pre_t1_trail_update(&mut pos, 124.0, &cfg), Some(128.30));
    }

    #[test]
    fn pre_t1_trail_never_loosens_a_stop_already_above_it() {
        let cfg = pre_t1_cfg();
        let mut pos = active_position();
        // A stop already sitting above the would-be trail (125 > 132 - 10)
        // stays where it is.
        pos.current_sl = 125.0;
        pos.signal.stop_loss = 125.0;
        assert_eq!(pre_t1_trail_update(&mut pos, 132.0, &cfg), None);
        assert_eq!(pos.current_sl, 125.0);
        // ...until the peak climbs far enough to beat it.
        assert_eq!(pre_t1_trail_update(&mut pos, 136.0, &cfg), Some(126.0));
    }

    #[test]
    fn tick_rounding_stays_on_the_grid() {
        // Exact multiples must survive binary-float error, not lose a tick.
        assert_eq!(round_down_tick(0.15, 0.05), 0.15);
        assert_eq!(round_down_tick(120.10, 0.05), 120.10);
        assert_eq!(round_down_tick(2.30, 0.05), 2.30);
        // Anything off the grid rounds down.
        assert_eq!(round_down_tick(120.13, 0.05), 120.10);
        assert_eq!(round_down_tick(120.19, 0.05), 120.15);
        assert_eq!(round_down_tick(3.07, 0.01), 3.07);
        // Degenerate tick sizes are passed through rather than dividing by zero.
        assert_eq!(round_down_tick(120.13, 0.0), 120.13);
    }

    #[test]
    fn target1_slice_rounds_up_to_a_whole_lot() {
        // 3 lots at 50% → sell 2, run 1 to target 2.
        assert_eq!(tgt1_slice_qty(225, 75, 50.0), 150);
        // 2 lots at 50% → an even split.
        assert_eq!(tgt1_slice_qty(150, 75, 50.0), 75);
        // 4 lots at 25% → exactly one lot, no rounding needed.
        assert_eq!(tgt1_slice_qty(300, 75, 25.0), 75);
        // 3 lots at 20% → still a whole lot, never a partial one.
        assert_eq!(tgt1_slice_qty(225, 75, 20.0), 75);
        // A single lot cannot be split, so target 1 closes the position.
        assert_eq!(tgt1_slice_qty(75, 75, 50.0), 75);
        // The slice can never exceed what we hold.
        assert_eq!(tgt1_slice_qty(150, 75, 100.0), 150);
    }

    fn dummy_order_request() -> shared_domain::OrderRequest {
        use shared_domain::{AmoFlag, ExchangeSegment, OrderRequest, OrderType, ProductCode, TransactionType, Validity};
        OrderRequest {
            after_market_order: AmoFlag::No,
            disclosed_quantity: "0".to_string(),
            exchange_segment: ExchangeSegment::NseFo,
            market_protection: "0".to_string(),
            product_code: ProductCode::Nrml,
            portfolio_flag: "N".to_string(),
            price: "0".to_string(),
            order_type: OrderType::Limit,
            quantity: "75".to_string(),
            validity: Validity::Day,
            trigger_price: "0".to_string(),
            trading_symbol: "NIFTY24JUL25000CE".to_string(),
            transaction_type: TransactionType::Buy,
        }
    }

    #[test]
    fn kill_switch_active_abandons_waiting_entry_in_decide_live() {
        let mut cfg = cfg_with_lots(1, 1);
        cfg.kill_switch_active = true;

        let mut pos = position_created_at(&shared_domain::current_ist_timestamp_string());
        pos.resolved_order = Some(dummy_order_request());
        pos.state = TradeState::WaitingForEntry;

        let ltp_map = Arc::new(DashMap::new());
        ltp_map.insert("NIFTY".to_string(), 125.0);

        let action = decide_live(&pos, &ltp_map, &cfg, false, false);
        assert_eq!(
            action,
            Some(LiveAction::AbandonEntry {
                reason: "KILL_SWITCH_ACTIVE".to_string(),
                loud: false,
            })
        );
    }

    #[test]
    fn kill_switch_inactive_allows_entry_trigger() {
        let mut cfg = cfg_with_lots(1, 1);
        cfg.kill_switch_active = false;

        let mut pos = position_created_at(&shared_domain::current_ist_timestamp_string());
        pos.resolved_order = Some(dummy_order_request());
        pos.state = TradeState::WaitingForEntry;
        pos.signal.entry_price = 120.0;
        pos.signal.entry_condition = "ABOVE".to_string();

        let ltp_map = Arc::new(DashMap::new());
        ltp_map.insert("NIFTY".to_string(), 125.0);

        let action = decide_live(&pos, &ltp_map, &cfg, false, false);
        assert!(matches!(action, Some(LiveAction::PlaceEntry { .. })));
    }

    #[test]
    fn kill_switch_force_exit_triggers_market_exit_on_active_position() {
        let cfg = cfg_with_lots(1, 1);

        let mut pos = active_position();
        pos.resolved_order = Some(dummy_order_request());
        pos.force_exit = Some("KILL_SWITCH".to_string());

        let ltp_map = Arc::new(DashMap::new());
        ltp_map.insert("NIFTY".to_string(), 130.0);

        let action = decide_live(&pos, &ltp_map, &cfg, false, false);
        assert_eq!(
            action,
            Some(LiveAction::ExitAll {
                qty: 75,
                reason: "KILL_SWITCH".to_string(),
            })
        );
    }
}
