use chrono::{DateTime, Datelike, Duration as ChronoDuration, FixedOffset, NaiveDate, Timelike};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::{LazyLock, RwLock};

use crate::{
    ist_offset, now_ist, MARKET_CLOSE_HOUR, MARKET_CLOSE_MINUTE, MARKET_OPEN_HOUR, MARKET_OPEN_MINUTE,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecialSession {
    pub date: NaiveDate,
    #[serde(serialize_with = "serialize_hhmm", deserialize_with = "deserialize_hhmm")]
    pub start: u32,
    #[serde(serialize_with = "serialize_hhmm", deserialize_with = "deserialize_hhmm")]
    pub end: u32,
}

pub fn serialize_hhmm<S>(minute: &u32, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let s = format_hhmm(*minute);
    serializer.serialize_str(&s)
}

pub fn deserialize_hhmm<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    parse_hhmm(&s).ok_or_else(|| serde::de::Error::custom("invalid HH:MM time"))
}

pub fn parse_hhmm(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.len() != 5 || s.as_bytes()[2] != b':' {
        return None;
    }
    let h: u32 = s[0..2].parse().ok()?;
    let m: u32 = s[3..5].parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some(h * 60 + m)
}

pub fn format_hhmm(minute: u32) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

static SPECIAL_SESSIONS: LazyLock<RwLock<Vec<SpecialSession>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

static TEST_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[doc(hidden)]
pub fn special_sessions_test_guard() -> std::sync::MutexGuard<'static, ()> {
    let guard = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    set_special_sessions(vec![]);
    guard
}

pub fn set_special_sessions(mut sessions: Vec<SpecialSession>) {
    sessions.sort_by_key(|s| s.date);
    let mut lock = SPECIAL_SESSIONS.write().unwrap_or_else(|e| e.into_inner());
    *lock = sessions;
}

pub fn special_sessions() -> Vec<SpecialSession> {
    SPECIAL_SESSIONS.read().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn special_session_on(date: NaiveDate) -> Option<SpecialSession> {
    SPECIAL_SESSIONS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|s| s.date == date)
        .copied()
}

pub fn in_special_session_at(now: DateTime<FixedOffset>) -> bool {
    let today = now.date_naive();
    if let Some(session) = special_session_on(today) {
        let minute = now.hour() * 60 + now.minute();
        session.start <= minute && minute < session.end
    } else {
        false
    }
}

pub fn in_special_session() -> bool {
    in_special_session_at(now_ist())
}

pub fn is_regular_market_hours_at(now: DateTime<FixedOffset>) -> bool {
    let weekday = now.weekday();
    if weekday == chrono::Weekday::Sat || weekday == chrono::Weekday::Sun {
        return false;
    }
    let minute = now.hour() * 60 + now.minute();
    let open_minute = MARKET_OPEN_HOUR * 60 + MARKET_OPEN_MINUTE;
    let close_minute = MARKET_CLOSE_HOUR * 60 + MARKET_CLOSE_MINUTE;
    minute >= open_minute && minute < close_minute
}

pub fn is_market_open_at(now: DateTime<FixedOffset>) -> bool {
    is_regular_market_hours_at(now) || in_special_session_at(now)
}

pub fn is_market_open() -> bool {
    is_market_open_at(now_ist())
}

fn open_candidates_for_day(date: NaiveDate) -> Vec<DateTime<FixedOffset>> {
    let mut candidates = Vec::new();
    let is_weekday = date.weekday() != chrono::Weekday::Sat && date.weekday() != chrono::Weekday::Sun;
    if is_weekday {
        if let Some(open) = date
            .and_hms_opt(MARKET_OPEN_HOUR, MARKET_OPEN_MINUTE, 0)
            .and_then(|t| t.and_local_timezone(ist_offset()).single())
        {
            candidates.push(open);
        }
    }
    if let Some(session) = special_session_on(date) {
        let h = session.start / 60;
        let m = session.start % 60;
        if let Some(open) = date
            .and_hms_opt(h, m, 0)
            .and_then(|t| t.and_local_timezone(ist_offset()).single())
        {
            candidates.push(open);
        }
    }
    candidates
}

pub fn next_market_open_ist_at(now: DateTime<FixedOffset>) -> DateTime<FixedOffset> {
    let mut date = now.date_naive();

    // Check today first: only candidates strictly in the future (> now)
    let today_candidates: Vec<DateTime<FixedOffset>> = open_candidates_for_day(date)
        .into_iter()
        .filter(|t| *t > now)
        .collect();

    if let Some(earliest) = today_candidates.into_iter().min() {
        return earliest;
    }

    // Look at future days
    loop {
        date += ChronoDuration::days(1);
        let future_candidates = open_candidates_for_day(date);
        if let Some(earliest) = future_candidates.into_iter().min() {
            return earliest;
        }
    }
}

pub fn next_market_open_ist() -> DateTime<FixedOffset> {
    next_market_open_ist_at(now_ist())
}

pub fn duration_until_market_open_at(now: DateTime<FixedOffset>) -> std::time::Duration {
    if is_market_open_at(now) {
        return std::time::Duration::ZERO;
    }
    let next_open = next_market_open_ist_at(now);
    (next_open - now).to_std().unwrap_or(std::time::Duration::ZERO)
}

pub fn duration_until_market_open() -> std::time::Duration {
    duration_until_market_open_at(now_ist())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<FixedOffset> {
        NaiveDate::from_ymd_opt(year, month, day)
            .unwrap()
            .and_hms_opt(hour, minute, 0)
            .unwrap()
            .and_local_timezone(ist_offset())
            .single()
            .unwrap()
    }

    #[test]
    fn special_session_serde_json() {
        let session = SpecialSession {
            date: NaiveDate::from_ymd_opt(2026, 11, 8).unwrap(),
            start: 14 * 60 + 20,
            end: 15 * 60 + 30,
        };
        let json = serde_json::to_string(&session).unwrap();
        assert_eq!(json, r#"{"date":"2026-11-08","start":"14:20","end":"15:30"}"#);

        let parsed: SpecialSession = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, session);
    }

    #[test]
    fn sunday_with_session_and_without_session() {
        let _guard = special_sessions_test_guard();
        let sunday = NaiveDate::from_ymd_opt(2026, 11, 8).unwrap();
        assert_eq!(sunday.weekday(), chrono::Weekday::Sun);

        // With session 14:20 - 15:30
        let session = SpecialSession {
            date: sunday,
            start: 14 * 60 + 20,
            end: 15 * 60 + 30,
        };
        set_special_sessions(vec![session]);

        assert!(is_market_open_at(dt(2026, 11, 8, 14, 20)));
        assert!(is_market_open_at(dt(2026, 11, 8, 15, 29)));
        assert!(!is_market_open_at(dt(2026, 11, 8, 14, 19)));
        assert!(!is_market_open_at(dt(2026, 11, 8, 15, 30)));
        assert!(!is_market_open_at(dt(2026, 11, 8, 10, 0)));

        // Next open before 14:20 on the Sunday is 14:20 that day
        let next_open = next_market_open_ist_at(dt(2026, 11, 8, 10, 0));
        assert_eq!(next_open, dt(2026, 11, 8, 14, 20));

        // The same Sunday with no session -> closed
        set_special_sessions(vec![]);
        assert!(!is_market_open_at(dt(2026, 11, 8, 14, 20)));
        assert!(!is_market_open_at(dt(2026, 11, 8, 15, 29)));
        assert!(!is_market_open_at(dt(2026, 11, 8, 10, 0)));

        // Next open from Sunday 10:00 with no session is Monday 09:15
        let next_open_no_session = next_market_open_ist_at(dt(2026, 11, 8, 10, 0));
        assert_eq!(next_open_no_session, dt(2026, 11, 9, 9, 15));

        set_special_sessions(vec![]);
    }

    #[test]
    fn normal_weekday_unchanged() {
        let _guard = special_sessions_test_guard();
        set_special_sessions(vec![]);
        // Wednesday 2026-11-04
        let wednesday = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        assert_eq!(wednesday.weekday(), chrono::Weekday::Wed);

        assert!(!is_market_open_at(dt(2026, 11, 4, 9, 14)));
        assert!(is_market_open_at(dt(2026, 11, 4, 9, 15)));
        assert!(is_market_open_at(dt(2026, 11, 4, 15, 39)));
        assert!(!is_market_open_at(dt(2026, 11, 4, 15, 40)));

        set_special_sessions(vec![]);
    }

    #[test]
    fn weekday_session_adds_window_to_regular_hours_without_closing_them() {
        let _guard = special_sessions_test_guard();
        let wednesday = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        // Session in the evening 18:00 - 19:15
        let session = SpecialSession {
            date: wednesday,
            start: 18 * 60,
            end: 19 * 60 + 15,
        };
        set_special_sessions(vec![session]);

        // Regular hours are still open
        assert!(is_market_open_at(dt(2026, 11, 4, 9, 15)));
        assert!(is_market_open_at(dt(2026, 11, 4, 12, 0)));
        assert!(is_market_open_at(dt(2026, 11, 4, 15, 39)));
        assert!(!is_market_open_at(dt(2026, 11, 4, 15, 40)));

        // Evening special session is open
        assert!(!is_market_open_at(dt(2026, 11, 4, 17, 59)));
        assert!(is_market_open_at(dt(2026, 11, 4, 18, 0)));
        assert!(is_market_open_at(dt(2026, 11, 4, 19, 14)));
        assert!(!is_market_open_at(dt(2026, 11, 4, 19, 15)));

        // Cleanup
        set_special_sessions(vec![]);
    }
}
