//! Ranking over the catalog's underlyings, and the grammar of a typed
//! contract. The list is small (a few thousand equities, the indices, the
//! F&O underlyings) and lives in memory, rebuilt at each load; a query is
//! ranked against it in one pass. The 150-200k contract rows are never
//! scanned: derivatives are expanded afterwards by indexed lookups
//! (`super::Catalog::search`).

use std::collections::{HashMap, HashSet};

use chrono::{Datelike, NaiveDate};
use serde::Serialize;

/// Which group a search row is listed under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    Indices,
    Stocks,
}

/// What an underlying is in the catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnderlyingKind {
    /// One of the six indices the app trades: has a virtual `INDEX` row.
    Index,
    /// An NSE equity or ETF: has a cash row.
    Equity,
    /// Only F&O contracts are listed under it (an index the app has no feed
    /// name for, a stock with no cash listing).
    FoOnly,
}

impl UnderlyingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UnderlyingKind::Index => "INDEX",
            UnderlyingKind::Equity => "EQ",
            UnderlyingKind::FoOnly => "FO",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "INDEX" => UnderlyingKind::Index,
            "EQ" => UnderlyingKind::Equity,
            "FO" => UnderlyingKind::FoOnly,
            _ => return None,
        })
    }
}

/// One symbol in the search list, with what its cash or index row carries
/// so the Indices and Stocks groups come straight from memory.
#[derive(Clone, Debug, PartialEq)]
pub struct Underlying {
    pub symbol: String,
    pub name: String,
    pub kind: UnderlyingKind,
    /// The cash or index row's id; `None` for an F&O-only underlying.
    pub id: Option<String>,
    pub has_fo: bool,
    /// Its F&O rows are index contracts (`OPTIDX`/`FUTIDX`).
    pub fo_index: bool,
    /// The segment its F&O contracts trade in.
    pub fo_segment: Option<String>,
    pub isin: Option<String>,
    pub exchange: Option<String>,
    pub ws_key: Option<String>,
    pub trading_symbol: Option<String>,
    pub lot_size: i64,
    pub tick_size: f64,
}

impl Underlying {
    pub fn group(&self) -> Group {
        match self.kind {
            UnderlyingKind::Index => Group::Indices,
            UnderlyingKind::Equity => Group::Stocks,
            UnderlyingKind::FoOnly if self.fo_index => Group::Indices,
            UnderlyingKind::FoOnly => Group::Stocks,
        }
    }
}

/// How a query matched an underlying, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// The symbol (or ISIN) itself.
    Exact,
    /// The symbol starts with the query.
    SymbolPrefix,
    /// Every word of the query (of two letters or more) starts a word of
    /// the symbol or name (`tata cons` → TCS).
    WordPrefix,
    /// The query appears inside the symbol or name.
    Substring,
}

/// One ranked underlying.
#[derive(Clone, Debug)]
pub struct Hit<'a> {
    pub entry: &'a Underlying,
    pub tier: Tier,
}

/// The underlyings, sorted by symbol, with what a query is matched against.
pub struct SearchIndex {
    entries: Vec<Underlying>,
    /// Upper-case tokens of each entry's symbol and name.
    words: Vec<Vec<String>>,
    upper_names: Vec<String>,
    by_symbol: HashMap<String, usize>,
}

impl SearchIndex {
    pub fn new(mut entries: Vec<Underlying>) -> Self {
        entries.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        entries.dedup_by(|a, b| a.symbol == b.symbol);
        let words = entries.iter().map(|e| tokens(&format!("{} {}", e.symbol, e.name))).collect();
        let upper_names = entries.iter().map(|e| e.name.to_uppercase()).collect();
        let by_symbol = entries.iter().enumerate().map(|(i, e)| (e.symbol.clone(), i)).collect();
        Self { entries, words, upper_names, by_symbol }
    }

    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn entries(&self) -> &[Underlying] {
        &self.entries
    }

    pub fn get(&self, symbol: &str) -> Option<&Underlying> {
        self.by_symbol.get(symbol).map(|&i| &self.entries[i])
    }

    /// The underlyings `text` matches, best first: by tier, then F&O
    /// underlyings before the rest, then the shorter symbol, then A to Z.
    /// `word_hits` is the set of symbols FTS matched for the word-prefix
    /// tier; `None` computes that tier here (the LIKE-style fallback).
    pub fn rank(&self, text: &str, word_hits: Option<&HashSet<String>>) -> Vec<Hit<'_>> {
        let text = normalize(text);
        if text.is_empty() {
            return Vec::new();
        }
        let query_words = prefix_words(&text);
        let mut hits: Vec<(Tier, usize)> = Vec::new();
        for (i, entry) in self.entries.iter().enumerate() {
            let tier = if entry.symbol == text || entry.isin.as_deref() == Some(text.as_str()) {
                Tier::Exact
            } else if entry.symbol.starts_with(&text) {
                Tier::SymbolPrefix
            } else if match word_hits {
                Some(set) => set.contains(&entry.symbol),
                None => word_prefix(&self.words[i], &query_words),
            } {
                Tier::WordPrefix
            } else if entry.symbol.contains(&text) || self.upper_names[i].contains(&text) {
                Tier::Substring
            } else {
                continue;
            };
            hits.push((tier, i));
        }
        hits.sort_by(|a, b| {
            let key = |&(tier, i): &(Tier, usize)| {
                let e = &self.entries[i];
                (tier, !e.has_fo, e.symbol.len(), e.symbol.as_str())
            };
            key(a).cmp(&key(b))
        });
        hits.into_iter().map(|(tier, i)| Hit { entry: &self.entries[i], tier }).collect()
    }
}

/// Upper-cased, trimmed, inner runs of whitespace collapsed to one space.
pub fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_uppercase()
}

/// The words of `text`: runs of letters and digits, upper-cased — what the
/// FTS5 `unicode61` tokenizer makes of it too (`M&M` → `M`, `M`;
/// `BAJAJ-AUTO` → `BAJAJ`, `AUTO`).
pub fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_uppercase())
        .collect()
}

/// The words of a query the word-prefix tier searches by: those of two
/// characters or more. A single letter (`M&M` splits into `M`, `M`) would
/// start a word of half the catalog; the symbol tiers still read it whole.
fn prefix_words(text: &str) -> Vec<String> {
    tokens(text).into_iter().filter(|w| w.chars().count() >= 2).collect()
}

/// Every query word starts some word of the entry.
fn word_prefix(entry_words: &[String], query_words: &[String]) -> bool {
    !query_words.is_empty() && query_words.iter().all(|q| entry_words.iter().any(|w| w.starts_with(q.as_str())))
}

/// The FTS5 query for the word-prefix tier: every word as a prefix term,
/// all required (`"tata"* "cons"*`). `None` when the text has no such word.
pub fn fts_query(text: &str) -> Option<String> {
    let words = prefix_words(text);
    if words.is_empty() {
        return None;
    }
    Some(words.iter().map(|w| format!("\"{}\"*", w.replace('"', "\"\""))).collect::<Vec<_>>().join(" "))
}

// ---------------------------------------------------------------------------
// Typed contracts
// ---------------------------------------------------------------------------

/// A call or a put.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Ce,
    Pe,
}

impl Side {
    fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "CE" | "CALL" | "CALLS" => Side::Ce,
            "PE" | "PUT" | "PUTS" => Side::Pe,
            _ => return None,
        })
    }
}

/// An expiry named in a query: `NOV`, `NOV26`, `03NOV`, `03NOV26`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonthHint {
    pub month: u32,
    pub day: Option<u32>,
    pub year: Option<i32>,
}

impl MonthHint {
    pub fn matches(&self, date: NaiveDate) -> bool {
        date.month() == self.month
            && self.day.is_none_or(|d| date.day() == d)
            && self.year.is_none_or(|y| date.year() == y)
    }

    fn parse(token: &str) -> Option<Self> {
        let bytes = token.as_bytes();
        let digits_before = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
        if digits_before > 2 {
            return None;
        }
        let rest = &token[digits_before..];
        if rest.len() < 3 || !rest.is_char_boundary(3) {
            return None;
        }
        let (name, after) = rest.split_at(3);
        let month = match name {
            "JAN" => 1,
            "FEB" => 2,
            "MAR" => 3,
            "APR" => 4,
            "MAY" => 5,
            "JUN" => 6,
            "JUL" => 7,
            "AUG" => 8,
            "SEP" => 9,
            "OCT" => 10,
            "NOV" => 11,
            "DEC" => 12,
            _ => return None,
        };
        let year = match after.len() {
            0 => None,
            2 if after.bytes().all(|b| b.is_ascii_digit()) => Some(2000 + after.parse::<i32>().ok()?),
            4 if after.bytes().all(|b| b.is_ascii_digit()) => Some(after.parse::<i32>().ok()?),
            _ => return None,
        };
        let day = if digits_before == 0 {
            None
        } else {
            let day = token[..digits_before].parse::<u32>().ok()?;
            if !(1..=31).contains(&day) {
                return None;
            }
            Some(day)
        };
        Some(MonthHint { month, day, year })
    }
}

/// What a query asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum Parsed {
    /// A symbol or name, or part of one.
    Plain(String),
    /// `<SYM> FUT`: the underlying's futures.
    Future { symbol: String },
    /// `<SYM> [<MON>] <STRIKE> CE|PE`: one strike of the underlying's options.
    Contract { symbol: String, month: Option<MonthHint>, strike: f64, side: Side },
}

impl Parsed {
    /// The symbol or name text to rank underlyings by.
    pub fn text(&self) -> &str {
        match self {
            Parsed::Plain(text) => text,
            Parsed::Future { symbol } | Parsed::Contract { symbol, .. } => symbol,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Parsed::Plain(_) => "plain",
            Parsed::Future { .. } => "future",
            Parsed::Contract { .. } => "contract",
        }
    }
}

/// Reads `BANKNIFTY 52000 CE`, `RELIANCE NOV 1400 PE`, `nifty 23000ce`,
/// `M&M 2800 PUT`, `TATA FUT`; anything else is a plain search.
pub fn parse_query(raw: &str) -> Parsed {
    let text = normalize(raw);
    let mut tokens: Vec<String> = text.split(' ').filter(|t| !t.is_empty()).map(str::to_string).collect();
    if tokens.len() >= 2 && matches!(tokens[tokens.len() - 1].as_str(), "FUT" | "FUTURE" | "FUTURES") {
        tokens.pop();
        return Parsed::Future { symbol: tokens.join(" ") };
    }
    // `23000CE` is a strike and a side in one token.
    if let Some((strike, side)) = tokens.last().and_then(|t| split_strike_side(t)) {
        tokens.pop();
        tokens.push(strike);
        tokens.push(side.to_string());
    }
    let n = tokens.len();
    if n >= 3 {
        if let (Some(side), Some(strike)) = (Side::parse(&tokens[n - 1]), strike_token(&tokens[n - 2])) {
            let (symbol_end, month) = match MonthHint::parse(&tokens[n - 3]) {
                Some(month) if n >= 4 => (n - 3, Some(month)),
                _ => (n - 2, None),
            };
            return Parsed::Contract { symbol: tokens[..symbol_end].join(" "), month, strike, side };
        }
    }
    Parsed::Plain(text)
}

/// `23000`, `23,000`, `22.5` as a strike.
fn strike_token(token: &str) -> Option<f64> {
    let cleaned = token.replace(',', "");
    if cleaned.is_empty() || !cleaned.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    cleaned.parse::<f64>().ok().filter(|s| s.is_finite() && *s > 0.0)
}

/// `23000CE` → (`23000`, `CE`).
fn split_strike_side(token: &str) -> Option<(String, &'static str)> {
    for side in ["CE", "PE"] {
        if let Some(number) = token.strip_suffix(side) {
            if strike_token(number).is_some() {
                return Some((number.to_string(), side));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(symbol: &str, name: &str, kind: UnderlyingKind, has_fo: bool) -> Underlying {
        Underlying {
            symbol: symbol.into(),
            name: name.into(),
            kind,
            id: Some(format!("nse_cm:{symbol}")),
            has_fo,
            fo_index: kind == UnderlyingKind::Index,
            fo_segment: has_fo.then(|| "nse_fo".to_string()),
            isin: None,
            exchange: Some("NSE".into()),
            ws_key: Some(format!("nse_cm|{symbol}")),
            trading_symbol: Some(format!("{symbol}-EQ")),
            lot_size: 1,
            tick_size: 0.05,
        }
    }

    fn index() -> SearchIndex {
        SearchIndex::new(vec![
            entry("RELIANCE", "Reliance Industries", UnderlyingKind::Equity, true),
            entry("RELAXO", "Relaxo Footwears", UnderlyingKind::Equity, false),
            entry("TCS", "Tata Consultancy Services", UnderlyingKind::Equity, false),
            entry("TATAMOTORS", "Tata Motors", UnderlyingKind::Equity, true),
            entry("TATASTEEL", "Tata Steel", UnderlyingKind::Equity, true),
            entry("M&M", "Mahindra & Mahindra", UnderlyingKind::Equity, true),
            entry("BAJAJ-AUTO", "Bajaj Auto", UnderlyingKind::Equity, true),
            entry("NIFTY", "Nifty 50", UnderlyingKind::Index, true),
            entry("NIFTYBEES", "Nippon India ETF Nifty 50 BeES", UnderlyingKind::Equity, false),
        ])
    }

    fn symbols<'a>(hits: &'a [Hit<'a>]) -> Vec<&'a str> {
        hits.iter().map(|h| h.entry.symbol.as_str()).collect()
    }

    #[test]
    fn ranking_goes_exact_prefix_words_substring_with_fo_underlyings_first() {
        let index = index();
        let hits = index.rank("REL", None);
        assert_eq!(symbols(&hits), vec!["RELIANCE", "RELAXO"]);
        assert_eq!(hits[0].tier, Tier::SymbolPrefix);

        // Exact beats prefix; the F&O stock beats the one without within a tier.
        let hits = index.rank("tata", None);
        assert_eq!(symbols(&hits), vec!["TATASTEEL", "TATAMOTORS", "TCS"]);
        assert_eq!((hits[0].tier, hits[2].tier), (Tier::SymbolPrefix, Tier::WordPrefix));
        assert_eq!(symbols(&index.rank("TCS", None)), vec!["TCS"]);
        assert_eq!(index.rank("TCS", None)[0].tier, Tier::Exact);

        // Name words, in any order; every word must start a word.
        assert_eq!(symbols(&index.rank("consultancy", None)), vec!["TCS"]);
        assert_eq!(symbols(&index.rank("services tata", None)), vec!["TCS"]);
        assert!(index.rank("tata consultancy motors", None).is_empty());
        // `&` and `-` split words, and the whole symbol still matches exactly.
        assert_eq!(symbols(&index.rank("M&M", None)), vec!["M&M"]);
        assert_eq!(symbols(&index.rank("auto", None)), vec!["BAJAJ-AUTO"]);
        assert_eq!(symbols(&index.rank("bajaj-auto", None)), vec!["BAJAJ-AUTO"]);
        // Substring last.
        let hits = index.rank("LIANC", None);
        assert_eq!(symbols(&hits), vec!["RELIANCE"]);
        assert_eq!(hits[0].tier, Tier::Substring);
        // The index comes out ahead of the ETF named after it.
        let hits = index.rank("nifty", None);
        assert_eq!(symbols(&hits), vec!["NIFTY", "NIFTYBEES"]);
        assert_eq!(hits[0].entry.group(), Group::Indices);
        assert_eq!(hits[1].entry.group(), Group::Stocks);
        assert!(index.rank("   ", None).is_empty());
        assert!(index.rank("zzz", None).is_empty());
    }

    #[test]
    fn fts_hits_stand_in_for_the_word_tier() {
        let index = index();
        // What FTS would answer for `tata`: both symbol and name words.
        let fts: HashSet<String> = ["TATAMOTORS", "TATASTEEL", "TCS"].iter().map(|s| s.to_string()).collect();
        assert_eq!(symbols(&index.rank("tata", Some(&fts))), symbols(&index.rank("tata", None)));
        // Without FTS naming it, TCS is no word match — only a substring one, last.
        let none: HashSet<String> = HashSet::new();
        let hits = index.rank("tata", Some(&none));
        assert_eq!(symbols(&hits), vec!["TATASTEEL", "TATAMOTORS", "TCS"]);
        assert_eq!(hits[2].tier, Tier::Substring);
        assert_eq!(fts_query("tata cons").as_deref(), Some("\"TATA\"* \"CONS\"*"));
        // Single letters are no word search: `M&M` is found whole, not as two Ms.
        assert_eq!(fts_query("m&m"), None);
        assert_eq!(fts_query("tata m").as_deref(), Some("\"TATA\"*"));
        assert_eq!(fts_query("&&"), None);
        assert_eq!(tokens("Bajaj-Auto 50"), vec!["BAJAJ", "AUTO", "50"]);
        assert_eq!(normalize("  tata   motors "), "TATA MOTORS");
    }

    #[test]
    fn the_query_parser_reads_contracts_and_futures() {
        assert_eq!(
            parse_query("BANKNIFTY 52000 CE"),
            Parsed::Contract { symbol: "BANKNIFTY".into(), month: None, strike: 52_000.0, side: Side::Ce }
        );
        assert_eq!(
            parse_query("reliance nov 1400 pe"),
            Parsed::Contract {
                symbol: "RELIANCE".into(),
                month: Some(MonthHint { month: 11, day: None, year: None }),
                strike: 1_400.0,
                side: Side::Pe,
            }
        );
        assert_eq!(
            parse_query("nifty 23000ce"),
            Parsed::Contract { symbol: "NIFTY".into(), month: None, strike: 23_000.0, side: Side::Ce }
        );
        assert_eq!(
            parse_query("NIFTY 03NOV26 23,000 CALL"),
            Parsed::Contract {
                symbol: "NIFTY".into(),
                month: Some(MonthHint { month: 11, day: Some(3), year: Some(2026) }),
                strike: 23_000.0,
                side: Side::Ce,
            }
        );
        assert_eq!(
            parse_query("M&M 2800 PUT"),
            Parsed::Contract { symbol: "M&M".into(), month: None, strike: 2_800.0, side: Side::Pe }
        );
        assert_eq!(
            parse_query("idea 7.5 ce"),
            Parsed::Contract { symbol: "IDEA".into(), month: None, strike: 7.5, side: Side::Ce }
        );
        // Several words name the underlying.
        assert_eq!(
            parse_query("tata motors dec 700 ce"),
            Parsed::Contract {
                symbol: "TATA MOTORS".into(),
                month: Some(MonthHint { month: 12, day: None, year: None }),
                strike: 700.0,
                side: Side::Ce,
            }
        );
        assert_eq!(parse_query("TATA FUT"), Parsed::Future { symbol: "TATA".into() });
        assert_eq!(parse_query("bajaj-auto futures"), Parsed::Future { symbol: "BAJAJ-AUTO".into() });
        // Not contracts: no symbol, no strike, a lone side, a plain name.
        assert_eq!(parse_query("23000 CE"), Parsed::Plain("23000 CE".into()));
        assert_eq!(parse_query("FUT"), Parsed::Plain("FUT".into()));
        assert_eq!(parse_query("NIFTY CE"), Parsed::Plain("NIFTY CE".into()));
        assert_eq!(parse_query("NIFTY NOV CE"), Parsed::Plain("NIFTY NOV CE".into()));
        assert_eq!(parse_query(" reliance "), Parsed::Plain("RELIANCE".into()));
        assert_eq!(parse_query("").text(), "");
        assert_eq!(parse_query("TATA FUT").label(), "future");

        let hint = MonthHint::parse("NOV26").unwrap();
        assert!(hint.matches(NaiveDate::from_ymd_opt(2026, 11, 24).unwrap()));
        assert!(!hint.matches(NaiveDate::from_ymd_opt(2025, 11, 24).unwrap()));
        assert!(MonthHint::parse("03NOV").unwrap().matches(NaiveDate::from_ymd_opt(2026, 11, 3).unwrap()));
        assert!(!MonthHint::parse("03NOV").unwrap().matches(NaiveDate::from_ymd_opt(2026, 11, 24).unwrap()));
        assert_eq!(MonthHint::parse("NOVEMBER"), None);
        assert_eq!(MonthHint::parse("123NOV"), None);
        assert_eq!(MonthHint::parse("32NOV"), None);
        assert_eq!(MonthHint::parse("NOV2"), None);
        assert_eq!(MonthHint::parse("23000"), None);
    }
}
