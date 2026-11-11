//! Each signed-in person's watchlists (`/api/v2/watchlists`): up to
//! [`MAX_LISTS`] named lists of up to [`MAX_ITEMS`] price-map keys each,
//! kept in `user_watchlists` and `user_watchlist_items` in trades.db by the
//! phone sign-in's user id — the admin is a `users` row like anyone else, so
//! the owner has lists of their own, keyed the same way.
//!
//! This is the store alone: rows in, rows out. It knows nothing of the
//! catalog or the quote hub (`routes::v2::watchlists` checks keys against
//! the catalog and prices the rows), nothing of the feed, the bridge or the
//! engine, and no row here is ever read by them.
//!
//! Writes go straight to the pool in short transactions, as the paper
//! desk's user tables do, never through the engine's `db_writer`. Each one
//! is a `BEGIN IMMEDIATE` ([`write_tx`]): it takes the database's write
//! lock first and reads under it, so a transaction that looks before it
//! writes (the ownership check, a position) waits its turn behind another
//! connection's write instead of failing with `SQLITE_BUSY` when its read
//! snapshot has gone stale. Every change is scoped to the caller's user id
//! in the SQL itself, so a list id that belongs to someone else is simply
//! "not found". The caps are enforced where the row is written: each
//! insert is conditional on the count under it, and inserts nothing when
//! the cap is reached (`rows_affected == 0`), so two requests at once can
//! never add a sixth list or a fifty-first item between them.

use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};

/// Lists one person may keep.
pub const MAX_LISTS: usize = 5;
/// Keys one list may hold.
pub const MAX_ITEMS: usize = 50;
/// The longest list name kept, in characters.
pub const MAX_NAME_CHARS: usize = 40;

/// The name of the list every person starts with.
pub const DEFAULT_NAME: &str = "Watchlist 1";

/// A row of `user_watchlists`.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct ListRow {
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    /// 0-based, dense: the lists of one person are numbered 0..n.
    pub position: i64,
    /// IST `YYYY-MM-DD HH:MM:SS`.
    pub created_at: String,
}

/// A row of `user_watchlist_items`.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct ItemRow {
    pub watchlist_id: i64,
    /// The price-map key (`nse_cm|2885`, `idx|NIFTY 50`).
    pub key: String,
    /// 0-based, dense within the list.
    pub position: i64,
    /// IST `YYYY-MM-DD HH:MM:SS`.
    pub added_at: String,
}

/// Why a write did not happen.
#[derive(Debug)]
pub enum Error {
    /// No list with that id belongs to the caller.
    NotFound,
    /// The caller already keeps [`MAX_LISTS`] lists.
    TooManyLists,
    /// The list already holds [`MAX_ITEMS`] keys.
    TooManyItems,
    /// A `position` past the caller's last list.
    BadPosition,
    /// The caller's only list: everyone keeps at least one.
    LastList,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        Error::Db(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotFound => write!(f, "no such watchlist"),
            Error::TooManyLists => write!(f, "at most {MAX_LISTS} watchlists"),
            Error::TooManyItems => write!(f, "at most {MAX_ITEMS} instruments in a watchlist"),
            Error::BadPosition => write!(f, "position is past the last watchlist"),
            Error::LastList => write!(f, "the last watchlist cannot be deleted"),
            Error::Db(e) => write!(f, "{e}"),
        }
    }
}

const LIST_COLUMNS: &str = "id, user_id, name, position, created_at";
const ITEM_COLUMNS: &str = "watchlist_id, key, position, added_at";

/// Adds a list for `user_id` at the end, unless they already keep
/// [`MAX_LISTS`]: the insert itself checks the count, so nothing is
/// written past the cap however many requests arrive at once.
const INSERT_LIST: &str = "INSERT INTO user_watchlists (user_id, name, position, created_at)
    SELECT ?1, ?2, COALESCE((SELECT MAX(position) + 1 FROM user_watchlists WHERE user_id = ?1), 0), ?3
    WHERE (SELECT COUNT(*) FROM user_watchlists WHERE user_id = ?1) < ?4";

/// Adds `key` to a list at the end, unless the list is full or already
/// holds it: one statement, so the cap holds under concurrent adds.
const INSERT_ITEM: &str = "INSERT INTO user_watchlist_items (watchlist_id, key, position, added_at)
    SELECT ?1, ?2, COALESCE((SELECT MAX(position) + 1 FROM user_watchlist_items WHERE watchlist_id = ?1), 0), ?3
    WHERE (SELECT COUNT(*) FROM user_watchlist_items WHERE watchlist_id = ?1) < ?4
      AND NOT EXISTS (SELECT 1 FROM user_watchlist_items WHERE watchlist_id = ?1 AND key = ?2)";

/// A write transaction on `pool`: `BEGIN IMMEDIATE`, so the connection
/// holds the write lock from the start. A deferred transaction (`BEGIN`)
/// that reads first and writes after can find, on the write, that another
/// connection of the pool committed in between: SQLite then refuses the
/// upgrade at once (`SQLITE_BUSY`, "database is locked") without waiting
/// out `busy_timeout`. Taking the lock up front makes the wait happen at
/// `BEGIN`, where the busy handler applies, and the reads inside see what
/// the write will.
async fn write_tx(pool: &SqlitePool) -> sqlx::Result<Transaction<'static, Sqlite>> {
    pool.begin_with("BEGIN IMMEDIATE").await
}

/// The keys every person's first list starts with: NIFTY, BANKNIFTY and
/// SENSEX, under the keys their spot ticks on (`idx|NIFTY 50`, …).
pub fn default_keys() -> Vec<String> {
    ["NIFTY", "BANKNIFTY", "SENSEX"]
        .into_iter()
        .filter_map(shared_domain::kotak_index_feed)
        .map(|(_, feed_name)| shared_domain::index_price_key(feed_name))
        .collect()
}

/// `user_id`'s lists, in order.
pub async fn lists(pool: &SqlitePool, user_id: i64) -> sqlx::Result<Vec<ListRow>> {
    sqlx::query_as::<_, ListRow>(&format!("SELECT {LIST_COLUMNS} FROM user_watchlists WHERE user_id = ? ORDER BY position, id"))
        .bind(user_id)
        .fetch_all(pool)
        .await
}

/// One of `user_id`'s lists; `None` when no list with that id is theirs.
pub async fn list(pool: &SqlitePool, user_id: i64, id: i64) -> sqlx::Result<Option<ListRow>> {
    sqlx::query_as::<_, ListRow>(&format!("SELECT {LIST_COLUMNS} FROM user_watchlists WHERE id = ? AND user_id = ?"))
        .bind(id)
        .bind(user_id)
        .fetch_optional(pool)
        .await
}

/// The items of every list of `user_id`, by list and in order.
pub async fn items(pool: &SqlitePool, user_id: i64) -> sqlx::Result<Vec<ItemRow>> {
    sqlx::query_as::<_, ItemRow>(&format!(
        "SELECT i.watchlist_id, i.key, i.position, i.added_at FROM user_watchlist_items i
         JOIN user_watchlists w ON w.id = i.watchlist_id
         WHERE w.user_id = ? ORDER BY i.watchlist_id, i.position, i.added_at, i.key"
    ))
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// The items of one of `user_id`'s lists, in order; empty for a list that
/// is not theirs.
pub async fn items_of(pool: &SqlitePool, user_id: i64, id: i64) -> sqlx::Result<Vec<ItemRow>> {
    sqlx::query_as::<_, ItemRow>(&format!(
        "SELECT {ITEM_COLUMNS} FROM user_watchlist_items
         WHERE watchlist_id = ? AND watchlist_id IN (SELECT id FROM user_watchlists WHERE id = ? AND user_id = ?)
         ORDER BY position, added_at, key"
    ))
    .bind(id)
    .bind(id)
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// Gives `user_id` their first list, [`DEFAULT_NAME`] with [`default_keys`],
/// when they have none at all. The insert is conditional on that in the
/// SQL, so two first calls at once make one list. `true` when it was made
/// now.
pub async fn ensure_default(pool: &SqlitePool, user_id: i64, now: &str) -> sqlx::Result<bool> {
    let mut tx = write_tx(pool).await?;
    let made = sqlx::query(
        "INSERT INTO user_watchlists (user_id, name, position, created_at)
         SELECT ?1, ?2, 0, ?3 WHERE NOT EXISTS (SELECT 1 FROM user_watchlists WHERE user_id = ?1)",
    )
    .bind(user_id)
    .bind(DEFAULT_NAME)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    if made.rows_affected() == 0 {
        tx.rollback().await?;
        return Ok(false);
    }
    let id = made.last_insert_rowid();
    for (position, key) in default_keys().iter().enumerate() {
        sqlx::query("INSERT INTO user_watchlist_items (watchlist_id, key, position, added_at) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(key)
            .bind(position as i64)
            .bind(now)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(true)
}

/// A new list at the end of `user_id`'s, named `name` (checked by the
/// route). [`Error::TooManyLists`] past [`MAX_LISTS`].
pub async fn create(pool: &SqlitePool, user_id: i64, name: &str, now: &str) -> Result<ListRow, Error> {
    let mut tx = write_tx(pool).await?;
    let made = sqlx::query(INSERT_LIST)
        .bind(user_id)
        .bind(name)
        .bind(now)
        .bind(MAX_LISTS as i64)
        .execute(&mut *tx)
        .await?;
    if made.rows_affected() == 0 {
        tx.rollback().await?;
        return Err(Error::TooManyLists);
    }
    let row = sqlx::query_as::<_, ListRow>(&format!("SELECT {LIST_COLUMNS} FROM user_watchlists WHERE id = ?"))
        .bind(made.last_insert_rowid())
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(row)
}

/// Renames list `id` of `user_id` and/or moves it to `position` among
/// their lists (0-based; the others shift). [`Error::NotFound`] for a list
/// that is not theirs, [`Error::BadPosition`] past their last list.
pub async fn update(pool: &SqlitePool, user_id: i64, id: i64, name: Option<&str>, position: Option<usize>) -> Result<ListRow, Error> {
    let mut tx = write_tx(pool).await?;
    if let Some(name) = name {
        let done = sqlx::query("UPDATE user_watchlists SET name = ? WHERE id = ? AND user_id = ?")
            .bind(name)
            .bind(id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        if done.rows_affected() == 0 {
            tx.rollback().await?;
            return Err(Error::NotFound);
        }
    }
    if let Some(position) = position {
        let mut ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM user_watchlists WHERE user_id = ? ORDER BY position, id")
            .bind(user_id)
            .fetch_all(&mut *tx)
            .await?;
        let Some(from) = ids.iter().position(|i| *i == id) else {
            tx.rollback().await?;
            return Err(Error::NotFound);
        };
        if position >= ids.len() {
            tx.rollback().await?;
            return Err(Error::BadPosition);
        }
        let moved = ids.remove(from);
        ids.insert(position, moved);
        renumber_lists(&mut tx, &ids).await?;
    }
    let row = sqlx::query_as::<_, ListRow>(&format!("SELECT {LIST_COLUMNS} FROM user_watchlists WHERE id = ? AND user_id = ?"))
        .bind(id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(row) = row else {
        tx.rollback().await?;
        return Err(Error::NotFound);
    };
    tx.commit().await?;
    Ok(row)
}

async fn renumber_lists(tx: &mut SqliteConnection, ids: &[i64]) -> sqlx::Result<()> {
    for (position, id) in ids.iter().enumerate() {
        sqlx::query("UPDATE user_watchlists SET position = ? WHERE id = ?")
            .bind(position as i64)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    Ok(())
}

/// Removes list `id` of `user_id` with its items and closes the gap in the
/// others' positions. [`Error::LastList`] for their only list: everyone
/// keeps at least one, so "no lists" means "never had one" and
/// [`ensure_default`] can tell a new person from one who deleted theirs.
pub async fn delete(pool: &SqlitePool, user_id: i64, id: i64) -> Result<(), Error> {
    let mut tx = write_tx(pool).await?;
    let mut ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM user_watchlists WHERE user_id = ? ORDER BY position, id")
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
    if !ids.contains(&id) {
        tx.rollback().await?;
        return Err(Error::NotFound);
    }
    if ids.len() == 1 {
        tx.rollback().await?;
        return Err(Error::LastList);
    }
    sqlx::query("DELETE FROM user_watchlist_items WHERE watchlist_id = ?").bind(id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM user_watchlists WHERE id = ? AND user_id = ?").bind(id).bind(user_id).execute(&mut *tx).await?;
    ids.retain(|i| *i != id);
    renumber_lists(&mut tx, &ids).await?;
    tx.commit().await?;
    Ok(())
}

/// Whether list `id` is `user_id`'s, inside `tx`.
async fn owned(tx: &mut SqliteConnection, user_id: i64, id: i64) -> sqlx::Result<bool> {
    let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM user_watchlists WHERE id = ? AND user_id = ?")
        .bind(id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
    Ok(found.is_some())
}

/// Makes `keys`, in that order, the whole of list `id` of `user_id`
/// (distinct and checked against the catalog by the route). A key that
/// was already in the list keeps its `added_at`. [`Error::TooManyItems`]
/// past [`MAX_ITEMS`], and nothing is changed then.
pub async fn replace_items(pool: &SqlitePool, user_id: i64, id: i64, keys: &[String], now: &str) -> Result<(), Error> {
    let mut tx = write_tx(pool).await?;
    if !owned(&mut tx, user_id, id).await? {
        tx.rollback().await?;
        return Err(Error::NotFound);
    }
    let kept: Vec<(String, String)> = sqlx::query_as("SELECT key, added_at FROM user_watchlist_items WHERE watchlist_id = ?")
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM user_watchlist_items WHERE watchlist_id = ?").bind(id).execute(&mut *tx).await?;
    for key in keys {
        let added_at = kept.iter().find(|(k, _)| k == key).map(|(_, at)| at.as_str()).unwrap_or(now);
        let done = sqlx::query(INSERT_ITEM)
            .bind(id)
            .bind(key)
            .bind(added_at)
            .bind(MAX_ITEMS as i64)
            .execute(&mut *tx)
            .await?;
        if done.rows_affected() == 0 {
            tx.rollback().await?;
            return Err(Error::TooManyItems);
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Adds `key` at the end of list `id` of `user_id`. `Ok(false)` when the
/// list already holds it (nothing changes), [`Error::TooManyItems`] when
/// it is full.
pub async fn add_item(pool: &SqlitePool, user_id: i64, id: i64, key: &str, now: &str) -> Result<bool, Error> {
    let mut tx = write_tx(pool).await?;
    if !owned(&mut tx, user_id, id).await? {
        tx.rollback().await?;
        return Err(Error::NotFound);
    }
    let done = sqlx::query(INSERT_ITEM)
        .bind(id)
        .bind(key)
        .bind(now)
        .bind(MAX_ITEMS as i64)
        .execute(&mut *tx)
        .await?;
    if done.rows_affected() == 1 {
        tx.commit().await?;
        return Ok(true);
    }
    let present: Option<i64> = sqlx::query_scalar("SELECT 1 FROM user_watchlist_items WHERE watchlist_id = ? AND key = ?")
        .bind(id)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
    tx.rollback().await?;
    if present.is_some() { Ok(false) } else { Err(Error::TooManyItems) }
}

/// Takes `key` out of list `id` of `user_id` and closes the gap.
/// `Ok(false)` when the list did not hold it.
pub async fn remove_item(pool: &SqlitePool, user_id: i64, id: i64, key: &str) -> Result<bool, Error> {
    let mut tx = write_tx(pool).await?;
    if !owned(&mut tx, user_id, id).await? {
        tx.rollback().await?;
        return Err(Error::NotFound);
    }
    let position: Option<i64> = sqlx::query_scalar("SELECT position FROM user_watchlist_items WHERE watchlist_id = ? AND key = ?")
        .bind(id)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(position) = position else {
        tx.rollback().await?;
        return Ok(false);
    };
    sqlx::query("DELETE FROM user_watchlist_items WHERE watchlist_id = ? AND key = ?").bind(id).bind(key).execute(&mut *tx).await?;
    sqlx::query("UPDATE user_watchlist_items SET position = position - 1 WHERE watchlist_id = ? AND position > ?")
        .bind(id)
        .bind(position)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

/// Removes every list of `user_id` with their items, inside the caller's
/// transaction: what `DELETE /api/admin/users/:id` does with the rest of a
/// user's rows, so a later user given the same id starts from nothing.
pub async fn delete_all(conn: &mut SqliteConnection, user_id: i64) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM user_watchlist_items WHERE watchlist_id IN (SELECT id FROM user_watchlists WHERE user_id = ?)")
        .bind(user_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query("DELETE FROM user_watchlists WHERE user_id = ?").bind(user_id).execute(&mut *conn).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{init_db, test_support::TempDb};

    const NOW: &str = "2026-11-10 10:00:00";
    const LATER: &str = "2026-11-10 10:05:00";

    async fn pool(name: &str) -> (TempDb, SqlitePool) {
        let db = TempDb::new(&format!("watchlists-{name}"));
        let pool = init_db(&db.url()).await;
        (db, pool)
    }

    fn keys(items: &[ItemRow]) -> Vec<&str> {
        items.iter().map(|i| i.key.as_str()).collect()
    }

    fn positions(items: &[ItemRow]) -> Vec<i64> {
        items.iter().map(|i| i.position).collect()
    }

    #[tokio::test]
    async fn a_new_person_gets_one_default_list_with_the_three_indices() {
        let (_db, pool) = pool("default").await;
        assert!(lists(&pool, 7).await.unwrap().is_empty());
        assert!(ensure_default(&pool, 7, NOW).await.unwrap());
        assert!(!ensure_default(&pool, 7, LATER).await.unwrap(), "a second call makes nothing");
        let mine = lists(&pool, 7).await.unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!((mine[0].user_id, mine[0].name.as_str(), mine[0].position, mine[0].created_at.as_str()), (7, "Watchlist 1", 0, NOW));
        let rows = items(&pool, 7).await.unwrap();
        assert_eq!(keys(&rows), vec!["idx|NIFTY 50", "idx|NIFTY BANK", "idx|SENSEX"]);
        assert_eq!(positions(&rows), vec![0, 1, 2]);
        assert!(rows.iter().all(|i| i.watchlist_id == mine[0].id && i.added_at == NOW));
        // Someone else is still new.
        assert!(lists(&pool, 8).await.unwrap().is_empty());
        assert!(items(&pool, 8).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn five_lists_at_most_and_the_sixth_is_refused_by_the_insert_itself() {
        let (_db, pool) = pool("cap-lists").await;
        ensure_default(&pool, 7, NOW).await.unwrap();
        for n in 2..=MAX_LISTS {
            let row = create(&pool, 7, &format!("List {n}"), NOW).await.unwrap();
            assert_eq!((row.user_id, row.position as usize), (7, n - 1));
        }
        assert!(matches!(create(&pool, 7, "One more", NOW).await, Err(Error::TooManyLists)));
        assert_eq!(lists(&pool, 7).await.unwrap().len(), MAX_LISTS);
        // The cap is per person: someone else can still start.
        assert_eq!(create(&pool, 8, "Theirs", NOW).await.unwrap().position, 0);
        // The conditional insert alone refuses too, with no check in front.
        let direct = sqlx::query(INSERT_LIST).bind(7).bind("Direct").bind(NOW).bind(MAX_LISTS as i64).execute(&pool).await.unwrap();
        assert_eq!(direct.rows_affected(), 0);
    }

    #[tokio::test]
    async fn fifty_items_at_most_in_order_and_a_repeat_adds_nothing() {
        let (_db, pool) = pool("cap-items").await;
        let list = create(&pool, 7, "Stocks", NOW).await.unwrap();
        let fifty: Vec<String> = (1..=MAX_ITEMS).map(|i| format!("nse_cm|{i}")).collect();
        replace_items(&pool, 7, list.id, &fifty, NOW).await.unwrap();
        let items_now = items_of(&pool, 7, list.id).await.unwrap();
        assert_eq!(keys(&items_now), fifty.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(positions(&items_now), (0..MAX_ITEMS as i64).collect::<Vec<_>>());
        assert!(matches!(add_item(&pool, 7, list.id, "nse_cm|51", NOW).await, Err(Error::TooManyItems)));
        assert_eq!(add_item(&pool, 7, list.id, "nse_cm|1", NOW).await.unwrap(), false, "already there: not an error, not a change");
        let mut fifty_one = fifty.clone();
        fifty_one.push("nse_cm|51".into());
        assert!(matches!(replace_items(&pool, 7, list.id, &fifty_one, LATER).await, Err(Error::TooManyItems)));
        assert_eq!(keys(&items_of(&pool, 7, list.id).await.unwrap()), fifty.iter().map(String::as_str).collect::<Vec<_>>(), "a refused replace changes nothing");
        // The conditional insert alone refuses the fifty-first too.
        let direct = sqlx::query(INSERT_ITEM).bind(list.id).bind("nse_cm|51").bind(NOW).bind(MAX_ITEMS as i64).execute(&pool).await.unwrap();
        assert_eq!(direct.rows_affected(), 0);

        // Reordered and shortened: the kept keys keep when they were added.
        let three = vec!["nse_cm|3".to_string(), "nse_cm|1".to_string(), "nse_fo|99".to_string()];
        replace_items(&pool, 7, list.id, &three, LATER).await.unwrap();
        let items_now = items_of(&pool, 7, list.id).await.unwrap();
        assert_eq!(keys(&items_now), vec!["nse_cm|3", "nse_cm|1", "nse_fo|99"]);
        assert_eq!(positions(&items_now), vec![0, 1, 2]);
        assert_eq!(items_now.iter().map(|i| i.added_at.as_str()).collect::<Vec<_>>(), vec![NOW, NOW, LATER]);
        // Add at the end, remove from the middle, and the gap closes.
        assert!(add_item(&pool, 7, list.id, "idx|NIFTY 50", LATER).await.unwrap());
        assert!(remove_item(&pool, 7, list.id, "nse_cm|1").await.unwrap());
        assert!(!remove_item(&pool, 7, list.id, "nse_cm|1").await.unwrap(), "gone already");
        let items_now = items_of(&pool, 7, list.id).await.unwrap();
        assert_eq!(keys(&items_now), vec!["nse_cm|3", "nse_fo|99", "idx|NIFTY 50"]);
        assert_eq!(positions(&items_now), vec![0, 1, 2]);
        replace_items(&pool, 7, list.id, &[], LATER).await.unwrap();
        assert!(items_of(&pool, 7, list.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn lists_are_renamed_moved_and_deleted_and_the_last_one_stays() {
        let (_db, pool) = pool("order").await;
        ensure_default(&pool, 7, NOW).await.unwrap();
        let a = lists(&pool, 7).await.unwrap()[0].id;
        let b = create(&pool, 7, "B", NOW).await.unwrap().id;
        let c = create(&pool, 7, "C", NOW).await.unwrap().id;
        let order = |rows: Vec<ListRow>| rows.into_iter().map(|r| (r.id, r.position)).collect::<Vec<_>>();
        assert_eq!(order(lists(&pool, 7).await.unwrap()), vec![(a, 0), (b, 1), (c, 2)]);

        let moved = update(&pool, 7, c, Some("First"), Some(0)).await.unwrap();
        assert_eq!((moved.id, moved.name.as_str(), moved.position), (c, "First", 0));
        assert_eq!(order(lists(&pool, 7).await.unwrap()), vec![(c, 0), (a, 1), (b, 2)]);
        let moved = update(&pool, 7, c, None, Some(2)).await.unwrap();
        assert_eq!(moved.position, 2);
        assert_eq!(order(lists(&pool, 7).await.unwrap()), vec![(a, 0), (b, 1), (c, 2)]);
        assert!(matches!(update(&pool, 7, c, None, Some(3)).await, Err(Error::BadPosition)));
        let same = update(&pool, 7, b, None, None).await.unwrap();
        assert_eq!((same.id, same.name.as_str(), same.position), (b, "B", 1), "nothing asked, nothing changed");

        add_item(&pool, 7, b, "nse_cm|2885", NOW).await.unwrap();
        delete(&pool, 7, b).await.unwrap();
        assert_eq!(order(lists(&pool, 7).await.unwrap()), vec![(a, 0), (c, 1)]);
        assert!(items(&pool, 7).await.unwrap().iter().all(|i| i.watchlist_id != b), "its items went with it");
        assert!(matches!(delete(&pool, 7, b).await, Err(Error::NotFound)));
        delete(&pool, 7, c).await.unwrap();
        assert!(matches!(delete(&pool, 7, a).await, Err(Error::LastList)));
        assert_eq!(lists(&pool, 7).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn one_person_cannot_see_or_change_anothers_lists() {
        let (_db, pool) = pool("isolation").await;
        ensure_default(&pool, 2, NOW).await.unwrap();
        ensure_default(&pool, 3, NOW).await.unwrap();
        let theirs = lists(&pool, 2).await.unwrap()[0].id;
        let mine = lists(&pool, 3).await.unwrap()[0].id;
        assert_ne!(theirs, mine);
        add_item(&pool, 2, theirs, "nse_cm|2885", NOW).await.unwrap();

        // Reads answer about the caller alone.
        assert!(list(&pool, 3, theirs).await.unwrap().is_none());
        assert!(items_of(&pool, 3, theirs).await.unwrap().is_empty());
        assert!(items(&pool, 3).await.unwrap().iter().all(|i| i.watchlist_id == mine));
        // Writes to someone else's list are "not found", and change nothing.
        assert!(matches!(update(&pool, 3, theirs, Some("Pwned"), None).await, Err(Error::NotFound)));
        assert!(matches!(update(&pool, 3, theirs, None, Some(0)).await, Err(Error::NotFound)));
        assert!(matches!(delete(&pool, 3, theirs).await, Err(Error::NotFound)));
        assert!(matches!(replace_items(&pool, 3, theirs, &["nse_cm|1".to_string()], NOW).await, Err(Error::NotFound)));
        assert!(matches!(add_item(&pool, 3, theirs, "nse_cm|1", NOW).await, Err(Error::NotFound)));
        assert!(matches!(remove_item(&pool, 3, theirs, "nse_cm|2885").await, Err(Error::NotFound)));
        let untouched = list(&pool, 2, theirs).await.unwrap().unwrap();
        assert_eq!(untouched.name, "Watchlist 1");
        assert_eq!(keys(&items_of(&pool, 2, theirs).await.unwrap()), vec!["idx|NIFTY 50", "idx|NIFTY BANK", "idx|SENSEX", "nse_cm|2885"]);
        // The admin (user 1) keeps lists of its own, the same way.
        ensure_default(&pool, 1, NOW).await.unwrap();
        assert_eq!(lists(&pool, 1).await.unwrap().len(), 1);
        assert!(list(&pool, 1, theirs).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_removed_user_takes_their_lists_with_them() {
        let (_db, pool) = pool("delete-all").await;
        ensure_default(&pool, 2, NOW).await.unwrap();
        ensure_default(&pool, 3, NOW).await.unwrap();
        create(&pool, 2, "More", NOW).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        delete_all(&mut tx, 2).await.unwrap();
        tx.commit().await.unwrap();
        assert!(lists(&pool, 2).await.unwrap().is_empty());
        assert!(items(&pool, 2).await.unwrap().is_empty());
        let orphans: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_watchlist_items WHERE watchlist_id NOT IN (SELECT id FROM user_watchlists)")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(orphans, 0);
        assert_eq!(lists(&pool, 3).await.unwrap().len(), 1, "someone else's stay");
        // A new person given the same id starts over.
        assert!(ensure_default(&pool, 2, LATER).await.unwrap());
    }

    /// The item writes look (is the list the caller's? where is the key?)
    /// before they write. On the pool's connections at once, a deferred
    /// transaction doing that is refused outright (`SQLITE_BUSY`) when
    /// another one committed between its read and its write; `BEGIN
    /// IMMEDIATE` queues them instead, so every one of these lands.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn item_writes_at_once_all_land() {
        let (_db, pool) = pool("concurrent").await;
        let list = create(&pool, 7, "Busy", NOW).await.unwrap();
        let adds: Vec<_> = (1..=20)
            .map(|i| {
                let pool = pool.clone();
                tokio::spawn(async move { add_item(&pool, 7, list.id, &format!("nse_cm|{i}"), NOW).await })
            })
            .collect();
        for add in adds {
            assert!(add.await.unwrap().unwrap_or_else(|e| panic!("an add at once failed: {e}")), "each key is new");
        }
        let rows = items_of(&pool, 7, list.id).await.unwrap();
        assert_eq!(rows.len(), 20);
        assert_eq!(positions(&rows), (0..20).collect::<Vec<_>>(), "dense, whatever the order they landed in");

        let removes: Vec<_> = (1..=10)
            .map(|i| {
                let pool = pool.clone();
                tokio::spawn(async move { remove_item(&pool, 7, list.id, &format!("nse_cm|{i}")).await })
            })
            .collect();
        let replace = {
            let pool = pool.clone();
            let keep: Vec<String> = (11..=20).map(|i| format!("nse_cm|{i}")).collect();
            tokio::spawn(async move { replace_items(&pool, 7, list.id, &keep, LATER).await })
        };
        for remove in removes {
            remove.await.unwrap().unwrap_or_else(|e| panic!("a remove at once failed: {e}"));
        }
        replace.await.unwrap().unwrap_or_else(|e| panic!("a replace at once failed: {e}"));
        let rows = items_of(&pool, 7, list.id).await.unwrap();
        let kept: Vec<String> = (11..=20).map(|i| format!("nse_cm|{i}")).collect();
        assert_eq!(keys(&rows), kept.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(positions(&rows), (0..10).collect::<Vec<_>>());
    }
}
