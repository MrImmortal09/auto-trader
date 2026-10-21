//! Every call format the channels post, parsed end to end, plus how the
//! Telegram event loop routes each message (trade / SL update / exit /
//! ignored). All dates resolve against a fixed day so the expected contracts
//! never drift with the calendar.
//!
//! When a channel starts posting a new shape, add it here first — and keep
//! the old shapes: they are the regression guard for every call already in
//! flight.

use chrono::NaiveDate;
use shared_domain::FeatureFlags;
use telegram_ingester::parser::{classify, classify_with, parse_signal_on, Inbound};

/// Monday 19 Oct 2026. This week's NIFTY expiry is pulled back to today
/// (Tue 20 Oct is a holiday); next week's is Tue 27 Oct, SENSEX Thu 29 Oct.
fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()
}

#[derive(Debug, PartialEq)]
struct Call {
    action: String,
    instrument: String,
    strike: Option<f64>,
    side: Option<String>,
    condition: String,
    entry: f64,
    entry_max: Option<f64>,
    targets: Vec<f64>,
    sl: f64,
    expiry: Option<String>,
}

fn parse(text: &str) -> Option<Call> {
    parse_signal_on(text, "test", None, today()).map(|s| Call {
        action: s.action,
        instrument: s.instrument_name,
        strike: s.strike,
        side: s.option_type,
        condition: s.entry_condition,
        entry: s.entry_price,
        entry_max: s.entry_price_max,
        targets: s.targets,
        sl: s.stop_loss,
        expiry: s.expiry,
    })
}

/// A classic `BUY X 425 CE ABOVE 8.25` call.
#[allow(clippy::too_many_arguments)]
fn trigger(
    action: &str,
    instrument: &str,
    strike: f64,
    side: &str,
    condition: &str,
    entry: f64,
    targets: &[f64],
    sl: f64,
    expiry: Option<&str>,
) -> Option<Call> {
    Some(Call {
        action: action.into(),
        instrument: instrument.into(),
        strike: Some(strike),
        side: Some(side.into()),
        condition: condition.into(),
        entry,
        entry_max: None,
        targets: targets.to_vec(),
        sl,
        expiry: expiry.map(Into::into),
    })
}

/// A `BUY X 23000 PUTS 140-145` range call: ABOVE the low, capped at the high.
#[allow(clippy::too_many_arguments)]
fn range(
    instrument: &str,
    strike: f64,
    side: &str,
    low: f64,
    high: f64,
    targets: &[f64],
    sl: f64,
    expiry: Option<&str>,
) -> Option<Call> {
    Some(Call {
        action: "BUY".into(),
        instrument: instrument.into(),
        strike: Some(strike),
        side: Some(side.into()),
        condition: "ABOVE".into(),
        entry: low,
        entry_max: Some(high),
        targets: targets.to_vec(),
        sl,
        expiry: expiry.map(Into::into),
    })
}

fn equity(instrument: &str, entry: f64, targets: &[f64], sl: f64) -> Option<Call> {
    Some(Call {
        action: "BUY".into(),
        instrument: instrument.into(),
        strike: None,
        side: None,
        condition: "ABOVE".into(),
        entry,
        entry_max: None,
        targets: targets.to_vec(),
        sl,
        expiry: None,
    })
}

fn check(cases: &[(&str, Option<Call>)]) {
    for (text, want) in cases {
        assert_eq!(&parse(text), want, "\n--- message ---\n{text}\n---------------");
    }
}

// ---------------------------------------------------------------------------
// Classic trigger calls — these must keep parsing exactly as they always have
// ---------------------------------------------------------------------------

#[test]
fn production_calls_parse() {
    // Verbatim from the production paper_trades table.
    check(&[
        (
            "BUY SENSEX 74400 CE ABOVE 620\n\nTARGET :- 670 / 900\n\nSL :- 500\n\n22 OCTOBER",
            trigger("BUY", "SENSEX", 74400.0, "CE", "ABOVE", 620.0, &[670.0, 900.0], 500.0, Some("22-OCT-2026")),
        ),
        (
            "BUY SENSEX 74400 PE ABOVE 600\n\nTARGET :- 650 / 750\n\nSL :- 500\n\n22 OCTOBER",
            trigger("BUY", "SENSEX", 74400.0, "PE", "ABOVE", 600.0, &[650.0, 750.0], 500.0, Some("22-OCT-2026")),
        ),
        (
            "BUY SENSEX 74500 CE ABOVE 550\n\nTARGET :- 600 / 700\n\nSL :- 450\n\n22 OCTOBER EXPIRY",
            trigger("BUY", "SENSEX", 74500.0, "CE", "ABOVE", 550.0, &[600.0, 700.0], 450.0, Some("22-OCT-2026")),
        ),
    ]);
}

#[test]
fn classic_trigger_calls_parse() {
    check(&[
        // Month-only on a stock: that month's monthly expiry, next year once passed.
        (
            "BUY BHEL 425 CE ABOVE 8.25\nTARGET :- 9.50 / 11.50\nSL :- 5\nJULY EXPIRY",
            trigger("BUY", "BHEL", 425.0, "CE", "ABOVE", 8.25, &[9.5, 11.5], 5.0, Some("29-JUL-2027")),
        ),
        (
            "BUY M&M 3200 CE ABOVE 45\nTARGET :- 55 / 65\nSL :- 38\nOCTOBER EXPIRY",
            trigger("BUY", "M&M", 3200.0, "CE", "ABOVE", 45.0, &[55.0, 65.0], 38.0, Some("29-OCT-2026")),
        ),
        // Day + month, `SL - x` separator.
        (
            "BUY BANKNIFTY 55400 CE ABOVE 690\nTARGET :- 750 / 850\nSL - 600\n26 NOV EXPIRY",
            trigger("BUY", "BANKNIFTY", 55400.0, "CE", "ABOVE", 690.0, &[750.0, 850.0], 600.0, Some("26-NOV-2026")),
        ),
        // Day + month with no EXPIRY word, already past this year -> next year.
        (
            "BUY NIFTY 24250 PE ABOVE 200\n\nTARGET :- 230 / 290\n\nSL :- 140\n\n21 JULY",
            trigger("BUY", "NIFTY", 24250.0, "PE", "ABOVE", 200.0, &[230.0, 290.0], 140.0, Some("21-JUL-2027")),
        ),
        // Ordinal days keep the day (weekly contract, not the monthly).
        (
            "BUY NIFTY 24600 PE ABOVE 200\n\nTARGET :- 230 / 270\n\nSL :- 160\n\n1ST SEPTEMBER EXPIRY",
            trigger("BUY", "NIFTY", 24600.0, "PE", "ABOVE", 200.0, &[230.0, 270.0], 160.0, Some("01-SEP-2027")),
        ),
        (
            "BUY SENSEX 82000 CE ABOVE 300\nTARGET :- 350 / 420\nSL :- 250\n1ST OCTOBER EXPIRY",
            trigger("BUY", "SENSEX", 82000.0, "CE", "ABOVE", 300.0, &[350.0, 420.0], 250.0, Some("01-OCT-2027")),
        ),
        // Hyphenated ticker.
        (
            "BUY BAJAJ-AUTO 11300 CE ABOVE 325\n\nTARGET :- 365 / 450\n\nSL :- 250\n\nAUGUST EXPIRY",
            trigger("BUY", "BAJAJ-AUTO", 11300.0, "CE", "ABOVE", 325.0, &[365.0, 450.0], 250.0, Some("26-AUG-2027")),
        ),
        // SELL / BELOW and `TARGET:` / `SL:` separators.
        (
            "SELL NIFTY 18500 PE BELOW 120\nTARGET: 80\nSL: 145\nAUG EXPIRY",
            trigger("SELL", "NIFTY", 18500.0, "PE", "BELOW", 120.0, &[80.0], 145.0, Some("31-AUG-2027")),
        ),
        // Lower case, decimals, no expiry (-> nearest at resolve time).
        (
            "buy nifty 25000 ce above 120.5\ntgt 140/160\nsl 100",
            trigger("BUY", "NIFTY", 25000.0, "CE", "ABOVE", 120.5, &[140.0, 160.0], 100.0, None),
        ),
        // Everything on one line.
        (
            "BUY NIFTY 25000 CE ABOVE 120 TARGET 140 SL 100",
            trigger("BUY", "NIFTY", 25000.0, "CE", "ABOVE", 120.0, &[140.0], 100.0, None),
        ),
        // Emoji prefix.
        (
            "✅ BUY NIFTY 25000 PE ABOVE 120\nTARGET :- 140\nSL :- 100",
            trigger("BUY", "NIFTY", 25000.0, "PE", "ABOVE", 120.0, &[140.0], 100.0, None),
        ),
        // `STOP LOSS` spelled out.
        (
            "BUY SENSEX 74400 CE ABOVE 620\nTARGET :- 670 / 900\nSTOP LOSS :- 500\n22 OCTOBER",
            trigger("BUY", "SENSEX", 74400.0, "CE", "ABOVE", 620.0, &[670.0, 900.0], 500.0, Some("22-OCT-2026")),
        ),
    ]);
}

#[test]
fn equity_calls_parse() {
    check(&[
        ("BUY RELIANCE ABOVE 2500\nTGT 2600 / 2700\nSL 2420", equity("RELIANCE", 2500.0, &[2600.0, 2700.0], 2420.0)),
        ("BUY TATAMOTORS ABOVE 950\nTARGET 980 / 1000\nS.L. 930", equity("TATAMOTORS", 950.0, &[980.0, 1000.0], 930.0)),
    ]);
}

// ---------------------------------------------------------------------------
// Range calls
// ---------------------------------------------------------------------------

#[test]
fn range_calls_parse() {
    check(&[
        // Verbatim from the channel.
        (
            "BUY NIFTY NEXT WEEK 23000 STRIKE PUTS 140-145 SL 120 TARGET 175",
            range("NIFTY", 23000.0, "PE", 140.0, 145.0, &[175.0], 120.0, Some("27-OCT-2026")),
        ),
        (
            "BUY NIFTY NEXT WEEK 23000 STRIKE PUTS @ 125-130 SL 105 TARGET 165",
            range("NIFTY", 23000.0, "PE", 125.0, 130.0, &[165.0], 105.0, Some("27-OCT-2026")),
        ),
        // The channel's `BUY AGAIN` shape, but naming the instrument.
        (
            "✅ BUY NIFTY 22300 CALLS CMP 100-102 SL 90 TARGET 125",
            range("NIFTY", 22300.0, "CE", 100.0, 102.0, &[125.0], 90.0, None),
        ),
        // SENSEX weeklies expire Thursday; singular CALL; `AT`; multi-line; two targets.
        (
            "BUY SENSEX NEXT WEEK 82000 CALL AT 250-260\nSL 200\nTARGET 300 / 350",
            range("SENSEX", 82000.0, "CE", 250.0, 260.0, &[300.0, 350.0], 200.0, Some("29-OCT-2026")),
        ),
        (
            "BUY NIFTY NEXT WEEK 23000 STRIKE PUTS 140-145\n\nSL 120\n\nTARGET 175 / 200",
            range("NIFTY", 23000.0, "PE", 140.0, 145.0, &[175.0, 200.0], 120.0, Some("27-OCT-2026")),
        ),
        // CE/PE with a range; en dash; decimals; lower case.
        (
            "BUY NIFTY 23000 PE 140–145 SL 120 TARGET 175",
            range("NIFTY", 23000.0, "PE", 140.0, 145.0, &[175.0], 120.0, None),
        ),
        (
            "BUY NIFTY 23000 PUTS 140.5-145.5 SL 120 TARGET 175",
            range("NIFTY", 23000.0, "PE", 140.5, 145.5, &[175.0], 120.0, None),
        ),
        (
            "buy nifty next week 23000 strike puts @ 125-130 sl 105 target 165",
            range("NIFTY", 23000.0, "PE", 125.0, 130.0, &[165.0], 105.0, Some("27-OCT-2026")),
        ),
        // An explicit date works with a range too.
        (
            "BUY NIFTY 23000 PUTS 140-145 SL 120 TARGET 175\n27 OCT EXPIRY",
            range("NIFTY", 23000.0, "PE", 140.0, 145.0, &[175.0], 120.0, Some("27-OCT-2026")),
        ),
        // No SL / target parses, as for classic calls — the engine then warns
        // that the position is unprotected.
        (
            "BUY NIFTY NEXT WEEK 23000 STRIKE PUTS 140-145",
            range("NIFTY", 23000.0, "PE", 140.0, 145.0, &[], 0.0, Some("27-OCT-2026")),
        ),
    ]);
}

// ---------------------------------------------------------------------------
// Refused — no trade may come out of these
// ---------------------------------------------------------------------------

#[test]
fn refused_calls_do_not_parse() {
    check(&[
        // No instrument named: never guess one.
        ("✅ BUY AGAIN 22300 CALLS CMP 100-102 SL 90 TARGET 125", None),
        ("BUY AGAIN 22300 CE ABOVE 100\nSL 90", None),
        // No weekly series to be "next week" of.
        ("BUY BANKNIFTY NEXT WEEK 55000 STRIKE CALLS 300-310 SL 250 TARGET 380", None),
        ("BUY RELIANCE NEXT WEEK 3000 CALLS 40-42 SL 30 TARGET 55", None),
        // Nonsense ranges: backwards, at/under the SL, reaching target 1.
        ("BUY NIFTY 23000 PUTS 145-140 SL 120 TARGET 175", None),
        ("BUY NIFTY 23000 PUTS 120-125 SL 120 TARGET 175", None),
        ("BUY NIFTY 23000 PUTS 140-1450 SL 120 TARGET 175", None),
        ("BUY NIFTY 23000 PUTS 140-175 SL 120 TARGET 175", None),
        // This account never writes options; ranges are BUY only.
        ("SELL NIFTY 23000 PUTS 140-145 SL 120 TARGET 175", None),
        // Chatter.
        ("Please Wait For Level \n\nBuy Only Above 550", None),
        ("Buy Now Above 550", None),
        ("Hello world!", None),
        (
            "155 - 218 HIT IN NIFTY PUT🎯\n\nEASY 60+ POINTS GAINS ✅😁\n\n 1ST TARGET ACHIEVED 🔥🔥\n\nGAINING RS 8,190/2 LOT 🚀🚀 \n\nBOOK PROFIT OR TRAIL SL 👍",
            None,
        ),
    ]);
}

// ---------------------------------------------------------------------------
// Routing: what the event loop does with each message
// ---------------------------------------------------------------------------

fn route(text: &str, reply_to: Option<&str>) -> String {
    describe(classify(text, "900", reply_to, today()))
}

/// [`route`] with `index_options_only` switched on (`/api/v2/flags`).
fn route_index_only(text: &str, reply_to: Option<&str>) -> String {
    describe(classify_with(text, "900", reply_to, today(), index_only()))
}

fn index_only() -> FeatureFlags {
    FeatureFlags { index_options_only: true, ..FeatureFlags::default() }
}

fn describe(inbound: Inbound) -> String {
    match inbound {
        Inbound::Signal(s) => format!(
            "signal {} {} #{}",
            s.instrument_name,
            s.strike.unwrap_or_default(),
            s.signal_id.unwrap_or_default()
        ),
        Inbound::NotIndexOption(s) => format!("skipped {}", s.instrument_name),
        Inbound::IgnoredCall => "ignored".into(),
        Inbound::SlUpdate { parent, new_sl } => format!("sl {new_sl} -> #{parent}"),
        Inbound::Exit { parent, price } => format!("exit {price} -> #{parent}"),
        Inbound::Unparsed => "unparsed".into(),
    }
}

#[test]
fn messages_route_to_the_right_action() {
    let parent = Some("500");
    let cases: &[(&str, Option<&str>, &str)] = &[
        // A full call is a trade even when posted as a reply — its `SL :- x`
        // line must not turn it into a stop move on the parent.
        ("BUY SENSEX 74400 CE ABOVE 620\n\nTARGET :- 670 / 900\n\nSL :- 500\n\n22 OCTOBER", None, "signal SENSEX 74400 #900"),
        ("BUY SENSEX 74400 CE ABOVE 620\n\nTARGET :- 670 / 900\n\nSL :- 500\n\n22 OCTOBER", parent, "signal SENSEX 74400 #900"),
        ("BUY NIFTY NEXT WEEK 23000 STRIKE PUTS 140-145 SL 120 TARGET 175", parent, "signal NIFTY 23000 #900"),
        // An unreadable BUY call is dropped — reply or not — and never
        // becomes an SL update on whatever it replies to.
        ("✅ BUY AGAIN 22300 CALLS CMP 100-102 SL 90 TARGET 125", parent, "ignored"),
        ("✅ BUY AGAIN 22300 CALLS CMP 100-102 SL 90 TARGET 125", None, "ignored"),
        ("Those who missed, buy 25000 CE again, SL 100", parent, "ignored"),
        ("BUY BANKNIFTY NEXT WEEK 55000 STRIKE CALLS 300-310 SL 250 TARGET 380", parent, "ignored"),
        // Reply commands act on the parent message's position.
        ("Move SL to 140", parent, "sl 140 -> #500"),
        ("SL :- 130", parent, "sl 130 -> #500"),
        ("Shift sl to 3", parent, "sl 3 -> #500"),
        ("Buy more if it dips, SL 95", parent, "sl 95 -> #500"),
        ("exit at 610", parent, "exit 610 -> #500"),
        ("exit all at 610", parent, "exit 610 -> #500"),
        ("EXIT AT 610.50", parent, "exit 610.5 -> #500"),
        // ...and only as replies: there is no parent to act on otherwise.
        ("Move SL to 140", None, "unparsed"),
        ("exit at 610", None, "unparsed"),
        // Chatter, reply or not.
        ("Book profit or trail SL 👍", parent, "unparsed"),
        ("Buy Only Above 550", parent, "unparsed"),
        (
            "155 - 218 HIT IN NIFTY PUT🎯\n\n 1ST TARGET ACHIEVED 🔥🔥\n\nBOOK PROFIT OR TRAIL SL 👍",
            parent,
            "unparsed",
        ),
    ];
    for (text, reply_to, want) in cases {
        assert_eq!(route(text, *reply_to), *want, "\n--- message (reply_to {reply_to:?}) ---\n{text}\n---");
    }
}

// ---------------------------------------------------------------------------
// Expiry calendar: NIFTY and SENSEX weeklies, the other four monthly, and a
// holiday pulls the expiry back to the trading day before
// ---------------------------------------------------------------------------

#[test]
fn expiry_follows_each_index_calendar() {
    let expiry = |text: &str| parse(text).and_then(|c| c.expiry);
    let call = |index: &str, when: &str| format!("BUY {index} 25000 CE ABOVE 120\nTARGET :- 140\nSL :- 100\n{when}");
    let cases: &[(&str, &str, &str)] = &[
        // Dussehra, Tue 20 Oct 2026, is a holiday: that week's NIFTY weekly
        // expired on Monday 19 Oct.
        ("NIFTY", "20 OCTOBER EXPIRY", "19-OCT-2026"),
        ("NIFTY", "27 OCTOBER EXPIRY", "27-OCT-2026"),
        // NSE monthlies: the last Tuesday, or the trading day before it.
        ("NIFTY", "OCTOBER EXPIRY", "27-OCT-2026"),
        ("BANKNIFTY", "OCTOBER EXPIRY", "27-OCT-2026"),
        ("FINNIFTY", "OCTOBER EXPIRY", "27-OCT-2026"),
        ("MIDCPNIFTY", "OCTOBER EXPIRY", "27-OCT-2026"),
        // Tue 24 Nov 2026 is Guru Nanak Jayanti.
        ("NIFTY", "NOVEMBER EXPIRY", "23-NOV-2026"),
        ("BANKNIFTY", "NOVEMBER EXPIRY", "23-NOV-2026"),
        ("FINNIFTY", "NOVEMBER EXPIRY", "23-NOV-2026"),
        ("MIDCPNIFTY", "NOVEMBER EXPIRY", "23-NOV-2026"),
        ("BANKNIFTY", "DECEMBER EXPIRY", "29-DEC-2026"),
        // BSE: SENSEX weeklies and both monthlies on Thursday.
        ("SENSEX", "22 OCTOBER", "22-OCT-2026"),
        ("SENSEX", "OCTOBER EXPIRY", "29-OCT-2026"),
        ("BANKEX", "OCTOBER EXPIRY", "29-OCT-2026"),
        ("BANKEX", "NOVEMBER EXPIRY", "26-NOV-2026"),
        ("BANKEX", "DECEMBER EXPIRY", "31-DEC-2026"),
    ];
    for (index, when, want) in cases {
        assert_eq!(expiry(&call(index, when)).as_deref(), Some(*want), "{index} {when}");
    }
}

// ---------------------------------------------------------------------------
// Routing with index_options_only on: only CE/PE calls on the six indices
// trade; every other call is skipped. Everything above holds with it off.
// ---------------------------------------------------------------------------

#[test]
fn with_index_options_only_every_other_call_is_skipped() {
    let parent = Some("500");
    let cases: &[(&str, Option<&str>, &str)] = &[
        // Index option calls trade exactly as with the switch off.
        ("BUY SENSEX 74400 CE ABOVE 620\n\nTARGET :- 670 / 900\n\nSL :- 500\n\n22 OCTOBER", None, "signal SENSEX 74400 #900"),
        ("BUY SENSEX 74400 CE ABOVE 620\n\nTARGET :- 670 / 900\n\nSL :- 500\n\n22 OCTOBER", parent, "signal SENSEX 74400 #900"),
        ("BUY NIFTY NEXT WEEK 23000 STRIKE PUTS 140-145 SL 120 TARGET 175", parent, "signal NIFTY 23000 #900"),
        ("BUY BANKNIFTY 55400 CE ABOVE 690\nTARGET :- 750 / 850\nSL - 600\n26 NOV EXPIRY", None, "signal BANKNIFTY 55400 #900"),
        ("buy nifty 25000 ce above 120.5\ntgt 140/160\nsl 100", None, "signal NIFTY 25000 #900"),
        ("BUY SENSEX NEXT WEEK 82000 CALL AT 250-260\nSL 200\nTARGET 300 / 350", None, "signal SENSEX 82000 #900"),
        ("BUY FINNIFTY 26000 PE ABOVE 80\nTARGET :- 100\nSL :- 60", None, "signal FINNIFTY 26000 #900"),
        ("BUY MIDCPNIFTY 12000 CE ABOVE 90\nTARGET :- 110\nSL :- 70", None, "signal MIDCPNIFTY 12000 #900"),
        ("BUY BANKEX 62000 CE ABOVE 300\nTARGET :- 350\nSL :- 250", None, "signal BANKEX 62000 #900"),
        // A SELL on an index option still reaches the engine, which drops SELL.
        ("SELL NIFTY 18500 PE BELOW 120\nTARGET: 80\nSL: 145\nAUG EXPIRY", None, "signal NIFTY 18500 #900"),
        // Stock options, equity and an index call with no strike are skipped,
        // and as a reply they never become a stop move on the parent.
        ("BUY BHEL 425 CE ABOVE 8.25\nTARGET :- 9.50 / 11.50\nSL :- 5\nJULY EXPIRY", None, "skipped BHEL"),
        ("BUY BHEL 425 CE ABOVE 8.25\nTARGET :- 9.50 / 11.50\nSL :- 5\nJULY EXPIRY", parent, "skipped BHEL"),
        ("BUY M&M 3200 CE ABOVE 45\nTARGET :- 55 / 65\nSL :- 38\nOCTOBER EXPIRY", None, "skipped M&M"),
        ("BUY BAJAJ-AUTO 11300 CE ABOVE 325\n\nTARGET :- 365 / 450\n\nSL :- 250\n\nAUGUST EXPIRY", None, "skipped BAJAJ-AUTO"),
        ("BUY RELIANCE ABOVE 2500\nTGT 2600 / 2700\nSL 2420", None, "skipped RELIANCE"),
        ("BUY TATAMOTORS ABOVE 950\nTARGET 980 / 1000\nS.L. 930", parent, "skipped TATAMOTORS"),
        ("BUY NIFTY ABOVE 25000\nTARGET 25100\nSL 24950", None, "skipped NIFTY"),
        // Exact names only — not an alias of NIFTY.
        ("BUY NIFTY50 25000 CE ABOVE 120\nTARGET :- 140\nSL :- 100", None, "skipped NIFTY50"),
        // Everything that isn't a complete call routes as with the switch off.
        ("✅ BUY AGAIN 22300 CALLS CMP 100-102 SL 90 TARGET 125", parent, "ignored"),
        ("BUY BANKNIFTY NEXT WEEK 55000 STRIKE CALLS 300-310 SL 250 TARGET 380", parent, "ignored"),
        ("BUY RELIANCE NEXT WEEK 3000 CALLS 40-42 SL 30 TARGET 55", None, "ignored"),
        ("Move SL to 140", parent, "sl 140 -> #500"),
        ("Buy more if it dips, SL 95", parent, "sl 95 -> #500"),
        ("exit at 610", parent, "exit 610 -> #500"),
        ("Move SL to 140", None, "unparsed"),
        ("Book profit or trail SL 👍", parent, "unparsed"),
    ];
    for (text, reply_to, want) in cases {
        assert_eq!(
            route_index_only(text, *reply_to),
            *want,
            "\n--- message (reply_to {reply_to:?}, index_options_only) ---\n{text}\n---"
        );
    }

    // The same skipped calls are trades with the switch off, as before.
    assert_eq!(route("BUY BHEL 425 CE ABOVE 8.25\nTARGET :- 9.50 / 11.50\nSL :- 5\nJULY EXPIRY", None), "signal BHEL 425 #900");
    assert_eq!(route("BUY RELIANCE ABOVE 2500\nTGT 2600 / 2700\nSL 2420", None), "signal RELIANCE 0 #900");
}

#[test]
fn with_index_options_only_the_dussehra_call_still_trades_the_monday_expiry() {
    let text = "BUY NIFTY 25000 CE ABOVE 120\nTARGET :- 140\nSL :- 100\n20 OCTOBER EXPIRY";
    match classify_with(text, "900", None, today(), index_only()) {
        Inbound::Signal(s) => assert_eq!(s.expiry.as_deref(), Some("19-OCT-2026")),
        other => panic!("expected a trade, got {}", describe(other)),
    }
}
