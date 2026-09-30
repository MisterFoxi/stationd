//! Reproducible random draws for the selection — a shuffle pick, a group's
//! shuffle permutation, a weighted member.
//!
//! Why: the on-air view simulates what follows by running the real engine on
//! a copy of the database (`onair_sim`). With draws from `thread_rng`, the
//! simulation and the real pull drew different numbers, so « À suivre »
//! diverged from what aired as soon as a playlist shuffled. Here a draw is a
//! pure function of state held IN the database — the station seed and the
//! number of draws its scope has consumed — so the copy draws exactly what
//! the real pull will draw next, from the same state.
//!
//! One counter per scope (`pick:<playlist>`, `perm:<group>`,
//! `weight:<group>`): an override or a rendez-vous that pulls from another
//! playlist does not shift this one's draws.
//!
//! What still makes the simulation differ is what changes the state or the
//! candidates between now and the pull: an override, a live, a grid applied,
//! a rescan, a time window crossed at a different instant.

use rand::{RngCore, SeedableRng};
use rand_chacha::ChaCha8Rng;
use sqlx::SqlitePool;

/// Create the station seed if it does not exist yet (`db::init`). Random,
/// once per station: two stations do not shuffle alike.
pub async fn ensure_seed(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT OR IGNORE INTO rng_seed (id, seed) VALUES (1, randomblob(32))")
        .execute(pool)
        .await?;
    Ok(())
}

/// The generator of the next draw of `scope`, and the draw consumed.
pub async fn rng(pool: &SqlitePool, scope: &str) -> Result<ChaCha8Rng, sqlx::Error> {
    // Normally created by `db::init`; a pool opened otherwise (tests) gets it here.
    ensure_seed(pool).await?;
    let (seed,): (Vec<u8>,) = sqlx::query_as("SELECT seed FROM rng_seed WHERE id = 1")
        .fetch_one(pool)
        .await?;
    // `fetch_all`, not `fetch_one`: a RETURNING statement keeps its write
    // transaction open until it has run to completion, and `fetch_one` hands
    // the row back after the first step — the lock then outlived the call and
    // the next writer on another connection of the pool waited on it
    // (« database is locked » once past the busy timeout, on a loaded host).
    let rows: Vec<(i64,)> = sqlx::query_as(
        "INSERT INTO rng_draws (scope, draws) VALUES (?1, 1)
         ON CONFLICT(scope) DO UPDATE SET draws = draws + 1
         RETURNING draws - 1",
    )
    .bind(scope)
    .fetch_all(pool)
    .await?;
    let n = rows.first().map(|r| r.0).ok_or(sqlx::Error::RowNotFound)?;
    Ok(stream(&seed, scope, n as u64))
}

/// Draw `n` of `scope`: a key derived from the seed and the scope, stream `n`.
fn stream(seed: &[u8], scope: &str, n: u64) -> ChaCha8Rng {
    let mut key = [0u8; 32];
    key.iter_mut().zip(seed).for_each(|(k, s)| *k = *s);
    let mut master = ChaCha8Rng::from_seed(key);
    master.set_stream(fnv1a(scope));
    let mut scoped = [0u8; 32];
    master.fill_bytes(&mut scoped);
    let mut r = ChaCha8Rng::from_seed(scoped);
    r.set_stream(n);
    r
}

/// FNV-1a 64 — stable across builds and Rust versions (unlike `DefaultHasher`),
/// so a scope keeps its key after an upgrade.
fn fnv1a(s: &str) -> u64 {
    s.bytes()
        .fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;

    async fn draws(pool: &SqlitePool, scope: &str, k: usize) -> Vec<u64> {
        let mut out = Vec::new();
        for _ in 0..k {
            out.push(rng(pool, scope).await.unwrap().gen());
        }
        out
    }

    #[tokio::test]
    async fn a_copy_draws_what_the_live_database_will_draw() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.db");
        let live = crate::db::init(&path).await.unwrap();
        draws(&live, "pick:music", 3).await;

        let copy = crate::db::memory_copy(&path).await.unwrap();
        let simulated = draws(&copy, "pick:music", 5).await;
        assert_eq!(draws(&live, "pick:music", 5).await, simulated);
    }

    #[tokio::test]
    async fn draws_move_on_and_scopes_are_independent() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("a.db")).await.unwrap();
        let a = draws(&pool, "pick:a", 4).await;
        let mut uniq = a.clone();
        uniq.dedup();
        assert_eq!(uniq.len(), 4, "each draw is a new one: {a:?}");
        // Same seed elsewhere: draws of another scope in between do not shift `pick:a`.
        let other = crate::db::init(&dir.path().join("b.db")).await.unwrap();
        let (seed,): (Vec<u8>,) = sqlx::query_as("SELECT seed FROM rng_seed").fetch_one(&pool).await.unwrap();
        sqlx::query("UPDATE rng_seed SET seed = ?1").bind(&seed).execute(&other).await.unwrap();
        let mut mixed = Vec::new();
        for _ in 0..4 {
            draws(&other, "weight:g", 2).await;
            mixed.extend(draws(&other, "pick:a", 1).await);
        }
        assert_eq!(mixed, a);
    }

    /// A draw leaves no write lock behind: a writer on another connection,
    /// which does not wait at all, gets through right after it.
    #[tokio::test]
    async fn a_draw_releases_its_write_lock_before_returning() {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock.db");
        let pool = crate::db::init(&path).await.unwrap();
        let impatient = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::new().filename(&path).busy_timeout(std::time::Duration::ZERO))
            .await
            .unwrap();
        for i in 0..50 {
            rng(&pool, "pick:x").await.unwrap();
            sqlx::query("INSERT INTO rng_draws (scope, draws) VALUES (?1, 0)")
                .bind(format!("other:{i}"))
                .execute(&impatient)
                .await
                .unwrap_or_else(|e| panic!("draw {i} left the database locked: {e}"));
        }
    }

    #[test]
    fn a_scope_keeps_its_key_across_builds() {
        assert_eq!(fnv1a(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a("a"), 0xaf63_dc4c_8601_ec8c);
    }
}
