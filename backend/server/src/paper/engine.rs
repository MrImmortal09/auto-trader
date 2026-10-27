//! The paper watch loop: every 500 ms, each open paper position's chart stop
//! and target, and the expiry square-off. Nothing else — no trailing, no
//! targets of its own, no signals, no sizing. It is handed the desk alone.

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use chrono::{DateTime, FixedOffset, NaiveTime};

use super::{PaperDesk, PaperPosition};

const EVERY: Duration = Duration::from_millis(500);

fn at(h: u32, m: u32, s: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, s).expect("a valid time")
}

/// From here on its expiry day, a position is sold at the first fresh price.
pub fn square_off_from() -> NaiveTime {
    at(15, 29, 30)
}

/// Whether a contract expiring on `expiry` is being squared off at `now`:
/// nothing more of it may be bought.
pub fn squaring_off(expiry: chrono::NaiveDate, now: DateTime<FixedOffset>) -> bool {
    let today = now.date_naive();
    expiry < today || (expiry == today && now.time() >= square_off_from())
}

/// What the watch loop does with `pos` at `now`: sell all of it at a price,
/// for a reason, or nothing yet. `premium` and `index` are fresh prices only.
///
/// - Expiry: from 15:29:30 IST on its expiry day, at the first fresh price;
///   with none by the close, at the last one seen (0 if there never was one).
///   A contract found past its expiry (the server was down) settles at once
///   the same way.
/// - Chart levels: only while the market is open, and only with a fresh
///   premium to sell at — a level crossed on a stale price waits.
pub fn decide(
    pos: &PaperPosition,
    now: DateTime<FixedOffset>,
    trading_day: bool,
    premium: Option<f64>,
    index: Option<f64>,
) -> Option<(f64, &'static str)> {
    let t = now.time();
    let close = at(15, 30, 0);
    if squaring_off(pos.expiry, now) {
        let expired = pos.expiry < now.date_naive();
        if let Some(p) = premium.filter(|_| !expired) {
            return Some((p, "EXPIRY_SQUAREOFF"));
        }
        if expired || t >= close {
            return Some((pos.last_ltp.filter(|p| *p > 0.0).unwrap_or(0.0), "EXPIRY_SQUAREOFF"));
        }
        return None;
    }
    if !(trading_day && t >= at(9, 15, 0) && t < close) {
        return None;
    }
    let reason = shared_domain::chart_level_reached(Some(&pos.option_type), &pos.chart, premium, index)?;
    Some((premium?, reason))
}

/// One pass over every open position at `now`. `fresh` gives a feed key's
/// price while it is recent enough to trade on.
pub async fn step(desk: &PaperDesk, now: DateTime<FixedOffset>, trading_day: bool, fresh: impl Fn(&str) -> Option<f64>) {
    let open = desk.all_open().await;
    if open.is_empty() {
        return;
    }
    let mut seen = Vec::new();
    for (user_id, pos) in open {
        let premium = fresh(&pos.ws_key);
        let index = shared_domain::kotak_index_feed(&pos.index).and_then(|(_, name)| fresh(&shared_domain::index_price_key(name)));
        if let Some(p) = premium {
            seen.push((user_id, pos.id.clone(), p));
        }
        let Some((price, reason)) = decide(&pos, now, trading_day, premium, index) else { continue };
        match desk.settle(user_id, &pos.id, price, reason).await {
            Ok(f) => tracing::info!(user_id, "Paper {reason}: sold {} {} at {:.2}", f.qty, f.contract, f.price),
            // Sold by hand meanwhile.
            Err(r) if r.status == StatusCode::NOT_FOUND => {}
            Err(r) => tracing::warn!(user_id, "Paper {reason} of {} not filled: {}", pos.label(), r.message),
        }
    }
    desk.note_prices(&seen).await;
}

/// The watch loop, for as long as the server runs.
pub async fn run(desk: Arc<PaperDesk>) {
    let mut tick = tokio::time::interval(EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let fresh = |key: &str| {
        desk.price(key)
            .filter(|_| crate::execute::contract_age_secs(key).is_some_and(|age| age <= crate::execute::MAX_CONTRACT_AGE_SECS))
    };
    loop {
        tick.tick().await;
        let now = shared_domain::now_ist();
        let trading_day = !telegram_ingester::parser::is_trading_holiday(now.date_naive());
        step(&desk, now, trading_day, fresh).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paper::tests::{desk, nifty};
    use chrono::{NaiveDate, TimeZone};
    use shared_domain::{ChartLevel, ChartLevels, LevelTrigger};

    fn ist(d: (i32, u32, u32), t: (u32, u32, u32)) -> DateTime<FixedOffset> {
        FixedOffset::east_opt(5 * 3600 + 1800)
            .unwrap()
            .with_ymd_and_hms(d.0, d.1, d.2, t.0, t.1, t.2)
            .unwrap()
    }

    fn held(side: &str, chart: ChartLevels) -> PaperPosition {
        let c = nifty(25_000.0, side);
        PaperPosition {
            id: "p".into(),
            index: c.index,
            trading_symbol: c.trading_symbol,
            ws_key: c.ws_key,
            expiry: NaiveDate::from_ymd_opt(2026, 10, 27).unwrap(),
            strike: c.strike,
            option_type: c.option_type,
            lot_size: 75,
            qty: 75,
            avg_price: 100.0,
            chart,
            opened_at: "2026-10-27 10:00:00".into(),
            last_ltp: Some(101.0),
        }
    }

    #[test]
    fn an_expiring_position_squares_off_from_15_29_30_and_settles_by_the_close() {
        let p = held("CE", ChartLevels::default());
        let day = (2026, 10, 27);
        assert_eq!(decide(&p, ist(day, (15, 29, 29)), true, Some(90.0), None), None);
        assert_eq!(decide(&p, ist(day, (15, 29, 30)), true, Some(90.0), None), Some((90.0, "EXPIRY_SQUAREOFF")));
        // No fresh price yet: wait for one until the close, then take the last seen.
        assert_eq!(decide(&p, ist(day, (15, 29, 45)), true, None, None), None);
        assert_eq!(decide(&p, ist(day, (15, 30, 0)), true, None, None), Some((101.0, "EXPIRY_SQUAREOFF")));
        let mut never_priced = p.clone();
        never_priced.last_ltp = None;
        assert_eq!(decide(&never_priced, ist(day, (15, 40, 0)), true, None, None), Some((0.0, "EXPIRY_SQUAREOFF")));
        // Found the day after: settled at once, never at a stale tick of a dead contract.
        assert_eq!(decide(&p, ist((2026, 10, 28), (9, 0, 0)), true, Some(5.0), None), Some((101.0, "EXPIRY_SQUAREOFF")));
        // A later expiry is left alone.
        let mut later = p.clone();
        later.expiry = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        assert_eq!(decide(&later, ist(day, (15, 35, 0)), true, None, None), None);
        assert!(squaring_off(p.expiry, ist(day, (15, 29, 30))) && !squaring_off(later.expiry, ist(day, (15, 29, 30))));
    }

    #[test]
    fn chart_levels_sell_only_in_market_hours_and_only_on_a_fresh_premium() {
        let stop = ChartLevel { price: 25_060.0, trigger: LevelTrigger::Index };
        let target = ChartLevel { price: 140.0, trigger: LevelTrigger::Premium };
        let p = held("CE", ChartLevels { stop: Some(stop), target: Some(target) });
        let mut later = p.clone();
        later.expiry = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        let open = ist((2026, 10, 27), (11, 0, 0));
        assert_eq!(decide(&later, open, true, Some(100.0), Some(25_100.0)), None);
        assert_eq!(decide(&later, open, true, Some(95.0), Some(25_050.0)), Some((95.0, "CHART_STOP")));
        assert_eq!(decide(&later, open, true, Some(141.0), Some(25_100.0)), Some((141.0, "CHART_TARGET")));
        // Crossed on the index with no fresh premium to sell at: wait.
        assert_eq!(decide(&later, open, true, None, Some(25_000.0)), None);
        // Closed market, or a holiday.
        assert_eq!(decide(&later, ist((2026, 10, 27), (9, 14, 0)), true, Some(95.0), Some(25_000.0)), None);
        assert_eq!(decide(&later, open, false, Some(95.0), Some(25_000.0)), None);
    }

    #[tokio::test]
    async fn a_step_sells_what_crossed_and_keeps_the_last_price_of_the_rest() {
        let (_db, desk) = desk("engine").await;
        let mut c = nifty(25_000.0, "CE");
        c.expiry = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        let a = desk.buy(1, &c, 1, 100.0).await.unwrap();
        let mut other = nifty(25_100.0, "PE");
        other.expiry = c.expiry;
        let b = desk.buy(1, &other, 1, 80.0).await.unwrap();
        desk.set_levels(1, &a.position_id, Some(Some(ChartLevel { price: 95.0, trigger: LevelTrigger::Premium })), None)
            .await
            .unwrap();
        let prices = |key: &str| match key {
            "nse_fo|25000CE" => Some(94.0),
            "nse_fo|25100PE" => Some(82.5),
            _ => None,
        };
        step(&desk, ist((2026, 10, 27), (11, 0, 0)), true, prices).await;
        let open = desk.open(1).await;
        assert_eq!(open.len(), 1);
        assert_eq!((open[0].id.as_str(), open[0].last_ltp), (b.position_id.as_str(), Some(82.5)));
        let legs = crate::history::load_user_legs(&desk.pool, 1, 0).await.unwrap();
        let sell = legs.last().unwrap();
        assert_eq!((sell.action.as_str(), sell.executed_price, sell.exit_reason.as_deref()), ("SELL", 94.0, Some("CHART_STOP")));
        assert_eq!(desk.activity(1)[0].message, "Sold 75 NIFTY 25000 CE at ₹94.00 (chart stop)");
    }
}
