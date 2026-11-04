//! Kotak's REST quotes, scheduled: which keys go into which call, and when.
//! The scheduler is pure (an `Instant` in, chunks out) so every limit is
//! tested without a network; `run_poller` drives it against the real
//! client twice a second.
//!
//! The limits, and why: Kotak allows 10 requests a second across every API
//! and the engine's order path must never wait behind a quote, so quotes
//! take at most [`CALLS_PER_SEC`] of them, process-wide, and none at all
//! while the Kotak session is unhealthy. How many keys one call may carry
//! is undocumented: a call starts at [`BATCH_MAX`] keys and a refusal halves
//! it; a key refused on its own is left out for a while, since one bad key
//! must not keep shrinking the batch for everyone else. A 429 pauses the
//! poller for a doubling interval, as does a transport error or a 5xx.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kotak_client::{KotakClient, KotakError};
use shared_domain::{kotak_index_feed, INDEX_NAMES};
use tokio::sync::Mutex;
use upstox_client::UpstoxClient;

use super::hub::QuoteHub;

/// Keys per call to start with. Undocumented; a refusal halves it.
pub const BATCH_MAX: usize = 25;
/// Calls a second, process-wide.
pub const CALLS_PER_SEC: u32 = 2;
/// How often the poller wakes.
pub const TICK: Duration = Duration::from_millis(500);
/// A wanted key is refreshed this often.
pub const POLL_EVERY: Duration = Duration::from_secs(2);
/// Keys asked for within this are polled.
pub const WANT_WINDOW: Duration = Duration::from_secs(15);
/// Successful calls at one batch size before it doubles back toward
/// [`BATCH_MAX`].
const GROW_AFTER_OK_CALLS: u32 = 10;
/// A key refused on its own is left out for this long.
const QUARANTINE: Duration = Duration::from_secs(600);
/// The pause after a 429, doubling each time up to the cap; a successful
/// call ends it.
const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(2);
const RATE_LIMIT_PAUSE_MAX: Duration = Duration::from_secs(60);
/// The pause after a transport error or a 5xx, likewise.
const TRANSIENT_PAUSE: Duration = Duration::from_secs(1);
const TRANSIENT_PAUSE_MAX: Duration = Duration::from_secs(30);
/// `last_sent` entries older than this are forgotten, so the map stays the
/// size of what is being polled.
const FORGET_SENT_AFTER: Duration = Duration::from_secs(600);

/// The key the quotes API takes for a price-map key: an index by its
/// segment and exact name (`idx|NIFTY 50` → `nse_cm|Nifty 50`), a contract
/// or an equity by its `segment|token` as it is. `None` for a key the API
/// has no form for: an index close, an index the app has no feed name for,
/// anything else.
pub fn rest_key(ws_key: &str) -> Option<String> {
    if let Some(name) = ws_key.strip_prefix("idx|") {
        return INDEX_NAMES
            .iter()
            .filter_map(|index| kotak_index_feed(index))
            .find(|(_, feed)| feed.eq_ignore_ascii_case(name.trim()))
            .map(|(segment, feed)| format!("{segment}|{feed}"));
    }
    let (segment, token) = ws_key.split_once('|')?;
    if token.trim().is_empty() || token.contains(',') {
        return None;
    }
    matches!(segment, "nse_cm" | "bse_cm" | "nse_fo" | "bse_fo" | "cde_fo").then(|| ws_key.to_string())
}

/// What a call came to, as the scheduler needs to know it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    /// HTTP 429.
    RateLimited,
    /// A transport error, a timeout, a 5xx, a session gone: try again later.
    Transient,
    /// Kotak refused the call as made: a 4xx or a `Not_Ok` body. The batch
    /// may be too big, or a key invalid.
    Refused,
}

/// How a failed call is scheduled.
pub fn classify(err: &KotakError) -> Outcome {
    match err {
        KotakError::ApiError { status_code: 429, .. } => Outcome::RateLimited,
        KotakError::ApiError { status_code, .. } if (500..=599).contains(status_code) => Outcome::Transient,
        // The dashboard access token refused: nothing a smaller batch fixes.
        KotakError::ApiError { status_code: 401 | 403, .. } => Outcome::Transient,
        KotakError::ApiError { .. } | KotakError::Json(_) => Outcome::Refused,
        _ => Outcome::Transient,
    }
}

/// At most `per_second` calls in any one-second window, process-wide.
pub struct Bucket {
    per_second: u32,
    window: Instant,
    used: u32,
}

impl Bucket {
    pub fn new(per_second: u32, now: Instant) -> Self {
        Bucket { per_second, window: now, used: 0 }
    }

    pub fn try_take(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.window) >= Duration::from_secs(1) {
            self.window = now;
            self.used = 0;
        }
        if self.used < self.per_second {
            self.used += 1;
            true
        } else {
            false
        }
    }
}

/// A pause that doubles on every hit up to a cap and ends on a success.
pub struct Backoff {
    first: Duration,
    max: Duration,
    next: Duration,
    until: Option<Instant>,
    /// Hits since the last success.
    pub hits: u32,
}

impl Backoff {
    pub fn new(first: Duration, max: Duration) -> Self {
        Backoff { first, max, next: first, until: None, hits: 0 }
    }

    /// Pauses from `now`; returns how long for.
    pub fn hit(&mut self, now: Instant) -> Duration {
        let pause = self.next;
        self.until = Some(now + pause);
        self.next = (self.next * 2).min(self.max);
        self.hits += 1;
        pause
    }

    pub fn clear(&mut self) {
        self.until = None;
        self.next = self.first;
        self.hits = 0;
    }

    pub fn active(&self, now: Instant) -> bool {
        self.until.is_some_and(|until| now < until)
    }
}

/// The batch size and the keys left out.
struct Batcher {
    size: usize,
    ok_streak: u32,
    quarantined: HashMap<String, Instant>,
}

impl Batcher {
    fn new() -> Self {
        Batcher { size: BATCH_MAX, ok_streak: 0, quarantined: HashMap::new() }
    }

    fn succeeded(&mut self) {
        self.ok_streak += 1;
        if self.ok_streak >= GROW_AFTER_OK_CALLS && self.size < BATCH_MAX {
            self.size = (self.size * 2).min(BATCH_MAX);
            self.ok_streak = 0;
        }
    }

    /// A chunk of several keys halves the batch; one key alone is
    /// quarantined, and returned.
    fn refused(&mut self, keys: &[String], now: Instant) -> Option<String> {
        self.ok_streak = 0;
        if keys.len() > 1 {
            self.size = (keys.len() / 2).clamp(1, self.size);
            return None;
        }
        let key = keys.first()?.clone();
        self.quarantined.insert(key.clone(), now + QUARANTINE);
        Some(key)
    }

    fn is_quarantined(&self, key: &str, now: Instant) -> bool {
        self.quarantined.get(key).is_some_and(|until| now < *until)
    }

    fn prune(&mut self, now: Instant) {
        self.quarantined.retain(|_, until| now < *until);
    }
}

/// One call's keys (price-map keys), and whether the close sweep sent them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub keys: Vec<String>,
    pub sweep: bool,
}

/// Which keys go into which call, under every limit above.
pub struct Scheduler {
    bucket: Bucket,
    rate_limit: Backoff,
    transient: Backoff,
    batch: Batcher,
    last_sent: HashMap<String, Instant>,
    last_forgotten: Instant,
}

impl Scheduler {
    pub fn new(now: Instant) -> Self {
        Scheduler {
            bucket: Bucket::new(CALLS_PER_SEC, now),
            rate_limit: Backoff::new(RATE_LIMIT_PAUSE, RATE_LIMIT_PAUSE_MAX),
            transient: Backoff::new(TRANSIENT_PAUSE, TRANSIENT_PAUSE_MAX),
            batch: Batcher::new(),
            last_sent: HashMap::new(),
            last_forgotten: now,
        }
    }

    pub fn batch_size(&self) -> usize {
        self.batch.size
    }

    pub fn quarantined(&self) -> usize {
        self.batch.quarantined.len()
    }

    /// Why nothing is sent at `now`, if a pause is on.
    pub fn paused(&self, now: Instant) -> Option<&'static str> {
        if self.rate_limit.active(now) {
            Some("rate limited by Kotak")
        } else if self.transient.active(now) {
            Some("backing off after an error")
        } else {
            None
        }
    }

    /// The calls to make now. `wanted` is the live keys in the caller's
    /// priority order; those sent within [`POLL_EVERY`] wait. `sweep` is
    /// the close sweep's queue, taken from the front; when both have keys,
    /// calls alternate between them, so a sweep of thousands of keys
    /// neither starves the screens nor waits behind them. Nothing is sent
    /// while the session is down or a pause is on.
    pub fn plan(&mut self, wanted: &[String], sweep: &mut VecDeque<String>, session_ok: bool, now: Instant) -> Vec<Chunk> {
        if !session_ok || self.paused(now).is_some() {
            return Vec::new();
        }
        self.batch.prune(now);
        if now.saturating_duration_since(self.last_forgotten) >= FORGET_SENT_AFTER {
            self.last_forgotten = now;
            self.last_sent.retain(|_, at| now.saturating_duration_since(*at) < FORGET_SENT_AFTER);
        }
        let mut due: Vec<&String> = wanted
            .iter()
            .filter(|k| !self.batch.is_quarantined(k, now))
            .filter(|k| self.last_sent.get(*k).is_none_or(|at| now.saturating_duration_since(*at) >= POLL_EVERY))
            .collect();
        // Least recently sent first; the caller's order breaks ties.
        due.sort_by_key(|k| self.last_sent.get(*k).copied());
        let mut due: VecDeque<&String> = due.into();

        let mut out = Vec::new();
        let mut live_turn = true;
        loop {
            let live = !due.is_empty();
            let swept = !sweep.is_empty();
            if !live && !swept {
                break;
            }
            let from_sweep = if live && swept { !live_turn } else { swept };
            let size = self.batch.size;
            let mut keys: Vec<String> = Vec::with_capacity(size);
            if from_sweep {
                while keys.len() < size {
                    match sweep.pop_front() {
                        Some(k) if self.batch.is_quarantined(&k, now) => continue,
                        Some(k) => keys.push(k),
                        None => break,
                    }
                }
            } else {
                while keys.len() < size {
                    match due.pop_front() {
                        Some(k) => keys.push(k.clone()),
                        None => break,
                    }
                }
            }
            if keys.is_empty() {
                continue;
            }
            if !self.bucket.try_take(now) {
                if from_sweep {
                    for k in keys.into_iter().rev() {
                        sweep.push_front(k);
                    }
                }
                break;
            }
            for k in &keys {
                self.last_sent.insert(k.clone(), now);
            }
            out.push(Chunk { keys, sweep: from_sweep });
            live_turn = !live_turn;
        }
        out
    }

    /// What `chunk`'s call came to. A call that did not answer puts its
    /// keys back: a sweep's to the front of its queue, live keys as due
    /// again. Returns the key quarantined, if the refusal was one key's.
    pub fn settle(&mut self, chunk: &Chunk, outcome: Outcome, sweep: &mut VecDeque<String>, now: Instant) -> Option<String> {
        match outcome {
            Outcome::Ok => {
                self.batch.succeeded();
                self.rate_limit.clear();
                self.transient.clear();
                None
            }
            Outcome::RateLimited => {
                self.rate_limit.hit(now);
                self.retry(chunk, sweep);
                None
            }
            Outcome::Transient => {
                self.transient.hit(now);
                self.retry(chunk, sweep);
                None
            }
            Outcome::Refused => {
                let quarantined = self.batch.refused(&chunk.keys, now);
                if quarantined.is_none() {
                    self.retry(chunk, sweep);
                }
                quarantined
            }
        }
    }

    fn retry(&mut self, chunk: &Chunk, sweep: &mut VecDeque<String>) {
        if chunk.sweep {
            for k in chunk.keys.iter().rev() {
                sweep.push_front(k.clone());
            }
        } else {
            for k in &chunk.keys {
                self.last_sent.remove(k);
            }
        }
    }
}

/// Runs forever: every [`TICK`], the calls the hub's scheduler allows,
/// against a clone of the Kotak client (the mutex is never held across a
/// call). While there is no healthy session, nothing is asked of Kotak;
/// Upstox's quotes stand in then, when it is configured.
pub async fn run_poller(
    hub: Arc<QuoteHub>,
    kotak: Arc<Mutex<Option<KotakClient>>>,
    healthy: Arc<AtomicBool>,
    upstox: Option<UpstoxClient>,
) {
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let client = kotak.lock().await.clone();
        let session_ok = client.is_some() && healthy.load(Ordering::Relaxed);
        let chunks = hub.plan(session_ok, Instant::now());
        match client.filter(|_| session_ok) {
            Some(client) => {
                for chunk in chunks {
                    let keys = QuoteHub::rest_keys(&chunk);
                    let asked: Vec<String> = keys.iter().map(|(rest, _)| rest.clone()).collect();
                    let result = client.quotes(&asked, "all").await;
                    hub.settle_kotak(chunk, &keys, result, Instant::now());
                }
            }
            None => {
                if let Some(upstox) = &upstox {
                    hub.poll_upstox(upstox).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(prefix: &str, n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("{prefix}|{i}")).collect()
    }

    fn secs(n: f64) -> Duration {
        Duration::from_secs_f64(n)
    }

    #[test]
    fn rest_keys_name_an_index_by_its_segment_and_exact_name() {
        assert_eq!(rest_key("idx|NIFTY 50").as_deref(), Some("nse_cm|Nifty 50"));
        assert_eq!(rest_key("idx|SENSEX").as_deref(), Some("bse_cm|SENSEX"));
        assert_eq!(rest_key("idx|NIFTY MID SELECT").as_deref(), Some("nse_cm|NIFTY MID SELECT"));
        assert_eq!(rest_key("nse_fo|51386").as_deref(), Some("nse_fo|51386"));
        assert_eq!(rest_key("nse_cm|2885").as_deref(), Some("nse_cm|2885"));
        // No form: an index close, an index with no feed name, junk.
        assert_eq!(rest_key("idx_close|NIFTY 50"), None);
        assert_eq!(rest_key("idx|NIFTY NEXT 50"), None);
        assert_eq!(rest_key("mcx_fo|1"), None);
        assert_eq!(rest_key("nse_fo|"), None);
        assert_eq!(rest_key("nse_fo|1,2"), None);
        assert_eq!(rest_key("RELIANCE"), None);
    }

    #[test]
    fn the_bucket_allows_two_calls_a_second() {
        let t0 = Instant::now();
        let mut bucket = Bucket::new(2, t0);
        assert!(bucket.try_take(t0));
        assert!(bucket.try_take(t0));
        assert!(!bucket.try_take(t0));
        assert!(!bucket.try_take(t0 + secs(0.999)));
        assert!(bucket.try_take(t0 + secs(1.0)));
        assert!(bucket.try_take(t0 + secs(1.5)));
        assert!(!bucket.try_take(t0 + secs(1.9)));
        assert!(bucket.try_take(t0 + secs(2.0)));
    }

    #[test]
    fn calls_carry_25_keys_and_pause_entirely_while_the_session_is_down() {
        let t0 = Instant::now();
        let mut s = Scheduler::new(t0);
        let wanted = keys("nse_cm", 60);
        let mut sweep = VecDeque::new();
        // Unhealthy: nothing, however much is wanted.
        assert!(s.plan(&wanted, &mut sweep, false, t0).is_empty());
        // Healthy: two calls of 25 (the second's limit), the rest waits.
        let chunks = s.plan(&wanted, &mut sweep, true, t0);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].keys, &wanted[..25]);
        assert_eq!(chunks[1].keys, &wanted[25..50]);
        assert!(!chunks[0].sweep);
        // The same second: the bucket is spent.
        assert!(s.plan(&wanted, &mut sweep, true, t0 + secs(0.5)).is_empty());
        // The next second: the ten not yet sent; the fifty wait their 2 s.
        let chunks = s.plan(&wanted, &mut sweep, true, t0 + secs(1.0));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].keys, &wanted[50..]);
        // At 2 s the first fifty are due again, least recently sent first.
        let chunks = s.plan(&wanted, &mut sweep, true, t0 + secs(2.0));
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].keys, &wanted[..25]);
        assert_eq!(chunks[1].keys, &wanted[25..50]);
        // Nothing wanted: nothing sent.
        assert!(s.plan(&[], &mut sweep, true, t0 + secs(10.0)).is_empty());
    }

    #[test]
    fn a_refused_call_halves_the_batch_and_a_refused_key_is_quarantined() {
        let t0 = Instant::now();
        let mut s = Scheduler::new(t0);
        let wanted = keys("nse_fo", 25);
        let mut sweep = VecDeque::new();
        let chunk = s.plan(&wanted, &mut sweep, true, t0).remove(0);
        assert_eq!(chunk.keys.len(), 25);
        assert_eq!(s.settle(&chunk, Outcome::Refused, &mut sweep, t0), None);
        assert_eq!(s.batch_size(), 12);
        // Its keys are due at once, in calls of 12.
        let chunks = s.plan(&wanted, &mut sweep, true, t0 + secs(1.0));
        assert_eq!(chunks.iter().map(|c| c.keys.len()).collect::<Vec<_>>(), vec![12, 12]);
        // Down to one key a call, then the one key refused is left out.
        // (Keys sent a second ago are not due yet, so two seconds a step;
        // a refused chunk's keys are due at once and go first.)
        let mut now = t0 + secs(3.0);
        let mut bad = None;
        for expect in [6, 3, 1, 1] {
            let chunk = s.plan(&wanted, &mut sweep, true, now).remove(0);
            let quarantined = s.settle(&chunk, Outcome::Refused, &mut sweep, now);
            assert_eq!(s.batch_size(), expect);
            assert_eq!(quarantined.is_some(), chunk.keys.len() == 1);
            bad = bad.or(quarantined);
            now += secs(2.0);
        }
        assert_eq!(s.quarantined(), 1);
        let bad = bad.unwrap();
        assert_eq!(bad, wanted[0]);
        let chunk = s.plan(&wanted, &mut sweep, true, now).remove(0);
        assert!(!chunk.keys.contains(&bad));
        // Ten good calls at a size double it, back up to 25.
        let mut size = 1;
        while size < BATCH_MAX {
            for _ in 0..GROW_AFTER_OK_CALLS {
                let chunk = Chunk { keys: vec!["nse_fo|2".into()], sweep: false };
                s.settle(&chunk, Outcome::Ok, &mut sweep, now);
            }
            size = (size * 2).min(BATCH_MAX);
            assert_eq!(s.batch_size(), size);
        }
        // The quarantined key comes back after its time.
        let later = now + QUARANTINE + secs(1.0);
        let chunk = s.plan(&wanted, &mut sweep, true, later).remove(0);
        assert!(chunk.keys.contains(&bad));
    }

    #[test]
    fn a_429_backs_off_exponentially_and_a_good_call_ends_it() {
        let t0 = Instant::now();
        let mut s = Scheduler::new(t0);
        let wanted = keys("nse_cm", 3);
        let mut sweep = VecDeque::new();
        let chunk = s.plan(&wanted, &mut sweep, true, t0).remove(0);
        s.settle(&chunk, Outcome::RateLimited, &mut sweep, t0);
        assert_eq!(s.paused(t0 + secs(1.0)), Some("rate limited by Kotak"));
        assert!(s.plan(&wanted, &mut sweep, true, t0 + secs(1.9)).is_empty());
        // 2 s later the keys are sent again (they were put back), and a
        // second 429 pauses for 4 s, a third for 8 s.
        let t1 = t0 + secs(2.0);
        let chunk = s.plan(&wanted, &mut sweep, true, t1).remove(0);
        assert_eq!(chunk.keys, wanted);
        s.settle(&chunk, Outcome::RateLimited, &mut sweep, t1);
        assert!(s.plan(&wanted, &mut sweep, true, t1 + secs(3.9)).is_empty());
        let t2 = t1 + secs(4.0);
        let chunk = s.plan(&wanted, &mut sweep, true, t2).remove(0);
        s.settle(&chunk, Outcome::RateLimited, &mut sweep, t2);
        assert!(s.plan(&wanted, &mut sweep, true, t2 + secs(7.9)).is_empty());
        let t3 = t2 + secs(8.0);
        let chunk = s.plan(&wanted, &mut sweep, true, t3).remove(0);
        s.settle(&chunk, Outcome::Ok, &mut sweep, t3);
        assert_eq!(s.paused(t3), None);
        // After a success the next 429 starts over at 2 s.
        let t4 = t3 + secs(2.0);
        let chunk = s.plan(&wanted, &mut sweep, true, t4).remove(0);
        s.settle(&chunk, Outcome::RateLimited, &mut sweep, t4);
        assert!(s.plan(&wanted, &mut sweep, true, t4 + secs(1.9)).is_empty());
        assert_eq!(s.plan(&wanted, &mut sweep, true, t4 + secs(2.0)).len(), 1);
        // A transport error pauses too, from 1 s.
        let t5 = t4 + secs(4.0);
        let chunk = s.plan(&wanted, &mut sweep, true, t5).remove(0);
        s.settle(&chunk, Outcome::Transient, &mut sweep, t5);
        assert_eq!(s.paused(t5), Some("backing off after an error"));
        assert!(s.plan(&wanted, &mut sweep, true, t5 + secs(0.9)).is_empty());
        assert_eq!(s.plan(&wanted, &mut sweep, true, t5 + secs(1.0)).len(), 1);
        // The cap: many 429s in a row never pause past a minute.
        let mut b = Backoff::new(RATE_LIMIT_PAUSE, RATE_LIMIT_PAUSE_MAX);
        let pauses: Vec<u64> = (0..7).map(|_| b.hit(t0).as_secs()).collect();
        assert_eq!(pauses, vec![2, 4, 8, 16, 32, 60, 60]);
    }

    #[test]
    fn sweep_calls_alternate_with_live_ones_and_come_back_unanswered() {
        let t0 = Instant::now();
        let mut s = Scheduler::new(t0);
        let wanted = keys("nse_fo", 30);
        let mut sweep: VecDeque<String> = keys("nse_cm", 100).into();
        // Both pending: one live call, one sweep call.
        let chunks = s.plan(&wanted, &mut sweep, true, t0);
        assert_eq!(chunks.len(), 2);
        assert!(!chunks[0].sweep && chunks[1].sweep);
        assert_eq!(chunks[0].keys, &wanted[..25]);
        assert_eq!(chunks[1].keys, keys("nse_cm", 25));
        assert_eq!(sweep.len(), 75);
        // A sweep call that got no answer goes back to the front, in order.
        s.settle(&chunks[1], Outcome::Transient, &mut sweep, t0);
        assert_eq!(sweep.len(), 100);
        assert_eq!(sweep.front().map(String::as_str), Some("nse_cm|1"));
        // Only the sweep pending: both calls are its.
        let t1 = t0 + secs(1.0);
        let chunks = s.plan(&[], &mut sweep, true, t1 + secs(1.0));
        assert_eq!(chunks.len(), 2);
        assert!(chunks.iter().all(|c| c.sweep));
        assert_eq!(sweep.len(), 50);
        // The bucket spent, a sweep chunk already taken is put back.
        assert!(s.plan(&[], &mut sweep, true, t1 + secs(1.5)).is_empty());
        assert_eq!(sweep.len(), 50);
        // A refusal of a sweep chunk halves the batch and puts it back too
        // (the second call of that second took the other 25).
        let chunks = s.plan(&[], &mut sweep, true, t1 + secs(2.0));
        assert_eq!(chunks.len(), 2);
        assert!(sweep.is_empty());
        s.settle(&chunks[0], Outcome::Refused, &mut sweep, t1 + secs(2.0));
        assert_eq!(s.batch_size(), 12);
        assert_eq!(sweep.len(), 25);
        assert_eq!(sweep.front().map(String::as_str), Some("nse_cm|51"));
    }

    #[test]
    fn errors_are_classified_for_the_scheduler() {
        let api = |code: i32| KotakError::ApiError { status_code: code, message: String::new() };
        assert_eq!(classify(&api(429)), Outcome::RateLimited);
        assert_eq!(classify(&api(503)), Outcome::Transient);
        assert_eq!(classify(&api(401)), Outcome::Transient);
        assert_eq!(classify(&api(400)), Outcome::Refused);
        assert_eq!(classify(&api(1009)), Outcome::Refused);
        assert_eq!(classify(&KotakError::NotAuthenticated), Outcome::Transient);
        assert_eq!(classify(&KotakError::SessionExpired { status: 401 }), Outcome::Transient);
        let junk = serde_json::from_str::<serde_json::Value>("<").unwrap_err();
        assert_eq!(classify(&KotakError::Json(junk)), Outcome::Refused);
    }
}
