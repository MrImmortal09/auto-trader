use serde::{Deserialize, Serialize};

// ===========================================================================
// Trading configuration
// ===========================================================================

/// System-wide trading parameters stored in the `trading_config` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingConfig {
    /// Maximum capital allocated per trade in INR.
    pub max_trade_amount_inr: f64,
    /// `"LIVE"` or `"PAPER"`.
    pub mode: String,
    /// Flat brokerage charged per order leg (INR).
    pub brokerage_per_order: f64,
    /// Percentage of target-1 profit at which to exit 50 % of the position.
    pub target_1_exit_pct: f64,
    /// Percentage of target-2 profit at which to exit the remaining position.
    pub target_2_exit_pct: f64,
}

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
    /// Current stop-loss level (may be trailed upward from initial SL).
    pub current_sl: f64,
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
/// Timestamps are omitted from both variants — SQLite's
/// `DEFAULT CURRENT_TIMESTAMP` fills them automatically.
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
    },
    Log {
        level: String,
        message: String,
    },
}
