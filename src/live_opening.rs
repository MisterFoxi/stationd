//! Family (B) — ad-hoc live openings (`stationctl live open`), against the
//! `live_opening` table of migration 0018.
//!
//! An opening lets one DJ connect outside the grid's `live` slots, from
//! `opened_at` to `until` (epoch UTC, end excluded). It only gates the
//! connection, like a slot: a DJ already on air stays on air past `until`.
//! Persisted so that a stationd restart keeps it.
//!
//! Like the other family-B modules: no business logic, no proto, addressed by
//! identity (the DJ id), never reset by an apply or a scan.

use sqlx::SqlitePool;

use crate::resolver::Epoch;

/// One opening (active or not).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opening {
    pub id: i64,
    pub dj: String,
    pub opened_at: Epoch,
    /// End, excluded: active while `until > now`.
    pub until: Epoch,
    /// The DJ was cut (silence or kick) during it: refused until its end.
    pub cut: bool,
}

type Row = (i64, String, i64, i64, i64);

fn from_row((id, dj, opened_at, until, cut): Row) -> Opening {
    Opening { id, dj, opened_at: Epoch(opened_at), until: Epoch(until), cut: cut != 0 }
}

/// The DJ's active opening at `now`, if any (the most recent one).
pub async fn active(pool: &SqlitePool, dj: &str, now: Epoch) -> Result<Option<Opening>, sqlx::Error> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, dj, opened_at, until, cut FROM live_opening
         WHERE dj = ?1 AND until > ?2 ORDER BY id DESC LIMIT 1",
    )
    .bind(dj)
    .bind(now.0)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(from_row))
}

/// Every opening active at `now`, soonest end first.
pub async fn list_active(pool: &SqlitePool, now: Epoch) -> Result<Vec<Opening>, sqlx::Error> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, dj, opened_at, until, cut FROM live_opening
         WHERE until > ?1 ORDER BY until, dj",
    )
    .bind(now.0)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(from_row).collect())
}

/// Open a window for `dj` from `now` to `until`. An opening still active for
/// that DJ is closed first (one active opening per DJ): the new one replaces
/// it, `cut` reset.
pub async fn open(pool: &SqlitePool, dj: &str, now: Epoch, until: Epoch) -> Result<Opening, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE live_opening SET until = ?2 WHERE dj = ?1 AND until > ?2")
        .bind(dj)
        .bind(now.0)
        .execute(&mut *tx)
        .await?;
    let id = sqlx::query("INSERT INTO live_opening (dj, opened_at, until, cut) VALUES (?1, ?2, ?3, 0)")
        .bind(dj)
        .bind(now.0)
        .bind(until.0)
        .execute(&mut *tx)
        .await?
        .last_insert_rowid();
    tx.commit().await?;
    Ok(Opening { id, dj: dj.to_string(), opened_at: now, until, cut: false })
}

/// Close the DJ's active opening now (`until` = `now`). Returns it as it was
/// before closing, `None` if the DJ had none.
pub async fn close(pool: &SqlitePool, dj: &str, now: Epoch) -> Result<Option<Opening>, sqlx::Error> {
    let Some(o) = active(pool, dj, now).await? else {
        return Ok(None);
    };
    sqlx::query("UPDATE live_opening SET until = ?2 WHERE id = ?1")
        .bind(o.id)
        .bind(now.0)
        .execute(pool)
        .await?;
    Ok(Some(o))
}

/// The DJ was cut during opening `id`: refused until its end.
pub async fn mark_cut(pool: &SqlitePool, id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE live_opening SET cut = 1 WHERE id = ?1").bind(id).execute(pool).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("t.db")).await.unwrap();
        (dir, pool)
    }

    #[tokio::test]
    async fn an_opening_is_active_until_its_end_excluded() {
        let (_d, pool) = pool().await;
        assert!(active(&pool, "marc", Epoch(100)).await.unwrap().is_none());
        let o = open(&pool, "marc", Epoch(100), Epoch(200)).await.unwrap();
        assert_eq!(active(&pool, "marc", Epoch(199)).await.unwrap(), Some(o.clone()));
        assert!(active(&pool, "marc", Epoch(200)).await.unwrap().is_none());
        assert!(active(&pool, "julie", Epoch(150)).await.unwrap().is_none());
        assert_eq!(list_active(&pool, Epoch(150)).await.unwrap(), vec![o]);
    }

    #[tokio::test]
    async fn a_new_opening_replaces_the_active_one_and_resets_the_cut() {
        let (_d, pool) = pool().await;
        let first = open(&pool, "marc", Epoch(100), Epoch(1000)).await.unwrap();
        mark_cut(&pool, first.id).await.unwrap();
        assert!(active(&pool, "marc", Epoch(150)).await.unwrap().unwrap().cut);
        let second = open(&pool, "marc", Epoch(200), Epoch(300)).await.unwrap();
        let now = active(&pool, "marc", Epoch(250)).await.unwrap().unwrap();
        assert_eq!(now.id, second.id);
        assert!(!now.cut);
        // the first one was closed at 200: past 300, nothing is left
        assert!(active(&pool, "marc", Epoch(500)).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn close_ends_the_active_opening_now() {
        let (_d, pool) = pool().await;
        assert!(close(&pool, "marc", Epoch(100)).await.unwrap().is_none());
        let o = open(&pool, "marc", Epoch(100), Epoch(1000)).await.unwrap();
        assert_eq!(close(&pool, "marc", Epoch(150)).await.unwrap(), Some(o));
        assert!(active(&pool, "marc", Epoch(150)).await.unwrap().is_none());
        assert!(list_active(&pool, Epoch(150)).await.unwrap().is_empty());
    }
}
