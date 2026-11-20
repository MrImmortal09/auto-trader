//! An account on Hyperliquid, read through the public `info` endpoint: its
//! margin and positions on one dex (`clearinghouseState`), its open orders
//! with their stop/target children (`frontendOpenOrders`), its fills
//! (`userFills`), and the API wallets it has approved (`extraAgents`).
//! Read-only: none of these needs a key.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::{num, HlError, InfoClient};

/// A dex's margin summary and positions for one account. A builder-deployed
/// dex (`xyz`, gold's) keeps its own margin, apart from the core perps'.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountState {
    pub account_value: f64,
    pub margin_used: f64,
    /// What could be taken out now.
    pub withdrawable: f64,
    pub positions: Vec<HlPosition>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HlPosition {
    pub coin: String,
    /// Signed: negative is short.
    pub szi: f64,
    pub entry: Option<f64>,
    pub leverage: u32,
    pub isolated: bool,
    pub liquidation: Option<f64>,
    pub margin_used: f64,
    pub position_value: f64,
    pub upnl: f64,
    /// P&L on the margin, as a fraction.
    pub roe: f64,
    /// Funding since the position opened (positive: paid), when sent.
    pub funding_since_open: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLeverage {
    #[serde(rename = "type")]
    kind: String,
    value: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFunding {
    since_open: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPosition {
    coin: String,
    szi: String,
    entry_px: Option<String>,
    leverage: RawLeverage,
    liquidation_px: Option<String>,
    margin_used: String,
    position_value: String,
    unrealized_pnl: String,
    return_on_equity: String,
    #[serde(default)]
    cum_funding: Option<RawFunding>,
}

#[derive(Deserialize)]
struct RawAssetPosition {
    position: RawPosition,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSummary {
    account_value: String,
    total_margin_used: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawState {
    asset_positions: Vec<RawAssetPosition>,
    margin_summary: RawSummary,
    withdrawable: String,
}

fn opt_num(s: &Option<String>) -> Result<Option<f64>, HlError> {
    s.as_deref().map(num).transpose()
}

fn read_state(raw: RawState) -> Result<AccountState, HlError> {
    let mut positions = Vec::new();
    for ap in raw.asset_positions {
        let p = ap.position;
        positions.push(HlPosition {
            szi: num(&p.szi)?,
            entry: opt_num(&p.entry_px)?,
            leverage: p.leverage.value,
            isolated: p.leverage.kind == "isolated",
            liquidation: opt_num(&p.liquidation_px)?,
            margin_used: num(&p.margin_used)?,
            position_value: num(&p.position_value)?,
            upnl: num(&p.unrealized_pnl)?,
            roe: num(&p.return_on_equity)?,
            funding_since_open: p.cum_funding.as_ref().map(|f| opt_num(&f.since_open)).transpose()?.flatten(),
            coin: p.coin,
        });
    }
    Ok(AccountState {
        account_value: num(&raw.margin_summary.account_value)?,
        margin_used: num(&raw.margin_summary.total_margin_used)?,
        withdrawable: num(&raw.withdrawable)?,
        positions,
    })
}

/// An open order. A trigger (a stop, a target, a stop entry) carries its
/// trigger price; an entry sent with a stop and target carries them as
/// `children`, live once it fills.
#[derive(Debug, Clone, PartialEq)]
pub struct HlOpenOrder {
    pub coin: String,
    pub oid: u64,
    pub is_buy: bool,
    pub limit_px: f64,
    pub sz: f64,
    pub orig_sz: f64,
    pub reduce_only: bool,
    pub is_trigger: bool,
    pub trigger_px: Option<f64>,
    /// A stop or target on the whole position, resized with it.
    pub is_position_tpsl: bool,
    /// `Limit`, `Stop Market`, `Take Profit Market`, `Stop Limit`, …
    pub order_type: String,
    /// `Price above 1810`, `N/A`, …
    pub trigger_condition: String,
    pub timestamp: i64,
    pub children: Vec<HlOpenOrder>,
}

impl HlOpenOrder {
    /// A stop (`sl`) rather than a take-profit, by its type.
    pub fn is_stop(&self) -> bool {
        self.order_type.starts_with("Stop")
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawOrder {
    coin: String,
    oid: u64,
    side: String,
    limit_px: String,
    sz: String,
    #[serde(default)]
    orig_sz: Option<String>,
    #[serde(default)]
    reduce_only: bool,
    #[serde(default)]
    is_trigger: bool,
    #[serde(default)]
    trigger_px: Option<String>,
    #[serde(default)]
    is_position_tpsl: bool,
    #[serde(default)]
    order_type: String,
    #[serde(default)]
    trigger_condition: String,
    timestamp: i64,
    #[serde(default)]
    children: Vec<RawOrder>,
}

fn read_order(r: RawOrder) -> Result<HlOpenOrder, HlError> {
    let sz = num(&r.sz)?;
    let trigger_px = opt_num(&r.trigger_px)?.filter(|p| *p > 0.0);
    Ok(HlOpenOrder {
        oid: r.oid,
        is_buy: r.side == "B",
        limit_px: num(&r.limit_px)?,
        sz,
        orig_sz: opt_num(&r.orig_sz)?.unwrap_or(sz),
        reduce_only: r.reduce_only,
        is_trigger: r.is_trigger,
        trigger_px,
        is_position_tpsl: r.is_position_tpsl,
        order_type: r.order_type,
        trigger_condition: r.trigger_condition,
        timestamp: r.timestamp,
        children: r.children.into_iter().map(read_order).collect::<Result<_, _>>()?,
        coin: r.coin,
    })
}

/// A fill of the account's.
#[derive(Debug, Clone, PartialEq)]
pub struct HlFill {
    pub coin: String,
    pub px: f64,
    pub sz: f64,
    pub is_buy: bool,
    /// Unix ms.
    pub time: i64,
    /// P&L the fill closed.
    pub closed_pnl: f64,
    pub fee: f64,
    pub oid: u64,
    pub tid: u64,
    /// `Open Long`, `Close Short`, …
    pub dir: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFill {
    coin: String,
    px: String,
    sz: String,
    side: String,
    time: i64,
    closed_pnl: String,
    fee: String,
    oid: u64,
    tid: u64,
    #[serde(default)]
    dir: String,
}

/// An API wallet an account has approved.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub address: String,
    #[serde(default)]
    pub name: String,
    /// Unix ms it stops working, if it does.
    #[serde(default)]
    pub valid_until: Option<i64>,
}

impl InfoClient {
    /// `user`'s margin and positions on `dex` (`None`: the core perps).
    pub async fn account_state(&self, user: &str, dex: Option<&str>) -> Result<AccountState, HlError> {
        let raw: RawState = serde_json::from_value(self.post(json!({ "type": "clearinghouseState", "user": user, "dex": dex.unwrap_or("") })).await?)
            .map_err(|e| HlError::Decode(format!("clearinghouseState: {e}")))?;
        read_state(raw)
    }

    /// `user`'s open orders on `dex`, triggers and their children included.
    pub async fn open_orders(&self, user: &str, dex: Option<&str>) -> Result<Vec<HlOpenOrder>, HlError> {
        let raw: Vec<RawOrder> = serde_json::from_value(self.post(json!({ "type": "frontendOpenOrders", "user": user, "dex": dex.unwrap_or("") })).await?)
            .map_err(|e| HlError::Decode(format!("frontendOpenOrders: {e}")))?;
        raw.into_iter().map(read_order).collect()
    }

    /// `user`'s latest fills (Hyperliquid answers the newest 2,000).
    pub async fn user_fills(&self, user: &str) -> Result<Vec<HlFill>, HlError> {
        let raw: Vec<RawFill> = serde_json::from_value(self.post(json!({ "type": "userFills", "user": user })).await?)
            .map_err(|e| HlError::Decode(format!("userFills: {e}")))?;
        raw.into_iter()
            .map(|f| {
                Ok(HlFill {
                    px: num(&f.px)?,
                    sz: num(&f.sz)?,
                    is_buy: f.side == "B",
                    time: f.time,
                    closed_pnl: num(&f.closed_pnl)?,
                    fee: num(&f.fee)?,
                    oid: f.oid,
                    tid: f.tid,
                    dir: f.dir,
                    coin: f.coin,
                })
            })
            .collect()
    }

    /// The API wallets `user` has approved.
    pub async fn agents(&self, user: &str) -> Result<Vec<Agent>, HlError> {
        let v: Value = self.post(json!({ "type": "extraAgents", "user": user })).await?;
        serde_json::from_value(v).map_err(|e| HlError::Decode(format!("extraAgents: {e}")))
    }

    /// `user`'s leverage setting on `coin` now: (isolated, value). Hyperliquid
    /// keeps one per coin, used by every order on it.
    pub async fn asset_leverage(&self, user: &str, coin: &str) -> Result<(bool, u32), HlError> {
        read_leverage(&self.post(json!({ "type": "activeAssetData", "user": user, "coin": coin })).await?)
    }

    /// What Hyperliquid knows `address` as: an account, an API wallet (with
    /// the account it trades for), a vault, a sub-account, or nothing.
    pub async fn user_role(&self, address: &str) -> Result<UserRole, HlError> {
        read_role(&self.post(json!({ "type": "userRole", "user": address })).await?)
    }
}

fn read_leverage(v: &Value) -> Result<(bool, u32), HlError> {
    let lev = v.get("leverage").ok_or_else(|| HlError::Decode(format!("activeAssetData without a leverage: {v}")))?;
    let isolated = lev.get("type").and_then(Value::as_str) == Some("isolated");
    let value = lev.get("value").and_then(Value::as_u64).ok_or_else(|| HlError::Decode(format!("a leverage without its value: {lev}")))?;
    Ok((isolated, value as u32))
}

/// An address as Hyperliquid knows it (`userRole`).
#[derive(Debug, Clone, PartialEq)]
pub enum UserRole {
    /// An account: its own key can withdraw.
    User,
    /// An API wallet, approved by `user`.
    Agent { user: String },
    Vault,
    SubAccount { master: String },
    /// Never seen on this network.
    Missing,
    Other(String),
}

fn read_role(v: &Value) -> Result<UserRole, HlError> {
    let role = v.get("role").and_then(Value::as_str).ok_or_else(|| HlError::Decode(format!("userRole without a role: {v}")))?;
    let field = |name: &str| v.get("data").and_then(|d| d.get(name)).and_then(Value::as_str).map(str::to_ascii_lowercase);
    Ok(match role {
        "user" => UserRole::User,
        "agent" => UserRole::Agent { user: field("user").ok_or_else(|| HlError::Decode("an agent's userRole without its user".into()))? },
        "vault" => UserRole::Vault,
        "subAccount" => UserRole::SubAccount { master: field("master").unwrap_or_default() },
        "missing" => UserRole::Missing,
        other => UserRole::Other(other.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_coins_leverage_setting_reads_its_kind_and_value() {
        let v: Value = serde_json::from_str(r#"{"user":"0x1","coin":"BTC","leverage":{"type":"isolated","value":5,"rawUsd":"-12.0"},"maxTradeSzs":["0.1","0.1"],"markPx":"61696.0"}"#).unwrap();
        assert_eq!(read_leverage(&v).unwrap(), (true, 5));
        let v: Value = serde_json::from_str(r#"{"leverage":{"type":"cross","value":20}}"#).unwrap();
        assert_eq!(read_leverage(&v).unwrap(), (false, 20));
        assert!(read_leverage(&serde_json::json!({})).is_err());
    }

    #[test]
    fn a_role_says_whose_api_wallet_it_is() {
        let role = |raw: &str| read_role(&serde_json::from_str(raw).unwrap()).unwrap();
        assert_eq!(role(r#"{"role":"user"}"#), UserRole::User);
        assert_eq!(role(r#"{"role":"agent","data":{"user":"0xAbC0000000000000000000000000000000000001"}}"#), UserRole::Agent { user: "0xabc0000000000000000000000000000000000001".into() });
        assert_eq!(role(r#"{"role":"missing"}"#), UserRole::Missing);
        assert_eq!(role(r#"{"role":"vault"}"#), UserRole::Vault);
        assert!(read_role(&serde_json::from_str(r#"{"role":"agent"}"#).unwrap()).is_err(), "an agent without its account");
    }

    #[test]
    fn a_clearinghouse_state_reads_its_positions() {
        let raw: RawState = serde_json::from_str(
            r#"{"assetPositions":[{"position":{"coin":"BTC","cumFunding":{"allTime":"1.2","sinceChange":"0.1","sinceOpen":"0.3"},"entryPx":"63768.5","leverage":{"rawUsd":"-500.0","type":"isolated","value":5},"liquidationPx":"51660.5","marginUsed":"127.5","maxLeverage":40,"positionValue":"637.5","returnOnEquity":"-0.0017","szi":"-0.01","unrealizedPnl":"-0.22"},"type":"oneWay"}],"crossMaintenanceMarginUsed":"0.0","crossMarginSummary":{"accountValue":"1000.0","totalMarginUsed":"0.0","totalNtlPos":"0.0","totalRawUsd":"1000.0"},"marginSummary":{"accountValue":"1127.3","totalMarginUsed":"127.5","totalNtlPos":"637.5","totalRawUsd":"1500.0"},"time":1795081500000,"withdrawable":"872.4"}"#,
        )
        .unwrap();
        let s = read_state(raw).unwrap();
        assert_eq!((s.account_value, s.margin_used, s.withdrawable), (1127.3, 127.5, 872.4));
        let p = &s.positions[0];
        assert_eq!((p.coin.as_str(), p.szi, p.entry, p.leverage, p.isolated), ("BTC", -0.01, Some(63768.5), 5, true));
        assert_eq!((p.liquidation, p.funding_since_open), (Some(51660.5), Some(0.3)));
    }

    #[test]
    fn an_entry_with_its_stop_and_target_reads_as_one_order_with_children() {
        let raw: Vec<RawOrder> = serde_json::from_str(
            r#"[{"children":[{"children":[],"coin":"ETH","isPositionTpsl":false,"isTrigger":true,"limitPx":"1700.0","oid":2,"orderType":"Stop Market","origSz":"0.2","reduceOnly":true,"side":"A","sz":"0.2","tif":null,"timestamp":1,"triggerCondition":"Price below 1750","triggerPx":"1750.0"}],"coin":"ETH","isPositionTpsl":false,"isTrigger":true,"limitPx":"1900.0","oid":1,"orderType":"Stop Market","origSz":"0.2","reduceOnly":false,"side":"B","sz":"0.2","tif":null,"timestamp":1,"triggerCondition":"Price above 1810","triggerPx":"1810.0"},
                {"children":[],"coin":"xyz:GOLD","isPositionTpsl":false,"isTrigger":false,"limitPx":"4000.0","oid":3,"orderType":"Limit","origSz":"0.5","reduceOnly":false,"side":"A","sz":"0.5","tif":"Gtc","timestamp":2,"triggerCondition":"N/A","triggerPx":"0.0"}]"#,
        )
        .unwrap();
        let orders: Vec<HlOpenOrder> = raw.into_iter().map(read_order).collect::<Result<_, _>>().unwrap();
        assert!(orders[0].is_trigger && orders[0].is_buy && !orders[0].reduce_only && orders[0].trigger_px == Some(1810.0));
        assert!(orders[0].children[0].is_stop() && orders[0].children[0].reduce_only);
        assert_eq!((orders[1].coin.as_str(), orders[1].is_trigger, orders[1].trigger_px), ("xyz:GOLD", false, None));
    }
}
