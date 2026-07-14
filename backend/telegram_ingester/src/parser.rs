use std::sync::LazyLock;

use regex::Regex;
use shared_domain::TradeSignal;
use chrono::{Datelike, Local, NaiveDate, Weekday};

// ---------------------------------------------------------------------------
// Compiled regex cache
// ---------------------------------------------------------------------------

/// Options signal: `BUY BHEL 425 CE ABOVE 8.25`
static OPTS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(BUY|SELL)\s+([A-Z0-9&]+)\s+(\d+(?:\.\d+)?)\s+(CE|PE)\s+(ABOVE|BELOW)\s+(\d+(?:\.\d+)?)",
    ).expect("OPTS_RE")
});

/// Equity signal (no strike/type): `BUY RELIANCE ABOVE 2500`
static EQT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(BUY|SELL)\s+([A-Z0-9&]+)\s+(ABOVE|BELOW)\s+(\d+(?:\.\d+)?)")
        .expect("EQT_RE")
});

/// TARGET / TGT line — captures the value(s) after the separator.
static TGT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:TGT|TARGET)[^\n:]*:?-?\s*([\d.\s/]+)").expect("TGT_RE")
});

/// Stop-loss value — handles `SL :- 5`, `S.L. : 5`, `STOP LOSS :- 5.5`.
static SL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:STOP[-\s]*LOSS|S\.?L\.?)\s*:?-?\s*([\d.]+)").expect("SL_RE")
});

/// Expiry month: `JULY EXPIRY`, `JUL EXPIRY`, `25 JUL EXPIRY`, `26 JUNE`.
static EXPIRY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:(\d{1,2})[-\s]+)?(JAN(?:UARY)?|FEB(?:RUARY)?|MAR(?:CH)?|APR(?:IL)?|MAY|JUN(?:E)?|JUL(?:Y)?|AUG(?:UST)?|SEP(?:TEMBER)?|OCT(?:OBER)?|NOV(?:EMBER)?|DEC(?:EMBER)?)(?:\s+EXPIRY)?\b"
    ).expect("EXPIRY_RE")
});

fn parse_month(m: &str) -> Option<u32> {
    let m = m.to_uppercase();
    if m.starts_with("JAN") { Some(1) }
    else if m.starts_with("FEB") { Some(2) }
    else if m.starts_with("MAR") { Some(3) }
    else if m.starts_with("APR") { Some(4) }
    else if m.starts_with("MAY") { Some(5) }
    else if m.starts_with("JUN") { Some(6) }
    else if m.starts_with("JUL") { Some(7) }
    else if m.starts_with("AUG") { Some(8) }
    else if m.starts_with("SEP") { Some(9) }
    else if m.starts_with("OCT") { Some(10) }
    else if m.starts_with("NOV") { Some(11) }
    else if m.starts_with("DEC") { Some(12) }
    else { None }
}

fn is_trading_holiday(d: NaiveDate) -> bool {
    if d.weekday() == Weekday::Sat || d.weekday() == Weekday::Sun {
        return true;
    }
    if d.year() == 2026 {
        let md = (d.month(), d.day());
        let holidays_2026 = [
            (1, 26), (3, 3), (3, 26), (3, 31), (4, 3), (4, 14),
            (5, 1), (5, 28), (6, 26), (9, 14), (10, 2), (10, 20),
            (11, 10), (11, 24), (12, 25),
        ];
        return holidays_2026.contains(&md);
    }
    false
}

fn adjust_for_holidays(mut d: NaiveDate) -> NaiveDate {
    while is_trading_holiday(d) {
        d = d.pred_opt().unwrap();
    }
    d
}

fn get_expiry_weekday(instrument: &str) -> Weekday {
    let inst = instrument.to_uppercase();
    if inst.contains("SENSEX") || inst.contains("BANKEX") {
        Weekday::Thu
    } else {
        // NIFTY, BANKNIFTY, FINNIFTY, MIDCPNIFTY
        Weekday::Tue
    }
}

fn next_weekday(mut d: NaiveDate, target: Weekday) -> NaiveDate {
    while d.weekday() != target {
        d = d.succ_opt().unwrap();
    }
    adjust_for_holidays(d)
}

fn resolve_expiry_date(day_str: Option<&str>, month_str: &str, instrument: &str) -> Option<String> {
    let parsed_m = parse_month(month_str)?;
    let now = Local::now().date_naive();
    let curr_m = now.month();
    let mut curr_y = now.year();

    if curr_m == 12 && parsed_m == 1 {
        curr_y += 1;
    } else if curr_m == 1 && parsed_m == 12 {
        curr_y -= 1;
    }

    let exact_date = if let Some(d) = day_str {
        if let Ok(day) = d.parse::<u32>() {
            let mut date = NaiveDate::from_ymd_opt(curr_y, parsed_m, day)?;
            // If they specified a date, we also ensure it's shifted if it falls on a holiday.
            date = adjust_for_holidays(date);
            Some(date)
        } else { None }
    } else { None };

    if let Some(date) = exact_date {
        Some(date.format("%d-%b-%Y").to_string().to_uppercase())
    } else {
        let start = if curr_y == now.year() && parsed_m == now.month() {
            now
        } else {
            NaiveDate::from_ymd_opt(curr_y, parsed_m, 1).unwrap_or(now)
        };
        let target_day = get_expiry_weekday(instrument);
        Some(next_weekday(start, target_day).format("%d-%b-%Y").to_string().to_uppercase())
    }
}

// ---------------------------------------------------------------------------
// Public parser
// ---------------------------------------------------------------------------

/// Parse a raw Telegram message into a [`TradeSignal`].
///
/// Tries the options pattern first, falls back to equity.
/// Returns `None` if neither pattern matches.
///
/// # Example
/// ```text
/// BUY BHEL 425 CE ABOVE 8.25
/// TARGET :- 9.50 / 11.50
/// SL :- 5
/// JULY EXPIRY
/// ```
pub fn parse_signal(text: &str, source: &str) -> Option<TradeSignal> {
    let (action, instrument_name, strike, option_type, entry_condition, entry_price) =
        if let Some(caps) = OPTS_RE.captures(text) {
            (
                caps[1].to_uppercase(), caps[2].to_uppercase(),
                Some(caps[3].parse::<f64>().ok()?),
                Some(caps[4].to_uppercase()),
                caps[5].to_uppercase(),
                caps[6].parse::<f64>().ok()?,
            )
        } else if let Some(caps) = EQT_RE.captures(text) {
            (
                caps[1].to_uppercase(), caps[2].to_uppercase(),
                None, None,
                caps[3].to_uppercase(),
                caps[4].parse::<f64>().ok()?,
            )
        } else {
            return None;
        };

    let targets: Vec<f64> = TGT_RE.captures(text)
        .map(|c| c[1].split('/').filter_map(|p| p.trim().parse().ok()).collect())
        .unwrap_or_default();

    let stop_loss = SL_RE.captures(text)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(0.0);

    let expiry = EXPIRY_RE.captures(text).and_then(|c| {
        let day = c.get(1).map(|m| m.as_str());
        let month = c.get(2).map(|m| m.as_str())?;
        resolve_expiry_date(day, month, &instrument_name)
    });

    Some(TradeSignal {
        instrument_name, strike, option_type, expiry,
        action, entry_condition, entry_price, targets, stop_loss,
        source: source.to_owned(),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_multi_target() {
        let text = "BUY BHEL 425 CE ABOVE 8.25\nTARGET :- 9.50 / 11.50\nSL :- 5\nJULY EXPIRY";
        let sig = parse_signal(text, "test").unwrap();
        assert_eq!(sig.action, "BUY");
        assert_eq!(sig.instrument_name, "BHEL");
        assert_eq!(sig.strike, Some(425.0));
        assert_eq!(sig.option_type.as_deref(), Some("CE"));
        assert_eq!(sig.targets, vec![9.50, 11.50]);
        assert!((sig.stop_loss - 5.0).abs() < f64::EPSILON);
        assert!(sig.expiry.is_some()); // e.g. "??-JUL-????"
    }

    #[test]
    fn options_exact_expiry() {
        let text = "BUY BANKNIFTY 55400 CE ABOVE 690\nTARGET :- 750 / 850\nSL - 600\n26 JUNE EXPIRY";
        let sig = parse_signal(text, "test").unwrap();
        assert_eq!(sig.action, "BUY");
        assert_eq!(sig.instrument_name, "BANKNIFTY");
        let expiry = sig.expiry.unwrap();
        assert!(expiry.starts_with("26-JUN"));
    }

    #[test]
    fn options_single_target() {
        let text = "SELL NIFTY 18500 PE BELOW 120\nTARGET: 80\nSL: 145\nAUG EXPIRY";
        let sig = parse_signal(text, "test").unwrap();
        assert_eq!(sig.action, "SELL");
        assert_eq!(sig.targets, vec![80.0]);
        assert!((sig.stop_loss - 145.0).abs() < f64::EPSILON);
    }

    #[test]
    fn equity_signal() {
        let text = "BUY RELIANCE ABOVE 2500\nTGT 2600 / 2700\nSL 2420";
        let sig = parse_signal(text, "test").unwrap();
        assert_eq!(sig.instrument_name, "RELIANCE");
        assert_eq!(sig.strike, None);
        assert_eq!(sig.targets.len(), 2);
    }

    #[test]
    fn no_match_returns_none() {
        assert!(parse_signal("Hello world!", "test").is_none());
    }
}
