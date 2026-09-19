//! Group traversal state (family B — durable playback state).
//!
//! A `sequence`/`shuffle` group hands out one track per turn, so it must
//! remember where it is across turns *and* across restarts. Same store shape
//! as `playlist_cursor`/`grid_store`: plain SQL over a `&SqlitePool`, no logic.
//!
//! Keyed by the group's ref (the view's `rel_path`). The stored state is:
//!
//! - `member_idx` — position in the current traversal. For `sequence` it
//!   indexes `members` directly; for `shuffle` it indexes the persisted
//!   `permutation`.
//! - `take_count` — tracks already handed out for the current member (its
//!   `take` quota, in tracks).
//! - `member_started_at` — epoch (s) the current member began, for a `runtime`
//!   quota (time budget). `None` when the member has no time budget, or on a
//!   fresh activation.
//! - `permutation` — the shuffled member order of the current cycle (`shuffle`
//!   only), so a restart mid-cycle does NOT redraw and risk repeating a member.
//!   `None` for `sequence` (declared order).
//!
//! `GroupState::default()` = `(0, 0, None, None)` — a fresh activation from the
//! top.

use sqlx::SqlitePool;

/// Durable traversal state of one group activation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GroupState {
    pub member_idx: usize,
    pub take_count: u32,
    /// Epoch (s) the current member started — only for a `runtime` budget.
    pub member_started_at: Option<i64>,
    /// The shuffled order of the current cycle (`shuffle` groups only).
    pub permutation: Option<Vec<usize>>,
}

/// Current state for a group, or the default `(0, 0, None, None)` if unset.
pub async fn get(pool: &SqlitePool, group_ref: &str) -> Result<GroupState, sqlx::Error> {
    let row: Option<(i64, i64, Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT member_idx, take_count, member_started_at, permutation \
         FROM group_state WHERE group_ref = ?1",
    )
    .bind(group_ref)
    .fetch_optional(pool)
    .await?;
    Ok(row
        .map(|(idx, count, started, perm)| GroupState {
            member_idx: idx as usize,
            take_count: count as u32,
            member_started_at: started,
            permutation: perm.as_deref().and_then(parse_perm),
        })
        .unwrap_or_default())
}

/// Persist the group's position for the next turn.
pub async fn set(
    pool: &SqlitePool,
    group_ref: &str,
    state: &GroupState,
) -> Result<(), sqlx::Error> {
    let now = now_epoch_seconds();
    let perm = state.permutation.as_deref().map(fmt_perm);
    sqlx::query(
        "INSERT INTO group_state (group_ref, member_idx, take_count, member_started_at, permutation, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(group_ref) DO UPDATE SET
             member_idx        = ?2,
             take_count        = ?3,
             member_started_at = ?4,
             permutation       = ?5,
             updated_at        = ?6",
    )
    .bind(group_ref)
    .bind(state.member_idx as i64)
    .bind(state.take_count as i64)
    .bind(state.member_started_at)
    .bind(perm)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Serialize a permutation as comma-separated indices ("2,0,1"). Kept as plain
/// CSV rather than JSON so this module stays dependency-light (sqlx only).
fn fmt_perm(p: &[usize]) -> String {
    p.iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Parse a stored permutation. Any malformed entry yields `None` (treated as
/// "no permutation" → the caller redraws), never a panic or a silent partial.
fn parse_perm(s: &str) -> Option<Vec<usize>> {
    if s.is_empty() {
        return None;
    }
    s.split(',').map(|t| t.trim().parse::<usize>().ok()).collect()
}

fn now_epoch_seconds() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[tokio::test]
    async fn defaults_then_roundtrips_take_state() {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::init(&dir.path().join("t.db")).await.unwrap();

        assert_eq!(get(&pool, "show").await.unwrap(), GroupState::default());

        let st = GroupState {
            member_idx: 1,
            take_count: 2,
            member_started_at: None,
            permutation: None,
        };
        set(&pool, "show", &st).await.unwrap();
        assert_eq!(get(&pool, "show").await.unwrap(), st);

        set(&pool, "show", &GroupState::default()).await.unwrap();
        assert_eq!(get(&pool, "show").await.unwrap(), GroupState::default());
    }

    #[tokio::test]
    async fn roundtrips_runtime_and_permutation() {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::init(&dir.path().join("t.db")).await.unwrap();

        let st = GroupState {
            member_idx: 2,
            take_count: 0,
            member_started_at: Some(1_700_000_000),
            permutation: Some(vec![2, 0, 1]),
        };
        set(&pool, "grp", &st).await.unwrap();
        assert_eq!(get(&pool, "grp").await.unwrap(), st);
    }

    #[test]
    fn perm_csv_roundtrip_and_bad_input() {
        assert_eq!(fmt_perm(&[2, 0, 1]), "2,0,1");
        assert_eq!(parse_perm("2,0,1"), Some(vec![2, 0, 1]));
        assert_eq!(parse_perm(""), None);
        // A malformed entry collapses the whole thing to None (→ redraw).
        assert_eq!(parse_perm("2,x,1"), None);
    }
}
