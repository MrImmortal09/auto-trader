//! The paper watch loop: every 500 ms, each open paper position's chart stop
//! and target, and the expiry square-off. Nothing else — no trailing, no
//! targets of its own, no signals, no sizing. It is handed the desk alone.
//! An index option and a stock option are watched alike: the premium is the
//! contract's own tick, and a level on the underlying reads the index's feed
//! value or the stock's cash row (`PaperPosition::underlying_price_key`).

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use chrono::{DateTime, FixedOffset, NaiveTime};

use shared_domain::ChartHit;

use super::{PaperDesk, PaperPosition};

const EVERY: Duration = Duration::from_millis(500);

fn at(h: u32, m: u32, s: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, s).expect("a valid time")
}

/// From here on its expiry day, a position is sold: the owner's engine's
/// expiry square-off time (15:38 IST), so no desk squares off earlier.
pub fn square_off_from() -> NaiveTime {
    at(shared_domain::EXPIRY_SQUAREOFF_HOUR, shared_domain::EXPIRY_SQUAREOFF_MINUTE, 0)
}

/// Whether a contract expiring on `expiry` is being squared off at `now`:
/// nothing more of it may be bought.
pub fn squaring_off(expiry: chrono::NaiveDate, now: DateTime<FixedOffset>) -> bool {
    let today = now.date_naive();
    expiry < today || (expiry == today && now.time() >= square_off_from())
}

/// What the watch loop sells of a position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sell {
    /// All of it, for this reason.
    All(&'static str),
    /// The lots of its chart targets `ids`, leaving the rest held.
    Targets(Vec<String>),
}

/// What the watch loop does with `pos` at `now`: sell all of it, or its
/// reached targets' lots, at a price — or nothing yet. `premium` and `index`
/// are fresh prices only.
///
/// - Expiry: at 15:38 IST on its expiry day (two minutes before the 15:40
///   options close), at a fresh price if there is one, else at the last one
///   seen (0 if there never was one). A contract found past its expiry (the
///   server was down) settles at once the same way.
/// - Chart levels: only while the market is open, and only with a fresh
///   premium to sell at — a level crossed on a stale price waits. The stop
///   sells everything; a target its lots (see `shared_domain::chart_hit`).
pub fn decide(
    pos: &PaperPosition,
    now: DateTime<FixedOffset>,
    trading_day: bool,
    premium: Option<f64>,
    index: Option<f64>,
) -> Option<(f64, Sell)> {
    let t = now.time();
    let close = at(15, 30, 0);
    if squaring_off(pos.expiry, now) {
        let expired = pos.expiry < now.date_naive();
        if let Some(p) = premium.filter(|_| !expired) {
            return Some((p, Sell::All("EXPIRY_SQUAREOFF")));
        }
        if expired || t >= close {
            return Some((pos.last_ltp.filter(|p| *p > 0.0).unwrap_or(0.0), Sell::All("EXPIRY_SQUAREOFF")));
        }
        return None;
    }
    let in_window = if shared_domain::special_session_on(now.date_naive()).is_some() {
        use chrono::Datelike;
        let is_weekend = matches!(now.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun);
        shared_domain::in_special_session_at(now) || (trading_day && !is_weekend && t >= at(9, 15, 0) && t < close)
    } else {
        trading_day && t >= at(9, 15, 0) && t < close
    };
    if !in_window {
        return None;
    }
    let hit = shared_domain::chart_hit(Some(&pos.option_type), &pos.chart, premium, index, pos.lots())?;
    let sell = match hit {
        ChartHit::All { reason } => Sell::All(reason),
        ChartHit::Part { ids, .. } => Sell::Targets(ids),
    };
    Some((premium?, sell))
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
        let index = pos.underlying_price_key().and_then(|key| fresh(&key));
        if let Some(p) = premium {
            seen.push((user_id, pos.id.clone(), p));
        }
        let Some((price, sell)) = decide(&pos, now, trading_day, premium, index) else { continue };
        let (sold, reason) = match &sell {
            Sell::All(reason) => (desk.settle(user_id, &pos.id, price, reason).await, *reason),
            Sell::Targets(ids) => (desk.settle_targets(user_id, &pos.id, ids, price).await, "CHART_TARGET"),
        };
        match sold {
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
    use crate::paper::tests::{desk, nifty, reliance};
    use chrono::{NaiveDate, TimeZone};
    use crate::routes::v2::positions::LevelsUpdate;
    use shared_domain::{ChartLevel, ChartLevels, ChartTarget, LevelTrigger};

    fn all(reason: &'static str) -> Sell {
        Sell::All(reason)
    }

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
            underlying_key: None,
            segment: "nse_fo".into(),
        }
    }

    /// A RELIANCE 1400 CE held, expiring 24 Nov 2026 (its monthly).
    fn held_stock(chart: ChartLevels) -> PaperPosition {
        let c = reliance(1_400.0, "CE");
        PaperPosition {
            id: "r".into(),
            index: c.index,
            trading_symbol: c.trading_symbol,
            ws_key: c.ws_key,
            expiry: c.expiry,
            strike: c.strike,
            option_type: c.option_type,
            lot_size: 500,
            qty: 500,
            avg_price: 40.0,
            chart,
            opened_at: "2026-11-10 10:00:00".into(),
            last_ltp: Some(41.0),
            underlying_key: c.underlying_key,
            segment: c.segment,
        }
    }

    #[test]
    fn an_expiring_position_squares_off_at_15_38_and_never_before() {
        let p = held("CE", ChartLevels::default());
        let day = (2026, 10, 27);
        // Nothing before 15:38, fresh price or not.
        assert_eq!(decide(&p, ist(day, (15, 29, 30)), true, Some(90.0), None), None);
        assert_eq!(decide(&p, ist(day, (15, 30, 0)), true, None, None), None);
        assert_eq!(decide(&p, ist(day, (15, 37, 59)), true, Some(90.0), None), None);
        // At 15:38: a fresh price if there is one, else the last seen.
        assert_eq!(decide(&p, ist(day, (15, 38, 0)), true, Some(90.0), None), Some((90.0, all("EXPIRY_SQUAREOFF"))));
        assert_eq!(decide(&p, ist(day, (15, 38, 0)), true, None, None), Some((101.0, all("EXPIRY_SQUAREOFF"))));
        let mut never_priced = p.clone();
        never_priced.last_ltp = None;
        assert_eq!(decide(&never_priced, ist(day, (15, 40, 0)), true, None, None), Some((0.0, all("EXPIRY_SQUAREOFF"))));
        // Found the day after: settled at once, never at a stale tick of a dead contract.
        assert_eq!(decide(&p, ist((2026, 10, 28), (9, 0, 0)), true, Some(5.0), None), Some((101.0, all("EXPIRY_SQUAREOFF"))));
        // A later expiry is left alone.
        let mut later = p.clone();
        later.expiry = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        assert_eq!(decide(&later, ist(day, (15, 35, 0)), true, None, None), None);
        assert!(!squaring_off(p.expiry, ist(day, (15, 37, 59))));
        assert!(squaring_off(p.expiry, ist(day, (15, 38, 0))) && !squaring_off(later.expiry, ist(day, (15, 38, 0))));
    }

    #[test]
    fn chart_levels_sell_only_in_market_hours_and_only_on_a_fresh_premium() {
        let _guard = shared_domain::special_sessions_test_guard();
        let stop = ChartLevel { price: 25_060.0, trigger: LevelTrigger::Index };
        let target = ChartLevel { price: 140.0, trigger: LevelTrigger::Premium };
        let target = ChartTarget { id: "t".into(), price: target.price, trigger: target.trigger, lots: None };
        let p = held("CE", ChartLevels { stop: Some(stop), targets: vec![target] });
        let mut later = p.clone();
        later.expiry = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        let open = ist((2026, 10, 27), (11, 0, 0));
        assert_eq!(decide(&later, open, true, Some(100.0), Some(25_100.0)), None);
        assert_eq!(decide(&later, open, true, Some(95.0), Some(25_050.0)), Some((95.0, all("CHART_STOP"))));
        assert_eq!(decide(&later, open, true, Some(141.0), Some(25_100.0)), Some((141.0, all("CHART_TARGET"))));
        // A target with fewer lots than are held sells only those.
        let mut two = later.clone();
        two.qty = 150;
        two.chart.targets[0].lots = Some(1);
        assert_eq!(decide(&two, open, true, Some(141.0), Some(25_100.0)), Some((141.0, Sell::Targets(vec!["t".into()]))));
        // Crossed on the index with no fresh premium to sell at: wait.
        assert_eq!(decide(&later, open, true, None, Some(25_000.0)), None);
        // Closed market, or a holiday.
        assert_eq!(decide(&later, ist((2026, 10, 27), (9, 14, 0)), true, Some(95.0), Some(25_000.0)), None);
        assert_eq!(decide(&later, open, false, Some(95.0), Some(25_000.0)), None);

        // Special session on a Sunday (2026-11-08 14:20–15:30)
        let sunday = (2026, 11, 8);
        let mut session_pos = later.clone();
        session_pos.expiry = NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        shared_domain::set_special_sessions(vec![shared_domain::SpecialSession {
            date: NaiveDate::from_ymd_opt(2026, 11, 8).unwrap(),
            start: 14 * 60 + 20,
            end: 15 * 60 + 30,
        }]);
        // Outside session window: Sunday 10:00 -> None
        assert_eq!(decide(&session_pos, ist(sunday, (10, 0, 0)), true, Some(95.0), Some(25_050.0)), None);
        // Inside session window: Sunday 14:30 -> triggers stop
        assert_eq!(decide(&session_pos, ist(sunday, (14, 30, 0)), true, Some(95.0), Some(25_050.0)), Some((95.0, all("CHART_STOP"))));
        // At/after session end: Sunday 15:30 -> None
        assert_eq!(decide(&session_pos, ist(sunday, (15, 30, 0)), true, Some(95.0), Some(25_050.0)), None);

        // Special session on a weekday holiday (trading_day=false) with session 18:00–19:15
        let weekday = (2026, 11, 4);
        shared_domain::set_special_sessions(vec![shared_domain::SpecialSession {
            date: NaiveDate::from_ymd_opt(weekday.0, weekday.1, weekday.2).unwrap(),
            start: 18 * 60,
            end: 19 * 60 + 15,
        }]);
        // Outside session window: weekday holiday 10:00 -> None
        assert_eq!(decide(&session_pos, ist(weekday, (10, 0, 0)), false, Some(95.0), Some(25_050.0)), None);
        // Inside session window: weekday holiday 18:30 -> triggers stop
        assert_eq!(decide(&session_pos, ist(weekday, (18, 30, 0)), false, Some(95.0), Some(25_050.0)), Some((95.0, all("CHART_STOP"))));

        shared_domain::set_special_sessions(vec![]);
    }

    #[test]
    fn a_stock_option_squares_off_at_15_38_on_its_own_expiry_day_only() {
        let p = held_stock(ChartLevels::default());
        let expiry = (2026, 11, 24);
        // The day before, and the index expiries around it, leave it alone.
        assert_eq!(decide(&p, ist((2026, 11, 23), (15, 38, 0)), true, Some(39.0), None), None);
        assert_eq!(decide(&p, ist(expiry, (15, 37, 59)), true, Some(39.0), None), None);
        assert_eq!(decide(&p, ist(expiry, (15, 38, 0)), true, Some(39.0), None), Some((39.0, all("EXPIRY_SQUAREOFF"))));
        assert_eq!(decide(&p, ist(expiry, (15, 38, 0)), true, None, None), Some((41.0, all("EXPIRY_SQUAREOFF"))));
        // Found after its expiry: settled at once at the last price seen.
        assert_eq!(decide(&p, ist((2026, 11, 25), (9, 20, 0)), true, Some(1.0), None), Some((41.0, all("EXPIRY_SQUAREOFF"))));
        assert!(squaring_off(p.expiry, ist(expiry, (15, 38, 0))) && !squaring_off(p.expiry, ist(expiry, (15, 37, 59))));
    }

    #[test]
    fn a_stock_options_levels_trigger_on_its_premium_or_on_the_stocks_price() {
        let _guard = shared_domain::special_sessions_test_guard();
        let stop = ChartLevel { price: 1_380.0, trigger: LevelTrigger::Index };
        let target = ChartTarget { id: "t".into(), price: 55.0, trigger: LevelTrigger::Premium, lots: None };
        let p = held_stock(ChartLevels { stop: Some(stop), targets: vec![target] });
        let open = ist((2026, 11, 10), (11, 0, 0));
        // The stock above the stop, the premium below the target: nothing.
        assert_eq!(decide(&p, open, true, Some(42.0), Some(1_395.0)), None);
        // A call's stop on the stock is reached as the stock falls.
        assert_eq!(decide(&p, open, true, Some(38.0), Some(1_379.5)), Some((38.0, all("CHART_STOP"))));
        // Reached on the stock with no fresh premium to sell at: wait.
        assert_eq!(decide(&p, open, true, None, Some(1_370.0)), None);
        // The target on the premium.
        assert_eq!(decide(&p, open, true, Some(55.5), Some(1_420.0)), Some((55.5, all("CHART_TARGET"))));
        // A put's stop on the stock is reached as the stock rises.
        let mut put = p.clone();
        put.option_type = "PE".into();
        put.chart.stop = Some(ChartLevel { price: 1_420.0, trigger: LevelTrigger::Index });
        assert_eq!(decide(&put, open, true, Some(38.0), Some(1_419.0)), None);
        assert_eq!(decide(&put, open, true, Some(38.0), Some(1_420.0)), Some((38.0, all("CHART_STOP"))));
        // Outside market hours nothing triggers.
        assert_eq!(decide(&p, ist((2026, 11, 10), (15, 31, 0)), true, Some(38.0), Some(1_370.0)), None);
    }

    #[tokio::test]
    async fn a_step_reads_a_stocks_price_from_its_cash_row() {
        let (_db, desk) = desk("engine-stock").await;
        let c = reliance(1_400.0, "CE");
        let a = desk.buy(2, &c, 1, 40.0).await.unwrap();
        let stop = LevelsUpdate { stop: Some(Some(ChartLevel { price: 1_380.0, trigger: LevelTrigger::Index })), ..Default::default() };
        desk.set_levels(2, &a.position_id, &stop).await.unwrap();
        let mut other = nifty(25_000.0, "CE");
        other.expiry = NaiveDate::from_ymd_opt(2026, 11, 24).unwrap();
        let b = desk.buy(2, &other, 1, 100.0).await.unwrap();
        // The stock stays above the stop: held, its last price noted.
        let prices = |key: &str| match key {
            "nse_fo|R1400CE" => Some(39.0),
            "nse_cm|2885" => Some(1_390.0),
            "nse_fo|25000CE" => Some(101.0),
            _ => None,
        };
        step(&desk, ist((2026, 11, 10), (11, 0, 0)), true, prices).await;
        assert_eq!(desk.open(2).await.len(), 2);
        assert_eq!(desk.open(2).await[0].last_ltp, Some(39.0));
        // The stock falls through it: sold at the premium's fresh price; the index option is left alone.
        let prices = |key: &str| match key {
            "nse_fo|R1400CE" => Some(37.0),
            "nse_cm|2885" => Some(1_379.0),
            "nse_fo|25000CE" => Some(101.0),
            _ => None,
        };
        step(&desk, ist((2026, 11, 10), (11, 0, 1)), true, prices).await;
        let open = desk.open(2).await;
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, b.position_id);
        assert_eq!(desk.activity(2)[0].message, "Sold 500 RELIANCE 1400 CE at ₹37.00 (chart stop)");
        // On the stock option's expiry day at 15:38 the index option (same expiry) goes too — each on its own expiry.
        step(&desk, ist((2026, 11, 24), (15, 38, 0)), true, prices).await;
        assert!(desk.open(2).await.is_empty());
        let legs = crate::history::load_user_legs(&desk.pool, 2, 0).await.unwrap();
        assert_eq!(legs.last().unwrap().exit_reason.as_deref(), Some("EXPIRY_SQUAREOFF"));
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
        let stop = LevelsUpdate { stop: Some(Some(ChartLevel { price: 95.0, trigger: LevelTrigger::Premium })), ..Default::default() };
        desk.set_levels(1, &a.position_id, &stop).await.unwrap();
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
