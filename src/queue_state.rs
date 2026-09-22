//! Family (B) — a `queue` playlist's runtime buffer, against the `queue_entry`
//! table of migration 0013. Pushed at runtime (audience requests / DJ
//! injection), consumed FIFO/LIFO; the played entry is deleted. Survives a
//! restart — a queued request must not vanish.
//!
//! No FK, keyed by the canonical playlist_ref. Like the other family-B modules,
//! each function does one thing and holds no business logic.

use sqlx::SqlitePool;

use crate::resolver::Epoch;

/// Outcome of a [`push`]: whether it was accepted, and the buffer length after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushOutcome {
    pub accepted: bool,
    pub len: u64,
}

/// Enqueue `rel_path` at the tail. `max_len` caps the buffer — `0`/`None` =
/// unlimited (per the grammar); at capacity the push is REFUSED
/// (`accepted = false`), never silently dropped nor overwriting an entry.
/// Returns the buffer length after (unchanged when refused).
pub async fn push(
    pool: &SqlitePool,
    playlist_ref: &str,
    rel_path: &str,
    max_len: Option<u32>,
    at: Epoch,
) -> Result<PushOutcome, sqlx::Error> {
    let current = count(pool, playlist_ref).await?;
    if let Some(cap) = max_len {
        if cap > 0 && current >= cap as u64 {
            return Ok(PushOutcome { accepted: false, len: current });
        }
    }
    sqlx::query("INSERT INTO queue_entry (playlist_ref, rel_path, enqueued_at) VALUES (?1, ?2, ?3)")
        .bind(playlist_ref)
        .bind(rel_path)
        .bind(at.0)
        .execute(pool)
        .await?;
    Ok(PushOutcome { accepted: true, len: current + 1 })
}

/// Pop the next entry — FIFO (oldest `id`) or LIFO (newest) — returning its
/// `rel_path` and DELETING it. `None` when the buffer is empty. Single-writer
/// (stationd) guarantees no concurrent pop, so the select-then-delete is safe.
pub async fn pop(
    pool: &SqlitePool,
    playlist_ref: &str,
    lifo: bool,
) -> Result<Option<String>, sqlx::Error> {
    // `order` is a constant, never user input — no injection surface.
    let order = if lifo { "DESC" } else { "ASC" };
    let row: Option<(i64, String)> = sqlx::query_as(&format!(
        "SELECT id, rel_path FROM queue_entry WHERE playlist_ref = ?1 ORDER BY id {order} LIMIT 1"
    ))
    .bind(playlist_ref)
    .fetch_optional(pool)
    .await?;
    match row {
        Some((id, rel_path)) => {
            sqlx::query("DELETE FROM queue_entry WHERE id = ?1")
                .bind(id)
                .execute(pool)
                .await?;
            Ok(Some(rel_path))
        }
        None => Ok(None),
    }
}

/// Current buffer length for `playlist_ref`.
pub async fn count(pool: &SqlitePool, playlist_ref: &str) -> Result<u64, sqlx::Error> {
    let (n,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM queue_entry WHERE playlist_ref = ?1")
            .bind(playlist_ref)
            .fetch_one(pool)
            .await?;
    Ok(n as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn fresh_db() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::init(&dir.path().join("t.db")).await.unwrap();
        (dir, pool)
    }

    #[tokio::test]
    async fn fifo_pops_oldest_first_and_empties() {
        let (_d, pool) = fresh_db().await;
        push(&pool, "q", "a", None, Epoch(1)).await.unwrap();
        push(&pool, "q", "b", None, Epoch(2)).await.unwrap();
        assert_eq!(pop(&pool, "q", false).await.unwrap().as_deref(), Some("a"));
        assert_eq!(pop(&pool, "q", false).await.unwrap().as_deref(), Some("b"));
        assert_eq!(pop(&pool, "q", false).await.unwrap(), None);
    }

    #[tokio::test]
    async fn lifo_pops_newest_first() {
        let (_d, pool) = fresh_db().await;
        push(&pool, "q", "a", None, Epoch(1)).await.unwrap();
        push(&pool, "q", "b", None, Epoch(2)).await.unwrap();
        assert_eq!(pop(&pool, "q", true).await.unwrap().as_deref(), Some("b"));
        assert_eq!(pop(&pool, "q", true).await.unwrap().as_deref(), Some("a"));
    }

    #[tokio::test]
    async fn max_len_refuses_beyond_capacity() {
        let (_d, pool) = fresh_db().await;
        assert!(push(&pool, "q", "a", Some(1), Epoch(1)).await.unwrap().accepted);
        let refused = push(&pool, "q", "b", Some(1), Epoch(2)).await.unwrap();
        assert!(!refused.accepted, "at capacity → refused");
        assert_eq!(refused.len, 1);
        // Scoped per playlist: another queue is unaffected.
        assert_eq!(count(&pool, "other").await.unwrap(), 0);
    }
}
