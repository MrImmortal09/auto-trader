use std::sync::LazyLock;

use chrono::{Datelike, Days, NaiveDate, Weekday};
use regex::Regex;
use shared_domain::{today_ist, FeatureFlags, TradeSignal};

// ---------------------------------------------------------------------------
// Compiled regex cache
// ---------------------------------------------------------------------------

/// Options signal: `BUY BHEL 425 CE ABOVE 8.25`
static OPTS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(BUY|SELL)\s+([A-Z0-9&\-]+)\s+(\d+(?:\.\d+)?)\s+(CE|PE)\s+(ABOVE|BELOW)\s+(\d+(?:\.\d+)?)",
    ).expect("OPTS_RE")
});

/// Options signal with an entry range instead of a trigger:
/// `BUY NIFTY NEXT WEEK 23000 STRIKE PUTS 140-145`,
/// `BUY NIFTY NEXT WEEK 23000 STRIKE PUTS @ 125-130`,
/// `BUY NIFTY 22300 CALLS CMP 100-102`. BUY only — this account never writes
/// options.
static RANGE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\bBUY\s+([A-Z][A-Z0-9&\-]*)\s+(NEXT\s+WEEK\s+)?(\d+(?:\.\d+)?)\s+(?:STRIKE\s+)?(CE|PE|CALLS?|PUTS?)\s+(?:(?:@|AT|CMP)\s*)?(\d+(?:\.\d+)?)\s*[-–]\s*(\d+(?:\.\d+)?)\b",
    ).expect("RANGE_RE")
});

/// A BUY that names a strike and a side — what a call looks like even when
/// `parse_signal` can't make a trade of it (`BUY AGAIN 22300 CALLS CMP 100-102`
/// names no instrument).
static BUY_CALL_SHAPE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bBUY\b[^\n]*?\b\d+(?:\.\d+)?\s+(?:STRIKE\s+)?(?:CE|PE|CALLS?|PUTS?)\b")
        .expect("BUY_CALL_SHAPE_RE")
});

/// Equity signal (no strike/type): `BUY RELIANCE ABOVE 2500`
static EQT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(BUY|SELL)\s+([A-Z0-9&\-]+)\s+(ABOVE|BELOW)\s+(\d+(?:\.\d+)?)")
        .expect("EQT_RE")
});

/// TARGET / TGT line — captures the value(s) after the separator.
static TGT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:TGT|TARGET)[^0-9\n]*([\d. \t/]+)").expect("TGT_RE")
});

/// Stop-loss value — handles `SL :- 5`, `S.L. : 5`, `STOP LOSS :- 5.5`.
static SL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:STOP[-\s]*LOSS|S\.?L\.?)\s*:?-?\s*([\d.]+)").expect("SL_RE")
});

/// Update Stop-loss value from replies — handles `Move SL to 4`, `SL to 4`, `SL 4`, `shift sl to 4`.
static UPDATE_SL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:MOVE|SHIFT)?\s*(?:STOP[-\s]*LOSS|S\.?L\.?)\s*(?:TO|:-|:|->|-)?\s*([\d.]+)").expect("UPDATE_SL_RE")
});

/// Update Exit command from replies — handles `exit at 610`, `exit @610`, `exit@610`, `exit 610`, `EXIT AT 610.5`, `exit all at 610`.
static UPDATE_EXIT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bexit\b\s*(?:all\s+)?(?:at|@|cmp)?\s*([\d.]+)").expect("UPDATE_EXIT_RE")
});

static EXPIRY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        // The `(?:ST|ND|RD|TH)?` swallows an ordinal suffix on the day — `1ST
        // SEPTEMBER`, `3RD JULY`, `21ST NOV` — which otherwise breaks the
        // `[ \t-]+` separator and drops the day entirely, silently turning a
        // weekly expiry into the monthly one.
        r"(?i)(?:(\d{1,2})(?:ST|ND|RD|TH)?[ \t-]+)?(JAN(?:UARY)?|FEB(?:RUARY)?|MAR(?:CH)?|APR(?:IL)?|MAY|JUN(?:E)?|JUL(?:Y)?|AUG(?:UST)?|SEP(?:TEMBER)?|OCT(?:OBER)?|NOV(?:EMBER)?|DEC(?:EMBER)?)(?:\s+EXPIRY)?\b"
    ).expect("EXPIRY_RE")
});

/// Words that turn up where an equity ticker would sit in channel chatter —
/// `Buy Only Above 550`, `Buy Now Above 550`.
///
/// The options pattern needs a strike and CE/PE, so plain English can't
/// satisfy it. The equity pattern is just `BUY <word> ABOVE <price>`, which
/// ordinary sentences do satisfy: "Buy Only Above 550" parsed as a BUY of an
/// instrument named `ONLY`. That phantom carried no target and no stop-loss,
/// so had the name resolved to a real scrip it would have opened a position
/// with `current_sl` 0 (an LTP can never cross it) and no target to exit on —
/// held until the expiry square-off with no protection at all.
///
/// Only words that are not NSE tickers belong here.
const NOISE_INSTRUMENTS: &[&str] = &[
    "ONLY", "NOW", "ABOVE", "BELOW", "AT", "CMP", "LEVEL", "WAIT", "AGAIN",
    "TODAY", "MORE", "THIS", "THAT", "THE", "AND", "FOR", "YOUR", "ALL",
    "ANY", "IT", "IS", "ON", "IN", "TO", "OF", "A", "AN", "BOOK", "HOLD",
];

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
    } else if inst.contains("NIFTY") || inst.contains("BANKNIFTY") || inst.contains("FINNIFTY") || inst.contains("MIDCPNIFTY") {
        // Keep existing weekly-index behavior.
        Weekday::Tue
    } else {
        // Stock options are monthly-only; use Thursday expiry calendar.
        Weekday::Thu
    }
}

fn next_weekday(mut d: NaiveDate, target: Weekday) -> NaiveDate {
    while d.weekday() != target {
        d = d.succ_opt().unwrap();
    }
    adjust_for_holidays(d)
}

fn last_day_of_month(year: i32, month: u32) -> Option<NaiveDate> {
    let (ny, nm) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    let first_next = NaiveDate::from_ymd_opt(ny, nm, 1)?;
    first_next.pred_opt()
}

fn monthly_expiry_date(year: i32, month: u32, instrument: &str) -> Option<NaiveDate> {
    let target = get_expiry_weekday(instrument);
    let mut d = last_day_of_month(year, month)?;
    while d.weekday() != target {
        d = d.pred_opt()?;
    }
    Some(adjust_for_holidays(d))
}

fn resolve_expiry_date_with_now(
    day_str: Option<&str>,
    month_str: &str,
    instrument: &str,
    now: NaiveDate,
) -> Option<String> {
    let parsed_m = parse_month(month_str)?;
    let target_day = get_expiry_weekday(instrument);

    let resolved = if let Some(d) = day_str {
        let day = d.parse::<u32>().ok()?;
        let mut year = now.year();
        if parsed_m < now.month() {
            year += 1;
        }

        let mut date = NaiveDate::from_ymd_opt(year, parsed_m, day)?;
        date = adjust_for_holidays(date);

        // If this exact-day/month already passed this year, roll to next year.
        if date < now {
            let mut rolled = NaiveDate::from_ymd_opt(year + 1, parsed_m, day)?;
            rolled = adjust_for_holidays(rolled);
            rolled
        } else {
            date
        }
    } else {
        // Month-only expiry means monthly contract expiry for that instrument.
        let mut year = now.year();
        if parsed_m < now.month() {
            year += 1;
        }

        let mut date = monthly_expiry_date(year, parsed_m, instrument)?;
        if date < now {
            date = monthly_expiry_date(year + 1, parsed_m, instrument)?;
        }

        // For explicit current-month weekly style wording, keep a sensible fallback.
        if date.month() != parsed_m {
            let start = if year == now.year() && parsed_m == now.month() {
                now
            } else {
                NaiveDate::from_ymd_opt(year, parsed_m, 1)?
            };
            next_weekday(start, target_day)
        } else {
            date
        }
    };

    Some(resolved.format("%d-%b-%Y").to_string().to_uppercase())
}

/// Weekday the weekly series expires on, for the indices that still list
/// weeklies. `None` for everything else (BANKNIFTY and the other NSE indices
/// lost their weeklies in Nov 2024; stocks never had them), so a `NEXT WEEK`
/// call on those is refused rather than guessed.
fn weekly_expiry_weekday(instrument: &str) -> Option<Weekday> {
    match instrument {
        "NIFTY" => Some(Weekday::Tue),
        "SENSEX" => Some(Weekday::Thu),
        _ => None,
    }
}

/// `NEXT WEEK` → the weekly expiry falling in next calendar week (Mon–Sun),
/// pulled back to the previous trading day when that day is a holiday. Posted
/// Wed 21 Oct 2026, a NIFTY call means the 27 Oct contract.
fn next_week_expiry_with_now(instrument: &str, now: NaiveDate) -> Option<String> {
    let weekday = weekly_expiry_weekday(instrument)?;
    let next_monday = now + Days::new(7 - u64::from(now.weekday().num_days_from_monday()));
    let date = adjust_for_holidays(next_monday + Days::new(u64::from(weekday.num_days_from_monday())));
    Some(date.format("%d-%b-%Y").to_string().to_uppercase())
}

fn option_side(word: &str) -> &'static str {
    if word.to_uppercase().starts_with('C') { "CE" } else { "PE" }
}

// ---------------------------------------------------------------------------
// Public parser
// ---------------------------------------------------------------------------

/// Parse a raw Telegram message into a [`TradeSignal`].
///
/// Tries the options trigger pattern first, then the options range pattern,
/// then equity. Returns `None` if none matches.
///
/// # Examples
/// ```text
/// BUY BHEL 425 CE ABOVE 8.25
/// TARGET :- 9.50 / 11.50
/// SL :- 5
/// JULY EXPIRY
///
/// BUY NIFTY NEXT WEEK 23000 STRIKE PUTS @ 125-130 SL 105 TARGET 165
/// ```
pub fn parse_signal(text: &str, source: &str, signal_id: Option<String>) -> Option<TradeSignal> {
    parse_signal_on(text, source, signal_id, today_ist())
}

/// [`parse_signal`] as if received on `today` (IST) — relative expiries
/// (`NEXT WEEK`, `JULY`, `1ST SEPTEMBER`) resolve against it.
pub fn parse_signal_on(
    text: &str,
    source: &str,
    signal_id: Option<String>,
    today: NaiveDate,
) -> Option<TradeSignal> {
    let targets: Vec<f64> = TGT_RE.captures(text)
        .map(|c| c[1].split('/').filter_map(|p| p.trim().parse().ok()).collect())
        .unwrap_or_default();

    let stop_loss = SL_RE.captures(text)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(0.0);

    let mut entry_price_max = None;
    let mut next_week = false;
    let (action, instrument_name, strike, option_type, entry_condition, entry_price) =
        if let Some(caps) = OPTS_RE.captures(text) {
            (
                caps[1].to_uppercase(), caps[2].to_uppercase(),
                Some(caps[3].parse::<f64>().ok()?),
                Some(caps[4].to_uppercase()),
                caps[5].to_uppercase(),
                caps[6].parse::<f64>().ok()?,
            )
        } else if let Some(caps) = RANGE_RE.captures(text) {
            let low = caps[5].parse::<f64>().ok()?;
            let high = caps[6].parse::<f64>().ok()?;
            // A range that is backwards, starts at/under the stop, or reaches
            // target 1 is a typo, not a trade.
            if low <= 0.0 || high < low || (stop_loss > 0.0 && low <= stop_loss) {
                return None;
            }
            if targets.first().is_some_and(|&t1| high >= t1) {
                return None;
            }
            entry_price_max = Some(high);
            next_week = caps.get(2).is_some();
            (
                "BUY".to_string(), caps[1].to_uppercase(),
                Some(caps[3].parse::<f64>().ok()?),
                Some(option_side(&caps[4]).to_string()),
                // The range's low edge behaves as an ABOVE trigger; the engine
                // caps the buy at `entry_price_max` (see `entry_buy_window`).
                "ABOVE".to_string(),
                low,
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

    // Plain English satisfies these patterns too (see NOISE_INSTRUMENTS), and
    // `BUY AGAIN 22300 CALLS` names no instrument at all — never guess one.
    if NOISE_INSTRUMENTS.contains(&instrument_name.as_str()) {
        return None;
    }

    let expiry = if next_week {
        // An instrument with no weekly series can't have a "next week" contract.
        Some(next_week_expiry_with_now(&instrument_name, today)?)
    } else {
        EXPIRY_RE.captures(text).and_then(|c| {
            let day = c.get(1).map(|m| m.as_str());
            let month = c.get(2).map(|m| m.as_str())?;
            resolve_expiry_date_with_now(day, month, &instrument_name, today)
        })
    };

    Some(TradeSignal {
        instrument_name, strike, option_type, expiry,
        action, entry_condition, entry_price, entry_price_max, targets, stop_loss,
        source: source.to_owned(),
        signal_id,
        raw_message: Some(text.to_owned()),
        chat: None,
    })
}

/// Whether `text` is a BUY call naming a strike and a side, whether or not
/// [`parse_signal`] could make a trade of it. An unparsed one must not fall
/// through to the reply handlers: its `SL 90` would otherwise be applied to
/// whatever position the message happens to reply to.
pub fn looks_like_buy_call(text: &str) -> bool {
    BUY_CALL_SHAPE_RE.is_match(text)
}

/// Parse a raw Telegram reply message to extract a stop-loss update.
/// Returns `Some(new_sl)` if it matches a stop-loss update pattern.
pub fn parse_reply_sl(text: &str) -> Option<f64> {
    UPDATE_SL_RE.captures(text)
        .and_then(|c| c[1].parse().ok())
}

/// Parse a raw Telegram reply message to extract an exit command price.
/// Returns `Some(exit_price)` if it matches an exit command pattern (e.g., "exit at 610").
pub fn parse_reply_exit(text: &str) -> Option<f64> {
    UPDATE_EXIT_RE.captures(text)
        .and_then(|c| c[1].parse().ok())
}

/// What the Telegram event loop does with one message.
#[derive(Debug, Clone)]
pub enum Inbound {
    /// A complete call — trade it.
    Signal(TradeSignal),
    /// A complete call that isn't an option on one of the six indices, while
    /// [`FeatureFlags::index_options_only`] is on — skip it, and say so.
    NotIndexOption(TradeSignal),
    /// Looks like a BUY call but isn't one we can trade (see
    /// [`looks_like_buy_call`]) — drop it, loudly.
    IgnoredCall,
    /// A reply moving the stop of the position opened from message `parent`.
    SlUpdate { parent: String, new_sl: f64 },
    /// A reply ordering the position opened from message `parent` out at `price`.
    Exit { parent: String, price: f64 },
    /// Nothing actionable.
    Unparsed,
}

/// Route one Telegram message. `reply_to` is the id of the message it replies
/// to, if any; `today` is the IST date it arrived.
///
/// Order matters. A complete signal wins over the reply patterns — every
/// signal body carries an `SL :- x` line, so a signal the channel posts *as a
/// reply* would otherwise be swallowed as a stop-loss update. Then an
/// unreadable BUY call is dropped before the reply patterns for the same
/// reason: `BUY AGAIN 22300 CALLS ... SL 90` must not move another stop.
pub fn classify(text: &str, msg_id: &str, reply_to: Option<&str>, today: NaiveDate) -> Inbound {
    classify_with(text, msg_id, reply_to, today, FeatureFlags::default())
}

/// [`classify`] under the engine switches in `flags`; with every switch off
/// it routes exactly as [`classify`]. With `index_options_only` on, a complete
/// call that isn't an index option is [`Inbound::NotIndexOption`] — checked
/// before the reply patterns, so it never becomes a stop move either.
pub fn classify_with(
    text: &str,
    msg_id: &str,
    reply_to: Option<&str>,
    today: NaiveDate,
    flags: FeatureFlags,
) -> Inbound {
    if let Some(signal) = parse_signal_on(text, "telegram", Some(msg_id.to_owned()), today) {
        if flags.index_options_only && !signal.is_index_option() {
            return Inbound::NotIndexOption(signal);
        }
        return Inbound::Signal(signal);
    }
    if looks_like_buy_call(text) {
        return Inbound::IgnoredCall;
    }
    let Some(parent) = reply_to else { return Inbound::Unparsed };
    if let Some(new_sl) = parse_reply_sl(text) {
        Inbound::SlUpdate { parent: parent.to_owned(), new_sl }
    } else if let Some(price) = parse_reply_exit(text) {
        Inbound::Exit { parent: parent.to_owned(), price }
    } else {
        Inbound::Unparsed
    }
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
        let sig = parse_signal(text, "test", None).unwrap();
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
        let text = "BUY BANKNIFTY 55400 CE ABOVE 690\nTARGET :- 750 / 850\nSL - 600\n26 NOV EXPIRY";
        let sig = parse_signal(text, "test", None).unwrap();
        assert_eq!(sig.action, "BUY");
        assert_eq!(sig.instrument_name, "BANKNIFTY");
        let expiry = sig.expiry.unwrap();
        assert!(expiry.starts_with("26-NOV"));
    }

    #[test]
    fn options_single_target() {
        let text = "SELL NIFTY 18500 PE BELOW 120\nTARGET: 80\nSL: 145\nAUG EXPIRY";
        let sig = parse_signal(text, "test", None).unwrap();
        assert_eq!(sig.action, "SELL");
        assert_eq!(sig.targets, vec![80.0]);
        assert!((sig.stop_loss - 145.0).abs() < f64::EPSILON);
    }

    #[test]
    fn equity_signal() {
        let text = "BUY RELIANCE ABOVE 2500\nTGT 2600 / 2700\nSL 2420";
        let sig = parse_signal(text, "test", None).unwrap();
        assert_eq!(sig.instrument_name, "RELIANCE");
        assert_eq!(sig.strike, None);
        assert_eq!(sig.targets.len(), 2);
    }

    #[test]
    fn no_match_returns_none() {
        assert!(parse_signal("Hello world!", "test", None).is_none());
    }

    #[test]
    fn stock_month_only_resolves_to_monthly_expiry() {
        let now = NaiveDate::from_ymd_opt(2026, 7, 15).unwrap();
        let expiry = resolve_expiry_date_with_now(None, "JULY", "BHEL", now).unwrap();
        assert_eq!(expiry, "30-JUL-2026");
    }

    #[test]
    fn month_only_uses_next_year_if_current_year_monthly_expiry_passed() {
        let now = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
        let expiry = resolve_expiry_date_with_now(None, "JULY", "BHEL", now).unwrap();
        assert_eq!(expiry, "29-JUL-2027");
    }

    #[test]
    fn test_parse_reply_sl() {
        assert_eq!(parse_reply_sl("Move SL to 4 and hold for the targets"), Some(4.0));
        assert_eq!(parse_reply_sl("SL to 4.50"), Some(4.5));
        assert_eq!(parse_reply_sl("Shift sl to 3"), Some(3.0));
        assert_eq!(parse_reply_sl("sl -> 2.5"), Some(2.5));
        assert_eq!(parse_reply_sl("SL:- 5"), Some(5.0));
        assert_eq!(parse_reply_sl("Update SL 6.0"), Some(6.0)); // matches SL_RE inside UPDATE_SL_RE logic
        assert_eq!(parse_reply_sl("Something else"), None);
    }

    #[test]
    fn test_parse_reply_exit() {
        assert_eq!(parse_reply_exit("exit at 610"), Some(610.0));
        assert_eq!(parse_reply_exit("EXIT AT 610.50"), Some(610.5));
        assert_eq!(parse_reply_exit("exit @ 610"), Some(610.0));
        assert_eq!(parse_reply_exit("exit @610"), Some(610.0));
        assert_eq!(parse_reply_exit("exit@610"), Some(610.0));
        assert_eq!(parse_reply_exit("exit all at 610"), Some(610.0));
        assert_eq!(parse_reply_exit("exit 610"), Some(610.0));
        assert_eq!(parse_reply_exit("exit cmp 610"), Some(610.0));
        assert_eq!(parse_reply_exit("Something else"), None);
    }

    #[test]
    fn ordinal_day_expiry_keeps_the_day() {
        // `1ST SEPTEMBER` must resolve to the weekly 01-SEP contract, not the
        // September monthly expiry — the wrong contract streams a much richer
        // premium and the position trips its target the moment it opens.
        for (day, suffix) in [("1", "ST"), ("2", "ND"), ("3", "RD"), ("4", "TH"), ("21", "ST")] {
            let text = format!("{day}{suffix} SEPTEMBER EXPIRY");
            let caps = EXPIRY_RE.captures(&text).unwrap();
            assert_eq!(caps.get(1).map(|m| m.as_str()), Some(day), "{day}{suffix}");
            assert!(caps
                .get(2)
                .map(|m| m.as_str().to_uppercase().starts_with("SEP"))
                .unwrap_or(false));
        }

        let now = NaiveDate::from_ymd_opt(2026, 8, 26).unwrap();
        let expiry = resolve_expiry_date_with_now(Some("1"), "SEPTEMBER", "NIFTY", now).unwrap();
        assert_eq!(expiry, "01-SEP-2026");
    }

    #[test]
    fn ordinal_day_full_signal() {
        let text = "BUY NIFTY 24600 PE ABOVE 200\n\nTARGET :- 230 / 270\n\nSL :- 160\n\n1ST SEPTEMBER EXPIRY";
        let sig = parse_signal(text, "test", None).unwrap();
        assert_eq!(sig.instrument_name, "NIFTY");
        assert_eq!(sig.strike, Some(24600.0));
        assert_eq!(sig.option_type.as_deref(), Some("PE"));
        assert_eq!(sig.entry_price, 200.0);
        assert_eq!(sig.targets, vec![230.0, 270.0]);
        assert_eq!(sig.stop_loss, 160.0);
        let expiry = sig.expiry.unwrap();
        assert!(expiry.starts_with("01-SEP"), "got {expiry}");
    }

    #[test]
    fn chatter_is_not_an_equity_signal() {
        // Real message from the channel. `BUY <word> ABOVE <price>` is plain
        // English as much as it is a signal, and this one produced a phantom
        // equity signal for an instrument literally named "ONLY".
        assert!(parse_signal("Please Wait For Level \n\nBuy Only Above 550", "test", None).is_none());
        assert!(parse_signal("Buy Now Above 550", "test", None).is_none());
        // A real equity signal must still get through.
        let sig = parse_signal("BUY RELIANCE ABOVE 2500\nTGT 2600\nSL 2420", "test", None).unwrap();
        assert_eq!(sig.instrument_name, "RELIANCE");
    }

    #[test]
    fn a_full_signal_also_matches_the_reply_sl_pattern() {
        // Why `parse_signal` has to be tried before the reply patterns in
        // `classify`: every signal body carries an `SL :- x` line, so a
        // signal the channel happens to post *as a reply* looks exactly like
        // a stop-loss update to `parse_reply_sl`. Checking replies first
        // swallowed the whole trade.
        let text = "BUY NIFTY 24150 CE ABOVE 170\n\nTARGET :- 200 / 240\n\nSL :- 130\n\n1ST SEPTEMBER EXPIRY";
        assert_eq!(parse_reply_sl(text), Some(130.0));
        let sig = parse_signal(text, "test", None).expect("must parse as a full signal");
        assert_eq!(sig.instrument_name, "NIFTY");
        assert_eq!(sig.strike, Some(24150.0));
        assert_eq!(sig.targets, vec![200.0, 240.0]);
        assert_eq!(sig.stop_loss, 130.0);

        // A genuine reply-style update must NOT look like a full signal, so
        // the fallback still reaches the reply handler.
        assert!(parse_signal("Move SL to 140", "test", None).is_none());
        assert_eq!(parse_reply_sl("Move SL to 140"), Some(140.0));
    }

    #[test]
    fn test_parse_options_with_hyphen() {
        let text = "BUY BAJAJ-AUTO 11300 CE ABOVE 325\n\nTARGET :- 365 / 450\n\nSL :- 250\n\nAUGUST EXPIRY";
        let sig = parse_signal(text, "test", None).unwrap();
        assert_eq!(sig.instrument_name, "BAJAJ-AUTO");
        assert_eq!(sig.strike, Some(11300.0));
        assert_eq!(sig.option_type.as_deref(), Some("CE"));
        assert_eq!(sig.action, "BUY");
        assert_eq!(sig.entry_condition, "ABOVE");
        assert_eq!(sig.entry_price, 325.0);
        assert_eq!(sig.targets, vec![365.0, 450.0]);
        assert_eq!(sig.stop_loss, 250.0);
        assert!(sig.expiry.is_some());
        assert_eq!(sig.entry_price_max, None);
    }

    #[test]
    fn range_call_with_next_week_expiry() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 21).unwrap(); // a Wednesday
        let text = "BUY NIFTY NEXT WEEK 23000 STRIKE PUTS 140-145 SL 120 TARGET 175";
        let sig = parse_signal_on(text, "test", None, today).unwrap();
        assert_eq!(sig.action, "BUY");
        assert_eq!(sig.instrument_name, "NIFTY");
        assert_eq!(sig.strike, Some(23000.0));
        assert_eq!(sig.option_type.as_deref(), Some("PE"));
        assert_eq!(sig.entry_condition, "ABOVE");
        assert_eq!(sig.entry_price, 140.0);
        assert_eq!(sig.entry_price_max, Some(145.0));
        assert_eq!(sig.stop_loss, 120.0);
        assert_eq!(sig.targets, vec![175.0]);
        assert_eq!(sig.expiry.as_deref(), Some("27-OCT-2026"));

        let text = "BUY NIFTY NEXT WEEK 23000 STRIKE PUTS @ 125-130 SL 105 TARGET 165";
        let sig = parse_signal_on(text, "test", None, today).unwrap();
        assert_eq!(sig.option_type.as_deref(), Some("PE"));
        assert_eq!((sig.entry_price, sig.entry_price_max), (125.0, Some(130.0)));
        assert_eq!(sig.stop_loss, 105.0);
        assert_eq!(sig.targets, vec![165.0]);
        assert_eq!(sig.expiry.as_deref(), Some("27-OCT-2026"));
    }

    #[test]
    fn range_call_with_cmp_and_calls() {
        let text = "✅ BUY NIFTY 22300 CALLS CMP 100-102 SL 90 TARGET 125";
        let sig = parse_signal(text, "test", None).unwrap();
        assert_eq!(sig.instrument_name, "NIFTY");
        assert_eq!(sig.strike, Some(22300.0));
        assert_eq!(sig.option_type.as_deref(), Some("CE"));
        assert_eq!((sig.entry_price, sig.entry_price_max), (100.0, Some(102.0)));
        assert_eq!(sig.stop_loss, 90.0);
        assert_eq!(sig.targets, vec![125.0]);
        assert_eq!(sig.expiry, None); // no expiry named -> nearest, as for CE/PE calls
    }

    #[test]
    fn call_without_an_instrument_is_ignored() {
        // Real message from the channel: no instrument, no expiry. Never guess.
        let text = "✅ BUY AGAIN 22300 CALLS CMP 100-102 SL 90 TARGET 125";
        assert!(parse_signal(text, "test", None).is_none());
        assert!(looks_like_buy_call(text));
        // The same with the classic trigger wording.
        assert!(parse_signal("BUY AGAIN 22300 CE ABOVE 100\nSL 90", "test", None).is_none());

        // Ordinary replies are not mistaken for calls.
        assert!(!looks_like_buy_call("Move SL to 140"));
        assert!(!looks_like_buy_call("exit at 610"));
        assert!(!looks_like_buy_call("Book profit in 22300 CE"));
    }

    #[test]
    fn nonsense_ranges_are_refused() {
        let base = "BUY NIFTY 23000 PUTS";
        // Backwards.
        assert!(parse_signal(&format!("{base} 145-140 SL 120 TARGET 175"), "test", None).is_none());
        // Starts at/under the stop-loss.
        assert!(parse_signal(&format!("{base} 120-125 SL 120 TARGET 175"), "test", None).is_none());
        // Reaches target 1 (a typo like `140-1450`).
        assert!(parse_signal(&format!("{base} 140-1450 SL 120 TARGET 175"), "test", None).is_none());
        // Sane one still parses.
        assert!(parse_signal(&format!("{base} 140-145 SL 120 TARGET 175"), "test", None).is_some());
    }

    #[test]
    fn next_week_needs_a_weekly_series() {
        let text = "BUY BANKNIFTY NEXT WEEK 55000 STRIKE CALLS 300-310 SL 250 TARGET 380";
        assert!(parse_signal(text, "test", None).is_none());
        assert!(looks_like_buy_call(text));
    }

    #[test]
    fn next_week_is_the_expiry_in_next_calendar_week() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        let nifty = |now| next_week_expiry_with_now("NIFTY", now);
        // Monday, Wednesday and Sunday of the week of 19 Oct 2026 all mean 27 Oct.
        assert_eq!(nifty(d(2026, 10, 19)).as_deref(), Some("27-OCT-2026"));
        assert_eq!(nifty(d(2026, 10, 21)).as_deref(), Some("27-OCT-2026"));
        assert_eq!(nifty(d(2026, 10, 25)).as_deref(), Some("27-OCT-2026"));
        // On expiry day itself, next week is the following Tuesday.
        assert_eq!(nifty(d(2026, 10, 27)).as_deref(), Some("03-NOV-2026"));
        // Next week's Tuesday a holiday (10 Nov 2026) -> the Monday before.
        assert_eq!(nifty(d(2026, 11, 4)).as_deref(), Some("09-NOV-2026"));
        // SENSEX expires Thursdays.
        assert_eq!(next_week_expiry_with_now("SENSEX", d(2026, 10, 19)).as_deref(), Some("29-OCT-2026"));
        assert_eq!(next_week_expiry_with_now("BANKNIFTY", d(2026, 10, 19)), None);
    }

    #[test]
    fn an_expiry_on_a_holiday_moves_to_the_trading_day_before() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        let monthly = |index, month, now| resolve_expiry_date_with_now(None, month, index, now);
        let dated = |index, day, month, now| resolve_expiry_date_with_now(Some(day), month, index, now);

        // NSE monthlies on the last Tuesday: Tue 31 Mar 2026 is Mahavir Jayanti.
        for index in ["NIFTY", "BANKNIFTY", "FINNIFTY", "MIDCPNIFTY"] {
            assert_eq!(monthly(index, "MARCH", d(2026, 3, 2)).as_deref(), Some("30-MAR-2026"), "{index}");
        }
        // BSE on the last Thursday: Thu 26 Mar is Ram Navami, Thu 28 May Bakri Id.
        for index in ["SENSEX", "BANKEX"] {
            assert_eq!(monthly(index, "MARCH", d(2026, 3, 2)).as_deref(), Some("25-MAR-2026"), "{index}");
            assert_eq!(monthly(index, "MAY", d(2026, 5, 4)).as_deref(), Some("27-MAY-2026"), "{index}");
        }

        // Weeklies named by date: Holi (Tue 3 Mar), Dussehra (Tue 20 Oct),
        // Diwali Balipratipada (Tue 10 Nov) and Ram Navami (Thu 26 Mar).
        assert_eq!(dated("NIFTY", "3", "MARCH", d(2026, 3, 2)).as_deref(), Some("02-MAR-2026"));
        assert_eq!(dated("NIFTY", "20", "OCTOBER", d(2026, 10, 19)).as_deref(), Some("19-OCT-2026"));
        assert_eq!(dated("NIFTY", "10", "NOVEMBER", d(2026, 11, 4)).as_deref(), Some("09-NOV-2026"));
        assert_eq!(dated("SENSEX", "26", "MARCH", d(2026, 3, 2)).as_deref(), Some("25-MAR-2026"));

        // `NEXT WEEK` posted the week before Dussehra lands on Monday 19 Oct.
        assert_eq!(next_week_expiry_with_now("NIFTY", d(2026, 10, 12)).as_deref(), Some("19-OCT-2026"));
    }
}
