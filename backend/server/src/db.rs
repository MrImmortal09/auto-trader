use shared_domain::{current_ist_timestamp_string, ArchivedCandle, DbWriteMessage, MonitoredPosition};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous};
use shared_domain::{telemetry, FeatureFlags, TradingConfig};
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::balance::BalanceCache;

async fn ensure_column(pool: &SqlitePool, sql: &str) {
    if let Err(e) = sqlx::query(sql).execute(pool).await {
        let msg = e.to_string();
        if !msg.contains("duplicate column name") {
            panic!("failed schema migration: {msg}");
        }
    }
}

// ---------------------------------------------------------------------------
// Initialisation
// ---------------------------------------------------------------------------

/// Open (or create) the SQLite database, enable WAL, and create all tables.
pub async fn init_db(db_url: &str) -> SqlitePool {
    let opts = SqliteConnectOptions::from_str(db_url)
        .expect("invalid DATABASE_URL")
        .create_if_missing(true);
    let pool = SqlitePool::connect_with(opts).await.expect("cannot open SQLite");

    sqlx::query("PRAGMA journal_mode=WAL;").execute(&pool).await.unwrap();

    sqlx::query("CREATE TABLE IF NOT EXISTS wallet (id INTEGER PRIMARY KEY, balance REAL NOT NULL)")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT OR IGNORE INTO wallet (id, balance) VALUES (1, 1000000.0)")
        .execute(&pool).await.unwrap();

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS paper_trades (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ticker TEXT NOT NULL, action TEXT NOT NULL, qty INTEGER NOT NULL,
            executed_price REAL NOT NULL,
            gross_value REAL NOT NULL DEFAULT 0.0, brokerage REAL NOT NULL DEFAULT 0.0,
            stt_charge REAL NOT NULL DEFAULT 0.0, sebi_fee REAL NOT NULL DEFAULT 0.0,
            stamp_duty REAL NOT NULL DEFAULT 0.0, transaction_charge REAL NOT NULL DEFAULT 0.0,
            gst REAL NOT NULL DEFAULT 0.0, net_value REAL NOT NULL DEFAULT 0.0,
            timestamp DATETIME NOT NULL
        )",
    ).execute(&pool).await.unwrap();

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS system_logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            level TEXT NOT NULL, message TEXT NOT NULL,
            timestamp DATETIME NOT NULL
        )",
    ).execute(&pool).await.unwrap();

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS trading_config (
            id INTEGER PRIMARY KEY DEFAULT 1 CHECK (id = 1),
            max_trade_amount_inr REAL NOT NULL DEFAULT 10000.0,
            index_lots INTEGER NOT NULL DEFAULT 1,
            other_lots INTEGER NOT NULL DEFAULT 1,
            mode TEXT NOT NULL DEFAULT 'PAPER',
            brokerage_per_order REAL NOT NULL DEFAULT 20.0,
            target_1_exit_pct REAL NOT NULL DEFAULT 50.0,
            target_2_exit_pct REAL NOT NULL DEFAULT 100.0,
            entry_market_protection REAL NOT NULL DEFAULT 5.0
        )",
    ).execute(&pool).await.unwrap();
    sqlx::query("INSERT OR IGNORE INTO trading_config (id) VALUES (1)")
        .execute(&pool).await.unwrap();
    ensure_column(
        &pool,
        "ALTER TABLE trading_config ADD COLUMN index_lots INTEGER NOT NULL DEFAULT 1",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE trading_config ADD COLUMN other_lots INTEGER NOT NULL DEFAULT 1",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE trading_config ADD COLUMN entry_market_protection REAL NOT NULL DEFAULT 5.0",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE trading_config ADD COLUMN dynamic_targeting INTEGER NOT NULL DEFAULT 0",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE trading_config ADD COLUMN index_lots_by_symbol TEXT NOT NULL DEFAULT '{}'",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE trading_config ADD COLUMN dynamic_targeting_trail_factor REAL NOT NULL DEFAULT 0.5",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE trading_config ADD COLUMN dynamic_targeting_extension_factor REAL NOT NULL DEFAULT 1.0",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE trading_config ADD COLUMN pre_t1_trailing INTEGER NOT NULL DEFAULT 0",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE trading_config ADD COLUMN pre_t1_trail_arm_pct REAL NOT NULL DEFAULT 60.0",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE trading_config ADD COLUMN pre_t1_trail_factor REAL NOT NULL DEFAULT 0.5",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE trading_config ADD COLUMN entry_window_pct REAL NOT NULL DEFAULT 0.0",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE trading_config ADD COLUMN dynamic_targeting_single_lot INTEGER NOT NULL DEFAULT 0",
    ).await;
    // Entry gate: VWAP + PCR indicator sizing (off by default) and the
    // Telegram call validity window. Additive columns with defaults, so an
    // existing database keeps working unchanged — no clearing needed.
    for sql in [
        "ALTER TABLE trading_config ADD COLUMN indicator_sizing INTEGER NOT NULL DEFAULT 0",
        // Fresh databases start at ×1 for a plain agree. An existing row
        // keeps whatever it has saved (this column is already there).
        "ALTER TABLE trading_config ADD COLUMN indicator_agree_multiplier REAL NOT NULL DEFAULT 1.0",
        "ALTER TABLE trading_config ADD COLUMN indicator_neutral_multiplier REAL NOT NULL DEFAULT 0.5",
        "ALTER TABLE trading_config ADD COLUMN indicator_interval_min INTEGER NOT NULL DEFAULT 15",
        "ALTER TABLE trading_config ADD COLUMN indicator_basis TEXT NOT NULL DEFAULT 'chg'",
        "ALTER TABLE trading_config ADD COLUMN indicator_strike_window INTEGER NOT NULL DEFAULT 10",
        "ALTER TABLE trading_config ADD COLUMN indicator_pcr_buy_above REAL NOT NULL DEFAULT 1.0",
        "ALTER TABLE trading_config ADD COLUMN indicator_pcr_sell_below REAL NOT NULL DEFAULT 1.0",
        "ALTER TABLE trading_config ADD COLUMN indicator_vwap_band_pct REAL NOT NULL DEFAULT 0.0",
        "ALTER TABLE trading_config ADD COLUMN call_validity_minutes INTEGER NOT NULL DEFAULT 120",
        // Opening rule + wait-on-disagree default on (they only matter once
        // indicator_sizing itself is on); the top-up buys more, so it is opt-in.
        "ALTER TABLE trading_config ADD COLUMN indicator_early_checks INTEGER NOT NULL DEFAULT 1",
        "ALTER TABLE trading_config ADD COLUMN indicator_wait_on_disagree INTEGER NOT NULL DEFAULT 1",
        "ALTER TABLE trading_config ADD COLUMN indicator_topup INTEGER NOT NULL DEFAULT 0",
        // Strong-agree tier (score ±2) and the top-up rule. Additive, and the
        // saved agree / neutral multipliers are left exactly as they are.
        "ALTER TABLE trading_config ADD COLUMN indicator_strong_agree_multiplier REAL NOT NULL DEFAULT 1.5",
        "ALTER TABLE trading_config ADD COLUMN indicator_topup_mode TEXT NOT NULL DEFAULT 'strong_to_agree'",
        // Lower-SL gate: opt-in, it changes how stops are moved.
        "ALTER TABLE trading_config ADD COLUMN indicator_sl_lower_gate INTEGER NOT NULL DEFAULT 0",
    ] {
        ensure_column(&pool, sql).await;
    }

    ensure_column(
        &pool,
        "ALTER TABLE paper_trades ADD COLUMN signal_id TEXT",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE paper_trades ADD COLUMN raw_message TEXT",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE paper_trades ADD COLUMN exit_reason TEXT",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE paper_trades ADD COLUMN mode TEXT NOT NULL DEFAULT 'PAPER'",
    ).await;
    // The call each trade belongs to, for the v2 history: its Telegram chat
    // (NULL for scalps, the webhook, and every row written before these
    // columns existed — "Unknown channel"), and its side and strike.
    for sql in [
        "ALTER TABLE paper_trades ADD COLUMN chat_id INTEGER",
        "ALTER TABLE paper_trades ADD COLUMN chat_title TEXT",
        "ALTER TABLE paper_trades ADD COLUMN option_type TEXT",
        "ALTER TABLE paper_trades ADD COLUMN strike REAL",
    ] {
        ensure_column(&pool, sql).await;
    }

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS open_positions (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            json TEXT NOT NULL DEFAULT '[]',
            updated_at DATETIME NOT NULL
        )",
    ).execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT OR IGNORE INTO open_positions (id, json, updated_at) VALUES (1, '[]', ?)",
    )
    .bind(current_ist_timestamp_string())
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS kotak_session (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            access_token TEXT NOT NULL,
            auth_token TEXT NOT NULL,
            sid TEXT NOT NULL,
            base_url TEXT NOT NULL,
            updated_at DATETIME NOT NULL
        )",
    ).execute(&pool).await.unwrap();
    // What the market-data feed needs to reconnect on a restored session.
    ensure_column(
        &pool,
        "ALTER TABLE kotak_session ADD COLUMN feed_url TEXT",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE kotak_session ADD COLUMN ucc TEXT",
    ).await;

    // What Telegram needs to reconnect by itself after a restart (see
    // `routes::resume_saved_telegram`); the login itself stays in
    // session.json. New table, so existing databases keep working.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS telegram_resume (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            api_id INTEGER NOT NULL,
            api_hash TEXT NOT NULL,
            chat_ids TEXT NOT NULL,
            updated_at DATETIME NOT NULL
        )",
    ).execute(&pool).await.unwrap();

    // Engine switches for the v2 redesign (`shared_domain::FeatureFlags`,
    // `/api/v2/flags`): one row per switch, a missing row means off. Kept out
    // of `trading_config` because v1's settings save replaces that row whole.
    // New table, so existing databases keep working.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS feature_flags (
            name TEXT PRIMARY KEY,
            enabled INTEGER NOT NULL,
            updated_at TEXT NOT NULL
        )",
    ).execute(&pool).await.unwrap();

    // Today's 1-minute bars for the v2 charts (see `candles`), so a restart
    // keeps the day. Pruned to today whenever the candle service starts. New
    // table, so existing databases keep working.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS candles (
            symbol TEXT NOT NULL,
            t INTEGER NOT NULL,
            open REAL NOT NULL,
            high REAL NOT NULL,
            low REAL NOT NULL,
            close REAL NOT NULL,
            volume REAL,
            source TEXT NOT NULL,
            PRIMARY KEY (symbol, t)
        )",
    ).execute(&pool).await.unwrap();

    // How long each LIVE order took (see `order_timing`): one row per order,
    // the send and Kotak's reply from the send tap, the rest from Kotak's push
    // stream, merged by order number whichever lands first. A send Kotak
    // refused has no number and gets a row of its own. Analytics only; kept,
    // like the trades. New table, so existing databases keep working.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS order_timings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            order_no TEXT UNIQUE,
            day TEXT NOT NULL,
            trading_symbol TEXT,
            side TEXT,
            order_type TEXT,
            qty INTEGER,
            price REAL,
            trigger_price REAL,
            sent_at_ms INTEGER,
            replied_at_ms INTEGER,
            error TEXT,
            status TEXT,
            filled_qty INTEGER,
            avg_price REAL,
            reject_reason TEXT,
            first_update_ms INTEGER,
            accepted_at_ms INTEGER,
            first_fill_ms INTEGER,
            filled_at_ms INTEGER,
            kotak_received_ns INTEGER,
            exchange_epoch_s INTEGER,
            exchange_confirm TEXT,
            order_time TEXT,
            updated_ms INTEGER
        )",
    ).execute(&pool).await.unwrap();
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_order_timings_day ON order_timings (day)")
        .execute(&pool).await.unwrap();
    // When the engine's own order-book poll first saw the order finished.
    ensure_column(&pool, "ALTER TABLE order_timings ADD COLUMN seen_at_ms INTEGER").await;

    // The permanent 1-minute archive (see `candles::archive`): the indices and
    // every listed future on them, with volume and OI, never pruned, so a past
    // day's VWAP and pivots can be rebuilt after Upstox stops serving the
    // contract. About 170 MB a year (Upstox sends a row for every minute,
    // traded or not). New table, so existing databases keep working.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS candle_archive (
            symbol TEXT NOT NULL,
            t INTEGER NOT NULL,
            open REAL NOT NULL,
            high REAL NOT NULL,
            low REAL NOT NULL,
            close REAL NOT NULL,
            volume REAL NOT NULL,
            oi REAL NOT NULL,
            PRIMARY KEY (symbol, t)
        ) WITHOUT ROWID",
    ).execute(&pool).await.unwrap();

    // Upstox option-chain analytics (see `market_data.rs`). New table, so this
    // is backward compatible with existing databases.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS oi_snapshots (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts TEXT NOT NULL,
            underlying TEXT NOT NULL,
            expiry TEXT NOT NULL,
            strike_window INTEGER NOT NULL,
            call_oi REAL NOT NULL,
            put_oi REAL NOT NULL,
            call_oi_chg REAL NOT NULL,
            put_oi_chg REAL NOT NULL,
            spot REAL NOT NULL,
            fut_ltp REAL,
            fut_vwap REAL
        )",
    ).execute(&pool).await.unwrap();
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_oi_snapshots_lookup ON oi_snapshots (underlying, strike_window, ts)",
    ).execute(&pool).await.unwrap();
    // By-day reads across every index: the startup feed seed and the prune below.
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_oi_snapshots_ts ON oi_snapshots (ts)",
    ).execute(&pool).await.unwrap();
    // ~4.5k rows per trading day; keep a month.
    let cutoff = (shared_domain::now_ist() - chrono::Duration::days(30)).format("%Y-%m-%d").to_string();
    let _ = sqlx::query("DELETE FROM oi_snapshots WHERE ts < ?").bind(&cutoff).execute(&pool).await;

    // Every message from the monitored chats, for the v2 Desk's feed (see
    // `routes::v2::feed`); display only. New table, so existing databases
    // keep working. A month is kept, like the OI snapshots.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS telegram_messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            chat_id INTEGER NOT NULL,
            chat_title TEXT,
            msg_id INTEGER NOT NULL,
            sent_at TEXT NOT NULL,
            text TEXT NOT NULL,
            kind TEXT NOT NULL,
            reply_to INTEGER,
            value REAL,
            edited_at TEXT,
            UNIQUE (chat_id, msg_id)
        )",
    ).execute(&pool).await.unwrap();
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_telegram_messages_sent ON telegram_messages (sent_at)",
    ).execute(&pool).await.unwrap();
    // What a call named, for the v2 charts' call markers.
    ensure_column(&pool, "ALTER TABLE telegram_messages ADD COLUMN instrument TEXT").await;
    ensure_column(&pool, "ALTER TABLE telegram_messages ADD COLUMN strike REAL").await;
    ensure_column(&pool, "ALTER TABLE telegram_messages ADD COLUMN side TEXT").await;
    let _ = sqlx::query("DELETE FROM telegram_messages WHERE sent_at < ?").bind(&cutoff).execute(&pool).await;

    // Who may sign in by phone (see `auth`): the allow-list the admin keeps,
    // and the one-time codes sent to it, one row per send. New tables, so
    // existing databases keep working.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            phone TEXT NOT NULL UNIQUE,
            name TEXT,
            role TEXT NOT NULL DEFAULT 'user',
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL,
            last_login_at TEXT
        )",
    ).execute(&pool).await.unwrap();
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS otp_codes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            phone TEXT NOT NULL,
            code_hash TEXT NOT NULL,
            sent_at TEXT NOT NULL,
            expires_at TEXT NOT NULL,
            attempts INTEGER NOT NULL DEFAULT 0,
            consumed INTEGER NOT NULL DEFAULT 0,
            ip TEXT
        )",
    ).execute(&pool).await.unwrap();
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_otp_codes_phone_sent ON otp_codes (phone, sent_at)")
        .execute(&pool).await.unwrap();
    let _ = crate::auth::otp::prune_before_window(&pool, &current_ist_timestamp_string()).await;

    // Each user's own paper portfolio (`paper`): a wallet, a ledger of fills
    // like `paper_trades`, and the open positions as one JSON snapshot per
    // user like `open_positions`. Nothing here is the owner's: the live
    // engine never reads these tables. A reset bumps `epoch`; older legs stay
    // in the ledger but drop out of every view.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS user_wallets (
            user_id INTEGER PRIMARY KEY,
            balance REAL NOT NULL,
            epoch INTEGER NOT NULL DEFAULT 0,
            updated_at TEXT NOT NULL
        )",
    ).execute(&pool).await.unwrap();
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS user_trades (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id INTEGER NOT NULL,
            epoch INTEGER NOT NULL DEFAULT 0,
            position_id TEXT NOT NULL,
            ticker TEXT NOT NULL,
            trading_symbol TEXT NOT NULL,
            action TEXT NOT NULL,
            qty INTEGER NOT NULL,
            executed_price REAL NOT NULL,
            timestamp TEXT NOT NULL,
            gross_value REAL NOT NULL, brokerage REAL NOT NULL,
            stt_charge REAL NOT NULL, sebi_fee REAL NOT NULL,
            stamp_duty REAL NOT NULL, transaction_charge REAL NOT NULL,
            gst REAL NOT NULL, net_value REAL NOT NULL,
            exit_reason TEXT,
            option_type TEXT,
            strike REAL,
            expiry TEXT,
            label TEXT
        )",
    ).execute(&pool).await.unwrap();
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_user_trades_user ON user_trades (user_id, epoch, id)")
        .execute(&pool).await.unwrap();
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS user_positions (
            user_id INTEGER PRIMARY KEY,
            json TEXT NOT NULL DEFAULT '[]',
            updated_at TEXT NOT NULL
        )",
    ).execute(&pool).await.unwrap();

    // Each signed-in person's watchlists (`watchlists`, `/api/v2/watchlists`):
    // up to five named lists of up to fifty price-map keys, kept by user id
    // (the admin is a `users` row too). Nothing here is read by the engine;
    // a list never touches the feed. New tables, so existing databases keep
    // working. The caps are enforced by the conditional inserts in
    // `watchlists`, never by a trigger, so a row written by hand is never
    // refused at start-up.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS user_watchlists (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            position INTEGER NOT NULL,
            created_at TEXT NOT NULL
        )",
    ).execute(&pool).await.unwrap();
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_user_watchlists_user ON user_watchlists (user_id, position)")
        .execute(&pool).await.unwrap();
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS user_watchlist_items (
            watchlist_id INTEGER NOT NULL,
            key TEXT NOT NULL,
            position INTEGER NOT NULL,
            added_at TEXT NOT NULL,
            PRIMARY KEY (watchlist_id, key)
        )",
    ).execute(&pool).await.unwrap();
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_user_watchlist_items_order ON user_watchlist_items (watchlist_id, position)")
        .execute(&pool).await.unwrap();

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS special_sessions (
            date TEXT PRIMARY KEY,
            start TEXT NOT NULL,
            end TEXT NOT NULL,
            created_at TEXT NOT NULL
        )",
    ).execute(&pool).await.unwrap();

    pool
}

// ---------------------------------------------------------------------------
// The instrument catalog's own database
// ---------------------------------------------------------------------------

/// The `instruments` table's columns — of `instruments` itself and of the
/// `instruments_new` a rebuild fills before renaming it over (see
/// `instruments::load`). One row per listed instrument: `id` is
/// `<segment>:<token>`, `kind` is INDEX, EQ, FUT, CE or PE, `expiry` is
/// `YYYY-MM-DD` or NULL for cash, `ws_key` the price-map key.
pub const INSTRUMENTS_COLUMNS: &str = "
    id TEXT PRIMARY KEY,
    segment TEXT NOT NULL,
    token TEXT NOT NULL,
    trading_symbol TEXT NOT NULL,
    symbol TEXT NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    underlying TEXT NOT NULL,
    expiry TEXT,
    strike REAL,
    lot_size INTEGER NOT NULL,
    tick_size REAL NOT NULL,
    isin TEXT,
    exchange TEXT NOT NULL,
    ws_key TEXT NOT NULL,
    loaded_day TEXT NOT NULL";

/// Its indexes: by symbol, by chain (underlying, kind, expiry, strike) and
/// by ISIN. A rebuild creates them again on the renamed table. The chain
/// index leads with `kind` after the underlying so the search's lookups are
/// seeks, never a walk of every contract under NIFTY: the futures from
/// today (`underlying, 'FUT', expiry >=`), an option expiry list as repeated
/// `MIN(expiry)` seeks per kind, one expiry's strikes around the spot
/// (`underlying, kind, expiry, strike`) — see `instruments::Catalog`.
pub const INSTRUMENT_INDEXES: [&str; 3] = [
    "CREATE INDEX IF NOT EXISTS idx_instruments_symbol ON instruments (symbol)",
    "CREATE INDEX IF NOT EXISTS idx_instruments_chain ON instruments (underlying, kind, expiry, strike)",
    "CREATE INDEX IF NOT EXISTS idx_instruments_isin ON instruments (isin)",
];

/// Opens (or creates) the instrument catalog's database at `path`
/// (`data/instruments.db`, see `instruments`). Its own file, apart from
/// trades.db: a rebuild writes 150-200k rows in one transaction, and SQLite
/// allows one writer at a time, so in trades.db it would hold the engine's
/// `db_writer` behind the busy timeout. The quote hub's close snapshot
/// (`quote_eod`) lives here too, for the same reason: its 15:35 write of a
/// few thousand rows then never queues behind, or holds up, the engine's
/// single writer. Everything in the file is market data rebuilt or
/// re-swept from Kotak. Tables are created if missing, never altered.
/// Returns the pool and whether FTS5 is available (`instruments_fts` could
/// be created), which the bundled SQLite has.
pub async fn init_catalog_db(path: &Path) -> Result<(SqlitePool, bool), sqlx::Error> {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal);
    let pool = SqlitePoolOptions::new().max_connections(3).connect_with(opts).await?;

    sqlx::query(&format!("CREATE TABLE IF NOT EXISTS instruments ({INSTRUMENTS_COLUMNS})"))
        .execute(&pool)
        .await?;
    for sql in INSTRUMENT_INDEXES {
        sqlx::query(sql).execute(&pool).await?;
    }
    // The search list as the last build left it, so a restart can search
    // without a rebuild: one row per symbol, with its cash or index row.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS instrument_underlyings (
            symbol TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            kind TEXT NOT NULL,
            id TEXT,
            has_fo INTEGER NOT NULL,
            fo_index INTEGER NOT NULL,
            fo_segment TEXT,
            isin TEXT,
            exchange TEXT,
            ws_key TEXT,
            trading_symbol TEXT,
            lot_size INTEGER NOT NULL DEFAULT 1,
            tick_size REAL NOT NULL DEFAULT 0.05
        )",
    )
    .execute(&pool)
    .await?;
    // One row per segment per build: how many rows, from a file modified
    // when — what the next build compares to know whether anything changed.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS instrument_load (
            day TEXT NOT NULL,
            segment TEXT NOT NULL,
            rows INTEGER NOT NULL,
            loaded_at TEXT NOT NULL,
            source_mtime INTEGER,
            PRIMARY KEY (day, segment)
        )",
    )
    .execute(&pool)
    .await?;
    // Rows read per kind (`{"CE": n, "PE": n, "FUT": n}` as JSON), counted
    // as the file is parsed, so the status never counts the table. Additive,
    // so a catalog from before the column keeps working.
    ensure_column(&pool, "ALTER TABLE instrument_load ADD COLUMN kinds TEXT").await;
    // The close snapshot the quote hub sweeps at 15:35 IST (see
    // `quotes::eod`): one row per instrument per day. The hub serves a
    // day's row as the instrument's close whenever the market is shut —
    // after 15:40, before 09:15, weekends and holidays — in place of any
    // live quote from that day or earlier, session or no session, until a
    // later day's quote arrives; never while the market is open.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS quote_eod (
            key TEXT NOT NULL,
            day TEXT NOT NULL,
            ltp REAL NOT NULL,
            close REAL,
            open REAL,
            high REAL,
            low REAL,
            volume REAL,
            taken_at TEXT NOT NULL,
            PRIMARY KEY (key, day)
        )",
    )
    .execute(&pool)
    .await?;
    // One row per sweep: how many keys it asked for and how many answered,
    // so a snapshot cut short by the 15:40 deadline (or short of answers) is
    // known to be partial after a restart too.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS quote_eod_days (
            day TEXT PRIMARY KEY,
            taken_at TEXT NOT NULL,
            asked INTEGER NOT NULL,
            answered INTEGER NOT NULL
        )",
    )
    .execute(&pool)
    .await?;
    let fts = match sqlx::query(
        "CREATE VIRTUAL TABLE IF NOT EXISTS instruments_fts USING fts5(symbol, name, tokenize = 'unicode61')",
    )
    .execute(&pool)
    .await
    {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(error = %e, "This SQLite has no FTS5 — instrument name search runs in memory instead");
            false
        }
    };
    Ok((pool, fts))
}

// ---------------------------------------------------------------------------
// Config loader
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct TradingConfigRow {
    max_trade_amount_inr: f64,
    index_lots: i32,
    other_lots: i32,
    mode: String,
    brokerage_per_order: f64,
    target_1_exit_pct: f64,
    target_2_exit_pct: f64,
    entry_market_protection: f64,
    dynamic_targeting: bool,
    index_lots_by_symbol: String,
    dynamic_targeting_trail_factor: f64,
    dynamic_targeting_extension_factor: f64,
    pre_t1_trailing: bool,
    pre_t1_trail_arm_pct: f64,
    pre_t1_trail_factor: f64,
    entry_window_pct: f64,
    dynamic_targeting_single_lot: bool,
    indicator_sizing: bool,
    indicator_agree_multiplier: f64,
    indicator_neutral_multiplier: f64,
    indicator_interval_min: i32,
    indicator_basis: String,
    indicator_strike_window: i64,
    indicator_pcr_buy_above: f64,
    indicator_pcr_sell_below: f64,
    indicator_vwap_band_pct: f64,
    call_validity_minutes: i32,
    indicator_early_checks: bool,
    indicator_wait_on_disagree: bool,
    indicator_topup: bool,
    indicator_strong_agree_multiplier: f64,
    indicator_topup_mode: String,
    indicator_sl_lower_gate: bool,
}

/// Load `TradingConfig` from SQLite, falling back to safe defaults.
pub async fn load_config_from_db(pool: &SqlitePool) -> TradingConfig {
    sqlx::query_as::<_, TradingConfigRow>(
        "SELECT max_trade_amount_inr, index_lots, other_lots, mode, brokerage_per_order,
                target_1_exit_pct, target_2_exit_pct, entry_market_protection, dynamic_targeting,
                index_lots_by_symbol, dynamic_targeting_trail_factor, dynamic_targeting_extension_factor,
                pre_t1_trailing, pre_t1_trail_arm_pct, pre_t1_trail_factor, entry_window_pct,
                dynamic_targeting_single_lot,
                indicator_sizing, indicator_agree_multiplier, indicator_neutral_multiplier,
                indicator_interval_min, indicator_basis, indicator_strike_window,
                indicator_pcr_buy_above, indicator_pcr_sell_below, indicator_vwap_band_pct,
                call_validity_minutes,
                indicator_early_checks, indicator_wait_on_disagree, indicator_topup,
                indicator_strong_agree_multiplier, indicator_topup_mode,
                indicator_sl_lower_gate
         FROM trading_config WHERE id = 1",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .map(|r| {
        let mut cfg = TradingConfig {
            max_trade_amount_inr: r.max_trade_amount_inr,
            // 0 is allowed — it means "don't auto-trade this class" (an index with
            // no per-symbol override / every stock option). Only guard against a
            // negative slipping in from a hand-edited row.
            index_lots: r.index_lots.max(0),
            other_lots: r.other_lots.max(0),
            mode: r.mode,
            brokerage_per_order: r.brokerage_per_order,
            target_1_exit_pct: r.target_1_exit_pct,
            target_2_exit_pct: r.target_2_exit_pct,
            entry_market_protection: r.entry_market_protection,
            dynamic_targeting: r.dynamic_targeting,
            index_lots_by_symbol: serde_json::from_str(&r.index_lots_by_symbol).unwrap_or_default(),
            dynamic_targeting_trail_factor: r.dynamic_targeting_trail_factor.clamp(0.0, 1.0),
            dynamic_targeting_extension_factor: r.dynamic_targeting_extension_factor.clamp(0.05, 5.0),
            dynamic_targeting_single_lot: r.dynamic_targeting_single_lot,
            pre_t1_trailing: r.pre_t1_trailing,
            pre_t1_trail_arm_pct: r.pre_t1_trail_arm_pct.clamp(0.0, 100.0),
            pre_t1_trail_factor: r.pre_t1_trail_factor.clamp(0.0, 1.0),
            kill_switch_active: false,
            entry_window_pct: r.entry_window_pct.clamp(0.0, 100.0),
            indicator_sizing: r.indicator_sizing,
            indicator_agree_multiplier: r.indicator_agree_multiplier,
            indicator_strong_agree_multiplier: r.indicator_strong_agree_multiplier,
            indicator_neutral_multiplier: r.indicator_neutral_multiplier,
            indicator_interval_min: r.indicator_interval_min,
            indicator_basis: r.indicator_basis,
            indicator_strike_window: r.indicator_strike_window,
            indicator_pcr_buy_above: r.indicator_pcr_buy_above,
            indicator_pcr_sell_below: r.indicator_pcr_sell_below,
            indicator_vwap_band_pct: r.indicator_vwap_band_pct,
            call_validity_minutes: r.call_validity_minutes,
            indicator_early_checks: r.indicator_early_checks,
            indicator_wait_on_disagree: r.indicator_wait_on_disagree,
            indicator_topup: r.indicator_topup,
            indicator_topup_mode: r.indicator_topup_mode,
            indicator_sl_lower_gate: r.indicator_sl_lower_gate,
        };
        // Same clamps the settings handler applies, so a hand-edited row
        // can't put an out-of-range value in front of the engine.
        cfg.sanitize_entry_gate();
        cfg
    })
    .unwrap_or_else(|| TradingConfig {
        max_trade_amount_inr: 10_000.0,
        index_lots: 1,
        other_lots: 1,
        mode: "PAPER".into(),
        brokerage_per_order: 20.0,
        target_1_exit_pct: 50.0,
        target_2_exit_pct: 100.0,
        entry_market_protection: 5.0,
        dynamic_targeting: false,
        index_lots_by_symbol: Default::default(),
        dynamic_targeting_trail_factor: 0.5,
        dynamic_targeting_extension_factor: 1.0,
        dynamic_targeting_single_lot: false,
        pre_t1_trailing: false,
        pre_t1_trail_arm_pct: 60.0,
        pre_t1_trail_factor: 0.5,
        kill_switch_active: false,
        entry_window_pct: 0.0,
        indicator_sizing: false,
        indicator_agree_multiplier: 1.0,
        indicator_strong_agree_multiplier: 1.5,
        indicator_neutral_multiplier: 0.5,
        indicator_interval_min: 15,
        indicator_basis: "chg".into(),
        indicator_strike_window: 10,
        indicator_pcr_buy_above: 1.0,
        indicator_pcr_sell_below: 1.0,
        indicator_vwap_band_pct: 0.0,
        call_validity_minutes: 120,
        indicator_early_checks: true,
        indicator_wait_on_disagree: true,
        indicator_topup: false,
        indicator_topup_mode: "strong_to_agree".into(),
        indicator_sl_lower_gate: false,
    })
}

pub async fn load_open_positions(pool: &SqlitePool) -> Vec<MonitoredPosition> {
    let json = sqlx::query_scalar::<_, String>(
        "SELECT json FROM open_positions WHERE id = 1",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| "[]".to_string());

    serde_json::from_str::<Vec<MonitoredPosition>>(&json)
        .unwrap_or_default()
}

pub struct KotakSessionRow {
    pub access_token: String,
    pub auth_token: String,
    pub sid: String,
    pub base_url: String,
    pub feed_url: Option<String>,
    pub ucc: String,
    pub updated_at: String,
}

pub async fn save_kotak_session(
    pool: &SqlitePool,
    access: &str,
    auth: &str,
    sid: &str,
    base_url: &str,
    feed_url: Option<&str>,
    ucc: &str,
) {
    sqlx::query(
        "INSERT INTO kotak_session (id, access_token, auth_token, sid, base_url, feed_url, ucc, updated_at)
         VALUES (1, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
         access_token = excluded.access_token,
         auth_token = excluded.auth_token,
         sid = excluded.sid,
         base_url = excluded.base_url,
         feed_url = excluded.feed_url,
         ucc = excluded.ucc,
         updated_at = excluded.updated_at"
    )
    .bind(access)
    .bind(auth)
    .bind(sid)
    .bind(base_url)
    .bind(feed_url)
    .bind(ucc)
    .bind(shared_domain::current_ist_timestamp_string())
    .execute(pool)
    .await
    .unwrap();
}

pub async fn load_kotak_session(pool: &SqlitePool) -> Option<KotakSessionRow> {
    let row = sqlx::query_as::<_, (String, String, String, String, Option<String>, Option<String>, String)>(
        "SELECT access_token, auth_token, sid, base_url, feed_url, ucc, updated_at FROM kotak_session WHERE id = 1",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;

    let (access_token, auth_token, sid, base_url, feed_url, ucc, updated_at) = row;

    // Check if the session is from today in IST. If not, it's expired.
    if !updated_at.starts_with(&shared_domain::now_ist().format("%Y-%m-%d").to_string()) {
        return None;
    }

    // A row saved before the market feed needed the client code can't start
    // the feed, and a session without prices can't trade — treat it as
    // absent so the caller logs in afresh.
    let ucc = ucc.filter(|u| !u.trim().is_empty())?;

    Some(KotakSessionRow {
        access_token,
        auth_token,
        sid,
        base_url,
        feed_url,
        ucc,
        updated_at,
    })
}

/// The Telegram app credentials and monitored chats saved by the last Start.
pub struct TelegramResume {
    pub api_id: i32,
    pub api_hash: String,
    pub chat_ids: Vec<i64>,
}

/// Save what Telegram needs to reconnect after a restart. Called on every
/// successful Start, so it always reflects the chats last chosen.
pub async fn save_telegram_resume(pool: &SqlitePool, api_id: i32, api_hash: &str, chat_ids: &[i64]) {
    let chat_ids = serde_json::to_string(chat_ids).unwrap_or_else(|_| "[]".to_string());
    if let Err(e) = sqlx::query(
        "INSERT INTO telegram_resume (id, api_id, api_hash, chat_ids, updated_at)
         VALUES (1, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
         api_id = excluded.api_id,
         api_hash = excluded.api_hash,
         chat_ids = excluded.chat_ids,
         updated_at = excluded.updated_at",
    )
    .bind(api_id)
    .bind(api_hash)
    .bind(&chat_ids)
    .bind(shared_domain::current_ist_timestamp_string())
    .execute(pool)
    .await
    {
        tracing::error!("save telegram_resume: {e}");
    }
}

/// `None` when nothing is saved, or the row is unreadable.
pub async fn load_telegram_resume(pool: &SqlitePool) -> Option<TelegramResume> {
    let (api_id, api_hash, chat_ids) = sqlx::query_as::<_, (i32, String, String)>(
        "SELECT api_id, api_hash, chat_ids FROM telegram_resume WHERE id = 1",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    let chat_ids = serde_json::from_str(&chat_ids).ok()?;
    Some(TelegramResume { api_id, api_hash, chat_ids })
}

/// Forget the saved Telegram credentials — an explicit Disconnect or Reset
/// must not be undone by the next restart.
pub async fn clear_telegram_resume(pool: &SqlitePool) {
    if let Err(e) = sqlx::query("DELETE FROM telegram_resume").execute(pool).await {
        tracing::error!("clear telegram_resume: {e}");
    }
}

/// The v2 engine switches. A missing row, an unknown name or an unreadable
/// table leaves a switch off — today's behaviour.
pub async fn load_feature_flags(pool: &SqlitePool) -> FeatureFlags {
    let rows = sqlx::query_as::<_, (String, bool)>("SELECT name, enabled FROM feature_flags")
        .fetch_all(pool)
        .await
        .unwrap_or_else(|e| {
            tracing::error!("load feature_flags: {e}");
            Vec::new()
        });
    let mut flags = FeatureFlags::default();
    for (name, enabled) in rows {
        if !flags.set(&name, enabled) {
            tracing::warn!(name, "Unknown row in feature_flags — ignored");
        }
    }
    flags
}

/// Store `changes` (`(name, on)` per switch) in one transaction: either all
/// of them are saved or none is.
pub async fn save_feature_flags(pool: &SqlitePool, changes: &[(&str, bool)]) -> Result<(), sqlx::Error> {
    let now = current_ist_timestamp_string();
    let mut tx = pool.begin().await?;
    for (name, enabled) in changes {
        sqlx::query(
            "INSERT INTO feature_flags (name, enabled, updated_at) VALUES (?, ?, ?)
             ON CONFLICT(name) DO UPDATE SET enabled = excluded.enabled, updated_at = excluded.updated_at",
        )
        .bind(name)
        .bind(enabled)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// Load upcoming special sessions from SQLite (and those within the last 7 days).
pub async fn load_special_sessions(pool: &SqlitePool) -> Result<Vec<shared_domain::SpecialSession>, sqlx::Error> {
    let cutoff = (shared_domain::today_ist() - chrono::Duration::days(7))
        .format("%Y-%m-%d")
        .to_string();
    let rows = sqlx::query_as::<_, (String, String, String)>(
        "SELECT date, start, end FROM special_sessions WHERE date >= ? ORDER BY date ASC",
    )
    .bind(cutoff)
    .fetch_all(pool)
    .await?;

    let mut sessions = Vec::new();
    for (d_str, s_str, e_str) in rows {
        let Ok(date) = chrono::NaiveDate::parse_from_str(&d_str, "%Y-%m-%d") else {
            tracing::warn!(date = %d_str, "Skipping malformed date in special_sessions");
            continue;
        };
        let Some(start) = shared_domain::parse_hhmm(&s_str) else {
            tracing::warn!(date = %d_str, start = %s_str, "Skipping malformed start in special_sessions");
            continue;
        };
        let Some(end) = shared_domain::parse_hhmm(&e_str) else {
            tracing::warn!(date = %d_str, end = %e_str, "Skipping malformed end in special_sessions");
            continue;
        };
        sessions.push(shared_domain::SpecialSession { date, start, end });
    }
    Ok(sessions)
}

/// Save or replace a special session row.
pub async fn save_special_session(
    pool: &SqlitePool,
    session: &shared_domain::SpecialSession,
) -> Result<(), sqlx::Error> {
    let now = current_ist_timestamp_string();
    let date_str = session.date.format("%Y-%m-%d").to_string();
    let start_str = shared_domain::format_hhmm(session.start);
    let end_str = shared_domain::format_hhmm(session.end);

    sqlx::query(
        "INSERT INTO special_sessions (date, start, end, created_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(date) DO UPDATE SET
         start = excluded.start,
         end = excluded.end,
         created_at = excluded.created_at",
    )
    .bind(&date_str)
    .bind(&start_str)
    .bind(&end_str)
    .bind(&now)
    .execute(pool)
    .await?;

    Ok(())
}

/// Delete a special session by date string (YYYY-MM-DD). Returns true if a row was deleted.
pub async fn delete_special_session(pool: &SqlitePool, date: &str) -> Result<bool, sqlx::Error> {
    let res = sqlx::query("DELETE FROM special_sessions WHERE date = ?")
        .bind(date)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

// ---------------------------------------------------------------------------
// Sequential writer task
// ---------------------------------------------------------------------------

/// Dedicated SQLite writer — processes one `DbWriteMessage` at a time,
/// eliminating "database is locked" errors from concurrent writers. Every
/// executed buy / sell passes through here, so it is also what tells the
/// dashboard's balance cache to re-read Kotak after a LIVE trade.
pub async fn db_writer(
    mut rx: mpsc::Receiver<DbWriteMessage>,
    pool: SqlitePool,
    balance: Arc<BalanceCache>,
) {
    while let Some(msg) = rx.recv().await {
        let _timer = telemetry::DbWriteTimer::start(msg.metric_kind());
        match msg {
            DbWriteMessage::Trade {
                ticker, action, qty, executed_price,
                gross_value, brokerage, stt_charge, sebi_fee,
                stamp_duty, transaction_charge, gst, net_value,
                tag, exit_reason, mode,
            } => {
                // The fill happened at the broker whether or not the rows
                // below land, so the balance is re-read either way.
                if mode == "LIVE" {
                    balance.trade_recorded();
                }
                let timestamp = current_ist_timestamp_string();
                let mut tx = match pool.begin().await {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::error!("DB begin tx: {e}");
                        telemetry::db_write_error("trade");
                        continue;
                    }
                };

                if let Err(e) = sqlx::query(
                    "INSERT INTO paper_trades
                     (ticker, action, qty, executed_price, timestamp,
                      gross_value, brokerage, stt_charge, sebi_fee,
                      stamp_duty, transaction_charge, gst, net_value,
                      signal_id, raw_message, exit_reason, mode,
                      chat_id, chat_title, option_type, strike)
                     VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                )
                 .bind(&ticker).bind(&action).bind(qty as i64).bind(executed_price).bind(&timestamp)
                .bind(gross_value).bind(brokerage).bind(stt_charge).bind(sebi_fee)
                .bind(stamp_duty).bind(transaction_charge).bind(gst).bind(net_value)
                .bind(&tag.signal_id).bind(&tag.raw_message).bind(&exit_reason).bind(&mode)
                .bind(tag.chat_id).bind(&tag.chat_title).bind(&tag.option_type).bind(tag.strike)
                .execute(&mut *tx).await
                {
                    tracing::error!("DB trade insert: {e}");
                    telemetry::db_write_error("trade");
                    let _ = tx.rollback().await;
                    continue;
                }

                let wallet_delta = if action.eq_ignore_ascii_case("BUY") {
                    -net_value
                } else {
                    net_value
                };

                if let Err(e) = sqlx::query("UPDATE wallet SET balance = balance + ? WHERE id = 1")
                    .bind(wallet_delta)
                    .execute(&mut *tx)
                    .await
                {
                    tracing::error!("DB wallet update: {e}");
                    telemetry::db_write_error("trade");
                    let _ = tx.rollback().await;
                    continue;
                }

                if let Err(e) = tx.commit().await {
                    tracing::error!("DB commit tx: {e}");
                    telemetry::db_write_error("trade");
                }
            }
            DbWriteMessage::Log { level, message } => {
                let timestamp = current_ist_timestamp_string();
                if let Err(e) = sqlx::query(
                    "INSERT INTO system_logs (level, message, timestamp) VALUES (?, ?, ?)",
                )
                .bind(&level).bind(&message).bind(&timestamp).execute(&pool).await
                {
                    tracing::error!("DB log insert: {e}");
                    telemetry::db_write_error("log");
                }
            }
            DbWriteMessage::PositionsSnapshot { json } => {
                let timestamp = current_ist_timestamp_string();
                if let Err(e) = sqlx::query(
                    "UPDATE open_positions SET json = ?, updated_at = ? WHERE id = 1",
                )
                .bind(&json)
                .bind(&timestamp)
                .execute(&pool)
                .await
                {
                    tracing::error!("DB positions snapshot update: {e}");
                    telemetry::db_write_error("positions_snapshot");
                }
            }
            DbWriteMessage::OiSnapshot {
                ts, underlying, expiry, strike_window,
                call_oi, put_oi, call_oi_chg, put_oi_chg,
                spot, fut_ltp, fut_vwap,
            } => {
                if let Err(e) = sqlx::query(
                    "INSERT INTO oi_snapshots
                     (ts, underlying, expiry, strike_window, call_oi, put_oi,
                      call_oi_chg, put_oi_chg, spot, fut_ltp, fut_vwap)
                     VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                )
                .bind(&ts).bind(&underlying).bind(&expiry).bind(strike_window)
                .bind(call_oi).bind(put_oi).bind(call_oi_chg).bind(put_oi_chg)
                .bind(spot).bind(fut_ltp).bind(fut_vwap)
                .execute(&pool)
                .await
                {
                    tracing::error!("DB oi snapshot insert: {e}");
                    telemetry::db_write_error("oi_snapshot");
                }
            }
            DbWriteMessage::CandleMinute { symbol, t, open, high, low, close, volume, source } => {
                if let Err(e) = sqlx::query(
                    "INSERT INTO candles (symbol, t, open, high, low, close, volume, source)
                     VALUES (?,?,?,?,?,?,?,?)
                     ON CONFLICT(symbol, t) DO UPDATE SET
                       open = excluded.open, high = excluded.high, low = excluded.low,
                       close = excluded.close, volume = excluded.volume, source = excluded.source
                     WHERE candles.source <> 'upstox' OR excluded.source = 'upstox'",
                )
                .bind(&symbol).bind(t).bind(open).bind(high).bind(low).bind(close).bind(volume).bind(source)
                .execute(&pool)
                .await
                {
                    tracing::error!("DB candle insert: {e}");
                    telemetry::db_write_error("candle");
                }
            }
            DbWriteMessage::OrderSent(sent) => {
                if let Err(e) = write_order_sent(&pool, &sent).await {
                    tracing::error!(order_no = ?sent.order_no, error = %e, "DB order timing insert");
                    telemetry::db_write_error("order_sent");
                }
            }
            DbWriteMessage::OrderUpdate(update) => {
                if let Err(e) = write_order_update(&pool, &update).await {
                    tracing::error!(order_no = %update.order_no, error = %e, "DB order update insert");
                    telemetry::db_write_error("order_update");
                }
            }
            DbWriteMessage::OrderSeen { order_no, at_ms } => {
                // Only an order this table already has: the first time kept.
                if let Err(e) = sqlx::query("UPDATE order_timings SET seen_at_ms = COALESCE(seen_at_ms, ?) WHERE order_no = ?")
                    .bind(at_ms)
                    .bind(&order_no)
                    .execute(&pool)
                    .await
                {
                    tracing::error!(%order_no, error = %e, "DB order seen update");
                    telemetry::db_write_error("order_seen");
                }
            }
            DbWriteMessage::ArchiveCandles { symbol, candles } => {
                if let Err(e) = write_archived_candles(&pool, &symbol, &candles).await {
                    // A later batch that lands moves the symbol's last archived
                    // minute past these, so the next sweep won't refetch them.
                    tracing::error!(symbol, candles = candles.len(), first_t = candles.first().map(|c| c.t), error = %e, "DB candle archive insert failed — these minutes stay missing from the archive");
                    telemetry::db_write_error("candle_archive");
                }
            }
            DbWriteMessage::TelegramMessage { chat_id, chat_title, msg_id, sent_at, text, kind, reply_to, value, instrument, strike, side, edited } => {
                // An edit keeps the first-seen send time and title, and
                // replaces the text and how it was routed.
                let edited_at = edited.then(current_ist_timestamp_string);
                if let Err(e) = sqlx::query(
                    "INSERT INTO telegram_messages
                     (chat_id, chat_title, msg_id, sent_at, text, kind, reply_to, value, edited_at, instrument, strike, side)
                     VALUES (?,?,?,?,?,?,?,?,?,?,?,?)
                     ON CONFLICT(chat_id, msg_id) DO UPDATE SET
                       chat_title = COALESCE(excluded.chat_title, telegram_messages.chat_title),
                       text = excluded.text, kind = excluded.kind, reply_to = excluded.reply_to,
                       value = excluded.value, edited_at = COALESCE(excluded.edited_at, telegram_messages.edited_at),
                       instrument = excluded.instrument, strike = excluded.strike, side = excluded.side",
                )
                .bind(chat_id).bind(&chat_title).bind(msg_id).bind(&sent_at).bind(&text)
                .bind(kind).bind(reply_to).bind(value).bind(&edited_at)
                .bind(&instrument).bind(strike).bind(&side)
                .execute(&pool)
                .await
                {
                    tracing::error!("DB telegram message insert: {e}");
                    telemetry::db_write_error("telegram_message");
                }
            }
        }
    }
}

/// One cached 1-minute bar: `(t, open, high, low, close, volume, source)`.
pub type CandleRow = (i64, f64, f64, f64, f64, Option<f64>, String);

/// `symbol`'s cached minutes from `since` (Unix ms) on, oldest first.
pub async fn load_candle_minutes(pool: &SqlitePool, symbol: &str, since: i64) -> Vec<CandleRow> {
    sqlx::query_as::<_, CandleRow>(
        "SELECT t, open, high, low, close, volume, source FROM candles
         WHERE symbol = ? AND t >= ? ORDER BY t ASC",
    )
    .bind(symbol)
    .bind(since)
    .fetch_all(pool)
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(symbol, error = %e, "Could not read cached candles");
        Vec::new()
    })
}

/// Drop cached minutes from before `before` (Unix ms): only today's are kept.
pub async fn prune_candles(pool: &SqlitePool, before: i64) {
    if let Err(e) = sqlx::query("DELETE FROM candles WHERE t < ?").bind(before).execute(pool).await {
        tracing::warn!(error = %e, "Could not prune cached candles");
    }
}

/// [`prune_candles`] for a spawned task.
pub async fn prune_candles_owned(pool: SqlitePool, before: i64) {
    prune_candles(&pool, before).await;
}

/// One symbol's archived candles in one transaction; a stored minute is
/// replaced by the newer answer for it.
async fn write_archived_candles(pool: &SqlitePool, symbol: &str, candles: &[ArchivedCandle]) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for c in candles {
        sqlx::query(
            "INSERT INTO candle_archive (symbol, t, open, high, low, close, volume, oi)
             VALUES (?,?,?,?,?,?,?,?)
             ON CONFLICT(symbol, t) DO UPDATE SET
               open = excluded.open, high = excluded.high, low = excluded.low,
               close = excluded.close, volume = excluded.volume, oi = excluded.oi",
        )
        .bind(symbol).bind(c.t).bind(c.open).bind(c.high).bind(c.low).bind(c.close).bind(c.volume).bind(c.oi)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// Unix ms of the newest archived minute of `symbol`, if any.
pub async fn last_archived_minute(pool: &SqlitePool, symbol: &str) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar("SELECT MAX(t) FROM candle_archive WHERE symbol = ?")
        .bind(symbol)
        .fetch_one(pool)
        .await
}

// ---------------------------------------------------------------------------
// Order timings (see `order_timing`)
// ---------------------------------------------------------------------------

/// The IST day of a Unix-ms instant, `YYYY-MM-DD`.
fn ist_day_of(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.with_timezone(&shared_domain::ist_offset()).format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// A send: its row, or the send fields on the row its updates already made.
async fn write_order_sent(pool: &SqlitePool, s: &shared_domain::OrderSent) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO order_timings (order_no, day, trading_symbol, side, order_type, qty, price, trigger_price, sent_at_ms, replied_at_ms, error)
         VALUES (?,?,?,?,?,?,?,?,?,?,?)
         ON CONFLICT(order_no) DO UPDATE SET
           day = excluded.day, trading_symbol = excluded.trading_symbol, side = excluded.side,
           order_type = excluded.order_type, qty = excluded.qty, price = excluded.price,
           trigger_price = excluded.trigger_price, sent_at_ms = excluded.sent_at_ms,
           replied_at_ms = excluded.replied_at_ms, error = excluded.error",
    )
    .bind(&s.order_no).bind(ist_day_of(s.sent_at_ms)).bind(&s.trading_symbol).bind(&s.side).bind(&s.order_type)
    .bind(s.qty).bind(s.price).bind(s.trigger).bind(s.sent_at_ms).bind(s.replied_at_ms).bind(&s.error)
    .execute(pool)
    .await
    .map(|_| ())
}

/// Kotak's status words that mean the exchange holds the order.
fn is_accepted(status: &str) -> bool {
    matches!(status, "open" | "trigger pending" | "complete" | "partially filled")
}

/// An update: the latest status, quantity and price, and the first time each
/// stage was seen (accepted, first fill, filled), kept from the first update
/// that showed it.
async fn write_order_update(pool: &SqlitePool, u: &shared_domain::OrderUpdate) -> Result<(), sqlx::Error> {
    let at = u.received_at_ms;
    let accepted = is_accepted(&u.status).then_some(at);
    let first_fill = u.filled_qty.filter(|q| *q > 0).map(|_| at);
    let filled = (u.status == "complete").then_some(at);
    sqlx::query(
        "INSERT INTO order_timings (order_no, day, trading_symbol, side, qty, status, filled_qty, avg_price, reject_reason,
                                    first_update_ms, accepted_at_ms, first_fill_ms, filled_at_ms,
                                    kotak_received_ns, exchange_epoch_s, exchange_confirm, order_time, updated_ms)
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
         ON CONFLICT(order_no) DO UPDATE SET
           trading_symbol = COALESCE(order_timings.trading_symbol, excluded.trading_symbol),
           side = COALESCE(order_timings.side, excluded.side),
           qty = COALESCE(order_timings.qty, excluded.qty),
           status = excluded.status,
           filled_qty = COALESCE(excluded.filled_qty, order_timings.filled_qty),
           avg_price = COALESCE(excluded.avg_price, order_timings.avg_price),
           reject_reason = COALESCE(excluded.reject_reason, order_timings.reject_reason),
           first_update_ms = COALESCE(order_timings.first_update_ms, excluded.first_update_ms),
           accepted_at_ms = COALESCE(order_timings.accepted_at_ms, excluded.accepted_at_ms),
           first_fill_ms = COALESCE(order_timings.first_fill_ms, excluded.first_fill_ms),
           filled_at_ms = COALESCE(order_timings.filled_at_ms, excluded.filled_at_ms),
           kotak_received_ns = COALESCE(excluded.kotak_received_ns, order_timings.kotak_received_ns),
           exchange_epoch_s = COALESCE(excluded.exchange_epoch_s, order_timings.exchange_epoch_s),
           exchange_confirm = COALESCE(excluded.exchange_confirm, order_timings.exchange_confirm),
           order_time = COALESCE(order_timings.order_time, excluded.order_time),
           updated_ms = excluded.updated_ms",
    )
    .bind(&u.order_no).bind(ist_day_of(at)).bind(&u.trading_symbol).bind(&u.side).bind(u.qty)
    .bind(&u.status).bind(u.filled_qty).bind(u.avg_price).bind(&u.reject_reason)
    .bind(at).bind(accepted).bind(first_fill).bind(filled)
    .bind(u.kotak_received_ns).bind(u.exchange_epoch_s).bind(&u.exchange_confirm).bind(&u.order_time).bind(at)
    .execute(pool)
    .await
    .map(|_| ())
}

/// One order's timing row, as the admin's Order timing section reads it.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow, serde::Serialize)]
pub struct OrderTimingRow {
    pub order_no: Option<String>,
    pub trading_symbol: Option<String>,
    pub side: Option<String>,
    pub order_type: Option<String>,
    pub qty: Option<i64>,
    pub price: Option<f64>,
    pub trigger_price: Option<f64>,
    pub sent_at_ms: Option<i64>,
    pub replied_at_ms: Option<i64>,
    pub error: Option<String>,
    pub status: Option<String>,
    pub filled_qty: Option<i64>,
    pub avg_price: Option<f64>,
    pub reject_reason: Option<String>,
    pub first_update_ms: Option<i64>,
    pub accepted_at_ms: Option<i64>,
    pub first_fill_ms: Option<i64>,
    pub filled_at_ms: Option<i64>,
    pub kotak_received_ns: Option<i64>,
    pub exchange_epoch_s: Option<i64>,
    pub exchange_confirm: Option<String>,
    pub order_time: Option<String>,
    /// When the engine's own order-book poll first saw it finished.
    pub seen_at_ms: Option<i64>,
}

/// The day's orders, newest first.
pub async fn load_order_timings(pool: &SqlitePool, day: &str) -> Result<Vec<OrderTimingRow>, sqlx::Error> {
    sqlx::query_as::<_, OrderTimingRow>(
        "SELECT order_no, trading_symbol, side, order_type, qty, price, trigger_price, sent_at_ms, replied_at_ms, error,
                status, filled_qty, avg_price, reject_reason, first_update_ms, accepted_at_ms, first_fill_ms, filled_at_ms,
                kotak_received_ns, exchange_epoch_s, exchange_confirm, order_time, seen_at_ms
         FROM order_timings WHERE day = ?
         ORDER BY COALESCE(sent_at_ms, first_update_ms) DESC, id DESC",
    )
    .bind(day)
    .fetch_all(pool)
    .await
}

// ---------------------------------------------------------------------------
// Users (the phone sign-in's allow-list — see `auth`)
// ---------------------------------------------------------------------------

/// A row of `users`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserRow {
    pub id: i64,
    pub phone: String,
    pub name: Option<String>,
    pub role: String,
    pub enabled: i64,
    pub created_at: String,
    pub last_login_at: Option<String>,
}

impl UserRow {
    pub fn into_record(self) -> crate::auth::UserRecord {
        crate::auth::UserRecord {
            id: self.id,
            phone: self.phone,
            name: self.name,
            role: crate::auth::Role::parse(&self.role),
            enabled: self.enabled != 0,
        }
    }
}

const USER_COLUMNS: &str = "id, phone, name, role, enabled, created_at, last_login_at";

/// Make the owner's phone (`ADMIN_PHONE`) the one admin — run at every
/// start. Its row is put back as an enabled admin whatever it says now, and
/// any other admin row becomes a user: the admin has full power, live orders
/// included, so nobody else may hold it. With no `ADMIN_PHONE`, nobody is admin.
pub async fn seed_admin(pool: &SqlitePool, phone: Option<&str>) {
    if let Some(phone) = phone {
        let done = sqlx::query(
            "INSERT INTO users (phone, name, role, enabled, created_at) VALUES (?, 'Admin', 'admin', 1, ?)
             ON CONFLICT(phone) DO UPDATE SET role = 'admin', enabled = 1",
        )
        .bind(phone)
        .bind(current_ist_timestamp_string())
        .execute(pool)
        .await;
        if let Err(e) = done {
            tracing::error!(error = %e, "could not seed the admin user");
        }
    }
    match sqlx::query("UPDATE users SET role = 'user' WHERE role = 'admin' AND phone IS NOT ?").bind(phone).execute(pool).await {
        Ok(done) if done.rows_affected() > 0 => {
            tracing::warn!(demoted = done.rows_affected(), "Other admin rows made users: only ADMIN_PHONE is admin")
        }
        Ok(_) => {}
        Err(e) => tracing::error!(error = %e, "could not demote the other admin rows"),
    }
}

/// Every user, oldest first.
pub async fn load_users(pool: &SqlitePool) -> sqlx::Result<Vec<UserRow>> {
    sqlx::query_as::<_, UserRow>(&format!("SELECT {USER_COLUMNS} FROM users ORDER BY id ASC")).fetch_all(pool).await
}

pub async fn load_user(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<UserRow>> {
    sqlx::query_as::<_, UserRow>(&format!("SELECT {USER_COLUMNS} FROM users WHERE id = ?")).bind(id).fetch_optional(pool).await
}

#[cfg(test)]
pub(crate) mod test_support {
    /// A throwaway SQLite file, removed (with its WAL files) when dropped.
    pub(crate) struct TempDb(std::path::PathBuf);

    impl TempDb {
        pub(crate) fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("auto-trader-{name}-{}.db", std::process::id()));
            let db = TempDb(path);
            db.remove();
            db
        }

        pub(crate) fn url(&self) -> String {
            format!("sqlite://{}", self.0.display())
        }

        /// The file itself, for `init_catalog_db`.
        pub(crate) fn path(&self) -> &std::path::Path {
            &self.0
        }

        fn remove(&self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
            }
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            self.remove();
        }
    }
}

#[cfg(test)]
mod user_tests {
    use super::test_support::TempDb;
    use super::*;

    #[tokio::test]
    async fn the_admin_is_seeded_once_and_always_put_back_as_an_enabled_admin() {
        let db = TempDb::new("users");
        let pool = init_db(&db.url()).await;
        seed_admin(&pool, Some("7000000001")).await;
        seed_admin(&pool, Some("7000000001")).await;
        let users = load_users(&pool).await.unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!((users[0].phone.as_str(), users[0].role.as_str(), users[0].enabled), ("7000000001", "admin", 1));

        // Demoted and disabled by hand, the next start repairs the row.
        sqlx::query("UPDATE users SET role = 'user', enabled = 0, name = 'Om' WHERE phone = '7000000001'").execute(&pool).await.unwrap();
        seed_admin(&pool, Some("7000000001")).await;
        let row = load_user(&pool, users[0].id).await.unwrap().unwrap();
        assert_eq!((row.role.as_str(), row.enabled, row.name.as_deref()), ("admin", 1, Some("Om")));
        let record = row.into_record();
        assert_eq!(record.role, crate::auth::Role::Admin);
        assert!(record.enabled);
        assert!(load_user(&pool, 999).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn only_the_admin_phone_stays_an_admin() {
        let db = TempDb::new("one-admin");
        let pool = init_db(&db.url()).await;
        sqlx::query("INSERT INTO users (phone, role, enabled, created_at) VALUES ('9876543210', 'admin', 1, 'x'), ('9876543211', 'user', 1, 'x')")
            .execute(&pool)
            .await
            .unwrap();
        seed_admin(&pool, Some("7000000001")).await;
        let roles = |users: Vec<UserRow>| users.into_iter().map(|u| (u.phone, u.role)).collect::<Vec<_>>();
        assert_eq!(
            roles(load_users(&pool).await.unwrap()),
            [("9876543210".into(), "user".into()), ("9876543211".into(), "user".into()), ("7000000001".into(), "admin".into())],
        );
        // Without ADMIN_PHONE nobody is admin.
        seed_admin(&pool, None).await;
        assert!(load_users(&pool).await.unwrap().iter().all(|u| u.role == "user"));
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::TempDb;
    use super::*;
    use shared_domain::entry_gate::{Multipliers, TopupMode};

    #[tokio::test]
    async fn a_fresh_database_starts_with_the_three_indicator_tiers() {
        let db = TempDb::new("fresh");
        let pool = init_db(&db.url()).await;
        let cfg = load_config_from_db(&pool).await;
        assert_eq!(cfg.indicator_multipliers(), Multipliers { strong: 1.5, agree: 1.0, neutral: 0.5 });
        assert_eq!(cfg.topup_mode(), TopupMode::StrongToAgree);
        assert!(!cfg.indicator_sizing && !cfg.indicator_topup);
        pool.close().await;
    }

    #[tokio::test]
    async fn upgrading_keeps_the_saved_multipliers_and_adds_the_strong_tier() {
        let db = TempDb::new("upgrade");
        let pool = init_db(&db.url()).await;
        // Put the table back to how it looked before the strong-agree tier,
        // with the settings a running install would have saved.
        for sql in [
            "ALTER TABLE trading_config DROP COLUMN indicator_strong_agree_multiplier",
            "ALTER TABLE trading_config DROP COLUMN indicator_topup_mode",
            "UPDATE trading_config SET indicator_sizing = 1, indicator_topup = 1,
                    indicator_agree_multiplier = 1.5, indicator_neutral_multiplier = 0.4",
        ] {
            sqlx::query(sql).execute(&pool).await.unwrap();
        }
        pool.close().await;

        let pool = init_db(&db.url()).await;
        let cfg = load_config_from_db(&pool).await;
        // Nothing saved is rewritten: a plain agree keeps buying what it did.
        assert_eq!(cfg.indicator_multipliers(), Multipliers { strong: 1.5, agree: 1.5, neutral: 0.4 });
        assert_eq!(cfg.topup_mode(), TopupMode::StrongToAgree);
        assert!(cfg.indicator_sizing && cfg.indicator_topup);
        pool.close().await;
    }

    #[tokio::test]
    async fn telegram_resume_round_trips_and_clears() {
        let db = TempDb::new("tg-resume");
        let pool = init_db(&db.url()).await;
        assert!(load_telegram_resume(&pool).await.is_none());

        save_telegram_resume(&pool, 123, "hash-a", &[-1001, -1002]).await;
        // A later Start replaces the row, it never adds a second one.
        save_telegram_resume(&pool, 456, "hash-b", &[-1003]).await;
        let saved = load_telegram_resume(&pool).await.expect("saved");
        assert_eq!((saved.api_id, saved.api_hash.as_str(), saved.chat_ids), (456, "hash-b", vec![-1003]));

        clear_telegram_resume(&pool).await;
        assert!(load_telegram_resume(&pool).await.is_none());
        pool.close().await;
    }

    #[tokio::test]
    async fn a_trade_row_keeps_its_call_chat_side_and_strike() {
        let db = TempDb::new("trade-tag");
        let pool = init_db(&db.url()).await;
        let (tx, rx) = mpsc::channel(4);
        let writer = tokio::spawn(db_writer(rx, pool.clone(), Arc::new(BalanceCache::new())));
        let tag = shared_domain::TradeTag {
            signal_id: Some("27962".into()),
            raw_message: Some("BUY NIFTY 25000 CE ABOVE 120".into()),
            chat_id: Some(-1001),
            chat_title: Some("Alpha Calls".into()),
            option_type: Some("CE".into()),
            strike: Some(25_000.0),
        };
        tx.send(DbWriteMessage::Trade {
            ticker: "NIFTY".into(), action: "BUY".into(), qty: 65, executed_price: 120.0,
            gross_value: 7_800.0, brokerage: 20.0, stt_charge: 0.0, sebi_fee: 0.0078,
            stamp_duty: 0.234, transaction_charge: 2.73, gst: 4.09, net_value: 7_827.06,
            tag, exit_reason: Some("ENTRY".into()), mode: "PAPER".into(),
        }).await.unwrap();
        drop(tx);
        writer.await.unwrap();

        let row: (Option<i64>, Option<String>, Option<String>, Option<f64>, Option<String>) = sqlx::query_as(
            "SELECT chat_id, chat_title, option_type, strike, signal_id FROM paper_trades",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row, (Some(-1001), Some("Alpha Calls".into()), Some("CE".into()), Some(25_000.0), Some("27962".into())));
        pool.close().await;
    }

    #[tokio::test]
    async fn archived_candles_keep_their_volume_and_a_newer_answer_replaces_a_minute() {
        let db = TempDb::new("candle-archive");
        let pool = init_db(&db.url()).await;
        assert_eq!(last_archived_minute(&pool, "NIFTY:FUT:2026-11-24").await.unwrap(), None);

        let candle = |t: i64, close: f64, volume: f64| ArchivedCandle {
            t, open: 25_000.0, high: 25_010.0, low: 24_990.0, close, volume, oi: 1_500_000.0,
        };
        let (tx, rx) = mpsc::channel(4);
        let writer = tokio::spawn(db_writer(rx, pool.clone(), Arc::new(BalanceCache::new())));
        tx.send(DbWriteMessage::ArchiveCandles {
            symbol: "NIFTY:FUT:2026-11-24".into(),
            candles: vec![candle(60_000, 25_005.0, 1_200.0), candle(120_000, 25_007.0, 800.0)],
        }).await.unwrap();
        // The next sweep fetches the same day again: a minute is replaced,
        // never doubled.
        tx.send(DbWriteMessage::ArchiveCandles {
            symbol: "NIFTY:FUT:2026-11-24".into(),
            candles: vec![candle(120_000, 25_008.0, 950.0)],
        }).await.unwrap();
        tx.send(DbWriteMessage::ArchiveCandles { symbol: "NIFTY".into(), candles: vec![candle(180_000, 24_950.0, 0.0)] })
            .await
            .unwrap();
        drop(tx);
        writer.await.unwrap();

        let rows: Vec<(i64, f64, f64, f64)> = sqlx::query_as(
            "SELECT t, close, volume, oi FROM candle_archive WHERE symbol = 'NIFTY:FUT:2026-11-24' ORDER BY t",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows, [(60_000, 25_005.0, 1_200.0, 1_500_000.0), (120_000, 25_008.0, 950.0, 1_500_000.0)]);
        assert_eq!(last_archived_minute(&pool, "NIFTY:FUT:2026-11-24").await.unwrap(), Some(120_000));
        assert_eq!(last_archived_minute(&pool, "NIFTY").await.unwrap(), Some(180_000));
        pool.close().await;
    }

    #[tokio::test]
    async fn feature_flags_start_off_and_survive_a_restart_and_a_settings_save() {
        let db = TempDb::new("flags");
        let pool = init_db(&db.url()).await;
        assert_eq!(load_feature_flags(&pool).await, FeatureFlags::default());

        save_feature_flags(&pool, &[("index_options_only", true)]).await.unwrap();
        // What v1's settings save does to its own table never touches them.
        sqlx::query("UPDATE trading_config SET mode = 'PAPER', other_lots = 1 WHERE id = 1")
            .execute(&pool).await.unwrap();
        // A row from a newer build is ignored, not an error.
        sqlx::query("INSERT INTO feature_flags (name, enabled, updated_at) VALUES ('from_the_future', 1, 'x')")
            .execute(&pool).await.unwrap();
        pool.close().await;

        let pool = init_db(&db.url()).await;
        assert_eq!(
            load_feature_flags(&pool).await,
            FeatureFlags { index_options_only: true, chart_stops: false, search: false, watchlists: false },
        );
        save_feature_flags(&pool, &[("index_options_only", false), ("chart_stops", true), ("search", true), ("watchlists", true)]).await.unwrap();
        assert_eq!(
            load_feature_flags(&pool).await,
            FeatureFlags { index_options_only: false, chart_stops: true, search: true, watchlists: true },
        );
        pool.close().await;
    }

    #[tokio::test]
    async fn special_sessions_save_load_delete_and_cutoff() {
        let db = TempDb::new("special-sessions");
        let pool = init_db(&db.url()).await;
        assert_eq!(load_special_sessions(&pool).await.unwrap(), vec![]);

        let today = shared_domain::today_ist();
        let session = shared_domain::SpecialSession {
            date: today,
            start: 14 * 60 + 20,
            end: 15 * 60 + 30,
        };
        save_special_session(&pool, &session).await.unwrap();

        // Also add an older one (> 7 days ago)
        let old_date = today - chrono::Duration::days(10);
        sqlx::query(
            "INSERT INTO special_sessions (date, start, end, created_at) VALUES (?, '10:00', '11:00', 'x')",
        )
        .bind(old_date.format("%Y-%m-%d").to_string())
        .execute(&pool)
        .await
        .unwrap();

        // Old session ignored by load_special_sessions
        let loaded = load_special_sessions(&pool).await.unwrap();
        assert_eq!(loaded, vec![session]);

        // Delete session
        let deleted = delete_special_session(&pool, &today.format("%Y-%m-%d").to_string()).await.unwrap();
        assert!(deleted);
        assert_eq!(load_special_sessions(&pool).await.unwrap(), vec![]);

        pool.close().await;
    }
}

#[cfg(test)]
mod order_timing_tests {
    use super::test_support::TempDb;
    use super::*;
    use shared_domain::{OrderSent, OrderUpdate};

    /// 2026-11-09 10:15:30 IST.
    const T0: i64 = 1_794_199_530_000;

    fn sent(order_no: Option<&str>, at: i64, error: Option<&str>) -> OrderSent {
        OrderSent {
            sent_at_ms: at,
            replied_at_ms: at + 180,
            order_no: order_no.map(str::to_string),
            error: error.map(str::to_string),
            trading_symbol: "NIFTY26N2425000CE".into(),
            side: "B".into(),
            order_type: "MKT".into(),
            qty: 75,
            price: 0.0,
            trigger: 0.0,
        }
    }

    fn update(order_no: &str, at: i64, status: &str, filled: i64, avg: Option<f64>) -> OrderUpdate {
        OrderUpdate {
            received_at_ms: at,
            order_no: order_no.into(),
            status: status.into(),
            filled_qty: Some(filled),
            avg_price: avg,
            exchange_epoch_s: Some(at / 1000),
            ..OrderUpdate::default()
        }
    }

    #[tokio::test]
    async fn sends_and_pushed_updates_merge_by_order_number_whichever_lands_first() {
        let db = TempDb::new("order-timings");
        let pool = init_db(&db.url()).await;
        let (tx, rx) = mpsc::channel(16);
        let writer = tokio::spawn(db_writer(rx, pool.clone(), Arc::new(BalanceCache::new())));
        for msg in [
            // Order 1: the send is recorded first, then open, a part fill, the rest.
            DbWriteMessage::OrderSent(sent(Some("1"), T0, None)),
            DbWriteMessage::OrderUpdate(update("1", T0 + 150, "open", 0, None)),
            DbWriteMessage::OrderUpdate(update("1", T0 + 300, "open", 25, Some(120.0))),
            DbWriteMessage::OrderUpdate(update("1", T0 + 420, "complete", 75, Some(120.5))),
            // The engine's poll saw it finished; a later sighting doesn't move it.
            DbWriteMessage::OrderSeen { order_no: "1".into(), at_ms: T0 + 1_200 },
            DbWriteMessage::OrderSeen { order_no: "1".into(), at_ms: T0 + 3_200 },
            // An order the table never had stays out of it.
            DbWriteMessage::OrderSeen { order_no: "99".into(), at_ms: T0 + 1_300 },
            // Order 2: Kotak's push beats the send tap to the writer.
            DbWriteMessage::OrderUpdate(update("2", T0 + 5_250, "complete", 75, Some(99.0))),
            DbWriteMessage::OrderSent(sent(Some("2"), T0 + 5_000, None)),
            // A send Kotak refused: no number, a row of its own.
            DbWriteMessage::OrderSent(sent(None, T0 + 9_000, Some("RMS: margin exceeds"))),
        ] {
            tx.send(msg).await.unwrap();
        }
        drop(tx);
        writer.await.unwrap();

        let rows = load_order_timings(&pool, "2026-11-09").await.unwrap();
        assert_eq!(rows.len(), 3);
        // Newest first.
        assert_eq!((rows[0].order_no.as_deref(), rows[0].error.as_deref()), (None, Some("RMS: margin exceeds")));

        let two = &rows[1];
        assert_eq!((two.sent_at_ms, two.replied_at_ms, two.filled_at_ms), (Some(T0 + 5_000), Some(T0 + 5_180), Some(T0 + 5_250)));
        assert_eq!((two.trading_symbol.as_deref(), two.status.as_deref(), two.avg_price), (Some("NIFTY26N2425000CE"), Some("complete"), Some(99.0)));

        let one = &rows[2];
        assert_eq!((one.sent_at_ms, one.replied_at_ms), (Some(T0), Some(T0 + 180)));
        assert_eq!((one.first_update_ms, one.accepted_at_ms, one.first_fill_ms, one.filled_at_ms), (Some(T0 + 150), Some(T0 + 150), Some(T0 + 300), Some(T0 + 420)));
        assert_eq!((one.status.as_deref(), one.filled_qty, one.avg_price, one.qty), (Some("complete"), Some(75), Some(120.5), Some(75)));
        assert_eq!(one.exchange_epoch_s, Some((T0 + 420) / 1000));
        assert_eq!((one.seen_at_ms, two.seen_at_ms), (Some(T0 + 1_200), None));
        assert!(load_order_timings(&pool, "2026-11-08").await.unwrap().is_empty());
        pool.close().await;
    }
}
