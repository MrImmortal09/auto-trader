//! What a v2 chart can draw — an index, its nearest future, or one of its
//! option contracts — named in a form neither broker owns, and found in each
//! broker's contract list by what it is: underlying, expiry, strike and side.

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

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Symbol {
    /// `NIFTY`: the index's spot value.
    Index(&'static str),
    /// `NIFTY:FUT`: the nearest future still trading.
    Future(&'static str),
    /// `NIFTY:2026-10-27:22750:CE`.
    Option { index: &'static str, expiry: NaiveDate, strike: f64, side: Side },
}

impl Symbol {
    pub fn parse(raw: &str) -> Option<Symbol> {
        let upper = raw.trim().to_ascii_uppercase();
        let mut parts = upper.split(':');
        let name = parts.next()?;
        let index = INDEX_NAMES.iter().copied().find(|i| *i == name)?;
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (None, ..) => Some(Symbol::Index(index)),
            (Some("FUT"), None, ..) => Some(Symbol::Future(index)),
            (Some(expiry), Some(strike), Some(side), None) => {
                let expiry = NaiveDate::parse_from_str(expiry, "%Y-%m-%d").ok()?;
                let strike = strike.parse::<f64>().ok().filter(|s| s.is_finite() && *s > 0.0)?;
                let side = match side {
                    "CE" => Side::Ce,
                    "PE" => Side::Pe,
                    _ => return None,
                };
                Some(Symbol::Option { index, expiry, strike, side })
            }
            _ => None,
        }
    }

    /// The canonical spelling, which [`Symbol::parse`] reads back.
    pub fn name(&self) -> String {
        match self {
            Symbol::Index(index) => index.to_string(),
            Symbol::Future(index) => format!("{index}:FUT"),
            Symbol::Option { index, expiry, strike, side } => {
                format!("{index}:{}:{strike}:{}", expiry.format("%Y-%m-%d"), side.as_str())
            }
        }
    }

    pub fn index(&self) -> &'static str {
        match self {
            Symbol::Index(i) | Symbol::Future(i) | Symbol::Option { index: i, .. } => i,
        }
    }
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

/// The contract `symbol` names among Upstox's index derivatives on `today`.
pub fn find_upstox<'a>(symbol: &Symbol, derivatives: &'a [Derivative], today: NaiveDate) -> Option<&'a Derivative> {
    match *symbol {
        Symbol::Index(_) => None,
        Symbol::Future(index) => derivatives
            .iter()
            .filter(|d| d.underlying == index && d.kind == "FUT" && d.expiry >= today)
            .min_by_key(|d| d.expiry),
        Symbol::Option { index, expiry, strike, side } => derivatives.iter().find(|d| {
            d.underlying == index && d.kind == side.as_str() && d.expiry == expiry && same_strike(d.strike, strike)
        }),
    }
}

/// The contract `symbol` names in Kotak's scrip master on `today`.
pub fn find_kotak<'a>(symbol: &Symbol, store: &'a ScripStore, today: NaiveDate) -> Option<&'a ScripRecord> {
    let index = symbol.index();
    let segment = index_fo_segment(index);
    let records = store.records.get(index)?.iter().filter(move |r| r.exchange_segment_code == segment);
    match *symbol {
        Symbol::Index(_) => None,
        Symbol::Future(_) => records
            .filter(|r| r.trading_symbol.trim().ends_with("FUT") && r.expiry_date >= today)
            .min_by_key(|r| r.expiry_date),
        Symbol::Option { expiry, strike, side, .. } => {
            let mut matches = records.filter(|r| r.option_type == side.as_str() && r.expiry_date == expiry);
            matches.find(|r| r.normalized_strike().is_some_and(|s| same_strike(s, strike)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, d).unwrap()
    }

    #[test]
    fn symbols_parse_and_read_back() {
        let option = Symbol::Option { index: "NIFTY", expiry: date(10, 27), strike: 22750.0, side: Side::Ce };
        assert_eq!(Symbol::parse("NIFTY"), Some(Symbol::Index("NIFTY")));
        assert_eq!(Symbol::parse(" sensex:fut "), Some(Symbol::Future("SENSEX")));
        assert_eq!(Symbol::parse("nifty:2026-10-27:22750:ce"), Some(option));
        assert_eq!(option.name(), "NIFTY:2026-10-27:22750:CE");
        assert_eq!(Symbol::parse(&option.name()), Some(option));
        assert_eq!(Symbol::parse("MIDCPNIFTY:2026-10-27:12437.5:PE").unwrap().name(), "MIDCPNIFTY:2026-10-27:12437.5:PE");

        for bad in [
            "", "RELIANCE", "NIFTY50", "NIFTY:FUTURE", "NIFTY:FUT:X", "NIFTY:2026-13-01:22750:CE",
            "NIFTY:2026-10-27:0:CE", "NIFTY:2026-10-27:22750:XX", "NIFTY:2026-10-27:22750", "NIFTY:2026-10-27:22750:CE:1",
        ] {
            assert_eq!(Symbol::parse(bad), None, "{bad}");
        }
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
        ];
        let today = date(10, 21);
        assert_eq!(find_upstox(&Symbol::Future("NIFTY"), &list, today).unwrap().instrument_key, "NSE_FO|48704");
        // The day after the October future expires, it rolls to November.
        assert_eq!(find_upstox(&Symbol::Future("NIFTY"), &list, date(10, 28)).unwrap().instrument_key, "NSE_FO|2");
        let put = Symbol::Option { index: "NIFTY", expiry: date(10, 27), strike: 22750.0, side: Side::Pe };
        assert_eq!(find_upstox(&put, &list, today).unwrap().instrument_key, "NSE_FO|51343");
        let missing = Symbol::Option { index: "NIFTY", expiry: date(11, 3), strike: 22750.0, side: Side::Pe };
        assert!(find_upstox(&missing, &list, today).is_none());
        assert!(find_upstox(&Symbol::Index("NIFTY"), &list, today).is_none());
        assert_eq!(upstox_index_key("MIDCPNIFTY"), Some("NSE_INDEX|NIFTY MID SELECT"));
    }

    fn record(symbol: &str, segment: &str, raw_strike: f64, opt: &str, expiry: NaiveDate, token: &str) -> ScripRecord {
        ScripRecord {
            instrument_token: token.into(),
            trading_symbol: symbol.into(),
            symbol_name: symbol.chars().take_while(|c| c.is_ascii_alphabetic()).collect(),
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
        assert_eq!(find_kotak(&Symbol::Future("NIFTY"), &store, today).unwrap().instrument_token, "48704");
        let call = Symbol::Option { index: "NIFTY", expiry: date(10, 27), strike: 22750.0, side: Side::Ce };
        assert_eq!(find_kotak(&call, &store, today).unwrap().ws_key(), "nse_fo|51342");
        let other_strike = Symbol::Option { index: "NIFTY", expiry: date(10, 27), strike: 22800.0, side: Side::Ce };
        assert!(find_kotak(&other_strike, &store, today).is_none());
        assert!(find_kotak(&Symbol::Future("SENSEX"), &store, today).is_none());
    }
}
