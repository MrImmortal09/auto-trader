use std::collections::HashMap;

use chrono::{DateTime, Duration as ChronoDuration, FixedOffset, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// Underlying index names this app treats as "index" options (vs. stock
/// options, which fall back to `TradingConfig::other_lots`). Matched against
/// `TradeSignal::instrument_name.to_uppercase()` with an *exact* match — do
/// not switch to a substring check, since e.g. "BANKNIFTY" and "FINNIFTY"
/// both contain "NIFTY".
pub const INDEX_NAMES: &[&str] = &["NIFTY", "BANKNIFTY", "FINNIFTY", "MIDCPNIFTY", "SENSEX", "BANKEX"];

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

pub fn is_market_open() -> bool {
    use chrono::Timelike;
    use chrono::Datelike;
    let now = now_ist();

    // Check if it is a weekday
    let weekday = now.weekday();
    if weekday == chrono::Weekday::Sat || weekday == chrono::Weekday::Sun {
        return false;
    }

    let h = now.hour();
    let m = now.minute();

    // Market hours are 09:15 to 15:40 IST (close is exclusive — sharp cutoff)
    if h < MARKET_OPEN_HOUR || (h == MARKET_OPEN_HOUR && m < MARKET_OPEN_MINUTE) {
        return false;
    }
    if h > MARKET_CLOSE_HOUR || (h == MARKET_CLOSE_HOUR && m >= MARKET_CLOSE_MINUTE) {
        return false;
    }

    true
}

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

/// The next market open instant (09:15:00 IST) strictly in the future,
/// skipping weekends. If today is a weekday and it's before 09:15, returns
/// today's open; otherwise returns the next weekday's open.
pub fn next_market_open_ist() -> DateTime<FixedOffset> {
    use chrono::Datelike;
    let now = now_ist();
    let mut date = now.date_naive();

    let today_open = date
        .and_hms_opt(MARKET_OPEN_HOUR, MARKET_OPEN_MINUTE, 0)
        .expect("valid open time")
        .and_local_timezone(ist_offset())
        .single()
        .expect("IST open time is unambiguous");

    let is_weekday = date.weekday() != chrono::Weekday::Sat && date.weekday() != chrono::Weekday::Sun;
    if is_weekday && now < today_open {
        return today_open;
    }

    // Advance to the next weekday
    loop {
        date += ChronoDuration::days(1);
        if date.weekday() != chrono::Weekday::Sat && date.weekday() != chrono::Weekday::Sun {
            break;
        }
    }

    date
        .and_hms_opt(MARKET_OPEN_HOUR, MARKET_OPEN_MINUTE, 0)
        .expect("valid open time")
        .and_local_timezone(ist_offset())
        .single()
        .expect("IST open time is unambiguous")
}

/// Duration to sleep until the next market open. Zero if the market is open right now.
pub fn duration_until_market_open() -> std::time::Duration {
    if is_market_open() {
        return std::time::Duration::ZERO;
    }
    let now = now_ist();
    let next_open = next_market_open_ist();
    (next_open - now).to_std().unwrap_or(std::time::Duration::ZERO)
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
    /// recurrence. Off by default: existing signals keep exiting at their own
    /// target 2.
    #[serde(default)]
    pub dynamic_targeting: bool,
    /// Multiplier applied to `diff` when trailing the stop under
    /// `dynamic_targeting` (see above). `0.5` trails halfway back from each
    /// rung; `0.0` locks the stop at the rung itself (tightest); `1.0` trails
    /// only back to the previous rung (loosest — on the first rung that's
    /// breakeven at entry). Clamped to `[0.0, 1.0]` on save so the stop can
    /// never trail below the entry price. Unused unless `dynamic_targeting`
    /// is on; PAPER mode does not use this — it always trails to the fixed
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
    /// `dynamic_targeting` is on; LIVE only, like the rest of dynamic
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
}

fn default_entry_mp() -> f64 { 5.0 }
fn default_trail_factor() -> f64 { 0.5 }
fn default_pre_t1_arm_pct() -> f64 { 60.0 }
fn default_extension_factor() -> f64 { 1.0 }

// ===========================================================================
// Trade signal (options-aware)
// ===========================================================================

/// An inbound signal parsed from Telegram or any other source.
///
/// Supports equity, F&O, and options instruments.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

// ===========================================================================
// Position lifecycle
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TradeState {
    /// Order placed; waiting for price to cross `entry_price`.
    WaitingForEntry,
    /// Position is open and actively being monitored.
    Active,
    /// First target hit; partial exit done and trailing SL engaged.
    Target1Hit,
    /// Position fully closed (target 2 hit, SL triggered, or manual close).
    Closed,
}

/// A live position held in memory by the trading engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
// Internal persistence channel
// ===========================================================================

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
        signal_id: Option<String>,
        raw_message: Option<String>,
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

