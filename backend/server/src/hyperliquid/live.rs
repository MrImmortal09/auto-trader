//! Hyperliquid LIVE, the owner's own account: its settings and its API
//! wallet, and — read-only in this release — what it holds on Hyperliquid.
//!
//! - **Mode** (`paper` / `live`) and **network** (`testnet` / `mainnet`) are
//!   the owner's switches in Settings, kept in `hl_live_settings`. Either
//!   changes only while the owner is flat, and LIVE needs the network's API
//!   wallet to have passed its check.
//! - **The API wallet** (one per network) is stored in `hl_credentials`: the
//!   account's address, the API wallet's address, and its private key sealed
//!   with AES-256-GCM under a key derived from `AUTH_SECRET` (read at run
//!   time, never in the binary). An API wallet can trade but never withdraw
//!   or transfer; the account's own key is refused.
//! - **The check** proves it end to end without trading: the key is the API
//!   wallet's, the account lists that wallet as approved, a signed `noop` is
//!   accepted by the exchange, and the account's balances are read.
//! - **The LIVE account** is Hyperliquid's: positions, open orders (stops and
//!   targets are its trigger orders) and fills read from the `info` endpoint
//!   and cached a few seconds, inside Hyperliquid's request limits. Nothing
//!   about it is kept here. A read that doesn't answer in a few seconds has
//!   failed, and a failure is answered again at once for a moment, so a
//!   stalled Hyperliquid never piles up waiting requests. It is read on the
//!   core perps and on each builder dex a perp on the app's list trades on
//!   (trade.xyz's for gold): each dex keeps its own USDC margin.
//! - **LIVE starts and ends flat**: entering or leaving it needs the
//!   Hyperliquid account to hold and rest nothing (on every dex, coins the
//!   app doesn't trade included), and entering it needs the owner's paper
//!   perps flat and the API wallet to pass a fresh check. One lock covers the
//!   switch and the API wallets' changes, and the owner's paper orders wait
//!   for it ([`LiveDesk::paper_gate`]), so nothing lands between a check and
//!   the change it allows.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use hyperliquid_client::account::{AccountState, HlFill, HlOpenOrder};
use hyperliquid_client::exchange::{Answer, ExchangeClient, Nonces};
use hyperliquid_client::signing::Wallet;
use hyperliquid_client::{InfoClient, Network};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tokio::sync::{Mutex, OwnedRwLockReadGuard, RwLock};

use super::paper::{AccountView, FillRow, PositionView, Refusal, Venue};
use super::perp::{PerpLevels, PerpLine, PerpOrder, PerpPosition, PerpTarget, Side};
use super::{markets, Market};
/// How long each part of the LIVE account is reused before it is read again.
/// An open Trade screen costs about 60 a minute per dex the account has
/// something on (its state every 2 s), 120 more for its open orders (every
/// 10 s), 4 per dex it has nothing on (every 30 s) and 20 to 40 for fills,
/// of the 1,200 request weight Hyperliquid allows an IP a minute: about 400
/// with money on the core perps and one builder dex.
const STATE_TTL: Duration = Duration::from_secs(2);
const ORDERS_TTL: Duration = Duration::from_secs(10);
const FILLS_TTL: Duration = Duration::from_secs(60);
/// A builder dex the account has nothing on (no money, position or order)
/// is read this seldom, and its open orders not at all: an order needs
/// margin on its own dex.
const IDLE_DEX_TTL: Duration = Duration::from_secs(30);
/// How long a network's list of dexes is reused before it is read again.
const DEXES_TTL: Duration = Duration::from_secs(10 * 60);
/// One read of the LIVE account that hasn't answered in this long has failed.
const ACCOUNT_READ_TIMEOUT: Duration = Duration::from_secs(4);
/// A failed read is answered again, at once, for this long: the requests
/// queued behind it don't each wait out their own timeout.
const FAILURE_TTL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HlMode {
    Paper,
    Live,
}

impl HlMode {
    fn parse(s: &str) -> Option<HlMode> {
        match s {
            "paper" => Some(HlMode::Paper),
            "live" => Some(HlMode::Live),
            _ => None,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            HlMode::Paper => "paper",
            HlMode::Live => "live",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveSettings {
    pub mode: HlMode,
    pub network: Network,
}

#[derive(Debug, Clone)]
struct Credential {
    account: String,
    agent: String,
    key_box: String,
    checked_at: Option<String>,
    check_ok: bool,
    check_note: Option<String>,
}

/// A stored API wallet, as Settings shows it — never its key.
#[derive(Debug, Clone, Serialize)]
pub struct CredentialView {
    pub account: String,
    pub agent: String,
    pub checked_at: Option<String>,
    pub ok: bool,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Credentials {
    pub testnet: Option<CredentialView>,
    pub mainnet: Option<CredentialView>,
}

/// `GET /api/v2/hl/live`.
#[derive(Debug, Clone, Serialize)]
pub struct LiveView {
    pub mode: HlMode,
    pub network: &'static str,
    pub credentials: Credentials,
    /// `AUTH_SECRET` is set, so a key can be sealed and stored.
    pub can_store_keys: bool,
    /// LIVE orders are not sent in this release.
    pub orders_enabled: bool,
}

/// The key a stored API wallet key is sealed under.
fn box_key(secret: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"auto-trader/hyperliquid-api-wallet/v1\0");
    h.update(secret.as_bytes());
    h.finalize().into()
}

fn seal(key: &[u8; 32], plain: &str) -> Result<String, Refusal> {
    let cipher = Aes256Gcm::new(key.into());
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let mut out = nonce.to_vec();
    out.extend(cipher.encrypt(&nonce, plain.as_bytes()).map_err(|_| Refusal::bad("could not seal the key"))?);
    Ok(base64::engine::general_purpose::STANDARD.encode(out))
}

fn unseal(key: &[u8; 32], boxed: &str) -> Result<String, Refusal> {
    let raw = base64::engine::general_purpose::STANDARD.decode(boxed).map_err(|_| Refusal::conflict("the stored key is unreadable — enter it again"))?;
    if raw.len() < 13 {
        return Err(Refusal::conflict("the stored key is unreadable — enter it again"));
    }
    let (nonce, ct) = raw.split_at(12);
    let plain = Aes256Gcm::new(key.into())
        .decrypt(Nonce::from_slice(nonce), ct)
        .map_err(|_| Refusal::conflict("the stored key can't be opened — AUTH_SECRET changed since it was saved; enter it again"))?;
    String::from_utf8(plain).map_err(|_| Refusal::conflict("the stored key is unreadable — enter it again"))
}

fn net_str(n: Network) -> &'static str {
    n.as_str()
}

fn is_address(s: &str) -> bool {
    s.len() == 42 && s.starts_with("0x") && s[2..].chars().all(|c| c.is_ascii_hexdigit())
}

fn ist(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.with_timezone(&shared_domain::ist_offset()).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

/// `read` within [`ACCOUNT_READ_TIMEOUT`], its failure in words.
async fn timed<T>(what: &str, read: impl Future<Output = Result<T, hyperliquid_client::HlError>>) -> Result<T, String> {
    match tokio::time::timeout(ACCOUNT_READ_TIMEOUT, read).await {
        Err(_) => Err(format!("couldn't read {what} from Hyperliquid: no answer in {} s", ACCOUNT_READ_TIMEOUT.as_secs())),
        Ok(Err(e)) => Err(format!("couldn't read {what} from Hyperliquid: {e}")),
        Ok(Ok(v)) => Ok(v),
    }
}

/// Each dex's account state: the core perps' (`None`) first.
type States = Vec<(Option<&'static str>, AccountState)>;

/// The dexes the LIVE account is read on: the core perps, and each builder
/// dex a perp on the app's list trades on.
fn live_dexes() -> Vec<Option<&'static str>> {
    let mut dexes: Vec<Option<&'static str>> = vec![None];
    for e in markets::entries() {
        if e.market.dex.is_some() && !dexes.contains(&e.market.dex) {
            dexes.push(e.market.dex);
        }
    }
    dexes
}

/// The [`live_dexes`] that a network lists (`have`, its `perpDexs` names):
/// one it doesn't have can hold nothing there, and isn't read (testnet has
/// `xyz` but not `para` or `io`, and answers 500 for them).
fn on_network(have: &[Option<String>]) -> Vec<Option<&'static str>> {
    live_dexes().into_iter().filter(|d| d.is_none() || have.iter().any(|h| h.as_deref() == *d)).collect()
}

/// The account has something on the dex: money, a position, or (as last
/// read) an order.
fn active(s: &DexState, orders: Option<&[HlOpenOrder]>) -> bool {
    s.dex.is_none()
        || s.state.account_value > 0.0
        || s.state.margin_used > 0.0
        || !s.state.positions.is_empty()
        || orders.is_some_and(|o| o.iter().any(|o| hyperliquid_client::dex_of(&o.coin) == s.dex))
}

/// A network's dexes (`perpDexs` names, the core perps' `None`), with when
/// they were read.
type NetDexes = (Instant, Vec<Option<String>>);

/// One dex's account state, with when it was read.
#[derive(Debug, Clone)]
struct DexState {
    dex: Option<&'static str>,
    at: Instant,
    state: AccountState,
}

/// "Core perps", "trade.xyz (xyz)".
fn venue_label(dex: Option<&str>) -> String {
    match dex {
        None => "Core perps".to_string(),
        Some(d) => format!("{} ({d})", markets::venue(Some(d))),
    }
}

/// What the LIVE account was last read as, per part — for one account: a
/// different one starts afresh.
#[derive(Default)]
struct LiveCache {
    account: String,
    /// The last read failed, and why: answered again until [`FAILURE_TTL`].
    failed: Option<(Instant, String)>,
    /// Each dex's state, in [`live_dexes`] order.
    states: Vec<DexState>,
    /// When a state was last read, in IST, for the screens' "as of".
    as_of: Option<String>,
    orders: Option<(Instant, Vec<HlOpenOrder>)>,
    fills: Option<(Instant, Vec<HlFill>)>,
}

pub struct LiveDesk {
    pool: SqlitePool,
    box_key: Option<[u8; 32]>,
    settings: RwLock<LiveSettings>,
    creds: RwLock<HashMap<Network, Credential>>,
    info: HashMap<Network, InfoClient>,
    exchange: HashMap<Network, ExchangeClient>,
    nonces: Nonces,
    cache: Mutex<HashMap<Network, LiveCache>>,
    /// Each network's dexes (`perpDexs` names), with when they were read.
    dexes: Mutex<HashMap<Network, NetDexes>>,
    /// Held exclusively by the mode switch and the API wallets' changes, and
    /// shared by the owner's paper orders ([`LiveDesk::paper_gate`]).
    gate: Arc<RwLock<()>>,
}

impl LiveDesk {
    /// The desk with the stored switches and API wallets; `secret` is
    /// `AUTH_SECRET`, without which no key can be stored or used.
    pub async fn load(pool: SqlitePool, secret: Option<&str>) -> Result<Self, sqlx::Error> {
        let row = sqlx::query_as::<_, (String, String)>("SELECT mode, network FROM hl_live_settings WHERE id = 1").fetch_optional(&pool).await?;
        let settings = match row {
            Some((mode, network)) => LiveSettings { mode: HlMode::parse(&mode).unwrap_or(HlMode::Paper), network: Network::parse(&network).unwrap_or(Network::Testnet) },
            // Testnet first: the owner's pick.
            None => LiveSettings { mode: HlMode::Paper, network: Network::Testnet },
        };
        let rows = sqlx::query_as::<_, (String, String, String, String, Option<String>, bool, Option<String>)>(
            "SELECT network, account, agent, key_box, checked_at, check_ok, check_note FROM hl_credentials",
        )
        .fetch_all(&pool)
        .await?;
        let mut creds = HashMap::new();
        for (network, account, agent, key_box, checked_at, check_ok, check_note) in rows {
            if let Some(n) = Network::parse(&network) {
                creds.insert(n, Credential { account, agent, key_box, checked_at, check_ok, check_note });
            }
        }
        let nets = [Network::Testnet, Network::Mainnet];
        Ok(LiveDesk {
            pool,
            box_key: secret.map(box_key),
            settings: RwLock::new(settings),
            creds: RwLock::new(creds),
            info: nets.iter().map(|n| (*n, InfoClient::new(*n))).collect(),
            exchange: nets.iter().map(|n| (*n, ExchangeClient::new(*n))).collect(),
            nonces: Nonces::default(),
            cache: Mutex::new(HashMap::new()),
            dexes: Mutex::new(HashMap::new()),
            gate: Arc::new(RwLock::new(())),
        })
    }

    pub async fn settings(&self) -> LiveSettings {
        *self.settings.read().await
    }

    /// Held by an owner's paper order from its "not LIVE" check until it is
    /// booked: the mode can't change in between.
    pub async fn paper_gate(&self) -> OwnedRwLockReadGuard<()> {
        Arc::clone(&self.gate).read_owned().await
    }

    /// The owner's network while Hyperliquid is LIVE.
    pub async fn live_network(&self) -> Option<Network> {
        let s = self.settings().await;
        (s.mode == HlMode::Live).then_some(s.network)
    }

    pub async fn view(&self) -> LiveView {
        let s = self.settings().await;
        let creds = self.creds.read().await;
        let show = |n: Network| {
            creds.get(&n).map(|c| CredentialView { account: c.account.clone(), agent: c.agent.clone(), checked_at: c.checked_at.clone(), ok: c.check_ok, note: c.check_note.clone() })
        };
        LiveView {
            mode: s.mode,
            network: net_str(s.network),
            credentials: Credentials { testnet: show(Network::Testnet), mainnet: show(Network::Mainnet) },
            can_store_keys: self.box_key.is_some(),
            orders_enabled: false,
        }
    }

    async fn save_settings(&self, s: LiveSettings) -> Result<(), Refusal> {
        sqlx::query(
            "INSERT INTO hl_live_settings (id, mode, network, updated_at) VALUES (1, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET mode = excluded.mode, network = excluded.network, updated_at = excluded.updated_at",
        )
        .bind(s.mode.as_str())
        .bind(net_str(s.network))
        .bind(shared_domain::current_ist_timestamp_string())
        .execute(&self.pool)
        .await
        .map_err(|e| Refusal::conflict(format!("could not save the Hyperliquid settings: {e}")))?;
        *self.settings.write().await = s;
        Ok(())
    }

    /// The [`live_dexes`] `network` has (its list kept [`DEXES_TTL`]).
    async fn dexes_on(&self, network: Network) -> Result<Vec<Option<&'static str>>, String> {
        let mut kept = self.dexes.lock().await;
        if let Some((_, have)) = kept.get(&network).filter(|(at, _)| at.elapsed() < DEXES_TTL) {
            return Ok(on_network(have));
        }
        let have: Vec<Option<String>> = timed("the dexes", self.info[&network].perp_dexes()).await?.into_iter().map(|d| d.name).collect();
        let out = on_network(&have);
        kept.insert(network, (Instant::now(), have));
        Ok(out)
    }

    /// What the account behind `network`'s API wallet holds and rests on
    /// Hyperliquid, every coin (the app's or not), read fresh: empty when it
    /// is flat. A read that fails can't say so.
    async fn live_open(&self, network: Network) -> Result<Vec<String>, Refusal> {
        let account = self.creds.read().await.get(&network).map(|c| c.account.clone()).ok_or_else(|| Refusal::conflict(format!("No {} API wallet", net_str(network))))?;
        let info = &self.info[&network];
        let read = async {
            let mut positions = Vec::new();
            let mut orders: Vec<HlOpenOrder> = Vec::new();
            // Every dex the network has must read flat: one that can't be
            // read can't say so.
            for dex in self.dexes_on(network).await? {
                positions.extend(timed("the account", info.account_state(&account, dex)).await?.positions);
                orders.extend(timed("the open orders", info.open_orders(&account, dex)).await?);
            }
            Ok::<_, String>(open_words(&positions, &orders))
        };
        read.await.map_err(|e| Refusal::conflict(format!("{e} — so it can't be seen to be flat; nothing changed")))
    }

    /// Change the mode and/or network: only while flat on both sides of the
    /// change. `paper_flat` says whether the owner's paper perps hold, rest
    /// and arm nothing; it is read once the switch is held, so no paper
    /// order lands between it and the change. LIVE also needs the network's
    /// API wallet to pass a check run now, and the Hyperliquid account to be
    /// flat (so it can be left again).
    pub async fn set(&self, mode: Option<HlMode>, network: Option<Network>, paper_flat: impl Future<Output = Result<bool, Refusal>>) -> Result<LiveView, Refusal> {
        let _switch = self.gate.write().await;
        let now = self.settings().await;
        let next = LiveSettings { mode: mode.unwrap_or(now.mode), network: network.unwrap_or(now.network) };
        if next == now {
            return Ok(self.view().await);
        }
        if now.mode == HlMode::Live {
            if next.network != now.network {
                return Err(Refusal::conflict("Switch Hyperliquid to Paper before changing the network"));
            }
            let open = self.live_open(now.network).await?;
            if !open.is_empty() {
                return Err(Refusal::conflict(format!("The LIVE {} account still holds {} — close and cancel them on Hyperliquid first", net_str(now.network), open.join(", "))));
            }
        }
        if next.mode == HlMode::Live {
            if !paper_flat.await? {
                return Err(Refusal::conflict("Your paper perps still hold, rest or arm something — close and cancel them first"));
            }
            if !self.creds.read().await.contains_key(&next.network) {
                return Err(Refusal::conflict(format!("Add a {} API wallet first", net_str(next.network))));
            }
            // A check of any age won't do: an approval can expire or be pruned.
            self.check_now(next.network).await?;
            let creds = self.creds.read().await;
            if let Some(c) = creds.get(&next.network).filter(|c| !c.check_ok) {
                return Err(Refusal::conflict(format!("The {} API wallet failed its check: {}", net_str(next.network), c.check_note.as_deref().unwrap_or("no reason given"))));
            }
            drop(creds);
            let open = self.live_open(next.network).await?;
            if !open.is_empty() {
                return Err(Refusal::conflict(format!(
                    "The {} Hyperliquid account holds {} — LIVE starts flat, so it can be left again: close and cancel them on Hyperliquid first",
                    net_str(next.network),
                    open.join(", ")
                )));
            }
        }
        self.save_settings(next).await?;
        tracing::warn!(mode = next.mode.as_str(), network = net_str(next.network), "Hyperliquid mode changed");
        Ok(self.view().await)
    }

    /// Store `network`'s API wallet: the account's address and the API
    /// wallet's private key (sealed), then check it.
    pub async fn save_credential(&self, network: Network, account: &str, key: &str) -> Result<LiveView, Refusal> {
        let _switch = self.gate.write().await;
        let Some(box_key) = self.box_key else {
            return Err(Refusal::conflict("AUTH_SECRET isn't set on the server, so no key can be stored"));
        };
        let s = self.settings().await;
        if s.mode == HlMode::Live && s.network == network {
            return Err(Refusal::conflict("Switch Hyperliquid to Paper before changing the LIVE API wallet"));
        }
        let account = account.trim().to_ascii_lowercase();
        if !is_address(&account) {
            return Err(Refusal::bad("The account must be a 0x address of 40 hex digits"));
        }
        let wallet = Wallet::from_hex(key).map_err(|e| Refusal::bad(e.to_string()))?;
        if wallet.address() == account {
            return Err(Refusal::bad("That is the account's own key, which can withdraw — create an API wallet on Hyperliquid (More → API) and use its key"));
        }
        let key_box = seal(&box_key, key.trim())?;
        let now = shared_domain::current_ist_timestamp_string();
        sqlx::query(
            "INSERT INTO hl_credentials (network, account, agent, key_box, checked_at, check_ok, check_note, updated_at) VALUES (?, ?, ?, ?, NULL, 0, NULL, ?)
             ON CONFLICT(network) DO UPDATE SET account = excluded.account, agent = excluded.agent, key_box = excluded.key_box,
               checked_at = NULL, check_ok = 0, check_note = NULL, updated_at = excluded.updated_at",
        )
        .bind(net_str(network))
        .bind(&account)
        .bind(wallet.address())
        .bind(&key_box)
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(|e| Refusal::conflict(format!("could not store the API wallet: {e}")))?;
        self.creds.write().await.insert(network, Credential { account, agent: wallet.address().to_string(), key_box, checked_at: None, check_ok: false, check_note: None });
        // Another account's: nothing read for the last one is shown for it.
        self.cache.lock().await.remove(&network);
        tracing::warn!(network = net_str(network), agent = wallet.address(), "Hyperliquid API wallet stored");
        self.check_now(network).await?;
        Ok(self.view().await)
    }

    /// Forget `network`'s API wallet; not while it is the LIVE one.
    pub async fn remove_credential(&self, network: Network) -> Result<LiveView, Refusal> {
        let _switch = self.gate.write().await;
        let s = self.settings().await;
        if s.mode == HlMode::Live && s.network == network {
            return Err(Refusal::conflict("Switch Hyperliquid to Paper before removing the LIVE API wallet"));
        }
        sqlx::query("DELETE FROM hl_credentials WHERE network = ?")
            .bind(net_str(network))
            .execute(&self.pool)
            .await
            .map_err(|e| Refusal::conflict(format!("could not remove the API wallet: {e}")))?;
        self.creds.write().await.remove(&network);
        self.cache.lock().await.remove(&network);
        Ok(self.view().await)
    }

    /// `network`'s API wallet, its key opened.
    async fn wallet(&self, network: Network) -> Result<(String, Wallet), Refusal> {
        let box_key = self.box_key.ok_or_else(|| Refusal::conflict("AUTH_SECRET isn't set on the server"))?;
        let c = self.creds.read().await.get(&network).cloned().ok_or_else(|| Refusal::conflict(format!("No {} API wallet", net_str(network))))?;
        let key = unseal(&box_key, &c.key_box)?;
        let wallet = Wallet::from_hex(&key).map_err(|e| Refusal::conflict(e.to_string()))?;
        if wallet.address() != c.agent {
            return Err(Refusal::conflict("the stored key isn't the stored API wallet's — enter it again"));
        }
        Ok((c.account, wallet))
    }

    /// Prove `network`'s API wallet end to end, trading nothing, and keep
    /// the result: approved by the account, accepted by the exchange (a
    /// signed `noop`), and the account's balances read.
    pub async fn check(&self, network: Network) -> Result<LiveView, Refusal> {
        let _switch = self.gate.write().await;
        self.check_now(network).await?;
        Ok(self.view().await)
    }

    /// [`LiveDesk::check`], with the switch already held.
    async fn check_now(&self, network: Network) -> Result<(), Refusal> {
        let (ok, note) = match self.wallet(network).await {
            Err(r) => (false, r.message),
            Ok((account, wallet)) => self.probe(network, &account, &wallet).await,
        };
        let now = shared_domain::current_ist_timestamp_string();
        sqlx::query("UPDATE hl_credentials SET checked_at = ?, check_ok = ?, check_note = ? WHERE network = ?")
            .bind(&now)
            .bind(ok)
            .bind(&note)
            .bind(net_str(network))
            .execute(&self.pool)
            .await
            .map_err(|e| Refusal::conflict(format!("could not save the check: {e}")))?;
        if let Some(c) = self.creds.write().await.get_mut(&network) {
            c.checked_at = Some(now);
            c.check_ok = ok;
            c.check_note = Some(note);
        }
        Ok(())
    }

    async fn probe(&self, network: Network, account: &str, wallet: &Wallet) -> (bool, String) {
        let info = &self.info[&network];
        match info.agents(account).await {
            Err(e) => return (false, format!("couldn't read the account's API wallets: {e}")),
            Ok(agents) => {
                let now = shared_domain::now_ist().timestamp_millis();
                let approved = agents.iter().find(|a| a.address.eq_ignore_ascii_case(wallet.address()));
                match approved {
                    None => return (false, format!("{account} hasn't approved API wallet {} on {} — approve it on Hyperliquid (More → API)", wallet.address(), net_str(network))),
                    Some(a) if a.valid_until.is_some_and(|until| until <= now) => return (false, "the API wallet's approval has expired — approve it again on Hyperliquid".to_string()),
                    Some(_) => {}
                }
            }
        }
        let nonce = self.nonces.next(shared_domain::now_ist().timestamp_millis().max(0) as u64);
        match self.exchange[&network].noop(wallet, nonce).await {
            Err(e) => return (false, format!("the exchange didn't answer a signed check ({e})")),
            Ok(Answer::Err(why)) => return (false, format!("the exchange refused a signed check: {why}")),
            Ok(Answer::Ok(_)) => {}
        }
        let states = match self.dexes_on(network).await {
            Ok(dexes) => read_states(info, account, &dexes).await,
            Err(e) => Err(e),
        };
        match states {
            Ok(states) => {
                let balances: Vec<String> = states.iter().map(|(dex, s)| format!("{} ${:.2}", venue_label(*dex), s.account_value)).collect();
                (true, format!("Approved and accepted. {}", balances.join(", ")))
            }
            Err(e) => (false, format!("approved and accepted, but {e}")),
        }
    }

    /// The owner's LIVE account on `network`, from Hyperliquid (cached a
    /// few seconds per part), in the paper desk's shape. Each read gives up
    /// after [`ACCOUNT_READ_TIMEOUT`]; a failure is answered again at once
    /// until [`FAILURE_TTL`], so the requests queued behind it don't wait.
    pub async fn account(&self, network: Network) -> Result<AccountView, Refusal> {
        let account = self.creds.read().await.get(&network).map(|c| c.account.clone()).ok_or_else(|| Refusal::conflict(format!("No {} API wallet", net_str(network))))?;
        let info = &self.info[&network];
        let mut all = self.cache.lock().await;
        let cache = all.entry(network).or_default();
        if cache.account != account {
            *cache = LiveCache { account: account.clone(), ..LiveCache::default() };
        }
        if let Some((at, why)) = &cache.failed {
            if at.elapsed() < FAILURE_TTL {
                return Err(Refusal::conflict(why.clone()));
            }
        }
        let read = match self.dexes_on(network).await {
            Ok(dexes) => read_into(cache, info, &account, &dexes).await,
            Err(e) => Err(e),
        };
        if let Err(why) = read {
            cache.failed = Some((Instant::now(), why.clone()));
            return Err(Refusal::conflict(why));
        }
        cache.failed = None;
        let states: States = cache.states.iter().map(|d| (d.dex, d.state.clone())).collect();
        let orders = &cache.orders.as_ref().expect("just read").1;
        let fills = cache.fills.as_ref().map(|(_, f)| f.as_slice()).unwrap_or_default();
        let mut view = live_account_view(network, &states, orders, fills);
        view.as_of = cache.as_of.clone();
        Ok(view)
    }
}

/// The account's state on each of `dexes` (the network's): every one must
/// be read.
async fn read_states(info: &InfoClient, account: &str, dexes: &[Option<&'static str>]) -> Result<States, String> {
    let mut out = Vec::new();
    for &dex in dexes {
        out.push((dex, timed("the LIVE account", info.account_state(account, dex)).await?));
    }
    Ok(out)
}

/// Read what `cache` holds too old a copy of, on `dexes` (the network's):
/// each part on its own TTL, a dex the account has nothing on seldom. A dex
/// that can't be read fails it all: nothing is shown without it.
async fn read_into(cache: &mut LiveCache, info: &InfoClient, account: &str, dexes: &[Option<&'static str>]) -> Result<(), String> {
    let kept_orders = cache.orders.as_ref().map(|(_, o)| o.clone());
    let mut read_any = false;
    for &dex in dexes {
        let kept = cache.states.iter().position(|d| d.dex == dex);
        let ttl = match kept {
            Some(i) if !active(&cache.states[i], kept_orders.as_deref()) => IDLE_DEX_TTL,
            _ => STATE_TTL,
        };
        if kept.is_none_or(|i| cache.states[i].at.elapsed() > ttl) {
            let state = timed("the LIVE account", info.account_state(account, dex)).await?;
            let fresh = DexState { dex, at: Instant::now(), state };
            match kept {
                Some(i) => cache.states[i] = fresh,
                None => cache.states.push(fresh),
            }
            read_any = true;
        }
    }
    cache.states.retain(|d| dexes.contains(&d.dex));
    cache.states.sort_by_key(|d| dexes.iter().position(|x| *x == d.dex));
    if read_any {
        cache.as_of = Some(shared_domain::current_ist_timestamp_string());
    }
    if cache.orders.as_ref().is_none_or(|(at, _)| at.elapsed() > ORDERS_TTL) {
        let mut orders = Vec::new();
        for d in cache.states.iter().filter(|d| active(d, kept_orders.as_deref())) {
            orders.extend(timed("the LIVE orders", info.open_orders(account, d.dex)).await?);
        }
        cache.orders = Some((Instant::now(), orders));
    }
    if cache.fills.as_ref().is_none_or(|(at, _)| at.elapsed() > FILLS_TTL) {
        // Fills are a nicety: a failed read keeps the last ones (this
        // account's: the cache starts afresh for another).
        match timed("the LIVE fills", info.user_fills(account)).await {
            Ok(f) => cache.fills = Some((Instant::now(), f)),
            Err(e) => tracing::warn!("Hyperliquid {e}"),
        }
    }
    Ok(())
}

/// "SOL long 3, BTC short 0.1, 2 open orders (ETH, xyz:GOLD)": what an
/// account holds and rests, every coin.
fn open_words(positions: &[hyperliquid_client::account::HlPosition], orders: &[HlOpenOrder]) -> Vec<String> {
    let mut out: Vec<String> = positions.iter().filter(|p| p.szi != 0.0).map(|p| format!("{} {} {}", p.coin, if p.szi > 0.0 { "long" } else { "short" }, p.szi.abs())).collect();
    if !orders.is_empty() {
        let mut coins: Vec<&str> = orders.iter().map(|o| o.coin.as_str()).collect();
        coins.sort_unstable();
        coins.dedup();
        out.push(format!("{} open order{} ({})", orders.len(), if orders.len() == 1 { "" } else { "s" }, coins.join(", ")));
    }
    out
}

/// A trigger order's stop or target, as a level.
fn levels_of(orders: &[&HlOpenOrder]) -> PerpLevels {
    let mut levels = PerpLevels::default();
    for o in orders {
        let Some(px) = o.trigger_px else { continue };
        if o.is_stop() {
            levels.stop.get_or_insert(px);
        } else {
            levels.targets.push(PerpTarget { id: o.oid.to_string(), price: px, size: (!o.is_position_tpsl).then_some(o.sz) });
        }
    }
    levels
}

/// Hyperliquid's account (the core perps' and each builder dex's) in the
/// paper desk's shape: positions with the stops and targets resting against
/// them, resting limits, stop entries (as line orders) with their children,
/// and the latest fills of the perps on the app's list.
pub fn live_account_view(network: Network, states: &[(Option<&'static str>, AccountState)], orders: &[HlOpenOrder], fills: &[HlFill]) -> AccountView {
    let ours = |coin: &str| Market::of_coin(coin);
    let mut positions = Vec::new();
    for p in states.iter().flat_map(|(_, s)| s.positions.iter()) {
        let Some(m) = ours(&p.coin) else { continue };
        let size = p.szi.abs();
        let side = if p.szi >= 0.0 { Side::Long } else { Side::Short };
        // Its stops and targets: reduce-only triggers on the coin, not an entry's children.
        let guards: Vec<&HlOpenOrder> = orders.iter().filter(|o| o.coin == p.coin && o.is_trigger && o.reduce_only).collect();
        let mark = (size > 0.0).then(|| p.position_value / size);
        positions.push(PositionView {
            position: PerpPosition {
                id: format!("{}:{}", net_str(network), m.key),
                market: m.key.to_string(),
                side,
                size,
                entry: p.entry.unwrap_or_default(),
                leverage: p.leverage,
                margin: p.margin_used,
                funding: 0.0,
                fees: 0.0,
                opened_at: String::new(),
                levels: levels_of(&guards),
            },
            symbol: m.symbol(),
            label: m.label,
            mark,
            upnl: Some(p.upnl),
            roe_pct: Some(p.roe * 100.0),
            liq: p.liquidation,
            notional: Some(p.position_value),
        });
    }
    let mut perp_orders = Vec::new();
    let mut lines = Vec::new();
    for o in orders {
        let Some(m) = ours(&o.coin) else { continue };
        let side = if o.is_buy { Side::Long } else { Side::Short };
        let children: Vec<&HlOpenOrder> = o.children.iter().collect();
        if !o.is_trigger {
            perp_orders.push(PerpOrder {
                id: o.oid.to_string(),
                market: m.key.to_string(),
                side,
                size: o.sz,
                price: o.limit_px,
                leverage: 0,
                reduce_only: o.reduce_only,
                placed_at: ist(o.timestamp),
                levels: levels_of(&children),
            });
        } else if !o.reduce_only {
            // A stop entry: a market order once the mark crosses it.
            lines.push(PerpLine {
                id: o.oid.to_string(),
                market: m.key.to_string(),
                side,
                size: o.sz,
                leverage: 0,
                price: o.trigger_px.unwrap_or(o.limit_px),
                rising: o.trigger_condition.to_ascii_lowercase().contains("above"),
                valid_until: None,
                placed_at: ist(o.timestamp),
                levels: levels_of(&children),
            });
        }
    }
    let mut ours_fills: Vec<&HlFill> = fills.iter().filter(|f| ours(&f.coin).is_some()).collect();
    ours_fills.sort_by_key(|f| std::cmp::Reverse(f.time));
    let fills = ours_fills
        .into_iter()
        .take(50)
        .map(|f| FillRow {
            id: f.tid as i64,
            position_id: String::new(),
            market: ours(&f.coin).map_or_else(|| f.coin.clone(), |m| m.key.to_string()),
            action: if f.is_buy { "BUY".into() } else { "SELL".into() },
            size: f.sz,
            price: f.px,
            fee: f.fee,
            realized: f.closed_pnl,
            funding: 0.0,
            reason: f.dir.clone(),
            timestamp: ist(f.time),
        })
        .collect();
    let margin: f64 = states.iter().map(|(_, s)| s.margin_used).sum();
    let upnl: f64 = positions.iter().filter_map(|p| p.upnl).sum();
    let equity: f64 = states.iter().map(|(_, s)| s.account_value).sum();
    AccountView {
        mode: "live",
        start: equity,
        cash: states.iter().map(|(_, s)| s.withdrawable).sum(),
        margin,
        upnl,
        equity,
        epoch: 0,
        positions,
        orders: perp_orders,
        lines,
        activity: Vec::new(),
        fills,
        network: Some(net_str(network)),
        venues: states
            .iter()
            .map(|(dex, s)| Venue { dex: dex.unwrap_or("core"), label: venue_label(*dex), account_value: s.account_value, withdrawable: s.withdrawable, margin_used: s.margin_used })
            .collect(),
        as_of: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyperliquid_client::account::HlPosition;

    #[test]
    fn a_key_is_sealed_and_opens_only_under_the_same_secret() {
        let k = box_key("secret-one");
        let boxed = seal(&k, "0xabc").unwrap();
        assert!(!boxed.contains("abc"));
        assert_eq!(unseal(&k, &boxed).unwrap(), "0xabc");
        assert!(unseal(&box_key("secret-two"), &boxed).is_err(), "another AUTH_SECRET can't open it");
        assert_ne!(seal(&k, "0xabc").unwrap(), boxed, "a fresh nonce each time");
    }

    fn state(value: f64, positions: Vec<HlPosition>) -> AccountState {
        AccountState { account_value: value, margin_used: positions.iter().map(|p| p.margin_used).sum(), withdrawable: value / 2.0, positions }
    }

    fn pos(coin: &str, szi: f64) -> HlPosition {
        HlPosition { coin: coin.into(), szi, entry: Some(100.0), leverage: 5, isolated: true, liquidation: Some(80.0), margin_used: 20.0, position_value: 101.0 * szi.abs(), upnl: 1.0 * szi, roe: 0.05, funding_since_open: None }
    }

    fn order(coin: &str, oid: u64, is_buy: bool, trigger: Option<(f64, &str)>, reduce_only: bool) -> HlOpenOrder {
        HlOpenOrder {
            coin: coin.into(),
            oid,
            is_buy,
            limit_px: 99.0,
            sz: 1.0,
            orig_sz: 1.0,
            reduce_only,
            is_trigger: trigger.is_some(),
            trigger_px: trigger.map(|t| t.0),
            is_position_tpsl: false,
            order_type: trigger.map_or("Limit", |t| t.1).to_string(),
            trigger_condition: if trigger.is_some() { "Price above 105".into() } else { "N/A".into() },
            timestamp: 0,
            children: Vec::new(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_read_that_hangs_fails_after_the_timeout() {
        let hang = async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok::<_, hyperliquid_client::HlError>(1)
        };
        let started = tokio::time::Instant::now();
        let err = timed("the LIVE account", hang).await.unwrap_err();
        assert_eq!(started.elapsed(), ACCOUNT_READ_TIMEOUT, "given up at the timeout, not after the read's own 15 s");
        assert!(err.contains("no answer in 4 s"), "{err}");
        assert_eq!(timed("x", async { Ok::<_, hyperliquid_client::HlError>(7) }).await, Ok(7));
    }

    #[test]
    fn only_the_dexes_a_network_lists_are_read() {
        // On the app's list: BTC, ETH (core) and gold (xyz).
        assert_eq!(on_network(&[None, Some("xyz".into()), Some("flx".into())]), [None, Some("xyz")]);
        assert_eq!(on_network(&[None, Some("para".into())]), [None], "a network without xyz: core only");
    }

    #[test]
    fn a_dex_with_nothing_on_it_is_idle() {
        let d = |dex: Option<&'static str>, value: f64, positions: Vec<HlPosition>| DexState { dex, at: Instant::now(), state: state(value, positions) };
        assert!(active(&d(None, 0.0, vec![]), None), "the core perps always");
        assert!(!active(&d(Some("xyz"), 0.0, vec![]), None));
        assert!(active(&d(Some("xyz"), 12.0, vec![]), None), "money on it");
        assert!(active(&d(Some("xyz"), 0.0, vec![pos("xyz:GOLD", 1.0)]), None));
        assert!(active(&d(Some("xyz"), 0.0, vec![]), Some(&[order("xyz:GOLD", 1, true, None, false)])), "an order seen on it");
        assert!(!active(&d(Some("xyz"), 0.0, vec![]), Some(&[order("BTC", 1, true, None, false)])));
    }

    #[test]
    fn what_is_open_is_named_coin_by_coin() {
        assert!(open_words(&[], &[]).is_empty(), "flat");
        let w = open_words(&[pos("SOL", 3.0), pos("BTC", -0.1), pos("ETH", 0.0)], &[order("ETH", 1, true, None, false), order("xyz:GOLD", 2, false, None, false), order("ETH", 3, true, None, false)]);
        assert_eq!(w, ["SOL long 3", "BTC short 0.1", "3 open orders (ETH, xyz:GOLD)"]);
    }

    #[test]
    fn the_live_account_reads_as_the_paper_desks_shape() {
        let core = state(1_000.0, vec![pos("BTC", -2.0), pos("SOL", 3.0)]);
        let gold = state(500.0, vec![pos("xyz:GOLD", 1.0)]);
        let orders = vec![
            order("BTC", 1, true, Some((110.0, "Stop Market")), true),
            order("BTC", 2, true, Some((90.0, "Take Profit Market")), true),
            order("ETH", 3, true, None, false),
            order("ETH", 4, true, Some((105.0, "Stop Market")), false),
        ];
        let v = live_account_view(Network::Testnet, &[(None, core), (Some("xyz"), gold)], &orders, &[]);
        assert_eq!((v.mode, v.network, v.equity), ("live", Some("testnet"), 1_500.0));
        // SOL is no perp of the app's.
        assert_eq!(v.positions.iter().map(|p| p.position.market.as_str()).collect::<Vec<_>>(), ["BTC", "GOLD"]);
        let btc = &v.positions[0];
        assert_eq!((btc.position.side, btc.position.size), (Side::Short, 2.0));
        assert_eq!(btc.position.levels.stop, Some(110.0));
        assert_eq!(btc.position.levels.targets.iter().map(|t| t.price).collect::<Vec<_>>(), [90.0]);
        assert_eq!((v.orders.len(), v.lines.len()), (1, 1), "a resting limit, and a stop entry as a line order");
        assert!(v.lines[0].rising);
        assert_eq!(v.venues.len(), 2);
    }
}
