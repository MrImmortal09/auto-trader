use shared_domain::{current_ist_timestamp_string, DbWriteMessage, MonitoredPosition};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};
use shared_domain::TradingConfig;
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
            target_2_exit_pct REAL NOT NULL DEFAULT 100.0
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
}

/// Load `TradingConfig` from SQLite, falling back to safe defaults.
pub async fn load_config_from_db(pool: &SqlitePool) -> TradingConfig {
    sqlx::query_as::<_, TradingConfigRow>(
        "SELECT max_trade_amount_inr, index_lots, other_lots, mode, brokerage_per_order,
                target_1_exit_pct, target_2_exit_pct
         FROM trading_config WHERE id = 1",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .map(|r| TradingConfig {
        max_trade_amount_inr: r.max_trade_amount_inr,
        index_lots: r.index_lots.max(1),
        other_lots: r.other_lots.max(1),
        mode: r.mode,
        brokerage_per_order: r.brokerage_per_order,
        target_1_exit_pct: r.target_1_exit_pct,
        target_2_exit_pct: r.target_2_exit_pct,
    })
    .unwrap_or_else(|| TradingConfig {
        max_trade_amount_inr: 10_000.0,
        index_lots: 1,
        other_lots: 1,
        mode: "PAPER".into(),
        brokerage_per_order: 20.0,
        target_1_exit_pct: 50.0,
        target_2_exit_pct: 100.0,
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
    pub updated_at: String,
}

pub async fn save_kotak_session(pool: &SqlitePool, access: &str, auth: &str, sid: &str, base_url: &str) {
    sqlx::query(
        "INSERT INTO kotak_session (id, access_token, auth_token, sid, base_url, updated_at) 
         VALUES (1, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET 
         access_token = excluded.access_token,
         auth_token = excluded.auth_token,
         sid = excluded.sid,
         base_url = excluded.base_url,
         updated_at = excluded.updated_at"
    )
    .bind(access)
    .bind(auth)
    .bind(sid)
    .bind(base_url)
    .bind(shared_domain::current_ist_timestamp_string())
    .execute(pool)
    .await
    .unwrap();
}

pub async fn load_kotak_session(pool: &SqlitePool) -> Option<KotakSessionRow> {
    let row = sqlx::query_as::<_, (String, String, String, String, String)>(
        "SELECT access_token, auth_token, sid, base_url, updated_at FROM kotak_session WHERE id = 1",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;

    let (access_token, auth_token, sid, base_url, updated_at) = row;
    
    // Check if the session is from today in IST. If not, it's expired.
    if !updated_at.starts_with(&shared_domain::now_ist().format("%Y-%m-%d").to_string()) {
        return None;
    }

    Some(KotakSessionRow {
        access_token,
        auth_token,
        sid,
        base_url,
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
        match msg {
            DbWriteMessage::Trade {
                ticker, action, qty, executed_price,
                gross_value, brokerage, stt_charge, sebi_fee,
                stamp_duty, transaction_charge, gst, net_value,
            } => {
                let timestamp = current_ist_timestamp_string();
                let mut tx = match pool.begin().await {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::error!("DB begin tx: {e}");
                        continue;
                    }
                };

                if let Err(e) = sqlx::query(
                    "INSERT INTO paper_trades
                     (ticker, action, qty, executed_price, timestamp,
                      gross_value, brokerage, stt_charge, sebi_fee,
                      stamp_duty, transaction_charge, gst, net_value)
                     VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
                )
                 .bind(&ticker).bind(&action).bind(qty as i64).bind(executed_price).bind(&timestamp)
                .bind(gross_value).bind(brokerage).bind(stt_charge).bind(sebi_fee)
                .bind(stamp_duty).bind(transaction_charge).bind(gst).bind(net_value)
                .execute(&mut *tx).await
                {
                    tracing::error!("DB trade insert: {e}");
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
                    let _ = tx.rollback().await;
                    continue;
                }

                if let Err(e) = tx.commit().await {
                    tracing::error!("DB commit tx: {e}");
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
                }
            }
        }
    }
}
