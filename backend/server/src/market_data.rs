//! Upstox option-chain analytics — NIFTY, BANKNIFTY, SENSEX.
//!
//! **Data only.** Nothing here reads or writes the Kotak client, its mutex, the
//! price map, or positions; it cannot influence order placement. It polls
//! Upstox once a minute during market hours with a read-only Analytics Token
//! (`UPSTOX_ANALYTICS_TOKEN`), keeps the latest chain in memory for the Market
//! page, and writes one aggregated row per (index, strike window) per minute to
//! `oi_snapshots` for the intraday-trend table.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Datelike, NaiveDate, Timelike};
use serde::Serialize;
use shared_domain::DbWriteMessage;
use tokio::sync::{mpsc, RwLock};
use upstox_client::{FutureContract, OptionChainStrike, UpstoxClient};

pub struct IndexSpec {
    /// Id used by the API and DB (`NIFTY`).
    pub id: &'static str,
    /// Upstox underlying key for option chain / contracts.
    pub underlying_key: &'static str,
    /// `underlying_symbol` of the index future in the instruments file.
    pub future_symbol: &'static str,
    pub instruments_url: &'static str,
}

pub const INDICES: [IndexSpec; 3] = [
    IndexSpec {
        id: "NIFTY",
        underlying_key: "NSE_INDEX|Nifty 50",
        future_symbol: "NIFTY",
        instruments_url: upstox_client::NSE_INSTRUMENTS_URL,
    },
    IndexSpec {
        id: "BANKNIFTY",
        underlying_key: "NSE_INDEX|Nifty Bank",
        future_symbol: "BANKNIFTY",
        instruments_url: upstox_client::NSE_INSTRUMENTS_URL,
    },
    IndexSpec {
        id: "SENSEX",
        underlying_key: "BSE_INDEX|SENSEX",
        future_symbol: "SENSEX",
        instruments_url: upstox_client::BSE_INSTRUMENTS_URL,
    },
];

/// Strike windows persisted every minute: ATM ± N strikes, 0 = whole chain.
pub const STRIKE_WINDOWS: [i64; 4] = [5, 10, 20, 0];

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct StrikeRow {
    pub strike: f64,
    pub call_oi: f64,
    pub call_prev_oi: f64,
    pub call_ltp: f64,
    pub call_volume: f64,
    pub call_iv: f64,
    pub put_oi: f64,
    pub put_prev_oi: f64,
    pub put_ltp: f64,
    pub put_volume: f64,
    pub put_iv: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct IndexLive {
    pub expiry: Option<String>,
    pub spot: Option<f64>,
    pub atm_strike: Option<f64>,
    pub future_symbol: Option<String>,
    pub fut_ltp: Option<f64>,
    pub fut_vwap: Option<f64>,
    /// IST `YYYY-MM-DD HH:MM:SS` of the last successful chain poll.
    pub updated_at: Option<String>,
    pub last_error: Option<String>,
    /// Sorted by strike ascending.
    #[serde(skip)]
    pub chain: Vec<StrikeRow>,
}

pub struct MarketDataState {
    pub configured: bool,
    pub indices: RwLock<HashMap<&'static str, IndexLive>>,
}

impl MarketDataState {
    pub fn new(configured: bool) -> Self {
        Self { configured, indices: RwLock::new(HashMap::new()) }
    }
}

// ---------------------------------------------------------------------------
// Aggregation (pure)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq)]
pub struct WindowAgg {
    pub strike_window: i64,
    pub call_oi: f64,
    pub put_oi: f64,
    pub call_oi_chg: f64,
    pub put_oi_chg: f64,
    pub min_strike: f64,
    pub max_strike: f64,
}

pub fn to_strike_rows(chain: &[OptionChainStrike]) -> Vec<StrikeRow> {
    let mut rows: Vec<StrikeRow> = chain
        .iter()
        .map(|s| {
            let call = s.call_options.clone().unwrap_or_default();
            let put = s.put_options.clone().unwrap_or_default();
            let cm = call.market_data.unwrap_or_default();
            let pm = put.market_data.unwrap_or_default();
            StrikeRow {
                strike: s.strike_price,
                call_oi: cm.oi,
                // Missing prev_oi → treat the change as 0, never as the whole OI.
                call_prev_oi: cm.prev_oi.unwrap_or(cm.oi),
                call_ltp: cm.ltp,
                call_volume: cm.volume,
                call_iv: call.option_greeks.map(|g| g.iv).unwrap_or(0.0),
                put_oi: pm.oi,
                put_prev_oi: pm.prev_oi.unwrap_or(pm.oi),
                put_ltp: pm.ltp,
                put_volume: pm.volume,
                put_iv: put.option_greeks.map(|g| g.iv).unwrap_or(0.0),
            }
        })
        .collect();
    rows.sort_by(|a, b| a.strike.total_cmp(&b.strike));
    rows
}

/// Index of the strike closest to spot in a strike-sorted chain.
pub fn atm_index(rows: &[StrikeRow], spot: f64) -> Option<usize> {
    rows.iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| (a.strike - spot).abs().total_cmp(&(b.strike - spot).abs()))
        .map(|(i, _)| i)
}

/// Sums OI over ATM ± `window` strikes (`window == 0` → every strike).
pub fn aggregate(rows: &[StrikeRow], spot: f64, window: i64) -> WindowAgg {
    let mut agg = WindowAgg { strike_window: window, ..Default::default() };
    let Some(atm) = atm_index(rows, spot) else { return agg };
    let (lo, hi) = if window <= 0 {
        (0, rows.len() - 1)
    } else {
        let w = window as usize;
        (atm.saturating_sub(w), (atm + w).min(rows.len() - 1))
    };
    for r in &rows[lo..=hi] {
        agg.call_oi += r.call_oi;
        agg.put_oi += r.put_oi;
        agg.call_oi_chg += r.call_oi - r.call_prev_oi;
        agg.put_oi_chg += r.put_oi - r.put_prev_oi;
    }
    agg.min_strike = rows[lo].strike;
    agg.max_strike = rows[hi].strike;
    agg
}

// ---------------------------------------------------------------------------
// Poller
// ---------------------------------------------------------------------------

const POLL_START_MINUTE: u32 = 9 * 60 + 15;
/// Through 15:31 so the closing "boundary + 1 minute" check (see
/// `routes::market::check_minute`) has a snapshot.
const POLL_END_MINUTE: u32 = 15 * 60 + 31;
/// Instruments files are fetched from 09:00, before the open, so the multi-MB
/// download and parse do not land on top of the 09:15 open on the small VM.
const FUTURES_REFRESH_START_MINUTE: u32 = 9 * 60;

/// True on a weekday when the IST minute-of-day is within `start..=end`.
fn weekday_minute_between(start: u32, end: u32) -> bool {
    let now = shared_domain::now_ist();
    if matches!(now.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
        return false;
    }
    let m = now.hour() * 60 + now.minute();
    (start..=end).contains(&m)
}

/// Sleep until 2s past the next minute boundary so snapshot timestamps line up
/// with the 3/5/15-minute bucket labels.
async fn sleep_to_next_minute() {
    let now = shared_domain::now_ist();
    let secs = 60 - now.second() as u64 + 2;
    tokio::time::sleep(Duration::from_secs(secs.min(62))).await;
}

/// Resolves index futures (for VWAP) once per day per instruments file. Each
/// file is tracked separately, so a failing file is retried on its own
/// (every `FUTURES_RETRY_GAP`) without re-downloading the one that succeeded.
async fn refresh_futures(
    client: &UpstoxClient,
    today: NaiveDate,
    futures: &mut HashMap<String, FutureContract>,
    resolved_day: &mut HashMap<&'static str, NaiveDate>,
    last_attempt: &mut HashMap<&'static str, Instant>,
) {
    const FUTURES_RETRY_GAP: Duration = Duration::from_secs(600);

    for url in [upstox_client::NSE_INSTRUMENTS_URL, upstox_client::BSE_INSTRUMENTS_URL] {
        if resolved_day.get(url) == Some(&today) {
            continue;
        }
        if last_attempt.get(url).is_some_and(|t| t.elapsed() < FUTURES_RETRY_GAP) {
            continue;
        }
        last_attempt.insert(url, Instant::now());
        let wanted: Vec<&str> =
            INDICES.iter().filter(|i| i.instruments_url == url).map(|i| i.future_symbol).collect();
        match client.nearest_index_futures(url, &wanted, today).await {
            Ok(map) => {
                // Replace only this file's symbols; keep the other file's.
                futures.retain(|sym, _| !wanted.contains(&sym.as_str()));
                futures.extend(map);
                resolved_day.insert(url, today);
                tracing::info!(
                    url,
                    futures = ?futures.values().map(|f| &f.trading_symbol).collect::<Vec<_>>(),
                    "Upstox index futures resolved"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, url, "Upstox instruments download failed — VWAP unavailable until retry");
            }
        }
    }
}

pub async fn run_poller(
    client: UpstoxClient,
    state: Arc<MarketDataState>,
    db_tx: mpsc::Sender<DbWriteMessage>,
) {
    let mut expiries: HashMap<&'static str, (NaiveDate, NaiveDate)> = HashMap::new();
    let mut futures: HashMap<String, FutureContract> = HashMap::new();
    let mut futures_resolved_day: HashMap<&'static str, NaiveDate> = HashMap::new();
    let mut futures_attempt: HashMap<&'static str, Instant> = HashMap::new();

    tracing::info!("Upstox market-data poller started");

    loop {
        sleep_to_next_minute().await;
        if !weekday_minute_between(FUTURES_REFRESH_START_MINUTE, POLL_END_MINUTE) {
            continue;
        }
        let today = shared_domain::today_ist();

        refresh_futures(&client, today, &mut futures, &mut futures_resolved_day, &mut futures_attempt).await;

        if !weekday_minute_between(POLL_START_MINUTE, POLL_END_MINUTE) {
            continue;
        }
        let ts = shared_domain::current_ist_timestamp_string();

        let fut_keys: Vec<String> = futures.values().map(|f| f.instrument_key.clone()).collect();
        let quotes = match client.quotes(&fut_keys).await {
            Ok(q) => q,
            Err(e) => {
                tracing::warn!(error = %e, "Upstox futures quote failed");
                HashMap::new()
            }
        };

        for spec in &INDICES {
            let result = poll_index(&client, spec, today, &mut expiries).await;
            let fut = futures.get(spec.future_symbol);
            let quote = fut.and_then(|f| quotes.get(&f.instrument_key));
            let fut_ltp = quote.map(|q| q.last_price).filter(|v| *v > 0.0);
            let fut_vwap = quote.map(|q| q.average_price).filter(|v| *v > 0.0);

            match result {
                Ok((expiry, rows, spot)) => {
                    let expiry_s = expiry.format("%Y-%m-%d").to_string();
                    // Queue DB writes before taking the state lock: if the
                    // shared writer channel is full, only this poller waits,
                    // not the /api/market/* readers.
                    for w in STRIKE_WINDOWS {
                        let agg = aggregate(&rows, spot, w);
                        let _ = db_tx
                            .send(DbWriteMessage::OiSnapshot {
                                ts: ts.clone(),
                                underlying: spec.id.to_string(),
                                expiry: expiry_s.clone(),
                                strike_window: w,
                                call_oi: agg.call_oi,
                                put_oi: agg.put_oi,
                                call_oi_chg: agg.call_oi_chg,
                                put_oi_chg: agg.put_oi_chg,
                                spot,
                                fut_ltp,
                                fut_vwap,
                            })
                            .await;
                    }
                    let mut guard = state.indices.write().await;
                    let live = guard.entry(spec.id).or_default();
                    live.atm_strike = atm_index(&rows, spot).map(|i| rows[i].strike);
                    live.expiry = Some(expiry_s);
                    live.spot = Some(spot);
                    live.chain = rows;
                    live.future_symbol = fut.map(|f| f.trading_symbol.clone());
                    live.fut_ltp = fut_ltp;
                    live.fut_vwap = fut_vwap;
                    live.updated_at = Some(ts.clone());
                    live.last_error = None;
                }
                Err(e) => {
                    let msg = if e.is_unauthorized() {
                        "Upstox rejected the analytics token (HTTP 401/403) — regenerate it".to_string()
                    } else {
                        e.to_string()
                    };
                    let mut guard = state.indices.write().await;
                    let live = guard.entry(spec.id).or_default();
                    // Log each distinct error once, so a new failure (e.g. a
                    // later token revocation) is not hidden by an earlier one.
                    if live.last_error.as_deref() != Some(msg.as_str()) {
                        tracing::warn!(index = spec.id, error = %e, "Upstox option chain poll failed");
                    }
                    live.last_error = Some(msg);
                }
            }
        }
    }
}

async fn poll_index(
    client: &UpstoxClient,
    spec: &IndexSpec,
    today: NaiveDate,
    expiries: &mut HashMap<&'static str, (NaiveDate, NaiveDate)>,
) -> Result<(NaiveDate, Vec<StrikeRow>, f64), upstox_client::UpstoxError> {
    let expiry = match expiries.get(spec.id) {
        Some((day, exp)) if *day == today => *exp,
        _ => {
            let exp = client.nearest_option_expiry(spec.underlying_key, today).await?;
            expiries.insert(spec.id, (today, exp));
            exp
        }
    };
    let chain = client.option_chain(spec.underlying_key, expiry).await?;
    let spot = chain.iter().map(|s| s.underlying_spot_price).find(|p| *p > 0.0).unwrap_or(0.0);
    let rows = to_strike_rows(&chain);
    if rows.is_empty() || spot <= 0.0 {
        return Err(upstox_client::UpstoxError::Decode(format!(
            "empty option chain for {} expiry {expiry}",
            spec.id
        )));
    }
    Ok((expiry, rows, spot))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(strike: f64, call_oi: f64, call_prev: f64, put_oi: f64, put_prev: f64) -> StrikeRow {
        StrikeRow {
            strike, call_oi, call_prev_oi: call_prev, call_ltp: 0.0, call_volume: 0.0, call_iv: 0.0,
            put_oi, put_prev_oi: put_prev, put_ltp: 0.0, put_volume: 0.0, put_iv: 0.0,
        }
    }

    #[test]
    fn null_prev_oi_counts_as_no_change() {
        let body = r#"[{"expiry":"2025-02-13","strike_price":21100,"underlying_spot_price":22976.2,
            "call_options":{"instrument_key":"NSE_FO|1","market_data":{"oi":750,"prev_oi":1500}},
            "put_options":{"instrument_key":"NSE_FO|2","market_data":{"oi":5636475,"prev_oi":null}}}]"#;
        let chain: Vec<OptionChainStrike> = serde_json::from_str(body).unwrap();
        let rows = to_strike_rows(&chain);
        let agg = aggregate(&rows, 22976.2, 0);
        assert_eq!(agg.call_oi_chg, -750.0);
        assert_eq!(agg.put_oi_chg, 0.0);
        assert_eq!(agg.put_oi, 5636475.0);
    }

    #[test]
    fn aggregates_atm_window_and_whole_chain() {
        let rows = vec![
            row(100.0, 1.0, 0.0, 10.0, 5.0),
            row(200.0, 2.0, 1.0, 20.0, 5.0),
            row(300.0, 3.0, 1.0, 30.0, 5.0),
            row(400.0, 4.0, 1.0, 40.0, 5.0),
            row(500.0, 5.0, 1.0, 50.0, 5.0),
        ];
        // spot 310 → ATM 300; ±1 → 200..400
        let w1 = aggregate(&rows, 310.0, 1);
        assert_eq!((w1.call_oi, w1.put_oi), (9.0, 90.0));
        assert_eq!((w1.call_oi_chg, w1.put_oi_chg), (6.0, 75.0));
        assert_eq!((w1.min_strike, w1.max_strike), (200.0, 400.0));
        // window larger than chain clamps
        let w9 = aggregate(&rows, 110.0, 9);
        assert_eq!((w9.min_strike, w9.max_strike), (100.0, 500.0));
        let all = aggregate(&rows, 310.0, 0);
        assert_eq!(all.call_oi, 15.0);
        assert_eq!(aggregate(&[], 100.0, 5), WindowAgg { strike_window: 5, ..Default::default() });
    }
}
