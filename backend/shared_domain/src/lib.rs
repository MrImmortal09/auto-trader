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
}

fn default_entry_mp() -> f64 { 5.0 }
fn default_trail_factor() -> f64 { 0.5 }
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
    /// LIVE mode: set when the engine has given up acting on this position
    /// automatically. Requires manual intervention; no further orders are sent.
    #[serde(default)]
    pub live_halt: Option<String>,
}

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
    /// there is no separate target 2 to enter. Quantity and average buy
    /// price are read fresh from the broker at apply time, never trusted
    /// from the client.
    AdoptManual { stop_loss: f64, target: f64 },
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
}
