use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
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
    },
    Cancel { reason: String },
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
) {
    let _ = tx.send(DbWriteMessage::Trade {
        ticker: ticker.to_owned(), action: action.to_owned(), qty,
        executed_price: price,
        gross_value: c.gross_value, brokerage: c.brokerage,
        stt_charge: c.stt_charge, sebi_fee: c.sebi_fee,
        stamp_duty: c.stamp_duty, transaction_charge: c.transaction_charge,
        gst: c.gst, net_value: c.net_value,
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
) {
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    tracing::info!("Position monitor started");

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

                        // If the signal has an ID (e.g. edited Telegram message), try to update existing
                        if let Some(ref sig_id) = signal.signal_id {
                            let mut write_guard = positions.write().await;
                            let mut updated = false;
                            for p in write_guard.iter_mut() {
                                if p.signal.signal_id.as_ref() == Some(sig_id) {
                                    if p.signal.entry_price != signal.entry_price || p.signal.entry_condition != signal.entry_condition {
                                        p.force_exit = Some("ENTRY_CHANGED_ERROR".to_string());
                                        if matches!(p.state, TradeState::WaitingForEntry) {
                                            p.state = TradeState::Closed;
                                        }
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
                                if let Some(ref store) = *scrip_guard {
                                    if let Some(record) = store.resolve_signal(&signal) {
                                        // Build OrderRequest
                                        use shared_domain::{OrderRequest, AmoFlag, ExchangeSegment, ProductCode, OrderType, Validity, TransactionType};
                                        let qty = record.lot_size.to_string(); // we use lot size by default
                                        resolved_token = Some(record.instrument_token.clone());
                                        resolved_segment_code = Some(record.exchange_segment_code.clone());
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

                                let mut pos_guard = positions.write().await;
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
                let mut pos_guard = positions.write().await;
                if pos_guard.is_empty() { continue; }

                let cfg = config.read().await;
                let mut pending: Vec<Pending> = Vec::new();
                let mut positions_mutated = false;

                // ── Pass 1: read-only scan ────────────────────────── //
                for (i, pos) in pos_guard.iter().enumerate() {
                    let lookup_key = pos.ws_scrip_key.as_ref().unwrap_or(&pos.signal.instrument_name);
                    let ltp = match ltp_map.get(lookup_key).map(|r| *r) {
                        Some(v) => v,
                        None => continue,
                    };

                    let pa = match pos.state {
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
                                        cfg.default_option_lots.max(1) * lot_size
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
                                    qty: pos.executed_qty, reason: reason.clone(), new_sl: None,
                                })
                            } else if ltp <= pos.current_sl {
                                Some(PosAction::ExitSell {
                                    qty: pos.executed_qty, reason: "SL_HIT".to_string(), new_sl: None,
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
                                    pos.avg_buy_price
                                });
                                Some(PosAction::ExitSell {
                                    qty: exit_qty,
                                    reason: (if has_t2 { "TGT1_PARTIAL" } else { "TGT1_FULL" }).to_string(),
                                    new_sl,
                                })
                            } else { None }
                        }

                        TradeState::Target1Hit => {
                            if let Some(ref reason) = pos.force_exit {
                                Some(PosAction::ExitSell {
                                    qty: pos.executed_qty, reason: reason.clone(), new_sl: None,
                                })
                            } else if ltp <= pos.current_sl {
                                Some(PosAction::ExitSell {
                                    qty: pos.executed_qty, reason: "TRAIL_SL_HIT".to_string(), new_sl: None,
                                })
                            } else if pos.signal.targets.len() > 1 && ltp >= pos.signal.targets[1] {
                                Some(PosAction::ExitSell {
                                    qty: pos.executed_qty, reason: "TGT2_HIT".to_string(), new_sl: None,
                                })
                            } else { None }
                        }

                        TradeState::Closed => None,
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
                            send_trade(&db_tx, &instrument, "BUY", qty, pa.ltp, &fees).await;
                            send_log(&db_tx, &log_tx, "INFO", &msg).await;
                        }

                        PosAction::ExitSell { qty, reason, new_sl } => {
                            let fees = FeeCalculator::calculate(
                                qty, pa.ltp, "SELL", is_options, cfg.brokerage_per_order,
                            );
                            let pnl = fees.net_value - pos.avg_buy_price * qty as f64;
                            let msg = format!(
                                r#"{{"event":"{reason}","instrument":"{instrument}","price":{:.2},"qty":{qty},"pnl":{pnl:.2}}}"#,
                                pa.ltp
                            );
                            tracing::info!(instrument = %instrument, reason, pnl, "Exit executed");
                            send_trade(&db_tx, &instrument, "SELL", qty, pa.ltp, &fees).await;
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
