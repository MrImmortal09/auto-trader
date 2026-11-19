//! Signing Hyperliquid's exchange actions with an API wallet.
//!
//! An L1 action (an order, a cancel, a modify, a leverage change) is signed
//! the way Hyperliquid's own SDKs do it: the action is msgpack-encoded with
//! its keys in a fixed order, the nonce (8 bytes, big-endian) and a "no
//! vault" byte are appended, and the keccak hash of that becomes the
//! `connectionId` of an EIP-712 `Agent` message — `source` `a` on mainnet,
//! `b` on testnet — under the `Exchange` domain (chain id 1337). The
//! signature is secp256k1 with RFC 6979 nonces and a low `s`, as Ethereum's
//! wallets make it. `tests/sdk_vectors.json` pins every byte of it against
//! the official Python SDK.
//!
//! An API wallet can trade but never withdraw or transfer: whatever key is
//! given here can move positions, not money.

use k256::ecdsa::SigningKey;
use serde::Serialize;
use sha3::{Digest, Keccak256};

use crate::{HlError, Network};

pub fn keccak(data: &[u8]) -> [u8; 32] {
    Keccak256::digest(data).into()
}

/// An API wallet's key, and the address it signs as.
#[derive(Clone)]
pub struct Wallet {
    key: SigningKey,
    address: String,
}

impl std::fmt::Debug for Wallet {
    // Never the key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wallet").field("address", &self.address).finish_non_exhaustive()
    }
}

impl Wallet {
    /// The wallet behind a private key given as 64 hex digits (`0x` optional).
    pub fn from_hex(key: &str) -> Result<Wallet, HlError> {
        let hex_key = key.trim().trim_start_matches("0x");
        let bytes = hex::decode(hex_key).map_err(|_| HlError::Key("the private key must be 64 hex digits".into()))?;
        if bytes.len() != 32 {
            return Err(HlError::Key("the private key must be 64 hex digits".into()));
        }
        let key = SigningKey::from_slice(&bytes).map_err(|_| HlError::Key("not a valid secp256k1 private key".into()))?;
        let point = key.verifying_key().to_encoded_point(false);
        // The last 20 bytes of the keccak of the uncompressed point, without its 0x04.
        let hash = keccak(&point.as_bytes()[1..]);
        let address = format!("0x{}", hex::encode(&hash[12..]));
        Ok(Wallet { key, address })
    }

    /// Lowercase `0x…` address.
    pub fn address(&self) -> &str {
        &self.address
    }
}

/// An ECDSA signature as Hyperliquid takes it: `r` and `s` as `0x` hex
/// without leading zeros, `v` 27 or 28.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Signature {
    pub r: String,
    pub s: String,
    pub v: u8,
}

fn trimmed_hex(bytes: &[u8]) -> String {
    let h = hex::encode(bytes);
    let t = h.trim_start_matches('0');
    format!("0x{}", if t.is_empty() { "0" } else { t })
}

/// The hash an L1 action is signed over: msgpack, nonce, no vault.
pub fn action_hash<T: Serialize>(action: &T, nonce: u64) -> Result<[u8; 32], HlError> {
    let mut data = rmp_serde::to_vec_named(action).map_err(|e| HlError::Encode(e.to_string()))?;
    data.extend_from_slice(&nonce.to_be_bytes());
    data.push(0);
    Ok(keccak(&data))
}

/// The EIP-712 digest of the `Agent` message carrying `connection_id`.
fn agent_digest(connection_id: &[u8; 32], network: Network) -> [u8; 32] {
    let domain_type = keccak(b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");
    let mut chain_id = [0u8; 32];
    chain_id[30..].copy_from_slice(&1337u16.to_be_bytes());
    let mut domain = Vec::with_capacity(32 * 5);
    domain.extend_from_slice(&domain_type);
    domain.extend_from_slice(&keccak(b"Exchange"));
    domain.extend_from_slice(&keccak(b"1"));
    domain.extend_from_slice(&chain_id);
    domain.extend_from_slice(&[0u8; 32]);
    let domain_separator = keccak(&domain);

    let agent_type = keccak(b"Agent(string source,bytes32 connectionId)");
    let source: &[u8] = match network {
        Network::Mainnet => b"a",
        Network::Testnet => b"b",
    };
    let mut message = Vec::with_capacity(32 * 3);
    message.extend_from_slice(&agent_type);
    message.extend_from_slice(&keccak(source));
    message.extend_from_slice(connection_id);
    let struct_hash = keccak(&message);

    let mut digest = Vec::with_capacity(66);
    digest.extend_from_slice(&[0x19, 0x01]);
    digest.extend_from_slice(&domain_separator);
    digest.extend_from_slice(&struct_hash);
    keccak(&digest)
}

/// Sign `action` with `nonce` (Unix ms, rising) for `network`.
pub fn sign_l1<T: Serialize>(wallet: &Wallet, action: &T, nonce: u64, network: Network) -> Result<Signature, HlError> {
    let hash = action_hash(action, nonce)?;
    let digest = agent_digest(&hash, network);
    let (sig, recid) = wallet.key.sign_prehash_recoverable(&digest).map_err(|e| HlError::Key(format!("signing failed: {e}")))?;
    let (r, s) = sig.split_bytes();
    Ok(Signature { r: trimmed_hex(&r), s: trimmed_hex(&s), v: 27 + recid.to_byte() })
}

/// A price or size as Hyperliquid's wire takes it: at most eight decimals,
/// no trailing zeros (`63768.5`, `1900`, `0.00001`). `None` when eight
/// decimals would change it.
pub fn float_to_wire(x: f64) -> Option<String> {
    if !x.is_finite() {
        return None;
    }
    let fixed = format!("{x:.8}");
    let back: f64 = fixed.parse().ok()?;
    if (back - x).abs() >= 1e-12 {
        return None;
    }
    let mut s = fixed.trim_end_matches('0').trim_end_matches('.').to_string();
    if s == "-0" || s.is_empty() {
        s = "0".to_string();
    }
    Some(s)
}

// ── The actions, keys in Hyperliquid's order ──────────────────────────── //

/// A limit order's time in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Tif {
    /// Rests until filled or cancelled.
    Gtc,
    /// Fills what it can now; the rest is cancelled (a "market" order).
    Ioc,
    /// Post only: refused if it would fill at once.
    Alo,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LimitWire {
    pub tif: Tif,
}

/// A trigger: once the mark reaches `trigger_px`, a market order (or a
/// limit at the order's price) goes out. `tpsl` is `tp` or `sl`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TriggerWire {
    #[serde(rename = "isMarket")]
    pub is_market: bool,
    #[serde(rename = "triggerPx")]
    pub trigger_px: String,
    pub tpsl: &'static str,
}

/// `{"limit": …}` or `{"trigger": …}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OrderTypeWire {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<LimitWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<TriggerWire>,
}

impl OrderTypeWire {
    pub fn limit(tif: Tif) -> Self {
        OrderTypeWire { limit: Some(LimitWire { tif }), trigger: None }
    }
    pub fn trigger(is_market: bool, trigger_px: String, tpsl: &'static str) -> Self {
        OrderTypeWire { limit: None, trigger: Some(TriggerWire { is_market, trigger_px, tpsl }) }
    }
}

/// One order: asset, buy, price, size, reduce only, type, client id.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OrderWire {
    pub a: u32,
    pub b: bool,
    pub p: String,
    pub s: String,
    pub r: bool,
    pub t: OrderTypeWire,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub c: Option<String>,
}

/// How orders sent together relate: `na`, `normalTpsl` (the first is an
/// entry, the rest its stop and target, live once it fills) or
/// `positionTpsl` (a stop or target on the whole position).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Grouping {
    Na,
    NormalTpsl,
    PositionTpsl,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OrderAction {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub orders: Vec<OrderWire>,
    pub grouping: Grouping,
}

impl OrderAction {
    pub fn new(orders: Vec<OrderWire>, grouping: Grouping) -> Self {
        OrderAction { kind: "order", orders, grouping }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CancelWire {
    pub a: u32,
    pub o: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CancelAction {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub cancels: Vec<CancelWire>,
}

impl CancelAction {
    pub fn new(cancels: Vec<CancelWire>) -> Self {
        CancelAction { kind: "cancel", cancels }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModifyWire {
    pub oid: u64,
    pub order: OrderWire,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModifyAction {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub modifies: Vec<ModifyWire>,
}

impl ModifyAction {
    pub fn new(modifies: Vec<ModifyWire>) -> Self {
        ModifyAction { kind: "batchModify", modifies }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LeverageAction {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub asset: u32,
    #[serde(rename = "isCross")]
    pub is_cross: bool,
    pub leverage: u32,
}

impl LeverageAction {
    /// Isolated margin at `leverage` on `asset`.
    pub fn isolated(asset: u32, leverage: u32) -> Self {
        LeverageAction { kind: "updateLeverage", asset, is_cross: false, leverage }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wallet_knows_its_address_and_never_prints_its_key() {
        let w = Wallet::from_hex("0x0123456789012345678901234567890123456789012345678901234567890123").unwrap();
        assert_eq!(w.address(), "0x14791697260e4c9a71f18484c9f997b308e59325");
        assert!(!format!("{w:?}").contains("0123456789"));
        assert!(Wallet::from_hex("0x12").is_err());
        assert!(Wallet::from_hex(&"0".repeat(64)).is_err(), "zero is no key");
    }

    #[test]
    fn floats_go_on_the_wire_as_the_sdk_writes_them() {
        assert_eq!(float_to_wire(63768.5).as_deref(), Some("63768.5"));
        assert_eq!(float_to_wire(1900.0).as_deref(), Some("1900"));
        assert_eq!(float_to_wire(0.00001).as_deref(), Some("0.00001"));
        assert_eq!(float_to_wire(3715.10).as_deref(), Some("3715.1"));
        assert_eq!(float_to_wire(0.0).as_deref(), Some("0"));
        assert_eq!(float_to_wire(0.123456789), None, "would round");
    }
}
