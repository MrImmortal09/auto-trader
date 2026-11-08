//! The arithmetic behind the feed-latency panel: matching one instrument's
//! price changes across the two feeds, and the last minute's and the day's
//! figures for each measurement.

use std::collections::VecDeque;

use serde::Serialize;

/// The live figure covers this much.
pub const WINDOW_MS: i64 = 60_000;
/// A Kotak and an Upstox change to the same price this far apart are not
/// taken for the same trade.
const MATCH_WINDOW_MS: i64 = 3_000;
/// The day's samples are counted in buckets this wide between these bounds;
/// anything outside lands in the end buckets.
const BUCKET_MS: f64 = 5.0;
const MIN_MS: f64 = -5_000.0;
const MAX_MS: f64 = 30_000.0;

/// What one price event measured, in ms.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Sample {
    /// Upstox's tick against the exchange's time of that trade.
    UpstoxDelay(f64),
    /// Kotak's tick against the exchange's time of the same trade (from
    /// Upstox: Kotak's own trade time is whole seconds).
    KotakDelay(f64),
    /// How much sooner Kotak had the price than Upstox; negative when Upstox
    /// was first.
    KotakLead(f64),
}

/// Prices from the two feeds are the same number when they agree to a
/// thousandth of a rupee.
fn same_price(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-3
}

#[derive(Debug, Clone, Copy)]
struct UpstoxChange {
    ltp: f64,
    ltt_ms: i64,
    recv_ms: i64,
}

#[derive(Debug, Clone, Copy)]
struct KotakChange {
    ltp: f64,
    recv_ms: i64,
}

/// One instrument's recent price changes on each feed, paired up as the
/// other feed reports the same price.
///
/// Kotak sends a price when it changes (about once a second for an option),
/// so only changes are paired: a Kotak price is the first trade at it, which
/// is Upstox's first trade at it after a different price. Older unpaired
/// changes are dropped once a newer one pairs — the other feed skipped them.
///
/// Each feed's first tick for an instrument is a snapshot of its last trade,
/// however old, so it only sets where prices start from.
#[derive(Debug, Default)]
pub struct Matcher {
    last_upstox_ltt: i64,
    last_upstox_ltp: Option<f64>,
    last_kotak_ltp: Option<f64>,
    upstox: VecDeque<UpstoxChange>,
    kotak: VecDeque<KotakChange>,
}

/// A delay outside these bounds is a stale snapshot (a reconnect) or a clock
/// problem, not how late the feed is; a stalled feed shows as quiet instead.
const PLAUSIBLE_MS: std::ops::RangeInclusive<f64> = -5_000.0..=30_000.0;

fn keep(samples: Vec<Sample>) -> Vec<Sample> {
    samples
        .into_iter()
        .filter(|s| match s {
            Sample::UpstoxDelay(v) | Sample::KotakDelay(v) => PLAUSIBLE_MS.contains(v),
            Sample::KotakLead(_) => true,
        })
        .collect()
}

impl Matcher {
    /// An Upstox tick, received at `recv_ms`, of the trade at `ltt_ms`.
    pub fn upstox(&mut self, ltp: f64, ltt_ms: i64, recv_ms: i64) -> Vec<Sample> {
        keep(self.upstox_samples(ltp, ltt_ms, recv_ms))
    }

    fn upstox_samples(&mut self, ltp: f64, ltt_ms: i64, recv_ms: i64) -> Vec<Sample> {
        // A repeat of the last trade (a quote update) is no new measurement.
        if ltt_ms <= 0 || ltt_ms == self.last_upstox_ltt {
            return Vec::new();
        }
        let first = self.last_upstox_ltp.is_none();
        self.last_upstox_ltt = ltt_ms;
        let changed = !self.last_upstox_ltp.is_some_and(|p| same_price(p, ltp));
        self.last_upstox_ltp = Some(ltp);
        if first {
            return Vec::new();
        }
        let mut out = vec![Sample::UpstoxDelay((recv_ms - ltt_ms) as f64)];
        if !changed {
            return out;
        }
        self.prune(recv_ms);
        if let Some(i) = self.kotak.iter().rposition(|k| same_price(k.ltp, ltp)) {
            let k = self.kotak[i];
            self.kotak.drain(..=i);
            out.push(Sample::KotakDelay((k.recv_ms - ltt_ms) as f64));
            out.push(Sample::KotakLead((recv_ms - k.recv_ms) as f64));
        } else {
            self.upstox.push_back(UpstoxChange { ltp, ltt_ms, recv_ms });
        }
        out
    }

    /// The Upstox stream dropped: its first tick after reconnecting is a
    /// snapshot of the last trade again, so it only sets where prices start.
    pub fn reset_upstox(&mut self) {
        self.last_upstox_ltp = None;
        self.upstox.clear();
    }

    /// A Kotak tick received at `recv_ms`.
    pub fn kotak(&mut self, ltp: f64, recv_ms: i64) -> Vec<Sample> {
        keep(self.kotak_samples(ltp, recv_ms))
    }

    fn kotak_samples(&mut self, ltp: f64, recv_ms: i64) -> Vec<Sample> {
        let first = self.last_kotak_ltp.is_none();
        if self.last_kotak_ltp.is_some_and(|p| same_price(p, ltp)) {
            return Vec::new();
        }
        self.last_kotak_ltp = Some(ltp);
        if first {
            return Vec::new();
        }
        self.prune(recv_ms);
        if let Some(i) = self.upstox.iter().rposition(|u| same_price(u.ltp, ltp)) {
            let u = self.upstox[i];
            self.upstox.drain(..=i);
            return vec![Sample::KotakDelay((recv_ms - u.ltt_ms) as f64), Sample::KotakLead((u.recv_ms - recv_ms) as f64)];
        }
        self.kotak.push_back(KotakChange { ltp, recv_ms });
        Vec::new()
    }

    fn prune(&mut self, now_ms: i64) {
        let cutoff = now_ms - MATCH_WINDOW_MS;
        while self.upstox.front().is_some_and(|u| u.recv_ms < cutoff) {
            self.upstox.pop_front();
        }
        while self.kotak.front().is_some_and(|k| k.recv_ms < cutoff) {
            self.kotak.pop_front();
        }
    }
}

/// Count, mean, median and 95th percentile, in ms.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Summary {
    pub n: u64,
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
}

/// One measurement's samples: the last minute's exactly, the day's as a
/// histogram (exact count and mean, percentiles to the bucket).
#[derive(Debug, Clone, Default)]
pub struct Series {
    recent: VecDeque<(i64, f64)>,
    day_n: u64,
    day_sum: f64,
    buckets: Vec<u32>,
}

fn bucket_count() -> usize {
    ((MAX_MS - MIN_MS) / BUCKET_MS).ceil() as usize
}

fn bucket_of(v: f64) -> usize {
    (((v - MIN_MS) / BUCKET_MS).floor().max(0.0) as usize).min(bucket_count() - 1)
}

/// The nearest-rank `p` quantile's 0-based index among `n` samples.
fn rank(p: f64, n: u64) -> u64 {
    ((p * n as f64).ceil() as u64).clamp(1, n) - 1
}

impl Series {
    pub fn add(&mut self, at_ms: i64, v: f64) {
        self.recent.push_back((at_ms, v));
        self.prune(at_ms);
        self.day_n += 1;
        self.day_sum += v;
        if self.buckets.is_empty() {
            self.buckets = vec![0; bucket_count()];
        }
        self.buckets[bucket_of(v)] += 1;
    }

    fn prune(&mut self, now_ms: i64) {
        while self.recent.front().is_some_and(|(at, _)| *at < now_ms - WINDOW_MS) {
            self.recent.pop_front();
        }
    }

    /// The samples of the minute up to `now_ms`.
    pub fn minute(&mut self, now_ms: i64) -> Option<Summary> {
        self.prune(now_ms);
        if self.recent.is_empty() {
            return None;
        }
        let mut values: Vec<f64> = self.recent.iter().map(|(_, v)| *v).collect();
        values.sort_by(f64::total_cmp);
        let n = values.len() as u64;
        Some(Summary {
            n,
            mean: values.iter().sum::<f64>() / n as f64,
            p50: values[rank(0.5, n) as usize],
            p95: values[rank(0.95, n) as usize],
        })
    }

    /// Every sample of the day.
    pub fn day(&self) -> Option<Summary> {
        if self.day_n == 0 {
            return None;
        }
        let quantile = |p: f64| {
            let want = rank(p, self.day_n);
            let mut seen = 0u64;
            for (i, count) in self.buckets.iter().enumerate() {
                seen += u64::from(*count);
                if seen > want {
                    return MIN_MS + (i as f64 + 0.5) * BUCKET_MS;
                }
            }
            MAX_MS
        };
        Some(Summary { n: self.day_n, mean: self.day_sum / self.day_n as f64, p50: quantile(0.5), p95: quantile(0.95) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstox_delay_counts_each_new_trade_once() {
        let mut m = Matcher::default();
        assert!(m.upstox(99.0, 1_000, 900_000).is_empty(), "the first tick is a snapshot, however old");
        assert_eq!(m.upstox(100.0, 10_000, 10_180), [Sample::UpstoxDelay(180.0)]);
        // The same trade again (a quote update) measures nothing.
        assert!(m.upstox(100.0, 10_000, 10_400).is_empty());
        // A new trade at the same price is a delay but no price change.
        assert_eq!(m.upstox(100.0, 10_500, 10_650), [Sample::UpstoxDelay(150.0)]);
        assert!(m.upstox(100.0, 0, 11_000).is_empty(), "no trade time");
        // A reconnect's snapshot of a trade a minute old is no delay.
        assert!(m.upstox(100.5, 20_000, 80_000).is_empty());
    }

    #[test]
    fn after_a_reconnect_the_first_upstox_tick_is_a_snapshot_again() {
        let mut m = Matcher::default();
        m.upstox(100.0, 1_000, 1_100);
        assert_eq!(m.upstox(101.0, 2_000, 2_150), [Sample::UpstoxDelay(150.0)]);
        m.reset_upstox();
        // The resubscribe snapshot: a trade from 20 s ago, new to the matcher.
        assert!(m.upstox(102.0, 3_000, 23_000).is_empty());
        assert_eq!(m.upstox(103.0, 24_000, 24_120), [Sample::UpstoxDelay(120.0)]);
    }

    #[test]
    fn a_price_both_feeds_report_measures_kotak_against_the_trade() {
        let mut m = Matcher::default();
        // Each feed's first tick only sets where prices start.
        assert!(m.upstox(100.0, 19_000, 19_100).is_empty());
        assert!(m.kotak(100.0, 19_050).is_empty());
        // Kotak first: it has 101 at 20_050, Upstox at 20_200 for the trade at 20_000.
        assert!(m.kotak(101.0, 20_050).is_empty());
        assert_eq!(
            m.upstox(101.0, 20_000, 20_200),
            [Sample::UpstoxDelay(200.0), Sample::KotakDelay(50.0), Sample::KotakLead(150.0)],
        );
        // Upstox first: Kotak's 102 comes 120 ms after Upstox's.
        assert_eq!(m.upstox(102.0, 21_000, 21_180), [Sample::UpstoxDelay(180.0)]);
        assert_eq!(m.kotak(102.0, 21_300), [Sample::KotakDelay(300.0), Sample::KotakLead(-120.0)]);
        // Kotak repeating its price measures nothing.
        assert!(m.kotak(102.0, 21_900).is_empty());
    }

    #[test]
    fn changes_the_other_feed_skipped_are_dropped_and_old_ones_expire() {
        let mut m = Matcher::default();
        m.upstox(100.0, 900, 950);
        m.kotak(100.0, 960);
        // Upstox sees 101, 102, 103; Kotak (conflated) only 103.
        m.upstox(101.0, 1_000, 1_100);
        m.upstox(102.0, 1_200, 1_300);
        m.upstox(103.0, 1_400, 1_500);
        assert_eq!(m.kotak(103.0, 1_450), [Sample::KotakDelay(50.0), Sample::KotakLead(50.0)]);
        // 101 and 102 went with it: a later Kotak 101 pairs with nothing old.
        assert!(m.kotak(101.0, 1_600).is_empty());
        // A change older than the window never pairs.
        assert_eq!(m.upstox(101.0, 9_000, 9_100), [Sample::UpstoxDelay(100.0)]);
    }

    #[test]
    fn the_minute_is_exact_and_the_day_is_to_the_bucket() {
        let mut s = Series::default();
        for (i, v) in [100.0, 200.0, 300.0, 400.0].iter().enumerate() {
            s.add(i as i64 * 1_000, *v);
        }
        let minute = s.minute(3_000).unwrap();
        assert_eq!((minute.n, minute.mean, minute.p50, minute.p95), (4, 250.0, 200.0, 400.0));
        // A minute later only the day remembers them.
        assert!(s.minute(70_000).is_none());
        let day = s.day().unwrap();
        assert_eq!((day.n, day.mean), (4, 250.0));
        assert!((day.p50 - 200.0).abs() <= BUCKET_MS && (day.p95 - 400.0).abs() <= BUCKET_MS, "{day:?}");
        // Outliers are counted in the end buckets, not lost.
        s.add(80_000, 99_000.0);
        s.add(80_000, -99_000.0);
        assert_eq!(s.day().unwrap().n, 6);
        assert!(Series::default().day().is_none());
    }
}
