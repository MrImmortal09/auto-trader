//! `/api/admin/data`: what the server's database keeps, for the Admin page's
//! Stored data section — each kept table's rows, the span they cover and how
//! long they stay, and the candle archive's coverage per symbol. Read-only.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::DateTime;
use serde::Serialize;
use serde_json::json;
use shared_domain::INDEX_NAMES;
use sqlx::SqlitePool;

use crate::AppState;

/// Where a table keeps the time of each row.
#[derive(Clone, Copy)]
enum Time {
    /// IST `YYYY-MM-DD HH:MM:SS` text.
    Text(&'static str),
    /// Unix ms.
    Millis(&'static str),
}

/// A table the section lists, and how long its rows stay.
struct Kept {
    table: &'static str,
    label: &'static str,
    what: &'static str,
    time: Time,
    kept: &'static str,
}

const KEPT: [Kept; 7] = [
    Kept {
        table: "candle_archive",
        label: "Candle archive",
        what: "1-minute candles of the indices and their futures, with volume and OI",
        time: Time::Millis("t"),
        kept: "Forever",
    },
    Kept {
        table: "paper_trades",
        label: "Owner's trades",
        what: "Every fill, paper and LIVE, with its charges and the call it came from",
        time: Time::Text("timestamp"),
        kept: "Forever",
    },
    Kept {
        table: "user_trades",
        label: "Users' paper trades",
        what: "Every user's fills, resets included; removing a user deletes theirs",
        time: Time::Text("timestamp"),
        kept: "Forever",
    },
    Kept {
        table: "system_logs",
        label: "Logs",
        what: "The engine's log lines",
        time: Time::Text("timestamp"),
        kept: "Forever",
    },
    Kept {
        table: "oi_snapshots",
        label: "Option-chain snapshots",
        what: "OI and its change, spot, and the future's price and VWAP, once a minute",
        time: Time::Text("ts"),
        kept: "30 days",
    },
    Kept {
        table: "telegram_messages",
        label: "Telegram messages",
        what: "Every message from the monitored chats",
        time: Time::Text("sent_at"),
        kept: "30 days",
    },
    Kept {
        table: "candles",
        label: "Chart candles",
        what: "Today's 1-minute bars of what the charts opened",
        time: Time::Millis("t"),
        kept: "Today",
    },
];

#[derive(Debug, Serialize, PartialEq)]
pub struct StoredData {
    /// The database file in bytes, its write-ahead log aside.
    pub db_bytes: i64,
    pub tables: Vec<TableOut>,
    /// Indices first, in the usual order, each followed by its futures by
    /// expiry.
    pub archive: Vec<ArchivedSymbol>,
    /// Upstox is configured, so the archive sweep runs.
    pub archiving: bool,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct TableOut {
    pub table: &'static str,
    pub label: &'static str,
    pub what: &'static str,
    pub kept: &'static str,
    pub rows: i64,
    /// IST `YYYY-MM-DD HH:MM:SS` of the oldest and newest row.
    pub first: Option<String>,
    pub last: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct ArchivedSymbol {
    /// `NIFTY`, or a future: `NIFTY:FUT:2026-11-24`.
    pub symbol: String,
    pub index: String,
    /// A future's expiry; none for the index.
    pub expiry: Option<String>,
    /// IST days of the oldest and newest candle.
    pub first_day: String,
    pub last_day: String,
    /// IST days with any candle.
    pub days: i64,
    pub candles: i64,
    /// Candles that traded (an index never does).
    pub with_volume: i64,
}

/// IST `YYYY-MM-DD HH:MM:SS` of a Unix-ms instant.
fn ist_text(ms: i64) -> Option<String> {
    Some(
        DateTime::from_timestamp_millis(ms)?
            .with_timezone(&shared_domain::ist_offset())
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
    )
}

async fn table(pool: &SqlitePool, k: &Kept) -> Result<TableOut, sqlx::Error> {
    // Names come from `KEPT`, never from the request.
    let (rows, first, last) = match k.time {
        Time::Text(col) => {
            sqlx::query_as::<_, (i64, Option<String>, Option<String>)>(&format!(
                "SELECT COUNT(*), MIN({col}), MAX({col}) FROM {}",
                k.table
            ))
            .fetch_one(pool)
            .await?
        }
        Time::Millis(col) => {
            let (rows, first, last) = sqlx::query_as::<_, (i64, Option<i64>, Option<i64>)>(&format!(
                "SELECT COUNT(*), MIN({col}), MAX({col}) FROM {}",
                k.table
            ))
            .fetch_one(pool)
            .await?;
            (rows, first.and_then(ist_text), last.and_then(ist_text))
        }
    };
    Ok(TableOut { table: k.table, label: k.label, what: k.what, kept: k.kept, rows, first, last })
}

async fn archive(pool: &SqlitePool) -> Result<Vec<ArchivedSymbol>, sqlx::Error> {
    let ist_ms = i64::from(shared_domain::IST_OFFSET_SECS) * 1000;
    let rows = sqlx::query_as::<_, (String, i64, i64, i64, i64, i64)>(
        "SELECT symbol, MIN(t), MAX(t), COUNT(*), SUM(volume > 0), COUNT(DISTINCT (t + ?) / 86400000)
         FROM candle_archive GROUP BY symbol",
    )
    .bind(ist_ms)
    .fetch_all(pool)
    .await?;
    let day = |ms: i64| ist_text(ms).map(|t| t[..10].to_string()).unwrap_or_default();
    let mut out: Vec<ArchivedSymbol> = rows
        .into_iter()
        .map(|(symbol, first, last, candles, with_volume, days)| {
            let mut parts = symbol.splitn(3, ':');
            let index = parts.next().unwrap_or_default().to_string();
            let expiry = match (parts.next(), parts.next()) {
                (Some("FUT"), Some(expiry)) => Some(expiry.to_string()),
                _ => None,
            };
            ArchivedSymbol { symbol, index, expiry, first_day: day(first), last_day: day(last), days, candles, with_volume }
        })
        .collect();
    let rank = |index: &str| INDEX_NAMES.iter().position(|i| *i == index).unwrap_or(INDEX_NAMES.len());
    out.sort_by(|a, b| (rank(&a.index), &a.index, &a.expiry).cmp(&(rank(&b.index), &b.index, &b.expiry)));
    Ok(out)
}

pub async fn stored_data(pool: &SqlitePool, archiving: bool) -> Result<StoredData, sqlx::Error> {
    let page_count: i64 = sqlx::query_scalar("PRAGMA page_count").fetch_one(pool).await?;
    let page_size: i64 = sqlx::query_scalar("PRAGMA page_size").fetch_one(pool).await?;
    let mut tables = Vec::with_capacity(KEPT.len());
    for k in &KEPT {
        tables.push(table(pool, k).await?);
    }
    Ok(StoredData { db_bytes: page_count * page_size, tables, archive: archive(pool).await?, archiving })
}

/// `GET /api/admin/data`.
pub async fn data_handler(State(state): State<AppState>) -> Response {
    match stored_data(&state.db_pool, state.upstox.is_some()).await {
        Ok(data) => Json(data).into_response(),
        Err(e) => {
            tracing::error!("admin data: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "could not read the database" }))).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{init_db, test_support::TempDb};

    #[tokio::test]
    async fn every_kept_table_is_counted_and_the_archive_is_listed_index_first() {
        let db = TempDb::new("admin-data");
        let pool = init_db(&db.url()).await;
        // 2026-11-06 09:15 and 15:39 IST, and 2026-11-05 09:15 IST.
        let (fri_open, fri_last, thu_open) = (1_793_936_700_000_i64, 1_793_959_740_000_i64, 1_793_850_300_000_i64);
        for (symbol, t, volume) in [
            ("SENSEX", fri_open, 0.0),
            ("NIFTY:FUT:2026-12-29", fri_open, 50.0),
            ("NIFTY:FUT:2026-11-24", thu_open, 1_200.0),
            ("NIFTY:FUT:2026-11-24", fri_open, 0.0),
            ("NIFTY:FUT:2026-11-24", fri_last, 900.0),
            ("NIFTY", fri_open, 0.0),
        ] {
            sqlx::query("INSERT INTO candle_archive VALUES (?, ?, 1, 1, 1, 1, ?, 0)")
                .bind(symbol)
                .bind(t)
                .bind(volume)
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO system_logs (level, message, timestamp) VALUES ('INFO', 'a', '2026-11-05 10:00:00'), ('INFO', 'b', '2026-11-06 11:00:00')")
            .execute(&pool)
            .await
            .unwrap();

        let data = stored_data(&pool, true).await.unwrap();
        assert!(data.archiving && data.db_bytes > 0);

        let archive = data.tables.iter().find(|t| t.table == "candle_archive").unwrap();
        assert_eq!(
            (archive.rows, archive.first.as_deref(), archive.last.as_deref()),
            (6, Some("2026-11-05 09:15:00"), Some("2026-11-06 15:39:00")),
        );
        let logs = data.tables.iter().find(|t| t.table == "system_logs").unwrap();
        assert_eq!((logs.rows, logs.first.as_deref(), logs.last.as_deref()), (2, Some("2026-11-05 10:00:00"), Some("2026-11-06 11:00:00")));
        let telegram = data.tables.iter().find(|t| t.table == "telegram_messages").unwrap();
        assert_eq!((telegram.rows, telegram.first.as_deref()), (0, None));
        assert_eq!(data.tables.len(), KEPT.len());

        let order: Vec<&str> = data.archive.iter().map(|s| s.symbol.as_str()).collect();
        assert_eq!(order, ["NIFTY", "NIFTY:FUT:2026-11-24", "NIFTY:FUT:2026-12-29", "SENSEX"]);
        assert_eq!(
            data.archive[1],
            ArchivedSymbol {
                symbol: "NIFTY:FUT:2026-11-24".into(),
                index: "NIFTY".into(),
                expiry: Some("2026-11-24".into()),
                first_day: "2026-11-05".into(),
                last_day: "2026-11-06".into(),
                days: 2,
                candles: 3,
                with_volume: 2,
            },
        );
        assert_eq!((data.archive[0].expiry.as_deref(), data.archive[0].with_volume), (None, 0));
        pool.close().await;
    }
}
