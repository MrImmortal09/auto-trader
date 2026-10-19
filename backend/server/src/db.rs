use shared_domain::{current_ist_timestamp_string, DbWriteMessage, MonitoredPosition};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};
use shared_domain::{telemetry, TradingConfig};
use std::str::FromStr;
use tokio::sync::mpsc;

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
    let _ = sqlx::query("DELETE FROM oi_snapshots WHERE ts < ?").bind(cutoff).execute(&pool).await;

    pool
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

// ---------------------------------------------------------------------------
// Sequential writer task
// ---------------------------------------------------------------------------

/// Dedicated SQLite writer — processes one `DbWriteMessage` at a time,
/// eliminating "database is locked" errors from concurrent writers.
pub async fn db_writer(mut rx: mpsc::Receiver<DbWriteMessage>, pool: SqlitePool) {
    while let Some(msg) = rx.recv().await {
        let _timer = telemetry::DbWriteTimer::start(msg.metric_kind());
        match msg {
            DbWriteMessage::Trade {
                ticker, action, qty, executed_price,
                gross_value, brokerage, stt_charge, sebi_fee,
                stamp_duty, transaction_charge, gst, net_value,
                signal_id, raw_message, exit_reason, mode,
            } => {
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
                      signal_id, raw_message, exit_reason, mode)
                     VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                )
                 .bind(&ticker).bind(&action).bind(qty as i64).bind(executed_price).bind(&timestamp)
                .bind(gross_value).bind(brokerage).bind(stt_charge).bind(sebi_fee)
                .bind(stamp_duty).bind(transaction_charge).bind(gst).bind(net_value)
                .bind(&signal_id).bind(&raw_message).bind(&exit_reason).bind(&mode)
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_domain::entry_gate::{Multipliers, TopupMode};

    /// A throwaway SQLite file, removed (with its WAL files) when dropped.
    struct TempDb(std::path::PathBuf);

    impl TempDb {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("auto-trader-{name}-{}.db", std::process::id()));
            let db = TempDb(path);
            db.remove();
            db
        }

        fn url(&self) -> String {
            format!("sqlite://{}", self.0.display())
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
}
