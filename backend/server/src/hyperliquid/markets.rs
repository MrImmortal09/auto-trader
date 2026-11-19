//! The perps the app charts and trades: BTC, ETH and trade.xyz's gold to
//! start with, and any other Hyperliquid perp the admin adds from Admin →
//! Hyperliquid markets ([`MarketAdmin`]).
//!
//! An added perp is charted, found by search and traded by everyone signed
//! in (the owner and every user's paper desk). A removed one is close-only
//! (the owner's choice, 2026-11-20): it leaves search and the pickers, an
//! order that would open or add to a position is refused and a resting one
//! is cancelled, but a position still held in it keeps its price, stop,
//! targets and liquidation until it is closed.
//!
//! The list lives in `hl_markets` and, for the hot paths (every watch tick
//! looks perps up), in a process-wide registry. Each perp's [`Market`] is
//! made once per set of rules and never freed (`&'static`): Hyperliquid
//! lists a few hundred perps, so there are at most that many, plus one each
//! time Hyperliquid changes a perp's rules.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex as StdMutex, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

use hyperliquid_client::{dex_of, InfoClient, Network, PerpListing, PerpRules};
use sqlx::SqlitePool;
use tokio::sync::Mutex;

use super::paper::Refusal;
use super::Market;

/// trade.xyz's gold, which the app has called `GOLD` from the start.
const GOLD_COIN: &str = "xyz:GOLD";
/// How long Hyperliquid's full list is kept for the admin's page.
const LISTINGS_TTL: Duration = Duration::from_secs(5 * 60);
/// How often the rules of the perps on the list are read again.
pub const REFRESH_EVERY: Duration = Duration::from_secs(15 * 60);
/// Most perps the Trade header shows.
pub const MAX_HEADER: usize = 8;
/// Most perps one change adds or removes.
pub const MAX_CHANGE: usize = 400;

/// The three the app started with, and has on a fresh database.
pub static BUILTIN: [Market; 3] = [
    Market { key: "BTC", coin: "BTC", base: "BTC", dex: None, label: "BTC-PERP", name: "Bitcoin", sz_decimals: 5, max_leverage: 40, taker_fee: 0.00045, maker_fee: 0.00015 },
    Market { key: "ETH", coin: "ETH", base: "ETH", dex: None, label: "ETH-PERP", name: "Ether", sz_decimals: 4, max_leverage: 25, taker_fee: 0.00045, maker_fee: 0.00015 },
    Market { key: "GOLD", coin: GOLD_COIN, base: "GOLD", dex: Some("xyz"), label: "GOLD-PERP", name: "Gold (trade.xyz)", sz_decimals: 4, max_leverage: 25, taker_fee: 0.0009, maker_fee: 0.0003 },
];

/// A perp on the list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Entry {
    pub market: &'static Market,
    /// Added: charted, searched and traded. Not: close-only.
    pub added: bool,
    /// Shown in the Trade header (an added perp only).
    pub header: bool,
}

struct Registry {
    entries: Vec<Entry>,
    /// Lowercased key → place in `entries`.
    by_key: HashMap<String, usize>,
    by_coin: HashMap<&'static str, usize>,
    /// Bumped on every change, so a screen knows to read the list again.
    rev: u64,
}

impl Registry {
    fn new(entries: Vec<Entry>, rev: u64) -> Self {
        let mut r = Registry { entries: Vec::new(), by_key: HashMap::new(), by_coin: HashMap::new(), rev };
        for e in entries {
            r.upsert(e);
        }
        r
    }

    fn upsert(&mut self, e: Entry) {
        match self.by_coin.get(e.market.coin) {
            Some(&i) => {
                self.by_key.remove(&self.entries[i].market.key.to_ascii_lowercase());
                self.entries[i] = e;
                self.by_key.insert(e.market.key.to_ascii_lowercase(), i);
            }
            None => {
                let i = self.entries.len();
                self.entries.push(e);
                self.by_key.insert(e.market.key.to_ascii_lowercase(), i);
                self.by_coin.insert(e.market.coin, i);
            }
        }
    }
}

fn builtin_entries() -> Vec<Entry> {
    BUILTIN.iter().map(|m| Entry { market: m, added: true, header: true }).collect()
}

static REGISTRY: LazyLock<RwLock<Registry>> = LazyLock::new(|| RwLock::new(Registry::new(builtin_entries(), 1)));

fn read() -> RwLockReadGuard<'static, Registry> {
    REGISTRY.read().unwrap_or_else(|e| e.into_inner())
}

fn write(change: impl FnOnce(&mut Registry)) {
    let mut r = REGISTRY.write().unwrap_or_else(|e| e.into_inner());
    change(&mut r);
    r.rev += 1;
}

/// The perp the app calls `key` (any case), added or close-only.
pub fn by_key(key: &str) -> Option<Entry> {
    let r = read();
    r.by_key.get(&key.to_ascii_lowercase()).map(|&i| r.entries[i])
}

/// The perp Hyperliquid calls `coin`, added or close-only.
pub fn by_coin(coin: &str) -> Option<Entry> {
    let r = read();
    r.by_coin.get(coin).map(|&i| r.entries[i])
}

/// Every perp on the list, in the order added.
pub fn entries() -> Vec<Entry> {
    read().entries.clone()
}

/// The list's revision: it changes whenever the list does.
pub fn rev() -> u64 {
    read().rev
}

/// Whether `key` is added (not close-only).
pub fn tradable(key: &str) -> bool {
    by_key(key).is_some_and(|e| e.added)
}

/// Strings made once and kept for the life of the process.
fn intern(s: &str) -> &'static str {
    static SEEN: LazyLock<StdMutex<HashSet<&'static str>>> = LazyLock::new(|| StdMutex::new(HashSet::new()));
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&s) = seen.get(s) {
        return s;
    }
    let s: &'static str = Box::leak(s.to_string().into_boxed_str());
    seen.insert(s);
    s
}

/// What the app calls Hyperliquid's `coin`: the coin itself, but `GOLD` for
/// trade.xyz's gold.
pub fn key_for(coin: &str) -> &str {
    if coin == GOLD_COIN {
        "GOLD"
    } else {
        coin
    }
}

/// The venue a dex is: `Hyperliquid` for the core perps, `trade.xyz` for
/// `xyz`, the builder's own name for the rest.
pub fn venue(dex: Option<&str>) -> &'static str {
    match dex {
        None => "Hyperliquid",
        Some("xyz") => "trade.xyz",
        Some("para") => "Paragon",
        Some("io") => "EntropyIO",
        Some("km") | Some("mkts") => "Markets by Kinetiq",
        Some("flx") => "Felix",
        Some("vntl") => "Ventuals",
        Some("hyna") => "HyENA",
        Some("cash") => "dreamcash",
        Some("abcd") => "ABCDEx",
        Some(other) => intern(other),
    }
}

/// Hyperliquid's rules for a perp, as the app keeps them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rules {
    pub sz_decimals: u32,
    pub max_leverage: u32,
    pub taker_fee: f64,
    pub maker_fee: f64,
}

impl Rules {
    pub fn of(r: &PerpRules) -> Self {
        let (taker_fee, maker_fee) = r.fees();
        Rules { sz_decimals: r.sz_decimals, max_leverage: r.max_leverage, taker_fee, maker_fee }
    }

    fn of_market(m: &Market) -> Self {
        Rules { sz_decimals: m.sz_decimals, max_leverage: m.max_leverage, taker_fee: m.taker_fee, maker_fee: m.maker_fee }
    }
}

/// `coin`'s market with `rules`: one of the three built in, the one already
/// on the list when nothing changed, else a new one, kept for good.
pub fn market_of(coin: &str, rules: Rules) -> &'static Market {
    if let Some(m) = BUILTIN.iter().find(|m| m.coin == coin && Rules::of_market(m) == rules) {
        return m;
    }
    if let Some(e) = by_coin(coin).filter(|e| Rules::of_market(e.market) == rules) {
        return e.market;
    }
    let dex = dex_of(coin);
    let base = coin.split_once(':').map_or(coin, |(_, b)| b);
    let builtin = BUILTIN.iter().find(|m| m.coin == coin);
    let label = match dex {
        None | Some("xyz") => format!("{base}-PERP"),
        Some(d) => format!("{base}-PERP ({d})"),
    };
    let name = match (builtin, dex) {
        (Some(b), _) => b.name.to_string(),
        (None, None) => base.to_string(),
        (None, Some(d)) => format!("{base} ({})", venue(Some(d))),
    };
    Box::leak(Box::new(Market {
        key: intern(key_for(coin)),
        coin: intern(coin),
        base: intern(base),
        dex: dex.map(intern),
        label: intern(&label),
        name: intern(&name),
        sz_decimals: rules.sz_decimals,
        max_leverage: rules.max_leverage,
        taker_fee: rules.taker_fee,
        maker_fee: rules.maker_fee,
    }))
}

type Row = (String, i64, i64, i64, i64, f64, f64);

/// Read the list from `hl_markets` into the registry; an empty table gets
/// the three built in.
pub async fn load(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let mut rows: Vec<Row> = sqlx::query_as("SELECT coin, added, header, sz_decimals, max_leverage, taker_fee, maker_fee FROM hl_markets ORDER BY position, coin")
        .fetch_all(pool)
        .await?;
    if rows.is_empty() {
        let now = shared_domain::current_ist_timestamp_string();
        for (i, m) in BUILTIN.iter().enumerate() {
            sqlx::query("INSERT OR IGNORE INTO hl_markets (coin, added, header, sz_decimals, max_leverage, taker_fee, maker_fee, position, updated_at) VALUES (?, 1, 1, ?, ?, ?, ?, ?, ?)")
                .bind(m.coin)
                .bind(m.sz_decimals as i64)
                .bind(m.max_leverage as i64)
                .bind(m.taker_fee)
                .bind(m.maker_fee)
                .bind(i as i64)
                .bind(&now)
                .execute(pool)
                .await?;
        }
        rows = BUILTIN.iter().map(|m| (m.coin.to_string(), 1, 1, m.sz_decimals as i64, m.max_leverage as i64, m.taker_fee, m.maker_fee)).collect();
    }
    let entries: Vec<Entry> = rows
        .into_iter()
        .filter(|r| r.3 >= 0 && r.4 > 0)
        .map(|(coin, added, header, sz, lev, taker, maker)| {
            let market = market_of(&coin, Rules { sz_decimals: sz as u32, max_leverage: lev as u32, taker_fee: taker, maker_fee: maker });
            Entry { market, added: added != 0, header: added != 0 && header != 0 }
        })
        .collect();
    let added = entries.iter().filter(|e| e.added).count();
    write(|r| *r = Registry::new(entries, r.rev));
    tracing::info!("Hyperliquid markets: {added} added");
    Ok(())
}

/// One dex as Hyperliquid lists it.
#[derive(Debug, Clone)]
pub struct DexListing {
    /// `None`: the core perps.
    pub dex: Option<String>,
    pub venue: String,
    pub perps: Vec<PerpListing>,
}

/// A change to the list, as the admin asked it.
#[derive(Debug, Clone, Default)]
pub struct Change {
    pub add: Vec<String>,
    pub remove: Vec<String>,
    pub header: Vec<(String, bool)>,
}

/// The admin's side of the list: Hyperliquid's every perp, and adding and
/// removing them.
pub struct MarketAdmin {
    pool: SqlitePool,
    info: InfoClient,
    listings: Mutex<Option<(Instant, Arc<Vec<DexListing>>)>>,
    /// One change (or rules refresh) at a time.
    changing: Mutex<()>,
}

impl MarketAdmin {
    pub fn new(pool: SqlitePool) -> Self {
        MarketAdmin { pool, info: InfoClient::new(Network::Mainnet), listings: Mutex::new(None), changing: Mutex::new(()) }
    }

    /// Every live perp Hyperliquid lists, by dex (core first), read again
    /// once [`LISTINGS_TTL`] old. A builder dex that can't be read is left
    /// out; the core perps that can't, an error.
    pub async fn listings(&self) -> Result<Arc<Vec<DexListing>>, Refusal> {
        let mut kept = self.listings.lock().await;
        if let Some((at, l)) = kept.as_ref().filter(|(at, _)| at.elapsed() < LISTINGS_TTL) {
            let _ = at;
            return Ok(Arc::clone(l));
        }
        let unreachable = |e: hyperliquid_client::HlError| Refusal { status: axum::http::StatusCode::BAD_GATEWAY, message: format!("Couldn't read Hyperliquid's markets: {e}") };
        let dexes = self.info.perp_dexes().await.map_err(unreachable)?;
        let names: Vec<Option<String>> = dexes.into_iter().map(|d| d.name).collect();
        let reads = futures::future::join_all(names.iter().map(|d| self.info.perp_listings(d.as_deref()))).await;
        let mut out = Vec::new();
        for (dex, read) in names.into_iter().zip(reads) {
            match read {
                Ok(perps) if perps.is_empty() => {}
                Ok(perps) => out.push(DexListing { venue: venue(dex.as_deref()).to_string(), dex, perps }),
                Err(e) if dex.is_none() => return Err(unreachable(e)),
                Err(e) => tracing::warn!("Hyperliquid markets: dex {} not read: {e}", dex.as_deref().unwrap_or("core")),
            }
        }
        let out = Arc::new(out);
        *kept = Some((Instant::now(), Arc::clone(&out)));
        Ok(out)
    }

    /// Add, remove and pin perps in one go: all of it or none.
    pub async fn change(&self, change: &Change) -> Result<(), Refusal> {
        if change.add.is_empty() && change.remove.is_empty() && change.header.is_empty() {
            return Err(Refusal::bad("Nothing to change"));
        }
        if change.add.len() + change.remove.len() > MAX_CHANGE {
            return Err(Refusal::bad(format!("At most {MAX_CHANGE} perps a change")));
        }
        let _one = self.changing.lock().await;
        let listings = if change.add.is_empty() { None } else { Some(self.listings().await?) };
        let mut next: HashMap<String, Entry> = entries().into_iter().map(|e| (e.market.coin.to_string(), e)).collect();
        let mut touched: BTreeSet<String> = BTreeSet::new();
        for coin in &change.add {
            let listed = listings.as_ref().and_then(|l| l.iter().flat_map(|d| d.perps.iter()).find(|p| &p.rules.coin == coin));
            let Some(listed) = listed else {
                return Err(Refusal::bad(format!("{coin} isn't a live Hyperliquid perp")));
            };
            let market = market_of(coin, Rules::of(&listed.rules));
            let header = next.get(coin).is_some_and(|e| e.header);
            next.insert(coin.clone(), Entry { market, added: true, header });
            touched.insert(coin.clone());
        }
        for coin in &change.remove {
            let e = next.get_mut(coin).ok_or_else(|| Refusal::not_found(format!("{coin} isn't on the list")))?;
            e.added = false;
            e.header = false;
            touched.insert(coin.clone());
        }
        for (coin, on) in &change.header {
            let e = next.get_mut(coin).ok_or_else(|| Refusal::not_found(format!("{coin} isn't on the list")))?;
            if *on && !e.added {
                return Err(Refusal::conflict(format!("Add {coin} before putting it in the header")));
            }
            e.header = *on;
            touched.insert(coin.clone());
        }
        if next.values().filter(|e| e.header).count() > MAX_HEADER {
            return Err(Refusal::conflict(format!("At most {MAX_HEADER} perps in the header")));
        }
        let changed: Vec<Entry> = touched.iter().filter_map(|c| next.get(c).copied()).collect();
        self.save(&changed).await?;
        write(|r| {
            for e in &changed {
                r.upsert(*e);
            }
        });
        let added: Vec<&str> = change.add.iter().map(String::as_str).collect();
        let removed: Vec<&str> = change.remove.iter().map(String::as_str).collect();
        tracing::warn!("Hyperliquid markets changed: added {added:?}, removed {removed:?}, header {:?}", change.header);
        Ok(())
    }

    /// Write `changed` to `hl_markets` in one transaction.
    async fn save(&self, changed: &[Entry]) -> Result<(), Refusal> {
        let now = shared_domain::current_ist_timestamp_string();
        let write = async {
            let mut tx = self.pool.begin().await?;
            let mut position: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(position), -1) FROM hl_markets").fetch_one(&mut *tx).await?;
            for e in changed {
                let m = e.market;
                position += 1;
                sqlx::query(
                    "INSERT INTO hl_markets (coin, added, header, sz_decimals, max_leverage, taker_fee, maker_fee, position, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT(coin) DO UPDATE SET added = excluded.added, header = excluded.header, sz_decimals = excluded.sz_decimals,
                       max_leverage = excluded.max_leverage, taker_fee = excluded.taker_fee, maker_fee = excluded.maker_fee, updated_at = excluded.updated_at",
                )
                .bind(m.coin)
                .bind(e.added as i64)
                .bind(e.header as i64)
                .bind(m.sz_decimals as i64)
                .bind(m.max_leverage as i64)
                .bind(m.taker_fee)
                .bind(m.maker_fee)
                .bind(position)
                .bind(&now)
                .execute(&mut *tx)
                .await?;
            }
            tx.commit().await
        };
        write.await.map_err(|e: sqlx::Error| {
            tracing::error!("Hyperliquid markets: not saved: {e}");
            Refusal { status: axum::http::StatusCode::INTERNAL_SERVER_ERROR, message: "could not save the markets".into() }
        })
    }

    /// Read the rules of every perp on the list again, from its dex. A perp
    /// Hyperliquid no longer lists turns close-only. What changed, as a count.
    pub async fn refresh(&self) -> Result<usize, Refusal> {
        let _one = self.changing.lock().await;
        let now = entries();
        let dexes: BTreeSet<Option<&'static str>> = now.iter().map(|e| e.market.dex).collect();
        let mut changed = Vec::new();
        for dex in dexes {
            let rules = match self.info.perp_rules(dex).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("Hyperliquid markets: {} rules not read: {e}", dex.unwrap_or("core"));
                    continue;
                }
            };
            for e in now.iter().filter(|e| e.market.dex == dex) {
                match rules.iter().find(|r| r.coin == e.market.coin) {
                    Some(r) => {
                        let market = market_of(e.market.coin, Rules::of(r));
                        if *market != *e.market {
                            tracing::info!("Hyperliquid markets: {}'s rules changed: {:?}", e.market.label, Rules::of(r));
                            changed.push(Entry { market, ..*e });
                        }
                    }
                    None if e.added => {
                        tracing::warn!("Hyperliquid markets: {} is no longer listed — close-only now", e.market.label);
                        changed.push(Entry { added: false, header: false, ..*e });
                    }
                    None => {}
                }
            }
        }
        if changed.is_empty() {
            return Ok(0);
        }
        self.save(&changed).await?;
        write(|r| {
            for e in &changed {
                r.upsert(*e);
            }
        });
        Ok(changed.len())
    }
}

/// The rules refresh, every [`REFRESH_EVERY`] while the switch is on.
pub async fn run_refresh(admin: Arc<MarketAdmin>, flags: Arc<tokio::sync::RwLock<shared_domain::FeatureFlags>>) {
    tokio::time::sleep(Duration::from_secs(20)).await;
    let mut tick = tokio::time::interval(REFRESH_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if !flags.read().await.hyperliquid {
            continue;
        }
        match admin.refresh().await {
            Ok(0) => {}
            Ok(n) => tracing::info!("Hyperliquid markets: {n} updated from Hyperliquid's rules"),
            Err(r) => tracing::warn!("Hyperliquid markets: rules refresh failed: {}", r.message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(sz: u32, lev: u32, taker: f64) -> Rules {
        Rules { sz_decimals: sz, max_leverage: lev, taker_fee: taker, maker_fee: taker / 3.0 }
    }

    #[test]
    fn a_perp_is_named_from_its_coin() {
        let nvda = market_of("xyz:NVDA", rules(3, 10, 0.00009));
        assert_eq!((nvda.key, nvda.base, nvda.dex, nvda.label, nvda.name), ("xyz:NVDA", "NVDA", Some("xyz"), "NVDA-PERP", "NVDA (trade.xyz)"));
        let io = market_of("io:NVDA", rules(3, 10, 0.00009));
        assert_eq!((io.label, io.name), ("NVDA-PERP (io)", "NVDA (EntropyIO)"), "another dex's NVDA is told apart");
        let sol = market_of("SOL", rules(2, 20, 0.00045));
        assert_eq!((sol.key, sol.label, sol.dex), ("SOL", "SOL-PERP", None));
        let gold = market_of("xyz:GOLD", rules(4, 25, 0.0009));
        assert_eq!((gold.key, gold.label, gold.name), ("GOLD", "GOLD-PERP", "Gold (trade.xyz)"), "gold keeps its key and name");
    }

    #[test]
    fn the_same_rules_make_the_same_market() {
        let btc = market_of("BTC", Rules::of_market(&BUILTIN[0]));
        assert!(std::ptr::eq(btc, &BUILTIN[0]), "the built-in one");
        let a = market_of("xyz:TSLA", rules(3, 10, 0.00009));
        let b = market_of("xyz:TSLA", rules(3, 10, 0.00009));
        assert_eq!(a, b);
        let c = market_of("xyz:TSLA", rules(3, 5, 0.00009));
        assert_eq!(c.max_leverage, 5, "new rules, a new market");
    }

    #[test]
    fn the_list_finds_a_perp_by_key_in_any_case_and_by_coin() {
        let mut r = Registry::new(builtin_entries(), 1);
        let nvda = market_of("xyz:NVDA", rules(3, 10, 0.00009));
        r.upsert(Entry { market: nvda, added: true, header: false });
        assert_eq!(r.by_key.get("xyz:nvda").map(|&i| r.entries[i].market.coin), Some("xyz:NVDA"));
        assert_eq!(r.by_key.get("gold").map(|&i| r.entries[i].market.coin), Some("xyz:GOLD"));
        r.upsert(Entry { market: nvda, added: false, header: false });
        assert_eq!(r.entries.len(), 4, "a change replaces, never adds a second");
        assert!(!r.entries[r.by_coin["xyz:NVDA"]].added);
    }
}
