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

use super::lines::PaperLineOrder;
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
    if !in_window(now, trading_day) {
        return None;
    }
    let hit = shared_domain::chart_hit(Some(&pos.option_type), &pos.chart, premium, index, pos.lots())?;
    let sell = match hit {
        ChartHit::All { reason } => Sell::All(reason),
        ChartHit::Part { ids, .. } => Sell::Targets(ids),
    };
    Some((premium?, sell))
}

/// Whether the desk's chart levels and limit sells act at `now`: 09:15–15:30
/// IST on a trading day, or inside a special session.
fn in_window(now: DateTime<FixedOffset>, trading_day: bool) -> bool {
    let t = now.time();
    let close = at(15, 30, 0);
    if shared_domain::special_session_on(now.date_naive()).is_some() {
        use chrono::Datelike;
        let is_weekend = matches!(now.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun);
        shared_domain::in_special_session_at(now) || (trading_day && !is_weekend && t >= at(9, 15, 0) && t < close)
    } else {
        trading_day && t >= at(9, 15, 0) && t < close
    }
}

/// What the watch loop does with a position's resting limit sell.
#[derive(Debug, Clone, PartialEq)]
pub enum LimitMove {
    /// Let it go, selling nothing: the market closed (it lasts the day).
    Drop,
    /// Sell it at this price.
    Fill(f64),
}

/// What happens to `pos`'s limit sell at `now`, with `premium` its fresh
/// price: past the session it goes; reached, it fills at its price (or at
/// the price now, for one that was already reached when it was placed). On
/// its expiry day's square-off it is left to the square-off, which cancels
/// it before it sells everything.
pub fn limit_move(pos: &PaperPosition, now: DateTime<FixedOffset>, trading_day: bool, premium: Option<f64>) -> Option<LimitMove> {
    let lc = pos.limit_close.as_ref()?;
    if squaring_off(pos.expiry, now) {
        return None;
    }
    if !in_window(now, trading_day) {
        return Some(LimitMove::Drop);
    }
    let p = premium.filter(|p| *p >= lc.price)?;
    Some(LimitMove::Fill(if lc.marketable { p } else { lc.price }))
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
        // A resting limit sell first: what it sells, the levels below see
        // gone on the next pass.
        match limit_move(&pos, now, trading_day, premium) {
            Some(LimitMove::Drop) => {
                let _ = desk.cancel_limit(user_id, &pos.id, "lapsed at the close (a limit sell lasts the day)").await;
                continue;
            }
            Some(LimitMove::Fill(price)) => {
                match desk.fill_limit(user_id, &pos.id, price).await {
                    Ok(f) => tracing::info!(user_id, "Paper limit sell: sold {} {} at {:.2}", f.qty, f.contract, f.price),
                    Err(r) if r.status == StatusCode::NOT_FOUND => {}
                    Err(r) => tracing::warn!(user_id, "Paper limit sell of {} not filled: {}", pos.label(), r.message),
                }
                continue;
            }
            None => {}
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

/// What the watch loop does with an armed line order.
#[derive(Debug, Clone, PartialEq)]
pub enum LineMove {
    /// Drop it unbought, for this reason.
    Drop(String),
    /// The index crossed its line but the option has no live price: mark it
    /// crossed, and buy at the first live price within 15 s.
    Crossed,
    /// Buy it now at this price.
    Fill(f64),
}

/// What happens to `order` at `now` (`now_ts` the same, as an IST
/// timestamp), with its index at `index` and its option at `premium` —
/// fresh prices only. The engine's rules for the owner's line orders: it is
/// dropped once its validity runs out, past the 15:39 cutoff, on a later
/// day, on its contract's square-off, or when the index jumps past the line
/// and its stop or a target at once; it buys at market when the index
/// reaches the line, and once crossed is dropped if it can't be bought
/// within 15 s.
pub fn decide_line(order: &PaperLineOrder, now: DateTime<FixedOffset>, now_ts: &str, index: Option<f64>, premium: Option<f64>) -> Option<LineMove> {
    let line = &order.line;
    let armed_today = order.armed_at.get(..10) == Some(&now_ts[..10.min(now_ts.len())]);
    if !armed_today {
        return Some(LineMove::Drop("it was armed on an earlier day".to_string()));
    }
    if trading_engine::is_entry_cutoff_passed_at(now) {
        return Some(LineMove::Drop("no new buys after the 15:39 cutoff".to_string()));
    }
    if squaring_off(order.expiry, now) {
        return Some(LineMove::Drop("its contract expires today and is being squared off".to_string()));
    }
    if let Some(at) = line.triggered_at.as_deref() {
        if let Some(p) = premium {
            return Some(LineMove::Fill(p));
        }
        let parse = |s: &str| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").ok();
        let late = match (parse(at), parse(now_ts)) {
            (Some(at), Some(now)) => (now - at).num_seconds() > shared_domain::MANUAL_ORDER_TTL_SECS,
            _ => true,
        };
        return late.then(|| LineMove::Drop(format!("no live price within {} s of the crossing", shared_domain::MANUAL_ORDER_TTL_SECS)));
    }
    if line.expired(now_ts) {
        let until = line.valid_until.as_deref().and_then(|t| t.get(11..16)).unwrap_or("?");
        return Some(LineMove::Drop(format!("its line wasn't reached by {until}")));
    }
    if !crate::execute::manual_buy_window_open_at(now) {
        return None;
    }
    let index = index?;
    if !line.reached(index) {
        return None;
    }
    if let Some(what) = shared_domain::line_levels_reached(Some(&order.option_type), &order.chart, index) {
        return Some(LineMove::Drop(format!("{} jumped to {index:.2}, past the line and the {what} at once", order.index)));
    }
    Some(premium.map_or(LineMove::Crossed, LineMove::Fill))
}

/// One pass over every armed line order at `now`.
pub async fn step_lines(desk: &PaperDesk, now: DateTime<FixedOffset>, fresh: impl Fn(&str) -> Option<f64>) {
    let orders = desk.all_lines().await;
    if orders.is_empty() {
        return;
    }
    let now_ts = now.format("%Y-%m-%d %H:%M:%S").to_string();
    for (user_id, order) in orders {
        let index = order.index_key().and_then(|key| fresh(&key));
        let premium = fresh(&order.ws_key);
        match decide_line(&order, now, &now_ts, index, premium) {
            None => {}
            Some(LineMove::Drop(why)) => desk.drop_line(user_id, &order.id, &why).await,
            Some(LineMove::Crossed) => desk.line_crossed(user_id, &order.id, &now_ts).await,
            Some(LineMove::Fill(price)) => match desk.fill_line(user_id, &order.id, price).await {
                Ok(f) => tracing::info!(user_id, "Paper buy line crossed: bought {} {} at {:.2}", f.qty, f.contract, f.price),
                Err(r) if r.status == StatusCode::NOT_FOUND => {}
                Err(r) => tracing::warn!(user_id, "Paper buy line on {} crossed but not bought: {}", order.label(), r.message),
            },
        }
    }
}

/// Whether the watch loop looks at armed buy lines on `now`'s day: a trading
/// day, or any day with a special session, weekends and holidays included —
/// the line route arms a line in one (`manual_buy_window_open` takes a
/// special session), and left unwatched it would neither buy nor drop until
/// the next trading day. `decide_line` keeps to the session's hours.
fn lines_watched(now: DateTime<FixedOffset>, trading_day: bool) -> bool {
    trading_day || shared_domain::special_session_on(now.date_naive()).is_some()
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
        if lines_watched(now, trading_day) {
            step_lines(&desk, now, fresh).await;
        }
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
            limit_close: None,
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
            limit_close: None,
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

    // ── Line orders ───────────────────────────────────────────────────── //

    /// A 2-lot NIFTY 25000 CE line order armed at 11:00 on 18 Nov 2026 (a
    /// Wednesday; the contract expires on the 24th): buys when NIFTY falls
    /// to 25,980, stop 25,950, a target at 26,050.
    fn armed() -> PaperLineOrder {
        let c = nifty(25_000.0, "CE");
        let index = |price| ChartLevel { price, trigger: LevelTrigger::Index };
        PaperLineOrder {
            id: "l".into(),
            index: c.index,
            trading_symbol: c.trading_symbol,
            ws_key: c.ws_key,
            expiry: NaiveDate::from_ymd_opt(2026, 11, 24).unwrap(),
            strike: c.strike,
            option_type: c.option_type,
            lot_size: 75,
            segment: c.segment,
            lots: 2,
            line: shared_domain::EntryLine { price: 25_980.0, rising: false, valid_until: Some("2026-11-18 11:30:00".into()), triggered_at: None },
            chart: ChartLevels { stop: Some(index(25_950.0)), targets: vec![ChartTarget { id: "a".into(), price: 26_050.0, trigger: LevelTrigger::Index, lots: Some(2) }] },
            armed_at: "2026-11-18 11:00:00".into(),
        }
    }

    fn at(h: u32, m: u32, s: u32) -> (DateTime<FixedOffset>, String) {
        let now = ist((2026, 11, 18), (h, m, s));
        (now, now.format("%Y-%m-%d %H:%M:%S").to_string())
    }

    #[test]
    fn a_line_order_fills_when_its_index_reaches_the_line_and_not_before() {
        let order = armed();
        let (now, ts) = at(11, 10, 0);
        assert_eq!(decide_line(&order, now, &ts, Some(26_000.0), Some(120.0)), None);
        assert_eq!(decide_line(&order, now, &ts, None, Some(120.0)), None);
        assert_eq!(decide_line(&order, now, &ts, Some(25_975.0), Some(120.0)), Some(LineMove::Fill(120.0)));
        // Crossed with no live price: marked, then bought within 15 s or dropped.
        assert_eq!(decide_line(&order, now, &ts, Some(25_975.0), None), Some(LineMove::Crossed));
        let mut crossed = order.clone();
        crossed.line.triggered_at = Some(ts.clone());
        let (soon, soon_ts) = at(11, 10, 10);
        assert_eq!(decide_line(&crossed, soon, &soon_ts, Some(26_100.0), None), None);
        assert_eq!(decide_line(&crossed, soon, &soon_ts, Some(26_100.0), Some(118.0)), Some(LineMove::Fill(118.0)));
        let (late, late_ts) = at(11, 10, 20);
        assert!(matches!(decide_line(&crossed, late, &late_ts, Some(26_100.0), None), Some(LineMove::Drop(_))));
    }

    #[test]
    fn a_line_order_is_dropped_when_it_runs_out_jumps_a_level_or_outlives_the_day() {
        let order = armed();
        // Its validity.
        let (now, ts) = at(11, 30, 0);
        assert!(matches!(decide_line(&order, now, &ts, Some(26_000.0), Some(120.0)), Some(LineMove::Drop(_))));
        // Past the line and the stop at once.
        let (now, ts) = at(11, 10, 0);
        match decide_line(&order, now, &ts, Some(25_940.0), Some(90.0)) {
            Some(LineMove::Drop(why)) => assert!(why.contains("stop"), "{why}"),
            other => panic!("expected the gap to drop it, got {other:?}"),
        }
        // Armed on an earlier day, or past the cutoff.
        let mut yesterday = order.clone();
        yesterday.armed_at = "2026-11-17 15:00:00".into();
        yesterday.line.valid_until = None;
        assert!(matches!(decide_line(&yesterday, now, &ts, Some(26_000.0), None), Some(LineMove::Drop(_))));
        let mut till_close = order.clone();
        till_close.line.valid_until = None;
        let (closing, closing_ts) = at(15, 39, 0);
        assert!(matches!(decide_line(&till_close, closing, &closing_ts, Some(26_000.0), None), Some(LineMove::Drop(_))));
        // Before the open nothing is bought, but nothing is dropped either.
        let mut early = till_close.clone();
        early.armed_at = "2026-11-18 09:00:00".into();
        let (pre, pre_ts) = at(9, 10, 0);
        assert_eq!(decide_line(&early, pre, &pre_ts, Some(25_970.0), Some(120.0)), None);
    }

    #[test]
    fn line_orders_are_watched_in_a_special_session_on_a_weekend() {
        let _guard = shared_domain::special_sessions_test_guard();
        let sunday = |h, m, s| {
            let now = ist((2026, 11, 8), (h, m, s));
            (now, now.format("%Y-%m-%d %H:%M:%S").to_string())
        };
        let mut order = armed();
        order.armed_at = "2026-11-08 14:25:00".into();
        order.line.valid_until = None;
        let (during, during_ts) = sunday(14, 30, 0);
        // No session that Sunday: not a day the lines are looked at.
        shared_domain::set_special_sessions(vec![]);
        assert!(!lines_watched(during, false));
        assert!(lines_watched(during, true));

        shared_domain::set_special_sessions(vec![shared_domain::SpecialSession {
            date: NaiveDate::from_ymd_opt(2026, 11, 8).unwrap(),
            start: 14 * 60 + 20,
            end: 15 * 60 + 30,
        }]);
        assert!(lines_watched(during, false));
        // Inside the session the line buys when its index reaches it ...
        assert_eq!(decide_line(&order, during, &during_ts, Some(26_000.0), Some(120.0)), None);
        assert_eq!(decide_line(&order, during, &during_ts, Some(25_975.0), Some(120.0)), Some(LineMove::Fill(120.0)));
        // ... and is dropped at the session's end, not left armed.
        let (end, end_ts) = sunday(15, 30, 0);
        assert!(matches!(decide_line(&order, end, &end_ts, Some(26_000.0), Some(120.0)), Some(LineMove::Drop(_))));
        shared_domain::set_special_sessions(vec![]);
    }

    #[tokio::test]
    async fn the_watch_fills_a_crossed_line_with_its_levels() {
        let (_db, desk) = desk("line-step").await;
        let order = armed();
        // Armed now (`arm_line` stamps the clock), on a contract a month out.
        let today = shared_domain::now_ist();
        let mut contract = nifty(25_000.0, "CE");
        contract.expiry = today.date_naive() + chrono::Duration::days(30);
        let mut line = order.line.clone();
        line.valid_until = None;
        desk.arm_line(1, &contract, 2, line, order.chart.clone()).await.unwrap();
        let now = today.with_time(chrono::NaiveTime::from_hms_opt(11, 10, 0).unwrap()).unwrap();
        // A weekend or a holiday has no session to buy in.
        if !crate::execute::manual_buy_window_open_at(now) {
            return;
        }
        let ws_key = contract.ws_key.clone();
        let quiet = |key: &str| (key == ws_key).then_some(121.0);
        step_lines(&desk, now, quiet).await;
        assert_eq!(desk.lines(1).await.len(), 1, "no index value: still armed");
        let crossed = |key: &str| match key {
            "idx|NIFTY 50" => Some(25_975.0),
            k if k == ws_key => Some(121.0),
            _ => None,
        };
        step_lines(&desk, now, crossed).await;
        let open = desk.open(1).await;
        assert_eq!(open.len(), 1);
        assert_eq!((open[0].qty, open[0].avg_price), (150, 121.0));
        assert_eq!(open[0].chart, order.chart);
        assert!(desk.lines(1).await.is_empty());
    }

    #[test]
    fn a_limit_sell_fills_once_reached_and_lapses_at_the_close() {
        let mut p = held("CE", ChartLevels::default());
        p.expiry = NaiveDate::from_ymd_opt(2026, 11, 24).unwrap();
        p.limit_close = Some(shared_domain::LimitClose::new(75, 120.0, "2026-11-18 10:00:00".into(), false));
        let day = (2026, 11, 18);
        assert_eq!(limit_move(&p, ist(day, (10, 5, 0)), true, Some(119.95)), None);
        assert_eq!(limit_move(&p, ist(day, (10, 5, 0)), true, None), None);
        assert_eq!(limit_move(&p, ist(day, (10, 5, 0)), true, Some(121.0)), Some(LimitMove::Fill(120.0)));
        // One the price had already reached when it was placed fills at the price.
        p.limit_close.as_mut().unwrap().marketable = true;
        assert_eq!(limit_move(&p, ist(day, (10, 5, 0)), true, Some(121.0)), Some(LimitMove::Fill(121.0)));
        // Past the session, or a holiday: it goes.
        assert_eq!(limit_move(&p, ist(day, (15, 31, 0)), true, Some(121.0)), Some(LimitMove::Drop));
        assert_eq!(limit_move(&p, ist(day, (10, 5, 0)), false, Some(121.0)), Some(LimitMove::Drop));
        // On its expiry day's square-off it is the square-off's to cancel.
        p.expiry = NaiveDate::from_ymd_opt(2026, 11, 18).unwrap();
        assert_eq!(limit_move(&p, ist(day, (15, 38, 0)), true, Some(121.0)), None);
        // None rests: nothing.
        p.limit_close = None;
        assert_eq!(limit_move(&p, ist(day, (10, 5, 0)), true, Some(121.0)), None);
    }
}
