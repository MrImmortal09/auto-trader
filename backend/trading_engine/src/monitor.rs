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

/// True when it is at/after 15:10 IST on this position's expiry day.
fn is_expiry_squareoff_due(_pos: &MonitoredPosition) -> bool {
    false
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

/// Pre-T1 trailing stop (see `TradingConfig::pre_t1_trailing`): feed one LTP
/// observation into `peak_ltp` and, once the peak has covered
/// `pre_t1_trail_arm_pct` % of the entry→target-1 distance, ratchet
/// `current_sl` up to `peak - diff * pre_t1_trail_factor` (tick-rounded).
///
/// Ratchet-only: the stop never moves down here, and it starts from the
/// signal's original SL, so it can never be looser than the signal asked for.
/// A signal edit that lowers `current_sl` on an armed position is re-asserted
/// on the next tick from the retained peak — when the two disagree, the
/// higher stop (closer to flat) wins. Returns `Some(new_sl)` when the stop
/// actually moved. Shared by the PAPER and LIVE paths — protection is a pure
/// software watch in both, so moving the stop is bookkeeping, not an order.
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
    let desired = round_down_tick(peak - diff * cfg.pre_t1_trail_factor, pos.tick_size);
    if desired > pos.current_sl {
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
/// for its trigger is dropped — we do not open anything this close to the bell.
const NO_ENTRY_HOUR: u32 = 15;
const NO_ENTRY_MINUTE: u32 = 29;

/// True at/after 15:29 IST, for the remainder of the day.
fn is_entry_cutoff_passed() -> bool {
    use chrono::Timelike;
    let now = shared_domain::now_ist();
    let (h, m) = (now.hour(), now.minute());
    h > NO_ENTRY_HOUR || (h == NO_ENTRY_HOUR && m >= NO_ENTRY_MINUTE)
}

/// Why a `WaitingForEntry` position should be abandoned rather than left to
/// watch for its trigger, or `None` if it is still live.
///
/// `entry_cutoff` (today's 15:29 IST bell) only catches a position while the
/// engine is running continuously through that moment. A crash, redeploy, or
/// manual restart that happens to fall before 15:29 lets a never-triggered
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

// ---------------------------------------------------------------------------
// LIVE mode — broker call wrappers
//
// Each takes the Kotak mutex only for the duration of the call. The positions
// lock is never held across any of these.
// ---------------------------------------------------------------------------

type KotakHandle = Arc<tokio::sync::Mutex<Option<KotakClient>>>;

async fn kotak_place(kotak: &KotakHandle, order: &shared_domain::OrderRequest) -> Result<String, String> {
    let guard = kotak.lock().await;
    let client = guard.as_ref().ok_or_else(|| "no Kotak session".to_string())?;
    client
        .place_live_order(order)
        .await
        .map(|e| e.order_id)
        .map_err(|e| e.to_string())
}

async fn kotak_limits(kotak: &KotakHandle) -> Result<kotak_client::KotakLimits, String> {
    let guard = kotak.lock().await;
    let client = guard.as_ref().ok_or_else(|| "no Kotak session".to_string())?;
    client.get_limits().await.map_err(|e| e.to_string())
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
    let legs: Vec<LiveLeg> = {
        let g = positions.read().await;
        g.iter()
            .filter(|p| {
                p.entry_order_id.is_some()
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
                sl_order_id: p.sl_order_id.clone(),
                pending_exit_order_id: p.pending_exit_order_id.clone(),
                pending_exit_qty: p.pending_exit_qty,
                pending_exit_reason: p.pending_exit_reason.clone(),
            })
            .collect()
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
                    } else {
                        with_position(positions, &leg.pos_id, |p| {
                            p.state = TradeState::Closed;
                            p.entry_order_id = None;
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
                    if filled > 0 && avg > 0.0 {
                        record_live_sell(db_tx, log_tx, leg, filled, avg, &reason, brokerage).await;
                    } else {
                        loud_error(db_tx, log_tx, &leg.instrument, &format!(
                            "exit order {oid} ({reason}) came back {} ({}) with nothing sold",
                            ord.status, ord.reject_reason
                        )).await;
                    }
                    let shortfall = leg.pending_exit_qty - filled;
                    with_position(positions, &leg.pos_id, |p| {
                        p.executed_qty = (p.executed_qty - filled).max(0);
                        p.pending_exit_order_id = None;
                        p.pending_exit_qty = 0;
                        p.pending_exit_reason = None;
                        if p.executed_qty <= 0 {
                            p.state = TradeState::Closed;
                            p.force_exit = None;
                        } else if shortfall > 0 {
                            // Still holding stock we meant to be out of, and the
                            // stop now covers less than we hold. Square the whole
                            // thing off rather than sit under-protected.
                            p.exit_attempts += 1;
                            p.force_exit = Some(format!("{reason}_SHORTFALL"));
                        }
                    }).await;
                    if shortfall > 0 {
                        loud_error(db_tx, log_tx, &leg.instrument, &format!(
                            "exit order {oid} ({reason}) filled {filled} of {} — squaring off the remainder",
                            leg.pending_exit_qty
                        )).await;
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
        live_halt: None,
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

#[derive(Debug)]
enum LiveAction {
    /// Entry condition met — check funds, then send a market buy for `qty`.
    /// `ltp` is the price that triggered it, used to size the funds check.
    PlaceEntry { qty: i32, ltp: f64 },
    /// Give up on an entry that will not be taken; cancel it if it is in flight.
    /// `loud` surfaces it as an ERROR the dashboard highlights (the signal
    /// looked wrong) rather than a routine INFO line (e.g. the EOD cutoff).
    AbandonEntry { reason: String, loud: bool },
    /// Target 1: trail the software stop to `new_sl`, then market-sell `slice`.
    /// `next_dynamic_target`/`dynamic_rung` seed the dynamic ladder (see
    /// `TrailDynamic`) when dynamic targeting is on; `None` keeps the
    /// existing fixed-target-2 behaviour for this position.
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
            let ltp = ltp?;
            let triggered = match pos.signal.entry_condition.to_uppercase().as_str() {
                "ABOVE" => ltp >= pos.signal.entry_price,
                "BELOW" => ltp <= pos.signal.entry_price,
                _ => false,
            };
            if !triggered {
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
            Some(LiveAction::PlaceEntry { qty, ltp })
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
            if is_expiry_squareoff_due(pos) {
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
                    // Dynamic targeting sells exactly one lot at target 1
                    // (not the configured percentage) so there's a runner
                    // left to extend; off, this is the existing behaviour.
                    let slice = if cfg.dynamic_targeting {
                        lot_size_of(pos).min(held)
                    } else {
                        tgt1_slice_qty(held, lot_size_of(pos), cfg.target_1_exit_pct)
                    };
                    // Target 2 only ever mattered here as an "is there more
                    // room to run" check — the dynamic path itself only ever
                    // reads target 1. So a single-target signal still ladders
                    // when dynamic targeting is on, instead of always fully
                    // exiting at target 1 regardless of the setting.
                    let has_t2 = pos.signal.targets.len() > 1;
                    if (!has_t2 && !cfg.dynamic_targeting) || slice >= held {
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

    let order = build_market_order(&ctx.base, shared_domain::TransactionType::Sell, qty, 0.0);
    match kotak_place(kotak, &order).await {
        Ok(order_id) => {
            with_position(positions, pos_id, |p| {
                p.pending_exit_order_id = Some(order_id.clone());
                p.pending_exit_qty = qty;
                p.pending_exit_reason = Some(reason.to_string());
                p.force_exit = None;
                p.override_exit_price = None;
            }).await;
            tracing::info!(instrument = %ctx.instrument, %order_id, qty, reason, "LIVE market exit placed");
            live_info(db_tx, log_tx, json!({
                "event": "LIVE_EXIT_PLACED",
                "instrument": ctx.instrument,
                "order_id": order_id,
                "qty": qty,
                "reason": reason,
                "mode": "LIVE",
            })).await;
        }
        Err(e) => {
            loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                "market exit ({reason}) for {qty} was rejected: {e} — the position is unprotected until a new stop is placed"
            )).await;
            bump_exit_attempts(positions, db_tx, log_tx, pos_id, &ctx.instrument).await;
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
        }
    };

    match &pending.action {
        LiveAction::PlaceEntry { qty, ltp } => {
            // Pre-flight funds check. We must be able to pay for the order and
            // still have the buffer left over — the account is never run to zero.
            let order_value = *qty as f64 * *ltp;
            let required = order_value + FUNDS_BUFFER_INR;
            match kotak_limits(kotak).await {
                Ok(limits) => {
                    if limits.net < required {
                        loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                            "not enough funds for {qty} @ ~₹{ltp:.2}: needs ₹{required:.2} (₹{order_value:.2} order + ₹{FUNDS_BUFFER_INR:.0} buffer) but only ₹{:.2} is available — signal dropped",
                            limits.net
                        )).await;
                        with_position(positions, &pending.pos_id, |p| p.state = TradeState::Closed).await;
                        return true;
                    }
                }
                Err(e) => {
                    // Unverifiable funds are treated as insufficient funds. Missing
                    // a signal is recoverable; buying blind is not.
                    loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                        "could not read account limits ({e}) — entry not placed because funds cannot be verified, signal dropped"
                    )).await;
                    with_position(positions, &pending.pos_id, |p| p.state = TradeState::Closed).await;
                    return true;
                }
            }

            let order = build_market_order(
                &ctx.base,
                shared_domain::TransactionType::Buy,
                *qty,
                cfg.entry_market_protection,
            );
            match kotak_place(kotak, &order).await {
                Ok(order_id) => {
                    with_position(positions, &pending.pos_id, |p| {
                        p.entry_order_id = Some(order_id.clone());
                        p.entry_cancel_sent = false;
                    }).await;
                    tracing::info!(instrument = %ctx.instrument, %order_id, qty, "LIVE entry market buy placed");
                    live_info(db_tx, log_tx, json!({
                        "event": "LIVE_ENTRY_PLACED",
                        "instrument": ctx.instrument,
                        "order_id": order_id,
                        "qty": qty,
                        "mode": "LIVE",
                    })).await;
                }
                Err(e) => {
                    loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                        "entry market buy for {qty} was rejected: {e} — signal dropped, it will not be retried"
                    )).await;
                    with_position(positions, &pending.pos_id, |p| p.state = TradeState::Closed).await;
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

            let sell = build_market_order(&ctx.base, shared_domain::TransactionType::Sell, *slice, 0.0);
            match kotak_place(kotak, &sell).await {
                Ok(order_id) => {
                    with_position(positions, &pending.pos_id, |p| {
                        p.pending_exit_order_id = Some(order_id.clone());
                        p.pending_exit_qty = *slice;
                        p.pending_exit_reason = Some("TGT1_PARTIAL".to_string());
                    }).await;
                    tracing::info!(instrument = %ctx.instrument, %order_id, slice, keep, "LIVE target 1 slice placed");
                    live_info(db_tx, log_tx, json!({
                        "event": "LIVE_EXIT_PLACED",
                        "instrument": ctx.instrument,
                        "order_id": order_id,
                        "qty": slice,
                        "reason": "TGT1_PARTIAL",
                        "mode": "LIVE",
                    })).await;
                }
                Err(e) => {
                    loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                        "target-1 sell of {slice} was rejected: {e} — still holding the full {} at the trailed stop, squaring off at market",
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
            exec_exit_all(positions, kotak, db_tx, log_tx, &pending.pos_id, &ctx, *qty, reason).await;
        }

        LiveAction::ManualSell { qty } => {
            // Clear the request immediately so a slow fill doesn't cause it to
            // re-fire — settlement is picked up the same way as a target-1
            // slice, via pending_exit_order_id in reconcile_live_orders.
            with_position(positions, &pending.pos_id, |p| p.manual_sell_qty = None).await;

            let sell = build_market_order(&ctx.base, shared_domain::TransactionType::Sell, *qty, 0.0);
            match kotak_place(kotak, &sell).await {
                Ok(order_id) => {
                    with_position(positions, &pending.pos_id, |p| {
                        p.pending_exit_order_id = Some(order_id.clone());
                        p.pending_exit_qty = *qty;
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
        mutated |= exec_live_action(positions, kotak, db_tx, log_tx, cfg, pending).await;
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
                            // A signal with no stop-loss or no target is not
                            // tradeable, whatever it parsed out of. `stop_loss`
                            // defaults to 0.0, and an LTP can never cross 0, so
                            // the software stop would never fire; with no target
                            // there is nothing to exit on either. Such a position
                            // runs unprotected until the expiry square-off, so
                            // refuse it rather than open it.
                            if signal.stop_loss <= 0.0 || signal.targets.is_empty() {
                                let msg = format!(
                                    r#"{{"event":"ERROR","message":"Signal discarded — no {} parsed; refusing to open an unprotected position","instrument":"{}"}}"#,
                                    if signal.stop_loss <= 0.0 { "stop-loss" } else { "target" },
                                    signal.instrument_name
                                );
                                send_log(&db_tx, &log_tx, "ERROR", &msg).await;
                                tracing::error!(
                                    instrument = %signal.instrument_name,
                                    stop_loss = signal.stop_loss,
                                    targets = signal.targets.len(),
                                    "Signal discarded — missing stop-loss or target"
                                );
                                continue;
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
                                if let Some(ref store) = *scrip_guard {
                                    if let Some(record) = store.resolve_signal(&signal) {
                                        // Build OrderRequest
                                        use shared_domain::{OrderRequest, AmoFlag, ExchangeSegment, ProductCode, OrderType, Validity, TransactionType};
                                        let qty = record.lot_size.to_string(); // we use lot size by default
                                        resolved_token = Some(record.instrument_token.clone());
                                        resolved_segment_code = Some(record.exchange_segment_code.clone());
                                        resolved_tick_size = record.tick_size;
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
                                    live_halt: None,
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
                    // guard below — a cancelled or rewritten signal, or the 15:29
                    // cutoff after which we do not open anything new.
                    if matches!(pos.state, TradeState::WaitingForEntry) {
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

                    // Expiry-day square-off: at/after 15:10 IST on the option's
                    // expiry day, force-close any still-open position at market so
                    // it is never carried into expiry/settlement.
                    let pa = if matches!(pos.state, TradeState::Active | TradeState::Target1Hit)
                        && pos.force_exit.is_none()
                        && is_expiry_squareoff_due(pos)
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
                            let triggered = match pos.signal.entry_condition.to_uppercase().as_str() {
                                "ABOVE" => ltp >= pos.signal.entry_price,
                                "BELOW" => ltp <= pos.signal.entry_price,
                                _ => false,
                            };
                            triggered.then(|| {
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
            live_halt: None,
        }
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
            pre_t1_trailing: false,
            pre_t1_trail_arm_pct: 60.0,
            pre_t1_trail_factor: 0.5,
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
        // 15:29 cutoff wins regardless of created_at, including a position
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
}
