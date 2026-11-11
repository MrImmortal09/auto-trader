use std::collections::HashMap;

use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

pub mod entry_gate;
pub mod special_session;
pub mod telemetry;

pub use special_session::*;

/// Underlying index names this app treats as "index" options (vs. stock
/// options, which fall back to `TradingConfig::other_lots`). Matched against
/// `TradeSignal::instrument_name.to_uppercase()` with an *exact* match — do
/// not switch to a substring check, since e.g. "BANKNIFTY" and "FINNIFTY"
/// both contain "NIFTY".
pub const INDEX_NAMES: &[&str] = &["NIFTY", "BANKNIFTY", "FINNIFTY", "MIDCPNIFTY", "SENSEX", "BANKEX"];

/// `(segment, name)` of an index's spot value on Kotak's market feed
/// (`subscribeIndices`), or `None` for anything that isn't one of
/// [`INDEX_NAMES`]. All six were confirmed live on 2026-10-19: the index
/// packets come back carrying exactly these names.
pub fn kotak_index_feed(index: &str) -> Option<(&'static str, &'static str)> {
    Some(match index.to_uppercase().as_str() {
        "NIFTY" => ("nse_cm", "Nifty 50"),
        "BANKNIFTY" => ("nse_cm", "Nifty Bank"),
        "FINNIFTY" => ("nse_cm", "Nifty Fin Service"),
        "MIDCPNIFTY" => ("nse_cm", "NIFTY MID SELECT"),
        "SENSEX" => ("bse_cm", "SENSEX"),
        "BANKEX" => ("bse_cm", "BANKEX"),
        _ => return None,
    })
}

/// Key under which the feed stores an index's live value in the shared price
/// map — `idx|NIFTY 50`. Index ticks are keyed by name, not by the opaque
/// numeric token the feed attaches to them.
pub fn index_price_key(feed_name: &str) -> String {
    format!("idx|{}", feed_name.trim().to_uppercase())
}

/// Key for an index's previous close, stored alongside [`index_price_key`].
pub fn index_close_key(feed_name: &str) -> String {
    format!("idx_close|{}", feed_name.trim().to_uppercase())
}

pub const IST_OFFSET_SECS: i32 = 5 * 60 * 60 + 30 * 60;

pub fn ist_offset() -> FixedOffset {
    FixedOffset::east_opt(IST_OFFSET_SECS).expect("valid IST offset")
}

pub fn now_ist() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&ist_offset())
}

pub fn today_ist() -> NaiveDate {
    now_ist().date_naive()
}

pub fn current_ist_timestamp_string() -> String {
    now_ist().format("%Y-%m-%d %H:%M:%S").to_string()
}

pub const MARKET_OPEN_HOUR: u32 = 9;
pub const MARKET_OPEN_MINUTE: u32 = 15;
pub const MARKET_CLOSE_HOUR: u32 = 15;
pub const MARKET_CLOSE_MINUTE: u32 = 40;

/// Cutoff for the automatic options expiry-day square-off: 15:38 IST, two
/// minutes ahead of the 15:40 market close. This account trades options
/// only, so nothing is ever meant to be carried into expiry/settlement.
pub const EXPIRY_SQUAREOFF_HOUR: u32 = 15;
pub const EXPIRY_SQUAREOFF_MINUTE: u32 = 38;

/// Today's market close instant (15:40:00 IST), regardless of weekday.
pub fn today_market_close_ist() -> DateTime<FixedOffset> {
    now_ist()
        .date_naive()
        .and_hms_opt(MARKET_CLOSE_HOUR, MARKET_CLOSE_MINUTE, 0)
        .expect("valid close time")
        .and_local_timezone(ist_offset())
        .single()
        .expect("IST close time is unambiguous")
}

/// Duration to sleep until today's 15:40:00 IST market close. `None` if that
/// instant has already passed for today.
pub fn duration_until_market_close() -> Option<std::time::Duration> {
    let now = now_ist();
    let close = today_market_close_ist();
    (close - now).to_std().ok()
}

// ===========================================================================
// Trading configuration
// ===========================================================================

/// System-wide trading parameters stored in the `trading_config` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingConfig {
    /// Maximum capital allocated per trade in INR.
    pub max_trade_amount_inr: f64,
    /// Default number of index option lots to buy when no per-trade override
    /// and no entry in `index_lots_by_symbol` applies.
    pub index_lots: i32,
    /// Default number of non-index option (stock option) lots to buy.
    pub other_lots: i32,
    /// Per-index default lot count (key: one of [`INDEX_NAMES`], e.g. `"NIFTY"`).
    /// An index missing from this map falls back to `index_lots`. Lets a
    /// trader who only trades indexes size each one independently — e.g. 2
    /// lots of SENSEX but 1 of MIDCPNIFTY, reflecting their very different
    /// lot sizes and notional value.
    #[serde(default)]
    pub index_lots_by_symbol: HashMap<String, i32>,
    /// `"LIVE"` or `"PAPER"`.
    pub mode: String,
    /// Flat brokerage charged per order leg (INR).
    pub brokerage_per_order: f64,
    /// Percentage of target-1 profit at which to exit 50 % of the position.
    pub target_1_exit_pct: f64,
    /// Percentage of target-2 profit at which to exit the remaining position.
    pub target_2_exit_pct: f64,
    /// Market-price-protection percentage for LIVE entry orders (SL-M buys).
    /// Protective exits always use 0. Kotak default 5 %, range 0–20 %.
    #[serde(default = "default_entry_mp")]
    pub entry_market_protection: f64,
    /// When enabled, target 1 sells exactly one lot (not `target_1_exit_pct`)
    /// and the runner never exits at a fixed target 2. Instead each rung,
    /// starting at target 1, extends the next one by
    /// `diff * dynamic_targeting_extension_factor` (`diff = target1 - entry`)
    /// and trails the stop to `rung - diff * dynamic_targeting_trail_factor`
    /// — see `decide_live`'s `TradeState::Target1Hit` branch for the exact
    /// recurrence. Applies in PAPER and LIVE alike (PAPER mirrors it in
    /// `paper_dynamic_target1` / `paper_dynamic_rung`). Off by default:
    /// existing signals keep exiting at their own target 2.
    #[serde(default)]
    pub dynamic_targeting: bool,
    /// Multiplier applied to `diff` when trailing the stop under
    /// `dynamic_targeting` (see above). `0.5` trails halfway back from each
    /// rung; `0.0` locks the stop at the rung itself (tightest); `1.0` trails
    /// only back to the previous rung (loosest — on the first rung that's
    /// breakeven at entry). Clamped to `[0.0, 1.0]` on save so the stop can
    /// never trail below the entry price. Unused unless `dynamic_targeting`
    /// is on — with it off, PAPER trails to the fixed
    /// `(entry + target1) / 2` midpoint. Changing this retroactively updates
    /// `current_sl` on every open `Target1Hit` dynamic-targeting position —
    /// see `post_settings_handler`.
    #[serde(default = "default_trail_factor")]
    pub dynamic_targeting_trail_factor: f64,
    /// Multiplier applied to `diff` when extending the next rung under
    /// `dynamic_targeting` (see above): `next_rung = rung + diff * this`.
    /// `1.0` (default) reproduces the original fixed `diff` spacing; `< 1.0`
    /// packs rungs closer together, `> 1.0` spreads them further apart.
    /// Clamped to `[0.05, 5.0]` on save — it must stay strictly positive, or
    /// the next rung would sit at or below the one just hit and re-trigger
    /// every tick. Changing this retroactively updates `next_dynamic_target`
    /// on every open `Target1Hit` dynamic-targeting position.
    #[serde(default = "default_extension_factor")]
    pub dynamic_targeting_extension_factor: f64,
    /// Extends `dynamic_targeting` to one-lot positions. Normally a position
    /// holding one lot (or less) has nothing left to run after target 1 sells
    /// its only lot, so it exits in full (`TGT1_FULL`). With this on, target 1
    /// sells **nothing** — the whole lot becomes the runner, with its stop
    /// trailed and next rung extended exactly as for a multi-lot runner. The
    /// trailed stop still never sits below entry (trail factor is clamped to
    /// `[0, 1]`). Multi-lot positions are unaffected. Unused unless
    /// `dynamic_targeting` is on; PAPER and LIVE, like the rest of dynamic
    /// targeting. Off by default.
    #[serde(default)]
    pub dynamic_targeting_single_lot: bool,
    /// When enabled, protect gains **before** target 1: track the peak LTP
    /// since entry and, once the peak has covered `pre_t1_trail_arm_pct` % of
    /// the entry→target-1 distance, ratchet `current_sl` up to
    /// `peak - diff * pre_t1_trail_factor` (`diff = target1 - entry`,
    /// tick-rounded, never below the signal's original SL, and never moved
    /// back down). Fixes the "price ran to 99 % of target 1, reversed, and
    /// gave the whole stop-loss back" case while leaving trades that do reach
    /// target 1 completely untouched — target 1 and everything after it
    /// behave exactly as with this off. Applies in both PAPER and LIVE
    /// (protection is a software watch in both, so trailing is bookkeeping
    /// only — no broker order is touched). Off by default.
    #[serde(default)]
    pub pre_t1_trailing: bool,
    /// Percent of the entry→target-1 distance the peak must cover before the
    /// pre-T1 trail arms (60.0 arms at `entry + 0.6 * diff`). Below that the
    /// position keeps the signal's full original SL room, so ordinary noise
    /// near entry is not trailed. Clamped to `[0, 100]` on save; unused
    /// unless `pre_t1_trailing` is on.
    #[serde(default = "default_pre_t1_arm_pct")]
    pub pre_t1_trail_arm_pct: f64,
    /// Multiplier applied to `diff` for the trail distance once armed:
    /// `stop = peak - diff * this`. `0.5` gives back half the entry→target-1
    /// distance from the peak; `0.0` exits on the first tick that isn't a new
    /// high (tightest); `1.0` trails a full `diff` behind (loosest — only
    /// reaches breakeven when the peak reaches target 1 itself). Clamped to
    /// `[0, 1]` on save; unused unless `pre_t1_trailing` is on.
    #[serde(default = "default_trail_factor")]
    pub pre_t1_trail_factor: f64,
    /// When true, halts all future trade entries and ignores new incoming signals.
    #[serde(default)]
    pub kill_switch_active: bool,
    /// Entry buy window, as a percent of the signal's entry→target-1 distance.
    ///
    /// `0` (default) is the classic behaviour: buy the instant the trigger price
    /// is crossed in the signalled direction, however far it has already run.
    ///
    /// `> 0` refuses to chase. The engine only buys while the premium sits
    /// within `w = entry_window_pct/100 * (target1 - entry)` of the trigger:
    /// `[entry, entry + w]` for an `ABOVE` signal, `[entry - w, entry]` for a
    /// `BELOW` one. If the price has already gapped past the far edge when the
    /// signal arrives, the position waits in `WaitingForEntry` for a pull-back
    /// into the window — and expires unfilled at the 15:39 no-entry cutoff if it
    /// never comes (same as any un-triggered signal). Falls back to a percent of the
    /// entry price when the signal has no usable target-1. Clamped to
    /// `[0, 100]` on save, so the window can never reach past target 1.
    #[serde(default)]
    pub entry_window_pct: f64,

    // ── Entry gate (see `entry_gate`) ─────────────────────────────────── //

    /// Size option entries on the VWAP + PCR indicator, read at the moment of
    /// buying from the parent index's last closed check
    /// (`entry_gate::indicator_parent` — BANKNIFTY and SENSEX use their own,
    /// everything else follows NIFTY):
    ///
    /// * strongly agrees with the call — PCR and VWAP both on its side, score
    ///   ±2 (BUY CE + a +2 reading, BUY PE + a −2 reading)
    ///   → default lots × `indicator_strong_agree_multiplier`
    /// * agrees with the call — one of them on its side, the other neutral,
    ///   score ±1 → default lots × `indicator_agree_multiplier`
    /// * NEUTRAL, or no fresh reading → default lots × `indicator_neutral_multiplier`
    /// * disagrees → the call is skipped, with the reason logged
    ///
    /// Lots round to the nearest whole lot with exact halves rounded down,
    /// never below one (`entry_gate::scale_lots`). A manual per-position qty
    /// override is never scaled, but a disagreeing reading still skips it.
    /// Off by default: turning it on is a deliberate change to order sizing.
    #[serde(default)]
    pub indicator_sizing: bool,
    /// Lot multiplier when the indicator agrees with the call on a ±1 score
    /// (one of PCR / VWAP, the other neutral). `[0.1, 5]`.
    #[serde(default = "default_agree_multiplier")]
    pub indicator_agree_multiplier: f64,
    /// Lot multiplier when the indicator strongly agrees with the call: a ±2
    /// score, PCR and VWAP both on its side. `[0.1, 5]`.
    #[serde(default = "default_strong_agree_multiplier")]
    pub indicator_strong_agree_multiplier: f64,
    /// Lot multiplier when the indicator is neutral or has no fresh reading. `[0.1, 5]`.
    #[serde(default = "default_neutral_multiplier")]
    pub indicator_neutral_multiplier: f64,
    /// Minutes per indicator check — one of `entry_gate::INDICATOR_INTERVALS`.
    #[serde(default = "default_indicator_interval")]
    pub indicator_interval_min: i32,
    /// `"chg"` (change in OI since yesterday's close) or `"total"` (total OI).
    #[serde(default = "default_indicator_basis")]
    pub indicator_basis: String,
    /// Strikes each side of ATM counted — one of `entry_gate::INDICATOR_STRIKE_WINDOWS` (0 = all).
    #[serde(default = "default_indicator_window")]
    pub indicator_strike_window: i64,
    /// Option Signal = BUY when PCR is above this.
    #[serde(default = "default_pcr_threshold")]
    pub indicator_pcr_buy_above: f64,
    /// Option Signal = SELL when PCR is below this (never above `indicator_pcr_buy_above`).
    #[serde(default = "default_pcr_threshold")]
    pub indicator_pcr_sell_below: f64,
    /// VWAP Signal is neutral while the future is within ± this % of VWAP. `[0, 5]`.
    #[serde(default)]
    pub indicator_vwap_band_pct: f64,
    /// A Telegram call that has not been bought within this many minutes of
    /// arriving is dropped (`CALL_EXPIRED`) instead of waiting until the 15:39
    /// cutoff. Only calls not yet bought — an entry already sent to the broker
    /// and every open position are untouched. Applies whether or not
    /// `indicator_sizing` is on. `0` = off. `[0, 390]`, default 120.
    #[serde(default = "default_call_validity_minutes")]
    pub call_validity_minutes: i32,
    /// Opening rule for the indicator (only matters with `indicator_sizing`
    /// on): the 09:16 check always reads NEUTRAL, and from 09:22 until the
    /// first regular check the latest closed 5-minute check is used — see
    /// `entry_gate::last_closed_reading`. Off = the 09:16 check is scored
    /// like any other and nothing newer is read until the first regular check.
    #[serde(default = "default_true")]
    pub indicator_early_checks: bool,
    /// What a disagreeing indicator does to a call whose entry has triggered
    /// (only with `indicator_sizing` on). On = the call keeps waiting and is
    /// re-read every tick, so it is bought once the indicator turns neutral
    /// or agrees while the price is still in the buy range; it still dies at
    /// `call_validity_minutes` / the 15:39 cutoff. Off = the call is dropped
    /// on the spot (`INDICATOR_SKIP`).
    #[serde(default = "default_true")]
    pub indicator_wait_on_disagree: bool,
    /// Aggressive top-up (only with `indicator_sizing` on): a call bought at
    /// the reduced NEUTRAL size is topped up if the indicator later agrees
    /// while the price is back in the buy range — which reading does it, and
    /// up to which size, is `indicator_topup_mode`. Only before target 1 and
    /// before any stop trail or sell, never past `call_validity_minutes` or
    /// the 15:39 cutoff, never on a manual qty override. The indicator only
    /// ever adds: it never sells or trims a position.
    /// Off by default: it buys more than the first entry did.
    #[serde(default)]
    pub indicator_topup: bool,
    /// The top-up rule while `indicator_topup` is on — one of
    /// `entry_gate::TopupMode`'s names:
    ///
    /// * `"strong_to_agree"` (default) — only a strong agree tops up, and
    ///   only to the Agrees size. Once per call.
    /// * `"strong_to_strong"` — only a strong agree tops up, straight to the
    ///   Strong Agrees size. Once per call.
    /// * `"tier"` — any agreement tops up, to that reading's own size. Once
    ///   per call.
    /// * `"step"` — as `"tier"`, but a call at the Agrees size (bought
    ///   there, or topped up to it) is topped up once more, to the Strong
    ///   Agrees size, when the reading turns strong. At most two top-ups.
    #[serde(default = "default_topup_mode")]
    pub indicator_topup_mode: String,
    /// Stop-loss lowering gate. On = a Telegram instruction that would lower
    /// a call's stop — a "move SL" reply, an edited call or a resend with a
    /// lower SL — is applied only if the parent index's indicator strongly
    /// agrees with the call at that moment: +2 for BUY CE, −2 for BUY PE
    /// (`entry_gate::sl_lower_verdict`). Anything less — a plain agree,
    /// NEUTRAL, the opening 09:16 check, no fresh reading — and the stop stays
    /// where it is, logged as `SL_LOWER_REFUSED`; the instruction is not
    /// retried later. Raising the stop is always applied. Works whether or not
    /// `indicator_sizing` is on, scored with the same indicator settings.
    /// Off by default: turning it on changes how stops are moved.
    #[serde(default)]
    pub indicator_sl_lower_gate: bool,
}

fn default_true() -> bool { true }

fn default_entry_mp() -> f64 { 5.0 }
fn default_trail_factor() -> f64 { 0.5 }
fn default_pre_t1_arm_pct() -> f64 { 60.0 }
fn default_extension_factor() -> f64 { 1.0 }
fn default_agree_multiplier() -> f64 { 1.0 }
fn default_strong_agree_multiplier() -> f64 { 1.5 }
fn default_neutral_multiplier() -> f64 { 0.5 }
fn default_topup_mode() -> String { entry_gate::TopupMode::StrongToAgree.as_str().to_string() }
fn default_indicator_interval() -> i32 { 15 }
fn default_indicator_basis() -> String { "chg".to_string() }
fn default_indicator_window() -> i64 { 10 }
fn default_pcr_threshold() -> f64 { 1.0 }
fn default_call_validity_minutes() -> i32 { 120 }

impl TradingConfig {
    /// Clamp the entry-gate fields to their documented ranges. Applied both
    /// when settings are saved and when they are loaded from SQLite, so the
    /// DB row, the in-memory config and what the engine acts on never differ.
    pub fn sanitize_entry_gate(&mut self) {
        let finite_or = |v: f64, d: f64| if v.is_finite() { v } else { d };
        self.indicator_agree_multiplier =
            finite_or(self.indicator_agree_multiplier, default_agree_multiplier()).clamp(0.1, 5.0);
        self.indicator_strong_agree_multiplier =
            finite_or(self.indicator_strong_agree_multiplier, default_strong_agree_multiplier()).clamp(0.1, 5.0);
        self.indicator_topup_mode = self.topup_mode().as_str().to_string();
        self.indicator_neutral_multiplier =
            finite_or(self.indicator_neutral_multiplier, default_neutral_multiplier()).clamp(0.1, 5.0);
        if !entry_gate::INDICATOR_INTERVALS.iter().any(|&i| i as i32 == self.indicator_interval_min) {
            self.indicator_interval_min = default_indicator_interval();
        }
        self.indicator_basis = if self.indicator_basis.eq_ignore_ascii_case("total") { "total" } else { "chg" }.to_string();
        if !entry_gate::INDICATOR_STRIKE_WINDOWS.contains(&self.indicator_strike_window) {
            self.indicator_strike_window = default_indicator_window();
        }
        self.indicator_pcr_buy_above = finite_or(self.indicator_pcr_buy_above, default_pcr_threshold()).max(0.0);
        self.indicator_pcr_sell_below = finite_or(self.indicator_pcr_sell_below, default_pcr_threshold())
            .clamp(0.0, self.indicator_pcr_buy_above);
        self.indicator_vwap_band_pct = finite_or(self.indicator_vwap_band_pct, 0.0).clamp(0.0, 5.0);
        self.call_validity_minutes = self.call_validity_minutes.clamp(0, 390);
    }

    /// The indicator settings as `entry_gate` takes them.
    pub fn indicator_params(&self) -> entry_gate::IndicatorParams {
        entry_gate::IndicatorParams {
            interval_min: self.indicator_interval_min.max(1) as u32,
            total_oi: self.indicator_basis.eq_ignore_ascii_case("total"),
            strike_window: self.indicator_strike_window,
            pcr_buy_above: self.indicator_pcr_buy_above,
            pcr_sell_below: self.indicator_pcr_sell_below,
            vwap_band_pct: self.indicator_vwap_band_pct,
            early_checks: self.indicator_early_checks,
        }
    }

    /// The lot multiplier of each agreement tier, as `entry_gate` takes them.
    pub fn indicator_multipliers(&self) -> entry_gate::Multipliers {
        entry_gate::Multipliers {
            strong: self.indicator_strong_agree_multiplier,
            agree: self.indicator_agree_multiplier,
            neutral: self.indicator_neutral_multiplier,
        }
    }

    /// `indicator_topup_mode`, parsed — an unknown value is the default rule.
    pub fn topup_mode(&self) -> entry_gate::TopupMode {
        entry_gate::TopupMode::parse(&self.indicator_topup_mode)
    }
}

// ===========================================================================
// Feature flags
// ===========================================================================

/// Engine switches for the v2 redesign, stored in their own `feature_flags`
/// table behind `GET/PUT /api/v2/flags` — never in [`TradingConfig`], which
/// `POST /api/settings` replaces whole, so a switch kept there would be turned
/// off every time v1 saves a setting. All default to off, and with all off
/// the engine trades exactly as it did before they existed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureFlags {
    /// Trade only CE/PE options on [`INDEX_NAMES`]: any other call (stock
    /// options, equity, an index call with no strike) is skipped instead.
    /// See [`TradeSignal::is_index_option`].
    pub index_options_only: bool,
    /// Stops and targets set on the v2 charts, for scalps only
    /// (`PATCH /api/v2/positions/:id`). While off, no new level can be set;
    /// levels already set stay until their position closes.
    pub chart_stops: bool,
    /// The instrument search and its quotes for anyone signed in:
    /// `GET /api/v2/search`, `/api/v2/quotes` and `/api/v2/instruments/*`.
    /// While off they answer 404. Nothing in the engine reads it.
    #[serde(default)]
    pub search: bool,
    /// Each signed-in person's watchlists (`/api/v2/watchlists*`), priced
    /// from the search slice's quote hub. While off they answer 404.
    /// Nothing in the engine reads it.
    #[serde(default)]
    pub watchlists: bool,
    /// Charts, option chains and ladders for any F&O underlying in the
    /// catalog, not only the six indices (`/api/v2/candles*`,
    /// `/api/v2/expiries`, `/api/v2/chain`, `/api/execute/stream`). While
    /// off, those routes accept the six indices exactly as before. Nothing
    /// in the engine reads it, and the owner's live buy
    /// (`POST /api/execute/buy`) stays index-only whatever it says.
    ///
    /// It needs [`Self::search`] on as well: the instrument catalog a
    /// stock's name is resolved against is built, and the quote hub that
    /// prices a ladder's polled rows, a stock's spot fallback and the close
    /// snapshot is polled, only while `search` is on. With `search` off a
    /// stock is not recognised (or, from an earlier build, shows no price);
    /// nothing is ever shown wrong.
    #[serde(default)]
    pub stock_charts: bool,
}

impl FeatureFlags {
    /// Every switch as `(name, on)`; `name` is both its `feature_flags` row
    /// and its JSON field.
    pub fn entries(&self) -> [(&'static str, bool); 5] {
        [
            ("index_options_only", self.index_options_only),
            ("chart_stops", self.chart_stops),
            ("search", self.search),
            ("watchlists", self.watchlists),
            ("stock_charts", self.stock_charts),
        ]
    }

    /// Set the switch called `name`. `false` when there is no such switch.
    pub fn set(&mut self, name: &str, on: bool) -> bool {
        match name {
            "index_options_only" => self.index_options_only = on,
            "chart_stops" => self.chart_stops = on,
            "search" => self.search = on,
            "watchlists" => self.watchlists = on,
            "stock_charts" => self.stock_charts = on,
            _ => return false,
        }
        true
    }
}

// ===========================================================================
// Trade signal (options-aware)
// ===========================================================================

/// An inbound signal parsed from Telegram or any other source.
///
/// Supports equity, F&O, and options instruments.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TradeSignal {
    /// Underlying or full instrument name (e.g. `"NIFTY"`, `"RELIANCE"`).
    pub instrument_name: String,
    /// Strike price for options; `None` for equity / futures.
    pub strike: Option<f64>,
    /// `"CE"` or `"PE"` for options; `None` otherwise.
    pub option_type: Option<String>,
    /// Expiry date string (e.g. `"26JUL2026"`); `None` for equity.
    pub expiry: Option<String>,
    /// `"BUY"` or `"SELL"`.
    pub action: String,
    /// Entry trigger condition — `"ABOVE"` or `"BELOW"` `entry_price`.
    pub entry_condition: String,
    /// Trigger / reference price for entry.
    pub entry_price: f64,
    /// Upper edge of an entry *range* (`140-145`, `@ 125-130`, `CMP 100-102`).
    /// When set, `entry_condition` is `"ABOVE"` and the engine buys only while
    /// LTP is in `[entry_price, entry_price_max]` — above it the call waits for
    /// a pullback. `None` for a single-trigger `ABOVE`/`BELOW` signal.
    #[serde(default)]
    pub entry_price_max: Option<f64>,
    /// Ordered list of price targets (e.g. `[250.0, 320.0]`).
    pub targets: Vec<f64>,
    /// Initial stop-loss price.
    pub stop_loss: f64,
    /// Signal origin (e.g. `"telegram"`, `"manual"`).
    pub source: String,
    /// Unique identifier for the signal (e.g. Telegram message ID), used for tracking edits.
    #[serde(default)]
    pub signal_id: Option<String>,
    /// The exact raw message text received, for displaying in reports.
    #[serde(default)]
    pub raw_message: Option<String>,
    /// The Telegram chat the call came from. `None` for a scalp, the webhook,
    /// and anything received before chats were kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<SignalChat>,
}

/// The Telegram chat a call came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignalChat {
    /// Bot-API style dialog id (`-100…` for a channel).
    pub id: i64,
    /// Its title, when Telegram's cache had it.
    #[serde(default)]
    pub title: Option<String>,
}

/// What a trade row records about the call it belongs to, beside the fill.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TradeTag {
    pub signal_id: Option<String>,
    pub raw_message: Option<String>,
    pub chat_id: Option<i64>,
    pub chat_title: Option<String>,
    pub option_type: Option<String>,
    pub strike: Option<f64>,
}

impl TradeTag {
    pub fn of(signal: &TradeSignal) -> Self {
        Self {
            signal_id: signal.signal_id.clone(),
            raw_message: signal.raw_message.clone(),
            chat_id: signal.chat.as_ref().map(|c| c.id),
            chat_title: signal.chat.as_ref().and_then(|c| c.title.clone()),
            option_type: signal.option_type.clone(),
            strike: signal.strike,
        }
    }
}

impl TradeSignal {
    /// A CE or PE with a strike on one of [`INDEX_NAMES`], matched by exact
    /// name — the only calls traded while
    /// [`FeatureFlags::index_options_only`] is on. `NIFTY50` or `BANK NIFTY`
    /// are not aliases: they don't match.
    pub fn is_index_option(&self) -> bool {
        let instrument = self.instrument_name.trim().to_uppercase();
        self.strike.is_some_and(|s| s > 0.0)
            && self.option_type.as_deref().is_some_and(|t| {
                t.eq_ignore_ascii_case("CE") || t.eq_ignore_ascii_case("PE")
            })
            && INDEX_NAMES.contains(&instrument.as_str())
    }
}

// ===========================================================================
// Position lifecycle
// ===========================================================================

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum TradeState {
    /// Order placed; waiting for price to cross `entry_price`.
    #[default]
    WaitingForEntry,
    /// Position is open and actively being monitored.
    Active,
    /// First target hit; partial exit done and trailing SL engaged.
    Target1Hit,
    /// Position fully closed (target 2 hit, SL triggered, or manual close).
    Closed,
}

/// A live position held in memory by the trading engine.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MonitoredPosition {
    pub id: String,
    pub signal: TradeSignal,
    pub state: TradeState,
    /// IST timestamp (`%Y-%m-%d %H:%M:%S`) this position was created from an
    /// incoming signal. Empty for positions persisted before this field
    /// existed. Used to tell a `WaitingForEntry` position apart from a stale
    /// one that survived a restart into a new calendar day — see
    /// `monitor::stale_entry_reason`.
    #[serde(default)]
    pub created_at: String,
    /// Current stop-loss level (may be trailed upward from initial SL).
    pub current_sl: f64,
    /// Highest LTP observed while `Active`, feeding the pre-T1 trailing stop
    /// (see `TradingConfig::pre_t1_trailing`). `None` until the first tick
    /// after entry, and unused once target 1 hits — from there the dynamic
    /// ladder or the fixed target-2 path owns the stop.
    #[serde(default)]
    pub peak_ltp: Option<f64>,
    /// Dynamic-targeting runner state: the next rung to watch for once target 1
    /// has been hit, if `TradingConfig::dynamic_targeting` was on when it hit.
    /// `None` means either dynamic targeting is off for this position (it
    /// exits at the signal's fixed target 2 as usual) or target 1 hasn't hit
    /// yet.
    #[serde(default)]
    pub next_dynamic_target: Option<f64>,
    /// Dynamic-targeting runner state: the price level of the rung most
    /// recently hit (target 1 itself the first time, then each extended rung
    /// after). `current_sl` and `next_dynamic_target` are both derived from
    /// this plus the live `dynamic_targeting_trail_factor` /
    /// `dynamic_targeting_extension_factor`, which is what lets
    /// `post_settings_handler` recompute both in place when those factors
    /// change, instead of only taking effect on the next rung hit.
    #[serde(default)]
    pub last_dynamic_rung: Option<f64>,
    /// Dynamic-targeting runner state: how many rungs have been hit (1 after
    /// target 1, 2 after the next extension, …). Display-only — drives the
    /// `TargetNHit` label in the frontend; `Target1Hit` stays the actual
    /// `TradeState` for the whole runner regardless of this count.
    #[serde(default)]
    pub dynamic_rung_number: i32,
    /// LIVE mode: a user-requested partial market sell for this quantity,
    /// picked up and cleared on the next tick. Distinct from `force_exit`,
    /// which always sells the whole holding.
    #[serde(default)]
    pub manual_sell_qty: Option<i32>,
    /// Number of units / lots currently held.
    pub executed_qty: i32,
    /// Volume-weighted average buy price.
    pub avg_buy_price: f64,
    /// Manual override for the quantity to execute.
    pub override_qty: Option<i32>,
    /// The precise Kotak OrderRequest mapped from the Scrip Master.
    pub resolved_order: Option<OrderRequest>,
    /// The resolved contract's actual expiry date, from the Scrip Master
    /// record `resolved_order` was built from. Distinct from
    /// `signal.expiry`, which is the raw *parsed* string and is `None`
    /// whenever the incoming message never stated one (`resolve_signal`
    /// then falls back to the nearest upcoming expiry) — this is what that
    /// resolution actually landed on, and what the expiry-day square-off
    /// checks against. `None` until resolved, e.g. a position adopted from
    /// the broker without a Scrip Master lookup — such a position never
    /// gets an automatic expiry square-off.
    #[serde(default)]
    pub resolved_expiry: Option<NaiveDate>,
    /// Live Last Traded Price populated just before returning via API
    #[serde(default)]
    pub ltp: Option<f64>,
    /// WebSocket scrip key for price map lookup (e.g. "nse_fo|51386")
    #[serde(default)]
    pub ws_scrip_key: Option<String>,
    /// Set this to a string reason to force the position to exit on the next tick
    #[serde(default)]
    pub force_exit: Option<String>,
    /// Optional price override for forced exits (e.g., from "exit at 610" Telegram reply)
    #[serde(default)]
    pub override_exit_price: Option<f64>,
    /// Minimum price increment for this contract (usually 0.05)
    #[serde(default = "default_tick_size")]
    pub tick_size: f64,
    /// LIVE mode: broker order number of the resting entry order.
    #[serde(default)]
    pub entry_order_id: Option<String>,
    /// LIVE mode: broker order number of the resting stop-loss (SL-M) order.
    #[serde(default)]
    pub sl_order_id: Option<String>,
    /// LIVE mode: quantity the resting SL-M order currently covers.
    ///
    /// Tracked so the "never sell more than we hold" invariant can be checked
    /// before any additional sell is placed: `sell_qty + sl_order_qty` must
    /// never exceed `executed_qty`.
    #[serde(default)]
    pub sl_order_qty: i32,
    /// LIVE mode: trigger price the resting SL-M order currently sits at
    /// (already tick-rounded). Used to detect when a trailed/updated SL needs
    /// a modify call.
    #[serde(default)]
    pub sl_order_trigger: f64,
    /// LIVE mode: broker order number of the resting target (LIMIT) order.
    ///
    /// Unused — targets are executed as market orders by the engine so that a
    /// resting SL and a resting target can never over-commit the held quantity.
    /// Retained for snapshot compatibility.
    #[serde(default)]
    pub target_order_id: Option<String>,
    /// LIVE mode: broker order number of an engine-initiated exit (market sell)
    /// that has been placed but not yet settled from the order book.
    #[serde(default)]
    pub pending_exit_order_id: Option<String>,
    /// LIVE mode: quantity of [`Self::pending_exit_order_id`].
    #[serde(default)]
    pub pending_exit_qty: i32,
    /// LIVE mode: exit reason recorded when [`Self::pending_exit_order_id`] settles.
    #[serde(default)]
    pub pending_exit_reason: Option<String>,
    /// LIVE mode: a cancel has already been sent for a partially-filled entry,
    /// so the poller does not spam cancels while waiting for it to take effect.
    #[serde(default)]
    pub entry_cancel_sent: bool,
    /// LIVE mode: consecutive failed exit attempts. Capped so a persistently
    /// rejecting broker cannot put the engine in a retry loop.
    #[serde(default)]
    pub exit_attempts: i32,
    /// LIVE mode: how many times an entry order has actually been sent to the
    /// broker for this position. Capped ([`MAX_ENTRY_ATTEMPTS`]) so an entry
    /// whose send keeps failing ambiguously cannot loop forever.
    #[serde(default)]
    pub entry_attempts: i32,
    /// LIVE mode: IST timestamp (`%Y-%m-%d %H:%M:%S`) of the most recent entry
    /// send attempt. Used to throttle retries after a failed `get_limits`
    /// pre-flight, and to bound how long the reconciler hunts the order book
    /// for an entry whose send outcome is unknown.
    #[serde(default)]
    pub entry_attempt_at: Option<String>,
    /// LIVE mode: an entry order was sent but the broker's response was lost
    /// (connect failure / timeout / unparseable reply), so it may or may not be
    /// live at the broker. While set, `decide_live` will not send another entry
    /// and the reconciler hunts the order book for the matching fill to adopt.
    /// Never `true` alongside a known `entry_order_id`.
    #[serde(default)]
    pub entry_send_uncertain: bool,
    /// LIVE mode: the quantity of the entry order whose send outcome is unknown
    /// — used to match it in the order book while `entry_send_uncertain` is set.
    #[serde(default)]
    pub entry_uncertain_qty: Option<i32>,
    /// LIVE mode: how many order-book polls the engine had completed when the
    /// unconfirmed entry went out. Only an order that first shows up in a
    /// later poll can be it — an older fill on the same contract (a scalp
    /// already closed) never is. Not persisted: after a restart it is `None`
    /// and the hunt matches on symbol and quantity alone, as it always did.
    #[serde(skip)]
    pub entry_uncertain_poll: Option<u64>,
    /// LIVE mode: the last market entry send for this position was hard-rejected
    /// by the broker / exchange RMS — the generic `Not_Ok` (`code 1041`-style)
    /// that Kotak returns for market orders in the first minutes after the
    /// 09:15 open. While set, `decide_live` retries the entry as an IOC **limit**
    /// order (price a protection-band above the touch, never past the entry buy
    /// window) instead of another market order. Cleared once the position leaves
    /// `WaitingForEntry`.
    #[serde(default)]
    pub entry_retry_as_limit: bool,
    /// LIVE mode: set when the engine has given up acting on this position
    /// automatically. Requires manual intervention; no further orders are sent.
    #[serde(default)]
    pub live_halt: Option<String>,
    /// Display-only: the `[low, high]` premium window the engine will buy this
    /// `WaitingForEntry` position in, derived from the live `entry_window_pct`
    /// config (see [`TradingConfig::entry_window_pct`]). Populated by the
    /// positions API just before returning, `None` when the feature is off or
    /// the row is not awaiting entry. Not meaningfully persisted.
    #[serde(default)]
    pub entry_zone: Option<(f64, f64)>,
    /// Display-only: IST `HH:MM` this `WaitingForEntry` call stops being valid
    /// (see [`TradingConfig::call_validity_minutes`]). Populated by the
    /// positions API just before returning; `None` when the rule is off or the
    /// row is not awaiting entry. Not meaningfully persisted.
    #[serde(default)]
    pub call_expires_at: Option<String>,
    /// Indicator-sizing bookkeeping for this call — see [`SizingState`].
    #[serde(default)]
    pub sizing: SizingState,
    /// Opened by hand from the Execute screen rather than from a signal. A
    /// manual position buys at market (`entry_condition` `"MARKET"`) exactly
    /// the lots asked for, with no indicator gate, and carries no stop or
    /// target — it closes only on the user's own exit, the kill switch, or
    /// the expiry-day square-off. Telegram signals never touch it.
    #[serde(default)]
    pub manual: bool,
    /// Manual position: an add the user asked for on top of the holding,
    /// picked up by the engine on its next tick and cleared once it acts.
    #[serde(default)]
    pub manual_add: Option<ManualAdd>,
    /// A scalp's stop and target set from a v2 chart — see [`ChartLevels`].
    /// Left out of the JSON while unset, so positions without one serialize
    /// exactly as before.
    #[serde(default, skip_serializing_if = "ChartLevels::is_empty")]
    pub chart: ChartLevels,
    /// LIVE: the chart targets the sell now at the broker is for
    /// (`pending_exit_reason` `CHART_TARGET`), out of `chart` while it
    /// settles. An IOC that misses puts back what it didn't sell.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chart_targets_selling: Vec<ChartTarget>,
}

/// What a chart stop or target watches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LevelTrigger {
    /// The option's own last traded price.
    Premium,
    /// Its underlying index's value. A call's stop fires when the index falls
    /// to the level and its target when the index rises to it; a put's the
    /// other way round.
    Index,
}

/// One stop or target level.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ChartLevel {
    pub price: f64,
    pub trigger: LevelTrigger,
}

/// Most chart targets one position may have.
pub const MAX_CHART_TARGETS: usize = 5;

/// One chart target: a level, and how many lots reaching it sells.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartTarget {
    /// Names it in an edit, so a move never lands on a target that has
    /// already sold and been replaced.
    pub id: String,
    pub price: f64,
    pub trigger: LevelTrigger,
    /// Lots it sells. `None` is "all left": whatever is still held when it
    /// is reached, lots added later included — what a target was before
    /// targets had lots.
    #[serde(default)]
    pub lots: Option<i32>,
}

impl ChartTarget {
    pub fn new_id() -> String {
        uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
    }
}

/// A scalp's stop and targets, set from a v2 chart
/// (`PATCH /api/v2/positions/:id`, behind `FeatureFlags::chart_stops`).
/// The stop sells the whole holding at market, through the same exit as any
/// stop; each target sells its own lots (see [`chart_hit`]). Once set they
/// protect the position until it closes, whatever the switch says later.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ChartLevels {
    pub stop: Option<ChartLevel>,
    /// In the order they were set.
    pub targets: Vec<ChartTarget>,
}

/// Also reads the one-target shape stored before targets had lots
/// (`{stop, target}`), as an "all left" target, so a position open across
/// the upgrade keeps its target.
impl<'de> Deserialize<'de> for ChartLevels {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Stored {
            #[serde(default)]
            stop: Option<ChartLevel>,
            #[serde(default)]
            targets: Option<Vec<ChartTarget>>,
            #[serde(default)]
            target: Option<ChartLevel>,
        }
        let s = Stored::deserialize(d)?;
        let targets = match (s.targets, s.target) {
            (Some(targets), _) => targets,
            (None, Some(t)) => vec![ChartTarget { id: ChartTarget::new_id(), price: t.price, trigger: t.trigger, lots: None }],
            (None, None) => Vec::new(),
        };
        Ok(Self { stop: s.stop, targets })
    }
}

/// One target set or moved from a chart. `id` names the target to move or
/// resize; without one it is a new target. `lots`: left out keeps a moved
/// target's lots, `None` is "all left", `Some(n)` is `n` lots.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetEdit {
    #[serde(default)]
    pub id: Option<String>,
    pub price: f64,
    pub trigger: LevelTrigger,
    #[serde(default, deserialize_with = "present_lots")]
    pub lots: Option<Option<i32>>,
}

/// Tells `lots: null` from `lots` left out.
fn present_lots<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<i32>>, D::Error> {
    Option::<i32>::deserialize(d).map(Some)
}

/// Why a chart level edit can't be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LevelRefusal {
    /// The request itself is wrong (a bad price or lot count).
    BadRequest(String),
    /// It can't apply to the position as it stands now.
    Conflict(String),
}

impl ChartLevels {
    pub fn is_empty(&self) -> bool {
        self.stop.is_none() && self.targets.is_empty()
    }

    /// Every level, stop first.
    pub fn levels(&self) -> impl Iterator<Item = ChartLevel> + '_ {
        self.stop.into_iter().chain(self.targets.iter().map(|t| ChartLevel { price: t.price, trigger: t.trigger }))
    }

    /// Set or move one target on a position holding `held_lots`; its id.
    /// A target's lots may only use what no other target has: a change that
    /// adds lots past the holding is refused. A move that keeps its lots, or
    /// a resize down, is always taken — the holding may have shrunk below
    /// the targets after a partial sell. Sums saturate, so no lot count can
    /// wrap round to pass.
    pub fn set_target(&mut self, edit: &TargetEdit, held_lots: i32) -> Result<String, LevelRefusal> {
        if !(edit.price.is_finite() && edit.price > 0.0) {
            return Err(LevelRefusal::BadRequest("a level's price must be a positive number".to_string()));
        }
        if let Some(Some(n)) = edit.lots {
            if n < 1 {
                return Err(LevelRefusal::BadRequest("a target sells at least 1 lot".to_string()));
            }
        }
        let at = match edit.id.as_deref() {
            Some(id) => Some(self.targets.iter().position(|t| t.id == id).ok_or_else(|| {
                LevelRefusal::Conflict("that target is gone — it may just have sold".to_string())
            })?),
            None if self.targets.len() >= MAX_CHART_TARGETS => {
                return Err(LevelRefusal::Conflict(format!("a trade can have at most {MAX_CHART_TARGETS} targets")));
            }
            None => None,
        };
        let lots = match (edit.lots, at) {
            (Some(lots), _) => lots,
            (None, Some(i)) => self.targets[i].lots,
            (None, None) => None,
        };
        // The explicit lots this target sold before: a new one, or an "all
        // left" one turned into a count, adds every lot it now asks for.
        let before = at.and_then(|i| self.targets[i].lots).unwrap_or(0);
        if let Some(n) = lots.filter(|n| *n > before) {
            let others = self
                .targets
                .iter()
                .enumerate()
                .filter(|(i, _)| Some(*i) != at)
                .filter_map(|(_, t)| t.lots)
                .fold(0, i32::saturating_add);
            let total = others.saturating_add(n);
            if total > held_lots {
                return Err(LevelRefusal::Conflict(format!("the targets would sell {total} lots — you hold {held_lots}")));
            }
        }
        let target = ChartTarget {
            id: at.map(|i| self.targets[i].id.clone()).unwrap_or_else(ChartTarget::new_id),
            price: edit.price,
            trigger: edit.trigger,
            lots,
        };
        let id = target.id.clone();
        match at {
            Some(i) => self.targets[i] = target,
            None => self.targets.push(target),
        }
        Ok(id)
    }

    /// Remove the target `id`; whether it was there.
    pub fn remove_target(&mut self, id: &str) -> bool {
        let before = self.targets.len();
        self.targets.retain(|t| t.id != id);
        self.targets.len() != before
    }

    /// Take the targets `ids` out (those still there), as a sell for them
    /// goes out.
    pub fn take_targets(&mut self, ids: &[String]) -> Vec<ChartTarget> {
        let (taken, kept) = std::mem::take(&mut self.targets).into_iter().partition(|t| ids.contains(&t.id));
        self.targets = kept;
        taken
    }

    /// Put back what a sell for `taken` left unsold: `sold_lots` come off
    /// the first targets, the rest return with what they still have to sell.
    pub fn restore_unsold(&mut self, taken: Vec<ChartTarget>, sold_lots: i32) {
        let (_, unsold) = split_target_lots(taken, sold_lots);
        self.targets.extend(unsold);
    }
}

/// `taken` split at `lots`: the first `lots` lots, off the first targets
/// (a target split in two keeps its id on both sides), and what is left of
/// them. An "all left" target is never counted into the first part.
pub fn split_target_lots(taken: Vec<ChartTarget>, lots: i32) -> (Vec<ChartTarget>, Vec<ChartTarget>) {
    let (mut first, mut rest) = (Vec::new(), Vec::new());
    let mut left = lots.max(0);
    for t in taken {
        let Some(n) = t.lots else {
            rest.push(t);
            continue;
        };
        let now = left.clamp(0, n);
        left -= now;
        if now > 0 {
            first.push(ChartTarget { lots: Some(now), ..t.clone() });
        }
        if n > now {
            rest.push(ChartTarget { lots: Some(n - now), ..t });
        }
    }
    (first, rest)
}

/// What reaching a chart level sells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChartHit {
    /// The whole holding: the stop, or targets that cover all of it.
    All { reason: &'static str },
    /// `lots` lots for the targets `ids`, leaving the rest held — reason
    /// `CHART_TARGET`.
    Part { lots: i32, ids: Vec<String> },
}

/// What a position holding `held_lots` sells now that an option's `premium`,
/// or its index's `index` value, has reached `chart`'s stop (checked first)
/// or targets. Targets reached together sell together; "all left" among
/// them, or lots covering the holding, sell everything. `option_type` is
/// `CE` or `PE`; anything else never reaches a level. Shared by the engine's
/// scalps and the users' paper positions.
pub fn chart_hit(option_type: Option<&str>, chart: &ChartLevels, premium: Option<f64>, index: Option<f64>, held_lots: i32) -> Option<ChartHit> {
    let call = match option_type {
        Some(t) if t.eq_ignore_ascii_case("CE") => true,
        Some(t) if t.eq_ignore_ascii_case("PE") => false,
        _ => return None,
    };
    let reached = |price: f64, trigger: LevelTrigger, is_stop: bool| {
        let (value, falling) = match trigger {
            // On its own premium a stop is reached falling and a target
            // rising, for a call and a put alike.
            LevelTrigger::Premium => (premium, is_stop),
            // On the index a call loses as it falls, a put as it rises.
            LevelTrigger::Index => (index, is_stop == call),
        };
        value.is_some_and(|v| v > 0.0 && if falling { v <= price } else { v >= price })
    };
    if chart.stop.as_ref().is_some_and(|l| reached(l.price, l.trigger, true)) {
        return Some(ChartHit::All { reason: "CHART_STOP" });
    }
    let hit: Vec<&ChartTarget> = chart.targets.iter().filter(|t| reached(t.price, t.trigger, false)).collect();
    if hit.is_empty() {
        return None;
    }
    let mut lots = 0;
    for t in &hit {
        match t.lots {
            Some(n) => lots = i32::saturating_add(lots, n.max(0)),
            None => return Some(ChartHit::All { reason: "CHART_TARGET" }),
        }
    }
    if lots >= held_lots {
        return Some(ChartHit::All { reason: "CHART_TARGET" });
    }
    Some(ChartHit::Part { lots, ids: hit.iter().map(|t| t.id.clone()).collect() })
}

impl MonitoredPosition {
    /// What this position sells now that the option's `premium`, or its
    /// index's `index` value, has reached its chart stop or targets — see
    /// [`chart_hit`]. `lot` is the contract's lot size.
    pub fn chart_hit(&self, premium: Option<f64>, index: Option<f64>, lot: i32) -> Option<ChartHit> {
        let held_lots = if lot > 0 { self.executed_qty / lot } else { self.executed_qty };
        chart_hit(self.signal.option_type.as_deref(), &self.chart, premium, index, held_lots)
    }
    /// A buy on top of the holding is pending: a manual add not sent yet, a
    /// top-up order at the broker, or one whose send outcome is unknown.
    pub fn buy_in_flight(&self) -> bool {
        self.manual_add.is_some() || self.sizing.topup_order_id.is_some() || self.sizing.topup_send_uncertain
    }

    /// A sell is pending: a requested exit, a sell order at the broker, or a
    /// partial sell not sent yet.
    pub fn sell_in_flight(&self) -> bool {
        self.force_exit.is_some() || self.pending_exit_order_id.is_some() || self.manual_sell_qty.is_some()
    }
}

/// A pending "buy more of this contract" request on a manual position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManualAdd {
    pub lots: i32,
    /// IST `%Y-%m-%d %H:%M:%S` of the tap — an add that can't be sent within
    /// a few seconds of it is dropped, never sent late.
    pub requested_at: String,
}

/// A manual order (the first buy or an add) not sent to the broker within
/// this many seconds of the tap is dropped rather than sent at a price the
/// user never saw.
pub const MANUAL_ORDER_TTL_SECS: i64 = 15;

/// Per-position state of the indicator entry gate
/// (`TradingConfig::indicator_sizing`): the wait-on-disagree log marker and
/// the aggressive top-up's order tracking.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SizingState {
    /// The disagreeing reading this waiting call was last logged as held back
    /// by — so `INDICATOR_WAIT` is logged once per check, not once per tick.
    pub hold_note: Option<String>,
    /// LIVE: the entry order now at the broker was sized below the top tier
    /// (see `topup_base_qty`), so its fill may be topped up later. Becomes
    /// `topup_base_qty` when the fill settles.
    pub topup_eligible: bool,
    /// The quantity a top-up may still add to: what was bought at the
    /// indicator's reduced NEUTRAL size — or, under the `"step"` top-up rule
    /// only, at the Agrees size (see `topup_stepped`). A top-up is only
    /// considered while the holding is still exactly this — any sell or
    /// broker reconciliation that changes `executed_qty` rules it out.
    /// Cleared once a top-up has been tried, unless the step rule re-arms it.
    pub topup_base_qty: Option<i32>,
    /// The holding is already at the Agrees size — bought there, or topped
    /// up to it — so the only top-up left is the `"step"` rule's second one,
    /// to the Strong Agrees size. Every other rule ignores such a position.
    pub topup_stepped: bool,
    /// LIVE: the top-up order now at the broker was sized to the Agrees size
    /// under the `"step"` rule — a complete fill re-arms `topup_base_qty`
    /// (with `topup_stepped`) for the step to the Strong Agrees size.
    pub topup_rearm: bool,
    /// LIVE: the top-up buy order currently at the broker. While set the
    /// position is never closed out from under it — a fill landing afterwards
    /// would otherwise be an untracked holding.
    pub topup_order_id: Option<String>,
    /// LIVE: a cancel was sent for the unfilled remainder of the top-up order.
    pub topup_cancel_sent: bool,
    /// LIVE: the whole position was exited while the top-up order was still
    /// at the broker — whatever that order fills is sold straight back.
    pub topup_exit_on_fill: bool,
    /// LIVE: top-up sends so far, capped at [`MAX_ENTRY_ATTEMPTS`] per call —
    /// both steps of the `"step"` rule share the one cap.
    pub topup_attempts: i32,
    /// LIVE: IST timestamp of the last top-up attempt, for the retry throttle.
    pub topup_attempt_at: Option<String>,
    /// LIVE: the market top-up was RMS-rejected; retry as an IOC limit
    /// (same rule as `MonitoredPosition::entry_retry_as_limit`).
    pub topup_retry_as_limit: bool,
    /// LIVE, manual add only: the buy was sent but Kotak's answer was lost, so
    /// it may be live. The reconciler hunts the order book for it (as for an
    /// unconfirmed entry) and adopts it as `topup_order_id`; until then the
    /// position takes no other buy and is never closed out from under it.
    pub topup_send_uncertain: bool,
    /// Quantity of that unconfirmed add, to match it in the order book.
    pub topup_uncertain_qty: Option<i32>,
    /// Order-book polls completed when it went out — see
    /// `MonitoredPosition::entry_uncertain_poll`.
    #[serde(skip)]
    pub topup_uncertain_poll: Option<u64>,
}

/// LIVE mode: hard cap on entry send attempts for one position — see
/// [`MonitoredPosition::entry_attempts`].
pub const MAX_ENTRY_ATTEMPTS: i32 = 3;

fn default_tick_size() -> f64 { 0.05 }

// ===========================================================================
// On-demand broker reconciliation — "Sync with Kotak"
// ===========================================================================
//
// Startup reconciliation (trading_engine::monitor) runs once, automatically,
// and is deliberately conservative: it self-corrects the unambiguous cases
// (broker shows less/none) and only *logs* the ambiguous ones. This is the
// on-demand counterpart triggered from the dashboard mid-session — same
// comparison, but every case is surfaced as a question with explicit options
// rather than silently acted on or merely logged, since by the time someone
// reaches for this button, local state and broker truth have already been
// diverging quietly for a while and a wrong guess is exactly what it exists
// to avoid.

/// One mismatch (or confirmed match) between what the engine tracks and what
/// the broker actually shows, found by a live `GET /api/positions/reconcile`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileFinding {
    /// `None` for broker exposure the engine has no record of at all.
    pub position_id: Option<String>,
    pub trading_symbol: String,
    pub instrument: String,
    pub category: ReconcileCategory,
    pub engine_qty: i32,
    pub broker_qty: i32,
    /// The broker's average buy price for this symbol (a same-day VWAP across
    /// every fill), shown so the user can judge whether to override it when
    /// adopting. `0.0` when the broker has no matching row.
    #[serde(default)]
    pub broker_avg_price: f64,
    pub message: String,
    pub options: Vec<ReconcileOption>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReconcileCategory {
    /// Engine qty and broker qty agree — included for a complete report, not
    /// something to act on.
    Matches,
    /// Broker holds less than the engine believes (partial exit elsewhere).
    QtyReduced,
    /// Broker holds none — the engine believes it still holds some.
    QtyZero,
    /// Broker holds more than the engine believes (extra entry elsewhere).
    QtyIncreased,
    /// The broker holds a quantity for a symbol the engine has zero record
    /// of — not even `WaitingForEntry`.
    UnexplainedExposure,
    /// More than one tracked position maps to the same broker symbol —
    /// unsafe to guess which is which; informational only, no options.
    DuplicateAmbiguous,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileOption {
    pub action: ReconcileAction,
    pub label: String,
    pub recommended: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ReconcileAction {
    /// Set `executed_qty` to the broker's figure; state and stop untouched.
    AdoptQty,
    /// Mark the position `Closed`, `executed_qty` to 0.
    Close,
    /// No local state changes — acknowledges the finding without acting.
    Ignore,
    /// Manually adopt broker exposure the engine has no record of (see
    /// `ReconcileCategory::UnexplainedExposure`) as a brand-new `Active`
    /// position, with a single user-entered target and stop-loss. `target`
    /// seeds `signal.targets[0]` ("target 1"); if `TradingConfig::dynamic_targeting`
    /// is on, the runner extends past it exactly like any other position —
    /// there is no separate target 2 to enter. Quantity is always read fresh
    /// from the broker at apply time, never trusted from the client.
    ///
    /// `avg_buy_price` is the entry price for this specific lot: `> 0.0` uses
    /// it verbatim, `0.0` (or an older client that omits it) falls back to the
    /// broker's figure. Kotak reports a day VWAP across every fill of the
    /// symbol, so when a position was scaled or partly closed that average is
    /// not what this lot actually cost — hence the override.
    AdoptManual {
        stop_loss: f64,
        target: f64,
        #[serde(default)]
        avg_buy_price: f64,
    },
}

/// One user-confirmed action from a reconciliation report, to actually apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileApplyItem {
    pub position_id: Option<String>,
    pub trading_symbol: String,
    pub action: ReconcileAction,
}

// ===========================================================================
// Execution result with full statutory charge breakdown
// ===========================================================================

/// Final result of an executed order including all statutory charges.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    /// Broker-assigned order ID.
    pub order_id: String,
    /// `"COMPLETE"`, `"REJECTED"`, `"PENDING"`, etc.
    pub status: String,
    /// Executed qty × executed price (before charges).
    pub gross_value: f64,
    /// Flat brokerage (INR).
    pub brokerage: f64,
    /// Securities Transaction Tax (INR).
    pub stt_charge: f64,
    /// SEBI turnover fee (INR).
    pub sebi_fee: f64,
    /// Stamp duty (INR).
    pub stamp_duty: f64,
    /// Exchange transaction charge (INR).
    pub transaction_charge: f64,
    /// GST on (brokerage + transaction charge) (INR).
    pub gst: f64,
    /// `gross_value ± brokerage + stt + sebi + stamp + txn + gst` (net INR).
    pub net_value: f64,
    /// ISO-8601 execution timestamp.
    pub timestamp: String,
}

// ===========================================================================
// Kotak Neo API — order placement
// Fields map EXACTLY to the `jData` JSON payload of:
//   POST {baseUrl}/quick/order/rule/ms/place
// Reference: kotak-api-docs/trading-apis.md §4 Request Body Fields
// ===========================================================================

/// Transaction type — Kotak field `tt`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransactionType {
    #[serde(rename = "B")]
    Buy,
    #[serde(rename = "S")]
    Sell,
}

/// Product code — Kotak field `pc`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProductCode {
    #[serde(rename = "NRML")]
    Nrml,
    #[serde(rename = "CNC")]
    Cnc,
    #[serde(rename = "MIS")]
    Mis,
    /// Cover Order (discontinued 1 Apr 2026 — kept for schema completeness).
    #[serde(rename = "CO")]
    Co,
    /// Bracket Order (discontinued 1 Apr 2026 — kept for schema completeness).
    #[serde(rename = "BO")]
    Bo,
    #[serde(rename = "MTF")]
    Mtf,
}

/// Order type — Kotak field `pt`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OrderType {
    #[serde(rename = "L")]
    Limit,
    #[serde(rename = "MKT")]
    Market,
    #[serde(rename = "SL")]
    StopLoss,
    #[serde(rename = "SL-M")]
    StopLossMarket,
}

/// Validity / duration — Kotak field `rt`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Validity {
    #[serde(rename = "DAY")]
    Day,
    #[serde(rename = "IOC")]
    Ioc,
}

/// Exchange segment — Kotak field `es`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExchangeSegment {
    #[serde(rename = "nse_cm")]
    NseCm,
    #[serde(rename = "bse_cm")]
    BseCm,
    #[serde(rename = "nse_fo")]
    NseFo,
    #[serde(rename = "bse_fo")]
    BseFo,
    #[serde(rename = "cde_fo")]
    CdeFo,
    #[serde(rename = "mcx_fo")]
    McxFo,
}

/// After-market order flag — Kotak field `am`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AmoFlag {
    #[serde(rename = "YES")]
    Yes,
    #[serde(rename = "NO")]
    No,
}

/// Kotak Neo `jData` payload for `POST {baseUrl}/quick/order/rule/ms/place`.
///
/// All Rust field names are descriptive; serde renames them to the abbreviated
/// Kotak API keys before serialisation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRequest {
    /// After-market order flag (`am`). `AmoFlag::No` for regular orders.
    #[serde(rename = "am")]
    pub after_market_order: AmoFlag,
    /// Disclosed quantity (`dq`). `"0"` = no disclosure.
    #[serde(rename = "dq")]
    pub disclosed_quantity: String,
    /// Exchange segment (`es`).
    #[serde(rename = "es")]
    pub exchange_segment: ExchangeSegment,
    /// Market protection (`mp`). `"0"` = disabled.
    #[serde(rename = "mp")]
    pub market_protection: String,
    /// Product code (`pc`).
    #[serde(rename = "pc")]
    pub product_code: ProductCode,
    /// Portfolio flag (`pf`). Always `"N"` for standard orders.
    #[serde(rename = "pf")]
    pub portfolio_flag: String,
    /// Limit price (`pr`). `"0"` for market orders.
    #[serde(rename = "pr")]
    pub price: String,
    /// Order type (`pt`).
    #[serde(rename = "pt")]
    pub order_type: OrderType,
    /// Quantity (`qt`).
    #[serde(rename = "qt")]
    pub quantity: String,
    /// Validity (`rt`).
    #[serde(rename = "rt")]
    pub validity: Validity,
    /// Trigger price (`tp`). `"0"` for non-SL orders.
    #[serde(rename = "tp")]
    pub trigger_price: String,
    /// Trading symbol from the scrip master (`ts`), e.g. `"NIFTY26JUL2600PE"`.
    #[serde(rename = "ts")]
    pub trading_symbol: String,
    /// Transaction type (`tt`).
    #[serde(rename = "tt")]
    pub transaction_type: TransactionType,
}

// ===========================================================================
// Order timing (analytics only — see `kotak_client::orders`)
// ===========================================================================

/// One order sent to Kotak, timed on this server's clock.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderSent {
    /// Just before the request went out, and when Kotak's answer came back.
    pub sent_at_ms: i64,
    pub replied_at_ms: i64,
    /// Kotak's order number, when it took the order.
    pub order_no: Option<String>,
    /// Why it didn't, when it didn't.
    pub error: Option<String>,
    pub trading_symbol: String,
    /// `B` / `S`.
    pub side: String,
    /// `MKT`, `L`, `SL`, `SL-M`.
    pub order_type: String,
    pub qty: i64,
    pub price: f64,
    pub trigger: f64,
}

/// One order update Kotak pushed, stamped with when this server received it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderUpdate {
    pub received_at_ms: i64,
    pub order_no: String,
    /// `open`, `complete`, `rejected`, `cancelled`, `trigger pending`, …
    pub status: String,
    pub trading_symbol: Option<String>,
    pub side: Option<String>,
    pub qty: Option<i64>,
    pub filled_qty: Option<i64>,
    pub avg_price: Option<f64>,
    pub reject_reason: Option<String>,
    /// When Kotak received the update (`updRecvTm`), epoch nanoseconds.
    pub kotak_received_ns: Option<i64>,
    /// The exchange's broadcast time (`boeSec`), epoch seconds.
    pub exchange_epoch_s: Option<i64>,
    /// The exchange's confirmation time (`exCfmTm`), as Kotak writes it.
    pub exchange_confirm: Option<String>,
    /// When the order was placed (`ordDtTm`), as Kotak writes it.
    pub order_time: Option<String>,
}

// ===========================================================================
// Internal persistence channel
// ===========================================================================

/// One finished 1-minute candle for the permanent archive (see
/// [`DbWriteMessage::ArchiveCandles`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArchivedCandle {
    /// Unix ms of the minute's start.
    pub t: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// Traded in the minute; 0 for an index, which has none.
    pub volume: f64,
    /// Open interest at the minute; 0 for an index.
    pub oi: f64,
}

/// Message sent over the `mpsc` channel to the dedicated SQLite writer task.
///
/// Defined here (not in `server`) so both `trading_engine` and `server` can
/// use it without creating a circular dependency.
///
/// Timestamps are omitted from both variants — the DB writer stamps them
/// explicitly in IST before persisting.
#[derive(Debug, Clone)]
pub enum DbWriteMessage {
    Trade {
        ticker: String,
        action: String,
        qty: i32,
        executed_price: f64,
        gross_value: f64,
        brokerage: f64,
        stt_charge: f64,
        sebi_fee: f64,
        stamp_duty: f64,
        transaction_charge: f64,
        gst: f64,
        net_value: f64,
        /// The call it belongs to: signal id, message, chat, side and strike.
        tag: TradeTag,
        exit_reason: Option<String>,
        /// `"LIVE"` or `"PAPER"` — which engine produced this fill.
        mode: String,
    },
    Log {
        level: String,
        message: String,
    },
    PositionsSnapshot {
        json: String,
    },
    /// One minute's aggregated option-chain OI for an index over a strike
    /// window (analytics only — sourced from Upstox, never used for orders).
    OiSnapshot {
        ts: String,
        underlying: String,
        expiry: String,
        /// Strikes each side of ATM included; 0 = the whole chain.
        strike_window: i64,
        call_oi: f64,
        put_oi: f64,
        call_oi_chg: f64,
        put_oi_chg: f64,
        spot: f64,
        fut_ltp: Option<f64>,
        fut_vwap: Option<f64>,
    },
    /// One of today's 1-minute bars for a v2 chart (analytics only), cached
    /// so a restart doesn't lose the day. `source` is `"upstox"` for an
    /// official minute, `"ticks"` for one built from live ticks; an official
    /// minute is never overwritten by a ticked one.
    CandleMinute {
        symbol: String,
        /// Unix ms of the minute's start.
        t: i64,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        volume: Option<f64>,
        source: &'static str,
    },
    /// Finished 1-minute candles for the permanent archive, all one symbol's,
    /// written in one transaction (analytics only — from Upstox, never read
    /// by the engine). Upstox's latest answer for a minute replaces any
    /// stored one.
    ArchiveCandles {
        symbol: String,
        candles: Vec<ArchivedCandle>,
    },
    /// A message from a monitored Telegram chat, for the v2 Desk's feed
    /// (display only — never read by the engine). Kept per chat and message
    /// id, so an edit replaces the text it was first stored with.
    /// A timed order send, for the admin's order timing (analytics only).
    OrderSent(OrderSent),
    /// An order update from Kotak's push stream, merged into its order's
    /// timing row (analytics only).
    OrderUpdate(OrderUpdate),
    TelegramMessage {
        chat_id: i64,
        chat_title: Option<String>,
        msg_id: i64,
        /// When Telegram says it was sent, IST `YYYY-MM-DD HH:MM:SS`.
        sent_at: String,
        text: String,
        /// How the ingester routed it: `signal`, `not_index_option`,
        /// `ignored_call`, `sl_update`, `exit` or `unparsed`.
        kind: &'static str,
        /// The message a reply command answers.
        reply_to: Option<i64>,
        /// A reply's new stop, or its exit price.
        value: Option<f64>,
        /// The index, strike and side a call named, when it named them.
        instrument: Option<String>,
        strike: Option<f64>,
        side: Option<String>,
        edited: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_market_is_open_on_weekdays_from_0915_until_1540() {
        // `is_market_open_at` (`special_session.rs`) also opens for a special
        // session on the day; the guard clears them, so this checks the
        // regular hours alone and no other test's session reaches it.
        let _guard = special_sessions_test_guard();
        let ist = |y, m, d, h, min| {
            NaiveDate::from_ymd_opt(y, m, d)
                .unwrap()
                .and_hms_opt(h, min, 0)
                .unwrap()
                .and_local_timezone(ist_offset())
                .single()
                .unwrap()
        };
        // 2026-11-04 is a Wednesday.
        assert!(!is_market_open_at(ist(2026, 11, 4, 9, 14)));
        assert!(is_market_open_at(ist(2026, 11, 4, 9, 15)));
        assert!(is_market_open_at(ist(2026, 11, 4, 15, 39)));
        assert!(!is_market_open_at(ist(2026, 11, 4, 15, 40)));
        assert!(!is_market_open_at(ist(2026, 11, 4, 20, 0)));
        assert!(!is_market_open_at(ist(2026, 11, 7, 12, 0)), "Saturday");
        assert!(!is_market_open_at(ist(2026, 11, 8, 12, 0)), "Sunday");
    }

    #[test]
    fn signal_saved_before_entry_ranges_still_loads() {
        // Open positions are persisted as JSON; ones written by an older build
        // have no `entry_price_max` and must restore as plain triggers.
        let json = r#"{"instrument_name":"SENSEX","strike":74400.0,"option_type":"CE",
            "expiry":"22-OCT-2026","action":"BUY","entry_condition":"ABOVE","entry_price":620.0,
            "targets":[670.0,900.0],"stop_loss":500.0,"source":"telegram","signal_id":"27962",
            "raw_message":"BUY SENSEX 74400 CE ABOVE 620"}"#;
        let sig: TradeSignal = serde_json::from_str(json).unwrap();
        assert_eq!(sig.entry_price, 620.0);
        assert_eq!(sig.entry_price_max, None);

        let round_trip: TradeSignal = serde_json::from_str(&serde_json::to_string(&TradeSignal {
            entry_price_max: Some(625.0),
            ..sig
        }).unwrap()).unwrap();
        assert_eq!(round_trip.entry_price_max, Some(625.0));
    }

    #[test]
    fn trading_config_kill_switch_defaults_to_false() {
        let json_str = r#"{
            "max_trade_amount_inr": 10000.0,
            "index_lots": 1,
            "other_lots": 1,
            "mode": "PAPER",
            "brokerage_per_order": 20.0,
            "target_1_exit_pct": 50.0,
            "target_2_exit_pct": 100.0
        }"#;

        let cfg: TradingConfig = serde_json::from_str(json_str).expect("deserialize config");
        assert_eq!(cfg.kill_switch_active, false);
    }

    #[test]
    fn trading_config_kill_switch_roundtrips() {
        let json_str = r#"{
            "max_trade_amount_inr": 10000.0,
            "index_lots": 1,
            "other_lots": 1,
            "mode": "PAPER",
            "brokerage_per_order": 20.0,
            "target_1_exit_pct": 50.0,
            "target_2_exit_pct": 100.0,
            "kill_switch_active": true
        }"#;

        let cfg: TradingConfig = serde_json::from_str(json_str).expect("deserialize config");
        assert_eq!(cfg.kill_switch_active, true);

        let serialized = serde_json::to_string(&cfg).expect("serialize config");
        assert!(serialized.contains(r#""kill_switch_active":true"#));
    }

    #[test]
    fn entry_gate_fields_default_to_off_with_a_two_hour_call_window() {
        // An older frontend / saved JSON that predates these fields.
        let json_str = r#"{
            "max_trade_amount_inr": 10000.0,
            "index_lots": 1,
            "other_lots": 1,
            "mode": "LIVE",
            "brokerage_per_order": 20.0,
            "target_1_exit_pct": 50.0,
            "target_2_exit_pct": 100.0
        }"#;
        let cfg: TradingConfig = serde_json::from_str(json_str).expect("deserialize config");
        assert!(!cfg.indicator_sizing);
        assert_eq!(
            cfg.indicator_multipliers(),
            entry_gate::Multipliers { strong: 1.5, agree: 1.0, neutral: 0.5 }
        );
        assert_eq!(cfg.topup_mode(), entry_gate::TopupMode::StrongToAgree);
        assert_eq!(cfg.call_validity_minutes, 120);
        assert_eq!(
            cfg.indicator_params(),
            entry_gate::IndicatorParams {
                interval_min: 15,
                total_oi: false,
                strike_window: 10,
                pcr_buy_above: 1.0,
                pcr_sell_below: 1.0,
                vwap_band_pct: 0.0,
                early_checks: true,
            }
        );
        assert!(cfg.indicator_wait_on_disagree);
        assert!(!cfg.indicator_topup, "top-up buys more, so it must be opted into");
        assert!(!cfg.indicator_sl_lower_gate, "changes how stops move, so it must be opted into");
    }

    #[test]
    fn sanitize_entry_gate_clamps_every_field() {
        let mut cfg: TradingConfig = serde_json::from_str(
            r#"{"max_trade_amount_inr":1,"index_lots":1,"other_lots":1,"mode":"PAPER",
                "brokerage_per_order":20,"target_1_exit_pct":50,"target_2_exit_pct":100,
                "indicator_agree_multiplier":99,"indicator_neutral_multiplier":0,
                "indicator_strong_agree_multiplier":-3,"indicator_topup_mode":"ALWAYS",
                "indicator_interval_min":7,"indicator_basis":"TOTAL","indicator_strike_window":3,
                "indicator_pcr_buy_above":-1,"indicator_pcr_sell_below":2,
                "indicator_vwap_band_pct":50,"call_validity_minutes":-5}"#,
        )
        .expect("deserialize config");
        cfg.sanitize_entry_gate();
        assert_eq!(cfg.indicator_agree_multiplier, 5.0);
        assert_eq!(cfg.indicator_neutral_multiplier, 0.1);
        assert_eq!(cfg.indicator_strong_agree_multiplier, 0.1);
        assert_eq!(cfg.indicator_topup_mode, "strong_to_agree");
        assert_eq!(cfg.indicator_interval_min, 15);
        assert_eq!(cfg.indicator_basis, "total");
        assert_eq!(cfg.indicator_strike_window, 10);
        assert_eq!(cfg.indicator_pcr_buy_above, 0.0);
        assert_eq!(cfg.indicator_pcr_sell_below, 0.0);
        assert_eq!(cfg.indicator_vwap_band_pct, 5.0);
        assert_eq!(cfg.call_validity_minutes, 0);
    }

    fn call(instrument: &str, strike: Option<f64>, side: Option<&str>) -> TradeSignal {
        TradeSignal {
            instrument_name: instrument.into(),
            strike,
            option_type: side.map(Into::into),
            action: "BUY".into(),
            ..Default::default()
        }
    }

    #[test]
    fn index_options_are_ce_or_pe_with_a_strike_on_the_six_indices() {
        for index in INDEX_NAMES {
            assert!(call(index, Some(25000.0), Some("CE")).is_index_option(), "{index}");
            assert!(call(index, Some(25000.0), Some("PE")).is_index_option(), "{index}");
        }
        assert!(call(" nifty ", Some(25000.0), Some("ce")).is_index_option());

        // Stock options and equity.
        assert!(!call("BHEL", Some(425.0), Some("CE")).is_index_option());
        assert!(!call("RELIANCE", None, None).is_index_option());
        // An index call with no strike or side could resolve to a future.
        assert!(!call("NIFTY", None, None).is_index_option());
        assert!(!call("NIFTY", Some(25000.0), None).is_index_option());
        assert!(!call("NIFTY", None, Some("CE")).is_index_option());
        assert!(!call("NIFTY", Some(0.0), Some("CE")).is_index_option());
        assert!(!call("NIFTY", Some(25000.0), Some("XX")).is_index_option());
        // Exact names only: no aliases, no substrings.
        assert!(!call("NIFTY50", Some(25000.0), Some("CE")).is_index_option());
        assert!(!call("NIFTYIT", Some(25000.0), Some("CE")).is_index_option());
        // A reply's bare update signal is not a call at all.
        assert!(!call("UPDATE", None, None).is_index_option());
    }

    fn scalp(side: &str, chart: ChartLevels) -> MonitoredPosition {
        MonitoredPosition {
            signal: TradeSignal {
                instrument_name: "NIFTY".into(),
                strike: Some(25_100.0),
                option_type: Some(side.into()),
                action: "BUY".into(),
                ..Default::default()
            },
            manual: true,
            chart,
            ..Default::default()
        }
    }

    fn on_index(price: f64) -> ChartLevel {
        ChartLevel { price, trigger: LevelTrigger::Index }
    }

    fn on_premium(price: f64) -> ChartLevel {
        ChartLevel { price, trigger: LevelTrigger::Premium }
    }

    /// An "all left" target at `l`.
    fn all_at(l: ChartLevel) -> ChartTarget {
        ChartTarget { id: format!("t{}", l.price), price: l.price, trigger: l.trigger, lots: None }
    }

    fn lots_at(id: &str, l: ChartLevel, lots: i32) -> ChartTarget {
        ChartTarget { id: id.into(), price: l.price, trigger: l.trigger, lots: Some(lots) }
    }

    fn levels(stop: Option<ChartLevel>, targets: Vec<ChartTarget>) -> ChartLevels {
        ChartLevels { stop, targets }
    }

    /// The exit reason a whole-holding hit gives, for a position of 4 lots.
    fn reason(p: &MonitoredPosition, premium: Option<f64>, index: Option<f64>) -> Option<&'static str> {
        match p.chart_hit(premium, index, 1) {
            Some(ChartHit::All { reason }) => Some(reason),
            Some(ChartHit::Part { .. }) => Some("PART"),
            None => None,
        }
    }

    fn holding(side: &str, chart: ChartLevels) -> MonitoredPosition {
        MonitoredPosition { executed_qty: 4, ..scalp(side, chart) }
    }

    #[test]
    fn index_levels_follow_the_side_of_the_option() {
        // A call: stop below the index, target above it.
        let call = holding("CE", levels(Some(on_index(25_060.0)), vec![all_at(on_index(25_180.0))]));
        assert_eq!(reason(&call, Some(100.0), Some(25_100.0)), None);
        assert_eq!(reason(&call, Some(100.0), Some(25_060.0)), Some("CHART_STOP"));
        assert_eq!(reason(&call, Some(100.0), Some(25_000.0)), Some("CHART_STOP"));
        assert_eq!(reason(&call, Some(100.0), Some(25_180.0)), Some("CHART_TARGET"));
        // A put: stop above the index, target below it.
        let put = holding("PE", levels(Some(on_index(25_140.0)), vec![all_at(on_index(25_020.0))]));
        assert_eq!(reason(&put, Some(100.0), Some(25_100.0)), None);
        assert_eq!(reason(&put, Some(100.0), Some(25_140.0)), Some("CHART_STOP"));
        assert_eq!(reason(&put, Some(100.0), Some(25_200.0)), Some("CHART_STOP"));
        assert_eq!(reason(&put, Some(100.0), Some(25_020.0)), Some("CHART_TARGET"));
        // No index value yet: nothing fires.
        assert_eq!(reason(&call, Some(100.0), None), None);
    }

    #[test]
    fn premium_levels_work_the_same_for_calls_and_puts() {
        for side in ["CE", "PE"] {
            let pos = holding(side, levels(Some(on_premium(92.5)), vec![all_at(on_premium(140.0))]));
            assert_eq!(reason(&pos, Some(100.0), Some(25_000.0)), None, "{side}");
            assert_eq!(reason(&pos, Some(92.5), None), Some("CHART_STOP"), "{side}");
            assert_eq!(reason(&pos, Some(140.05), None), Some("CHART_TARGET"), "{side}");
            assert_eq!(reason(&pos, None, Some(25_000.0)), None, "{side}");
        }
    }

    #[test]
    fn a_stop_wins_over_a_target_and_only_real_prices_count() {
        let mixed = holding("CE", levels(Some(on_premium(92.5)), vec![all_at(on_index(25_180.0))]));
        assert_eq!(reason(&mixed, Some(90.0), Some(25_200.0)), Some("CHART_STOP"));
        assert_eq!(reason(&mixed, Some(0.0), Some(0.0)), None);
        assert_eq!(reason(&holding("CE", ChartLevels::default()), Some(1.0), Some(1.0)), None);
        // Only options have chart levels.
        let mut not_an_option = mixed;
        not_an_option.signal.option_type = None;
        assert_eq!(reason(&not_an_option, Some(90.0), Some(25_200.0)), None);
    }

    #[test]
    fn a_target_with_lots_sells_only_its_lots() {
        // 4 lots of 75: 2 at ₹120, 2 at ₹140.
        let mut pos = holding("CE", levels(Some(on_premium(90.0)), vec![lots_at("a", on_premium(120.0), 2), lots_at("b", on_premium(140.0), 2)]));
        pos.executed_qty = 300;
        assert_eq!(pos.chart_hit(Some(110.0), None, 75), None);
        assert_eq!(pos.chart_hit(Some(120.0), None, 75), Some(ChartHit::Part { lots: 2, ids: vec!["a".into()] }));
        // Both reached at once (a jump, or a line dropped past the price): one sell for all their lots — here the whole holding.
        assert_eq!(pos.chart_hit(Some(141.0), None, 75), Some(ChartHit::All { reason: "CHART_TARGET" }));
        // After the first sold: the second's 2 lots are all that's left.
        pos.executed_qty = 150;
        pos.chart.targets.remove(0);
        assert_eq!(pos.chart_hit(Some(140.0), None, 75), Some(ChartHit::All { reason: "CHART_TARGET" }));
        // The stop still sells everything.
        assert_eq!(pos.chart_hit(Some(89.0), None, 75), Some(ChartHit::All { reason: "CHART_STOP" }));
    }

    #[test]
    fn an_all_left_target_sells_whatever_is_held() {
        // 2 lots at ₹120, all left at ₹130 — including lots added later.
        let mut pos = holding("CE", levels(None, vec![lots_at("a", on_premium(120.0), 2), all_at(on_premium(130.0))]));
        pos.executed_qty = 6;
        assert_eq!(pos.chart_hit(Some(120.0), None, 1), Some(ChartHit::Part { lots: 2, ids: vec!["a".into()] }));
        assert_eq!(pos.chart_hit(Some(130.0), None, 1), Some(ChartHit::All { reason: "CHART_TARGET" }));
    }

    #[test]
    fn target_lots_never_add_up_past_the_holding() {
        let mut chart = ChartLevels::default();
        let edit = |id: Option<&str>, price: f64, lots: Option<Option<i32>>| TargetEdit { id: id.map(Into::into), price, trigger: LevelTrigger::Premium, lots };
        let a = chart.set_target(&edit(None, 120.0, Some(Some(2))), 4).unwrap();
        let b = chart.set_target(&edit(None, 140.0, Some(Some(2))), 4).unwrap();
        assert_ne!(a, b);
        assert!(matches!(chart.set_target(&edit(None, 150.0, Some(Some(1))), 4), Err(LevelRefusal::Conflict(_))));
        // "All left" always fits.
        chart.set_target(&edit(None, 150.0, Some(None)), 4).unwrap();
        // A move keeps its lots — even once the holding is smaller than the targets.
        chart.set_target(&edit(Some(&a), 125.0, None), 3).unwrap();
        assert_eq!(chart.targets[0], ChartTarget { id: a.clone(), price: 125.0, trigger: LevelTrigger::Premium, lots: Some(2) });
        // Resizing past the holding is refused; within it, taken.
        assert!(matches!(chart.set_target(&edit(Some(&a), 125.0, Some(Some(3))), 4), Err(LevelRefusal::Conflict(_))));
        chart.set_target(&edit(Some(&a), 125.0, Some(Some(1))), 4).unwrap();
        assert_eq!(chart.targets[0].lots, Some(1));
        // A target that has sold can't be moved; no lot count below 1, no bad price.
        assert!(chart.remove_target(&a));
        assert!(matches!(chart.set_target(&edit(Some(&a), 125.0, None), 4), Err(LevelRefusal::Conflict(_))));
        assert!(matches!(chart.set_target(&edit(None, 125.0, Some(Some(0))), 4), Err(LevelRefusal::BadRequest(_))));
        assert!(matches!(chart.set_target(&edit(None, f64::NAN, None), 4), Err(LevelRefusal::BadRequest(_))));
        // At most MAX_CHART_TARGETS.
        while chart.targets.len() < MAX_CHART_TARGETS {
            chart.set_target(&edit(None, 160.0, Some(None)), 4).unwrap();
        }
        assert!(matches!(chart.set_target(&edit(None, 170.0, Some(None)), 4), Err(LevelRefusal::Conflict(_))));
    }

    #[test]
    fn no_lot_count_wraps_round_past_the_check() {
        let edit = |id: Option<&str>, lots: i32| TargetEdit { id: id.map(Into::into), price: 140.0, trigger: LevelTrigger::Premium, lots: Some(Some(lots)) };
        let mut chart = ChartLevels::default();
        chart.set_target(&edit(None, 2), 4).unwrap();
        // 2 + i32::MAX would wrap negative in a release build and pass.
        assert!(matches!(chart.set_target(&edit(None, i32::MAX), 4), Err(LevelRefusal::Conflict(_))));
        assert_eq!(chart.targets.len(), 1);
        // Stored counts that big, reached together, sell everything — never a negative part.
        let huge = levels(None, vec![lots_at("a", on_premium(120.0), i32::MAX), lots_at("b", on_premium(121.0), i32::MAX)]);
        assert_eq!(chart_hit(Some("CE"), &huge, Some(125.0), None, 4), Some(ChartHit::All { reason: "CHART_TARGET" }));
    }

    #[test]
    fn a_target_can_shrink_after_the_holding_has() {
        // 2 + 2 lots of targets, then a −2 lots sell: 2 held.
        let mut chart = levels(None, vec![lots_at("a", on_premium(120.0), 2), lots_at("b", on_premium(140.0), 2)]);
        let resize = |lots: i32| TargetEdit { id: Some("a".into()), price: 120.0, trigger: LevelTrigger::Premium, lots: Some(Some(lots)) };
        chart.set_target(&resize(1), 2).unwrap();
        assert_eq!(chart.targets[0].lots, Some(1));
        // Growing it back past the holding is still refused.
        assert!(matches!(chart.set_target(&resize(2), 2), Err(LevelRefusal::Conflict(_))));
        // "All left" turned into a count adds every lot it asks for.
        let mut all = levels(None, vec![lots_at("a", on_premium(120.0), 2), all_at(on_premium(140.0))]);
        let id = all.targets[1].id.clone();
        let count = TargetEdit { id: Some(id), price: 140.0, trigger: LevelTrigger::Premium, lots: Some(Some(3)) };
        assert!(matches!(all.set_target(&count, 4), Err(LevelRefusal::Conflict(_))));
    }

    #[test]
    fn a_trimmed_target_sell_splits_its_targets_at_the_lots_sent() {
        let taken = vec![lots_at("a", on_premium(120.0), 2), lots_at("b", on_premium(121.0), 3)];
        // 3 of the 5 lots sent: all of a and 1 of b go out, 2 of b stay on the chart.
        let (sent, unsent) = split_target_lots(taken.clone(), 3);
        assert_eq!(sent.iter().map(|t| (t.id.as_str(), t.lots)).collect::<Vec<_>>(), [("a", Some(2)), ("b", Some(1))]);
        assert_eq!(unsent.iter().map(|t| (t.id.as_str(), t.lots)).collect::<Vec<_>>(), [("b", Some(2))]);
        assert_eq!(split_target_lots(taken.clone(), 0).0, vec![]);
        assert_eq!(split_target_lots(taken.clone(), 9).1, vec![]);
        // "All left" is never counted as sent.
        let (sent, unsent) = split_target_lots(vec![all_at(on_premium(150.0))], 4);
        assert!(sent.is_empty() && unsent.len() == 1);
    }

    #[test]
    fn an_unfilled_target_sell_puts_back_what_it_did_not_sell() {
        let mut chart = levels(None, vec![lots_at("a", on_premium(120.0), 2), lots_at("b", on_premium(121.0), 3), lots_at("c", on_premium(140.0), 1)]);
        let taken = chart.take_targets(&["a".into(), "b".into()]);
        assert_eq!(taken.len(), 2);
        assert_eq!(chart.targets.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["c"]);
        // 3 of the 5 lots filled: a is done, b has 2 left.
        chart.restore_unsold(taken.clone(), 3);
        assert_eq!(chart.targets.iter().map(|t| (t.id.as_str(), t.lots)).collect::<Vec<_>>(), [("c", Some(1)), ("b", Some(2))]);
        // Nothing filled: both back as they were.
        let mut none = levels(None, vec![]);
        none.restore_unsold(taken, 0);
        assert_eq!(none.targets.iter().map(|t| (t.id.as_str(), t.lots)).collect::<Vec<_>>(), [("a", Some(2)), ("b", Some(3))]);
    }

    #[test]
    fn chart_levels_stay_out_of_the_json_until_set() {
        let plain = scalp("CE", ChartLevels::default());
        let json = serde_json::to_value(&plain).unwrap();
        assert!(json.get("chart").is_none(), "positions without levels serialize as before");
        assert!(json.get("chart_targets_selling").is_none());
        let restored: MonitoredPosition = serde_json::from_value(json).unwrap();
        assert!(restored.chart.is_empty());

        let with = scalp("PE", levels(Some(on_index(25_140.0)), vec![lots_at("a", on_premium(140.0), 2)]));
        let json = serde_json::to_value(&with).unwrap();
        assert_eq!(
            json["chart"],
            serde_json::json!({ "stop": { "price": 25140.0, "trigger": "index" }, "targets": [{ "id": "a", "price": 140.0, "trigger": "premium", "lots": 2 }] })
        );
        let restored: MonitoredPosition = serde_json::from_value(json).unwrap();
        assert_eq!(restored.chart, with.chart);
    }

    #[test]
    fn a_target_stored_before_lots_comes_back_as_all_left() {
        let stored = serde_json::json!({ "stop": null, "target": { "price": 140.0, "trigger": "premium" } });
        let chart: ChartLevels = serde_json::from_value(stored).unwrap();
        assert_eq!(chart.stop, None);
        assert_eq!(chart.targets.len(), 1);
        assert_eq!((chart.targets[0].price, chart.targets[0].trigger, chart.targets[0].lots), (140.0, LevelTrigger::Premium, None));
        assert!(!chart.targets[0].id.is_empty());
        let none: ChartLevels = serde_json::from_value(serde_json::json!({ "stop": null, "target": null })).unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn feature_flags_default_off_and_keep_their_api_names() {
        assert_eq!(
            FeatureFlags::default(),
            FeatureFlags { index_options_only: false, chart_stops: false, search: false, watchlists: false, stock_charts: false }
        );
        assert_eq!(
            serde_json::to_value(FeatureFlags { index_options_only: true, ..FeatureFlags::default() }).unwrap(),
            serde_json::json!({ "index_options_only": true, "chart_stops": false, "search": false, "watchlists": false, "stock_charts": false }),
        );
        // A client from before a switch existed leaves it off.
        let old: FeatureFlags = serde_json::from_str(r#"{"index_options_only": true, "chart_stops": true}"#).unwrap();
        assert_eq!(old, FeatureFlags { index_options_only: true, chart_stops: true, search: false, watchlists: false, stock_charts: false });

        // `entries` and `set` use exactly the JSON field names, so a stored
        // row always lands on the switch the API shows.
        let flags = FeatureFlags { index_options_only: true, chart_stops: false, search: true, watchlists: true, stock_charts: true };
        let json = serde_json::to_value(flags).unwrap();
        assert_eq!(json.as_object().unwrap().len(), flags.entries().len());
        for (name, on) in flags.entries() {
            assert_eq!(json[name], serde_json::json!(on), "{name}");
            let mut copy = FeatureFlags::default();
            assert!(copy.set(name, on));
            assert_eq!(copy.entries().into_iter().find(|(n, _)| *n == name), Some((name, on)));
        }
        assert!(!FeatureFlags::default().set("no_such_flag", true));
    }
}

