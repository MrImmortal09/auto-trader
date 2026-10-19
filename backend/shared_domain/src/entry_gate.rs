//! Rules applied at the moment a Telegram call would be **bought**:
//!
//! 1. **VWAP + PCR indicator sizing.** The same score the Market Data page
//!    shows (ported from `MarketDataScreen.tsx`): Option Signal from the
//!    put/call ratio (+1 / −1 / 0) plus VWAP Signal from the index future vs
//!    its VWAP (+1 / −1 / 0), read at the last *closed* check. When it agrees
//!    with the call (BUY CE + bullish, BUY PE + bearish) the default lot count
//!    takes the agree multiplier — or the strong-agree one when PCR and VWAP
//!    both agree (±2); when it is neutral — or there is no fresh reading — it
//!    takes the neutral multiplier; when it disagrees the call is skipped.
//! 2. **Call validity.** A call that has not triggered within N minutes of
//!    arriving is dropped instead of waiting until the 15:39 cutoff.
//!
//! Pure and self-contained (std + serde derives only): no clock, no I/O, no
//! async. Callers pass "now" as an IST `YYYY-MM-DD HH:MM:SS` string — the
//! format every timestamp in this app already uses.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

/// Check intervals (minutes) — must match `TREND_INTERVALS` the Market page offers.
pub const INDICATOR_INTERVALS: [u32; 4] = [5, 15, 30, 60];
/// Strike windows the poller aggregates: ATM ± N strikes, 0 = the whole chain.
pub const INDICATOR_STRIKE_WINDOWS: [i64; 4] = [5, 10, 20, 0];
/// Indices the Upstox poller produces an indicator for.
pub const INDICATOR_INDICES: [&str; 3] = ["NIFTY", "BANKNIFTY", "SENSEX"];

/// A closed check older than `interval + this` minutes means the poller has
/// stopped keeping up (Upstox down, token revoked, server just restarted with
/// no data) — that reading is ignored and the call is treated as having no
/// fresh reading.
pub const STALE_GRACE_MINUTES: u32 = 2;

/// Keeps a runaway writer from growing the feed without bound: four strike
/// windows × one row a minute × a 376-minute session is ~1.5k rows per index.
const MAX_ROWS_PER_INDEX: usize = 4 * 420;

// ---------------------------------------------------------------------------
// Time helpers
// ---------------------------------------------------------------------------

/// `(date, seconds-of-day)` from an IST `YYYY-MM-DD HH:MM:SS` string.
fn parse_ts(ts: &str) -> Option<(&str, u32)> {
    let b = ts.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b' ' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let date = ts.get(0..10)?;
    let h: u32 = ts.get(11..13)?.parse().ok()?;
    let m: u32 = ts.get(14..16)?.parse().ok()?;
    let s: u32 = ts.get(17..19)?.parse().ok()?;
    if h > 23 || m > 59 || s > 60 {
        return None;
    }
    Some((date, h * 3600 + m * 60 + s))
}

fn hhmm(minute_of_day: u32) -> String {
    format!("{:02}:{:02}", minute_of_day / 60, minute_of_day % 60)
}

/// First check of the day: one minute after the 09:15 open.
const OPEN_CHECK_MINUTE: u32 = 9 * 60 + 16;
/// Last check of the day: one minute after the 15:30 close.
const CLOSE_CHECK_MINUTE: u32 = 15 * 60 + 31;

/// The check a snapshot taken at `minute` (of day, IST) belongs to: the first
/// "interval boundary + 1 minute" at or after it — 09:16, 09:31, 09:46… for
/// 15 min. Boundaries are clock-aligned multiples of `interval`; 09:16 is
/// always the first check and 15:31 always the last, regardless of interval
/// (so 60 min → 09:16, 10:01, 11:01 … 15:01, 15:31).
/// The +1 lets the boundary minute fully complete before it is read.
pub fn check_minute(minute: u32, interval: u32) -> u32 {
    let interval = interval.max(1);
    if minute <= OPEN_CHECK_MINUTE {
        return OPEN_CHECK_MINUTE;
    }
    ((minute - 1).div_ceil(interval) * interval + 1).min(CLOSE_CHECK_MINUTE.max(minute))
}

// ---------------------------------------------------------------------------
// Feed — today's per-minute option-chain aggregates, per index
// ---------------------------------------------------------------------------

/// One aggregated option-chain minute for an index over one strike window —
/// the in-memory twin of an `oi_snapshots` row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OiMinute {
    /// IST `YYYY-MM-DD HH:MM:SS` the poll was taken.
    pub ts: String,
    /// Strikes each side of ATM included; 0 = the whole chain.
    pub strike_window: i64,
    pub call_oi: f64,
    pub put_oi: f64,
    pub call_oi_chg: f64,
    pub put_oi_chg: f64,
    pub spot: f64,
    pub fut_ltp: Option<f64>,
    pub fut_vwap: Option<f64>,
}

/// Today's minute snapshots per index id (`"NIFTY"`, …). Written once a minute
/// by the server's Upstox poller (and seeded from `oi_snapshots` at startup),
/// read by the position monitor only at the moment an entry triggers.
///
/// A std (not tokio) lock on purpose: the engine's decision functions are
/// synchronous, writes are tiny and once a minute, and the guard is never
/// held across an `.await`.
pub type IndicatorFeed = Arc<RwLock<HashMap<String, Vec<OiMinute>>>>;

pub fn new_indicator_feed() -> IndicatorFeed {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Append one poll's rows for `index`. Rows from any other day than the
/// incoming ones are dropped first, so the feed only ever holds one session.
pub fn feed_push(feed: &IndicatorFeed, index: &str, rows: Vec<OiMinute>) {
    let Some(day) = rows.first().and_then(|r| parse_ts(&r.ts)).map(|(d, _)| d.to_string()) else {
        return;
    };
    // A poisoned lock only means a writer panicked mid-push; the data is
    // still a valid Vec, and a reader would at worst see a stale reading
    // (which the staleness rule already turns into "no fresh reading").
    let mut guard = feed.write().unwrap_or_else(|e| e.into_inner());
    let list = guard.entry(index.to_string()).or_default();
    list.retain(|r| r.ts.starts_with(day.as_str()));
    list.extend(rows);
    if list.len() > MAX_ROWS_PER_INDEX {
        let excess = list.len() - MAX_ROWS_PER_INDEX;
        list.drain(..excess);
    }
}

/// The last closed reading for `index` straight from the feed.
pub fn feed_reading(
    feed: &IndicatorFeed,
    index: &str,
    params: &IndicatorParams,
    now_ts: &str,
) -> Option<IndicatorReading> {
    let guard = feed.read().unwrap_or_else(|e| e.into_inner());
    let rows = guard.get(index)?;
    last_closed_reading(rows, index, params, now_ts)
}

// ---------------------------------------------------------------------------
// Scoring — mirrors MarketDataScreen.tsx exactly
// ---------------------------------------------------------------------------

/// Indicator settings (stored on `TradingConfig`, edited on the Settings page).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct IndicatorParams {
    /// Minutes per check — one of [`INDICATOR_INTERVALS`].
    pub interval_min: u32,
    /// `true` = total OI, `false` = change in OI since yesterday's close.
    pub total_oi: bool,
    /// One of [`INDICATOR_STRIKE_WINDOWS`].
    pub strike_window: i64,
    /// Option Signal = BUY (+1) when PCR is above this.
    pub pcr_buy_above: f64,
    /// Option Signal = SELL (−1) when PCR is below this.
    pub pcr_sell_below: f64,
    /// VWAP Signal is 0 while the future is within ± this % of VWAP.
    pub vwap_band_pct: f64,
    /// Opening rule (see [`last_closed_reading`]): the 09:16 check always
    /// reads NEUTRAL, and until the first regular check closes the latest
    /// closed 5-minute check (09:21, 09:26, …) is used instead.
    #[serde(default)]
    pub early_checks: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Score {
    /// Call / Put side actually compared (OI or change in OI, per `total_oi`).
    pub call: f64,
    pub put: f64,
    /// Put ÷ Call, only when both sides are positive.
    pub ratio: Option<f64>,
    /// +1 BUY / −1 SELL / 0 from the PCR thresholds.
    pub option: i32,
    /// +1 BUY / −1 SELL / 0 from future vs VWAP.
    pub vwap: i32,
    /// `option + vwap`, −2 … +2. Above 0 is BUY, below 0 SELL, 0 NEUTRAL.
    pub overall: i32,
}

/// PCR is only meaningful when both sides are positive (change-in-OI can go negative).
pub fn pcr(call: f64, put: f64) -> Option<f64> {
    (call > 0.0 && put > 0.0).then(|| put / call)
}

/// +1 BUY / −1 SELL from the PCR thresholds; falls back to the sign of
/// Put − Call when PCR is undefined (a negative change-in-OI side).
pub fn option_score(call: f64, put: f64, p: &IndicatorParams) -> i32 {
    match pcr(call, put) {
        Some(ratio) if ratio > p.pcr_buy_above => 1,
        Some(ratio) if ratio < p.pcr_sell_below => -1,
        Some(_) => 0,
        None if put > call => 1,
        None if put < call => -1,
        None => 0,
    }
}

/// +1 BUY when the future trades above VWAP (+ band), −1 below (− band).
/// A missing or non-positive price / VWAP is bad data, not a signal: 0.
pub fn vwap_score(price: Option<f64>, vwap: Option<f64>, p: &IndicatorParams) -> i32 {
    let (Some(price), Some(vwap)) = (price, vwap) else { return 0 };
    if price <= 0.0 || vwap <= 0.0 {
        return 0;
    }
    let band = vwap * p.vwap_band_pct / 100.0;
    if price > vwap + band {
        1
    } else if price < vwap - band {
        -1
    } else {
        0
    }
}

pub fn score(row: &OiMinute, p: &IndicatorParams) -> Score {
    let (call, put) = if p.total_oi { (row.call_oi, row.put_oi) } else { (row.call_oi_chg, row.put_oi_chg) };
    let option = option_score(call, put, p);
    let vwap = vwap_score(row.fut_ltp, row.fut_vwap, p);
    Score { call, put, ratio: pcr(call, put), option, vwap, overall: option + vwap }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Bias {
    Bullish,
    Bearish,
    Neutral,
}

impl Bias {
    pub fn of_overall(overall: i32) -> Self {
        match overall {
            o if o > 0 => Bias::Bullish,
            o if o < 0 => Bias::Bearish,
            _ => Bias::Neutral,
        }
    }

    /// The word the Market Data page uses for this bias.
    pub fn word(self) -> &'static str {
        match self {
            Bias::Bullish => "BUY",
            Bias::Bearish => "SELL",
            Bias::Neutral => "NEUTRAL",
        }
    }
}

/// One closed check for an index, scored with the current settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndicatorReading {
    pub index: String,
    /// Check label `HH:MM` the reading belongs to, e.g. `"11:01"`.
    pub check_time: String,
    /// When the snapshot behind it was polled (IST).
    pub ts: String,
    pub score: Score,
    pub bias: Bias,
    /// The opening 09:16 check under the early-check rule: `bias` is forced
    /// to NEUTRAL whatever `score` says.
    #[serde(default)]
    pub opening: bool,
}

impl IndicatorReading {
    /// e.g. `+2 BUY at the 11:01 check (PCR 1.24 BUY, VWAP BUY)`.
    pub fn describe(&self) -> String {
        let s = &self.score;
        let pcr = match s.ratio {
            Some(r) => format!("PCR {r:.2} {}", component_word(s.option)),
            None => format!("PCR n/a, Put−Call {}", component_word(s.option)),
        };
        let overall = if s.overall > 0 { format!("+{}", s.overall) } else { s.overall.to_string() };
        if self.opening {
            return format!(
                "opening {} check, always NEUTRAL (raw {overall}: {pcr}, VWAP {})",
                self.check_time,
                component_word(s.vwap)
            );
        }
        format!(
            "{overall} {} at the {} check ({pcr}, VWAP {})",
            self.bias.word(),
            self.check_time,
            component_word(s.vwap)
        )
    }
}

fn component_word(score: i32) -> &'static str {
    match score {
        s if s > 0 => "BUY",
        s if s < 0 => "SELL",
        _ => "neutral",
    }
}

/// The last *closed* check for one index — exactly what the Market Data
/// cards show as their headline badge — or `None` when there isn't a fresh
/// one.
///
/// Snapshots are bucketed by [`check_minute`]; a bucket is still open
/// ("live") until the snapshot for its own label minute has landed. The
/// newest bucket that is not live wins, provided its snapshot is no older
/// than `interval + STALE_GRACE_MINUTES` minutes. Only `now_ts`'s date
/// counts, and only rows for `params.strike_window`.
///
/// With `params.early_checks` on, the open is handled separately: the 09:16
/// check always reads NEUTRAL (one minute of data is not a reading), and
/// until the first regular check closes (09:31 for 15 / 30 min, 10:01 for
/// 60 min) the latest closed **5-minute** check — 09:21, 09:26, … — is used
/// instead, scored exactly the same way. After that the regular interval
/// takes over.
pub fn last_closed_reading(
    rows: &[OiMinute],
    index: &str,
    params: &IndicatorParams,
    now_ts: &str,
) -> Option<IndicatorReading> {
    let interval = if INDICATOR_INTERVALS.contains(&params.interval_min) { params.interval_min } else { 15 };
    let regular = closed_reading(rows, index, params, interval, now_ts);
    if !params.early_checks {
        return regular;
    }
    let is_opening = |r: &IndicatorReading| r.check_time == hhmm(OPEN_CHECK_MINUTE);
    if regular.as_ref().is_some_and(|r| !is_opening(r)) {
        return regular;
    }
    // Nothing later than 09:16 has closed on the regular interval. While the
    // first regular check is still ahead (plus the usual grace for its
    // snapshot to land), fall back to 5-minute checks.
    let (_, now_secs) = parse_ts(now_ts)?;
    let first_regular = check_minute(OPEN_CHECK_MINUTE + 1, interval);
    let early = if now_secs / 60 <= first_regular + STALE_GRACE_MINUTES {
        closed_reading(rows, index, params, EARLY_CHECK_INTERVAL, now_ts)
    } else {
        None
    };
    match early {
        Some(r) if !is_opening(&r) => Some(r),
        // Still only the 09:16 check (from either interval): NEUTRAL.
        Some(mut r) => {
            r.bias = Bias::Neutral;
            r.opening = true;
            Some(r)
        }
        None => regular.map(|mut r| {
            r.bias = Bias::Neutral;
            r.opening = true;
            r
        }),
    }
}

/// Interval used between the open and the first regular check when
/// `IndicatorParams::early_checks` is on.
pub const EARLY_CHECK_INTERVAL: u32 = 5;

/// The newest closed check on `interval`-minute buckets — see
/// [`last_closed_reading`].
fn closed_reading(
    rows: &[OiMinute],
    index: &str,
    params: &IndicatorParams,
    interval: u32,
    now_ts: &str,
) -> Option<IndicatorReading> {
    let (today, now_secs) = parse_ts(now_ts)?;
    let now_minute = now_secs / 60;

    // label minute → (row, snapshot seconds-of-day); the latest snapshot in
    // each bucket wins, without assuming the rows arrive in time order.
    let mut buckets: BTreeMap<u32, (&OiMinute, u32)> = BTreeMap::new();
    for row in rows {
        if row.strike_window != params.strike_window {
            continue;
        }
        let Some((day, secs)) = parse_ts(&row.ts) else { continue };
        if day != today {
            continue;
        }
        let label = check_minute(secs / 60, interval);
        let newer = buckets.get(&label).is_none_or(|(_, prev)| secs >= *prev);
        if newer {
            buckets.insert(label, (row, secs));
        }
    }

    for (&label, &(row, secs)) in buckets.iter().rev() {
        let live = secs / 60 < label && now_minute <= label;
        if live {
            continue;
        }
        let age_secs = now_secs.saturating_sub(secs);
        if age_secs > (interval + STALE_GRACE_MINUTES) * 60 {
            return None;
        }
        let sc = score(row, params);
        return Some(IndicatorReading {
            index: index.to_string(),
            check_time: hhmm(label),
            ts: row.ts.clone(),
            score: sc,
            bias: Bias::of_overall(sc.overall),
            opening: false,
        });
    }
    None
}

// ---------------------------------------------------------------------------
// Sizing
// ---------------------------------------------------------------------------

/// Which index's indicator reads the market for `instrument`. BANKNIFTY and
/// SENSEX have their own; everything else — NIFTY itself, FINNIFTY,
/// MIDCPNIFTY, BANKEX and every stock option — follows NIFTY as the broad
/// market's sentiment.
pub fn indicator_parent(instrument: &str) -> &'static str {
    match instrument.trim().to_uppercase().as_str() {
        "BANKNIFTY" => "BANKNIFTY",
        "SENSEX" => "SENSEX",
        _ => "NIFTY",
    }
}

/// How firmly the indicator backs a call it does not veto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agreement {
    /// NEUTRAL, or no fresh reading.
    Neutral,
    /// Score ±1 in the call's direction: one of PCR / VWAP agrees and the
    /// other is neutral.
    Agree,
    /// Score ±2 in the call's direction: PCR and VWAP both agree — the
    /// reading the Market Data page shows as a solid badge.
    Strong,
}

impl Agreement {
    /// The name the Settings page gives this tier's lot size.
    pub fn size_name(self) -> &'static str {
        match self {
            Agreement::Neutral => "Neutral",
            Agreement::Agree => "Agrees",
            Agreement::Strong => "Strong Agrees",
        }
    }
}

/// Lot multiplier for each [`Agreement`] tier (stored on `TradingConfig`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Multipliers {
    pub strong: f64,
    pub agree: f64,
    pub neutral: f64,
}

impl Multipliers {
    pub fn of(&self, tier: Agreement) -> f64 {
        match tier {
            Agreement::Neutral => self.neutral,
            Agreement::Agree => self.agree,
            Agreement::Strong => self.strong,
        }
    }
}

/// What the indicator says about buying a call right now.
#[derive(Debug, Clone, PartialEq)]
pub enum SizingVerdict {
    /// Buy, with the default lot count scaled by `multiplier` — the
    /// [`Multipliers`] entry for `agreement`.
    Scale { multiplier: f64, agreement: Agreement, note: String },
    /// Do not buy; `reason` says why, for the log.
    Skip { reason: String },
}

/// The indicator verdict for a `BUY <option_type>` call on `instrument`,
/// given the parent index's last closed reading (`None` = no fresh reading,
/// which is treated as NEUTRAL).
pub fn sizing_verdict(
    option_type: &str,
    instrument: &str,
    reading: Option<&IndicatorReading>,
    multipliers: &Multipliers,
) -> SizingVerdict {
    let opt = option_type.trim().to_uppercase();
    // Never expected (the parser only emits CE / PE), but an option the
    // indicator can't be matched against is not traded on a guess.
    let Some(call_bias) = call_bias(&opt) else {
        return SizingVerdict::Skip { reason: unknown_option_type(option_type) };
    };
    let source = indicator_source(instrument);

    let neutral = multipliers.neutral;
    let Some(reading) = reading else {
        return SizingVerdict::Scale {
            multiplier: neutral,
            agreement: Agreement::Neutral,
            note: format!("{source} has no fresh reading — treated as NEUTRAL, ×{neutral}"),
        };
    };
    let desc = reading.describe();
    match reading.bias {
        Bias::Neutral => SizingVerdict::Scale {
            multiplier: neutral,
            agreement: Agreement::Neutral,
            note: format!("{source} {desc} — NEUTRAL, ×{neutral}"),
        },
        // `bias` is the sign of the score, so ±2 here is PCR and VWAP both
        // on the call's side; ±1 is one of them with the other neutral.
        b if b == call_bias && reading.score.overall.abs() >= 2 => {
            let m = multipliers.strong;
            SizingVerdict::Scale {
                multiplier: m,
                agreement: Agreement::Strong,
                note: format!("{source} {desc} strongly agrees with BUY {opt}, ×{m}"),
            }
        }
        b if b == call_bias => {
            let m = multipliers.agree;
            SizingVerdict::Scale {
                multiplier: m,
                agreement: Agreement::Agree,
                note: format!("{source} {desc} agrees with BUY {opt}, ×{m}"),
            }
        }
        _ => SizingVerdict::Skip {
            reason: format!("{source} {desc} disagrees with BUY {opt} — call skipped"),
        },
    }
}

/// The bias a `BUY <opt>` call needs (`opt` already upper-cased): bullish
/// for CE, bearish for PE, `None` for anything else.
fn call_bias(opt: &str) -> Option<Bias> {
    match opt {
        "CE" => Some(Bias::Bullish),
        "PE" => Some(Bias::Bearish),
        _ => None,
    }
}

fn unknown_option_type(option_type: &str) -> String {
    format!("unknown option type '{}' — cannot match it against the indicator", option_type.trim())
}

/// `"NIFTY indicator"`, or `"NIFTY indicator (parent of FINNIFTY)"` when
/// `instrument` follows another index's reading.
fn indicator_source(instrument: &str) -> String {
    let parent = indicator_parent(instrument);
    if instrument.trim().eq_ignore_ascii_case(parent) {
        format!("{parent} indicator")
    } else {
        format!("{parent} indicator (parent of {})", instrument.trim().to_uppercase())
    }
}

/// Whether the indicator lets a Telegram instruction **lower** the stop on a
/// `BUY <option_type>` call (`TradingConfig::indicator_sl_lower_gate`).
///
/// A lower stop is more risk on a position we hold, so only a strong agree
/// allows it: score +2 for BUY CE, −2 for BUY PE, PCR and VWAP both on the
/// call's side. Anything less refuses and the stop stays where it is: a
/// plain ±1 agree, NEUTRAL, the opening 09:16 check (always NEUTRAL under
/// the early-check rule), no fresh reading, or a reading against the call.
///
/// `Ok(note)` when the lower stop may be applied, `Err(reason)` when not —
/// both for the log.
pub fn sl_lower_verdict(
    option_type: &str,
    instrument: &str,
    reading: Option<&IndicatorReading>,
) -> Result<String, String> {
    let opt = option_type.trim().to_uppercase();
    let Some(call_bias) = call_bias(&opt) else {
        return Err(unknown_option_type(option_type));
    };
    let source = indicator_source(instrument);
    let Some(reading) = reading else {
        return Err(format!("{source} has no fresh reading — a lower stop needs a strong agree with BUY {opt}"));
    };
    let desc = reading.describe();
    if reading.bias == call_bias && reading.score.overall.abs() >= 2 {
        Ok(format!("{source} {desc} strongly agrees with BUY {opt}"))
    } else {
        Err(format!("{source} {desc} — a lower stop needs a strong agree with BUY {opt}"))
    }
}

/// Which reading tops up an open call, and up to which tier's size
/// (`TradingConfig::indicator_topup_mode`). Only consulted while
/// `TradingConfig::indicator_topup` is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopupMode {
    /// Only a STRONG agree tops up, and only to the Agrees size. The default,
    /// and the rule that buys least.
    StrongToAgree,
    /// Only a STRONG agree tops up, straight to the Strong Agrees size.
    StrongToStrong,
    /// Any agreement tops up, to that reading's own size. Once per call.
    ToTier,
    /// As [`TopupMode::ToTier`], but a call sitting at the Agrees size —
    /// bought there, or topped up to it — is topped up once more to the
    /// Strong Agrees size when the reading turns strong.
    Step,
}

impl TopupMode {
    pub const ALL: [TopupMode; 4] =
        [TopupMode::StrongToAgree, TopupMode::StrongToStrong, TopupMode::ToTier, TopupMode::Step];

    /// The stored / wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            TopupMode::StrongToAgree => "strong_to_agree",
            TopupMode::StrongToStrong => "strong_to_strong",
            TopupMode::ToTier => "tier",
            TopupMode::Step => "step",
        }
    }

    /// Anything unrecognised is the default — the rule that buys least.
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        Self::ALL.into_iter().find(|m| m.as_str().eq_ignore_ascii_case(s)).unwrap_or(TopupMode::StrongToAgree)
    }

    /// The tier whose size a top-up brings the holding up to when the
    /// indicator currently reads `now`; `None` = this reading tops up nothing.
    pub fn target(self, now: Agreement) -> Option<Agreement> {
        match (self, now) {
            (_, Agreement::Neutral) => None,
            (TopupMode::StrongToAgree, Agreement::Strong) => Some(Agreement::Agree),
            (TopupMode::StrongToStrong, Agreement::Strong) => Some(Agreement::Strong),
            (TopupMode::StrongToAgree | TopupMode::StrongToStrong, Agreement::Agree) => None,
            (TopupMode::ToTier | TopupMode::Step, tier) => Some(tier),
        }
    }

    /// True for the one rule that may top the same call up twice.
    pub fn steps(self) -> bool {
        matches!(self, TopupMode::Step)
    }
}

/// `base_lots × multiplier`, rounded to the nearest whole lot with exact
/// halves rounded **down** (toward the smaller position), and never below
/// one lot. `base_lots <= 0` stays 0 (that class is not auto-traded at all).
///
/// 1 lot × 0.5 → 1, 1 × 1.5 → 1, 2 × 1.5 → 3, 3 × 0.5 → 1, 3 × 1.5 → 4.
pub fn scale_lots(base_lots: i32, multiplier: f64) -> i32 {
    if base_lots <= 0 {
        return 0;
    }
    if !multiplier.is_finite() || multiplier <= 0.0 {
        return 1;
    }
    let scaled = base_lots as f64 * multiplier;
    // `x - 0.5` ceiled is "nearest, halves down"; the epsilon keeps a value
    // like 1.5000000000000002 (0.1-step float noise) on the down side of the tie.
    let lots = (scaled - 0.5 - 1e-9).ceil();
    if lots >= i32::MAX as f64 {
        return i32::MAX;
    }
    (lots as i32).max(1)
}

// ---------------------------------------------------------------------------
// Call validity
// ---------------------------------------------------------------------------

/// True once a call created at `created_at` has gone `validity_minutes`
/// without being bought. `validity_minutes <= 0` disables the rule. A call
/// from an earlier day is always expired. An unparseable `created_at` returns
/// `false` — the engine's stale-carryover rule already drops those.
pub fn call_expired(created_at: &str, validity_minutes: i32, now_ts: &str) -> bool {
    if validity_minutes <= 0 {
        return false;
    }
    let (Some((created_day, created_secs)), Some((today, now_secs))) = (parse_ts(created_at), parse_ts(now_ts)) else {
        return false;
    };
    if created_day != today {
        return created_day < today;
    }
    now_secs >= created_secs + validity_minutes as u32 * 60
}

/// `HH:MM` (IST) a call created at `created_at` stops being valid, or `None`
/// when the rule is off, the timestamp is unparseable, or it would run past
/// midnight.
pub fn call_expires_at(created_at: &str, validity_minutes: i32) -> Option<String> {
    if validity_minutes <= 0 {
        return None;
    }
    let (_, secs) = parse_ts(created_at)?;
    let end = secs + validity_minutes as u32 * 60;
    (end < 24 * 3600).then(|| hhmm(end / 60))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const fn hm(h: u32, m: u32) -> u32 {
        h * 60 + m
    }

    fn params() -> IndicatorParams {
        IndicatorParams {
            interval_min: 15,
            total_oi: false,
            strike_window: 10,
            pcr_buy_above: 1.0,
            pcr_sell_below: 1.0,
            vwap_band_pct: 0.0,
            early_checks: false,
        }
    }

    /// A NIFTY minute with change-in-OI call/put and future vs VWAP.
    fn minute(ts: &str, call_chg: f64, put_chg: f64, fut: f64, vwap: f64) -> OiMinute {
        OiMinute {
            ts: ts.to_string(),
            strike_window: 10,
            call_oi: 1_000.0,
            put_oi: 1_000.0,
            call_oi_chg: call_chg,
            put_oi_chg: put_chg,
            spot: 25_000.0,
            fut_ltp: Some(fut),
            fut_vwap: Some(vwap),
        }
    }

    fn bullish(ts: &str) -> OiMinute {
        minute(ts, 100.0, 200.0, 25_100.0, 25_000.0) // PCR 2.0 BUY, above VWAP → +2
    }

    fn bearish(ts: &str) -> OiMinute {
        minute(ts, 200.0, 100.0, 24_900.0, 25_000.0) // PCR 0.5 SELL, below VWAP → −2
    }

    fn reading_of(bias: Bias) -> IndicatorReading {
        scored(match bias {
            Bias::Bullish => 2,
            Bias::Bearish => -2,
            Bias::Neutral => 0,
        })
    }

    /// A reading with the given overall score: ±2 is PCR and VWAP together,
    /// ±1 is PCR alone with VWAP neutral.
    fn scored(overall: i32) -> IndicatorReading {
        let option = overall.signum();
        IndicatorReading {
            index: "NIFTY".into(),
            check_time: "11:01".into(),
            ts: "2026-10-09 11:01:02".into(),
            score: Score { call: 1.0, put: 1.0, ratio: Some(1.0), option, vwap: overall - option, overall },
            bias: Bias::of_overall(overall),
            opening: false,
        }
    }

    /// The requested tiers: strong ×1.5, agree ×1, neutral ×0.5.
    const MULTS: Multipliers = Multipliers { strong: 1.5, agree: 1.0, neutral: 0.5 };

    // ── check_minute (moved from routes/market.rs, same cases) ───────────

    #[test]
    fn checks_one_minute_after_each_boundary() {
        for iv in [5, 15, 30, 60] {
            assert_eq!(check_minute(hm(9, 15), iv), hm(9, 16));
            assert_eq!(check_minute(hm(9, 16), iv), hm(9, 16));
        }
        assert_eq!(check_minute(hm(9, 17), 5), hm(9, 21));
        assert_eq!(check_minute(hm(9, 21), 5), hm(9, 21));
        assert_eq!(check_minute(hm(9, 17), 15), hm(9, 31));
        assert_eq!(check_minute(hm(9, 31), 15), hm(9, 31));
        assert_eq!(check_minute(hm(9, 32), 15), hm(9, 46));
        assert_eq!(check_minute(hm(9, 17), 30), hm(9, 31));
        assert_eq!(check_minute(hm(9, 32), 30), hm(10, 1));
        assert_eq!(check_minute(hm(9, 17), 60), hm(10, 1));
        assert_eq!(check_minute(hm(10, 2), 60), hm(11, 1));
        assert_eq!(check_minute(hm(15, 30), 15), hm(15, 31));
        assert_eq!(check_minute(hm(15, 1), 60), hm(15, 1));
        assert_eq!(check_minute(hm(15, 2), 60), hm(15, 31));
        assert_eq!(check_minute(hm(15, 31), 60), hm(15, 31));
    }

    // ── scoring parity with MarketDataScreen.tsx ─────────────────────────

    #[test]
    fn option_score_uses_thresholds_then_falls_back_to_sign() {
        let mut p = params();
        p.pcr_buy_above = 1.2;
        p.pcr_sell_below = 0.8;
        assert_eq!(option_score(100.0, 130.0, &p), 1); // 1.3 > 1.2
        assert_eq!(option_score(100.0, 70.0, &p), -1); // 0.7 < 0.8
        assert_eq!(option_score(100.0, 100.0, &p), 0); // between
        assert_eq!(option_score(100.0, 120.0, &p), 0); // exactly at buy threshold is not above
        // A negative change-in-OI side → PCR undefined → sign of Put − Call.
        assert_eq!(option_score(-50.0, 10.0, &p), 1);
        assert_eq!(option_score(50.0, -10.0, &p), -1);
        assert_eq!(option_score(0.0, 0.0, &p), 0);
        assert_eq!(pcr(-1.0, 5.0), None);
    }

    #[test]
    fn vwap_score_respects_the_band_and_missing_data() {
        let mut p = params();
        assert_eq!(vwap_score(Some(101.0), Some(100.0), &p), 1);
        assert_eq!(vwap_score(Some(99.0), Some(100.0), &p), -1);
        assert_eq!(vwap_score(Some(100.0), Some(100.0), &p), 0);
        assert_eq!(vwap_score(None, Some(100.0), &p), 0);
        assert_eq!(vwap_score(Some(100.0), None, &p), 0);
        // A zero / negative VWAP or price is bad data, never a BUY.
        assert_eq!(vwap_score(Some(100.0), Some(0.0), &p), 0);
        assert_eq!(vwap_score(Some(100.0), Some(-5.0), &p), 0);
        assert_eq!(vwap_score(Some(0.0), Some(100.0), &p), 0);
        p.vwap_band_pct = 2.0; // ±2
        assert_eq!(vwap_score(Some(101.5), Some(100.0), &p), 0);
        assert_eq!(vwap_score(Some(102.5), Some(100.0), &p), 1);
        assert_eq!(vwap_score(Some(97.5), Some(100.0), &p), -1);
    }

    #[test]
    fn score_picks_the_configured_oi_basis() {
        let mut row = minute("2026-10-09 11:01:02", 100.0, 50.0, 25_000.0, 25_000.0);
        row.call_oi = 100.0;
        row.put_oi = 300.0;
        let mut p = params();
        assert_eq!(score(&row, &p).option, -1); // change in OI: PCR 0.5
        p.total_oi = true;
        let s = score(&row, &p);
        assert_eq!((s.option, s.vwap, s.overall), (1, 0, 1)); // total OI: PCR 3.0
        assert_eq!(s.ratio, Some(3.0));
    }

    // ── last closed check ────────────────────────────────────────────────

    #[test]
    fn reads_the_last_closed_check_not_the_open_one() {
        let rows = vec![
            bearish("2026-10-09 11:00:02"),
            bearish("2026-10-09 11:01:02"), // closes the 11:01 check
            bullish("2026-10-09 11:05:02"), // inside the still-open 11:16 bucket
        ];
        let r = last_closed_reading(&rows, "NIFTY", &params(), "2026-10-09 11:05:30").unwrap();
        assert_eq!(r.check_time, "11:01");
        assert_eq!(r.ts, "2026-10-09 11:01:02");
        assert_eq!(r.bias, Bias::Bearish);
        assert_eq!(r.score.overall, -2);
    }

    #[test]
    fn a_check_closes_once_its_own_minute_lands() {
        let mut rows = vec![bearish("2026-10-09 11:01:02"), bullish("2026-10-09 11:15:02")];
        // 11:16:01 — the 11:16 snapshot isn't in yet, so 11:16 is still open.
        let r = last_closed_reading(&rows, "NIFTY", &params(), "2026-10-09 11:16:01").unwrap();
        assert_eq!(r.check_time, "11:01");
        rows.push(bullish("2026-10-09 11:16:02"));
        let r = last_closed_reading(&rows, "NIFTY", &params(), "2026-10-09 11:16:05").unwrap();
        assert_eq!((r.check_time.as_str(), r.bias), ("11:16", Bias::Bullish));
        // A missed 11:16 poll: once 11:16 has passed the bucket closes on 11:15.
        rows.pop();
        let r = last_closed_reading(&rows, "NIFTY", &params(), "2026-10-09 11:17:10").unwrap();
        assert_eq!((r.check_time.as_str(), r.ts.as_str()), ("11:16", "2026-10-09 11:15:02"));
    }

    fn early() -> IndicatorParams {
        IndicatorParams { early_checks: true, ..params() }
    }

    #[test]
    fn early_checks_make_the_opening_check_neutral() {
        // 09:16 has closed and reads +2 BUY; with the opening rule it is NEUTRAL.
        let rows = vec![bullish("2026-10-09 09:16:02")];
        let now = "2026-10-09 09:18:00";
        assert_eq!(last_closed_reading(&rows, "NIFTY", &params(), now).unwrap().bias, Bias::Bullish);
        let r = last_closed_reading(&rows, "NIFTY", &early(), now).unwrap();
        assert_eq!((r.bias, r.opening, r.check_time.as_str()), (Bias::Neutral, true, "09:16"));
        assert!(r.describe().contains("always NEUTRAL"), "{}", r.describe());
        // Still the opening check at 09:21:30 — the 09:21 snapshot hasn't landed.
        let r = last_closed_reading(&rows, "NIFTY", &early(), "2026-10-09 09:21:30").unwrap();
        assert!(r.opening);
    }

    #[test]
    fn early_checks_use_five_minute_checks_until_the_first_regular_one() {
        let mut rows = vec![
            bullish("2026-10-09 09:16:02"),
            bearish("2026-10-09 09:20:02"),
            bearish("2026-10-09 09:21:02"),
        ];
        // 09:22: the 09:21 five-minute check has closed and is used as scored.
        let r = last_closed_reading(&rows, "NIFTY", &early(), "2026-10-09 09:22:00").unwrap();
        assert_eq!((r.bias, r.opening, r.check_time.as_str()), (Bias::Bearish, false, "09:21"));
        // Without the rule the 15-minute reading is still the 09:16 check.
        let r = last_closed_reading(&rows, "NIFTY", &params(), "2026-10-09 09:22:00").unwrap();
        assert_eq!(r.check_time, "09:16");

        // 09:27: the 09:26 check takes over.
        rows.push(bullish("2026-10-09 09:26:02"));
        let r = last_closed_reading(&rows, "NIFTY", &early(), "2026-10-09 09:27:00").unwrap();
        assert_eq!((r.bias, r.check_time.as_str()), (Bias::Bullish, "09:26"));

        // 09:31 lands: the regular 15-minute check is back in charge, and a
        // later 5-minute bucket no longer overrides it.
        rows.push(bearish("2026-10-09 09:31:02"));
        rows.push(bullish("2026-10-09 09:36:02"));
        let r = last_closed_reading(&rows, "NIFTY", &early(), "2026-10-09 09:37:00").unwrap();
        assert_eq!((r.bias, r.opening, r.check_time.as_str()), (Bias::Bearish, false, "09:31"));
    }

    #[test]
    fn early_checks_do_not_revive_five_minute_readings_later_in_the_day() {
        // The poller was down from the open until 11:06. The regular interval
        // has nothing closed and fresh; a lone 5-minute bucket must not stand in.
        let rows = vec![bullish("2026-10-09 11:06:02")];
        assert!(last_closed_reading(&rows, "NIFTY", &early(), "2026-10-09 11:07:00").is_none());
    }

    #[test]
    fn nothing_closed_before_the_first_check() {
        let rows = vec![bullish("2026-10-09 09:15:02")];
        assert_eq!(last_closed_reading(&rows, "NIFTY", &params(), "2026-10-09 09:15:40"), None);
        let rows = vec![bullish("2026-10-09 09:15:02"), bullish("2026-10-09 09:16:02")];
        let r = last_closed_reading(&rows, "NIFTY", &params(), "2026-10-09 09:16:10").unwrap();
        assert_eq!(r.check_time, "09:16");
    }

    #[test]
    fn a_stalled_poller_gives_no_reading() {
        let rows = vec![bullish("2026-10-09 10:30:02")];
        // 15-min interval: allowed up to 17 minutes old.
        assert!(last_closed_reading(&rows, "NIFTY", &params(), "2026-10-09 10:46:59").is_some());
        assert_eq!(last_closed_reading(&rows, "NIFTY", &params(), "2026-10-09 10:47:10"), None);
        // 60-min interval tolerates a check that is up to 62 minutes old.
        let mut p = params();
        p.interval_min = 60;
        assert!(last_closed_reading(&rows, "NIFTY", &p, "2026-10-09 11:20:00").is_some());
    }

    #[test]
    fn only_today_and_only_the_configured_window_count() {
        let mut other_window = bearish("2026-10-09 11:01:02");
        other_window.strike_window = 20;
        let rows = vec![bullish("2026-10-08 15:31:02"), other_window];
        assert_eq!(last_closed_reading(&rows, "NIFTY", &params(), "2026-10-09 11:05:00"), None);
        let mut p = params();
        p.strike_window = 20;
        let r = last_closed_reading(&rows, "NIFTY", &p, "2026-10-09 11:05:00").unwrap();
        assert_eq!(r.bias, Bias::Bearish);
    }

    #[test]
    fn rows_out_of_order_still_use_the_latest_snapshot_per_check() {
        let rows = vec![bearish("2026-10-09 11:01:02"), bullish("2026-10-09 10:59:02")];
        let r = last_closed_reading(&rows, "NIFTY", &params(), "2026-10-09 11:05:00").unwrap();
        assert_eq!((r.ts.as_str(), r.bias), ("2026-10-09 11:01:02", Bias::Bearish));
    }

    #[test]
    fn feed_keeps_one_day_and_reads_through() {
        let feed = new_indicator_feed();
        feed_push(&feed, "NIFTY", vec![bullish("2026-10-08 15:31:02")]);
        feed_push(&feed, "NIFTY", vec![bearish("2026-10-09 11:01:02")]);
        assert_eq!(feed.read().unwrap()["NIFTY"].len(), 1);
        let r = feed_reading(&feed, "NIFTY", &params(), "2026-10-09 11:02:00").unwrap();
        assert_eq!(r.bias, Bias::Bearish);
        assert_eq!(feed_reading(&feed, "SENSEX", &params(), "2026-10-09 11:02:00"), None);
        feed_push(&feed, "NIFTY", Vec::new()); // no-op
        assert_eq!(feed.read().unwrap()["NIFTY"].len(), 1);
    }

    #[test]
    fn describe_reads_like_the_market_page() {
        let rows = vec![minute("2026-10-09 11:01:02", 100.0, 124.0, 25_100.0, 25_000.0)];
        let r = last_closed_reading(&rows, "NIFTY", &params(), "2026-10-09 11:05:00").unwrap();
        assert_eq!(r.describe(), "+2 BUY at the 11:01 check (PCR 1.24 BUY, VWAP BUY)");
    }

    // ── verdicts ─────────────────────────────────────────────────────────

    #[test]
    fn strong_agree_agree_neutral_and_conflict_each_get_their_own_size() {
        let up = |opt: &str, overall: i32| match sizing_verdict(opt, "NIFTY", Some(&scored(overall)), &MULTS) {
            SizingVerdict::Scale { multiplier, agreement, .. } => Some((multiplier, agreement)),
            SizingVerdict::Skip { .. } => None,
        };
        // BUY CE wants a bullish reading, BUY PE a bearish one.
        for (opt, side) in [("CE", 1), ("PE", -1)] {
            assert_eq!(up(opt, 2 * side), Some((1.5, Agreement::Strong)), "{opt} strong agree");
            assert_eq!(up(opt, side), Some((1.0, Agreement::Agree)), "{opt} agree");
            assert_eq!(up(opt, 0), Some((0.5, Agreement::Neutral)), "{opt} neutral");
            // Against the call — mildly or strongly — is never bought.
            assert_eq!(up(opt, -side), None, "{opt} disagree");
            assert_eq!(up(opt, -2 * side), None, "{opt} strong disagree");
        }
        assert_eq!(up("ce", 2), Some((1.5, Agreement::Strong)));
    }

    #[test]
    fn the_note_names_the_tier_and_its_multiplier() {
        let note = |overall: i32| match sizing_verdict("CE", "NIFTY", Some(&scored(overall)), &MULTS) {
            SizingVerdict::Scale { note, .. } => note,
            v => panic!("expected a scale, got {v:?}"),
        };
        assert!(note(2).ends_with("strongly agrees with BUY CE, ×1.5"), "{}", note(2));
        assert!(note(1).ends_with(") agrees with BUY CE, ×1"), "{}", note(1));
        assert!(note(0).ends_with("NEUTRAL, ×0.5"), "{}", note(0));
    }

    #[test]
    fn only_a_strong_agree_lets_the_stop_go_lower() {
        let allowed = |opt: &str, overall: i32| sl_lower_verdict(opt, "NIFTY", Some(&scored(overall))).is_ok();
        for (opt, side) in [("CE", 1), ("PE", -1)] {
            assert!(allowed(opt, 2 * side), "{opt} strong agree");
            assert!(!allowed(opt, side), "{opt} plain agree is not enough");
            assert!(!allowed(opt, 0), "{opt} neutral");
            assert!(!allowed(opt, -side), "{opt} disagree");
            assert!(!allowed(opt, -2 * side), "{opt} strong disagree");
        }
        assert!(allowed("ce", 2));
        assert!(sl_lower_verdict("FUT", "NIFTY", Some(&scored(2))).is_err(), "unknown option type");
        let err = sl_lower_verdict("CE", "NIFTY", None).unwrap_err();
        assert!(err.contains("no fresh reading"), "{err}");
        let ok = sl_lower_verdict("CE", "FINNIFTY", Some(&scored(2))).unwrap();
        assert!(ok.starts_with("NIFTY indicator (parent of FINNIFTY) +2 BUY"), "{ok}");
    }

    #[test]
    fn the_opening_check_never_lets_the_stop_go_lower() {
        // Raw +2, but the 09:16 check reads NEUTRAL under the opening rule.
        let mut opening = scored(2);
        opening.bias = Bias::Neutral;
        opening.opening = true;
        let err = sl_lower_verdict("CE", "NIFTY", Some(&opening)).unwrap_err();
        assert!(err.contains("always NEUTRAL"), "{err}");
    }

    #[test]
    fn an_opening_check_is_neutral_however_strong_its_raw_score() {
        let mut opening = scored(2);
        opening.bias = Bias::Neutral;
        opening.opening = true;
        assert!(matches!(
            sizing_verdict("CE", "NIFTY", Some(&opening), &MULTS),
            SizingVerdict::Scale { agreement: Agreement::Neutral, .. }
        ));
    }

    #[test]
    fn no_fresh_reading_counts_as_neutral() {
        match sizing_verdict("CE", "NIFTY", None, &MULTS) {
            SizingVerdict::Scale { multiplier, agreement, note } => {
                assert_eq!((multiplier, agreement), (0.5, Agreement::Neutral));
                assert!(note.contains("no fresh reading"), "{note}");
            }
            v => panic!("expected neutral scale, got {v:?}"),
        }
    }

    #[test]
    fn top_up_rules_pick_the_reading_and_the_size() {
        use Agreement::{Agree, Neutral, Strong};
        // (rule, on a plain agree, on a strong agree)
        let cases = [
            (TopupMode::StrongToAgree, None, Some(Agree)),
            (TopupMode::StrongToStrong, None, Some(Strong)),
            (TopupMode::ToTier, Some(Agree), Some(Strong)),
            (TopupMode::Step, Some(Agree), Some(Strong)),
        ];
        for (mode, on_agree, on_strong) in cases {
            assert_eq!(mode.target(Neutral), None, "{mode:?} never tops up on neutral");
            assert_eq!(mode.target(Agree), on_agree, "{mode:?} on agree");
            assert_eq!(mode.target(Strong), on_strong, "{mode:?} on strong agree");
            assert_eq!(TopupMode::parse(mode.as_str()), mode);
            assert_eq!(mode.steps(), mode == TopupMode::Step);
        }
        // Unknown / empty / hand-edited values fall back to the rule that buys least.
        for junk in ["", "STEP ", "always", "strong"] {
            let want = if junk.trim().eq_ignore_ascii_case("step") { TopupMode::Step } else { TopupMode::StrongToAgree };
            assert_eq!(TopupMode::parse(junk), want, "{junk:?}");
        }
    }

    #[test]
    fn a_skip_says_why() {
        match sizing_verdict("CE", "FINNIFTY", Some(&reading_of(Bias::Bearish)), &MULTS) {
            SizingVerdict::Skip { reason } => {
                assert!(reason.contains("NIFTY indicator (parent of FINNIFTY)"), "{reason}");
                assert!(reason.contains("-2 SELL at the 11:01 check"), "{reason}");
                assert!(reason.contains("disagrees with BUY CE"), "{reason}");
            }
            v => panic!("expected skip, got {v:?}"),
        }
        assert!(matches!(sizing_verdict("FUT", "NIFTY", None, &MULTS), SizingVerdict::Skip { .. }));
    }

    #[test]
    fn nifty_is_the_parent_of_everything_but_banknifty_and_sensex() {
        assert_eq!(indicator_parent("NIFTY"), "NIFTY");
        assert_eq!(indicator_parent("banknifty"), "BANKNIFTY");
        assert_eq!(indicator_parent("SENSEX"), "SENSEX");
        for other in ["FINNIFTY", "MIDCPNIFTY", "BANKEX", "RELIANCE", "TATASTEEL"] {
            assert_eq!(indicator_parent(other), "NIFTY", "{other}");
        }
    }

    // ── lot rounding ─────────────────────────────────────────────────────

    #[test]
    fn lots_round_to_nearest_with_halves_down_and_a_one_lot_floor() {
        let cases = [
            (1, 0.5, 1), (1, 1.5, 1), (2, 0.5, 1), (2, 1.5, 3), (3, 0.5, 1),
            (3, 1.5, 4), (4, 0.5, 2), (4, 1.5, 6), (5, 0.5, 2), (5, 1.5, 7),
            (2, 1.6, 3), (2, 1.8, 4), (3, 0.7, 2), (1, 2.0, 2), (10, 0.1, 1),
        ];
        for (base, mult, want) in cases {
            assert_eq!(scale_lots(base, mult), want, "{base} × {mult}");
        }
        // 0.1-step float noise must not tip a tie upward: 3 × (1.4 + 0.1) = 4.5.
        assert_eq!(scale_lots(3, 1.4 + 0.1), 4);
        assert_eq!(scale_lots(0, 1.5), 0);
        assert_eq!(scale_lots(-2, 1.5), 0);
        assert_eq!(scale_lots(3, f64::NAN), 1);
        assert_eq!(scale_lots(3, 0.0), 1);
    }

    // ── call validity ────────────────────────────────────────────────────

    #[test]
    fn calls_expire_after_the_validity_window() {
        let created = "2026-10-09 11:02:30";
        assert!(!call_expired(created, 120, "2026-10-09 13:02:29"));
        assert!(call_expired(created, 120, "2026-10-09 13:02:30"));
        assert!(!call_expired(created, 0, "2026-10-09 15:30:00")); // off
        assert!(call_expired("2026-10-08 15:00:00", 120, "2026-10-09 09:20:00"));
        assert!(!call_expired("garbage", 120, "2026-10-09 13:02:30"));
        assert_eq!(call_expires_at(created, 120).as_deref(), Some("13:02"));
        assert_eq!(call_expires_at(created, 0), None);
        assert_eq!(call_expires_at("2026-10-09 23:30:00", 120), None);
    }
}
