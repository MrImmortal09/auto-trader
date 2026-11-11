//! What a v2 chart can draw — an index or a stock, one of its futures, or
//! one of its option contracts — named in a form neither broker owns, and
//! found in each broker's contract list by what it is: underlying, expiry,
//! strike and side.
//!
//! The grammar: `NIFTY` or `RELIANCE` (the spot; `RELIANCE@BSE` names a BSE
//! listing, which is read but refused while the catalog is NSE-only),
//! `NIFTY:FUT` / `RELIANCE:FUT` (the nearest future still trading),
//! `NIFTY:FUT:2026-11-24` / `RELIANCE:FUT:2026-12-29` (the future expiring
//! that day, spelt as the candle archive names it — see
//! [`dated_future_name`]) and `NIFTY:2026-10-27:22750:CE` /
//! `RELIANCE:2026-11-24:1400:CE`. The six indices always parse, the dated
//! future included; any other name only when the caller's `known` check
//! accepts it, which the routes answer from the instrument catalog behind
//! the `stock_charts` switch.

use chrono::NaiveDate;
use shared_domain::INDEX_NAMES;
use trading_engine::{index_fo_segment, ScripRecord, ScripStore};
use upstox_client::Derivative;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Ce,
    Pe,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Ce => "CE",
            Side::Pe => "PE",
        }
    }
}

/// A stock's listing. The catalog is NSE-only (question 3), so a BSE
/// listing parses but is refused — see [`Symbol::refusal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exchange {
    Nse,
    Bse,
}

impl Exchange {
    pub fn as_str(self) -> &'static str {
        match self {
            Exchange::Nse => "NSE",
            Exchange::Bse => "BSE",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "NSE" => Some(Exchange::Nse),
            "BSE" => Some(Exchange::Bse),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Symbol {
    /// `NIFTY`: the index's spot value.
    Index(String),
    /// `RELIANCE` (NSE) or `RELIANCE@BSE`: a stock's cash price.
    Equity { symbol: String, exchange: Exchange },
    /// `NIFTY:FUT`, `RELIANCE:FUT` (`expiry: None`): the nearest future
    /// still trading. `NIFTY:FUT:2026-11-24`, `RELIANCE:FUT:2026-12-29`:
    /// the one expiring that day, and only that one — a later month's
    /// contract is never charted as the nearest.
    Future { underlying: String, expiry: Option<NaiveDate> },
    /// `NIFTY:2026-10-27:22750:CE`, `RELIANCE:2026-11-24:1400:CE`.
    Option { underlying: String, expiry: NaiveDate, strike: f64, side: Side },
}

/// An underlying's name as the scrip master spells it: letters, digits and
/// the `&`, `-` and `_` some stocks carry (`M&M`, `BAJAJ-AUTO`).
fn is_ticker(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || matches!(c, '&' | '-' | '_'))
}

impl Symbol {
    /// The six indices' forms only — what every chart could name before the
    /// catalog existed, and all the routes accept while the `stock_charts`
    /// switch is off.
    pub fn parse(raw: &str) -> Option<Symbol> {
        Self::parse_with(raw, &|_| false)
    }

    /// Reads `raw`; a name that is not one of the six indices is taken as a
    /// stock only when `known` says the catalog lists it.
    pub fn parse_with(raw: &str, known: &dyn Fn(&str) -> bool) -> Option<Symbol> {
        let upper = raw.trim().to_ascii_uppercase();
        let mut parts = upper.split(':');
        let first = parts.next()?;
        let rest = (parts.next(), parts.next(), parts.next(), parts.next());

        // `NAME@EXCHANGE` names a listing: a stock's spot only.
        let (name, exchange) = match first.split_once('@') {
            Some((name, exchange)) => (name, Some(Exchange::parse(exchange)?)),
            None => (first, None),
        };
        let underlying = if INDEX_NAMES.contains(&name) {
            if exchange.is_some() {
                return None;
            }
            Underlying::Index(name.to_string())
        } else if is_ticker(name) && known(name) {
            Underlying::Stock(name.to_string())
        } else {
            return None;
        };

        match (underlying, rest) {
            (Underlying::Index(index), (None, ..)) => Some(Symbol::Index(index)),
            (Underlying::Stock(symbol), (None, ..)) => Some(Symbol::Equity { symbol, exchange: exchange.unwrap_or(Exchange::Nse) }),
            (_, (Some(_), ..)) if exchange.is_some() => None,
            (u, (Some("FUT"), None, ..)) => Some(Symbol::Future { underlying: u.into_name(), expiry: None }),
            (u, (Some("FUT"), Some(expiry), None, ..)) => {
                let expiry = NaiveDate::parse_from_str(expiry, "%Y-%m-%d").ok()?;
                Some(Symbol::Future { underlying: u.into_name(), expiry: Some(expiry) })
            }
            (u, (Some(expiry), Some(strike), Some(side), None)) => {
                let expiry = NaiveDate::parse_from_str(expiry, "%Y-%m-%d").ok()?;
                let strike = strike.parse::<f64>().ok().filter(|s| s.is_finite() && *s > 0.0)?;
                let side = match side {
                    "CE" => Side::Ce,
                    "PE" => Side::Pe,
                    _ => return None,
                };
                Some(Symbol::Option { underlying: u.into_name(), expiry, strike, side })
            }
            _ => None,
        }
    }

    /// The canonical spelling, which [`Symbol::parse_with`] reads back. An
    /// NSE listing is spelt bare (`RELIANCE`), so saved layouts and chart
    /// names never change when the exchange is the default.
    pub fn name(&self) -> String {
        match self {
            Symbol::Index(index) => index.to_string(),
            Symbol::Equity { symbol, exchange: Exchange::Nse } => symbol.to_string(),
            Symbol::Equity { symbol, exchange } => format!("{symbol}@{}", exchange.as_str()),
            Symbol::Future { underlying, expiry: None } => format!("{underlying}:FUT"),
            Symbol::Future { underlying, expiry: Some(expiry) } => dated_future_name(underlying, *expiry),
            Symbol::Option { underlying, expiry, strike, side } => {
                format!("{underlying}:{}:{strike}:{}", expiry.format("%Y-%m-%d"), side.as_str())
            }
        }
    }

    /// The index or stock this is, or is a contract on.
    pub fn underlying(&self) -> &str {
        match self {
            Symbol::Index(u) | Symbol::Future { underlying: u, .. } | Symbol::Option { underlying: u, .. } => u,
            Symbol::Equity { symbol, .. } => symbol,
        }
    }

    /// Whether the underlying is one of the six indices.
    pub fn is_index(&self) -> bool {
        INDEX_NAMES.contains(&self.underlying())
    }

    /// Why this symbol cannot be charted on `today` (IST) although it
    /// parsed: a listing the catalog does not carry, or a dated future that
    /// has expired (it traded until its expiry day's close; neither broker
    /// lists it any more, and the archive keeps its minutes under the same
    /// name). `None` when it can.
    pub fn refusal(&self, today: NaiveDate) -> Option<String> {
        match self {
            Symbol::Equity { symbol, exchange: Exchange::Bse } => {
                Some(format!("{symbol}@BSE: only NSE listings are charted (the catalog is NSE-only)"))
            }
            Symbol::Future { expiry: Some(expiry), .. } if *expiry < today => Some(format!(
                "{}: this future expired on {}; only a future still trading is charted",
                self.name(),
                expiry.format("%Y-%m-%d")
            )),
            _ => None,
        }
    }
}

/// The one spelling of a dated future, `NIFTY:FUT:2026-11-24`: what
/// [`Symbol::name`] gives a [`Symbol::Future`] with an expiry, and the name
/// the candle archive keeps that contract's minutes under
/// (`candles::archive`). The live service's minutes of the day go to the
/// `candles` table and the archive's to `candle_archive`, so the two never
/// write over each other under the shared name.
pub fn dated_future_name(underlying: &str, expiry: NaiveDate) -> String {
    format!("{underlying}:FUT:{}", expiry.format("%Y-%m-%d"))
}

/// What the first segment of a symbol named.
enum Underlying {
    Index(String),
    Stock(String),
}

impl Underlying {
    fn into_name(self) -> String {
        match self {
            Underlying::Index(n) | Underlying::Stock(n) => n,
        }
    }
}

/// The F&O segment an underlying's contracts trade in: SENSEX and BANKEX in
/// `bse_fo`, every other index and every stock in `nse_fo`.
pub fn fo_segment(underlying: &str) -> &'static str {
    index_fo_segment(underlying)
}

/// Upstox's key for an index's spot value.
pub fn upstox_index_key(index: &str) -> Option<&'static str> {
    Some(match index {
        "NIFTY" => "NSE_INDEX|Nifty 50",
        "BANKNIFTY" => "NSE_INDEX|Nifty Bank",
        "FINNIFTY" => "NSE_INDEX|Nifty Fin Service",
        "MIDCPNIFTY" => "NSE_INDEX|NIFTY MID SELECT",
        "SENSEX" => "BSE_INDEX|SENSEX",
        "BANKEX" => "BSE_INDEX|BANKEX",
        _ => return None,
    })
}

/// Strikes match the same way in both brokers' lists.
fn same_strike(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

/// The contract `symbol` names among Upstox's derivatives on `today`: a
/// dated future by that exact expiry (never another month's), the bare one
/// as the nearest still trading; neither once expired.
pub fn find_upstox<'a>(symbol: &Symbol, derivatives: &'a [Derivative], today: NaiveDate) -> Option<&'a Derivative> {
    match symbol {
        Symbol::Index(_) | Symbol::Equity { .. } => None,
        Symbol::Future { underlying, expiry } => derivatives
            .iter()
            .filter(|d| d.underlying == *underlying && d.kind == "FUT" && d.expiry >= today)
            .filter(|d| expiry.is_none_or(|e| d.expiry == e))
            .min_by_key(|d| d.expiry),
        Symbol::Option { underlying, expiry, strike, side } => derivatives.iter().find(|d| {
            d.underlying == *underlying && d.kind == side.as_str() && d.expiry == *expiry && same_strike(d.strike, *strike)
        }),
    }
}

/// The contract `symbol` names in Kotak's scrip master on `today`, in the
/// underlying's own F&O segment: a dated future by that exact expiry, the
/// bare one as the nearest still trading; neither once expired.
pub fn find_kotak<'a>(symbol: &Symbol, store: &'a ScripStore, today: NaiveDate) -> Option<&'a ScripRecord> {
    let underlying = symbol.underlying();
    let segment = fo_segment(underlying);
    let records = store.records.get(underlying)?.iter().filter(move |r| r.exchange_segment_code == segment);
    match symbol {
        Symbol::Index(_) | Symbol::Equity { .. } => None,
        Symbol::Future { expiry, .. } => records
            .filter(|r| r.trading_symbol.trim().ends_with("FUT") && r.expiry_date >= today)
            .filter(|r| expiry.is_none_or(|e| r.expiry_date == e))
            .min_by_key(|r| r.expiry_date),
        Symbol::Option { expiry, strike, side, .. } => {
            let mut matches = records.filter(|r| r.option_type == side.as_str() && r.expiry_date == *expiry);
            matches.find(|r| r.normalized_strike().is_some_and(|s| same_strike(s, *strike)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, d).unwrap()
    }

    fn index(name: &str) -> Symbol {
        Symbol::Index(name.into())
    }

    fn future(name: &str) -> Symbol {
        Symbol::Future { underlying: name.into(), expiry: None }
    }

    fn future_at(name: &str, expiry: NaiveDate) -> Symbol {
        Symbol::Future { underlying: name.into(), expiry: Some(expiry) }
    }

    fn option(underlying: &str, expiry: NaiveDate, strike: f64, side: Side) -> Symbol {
        Symbol::Option { underlying: underlying.into(), expiry, strike, side }
    }

    /// The catalog of the tests: three stocks.
    fn catalog(name: &str) -> bool {
        matches!(name, "RELIANCE" | "M&M" | "BAJAJ-AUTO")
    }

    #[test]
    fn symbols_parse_and_read_back() {
        let call = option("NIFTY", date(10, 27), 22750.0, Side::Ce);
        assert_eq!(Symbol::parse("NIFTY"), Some(index("NIFTY")));
        assert_eq!(Symbol::parse(" sensex:fut "), Some(future("SENSEX")));
        assert_eq!(Symbol::parse("nifty:2026-10-27:22750:ce"), Some(call.clone()));
        assert_eq!(call.name(), "NIFTY:2026-10-27:22750:CE");
        assert_eq!(Symbol::parse(&call.name()), Some(call));
        assert_eq!(Symbol::parse("MIDCPNIFTY:2026-10-27:12437.5:PE").unwrap().name(), "MIDCPNIFTY:2026-10-27:12437.5:PE");

        for bad in [
            "", "RELIANCE", "NIFTY50", "NIFTY:FUTURE", "NIFTY:FUT:X", "NIFTY:2026-13-01:22750:CE",
            "NIFTY:2026-10-27:0:CE", "NIFTY:2026-10-27:22750:XX", "NIFTY:2026-10-27:22750", "NIFTY:2026-10-27:22750:CE:1",
            "NIFTY@NSE", "NIFTY@BSE",
        ] {
            assert_eq!(Symbol::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn stocks_parse_only_when_the_catalog_knows_them() {
        let nse = Symbol::Equity { symbol: "RELIANCE".into(), exchange: Exchange::Nse };
        let bse = Symbol::Equity { symbol: "RELIANCE".into(), exchange: Exchange::Bse };
        assert_eq!(Symbol::parse_with("reliance", &catalog), Some(nse.clone()));
        assert_eq!(Symbol::parse_with("RELIANCE@NSE", &catalog), Some(nse.clone()));
        assert_eq!(nse.name(), "RELIANCE", "the default listing is spelt bare");
        assert_eq!(Symbol::parse_with("RELIANCE@BSE", &catalog), Some(bse.clone()));
        assert_eq!(bse.name(), "RELIANCE@BSE");
        assert_eq!(Symbol::parse_with(&bse.name(), &catalog), Some(bse.clone()));
        // Parsed, but refused: the catalog is NSE-only.
        let today = date(11, 11);
        assert!(nse.refusal(today).is_none());
        assert!(bse.refusal(today).unwrap().contains("NSE"));
        assert!(index("NIFTY").refusal(today).is_none());

        assert_eq!(Symbol::parse_with("RELIANCE:FUT", &catalog), Some(future("RELIANCE")));
        let call = option("RELIANCE", date(11, 24), 1400.0, Side::Ce);
        assert_eq!(Symbol::parse_with("reliance:2026-11-24:1400:ce", &catalog), Some(call.clone()));
        assert_eq!(call.name(), "RELIANCE:2026-11-24:1400:CE");
        assert_eq!(Symbol::parse_with(&call.name(), &catalog), Some(call.clone()));
        assert_eq!((call.underlying(), call.is_index()), ("RELIANCE", false));
        assert!(index("BANKNIFTY").is_index());
        // Names with the characters stocks carry.
        assert_eq!(Symbol::parse_with("m&m", &catalog).unwrap().name(), "M&M");
        assert_eq!(Symbol::parse_with("BAJAJ-AUTO:FUT", &catalog), Some(future("BAJAJ-AUTO")));
        // The indices never need the catalog, and parse the same with one.
        assert_eq!(Symbol::parse_with("NIFTY:FUT", &catalog), Some(future("NIFTY")));

        for bad in [
            "TCS", "TCS:FUT", "RELIANCE@", "RELIANCE@MCX", "RELIANCE@BSE:FUT", "RELIANCE@NSE:2026-11-24:1400:CE",
            "RELIANCE:FUT:X", "RELIANCE:2026-11-24:1400", "RELI ANCE", "RELIANCE.NS", "@BSE",
        ] {
            assert_eq!(Symbol::parse_with(bad, &catalog), None, "{bad}");
        }
        // Without the catalog, a stock is still not a symbol.
        assert_eq!(Symbol::parse("RELIANCE:FUT"), None);
        assert_eq!(Symbol::parse("RELIANCE@BSE"), None);
    }

    #[test]
    fn a_future_is_named_by_its_expiry_beside_the_nearest_one() {
        let (nov, dec) = (date(11, 24), date(12, 29));
        // An index's dated future is in the index-only grammar: it needs no
        // catalog and no `stock_charts`.
        assert_eq!(Symbol::parse("NIFTY:FUT:2026-11-24"), Some(future_at("NIFTY", nov)));
        assert_eq!(Symbol::parse(" nifty:fut:2026-11-24 "), Some(future_at("NIFTY", nov)));
        assert_eq!(Symbol::parse_with("NIFTY:FUT:2026-11-24", &catalog), Some(future_at("NIFTY", nov)));
        // The canonical echo keeps the date, and reads back.
        assert_eq!(future_at("NIFTY", nov).name(), "NIFTY:FUT:2026-11-24");
        let sensex = future_at("SENSEX", date(11, 26));
        assert_eq!(Symbol::parse(&sensex.name()), Some(sensex.clone()));
        assert_eq!((sensex.underlying(), sensex.is_index()), ("SENSEX", true));
        // The bare form keeps meaning the nearest future, a different name.
        assert_eq!(Symbol::parse("NIFTY:FUT"), Some(future("NIFTY")));
        assert_eq!(future("NIFTY").name(), "NIFTY:FUT");
        assert_ne!(future("NIFTY").name(), future_at("NIFTY", nov).name());

        // A stock's, only when the catalog knows the stock (behind `stock_charts`).
        let reliance = future_at("RELIANCE", dec);
        assert_eq!(Symbol::parse_with("reliance:fut:2026-12-29", &catalog), Some(reliance.clone()));
        assert_eq!(reliance.name(), "RELIANCE:FUT:2026-12-29");
        assert_eq!(Symbol::parse_with(&reliance.name(), &catalog), Some(reliance.clone()));
        assert_eq!(Symbol::parse_with("BAJAJ-AUTO:FUT:2026-12-29", &catalog).unwrap().name(), "BAJAJ-AUTO:FUT:2026-12-29");
        assert_eq!(Symbol::parse("RELIANCE:FUT:2026-12-29"), None, "no catalog: no stock");

        for bad in [
            "NIFTY:FUT:", "NIFTY:FUT:2026-13-01", "NIFTY:FUT:2026-11-31", "NIFTY:FUT:24-NOV-2026", "NIFTY:FUT:2026-11-24:CE",
            "NIFTY:FUT:2026-11-24:1", "NIFTY@NSE:FUT:2026-11-24", "RELIANCE@BSE:FUT:2026-12-29", "TCS:FUT:2026-12-29",
        ] {
            assert_eq!(Symbol::parse_with(bad, &catalog), None, "{bad}");
        }

        // The archive keeps a future's minutes under this same spelling.
        assert_eq!(dated_future_name("NIFTY", nov), future_at("NIFTY", nov).name());
        assert_eq!(Symbol::parse(&dated_future_name("BANKNIFTY", nov)), Some(future_at("BANKNIFTY", nov)));
    }

    #[test]
    fn an_expired_dated_future_is_refused() {
        let nov = date(11, 24);
        // It trades until its expiry day's close; from the next day, refused.
        assert!(future_at("NIFTY", nov).refusal(date(11, 20)).is_none());
        assert!(future_at("NIFTY", nov).refusal(nov).is_none());
        let why = future_at("NIFTY", nov).refusal(date(11, 25)).unwrap();
        assert!(why.contains("NIFTY:FUT:2026-11-24") && why.contains("expired on 2026-11-24"), "{why}");
        assert!(future_at("RELIANCE", nov).refusal(date(12, 1)).is_some());
        // The bare form never expires: it rolls to the next month.
        assert!(future("NIFTY").refusal(date(11, 25)).is_none());
        // An option is left to the brokers' lists, as before (a 404).
        assert!(option("NIFTY", date(10, 27), 22750.0, Side::Ce).refusal(date(11, 25)).is_none());
    }

    #[test]
    fn the_fo_segment_follows_the_underlying() {
        assert_eq!(fo_segment("SENSEX"), "bse_fo");
        assert_eq!(fo_segment("BANKEX"), "bse_fo");
        assert_eq!(fo_segment("NIFTY"), "nse_fo");
        assert_eq!(fo_segment("RELIANCE"), "nse_fo");
        assert_eq!(fo_segment("M&M"), "nse_fo");
    }

    fn derivative(underlying: &str, kind: &str, expiry: NaiveDate, strike: f64, key: &str) -> Derivative {
        Derivative {
            underlying: underlying.into(),
            kind: kind.into(),
            expiry,
            strike,
            instrument_key: key.into(),
            lot_size: 65,
        }
    }

    #[test]
    fn upstox_contracts_are_found_by_what_they_are() {
        let list = vec![
            derivative("NIFTY", "FUT", date(11, 23), 0.0, "NSE_FO|2"),
            derivative("NIFTY", "FUT", date(10, 27), 0.0, "NSE_FO|48704"),
            derivative("NIFTY", "CE", date(10, 27), 22750.0, "NSE_FO|51342"),
            derivative("NIFTY", "PE", date(10, 27), 22750.0, "NSE_FO|51343"),
            derivative("BANKNIFTY", "CE", date(10, 27), 22750.0, "NSE_FO|9"),
            derivative("RELIANCE", "FUT", date(11, 24), 0.0, "NSE_FO|35001"),
            derivative("RELIANCE", "CE", date(11, 24), 1400.0, "NSE_FO|35002"),
        ];
        let today = date(10, 21);
        assert_eq!(find_upstox(&future("NIFTY"), &list, today).unwrap().instrument_key, "NSE_FO|48704");
        // The day after the October future expires, it rolls to November.
        assert_eq!(find_upstox(&future("NIFTY"), &list, date(10, 28)).unwrap().instrument_key, "NSE_FO|2");
        let put = option("NIFTY", date(10, 27), 22750.0, Side::Pe);
        assert_eq!(find_upstox(&put, &list, today).unwrap().instrument_key, "NSE_FO|51343");
        let missing = option("NIFTY", date(11, 3), 22750.0, Side::Pe);
        assert!(find_upstox(&missing, &list, today).is_none());
        assert!(find_upstox(&index("NIFTY"), &list, today).is_none());
        assert_eq!(upstox_index_key("MIDCPNIFTY"), Some("NSE_INDEX|NIFTY MID SELECT"));
        // A stock's contracts, by the same rules.
        assert_eq!(find_upstox(&future("RELIANCE"), &list, today).unwrap().instrument_key, "NSE_FO|35001");
        let call = option("RELIANCE", date(11, 24), 1400.0, Side::Ce);
        assert_eq!(find_upstox(&call, &list, today).unwrap().instrument_key, "NSE_FO|35002");
        assert!(find_upstox(&option("RELIANCE", date(11, 24), 1400.0, Side::Pe), &list, today).is_none());
        let equity = Symbol::Equity { symbol: "RELIANCE".into(), exchange: Exchange::Nse };
        assert!(find_upstox(&equity, &list, today).is_none(), "an equity is not a derivative");

        // A dated future is that contract, never the nearest one instead.
        assert_eq!(find_upstox(&future_at("NIFTY", date(11, 23)), &list, today).unwrap().instrument_key, "NSE_FO|2");
        assert_eq!(find_upstox(&future_at("NIFTY", date(10, 27)), &list, today).unwrap().instrument_key, "NSE_FO|48704");
        assert_eq!(find_upstox(&future_at("RELIANCE", date(11, 24)), &list, today).unwrap().instrument_key, "NSE_FO|35001");
        // A date with no listed future, or an option's expiry, finds nothing.
        assert!(find_upstox(&future_at("NIFTY", date(12, 29)), &list, today).is_none());
        assert!(find_upstox(&future_at("BANKNIFTY", date(10, 27)), &list, today).is_none(), "BANKNIFTY lists only an option that day");
        // One past its expiry is not found, even while a stale list holds it.
        assert!(find_upstox(&future_at("NIFTY", date(10, 27)), &list, date(10, 28)).is_none());
    }

    fn record(symbol: &str, segment: &str, raw_strike: f64, opt: &str, expiry: NaiveDate, token: &str) -> ScripRecord {
        ScripRecord {
            instrument_token: token.into(),
            trading_symbol: symbol.into(),
            symbol_name: symbol.chars().take_while(|c| c.is_ascii_alphabetic() || *c == '&' || *c == '-').collect(),
            exchange_segment_code: segment.into(),
            strike_price: raw_strike,
            option_type: opt.into(),
            expiry_date: expiry,
            lot_size: 65,
            tick_size: 0.05,
        }
    }

    #[test]
    fn kotak_contracts_are_found_by_what_they_are() {
        let mut store = ScripStore::default();
        store.records.insert("NIFTY".into(), vec![
            record("NIFTY26OCTFUT", "nse_fo", 0.0, "XX", date(10, 27), "48704"),
            record("NIFTY26NOVFUT", "nse_fo", 0.0, "XX", date(11, 23), "48705"),
            record("NIFTY26O2722750CE", "nse_fo", 2275000.0, "CE", date(10, 27), "51342"),
            record("NIFTY26N0322750CE", "nse_fo", 2275000.0, "CE", date(11, 3), "51500"),
        ]);
        let today = date(10, 21);
        assert_eq!(find_kotak(&future("NIFTY"), &store, today).unwrap().instrument_token, "48704");
        let call = option("NIFTY", date(10, 27), 22750.0, Side::Ce);
        assert_eq!(find_kotak(&call, &store, today).unwrap().ws_key(), "nse_fo|51342");
        let other_strike = option("NIFTY", date(10, 27), 22800.0, Side::Ce);
        assert!(find_kotak(&other_strike, &store, today).is_none());
        assert!(find_kotak(&future("SENSEX"), &store, today).is_none());

        // A dated future is that contract: November's by its date, though
        // October's is the nearest.
        assert_eq!(find_kotak(&future_at("NIFTY", date(11, 23)), &store, today).unwrap().instrument_token, "48705");
        assert_eq!(find_kotak(&future_at("NIFTY", date(10, 27)), &store, today).unwrap().instrument_token, "48704");
        // An option's expiry or a date nothing expires on names no future.
        assert!(find_kotak(&future_at("NIFTY", date(11, 3)), &store, today).is_none());
        assert!(find_kotak(&future_at("NIFTY", date(12, 29)), &store, today).is_none());
        // Past its expiry it is gone, even while the scrip master still lists it.
        assert!(find_kotak(&future_at("NIFTY", date(10, 27)), &store, date(10, 28)).is_none());
    }

    #[test]
    fn a_stocks_kotak_contracts_come_from_nse_fo_only() {
        let mut store = ScripStore::default();
        store.records.insert("RELIANCE".into(), vec![
            // Rows as the real nse_fo file lists them (strike in paise).
            record("RELIANCE26NOVFUT", "nse_fo", 0.0, "XX", date(11, 24), "43001"),
            record("RELIANCE26DECFUT", "nse_fo", 0.0, "XX", date(12, 29), "43002"),
            record("RELIANCE26NOV1400CE", "nse_fo", 140000.0, "CE", date(11, 24), "41222"),
            record("RELIANCE26NOV1400PE", "nse_fo", 140000.0, "PE", date(11, 24), "41223"),
            record("RELIANCE26DEC1400CE", "nse_fo", 140000.0, "CE", date(12, 29), "41229"),
            // The same contract on BSE: never a stock's, the catalog is NSE-only.
            record("RELIANCE26NOV1400CE", "bse_fo", 140000.0, "CE", date(11, 24), "876001"),
        ]);
        store.records.insert("SENSEX".into(), vec![
            record("SENSEX26NOVFUT", "bse_fo", 0.0, "XX", date(11, 26), "874003"),
            record("SENSEX26N0575100CE", "bse_fo", 7510000.0, "CE", date(11, 5), "874001"),
        ]);
        let today = date(11, 4);
        assert_eq!(find_kotak(&future("RELIANCE"), &store, today).unwrap().ws_key(), "nse_fo|43001");
        // On its expiry day the November future still trades; the day after, December's.
        assert_eq!(find_kotak(&future("RELIANCE"), &store, date(11, 24)).unwrap().ws_key(), "nse_fo|43001");
        assert_eq!(find_kotak(&future("RELIANCE"), &store, date(11, 25)).unwrap().ws_key(), "nse_fo|43002");
        // December's by its date, in nse_fo, while November's is the nearest.
        assert_eq!(find_kotak(&future_at("RELIANCE", date(12, 29)), &store, today).unwrap().ws_key(), "nse_fo|43002");
        assert_eq!(find_kotak(&future_at("RELIANCE", date(11, 24)), &store, today).unwrap().ws_key(), "nse_fo|43001");
        assert!(find_kotak(&future_at("RELIANCE", date(1, 26)), &store, today).is_none());
        assert_eq!(find_kotak(&future_at("SENSEX", date(11, 26)), &store, today).unwrap().ws_key(), "bse_fo|874003");
        let call = option("RELIANCE", date(11, 24), 1400.0, Side::Ce);
        assert_eq!(find_kotak(&call, &store, today).unwrap().ws_key(), "nse_fo|41222");
        let put = option("RELIANCE", date(11, 24), 1400.0, Side::Pe);
        assert_eq!(find_kotak(&put, &store, today).unwrap().ws_key(), "nse_fo|41223");
        assert!(find_kotak(&option("RELIANCE", date(11, 24), 1420.0, Side::Ce), &store, today).is_none());
        assert!(find_kotak(&option("RELIANCE", date(11, 25), 1400.0, Side::Ce), &store, today).is_none());
        // SENSEX stays in bse_fo, by the same segment rule.
        assert_eq!(find_kotak(&future("SENSEX"), &store, today).unwrap().ws_key(), "bse_fo|874003");
        assert_eq!(find_kotak(&option("SENSEX", date(11, 5), 75100.0, Side::Ce), &store, today).unwrap().ws_key(), "bse_fo|874001");
        // An equity is not in the F&O lists; an unknown stock has no records.
        let equity = Symbol::Equity { symbol: "RELIANCE".into(), exchange: Exchange::Nse };
        assert!(find_kotak(&equity, &store, today).is_none());
        assert!(find_kotak(&future("TCS"), &store, today).is_none());
    }
}
