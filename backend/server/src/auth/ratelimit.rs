//! A small fixed-window limiter: at most `max` attempts under a key inside
//! `window`, then `Err(seconds until the window ends)`.

use std::time::{Duration, Instant};

use dashmap::DashMap;

struct Window {
    count: u32,
    started: Instant,
}

/// Past this many keys, expired windows are dropped on the next check.
const PRUNE_ABOVE: usize = 10_000;

#[derive(Default)]
pub struct Limiter {
    windows: DashMap<String, Window>,
}

impl Limiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Count one attempt under `key`.
    pub fn check(&self, key: &str, max: u32, window: Duration) -> Result<(), u64> {
        self.check_at(key, max, window, Instant::now())
    }

    pub fn check_at(&self, key: &str, max: u32, window: Duration, now: Instant) -> Result<(), u64> {
        if self.windows.len() > PRUNE_ABOVE {
            self.windows.retain(|_, w| now.duration_since(w.started) < window);
        }
        let mut entry = self.windows.entry(key.to_string()).or_insert(Window { count: 0, started: now });
        if now.duration_since(entry.started) >= window {
            entry.count = 0;
            entry.started = now;
        }
        if entry.count >= max {
            let left = window.saturating_sub(now.duration_since(entry.started));
            return Err(left.as_secs().max(1));
        }
        entry.count += 1;
        Ok(())
    }

    /// Forget `key` — after a success, so the next failures start afresh.
    pub fn clear(&self, key: &str) {
        self.windows.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limit_holds_inside_the_window_and_resets_after_it() {
        let limiter = Limiter::new();
        let t0 = Instant::now();
        let window = Duration::from_secs(600);
        for _ in 0..3 {
            assert!(limiter.check_at("k", 3, window, t0).is_ok());
        }
        assert_eq!(limiter.check_at("k", 3, window, t0 + Duration::from_secs(100)), Err(500));
        assert!(limiter.check_at("other", 3, window, t0).is_ok());
        assert!(limiter.check_at("k", 3, window, t0 + window).is_ok());
    }

    #[test]
    fn clearing_a_key_starts_over() {
        let limiter = Limiter::new();
        let t0 = Instant::now();
        let window = Duration::from_secs(60);
        assert!(limiter.check_at("k", 1, window, t0).is_ok());
        assert!(limiter.check_at("k", 1, window, t0).is_err());
        limiter.clear("k");
        assert!(limiter.check_at("k", 1, window, t0).is_ok());
    }
}
