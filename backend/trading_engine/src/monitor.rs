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

/// IST hour/minute at/after which open positions are squared off on their
/// expiry day, so an option is never carried into expiry/settlement.
const EXPIRY_SQUAREOFF_HOUR: u32 = 15;
const EXPIRY_SQUAREOFF_MINUTE: u32 = 10;

/// True when it is at/after 15:10 IST on this position's expiry day.
fn is_expiry_squareoff_due(pos: &MonitoredPosition) -> bool {
    use chrono::Timelike;
    let Some(ref expiry_str) = pos.signal.expiry else { return false; };
    // Expiry is stored as e.g. "26-JUL-2026" (`%d-%b-%Y`, uppercased); chrono
    // parses month abbreviations case-insensitively.
    let Ok(exp_date) = chrono::NaiveDate::parse_from_str(expiry_str, "%d-%b-%Y") else {
        return false;
    };
    let now = shared_domain::now_ist();
    if exp_date != now.date_naive() {
        return false;
    }
    let (h, m) = (now.hour(), now.minute());
    h > EXPIRY_SQUAREOFF_HOUR || (h == EXPIRY_SQUAREOFF_HOUR && m >= EXPIRY_SQUAREOFF_MINUTE)
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
        let inst = signal.instrument_name.to_uppercase();
        const INDEX_NAMES: &[&str] = &["NIFTY", "BANKNIFTY", "FINNIFTY", "MIDCPNIFTY", "SENSEX", "BANKEX"];
        let is_index = INDEX_NAMES.iter().any(|&idx| inst.contains(idx));
        let lots = if is_index { cfg.index_lots.max(1) } else { cfg.other_lots.max(1) };
        lots * lot_size
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

/// Round `price` DOWN to a whole multiple of `tick`.
///
/// Every price the engine sends to the broker goes through this. The exchange
/// rejects orders that are off the tick grid, and rounding down is the agreed
/// default direction for all of them — stops, targets and trailed stops alike.
fn round_down_tick(price: f64, tick: f64) -> f64 {
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

/// Protective stop — `SL-M` sell for `qty`, triggering at `trigger`.
///
/// `mp = 0`: a protective exit must never be held back by price protection.
fn build_stop_order(
    base: &shared_domain::OrderRequest,
    qty: i32,
    trigger: f64,
) -> shared_domain::OrderRequest {
    use shared_domain::{OrderType, TransactionType};
    let mut order = base.clone();
    order.quantity = qty.to_string();
    order.transaction_type = TransactionType::Sell;
    order.order_type = OrderType::StopLossMarket;
    order.price = "0".to_string();
    order.trigger_price = format!("{trigger:.2}");
    order.market_protection = "0".to_string();
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

async fn kotak_modify(
    kotak: &KotakHandle,
    order: &shared_domain::OrderRequest,
    order_no: &str,
) -> Result<(), String> {
    let guard = kotak.lock().await;
    let client = guard.as_ref().ok_or_else(|| "no Kotak session".to_string())?;
    client.modify_order(order, order_no).await.map(|_| ()).map_err(|e| e.to_string())
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
async fn reconcile_on_startup(
    positions: &Arc<RwLock<Vec<MonitoredPosition>>>,
    kotak: &KotakHandle,
    db_tx: &mpsc::Sender<DbWriteMessage>,
    log_tx: &broadcast::Sender<String>,
    brokerage: f64,
) {
    tracing::info!("LIVE startup reconciliation starting");

    // 1. Settle whatever we were already tracking first, so a fill that landed
    //    while we were down is recorded as a real trade rather than inferred
    //    from a quantity difference below.
    reconcile_live_orders(positions, kotak, db_tx, log_tx, brokerage).await;

    // 2. Broker truth.
    let (broker_positions, book) = {
        let guard = kotak.lock().await;
        let Some(client) = guard.as_ref() else {
            tracing::warn!("LIVE startup reconciliation skipped — no Kotak session");
            return;
        };
        (client.get_positions().await, client.get_order_book().await)
    };
    let broker_positions = match broker_positions {
        Ok(v) => v,
        Err(e) => {
            loud_error(db_tx, log_tx, "SYSTEM", &format!(
                "startup reconciliation could not read positions ({e}) — engine state is unverified against the broker, check the terminal before trading"
            )).await;
            return;
        }
    };
    let book = match book {
        Ok(v) => v,
        Err(e) => {
            loud_error(db_tx, log_tx, "SYSTEM", &format!(
                "startup reconciliation could not read the order book ({e}) — resting orders are unverified, check the terminal before trading"
            )).await;
            return;
        }
    };

    struct Tracked {
        pos_id: String,
        instrument: String,
        trading_symbol: String,
        is_open: bool,
        executed_qty: i32,
        entry_order_id: Option<String>,
        sl_order_id: Option<String>,
    }
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

    tracing::info!(positions = snapshot.len(), "LIVE startup reconciliation complete");
    live_info(db_tx, log_tx, json!({
        "event": "LIVE_STARTUP_RECONCILED",
        "positions": snapshot.len(),
        "mode": "LIVE",
    })).await;
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
    AbandonEntry { reason: String },
    /// Establish protection: an `SL-M` sell covering the whole holding.
    PlaceStop { qty: i32, trigger: f64 },
    /// Move the resting stop to a new quantity/trigger.
    ModifyStop { qty: i32, trigger: f64 },
    /// Target 1: shrink the stop to the runner, then market-sell the slice.
    Target1 { slice: i32, keep: i32, new_sl: f64 },
    /// Cancel the resting stop, then market-sell everything we hold.
    ExitAll { qty: i32, reason: String },
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
                return Some(LiveAction::AbandonEntry { reason });
            }
            if entry_cutoff {
                return Some(LiveAction::AbandonEntry { reason: "EOD_NO_ENTRY".to_string() });
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
            let lot = lot_size_of(pos);
            let qty = compute_entry_qty(&pos.signal, lot, pos.override_qty, cfg, Some(ltp));
            if qty <= 0 || qty % lot != 0 {
                return Some(LiveAction::AbandonEntry {
                    reason: format!("invalid quantity {qty} for lot size {lot}"),
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
            let sl_reason = if matches!(pos.state, TradeState::Target1Hit) {
                "TRAIL_SL_HIT"
            } else {
                "SL_HIT"
            };

            // Protection comes before anything else.
            match pos.sl_order_id {
                None => {
                    return Some(match ltp {
                        // Unprotected and already through the stop: a fresh SL-M
                        // would be rejected for triggering immediately, so leave
                        // at market instead.
                        Some(p) if p <= trigger => LiveAction::ExitAll {
                            qty: held,
                            reason: sl_reason.to_string(),
                        },
                        _ => LiveAction::PlaceStop { qty: held, trigger },
                    });
                }
                Some(_) => {
                    if pos.sl_order_qty != held || (pos.sl_order_trigger - trigger).abs() >= 0.005 {
                        return Some(LiveAction::ModifyStop { qty: held, trigger });
                    }
                }
            }

            let ltp = ltp?;
            match pos.state {
                TradeState::Active => {
                    let t1 = *pos.signal.targets.first()?;
                    if ltp < t1 {
                        return None;
                    }
                    let slice = tgt1_slice_qty(held, lot_size_of(pos), cfg.target_1_exit_pct);
                    let has_t2 = pos.signal.targets.len() > 1;
                    if !has_t2 || slice >= held {
                        Some(LiveAction::ExitAll { qty: held, reason: "TGT1_FULL".to_string() })
                    } else {
                        Some(LiveAction::Target1 {
                            slice,
                            keep: held - slice,
                            new_sl: round_down_tick((pos.avg_buy_price + t1) / 2.0, pos.tick_size),
                        })
                    }
                }
                TradeState::Target1Hit => {
                    let t2 = *pos.signal.targets.get(1)?;
                    (ltp >= t2).then(|| LiveAction::ExitAll {
                        qty: held,
                        reason: "TGT2_HIT".to_string(),
                    })
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
/// The cancel has to succeed first. A resting stop *plus* a market sell for the
/// same quantity could both fill, which would leave the account short — and
/// this account carries no margin to be short with. If the cancel fails we do
/// not sell at all.
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
            loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                "could not cancel stop-loss {sl_id} before the {reason} exit ({e}) — holding the market sell back so we cannot end up short"
            )).await;
            bump_exit_attempts(positions, db_tx, log_tx, pos_id, &ctx.instrument).await;
            return;
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

        LiveAction::AbandonEntry { reason } => {
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
            tracing::info!(instrument = %ctx.instrument, reason, "LIVE entry abandoned");
            live_info(db_tx, log_tx, json!({
                "event": "ENTRY_ABANDONED",
                "instrument": ctx.instrument,
                "reason": reason,
                "mode": "LIVE",
            })).await;
        }

        LiveAction::PlaceStop { qty, trigger } => {
            let order = build_stop_order(&ctx.base, *qty, *trigger);
            match kotak_place(kotak, &order).await {
                Ok(order_id) => {
                    with_position(positions, &pending.pos_id, |p| {
                        p.sl_order_id = Some(order_id.clone());
                        p.sl_order_qty = *qty;
                        p.sl_order_trigger = *trigger;
                    }).await;
                    tracing::info!(instrument = %ctx.instrument, %order_id, qty, trigger, "LIVE stop-loss placed");
                    live_info(db_tx, log_tx, json!({
                        "event": "LIVE_SL_PLACED",
                        "instrument": ctx.instrument,
                        "order_id": order_id,
                        "qty": qty,
                        "trigger": trigger,
                        "mode": "LIVE",
                    })).await;
                }
                Err(e) => {
                    // An open position with no stop is the one state we refuse to
                    // sit in: get flat immediately.
                    loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                        "stop-loss placement for {qty} at {trigger:.2} failed: {e} — squaring the position off at market"
                    )).await;
                    exec_exit_all(
                        positions, kotak, db_tx, log_tx, &pending.pos_id, &ctx,
                        ctx.executed_qty, "SL_PLACE_FAILED",
                    ).await;
                }
            }
        }

        LiveAction::ModifyStop { qty, trigger } => {
            let Some(sl_id) = ctx.sl_order_id.clone() else { return false; };
            let order = build_stop_order(&ctx.base, *qty, *trigger);
            match kotak_modify(kotak, &order, &sl_id).await {
                Ok(()) => {
                    with_position(positions, &pending.pos_id, |p| {
                        p.sl_order_qty = *qty;
                        p.sl_order_trigger = *trigger;
                    }).await;
                    tracing::info!(instrument = %ctx.instrument, %sl_id, qty, trigger, "LIVE stop-loss modified");
                    live_info(db_tx, log_tx, json!({
                        "event": "SL_TRAILED",
                        "instrument": ctx.instrument,
                        "new_sl": trigger,
                        "qty": qty,
                        "mode": "LIVE",
                    })).await;
                }
                Err(e) => {
                    loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                        "could not move the stop to {qty} @ {trigger:.2} ({e}) — squaring the position off at market rather than running it on a stale stop"
                    )).await;
                    exec_exit_all(
                        positions, kotak, db_tx, log_tx, &pending.pos_id, &ctx,
                        ctx.executed_qty, "SL_MODIFY_FAILED",
                    ).await;
                }
            }
        }

        LiveAction::Target1 { slice, keep, new_sl } => {
            let Some(sl_id) = ctx.sl_order_id.clone() else {
                // No stop to shrink — take the whole position off at target 1
                // rather than sell a slice with nothing protecting the runner.
                exec_exit_all(
                    positions, kotak, db_tx, log_tx, &pending.pos_id, &ctx,
                    ctx.executed_qty, "TGT1_FULL",
                ).await;
                return true;
            };

            // Shrink the stop *first*. After this the stop covers `keep` and the
            // sell covers `slice`, and keep + slice == what we hold, so both can
            // fill at once without going short.
            let stop = build_stop_order(&ctx.base, *keep, *new_sl);
            if let Err(e) = kotak_modify(kotak, &stop, &sl_id).await {
                loud_error(db_tx, log_tx, &ctx.instrument, &format!(
                    "could not resize the stop to {keep} for target 1 ({e}) — squaring the whole position off at market instead"
                )).await;
                exec_exit_all(
                    positions, kotak, db_tx, log_tx, &pending.pos_id, &ctx,
                    ctx.executed_qty, "TGT1_SL_RESIZE_FAILED",
                ).await;
                return true;
            }
            with_position(positions, &pending.pos_id, |p| {
                p.sl_order_qty = *keep;
                p.sl_order_trigger = *new_sl;
                p.current_sl = *new_sl;
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
                        "target-1 sell of {slice} was rejected: {e} — the stop now covers only {keep}, squaring the rest off at market"
                    )).await;
                    with_position(positions, &pending.pos_id, |p| {
                        p.force_exit = Some("TGT1_EXIT_FAILED".to_string());
                    }).await;
                }
            }
        }

        LiveAction::ExitAll { qty, reason } => {
            exec_exit_all(positions, kotak, db_tx, log_tx, &pending.pos_id, &ctx, *qty, reason).await;
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

                            let mut already_above_target = false;
                            let ltp_val = ltp_map.get(signal.instrument_name.as_str()).map(|r| *r);
                            if let Some(price) = ltp_val {
                                for t in &signal.targets {
                                    if price >= *t { already_above_target = true; break; }
                                }
                            }
                            
                            if already_above_target {
                                let price = ltp_val.unwrap_or(0.0);
                                let msg = format!(
                                    r#"{{"event":"ERROR","message":"Option to buy already above target","instrument":"{}","price":{:.2}}}"#,
                                    signal.instrument_name, price
                                );
                                send_log(&db_tx, &log_tx, "ERROR", &msg).await;
                                tracing::error!(instrument = %signal.instrument_name, price, "Signal discarded — already above target");
                            } else {
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
                                    ltp_map.insert(ws_scrip.clone(), 0.0);
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
                                    current_sl: sl,
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
                    if !startup_reconciled {
                        let has_session = { kotak.lock().await.is_some() };
                        if has_session {
                            startup_reconciled = true;
                            reconcile_on_startup(&positions, &kotak, &db_tx, &log_tx, cfg.brokerage_per_order).await;
                        }
                    }
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
                        if entry_cutoff {
                            pending.push(Pending {
                                idx: i, ltp: 0.0,
                                action: PosAction::Expire { reason: "EOD_NO_ENTRY".to_string() },
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
                                let lot_size = pos
                                    .resolved_order
                                    .as_ref()
                                    .and_then(|o| o.quantity.parse::<i32>().ok())
                                    .filter(|v| *v > 0)
                                    .unwrap_or(1);

                                let qty = if let Some(override_lots) = pos.override_qty {
                                    override_lots * lot_size
                                } else {
                                    if pos.signal.option_type.is_some() {
                                        let inst = pos.signal.instrument_name.to_uppercase();
                                        const INDEX_NAMES: &[&str] = &["NIFTY", "BANKNIFTY", "FINNIFTY", "MIDCPNIFTY", "SENSEX", "BANKEX"];
                                        let is_index = INDEX_NAMES.iter().any(|&idx| inst.contains(idx));
                                        let lots = if is_index { cfg.index_lots.max(1) } else { cfg.other_lots.max(1) };
                                        lots * lot_size
                                    } else {
                                        let raw_qty = ((cfg.max_trade_amount_inr / ltp).floor() as i32).max(1);
                                        let mut multiple = (raw_qty / lot_size) * lot_size;
                                        if multiple == 0 { multiple = lot_size; }
                                        multiple
                                    }
                                };

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
                                Some(PosAction::ExitSell {
                                    qty: pos.executed_qty, reason: "SL_HIT".to_string(), new_sl: None, exec_price: None,
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
