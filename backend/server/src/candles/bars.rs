//! Candle arithmetic for the v2 charts — pure, no I/O.
//!
//! Everything is built from 1-minute bars. Upstox's official minutes are
//! complete (every trade, with volume); the minutes built here from live ticks
//! hold only what this server saw, so wherever Upstox has a minute, it wins.

use std::collections::BTreeMap;

use serde::Serialize;

pub const MINUTE_MS: i64 = 60_000;
const DAY_MS: i64 = 24 * 60 * MINUTE_MS;
const IST_OFFSET_MS: i64 = shared_domain::IST_OFFSET_SECS as i64 * 1000;
/// The session in minutes after midnight IST: 09:15 to 15:30.
const SESSION_OPEN_MIN: i64 = 9 * 60 + 15;
const SESSION_CLOSE_MIN: i64 = 15 * 60 + 30;
/// How long after the close Upstox's last minute is taken as final.
const SETTLE_MS: i64 = 2 * MINUTE_MS;

/// The timeframes the charts use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tf {
    M1,
    M5,
    M15,
}

impl Tf {
    pub fn parse(s: &str) -> Option<Tf> {
        match s.trim().to_ascii_lowercase().as_str() {
            "1m" => Some(Tf::M1),
            "5m" => Some(Tf::M5),
            "15m" => Some(Tf::M15),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Tf::M1 => "1m",
            Tf::M5 => "5m",
            Tf::M15 => "15m",
        }
    }

    pub fn minutes(self) -> u32 {
        match self {
            Tf::M1 => 1,
            Tf::M5 => 5,
            Tf::M15 => 15,
        }
    }

    pub fn ms(self) -> i64 {
        i64::from(self.minutes()) * MINUTE_MS
    }

    /// Start of the bar `t_ms` falls in. IST is UTC+05:30, a whole number of
    /// 15-minute steps, so flooring Unix time lines every bar up with 09:15.
    pub fn bucket(self, t_ms: i64) -> i64 {
        t_ms - t_ms.rem_euclid(self.ms())
    }
}

/// One OHLC bar; `t` is the Unix ms of its start.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Bar {
    pub t: i64,
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
    /// Volume, once every minute in the bar has Upstox's official count.
    pub v: Option<f64>,
}

impl Bar {
    pub fn at(t: i64, price: f64) -> Bar {
        Bar { t, o: price, h: price, l: price, c: price, v: None }
    }

    fn apply(&mut self, price: f64) {
        self.h = self.h.max(price);
        self.l = self.l.min(price);
        self.c = price;
    }
}

fn ist_minute_of_day(t_ms: i64) -> i64 {
    (t_ms + IST_OFFSET_MS).rem_euclid(DAY_MS) / MINUTE_MS
}

/// Whether `t_ms` falls inside the 09:15–15:30 IST session of its day.
pub fn in_session(t_ms: i64) -> bool {
    (SESSION_OPEN_MIN..SESSION_CLOSE_MIN).contains(&ist_minute_of_day(t_ms))
}

/// Unix ms of 00:00 IST on `t_ms`'s IST day.
pub fn ist_day_start(t_ms: i64) -> i64 {
    t_ms - (t_ms + IST_OFFSET_MS).rem_euclid(DAY_MS)
}

/// Unix ms of 15:30 IST, the session's end, on `t_ms`'s IST day.
pub fn session_close(t_ms: i64) -> i64 {
    ist_day_start(t_ms) + SESSION_CLOSE_MIN * MINUTE_MS
}

/// Upstox's intraday minutes (oldest first) that are finished at `now_ms`.
/// During the session the newest is still forming — Upstox fills it with the
/// last price alone — so it is never taken; after the close it is, once two
/// minutes have passed. Minutes outside the session are never bars.
pub fn finished_minutes(mut minutes: Vec<Bar>, now_ms: i64) -> Vec<Bar> {
    minutes.retain(|b| in_session(b.t));
    if let Some(newest) = minutes.last() {
        let settled = !in_session(now_ms) && now_ms >= newest.t + MINUTE_MS + SETTLE_MS;
        if !settled {
            minutes.pop();
        }
    }
    minutes
}

/// Roll 1-minute bars (oldest first, each flagged official or not) up into
/// `tf` bars. A bar gets volume only when every minute in it is official.
fn aggregate(minutes: impl Iterator<Item = (Bar, bool)>, tf: Tf) -> Vec<Bar> {
    let mut out: Vec<Bar> = Vec::new();
    for (m, official) in minutes {
        let start = tf.bucket(m.t);
        let volume = if official { m.v } else { None };
        match out.last_mut() {
            Some(bar) if bar.t == start => {
                bar.h = bar.h.max(m.h);
                bar.l = bar.l.min(m.l);
                bar.c = m.c;
                bar.v = bar.v.zip(volume).map(|(a, b)| a + b);
            }
            _ => out.push(Bar { t: start, o: m.o, h: m.h, l: m.l, c: m.c, v: volume }),
        }
    }
    out
}

/// One instrument's 1-minute bars for one IST day.
#[derive(Debug, Default, Clone)]
pub struct DaySeries {
    /// Finished minutes from Upstox, by start: complete, with volume.
    official: BTreeMap<i64, Bar>,
    /// Minutes built from live ticks, by start; the newest may be forming.
    ticked: BTreeMap<i64, Bar>,
}

impl DaySeries {
    /// Fold in a tick read at `at_ms`. Returns the start of the minute it
    /// went into, or `None` for a tick outside the session or not a price.
    pub fn tick(&mut self, at_ms: i64, price: f64) -> Option<i64> {
        if !(price.is_finite() && price > 0.0) || !in_session(at_ms) {
            return None;
        }
        let minute = Tf::M1.bucket(at_ms);
        self.ticked
            .entry(minute)
            .and_modify(|b| b.apply(price))
            .or_insert_with(|| Bar::at(minute, price));
        Some(minute)
    }

    /// Merge in Upstox's finished minutes (see [`finished_minutes`]). Returns
    /// the ones that are new or changed.
    pub fn add_official(&mut self, minutes: Vec<Bar>) -> Vec<Bar> {
        let mut changed = Vec::new();
        for m in minutes {
            if self.official.insert(m.t, m) != Some(m) {
                changed.push(m);
            }
        }
        changed
    }

    /// Put back a minute saved earlier today.
    pub fn restore(&mut self, minute: Bar, official: bool) {
        let map = if official { &mut self.official } else { &mut self.ticked };
        map.insert(minute.t, minute);
    }

    /// Whether Upstox's official minute starting at `t` is in.
    pub fn has_official(&self, t: i64) -> bool {
        self.official.contains_key(&t)
    }

    /// The ticked minute starting at `t`, if any.
    pub fn ticked_minute(&self, t: i64) -> Option<Bar> {
        self.ticked.get(&t).copied()
    }

    /// Every minute, official where Upstox has it, from ticks otherwise.
    fn minutes(&self, range: std::ops::Range<i64>) -> impl Iterator<Item = (Bar, bool)> + '_ {
        let mut merged: BTreeMap<i64, (Bar, bool)> =
            self.ticked.range(range.clone()).map(|(t, b)| (*t, (*b, false))).collect();
        merged.extend(self.official.range(range).map(|(t, b)| (*t, (*b, true))));
        merged.into_values()
    }

    /// The day's bars at `tf`, oldest first.
    pub fn bars(&self, tf: Tf) -> Vec<Bar> {
        aggregate(self.minutes(i64::MIN..i64::MAX), tf)
    }

    /// The `tf` bar containing `t`, if any minute of it is known.
    pub fn bar_at(&self, tf: Tf, t: i64) -> Option<Bar> {
        let start = tf.bucket(t);
        aggregate(self.minutes(start..start + tf.ms()), tf).pop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unix ms of `h:m:s.ms` IST on Wed 21 Oct 2026.
    fn at(h: i64, m: i64, s: i64, ms: i64) -> i64 {
        let day = chrono::NaiveDate::from_ymd_opt(2026, 10, 21)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(shared_domain::ist_offset())
            .unwrap()
            .timestamp_millis();
        day + ((h * 60 + m) * 60 + s) * 1000 + ms
    }

    fn official(t: i64, o: f64, h: f64, l: f64, c: f64, v: f64) -> Bar {
        Bar { t, o, h, l, c, v: Some(v) }
    }

    #[test]
    fn ticks_build_a_minute_bar() {
        let mut day = DaySeries::default();
        for (s, price) in [(5, 100.0), (30, 105.0), (59, 98.0)] {
            assert_eq!(day.tick(at(9, 15, s, 0), price), Some(at(9, 15, 0, 0)));
        }
        day.tick(at(9, 16, 0, 0), 101.0);
        assert_eq!(day.bars(Tf::M1), vec![
            Bar { t: at(9, 15, 0, 0), o: 100.0, h: 105.0, l: 98.0, c: 98.0, v: None },
            Bar::at(at(9, 16, 0, 0), 101.0),
        ]);
    }

    #[test]
    fn a_tick_on_the_five_minute_boundary_opens_the_next_bar() {
        let mut day = DaySeries::default();
        day.tick(at(9, 19, 59, 999), 100.0);
        day.tick(at(9, 20, 0, 0), 102.0);
        let bars = day.bars(Tf::M5);
        assert_eq!(bars.iter().map(|b| b.t).collect::<Vec<_>>(), [at(9, 15, 0, 0), at(9, 20, 0, 0)]);
        assert_eq!((bars[0].c, bars[1].o), (100.0, 102.0));
        // 15-minute bars line up with 09:15 too.
        assert_eq!(Tf::M15.bucket(at(9, 29, 59, 999)), at(9, 15, 0, 0));
        assert_eq!(Tf::M15.bucket(at(9, 30, 0, 0)), at(9, 30, 0, 0));
        assert_eq!(Tf::M5.bucket(at(15, 29, 0, 0)), at(15, 25, 0, 0));
    }

    #[test]
    fn only_session_ticks_that_are_prices_count() {
        let mut day = DaySeries::default();
        assert_eq!(day.tick(at(9, 14, 59, 999), 100.0), None);
        assert_eq!(day.tick(at(15, 30, 0, 0), 100.0), None);
        assert_eq!(day.tick(at(15, 35, 0, 0), 100.0), None);
        assert_eq!(day.tick(at(10, 0, 0, 0), 0.0), None);
        assert_eq!(day.tick(at(10, 0, 0, 0), f64::NAN), None);
        assert!(day.bars(Tf::M1).is_empty());
        assert_eq!(day.tick(at(9, 15, 0, 0), 100.0), Some(at(9, 15, 0, 0)));
        assert_eq!(day.tick(at(15, 29, 59, 999), 100.0), Some(at(15, 29, 0, 0)));
    }

    #[test]
    fn official_minutes_win_and_volume_waits_for_all_of_them() {
        let mut day = DaySeries::default();
        // Ticks for 09:15–09:19; this server missed 09:16's high.
        for (m, price) in [(15, 100.0), (16, 101.0), (17, 102.0), (18, 103.0), (19, 104.0)] {
            day.tick(at(9, m, 10, 0), price);
        }
        let first_four: Vec<Bar> = (15..19)
            .map(|m| official(at(9, m, 0, 0), 100.0, if m == 16 { 110.0 } else { 104.0 }, 99.0, 103.0, 50.0))
            .collect();
        assert_eq!(day.add_official(first_four.clone()).len(), 4);
        let bar = day.bars(Tf::M5)[0];
        assert_eq!((bar.h, bar.l, bar.c), (110.0, 99.0, 104.0));
        assert_eq!(bar.v, None, "09:19 is still from ticks");

        // Re-sending the same minutes changes nothing; 09:19 completes the bar.
        assert!(day.add_official(first_four).is_empty());
        day.add_official(vec![official(at(9, 19, 0, 0), 103.0, 105.0, 102.0, 104.5, 30.0)]);
        let bar = day.bars(Tf::M5)[0];
        assert_eq!((bar.o, bar.h, bar.c, bar.v), (100.0, 110.0, 104.5, Some(230.0)));
        assert_eq!(day.bar_at(Tf::M5, at(9, 17, 0, 0)), Some(bar));
        assert_eq!(day.bar_at(Tf::M5, at(9, 25, 0, 0)), None);
    }

    #[test]
    fn upstox_newest_minute_is_never_taken_during_the_session() {
        let minutes = |ts: &[i64]| ts.iter().map(|t| official(*t, 1.0, 1.0, 1.0, 1.0, 1.0)).collect::<Vec<_>>();
        let starts = |bars: Vec<Bar>| bars.iter().map(|b| b.t).collect::<Vec<_>>();
        let day = [at(15, 27, 0, 0), at(15, 28, 0, 0), at(15, 29, 0, 0)];

        // At 15:29:30 the 15:29 minute is forming.
        assert_eq!(starts(finished_minutes(minutes(&day), at(15, 29, 30, 0))), &day[..2]);
        // Just after the close it is still not settled; two minutes on, it is.
        assert_eq!(starts(finished_minutes(minutes(&day), at(15, 31, 0, 0))), &day[..2]);
        assert_eq!(starts(finished_minutes(minutes(&day), at(15, 32, 0, 0))), &day[..]);
        // Anything outside the session is not a bar.
        let with_post_close = [at(15, 29, 0, 0), at(15, 30, 0, 0)];
        assert_eq!(starts(finished_minutes(minutes(&with_post_close), at(16, 0, 0, 0))), [at(15, 29, 0, 0)]);
        assert!(finished_minutes(Vec::new(), at(10, 0, 0, 0)).is_empty());
    }

    #[test]
    fn day_helpers_use_the_ist_calendar() {
        assert_eq!(ist_day_start(at(15, 29, 0, 0)), at(0, 0, 0, 0));
        assert_eq!(session_close(at(9, 15, 0, 0)), at(15, 30, 0, 0));
        assert_eq!(Tf::parse("5M"), Some(Tf::M5));
        assert_eq!(Tf::parse("30m"), None);
    }
}
