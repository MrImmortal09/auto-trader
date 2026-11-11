//! The catalog's own pass over the scrip-master CSVs on disk: a second parser
//! beside `ScripStore::parse_csv`, which stays exactly as it is (the engine
//! resolves calls from that store; this one only fills the search tables).
//!
//! Each file is read a line at a time and turned into `instruments` rows a
//! batch at a time, so the text is never held whole, and the whole build runs
//! in one transaction into `instruments_new`, renamed over `instruments` at
//! the end: a reader sees the old catalog or the new one, never half of
//! either, and a build that fails leaves the old one in place. The build runs
//! on a blocking thread (`rebuild`), off the runtime the engine ticks on.
//!
//! Kotak documents only `pSymbol`, `pExchSeg`, `pTrdSymbol`, `lLotSize` and
//! `lExpiryDate` (`kotak-api-docs/instruments-scrip-master.md` §6); the rest
//! is matched by name when present and tolerated when not. The real header
//! of every file is logged once per load, naming what it lacks.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io::{self, BufRead};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, FixedOffset, NaiveDate};
use serde::Serialize;
use shared_domain::{index_price_key, kotak_index_feed, INDEX_NAMES};
use sqlx::pool::PoolConnection;
use sqlx::{Connection as _, Sqlite, SqlitePool, Transaction};

use super::search::{SearchIndex, Underlying, UnderlyingKind};
use super::LoadError;

/// Kotak's nse_fo / cde_fo expiry rule: `lExpiryDate` plus this is the Unix
/// epoch (`kotak-api-docs/instruments-scrip-master.md` §6). bse_fo and mcx_fo
/// carry the plain epoch.
pub const NSE_FO_EXPIRY_OFFSET: i64 = 315_511_200;

/// Rows per `INSERT`: 16 columns each, well under SQLite's variable limit.
const BATCH: usize = 400;

/// `instrument_load` rows older than this are dropped at each build.
const LOAD_HISTORY_DAYS: i64 = 14;

/// What an `instruments` row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Kind {
    #[serde(rename = "INDEX")]
    Index,
    #[serde(rename = "EQ")]
    Eq,
    #[serde(rename = "FUT")]
    Fut,
    #[serde(rename = "CE")]
    Ce,
    #[serde(rename = "PE")]
    Pe,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Index => "INDEX",
            Kind::Eq => "EQ",
            Kind::Fut => "FUT",
            Kind::Ce => "CE",
            Kind::Pe => "PE",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "INDEX" => Kind::Index,
            "EQ" => Kind::Eq,
            "FUT" => Kind::Fut,
            "CE" => Kind::Ce,
            "PE" => Kind::Pe,
            _ => return None,
        })
    }

    pub fn is_option(self) -> bool {
        matches!(self, Kind::Ce | Kind::Pe)
    }
}

/// One `instruments` row.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// `<segment>:<token>` — `nse_fo:51386`, `nse_cm:Nifty 50` for an index.
    pub id: String,
    pub segment: &'static str,
    /// `pSymbol`: the token the feed and the quotes API take; an index's
    /// exact case-sensitive name.
    pub token: String,
    pub trading_symbol: String,
    /// `RELIANCE`, `NIFTY`, `M&M`, `BAJAJ-AUTO`.
    pub symbol: String,
    pub name: String,
    pub kind: Kind,
    /// The symbol a derivative is listed under; the symbol itself for cash
    /// and index rows.
    pub underlying: String,
    pub expiry: Option<NaiveDate>,
    /// In rupees, with the scrip master's scale read off the trading symbol
    /// (`strike_of`). `None` for a future, a cash row, or an option whose
    /// symbol agrees with no known scale.
    pub strike: Option<f64>,
    pub lot_size: i64,
    pub tick_size: f64,
    pub isin: Option<String>,
    /// `NSE` or `BSE`.
    pub exchange: &'static str,
    /// The price-map key: `nse_fo|51386`, `idx|NIFTY 50`.
    pub ws_key: String,
}

/// One CSV on disk the catalog is built from.
#[derive(Clone, Debug)]
pub struct Source {
    pub segment: &'static str,
    pub path: PathBuf,
    /// The file's `modified` time, nanoseconds since the epoch: what a
    /// later build compares to know whether the file changed.
    pub mtime: i64,
}

impl Source {
    /// The IST date the file was written.
    pub fn day(&self) -> NaiveDate {
        let secs = self.mtime.div_euclid(1_000_000_000);
        DateTime::from_timestamp(secs, 0)
            .map(|t| t.with_timezone(&shared_domain::ist_offset()).date_naive())
            .unwrap_or_else(shared_domain::today_ist)
    }
}

/// The data day a build from `sources` is stamped with: the IST date of
/// the oldest file. `None` when there is none.
pub fn files_day(sources: &[Source]) -> Option<NaiveDate> {
    sources.iter().map(Source::day).min()
}

/// What one file's pass produced, for the log and `instrument_load`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SegmentReport {
    pub segment: &'static str,
    /// The header line as the file has it.
    pub header: String,
    /// Expected columns the header lacks, by Kotak's names.
    pub missing: Vec<&'static str>,
    /// Non-blank data lines read.
    pub lines: u64,
    /// Rows written.
    pub kept: u64,
    /// Lines left out, with the first reason seen for each.
    pub skipped: u64,
    pub skip_reasons: BTreeMap<&'static str, u64>,
    /// An example of the first skipped line, trimmed.
    pub first_skip: Option<String>,
    /// Options kept with no strike, their symbol agreeing with no known scale.
    pub unresolved_strikes: u64,
    /// Rows whose id a row before them already used.
    pub duplicates: u64,
    /// Rows kept per kind (`EQ`, `FUT`, `CE`, `PE`), counted as the file is
    /// read: a token repeated in the file counts at each row here and once
    /// in `stored`.
    pub kinds: BTreeMap<&'static str, u64>,
}

impl SegmentReport {
    /// Rows in the table: those kept less those a row before them already
    /// held the id of.
    pub fn stored(&self) -> u64 {
        self.kept.saturating_sub(self.duplicates)
    }

    fn note_skip(&mut self, reason: &'static str, line: &str) {
        self.skipped += 1;
        *self.skip_reasons.entry(reason).or_insert(0) += 1;
        if self.first_skip.is_none() {
            self.first_skip = Some(format!("{reason}: {}", line.chars().take(120).collect::<String>()));
        }
    }
}

/// What a whole build produced.
pub struct LoadReport {
    /// IST date of the oldest file: the catalog's data day.
    pub day: String,
    /// IST `YYYY-MM-DD HH:MM:SS` the build finished.
    pub loaded_at: String,
    pub segments: Vec<SegmentReport>,
    /// The six virtual index rows.
    pub index_rows: u64,
    pub underlyings: usize,
    pub secs: f64,
    /// The search list built in the same pass.
    pub index: Arc<SearchIndex>,
}

// ---------------------------------------------------------------------------
// The CSV header
// ---------------------------------------------------------------------------

/// Where each column sits in a file's header. Names are matched lower-cased
/// and trimmed, the stray `dStrikePrice;` and the padded `dTickSize ` of the
/// real files included; a reordered or widened header costs nothing.
#[derive(Debug, Default)]
pub struct Columns {
    token: Option<usize>,
    exch_seg: Option<usize>,
    inst_type: Option<usize>,
    symbol_name: Option<usize>,
    trd_symbol: Option<usize>,
    option_type: Option<usize>,
    lot_size: Option<usize>,
    expiry: Option<usize>,
    strike: Option<usize>,
    tick_size: Option<usize>,
    isin: Option<usize>,
    desc: Option<usize>,
    group: Option<usize>,
}

impl Columns {
    pub fn from_header(header: &str) -> Self {
        let mut c = Columns::default();
        for (i, raw) in split_fields(header).iter().enumerate() {
            let key = raw.trim_start_matches('\u{feff}').trim().to_lowercase();
            let key = key.trim_end_matches(';').trim();
            let slot = match key {
                "psymbol" => &mut c.token,
                "pexchseg" => &mut c.exch_seg,
                "pinsttype" => &mut c.inst_type,
                "psymbolname" => &mut c.symbol_name,
                "ptrdsymbol" => &mut c.trd_symbol,
                "poptiontype" => &mut c.option_type,
                "llotsize" => &mut c.lot_size,
                "lexpirydate" | "pexpirydate" => &mut c.expiry,
                "pisin" => &mut c.isin,
                "pdesc" => &mut c.desc,
                "pgroup" | "pseries" => &mut c.group,
                k if k.contains("strikeprice") => &mut c.strike,
                k if k.contains("ticksize") => &mut c.tick_size,
                _ => continue,
            };
            if slot.is_none() {
                *slot = Some(i);
            }
        }
        c
    }

    /// Whether rows can be read at all: a token and a trading symbol.
    pub fn usable(&self) -> bool {
        self.token.is_some() && self.trd_symbol.is_some()
    }

    /// The columns the catalog expects of `segment`'s file and the header
    /// lacks, by Kotak's names.
    pub fn missing(&self, segment: &str) -> Vec<&'static str> {
        let cash = segment.ends_with("_cm");
        let mut out = Vec::new();
        let mut want = |present: bool, name: &'static str| {
            if !present {
                out.push(name);
            }
        };
        want(self.token.is_some(), "pSymbol");
        want(self.exch_seg.is_some(), "pExchSeg");
        want(self.inst_type.is_some(), "pInstType");
        want(self.symbol_name.is_some(), "pSymbolName");
        want(self.trd_symbol.is_some(), "pTrdSymbol");
        if !cash {
            want(self.option_type.is_some(), "pOptionType");
            want(self.expiry.is_some(), "lExpiryDate");
            want(self.strike.is_some(), "dStrikePrice");
        }
        want(self.lot_size.is_some(), "lLotSize");
        want(self.tick_size.is_some(), "dTickSize");
        if cash {
            want(self.isin.is_some(), "pISIN");
        }
        want(self.desc.is_some(), "pDesc");
        out
    }
}

/// The fields of one CSV line, trimmed, surrounding quotes removed and a
/// comma inside quotes kept (`"Larsen & Toubro, Ltd"`). A line without a
/// quote splits exactly as `str::split(',')` does.
pub fn split_fields(line: &str) -> Vec<Cow<'_, str>> {
    if !line.contains('"') {
        return line.split(',').map(|f| Cow::Borrowed(f.trim())).collect();
    }
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' if in_quotes => {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            }
            '"' if !quoted && field.trim().is_empty() => {
                in_quotes = true;
                quoted = true;
                field.clear();
            }
            ',' if !in_quotes => {
                fields.push(Cow::Owned(std::mem::take(&mut field).trim().to_string()));
                quoted = false;
            }
            c => field.push(c),
        }
    }
    fields.push(Cow::Owned(field.trim().to_string()));
    fields
}

// ---------------------------------------------------------------------------
// Strike and expiry
// ---------------------------------------------------------------------------

/// The option's strike in rupees, read off the trading symbol the way the
/// engine's `ScripRecord::normalized_strike` reads it
/// (`trading_engine/src/scrip_master.rs`): the scrip master stores strikes
/// scaled (×100 for every index option seen so far: `2300000` for
/// `NIFTY26N0323000CE`), so each known scale is tried, ×100 first, and the
/// one whose result the symbol ends with wins. A copy on purpose, not a
/// shared function: the engine resolves whole-rupee strikes only and its
/// parser stays byte for byte as it is, while the catalog also reads the
/// ₹x.5 strikes stock options list (`IDEA26NOV7.5CE`, `750`). The two are
/// held together by a test (`tests::the_catalog_agrees_with_the_engine`).
pub fn strike_of(trading_symbol: &str, option_type: &str, raw: f64) -> Option<f64> {
    if option_type != "CE" && option_type != "PE" {
        return None;
    }
    let stem = trading_symbol.trim().strip_suffix(option_type)?;
    if !(raw > 0.0) || !raw.is_finite() {
        return None;
    }
    [100.0, 1.0, 1000.0, 10.0].into_iter().find_map(|div| {
        let strike = raw / div;
        let text = strike_text(strike)?;
        stem.ends_with(&text).then_some(strike)
    })
}

/// `strike` as a trading symbol writes it: `23000`, `7.5`, `22.25`. `None`
/// unless it is a clean number of paise.
pub fn strike_text(strike: f64) -> Option<String> {
    let paise = (strike * 100.0).round();
    if !(paise > 0.0) || (strike * 100.0 - paise).abs() > 1e-6 {
        return None;
    }
    let paise = paise as i64;
    let (whole, frac) = (paise / 100, paise % 100);
    Some(if frac == 0 {
        whole.to_string()
    } else if frac % 10 == 0 {
        format!("{whole}.{}", frac / 10)
    } else {
        format!("{whole}.{frac:02}")
    })
}

/// Kotak's `dTickSize` in rupees. The real files carry it in paise, as they
/// do strikes: `5` on every NSE and BSE option (₹0.05), `10` on RELIANCE-EQ
/// (₹0.10), `1` on IDEA-EQ (₹0.01), `100` on BAJAJ-AUTO-EQ (₹1.00) — a value
/// of 1 or more is paise and is divided by 100; a fraction under 1 is
/// already rupees (no real file writes one, but a tick is never ₹1 or more
/// on these exchanges, so the rule cannot misread either form). Blank or
/// unreadable is the exchanges' usual ₹0.05. The engine's `ScripRecord`
/// never read this column (it looks for `pTickSize`), so nothing there
/// changes.
pub fn tick_size_of(raw: &str) -> f64 {
    match raw.trim().parse::<f64>() {
        Ok(v) if v.is_finite() && v >= 1.0 => v / 100.0,
        Ok(v) if v.is_finite() && v > 0.0 => v,
        _ => 0.05,
    }
}

/// Kotak's `lExpiryDate` as an IST calendar date. nse_fo (and cde_fo) files
/// carry the Unix epoch minus [`NSE_FO_EXPIRY_OFFSET`]; bse_fo and mcx_fo
/// the plain epoch. The engine's parser (`ScripStore::parse_csv`) tells the
/// two apart by the epoch's size instead, which agrees with this rule until
/// 2030; it is left as it is, and with the segment known here the documented
/// rule is used directly. A date written out (`03-NOV-2026`, `2026-11-03`)
/// is read as well, in case a file ever carries one.
pub fn expiry_of(segment: &str, text: &str) -> Option<NaiveDate> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(epoch) = text.parse::<i64>() {
        if epoch <= 0 {
            return None;
        }
        let unix = match segment {
            "nse_fo" | "cde_fo" => epoch + NSE_FO_EXPIRY_OFFSET,
            _ => epoch,
        };
        return DateTime::from_timestamp(unix, 0).map(|t| t.with_timezone(&shared_domain::ist_offset()).date_naive());
    }
    ["%d-%b-%Y", "%d/%m/%Y", "%d-%m-%Y", "%Y-%m-%d"]
        .into_iter()
        .find_map(|fmt| NaiveDate::parse_from_str(text, fmt).ok())
}

/// `RELIANCE 24 NOV 26 1400 CE`, for a file without `pDesc`.
fn derived_name(underlying: &str, kind: Kind, expiry: NaiveDate, strike: Option<f64>) -> String {
    let when = expiry.format("%d %b %y").to_string().to_uppercase();
    match (kind, strike.and_then(strike_text)) {
        (Kind::Fut, _) => format!("{underlying} {when} FUT"),
        (kind, Some(strike)) => format!("{underlying} {when} {strike} {}", kind.as_str()),
        (kind, None) => format!("{underlying} {when} {}", kind.as_str()),
    }
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// The six indices the app trades, as virtual rows keyed the way Kotak's
/// feed and quotes API name them (`kotak_index_feed`).
pub fn index_rows() -> Vec<Row> {
    INDEX_NAMES
        .iter()
        .filter_map(|name| {
            let (segment, feed) = kotak_index_feed(name)?;
            Some(Row {
                id: format!("{segment}:{feed}"),
                segment,
                token: feed.to_string(),
                trading_symbol: feed.to_string(),
                symbol: name.to_string(),
                name: feed.to_string(),
                kind: Kind::Index,
                underlying: name.to_string(),
                expiry: None,
                strike: None,
                lot_size: 1,
                tick_size: 0.05,
                isin: None,
                exchange: exchange_of(segment),
                ws_key: index_price_key(feed),
            })
        })
        .collect()
}

fn exchange_of(segment: &str) -> &'static str {
    if segment.starts_with("bse") {
        "BSE"
    } else {
        "NSE"
    }
}

/// `RELIANCE-EQ` → (`RELIANCE`, `EQ`); `BAJAJ-AUTO-EQ` → (`BAJAJ-AUTO`, `EQ`);
/// `Nifty 50` → (`Nifty 50`, none). NSE's series are one to three letters or
/// digits after the last dash.
fn split_series(trading_symbol: &str) -> (&str, Option<&str>) {
    match trading_symbol.rsplit_once('-') {
        Some((stem, series))
            if !stem.is_empty()
                && (1..=3).contains(&series.len())
                && series.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            (stem, Some(series))
        }
        _ => (trading_symbol, None),
    }
}

/// Why a line was left out — a fixed set, counted in the report.
struct Skip(&'static str);

/// One parsed line.
struct Parsed {
    row: Row,
    /// Its series for a cash row (`EQ` or `BE`).
    series: Option<&'static str>,
    /// Its `pInstType` says index (`OPTIDX`, `FUTIDX`), for an F&O row.
    index_type: bool,
    unresolved_strike: bool,
}

fn row_from(segment: &'static str, c: &Columns, line: &str) -> Result<Parsed, Skip> {
    let fields = split_fields(line);
    let field = |i: Option<usize>| -> &str { i.and_then(|i| fields.get(i)).map(|f| f.as_ref()).unwrap_or("") };

    let token = field(c.token);
    if token.is_empty() {
        return Err(Skip("no pSymbol"));
    }
    let trading_symbol = field(c.trd_symbol);
    if trading_symbol.is_empty() {
        return Err(Skip("no pTrdSymbol"));
    }
    let symbol_name = field(c.symbol_name).to_uppercase();
    let lot_size = field(c.lot_size).parse::<u32>().map(i64::from).unwrap_or(1);
    let tick_size = tick_size_of(field(c.tick_size));
    let isin = Some(field(c.isin)).filter(|s| !s.is_empty()).map(str::to_string);
    let desc = Some(field(c.desc)).filter(|s| !s.is_empty()).map(str::to_string);
    let exchange = exchange_of(segment);
    let id = format!("{segment}:{token}");
    let ws_key = format!("{segment}|{token}");

    if segment.ends_with("_cm") {
        let (stem, suffix) = split_series(trading_symbol);
        let series = match suffix {
            Some(series) => series,
            None => field(c.group),
        }
        .to_uppercase();
        // The cash series the catalog lists: normally traded equities and
        // ETFs (`EQ`) and trade-for-trade stocks (`BE`). SME, bonds and the
        // rest are not in the universe.
        let series: &'static str = match series.as_str() {
            "EQ" => "EQ",
            "BE" => "BE",
            "" => return Err(Skip("no series")),
            _ => return Err(Skip("series not EQ or BE")),
        };
        let symbol = if symbol_name.is_empty() { stem.to_uppercase() } else { symbol_name };
        let name = desc.unwrap_or_else(|| symbol.clone());
        return Ok(Parsed {
            row: Row {
                id,
                segment,
                token: token.to_string(),
                trading_symbol: trading_symbol.to_string(),
                underlying: symbol.clone(),
                symbol,
                name,
                kind: Kind::Eq,
                expiry: None,
                strike: None,
                lot_size,
                tick_size,
                isin,
                exchange,
                ws_key,
            },
            series: Some(series),
            index_type: false,
            unresolved_strike: false,
        });
    }

    let underlying = symbol_name;
    if underlying.is_empty() {
        return Err(Skip("no pSymbolName"));
    }
    // bse_fo only for the index options the app already trades; the NSE
    // listing is the one for every stock (the owner's answer: NSE only).
    if segment == "bse_fo" && !matches!(underlying.as_str(), "SENSEX" | "BANKEX") {
        return Err(Skip("bse_fo contract on a stock"));
    }
    let inst_type = field(c.inst_type).to_uppercase();
    let option_type = field(c.option_type).to_uppercase();
    let kind = match option_type.as_str() {
        "CE" => Kind::Ce,
        "PE" => Kind::Pe,
        _ if inst_type.starts_with("FUT") || trading_symbol.ends_with("FUT") => Kind::Fut,
        _ if trading_symbol.ends_with("CE") && (inst_type.starts_with("OPT") || inst_type.is_empty()) => Kind::Ce,
        _ if trading_symbol.ends_with("PE") && (inst_type.starts_with("OPT") || inst_type.is_empty()) => Kind::Pe,
        _ => return Err(Skip("neither an option nor a future")),
    };
    let Some(expiry) = expiry_of(segment, field(c.expiry)) else {
        return Err(Skip("no expiry"));
    };
    let (strike, unresolved_strike) = if kind.is_option() {
        let raw = field(c.strike).parse::<f64>().unwrap_or(0.0);
        match strike_of(trading_symbol, kind.as_str(), raw) {
            Some(strike) => (Some(strike), false),
            None => (None, true),
        }
    } else {
        (None, false)
    };
    let name = desc.unwrap_or_else(|| derived_name(&underlying, kind, expiry, strike));
    Ok(Parsed {
        row: Row {
            id,
            segment,
            token: token.to_string(),
            trading_symbol: trading_symbol.to_string(),
            symbol: underlying.clone(),
            name,
            kind,
            underlying: underlying.clone(),
            expiry: Some(expiry),
            strike,
            lot_size,
            tick_size,
            isin,
            exchange,
            ws_key,
        },
        series: None,
        index_type: inst_type.ends_with("IDX") || INDEX_NAMES.contains(&underlying.as_str()),
        unresolved_strike,
    })
}

// ---------------------------------------------------------------------------
// The underlyings, gathered as the rows go by
// ---------------------------------------------------------------------------

/// The search list, built in the same pass as the rows: one entry per
/// symbol, from its index row, its cash row (an `EQ` listing over a `BE`
/// one) and whether any F&O contract is listed under it.
#[derive(Default)]
pub struct Underlyings {
    map: BTreeMap<String, Underlying>,
}

impl Underlyings {
    fn entry(&mut self, symbol: &str) -> &mut Underlying {
        self.map.entry(symbol.to_string()).or_insert_with(|| Underlying {
            symbol: symbol.to_string(),
            name: symbol.to_string(),
            kind: UnderlyingKind::FoOnly,
            id: None,
            has_fo: false,
            fo_index: false,
            fo_segment: None,
            isin: None,
            exchange: None,
            ws_key: None,
            trading_symbol: None,
            lot_size: 1,
            tick_size: 0.05,
        })
    }

    fn note_cash(&mut self, row: &Row, kind: UnderlyingKind, series: Option<&str>) {
        let entry = self.entry(&row.symbol);
        // A symbol's `EQ` listing wins over any other cash row for it.
        let keep = entry.kind == UnderlyingKind::Equity
            && entry.trading_symbol.as_deref().is_some_and(|t| t.ends_with("-EQ"))
            && series != Some("EQ");
        if entry.id.is_some() && (entry.kind == UnderlyingKind::Index || keep) {
            return;
        }
        entry.kind = kind;
        entry.name = row.name.clone();
        entry.id = Some(row.id.clone());
        entry.isin = row.isin.clone();
        entry.exchange = Some(row.exchange.to_string());
        entry.ws_key = Some(row.ws_key.clone());
        entry.trading_symbol = Some(row.trading_symbol.clone());
        entry.lot_size = row.lot_size;
        entry.tick_size = row.tick_size;
    }

    fn note_fo(&mut self, row: &Row, index_type: bool) {
        let entry = self.entry(&row.underlying);
        entry.has_fo = true;
        entry.fo_index |= index_type;
        if entry.fo_segment.is_none() {
            entry.fo_segment = Some(row.segment.to_string());
        }
    }

    pub fn into_index(self) -> SearchIndex {
        SearchIndex::new(self.map.into_values().collect())
    }
}

// ---------------------------------------------------------------------------
// One file
// ---------------------------------------------------------------------------

/// `line` without its line ending; `\r\n` as well as `\n`.
fn trim_line(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// Reads one segment's file a batch of rows at a time.
pub struct SegmentParser<R: BufRead> {
    reader: R,
    segment: &'static str,
    columns: Columns,
    line: Vec<u8>,
    done: bool,
    pub report: SegmentReport,
}

impl<R: BufRead> SegmentParser<R> {
    /// Reads the header and logs it. `Ok(None)` for an empty file.
    pub fn start(mut reader: R, segment: &'static str) -> io::Result<Option<Self>> {
        let mut line = Vec::new();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(None);
        }
        let header = String::from_utf8_lossy(trim_line(&line)).trim_start_matches('\u{feff}').to_string();
        let columns = Columns::from_header(&header);
        let missing = columns.missing(segment);
        if !columns.usable() {
            tracing::error!(segment, header = %header, "Scrip Master CSV header has no pSymbol or pTrdSymbol column — the file is left out of the catalog");
        } else if missing.is_empty() {
            tracing::info!(segment, header = %header, "Scrip Master CSV header");
        } else {
            tracing::warn!(segment, header = %header, missing = ?missing, "Scrip Master CSV header lacks columns the catalog expected — their fields are left empty");
        }
        let done = !columns.usable();
        Ok(Some(Self {
            reader,
            segment,
            columns,
            line,
            done,
            report: SegmentReport { segment, header, missing, ..Default::default() },
        }))
    }

    /// Up to `n` rows more, with their underlyings noted in `acc`; empty
    /// at the end of the file.
    pub fn next_batch(&mut self, n: usize, acc: &mut Underlyings) -> io::Result<Vec<Row>> {
        let mut batch = Vec::with_capacity(n);
        while !self.done && batch.len() < n {
            self.line.clear();
            if self.reader.read_until(b'\n', &mut self.line)? == 0 {
                self.done = true;
                break;
            }
            let text = String::from_utf8_lossy(trim_line(&self.line));
            if text.trim().is_empty() {
                continue;
            }
            self.report.lines += 1;
            match row_from(self.segment, &self.columns, &text) {
                Ok(parsed) => {
                    if parsed.row.kind == Kind::Eq {
                        acc.note_cash(&parsed.row, UnderlyingKind::Equity, parsed.series);
                    } else {
                        acc.note_fo(&parsed.row, parsed.index_type);
                    }
                    if parsed.unresolved_strike {
                        self.report.unresolved_strikes += 1;
                    }
                    self.report.kept += 1;
                    *self.report.kinds.entry(parsed.row.kind.as_str()).or_insert(0) += 1;
                    batch.push(parsed.row);
                }
                Err(Skip(reason)) => self.report.note_skip(reason, &text),
            }
        }
        Ok(batch)
    }
}

// ---------------------------------------------------------------------------
// The build
// ---------------------------------------------------------------------------

/// Builds the catalog from `sources` on a blocking thread, in one
/// transaction on `pool`, and returns what it found. The old catalog is
/// replaced only by the commit at the end.
pub async fn rebuild(pool: &SqlitePool, sources: Vec<Source>, fts: bool, now: DateTime<FixedOffset>) -> Result<LoadReport, LoadError> {
    let conn = pool.acquire().await?;
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || handle.block_on(build(conn, sources, fts, now)))
        .await
        .map_err(|e| LoadError::Join(e.to_string()))?
}

async fn build(mut conn: PoolConnection<Sqlite>, sources: Vec<Source>, fts: bool, now: DateTime<FixedOffset>) -> Result<LoadReport, LoadError> {
    let started = Instant::now();
    let day = files_day(&sources).unwrap_or_else(|| now.date_naive()).format("%Y-%m-%d").to_string();
    let loaded_at = now.format("%Y-%m-%d %H:%M:%S").to_string();

    let mut tx = conn.begin().await?;
    sqlx::query("DROP TABLE IF EXISTS instruments_new").execute(&mut *tx).await?;
    sqlx::query(&format!("CREATE TABLE instruments_new ({})", crate::db::INSTRUMENTS_COLUMNS))
        .execute(&mut *tx)
        .await?;

    let mut acc = Underlyings::default();
    let index = index_rows();
    for row in &index {
        acc.note_cash(row, UnderlyingKind::Index, None);
    }
    insert_rows(&mut tx, &index, &day).await?;

    let mut segments = Vec::with_capacity(sources.len());
    for source in &sources {
        let io_error = |error: io::Error| LoadError::Io { segment: source.segment, error };
        let file = std::fs::File::open(&source.path).map_err(io_error)?;
        let reader = io::BufReader::with_capacity(64 * 1024, file);
        let Some(mut parser) = SegmentParser::start(reader, source.segment).map_err(io_error)? else {
            segments.push(SegmentReport { segment: source.segment, ..Default::default() });
            continue;
        };
        loop {
            let batch = parser.next_batch(BATCH, &mut acc).map_err(io_error)?;
            if batch.is_empty() {
                break;
            }
            let inserted = insert_rows(&mut tx, &batch, &day).await?;
            parser.report.duplicates += (batch.len() as u64).saturating_sub(inserted);
        }
        segments.push(parser.report);
    }

    // The swap: the old table goes, the new one takes its name and gets the
    // indexes — all still inside the transaction, so a reader keeps seeing
    // the old catalog until the commit.
    sqlx::query("DROP TABLE IF EXISTS instruments").execute(&mut *tx).await?;
    sqlx::query("ALTER TABLE instruments_new RENAME TO instruments").execute(&mut *tx).await?;
    for sql in crate::db::INSTRUMENT_INDEXES {
        sqlx::query(sql).execute(&mut *tx).await?;
    }

    let search = acc.into_index();
    sqlx::query("DELETE FROM instrument_underlyings").execute(&mut *tx).await?;
    for chunk in search.entries().chunks(BATCH) {
        insert_underlyings(&mut tx, chunk).await?;
    }
    if fts {
        sqlx::query("DELETE FROM instruments_fts").execute(&mut *tx).await?;
        for chunk in search.entries().chunks(BATCH) {
            insert_fts(&mut tx, chunk).await?;
        }
    }

    for (source, report) in sources.iter().zip(&segments) {
        sqlx::query(
            "INSERT OR REPLACE INTO instrument_load (day, segment, rows, loaded_at, source_mtime, kinds) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&day)
        .bind(source.segment)
        .bind(report.stored() as i64)
        .bind(&loaded_at)
        .bind(source.mtime)
        .bind(serde_json::to_string(&report.kinds).unwrap_or_else(|_| "{}".into()))
        .execute(&mut *tx)
        .await?;
    }
    let keep_from = (now - chrono::Duration::days(LOAD_HISTORY_DAYS)).format("%Y-%m-%d %H:%M:%S").to_string();
    sqlx::query("DELETE FROM instrument_load WHERE loaded_at < ?").bind(&keep_from).execute(&mut *tx).await?;
    tx.commit().await?;

    // The build just wrote the whole catalog through the WAL; fold it back
    // into the database file and truncate it, or the WAL stays the size of
    // a full catalog (tens of MB) for good. A reader mid-query keeps the
    // checkpoint from finishing; that is fine, the next one gets it.
    match sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(&mut *conn).await {
        Ok(_) => tracing::debug!("Instrument catalog WAL checkpointed"),
        Err(e) => tracing::warn!(error = %e, "Instrument catalog WAL checkpoint failed — the WAL file stays large until the next build"),
    }

    Ok(LoadReport {
        day,
        loaded_at,
        segments,
        index_rows: index.len() as u64,
        underlyings: search.len(),
        secs: started.elapsed().as_secs_f64(),
        index: Arc::new(search),
    })
}

/// Inserts `rows` into `instruments_new` in one statement; rows whose id is
/// already there are left out. Returns how many went in.
async fn insert_rows(tx: &mut Transaction<'_, Sqlite>, rows: &[Row], day: &str) -> Result<u64, sqlx::Error> {
    if rows.is_empty() {
        return Ok(0);
    }
    let sql = format!(
        "INSERT OR IGNORE INTO instruments_new (id, segment, token, trading_symbol, symbol, name, kind, underlying, expiry, strike, lot_size, tick_size, isin, exchange, ws_key, loaded_day) VALUES {}",
        placeholders(16, rows.len())
    );
    let mut query = sqlx::query(&sql);
    for r in rows {
        query = query
            .bind(&r.id)
            .bind(r.segment)
            .bind(&r.token)
            .bind(&r.trading_symbol)
            .bind(&r.symbol)
            .bind(&r.name)
            .bind(r.kind.as_str())
            .bind(&r.underlying)
            .bind(r.expiry.map(|d| d.format("%Y-%m-%d").to_string()))
            .bind(r.strike)
            .bind(r.lot_size)
            .bind(r.tick_size)
            .bind(r.isin.as_deref())
            .bind(r.exchange)
            .bind(&r.ws_key)
            .bind(day);
    }
    Ok(query.execute(&mut **tx).await?.rows_affected())
}

async fn insert_underlyings(tx: &mut Transaction<'_, Sqlite>, entries: &[Underlying]) -> Result<(), sqlx::Error> {
    if entries.is_empty() {
        return Ok(());
    }
    let sql = format!(
        "INSERT OR REPLACE INTO instrument_underlyings (symbol, name, kind, id, has_fo, fo_index, fo_segment, isin, exchange, ws_key, trading_symbol, lot_size, tick_size) VALUES {}",
        placeholders(13, entries.len())
    );
    let mut query = sqlx::query(&sql);
    for u in entries {
        query = query
            .bind(&u.symbol)
            .bind(&u.name)
            .bind(u.kind.as_str())
            .bind(u.id.as_deref())
            .bind(u.has_fo)
            .bind(u.fo_index)
            .bind(u.fo_segment.as_deref())
            .bind(u.isin.as_deref())
            .bind(u.exchange.as_deref())
            .bind(u.ws_key.as_deref())
            .bind(u.trading_symbol.as_deref())
            .bind(u.lot_size)
            .bind(u.tick_size);
    }
    query.execute(&mut **tx).await?;
    Ok(())
}

async fn insert_fts(tx: &mut Transaction<'_, Sqlite>, entries: &[Underlying]) -> Result<(), sqlx::Error> {
    if entries.is_empty() {
        return Ok(());
    }
    let sql = format!("INSERT INTO instruments_fts (symbol, name) VALUES {}", placeholders(2, entries.len()));
    let mut query = sqlx::query(&sql);
    for u in entries {
        query = query.bind(&u.symbol).bind(&u.name);
    }
    query.execute(&mut **tx).await?;
    Ok(())
}

/// `(?,?,?),(?,?,?)` for `rows` rows of `columns` columns.
fn placeholders(columns: usize, rows: usize) -> String {
    let one = format!("({})", vec!["?"; columns].join(","));
    vec![one; rows].join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn strikes_are_read_off_the_symbol_at_every_scale() {
        // Real index options (×100) and a ×1 one, as the engine's test has them.
        assert_eq!(strike_of("NIFTY26O1922900PE", "PE", 2_290_000.0), Some(22_900.0));
        assert_eq!(strike_of("SENSEX26O2275100CE", "CE", 7_510_000.0), Some(75_100.0));
        assert_eq!(strike_of("MIDCPNIFTY26OCT11875PE", "PE", 1_187_500.0), Some(11_875.0));
        assert_eq!(strike_of("BANKNIFTY26OCT45300PE", "PE", 45_300.0), Some(45_300.0));
        // Stock options: ×100, and the half-rupee strikes the engine cannot read.
        assert_eq!(strike_of("RELIANCE26NOV1400CE", "CE", 140_000.0), Some(1_400.0));
        assert_eq!(strike_of("IDEA26NOV7.5CE", "CE", 750.0), Some(7.5));
        assert_eq!(strike_of("IDEA26NOV12.5PE", "PE", 12.5), Some(12.5));
        // ×1000, as the engine allows for.
        assert_eq!(strike_of("ABC26NOV250CE", "CE", 250_000.0), Some(250.0));
        // Futures, a symbol that disagrees with every scale, and no strike at all.
        assert_eq!(strike_of("NIFTY26OCTFUT", "XX", 0.0), None);
        assert_eq!(strike_of("NIFTY26O1922900PE", "PE", 2_295_000.0), None);
        assert_eq!(strike_of("WEIRD26NOV500CE", "CE", 12_345.0), None);
        assert_eq!(strike_of("NIFTY26O1922900PE", "PE", 0.0), None);
        assert_eq!(strike_text(22.25), Some("22.25".into()));
        assert_eq!(strike_text(1400.0), Some("1400".into()));
        assert_eq!(strike_text(0.001), None);
    }

    #[test]
    fn expiries_follow_the_documented_epoch_rule_per_segment() {
        // nse_fo: epoch + 315511200 is 2026-11-03 00:00 UTC, an IST day later than nothing.
        assert_eq!(expiry_of("nse_fo", "1478152800"), Some(d(2026, 11, 3)));
        assert_eq!(expiry_of("nse_fo", " 1479967200 "), Some(d(2026, 11, 24)));
        // bse_fo: the plain epoch.
        assert_eq!(expiry_of("bse_fo", "1793664000"), Some(d(2026, 11, 3)));
        // An IST-midnight epoch (18:30 UTC the evening before) lands on the right day too.
        assert_eq!(expiry_of("bse_fo", "1793645400"), Some(d(2026, 11, 3)));
        // Dates written out, in the forms the engine accepts.
        assert_eq!(expiry_of("nse_fo", "03-Nov-2026"), Some(d(2026, 11, 3)));
        assert_eq!(expiry_of("nse_fo", "2026-11-03"), Some(d(2026, 11, 3)));
        assert_eq!(expiry_of("nse_fo", "03/11/2026"), Some(d(2026, 11, 3)));
        assert_eq!(expiry_of("nse_fo", ""), None);
        assert_eq!(expiry_of("nse_fo", "0"), None);
        assert_eq!(expiry_of("nse_fo", "-1"), None);
        assert_eq!(expiry_of("nse_fo", "soon"), None);
    }

    #[test]
    fn fields_split_on_commas_outside_quotes() {
        let fields = split_fields("2885,nse_cm,EQ, RELIANCE ,RELIANCE-EQ,,");
        let plain: Vec<&str> = fields.iter().map(|f| f.as_ref()).collect();
        assert_eq!(plain, vec!["2885", "nse_cm", "EQ", "RELIANCE", "RELIANCE-EQ", "", ""]);
        let fields = split_fields(r#""41222",nse_fo,"Larsen & Toubro, Ltd","say ""hi""",x"#);
        let quoted: Vec<&str> = fields.iter().map(|f| f.as_ref()).collect();
        assert_eq!(quoted, vec!["41222", "nse_fo", "Larsen & Toubro, Ltd", "say \"hi\"", "x"]);
    }

    #[test]
    fn the_header_is_matched_by_name_whatever_its_order_and_padding() {
        let c = Columns::from_header(
            "\u{feff}pSymbol,pGroup,pExchSeg,pInstType,pSymbolName,pTrdSymbol,pOptionType,pScripRefKey,pISIN,dTickSize ,lLotSize,lExpiryDate ,lMultiplier,lPrecision,dStrikePrice;,pDesc,lFreezeQty",
        );
        assert_eq!((c.token, c.group, c.exch_seg, c.inst_type), (Some(0), Some(1), Some(2), Some(3)));
        assert_eq!((c.symbol_name, c.trd_symbol, c.option_type, c.isin), (Some(4), Some(5), Some(6), Some(8)));
        assert_eq!((c.tick_size, c.lot_size, c.expiry, c.strike, c.desc), (Some(9), Some(10), Some(11), Some(14), Some(15)));
        assert!(c.usable());
        assert!(c.missing("nse_fo").is_empty());
        assert!(c.missing("nse_cm").is_empty());

        let cash = Columns::from_header("pSymbol,pExchSeg,pSymbolName,pTrdSymbol,lLotSize");
        assert_eq!(cash.missing("nse_cm"), vec!["pInstType", "dTickSize", "pISIN", "pDesc"]);
        assert_eq!(
            cash.missing("nse_fo"),
            vec!["pInstType", "pOptionType", "lExpiryDate", "dStrikePrice", "dTickSize", "pDesc"]
        );
        assert!(!Columns::from_header("pExchSeg,pSymbolName").usable());
    }

    #[test]
    fn tick_sizes_are_read_as_paise() {
        // The real files: every option 5, RELIANCE-EQ 10, IDEA-EQ 1, BAJAJ-AUTO-EQ 100.
        assert_eq!(tick_size_of("5"), 0.05);
        assert_eq!(tick_size_of(" 10 "), 0.10);
        assert_eq!(tick_size_of("1"), 0.01);
        assert_eq!(tick_size_of("100"), 1.0);
        assert_eq!(tick_size_of("500"), 5.0);
        // Already in rupees (no real file writes this), blank, junk.
        assert_eq!(tick_size_of("0.05"), 0.05);
        assert_eq!(tick_size_of(""), 0.05);
        assert_eq!(tick_size_of("-1"), 0.05);
        assert_eq!(tick_size_of("abc"), 0.05);
    }

    #[test]
    fn series_come_off_the_trading_symbol() {
        assert_eq!(split_series("RELIANCE-EQ"), ("RELIANCE", Some("EQ")));
        assert_eq!(split_series("BAJAJ-AUTO-EQ"), ("BAJAJ-AUTO", Some("EQ")));
        assert_eq!(split_series("M&M-BE"), ("M&M", Some("BE")));
        assert_eq!(split_series("SOMEONE-SM"), ("SOMEONE", Some("SM")));
        assert_eq!(split_series("Nifty 50"), ("Nifty 50", None));
        assert_eq!(split_series("BAJAJ-AUTO"), ("BAJAJ-AUTO", None));
        assert_eq!(split_series("-EQ"), ("-EQ", None));
    }

    #[test]
    fn the_six_indices_are_keyed_as_the_feed_names_them() {
        let rows = index_rows();
        assert_eq!(rows.len(), 6);
        let nifty = &rows[0];
        assert_eq!((nifty.id.as_str(), nifty.symbol.as_str(), nifty.name.as_str()), ("nse_cm:Nifty 50", "NIFTY", "Nifty 50"));
        assert_eq!((nifty.ws_key.as_str(), nifty.exchange, nifty.kind), ("idx|NIFTY 50", "NSE", Kind::Index));
        let sensex = rows.iter().find(|r| r.symbol == "SENSEX").unwrap();
        assert_eq!((sensex.id.as_str(), sensex.ws_key.as_str(), sensex.exchange), ("bse_cm:SENSEX", "idx|SENSEX", "BSE"));
        assert_eq!(derived_name("RELIANCE", Kind::Ce, d(2026, 11, 24), Some(1400.0)), "RELIANCE 24 NOV 26 1400 CE");
        assert_eq!(derived_name("RELIANCE", Kind::Fut, d(2026, 11, 24), None), "RELIANCE 24 NOV 26 FUT");
        assert_eq!(placeholders(2, 2), "(?,?),(?,?)");
    }
}
