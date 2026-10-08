//! Persisted UUID shuffle bags for leaf playlists. Transient exclusions only
//! skip a member; dynamic pool removal keeps a used tombstone until rollover.
use crate::{plugin::Candidate, selection::SelectionError};
use rand::seq::SliceRandom;
use rand::Rng;
use sqlx::SqlitePool;
use std::collections::{HashMap, HashSet};

pub async fn pick_reserved(
    pool: &SqlitePool,
    reference: &str,
    base: &[Candidate],
    eligible: &[Candidate],
    max: Option<u64>,
) -> Result<(String, i64), SelectionError> {
    if base.is_empty() || eligible.is_empty() {
        return Err(SelectionError::PoolEmpty);
    }
    let mut tx = pool.begin().await?;
    // Acquire the write lock before reading: concurrent pulls cannot consume
    // the same bag entry, or upgrade a stale SQLite read transaction.
    sqlx::query("INSERT OR IGNORE INTO shuffle_cycle (playlist_ref) VALUES (?)")
        .bind(reference)
        .execute(&mut *tx)
        .await?;
    let mut cycle: i64 =
        sqlx::query_scalar("SELECT cycle FROM shuffle_cycle WHERE playlist_ref = ?")
            .bind(reference)
            .fetch_one(&mut *tx)
            .await?;
    let rows: Vec<(String, i64, bool, bool)> = sqlx::query_as(
        "SELECT media_uuid, position, used, active FROM shuffle_member WHERE playlist_ref = ? ORDER BY position, media_uuid")
        .bind(reference).fetch_all(&mut *tx).await?;
    let active: Vec<(String, bool)> = rows
        .iter()
        .map(|(id, _, _, active)| (id.clone(), *active))
        .collect();
    let mut known: HashMap<String, (i64, bool)> = rows
        .into_iter()
        .map(|(id, position, used, _)| (id, (position, used)))
        .collect();
    let ids: HashSet<&str> = base.iter().map(|c| c.media_uuid.as_str()).collect();

    let mut added: Vec<&Candidate> = base
        .iter()
        .filter(|c| !known.contains_key(&c.media_uuid))
        .collect();
    // Stable input for the seeded shuffle: URI ordering and rescans do not
    // change the permutation of the same physical media.
    added.sort_by(|a, b| a.media_uuid.cmp(&b.media_uuid));
    added.dedup_by(|a, b| a.media_uuid == b.media_uuid);
    if !added.is_empty() {
        let mut rng = crate::draw::rng_on(&mut tx, &format!("bag:{reference}")).await?;
        // Insert newcomers at random places amongst pending members. Existing
        // pending members retain their relative order; rescans do not reshuffle.
        let mut pending: Vec<(String, i64)> = known
            .iter()
            .filter(|(_, (_, used))| !used)
            .map(|(id, (pos, _))| (id.clone(), *pos))
            .collect();
        pending.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
        let mut order: Vec<String> = pending.into_iter().map(|(id, _)| id).collect();
        if order.is_empty() {
            // Initial large catalogues use linear Fisher-Yates rather than
            // inserting every new member into a growing Vec (quadratic).
            order.extend(added.iter().map(|c| c.media_uuid.clone()));
            order.shuffle(&mut rng);
        } else {
            for c in added {
                let at = rng.gen_range(0..=order.len());
                order.insert(at, c.media_uuid.clone());
            }
        }
        for (position, id) in order.into_iter().enumerate() {
            sqlx::query(
                "INSERT INTO shuffle_member (playlist_ref, media_uuid, position) VALUES (?, ?, ?)
                ON CONFLICT(playlist_ref, media_uuid) DO UPDATE SET position = excluded.position",
            )
            .bind(reference)
            .bind(&id)
            .bind(position as i64)
            .execute(&mut *tx)
            .await?;
            known.insert(id, (position as i64, false));
        }
    }
    // A rescan with unchanged membership writes no member rows. Only real
    // membership changes update active flags; transient filters are absent here.
    for (id, was_active) in active {
        let present = ids.contains(id.as_str());
        if present != was_active {
            sqlx::query(
                "UPDATE shuffle_member SET active = ? WHERE playlist_ref = ? AND media_uuid = ?",
            )
            .bind(present)
            .bind(reference)
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        }
    }
    if ids
        .iter()
        .all(|id| known.get(*id).is_some_and(|(_, used)| *used))
    {
        cycle += 1;
        let mut order: Vec<&str> = ids.iter().copied().collect();
        order.sort_unstable();
        let mut rng = crate::draw::rng_on(&mut tx, &format!("bag:{reference}")).await?;
        order.shuffle(&mut rng);
        sqlx::query("DELETE FROM shuffle_member WHERE playlist_ref = ? AND active = 0")
            .bind(reference)
            .execute(&mut *tx)
            .await?;
        for (position, id) in order.into_iter().enumerate() {
            sqlx::query("UPDATE shuffle_member SET used = 0, position = ? WHERE playlist_ref = ? AND media_uuid = ?")
                .bind(position as i64).bind(reference).bind(id).execute(&mut *tx).await?;
            known.insert(id.to_string(), (position as i64, false));
        }
        sqlx::query("UPDATE shuffle_cycle SET cycle = ? WHERE playlist_ref = ?")
            .bind(cycle)
            .bind(reference)
            .execute(&mut *tx)
            .await?;
    }
    let mut remaining: Vec<&Candidate> = eligible
        .iter()
        .filter(|c| known.get(&c.media_uuid).is_some_and(|(_, used)| !used))
        .collect();
    if let Some(max) = max {
        let had = !remaining.is_empty();
        remaining.retain(|c| c.duration_ms > 0 && c.duration_ms <= max);
        if had && remaining.is_empty() {
            // Transaction rollback includes RNG and rollover: the continuity
            // retry sees exactly the same bag, without losing long tracks.
            return Err(SelectionError::NoFit);
        }
        if let Some(best) = remaining.iter().map(|c| c.duration_ms).max() {
            remaining.retain(|c| c.duration_ms == best);
        }
    }
    let aired: Vec<String> = sqlx::query_scalar(
        "SELECT s.media_uuid FROM shuffle_member s WHERE s.playlist_ref = ? AND s.active = 1
         AND EXISTS (SELECT 1 FROM broadcast_log b WHERE b.media_uuid = s.media_uuid AND b.aired_at IS NOT NULL)")
        .bind(reference).fetch_all(&mut *tx).await?;
    let aired: HashSet<String> = aired.into_iter().collect();
    remaining.sort_by(|a, b| {
        (
            aired.contains(&a.media_uuid),
            known[&a.media_uuid].0,
            &a.media_uuid,
        )
            .cmp(&(
                aired.contains(&b.media_uuid),
                known[&b.media_uuid].0,
                &b.media_uuid,
            ))
    });
    let Some(chosen) = remaining.first() else {
        // Reconcile even when pending members are temporarily blocked, but
        // do not start a new cycle just to bypass constraints.
        tx.commit().await?;
        return Err(SelectionError::PoolEmpty);
    };
    let path = chosen.rel_path.clone();
    sqlx::query("UPDATE shuffle_member SET used = 1 WHERE playlist_ref = ? AND media_uuid = ?")
        .bind(reference)
        .bind(&chosen.media_uuid)
        .execute(&mut *tx)
        .await?;
    let reservation =
        sqlx::query("INSERT INTO shuffle_pick (playlist_ref, media_uuid, cycle) VALUES (?, ?, ?)")
            .bind(reference)
            .bind(&chosen.media_uuid)
            .bind(cycle)
            .execute(&mut *tx)
            .await?;
    let reservation = reservation.last_insert_rowid();
    tx.commit().await?;
    Ok((path, reservation))
}

/// Return a prepared track that Liquidsoap discarded. An already aired log
/// is never returned. If a newer live reservation exists for this UUID, it supersedes
/// the old reservation (important with one-track pools and cycle rollover).
pub async fn cancel(pool: &SqlitePool, log_id: i64) -> Result<(), sqlx::Error> {
    let reservation: Option<i64> =
        sqlx::query_scalar("SELECT id FROM shuffle_pick WHERE log_id = ?")
            .bind(log_id)
            .fetch_optional(pool)
            .await?;
    if let Some(id) = reservation {
        cancel_pick(pool, id).await?;
    }
    Ok(())
}

/// Exact reservation token, also usable before a file was logged. Never infer
/// the token from a path: concurrent pulls and rename races must not cancel
/// or attach somebody else's request.
pub async fn cancel_pick(pool: &SqlitePool, id: i64) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let changed = sqlx::query("UPDATE shuffle_pick SET settled = 2 WHERE id = ? AND settled = 0
        AND (log_id IS NULL OR EXISTS (SELECT 1 FROM broadcast_log b WHERE b.id = log_id AND b.aired_at IS NULL))")
        .bind(id).execute(&mut *tx).await?.rows_affected();
    if changed > 0 {
        sqlx::query("UPDATE shuffle_member SET used = 0 WHERE (playlist_ref, media_uuid) IN
            (SELECT p.playlist_ref, p.media_uuid FROM shuffle_pick p WHERE p.id = ?
             AND NOT EXISTS (SELECT 1 FROM shuffle_pick newer WHERE newer.playlist_ref = p.playlist_ref
                 AND newer.media_uuid = p.media_uuid AND newer.id > p.id AND newer.settled != 2))")
            .bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await
}

#[cfg(test)]
async fn pick(
    pool: &SqlitePool,
    reference: &str,
    base: &[Candidate],
    eligible: &[Candidate],
    max: Option<u64>,
) -> Result<String, SelectionError> {
    pick_reserved(pool, reference, base, eligible, max)
        .await
        .map(|(path, _)| path)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        broadcast_log::{self, Provenance},
        resolver::Epoch,
    };

    async fn setup(n: usize) -> (tempfile::TempDir, SqlitePool, Vec<Candidate>) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("bag.db")).await.unwrap();
        sqlx::query("UPDATE rng_seed SET seed = zeroblob(32)")
            .execute(&pool)
            .await
            .unwrap();
        let mut members = Vec::new();
        for i in 0..n {
            let path = format!("{i}.mp3");
            let id = format!("00000000-0000-4000-8000-{i:012}");
            sqlx::query("INSERT INTO media_identity (uuid, uri, confirmed) VALUES (?, ?, 1)")
                .bind(&id)
                .bind(&path)
                .execute(&pool)
                .await
                .unwrap();
            members.push(Candidate {
                media_uuid: id,
                rel_path: path,
                artist: None,
                title: None,
                album: None,
                year: None,
                duration_ms: (i as u64 + 1) * 60_000,
                genres: vec![],
                mtime_ns: 0,
            });
        }
        (dir, pool, members)
    }
    async fn next(pool: &SqlitePool, members: &[Candidate]) -> String {
        pick(pool, "music", members, members, None).await.unwrap()
    }
    async fn log(pool: &SqlitePool, path: &str, aired: bool) -> i64 {
        let reservation: Option<i64> = sqlx::query_scalar("SELECT id FROM shuffle_pick WHERE playlist_ref = 'music' AND media_uuid = (SELECT uuid FROM media_identity WHERE uri = ?) AND log_id IS NULL AND settled = 0 ORDER BY id DESC LIMIT 1")
            .bind(path).fetch_optional(pool).await.unwrap();
        let id = broadcast_log::record_pick(
            pool,
            path,
            None,
            Epoch(100),
            Provenance {
                leaf_ref: Some("music"),
                ..Default::default()
            },
            reservation,
        )
        .await
        .unwrap();
        if aired {
            broadcast_log::mark_aired(pool, id, Epoch(100))
                .await
                .unwrap();
        }
        id
    }

    #[tokio::test]
    async fn each_cycle_is_complete_and_first_positions_are_random() {
        let (_d, pool, members) = setup(5).await;
        let memory = crate::db::memory_copy(&_d.path().join("bag.db"))
            .await
            .unwrap();
        pool.close().await;
        let pool = memory;
        let mut firsts = [0usize; 5];
        for _ in 0..250 {
            let mut seen = HashSet::new();
            for j in 0..5 {
                let path = next(&pool, &members).await;
                if j == 0 {
                    firsts[path[..1].parse::<usize>().unwrap()] += 1;
                }
                assert!(seen.insert(path.clone()), "repeat in a cycle");
                log(&pool, &path, true).await;
            }
            assert_eq!(seen.len(), 5);
        }
        // Fixed seed, broad bounds: detect biased/deterministic order without
        // relying on fragile exact permutations or probabilistic test failure.
        assert!(firsts.iter().all(|n| (25..=80).contains(n)), "{firsts:?}");
    }

    #[tokio::test]
    async fn never_aired_beats_already_aired_and_unstarted_is_not_history() {
        let (_d, pool, members) = setup(5).await;
        for i in [0, 2, 4] {
            log(&pool, &members[i].rel_path, true).await;
        }
        log(&pool, &members[1].rel_path, false).await;
        let a = next(&pool, &members).await;
        let b = next(&pool, &members).await;
        assert_eq!(
            HashSet::from([a, b]),
            HashSet::from(["1.mp3".into(), "3.mp3".into()])
        );
        for _ in 0..3 {
            assert!(["0.mp3", "2.mp3", "4.mp3"].contains(&next(&pool, &members).await.as_str()));
        }
    }

    #[tokio::test]
    async fn temporary_exclusion_does_not_refill_or_lose_a_member() {
        let (_d, pool, members) = setup(3).await;
        let only = &members[..1];
        assert_eq!(
            pick(&pool, "music", &members, only, None).await.unwrap(),
            "0.mp3"
        );
        assert!(matches!(
            pick(&pool, "music", &members, only, None).await,
            Err(SelectionError::PoolEmpty)
        ));
        let mut seen = HashSet::new();
        seen.insert(next(&pool, &members).await);
        seen.insert(next(&pool, &members).await);
        assert_eq!(seen, HashSet::from(["1.mp3".into(), "2.mp3".into()]));
    }

    #[tokio::test]
    async fn duration_uses_pending_members_and_no_fit_changes_nothing() {
        let (d, pool, members) = setup(3).await;
        // Consume longest before entering the protected boundary.
        assert_eq!(
            pick(&pool, "music", &members, &members[2..], None)
                .await
                .unwrap(),
            "2.mp3"
        );
        assert_eq!(
            pick(&pool, "music", &members, &members, Some(180_000))
                .await
                .unwrap(),
            "1.mp3"
        );
        let before = crate::onair_sim::dump(&pool).await;
        assert!(matches!(
            pick(&pool, "music", &members, &members, Some(30_000)).await,
            Err(SelectionError::NoFit)
        ));
        assert_eq!(crate::onair_sim::dump(&pool).await, before);
        // TOPh may pull another bag; it must not reset the interrupted one.
        assert_eq!(
            pick(&pool, "toph", &members[2..], &members[2..], None)
                .await
                .unwrap(),
            "2.mp3"
        );
        let copy = crate::db::memory_copy(&d.path().join("bag.db"))
            .await
            .unwrap();
        assert_eq!(next(&copy, &members).await, "0.mp3");
        assert_eq!(next(&pool, &members).await, "0.mp3");
    }

    #[tokio::test]
    async fn dynamic_remove_readd_and_new_members_keep_the_cycle() {
        let (_d, pool, members) = setup(4).await;
        let initial = &members[..3];
        pick(&pool, "music", initial, &members[..1], None)
            .await
            .unwrap();
        // A tag edit temporarily removes the used track, then brings it back.
        let shortened = &members[1..3];
        pick(&pool, "music", shortened, &members[1..2], None)
            .await
            .unwrap();
        assert_eq!(
            pick(&pool, "music", &members, &members[2..3], None)
                .await
                .unwrap(),
            "2.mp3"
        );
        assert_eq!(next(&pool, &members).await, "3.mp3");
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT cycle FROM shuffle_cycle WHERE playlist_ref = 'music'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
        next(&pool, &members).await;
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT cycle FROM shuffle_cycle WHERE playlist_ref = 'music'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn restart_rename_and_simulation_keep_uuid_rotation() {
        let (d, pool, mut members) = setup(4).await;
        let first = next(&pool, &members).await;
        pool.close().await;
        let pool = crate::db::init(&d.path().join("bag.db")).await.unwrap();
        let chosen = members.iter_mut().find(|m| m.rel_path == first).unwrap();
        chosen.rel_path = "renamed.mp3".into();
        sqlx::query("UPDATE media_identity SET uri = 'renamed.mp3' WHERE uuid = ?")
            .bind(&chosen.media_uuid)
            .execute(&pool)
            .await
            .unwrap();
        let copy = crate::db::memory_copy(&d.path().join("bag.db"))
            .await
            .unwrap();
        let before = crate::onair_sim::dump(&pool).await;
        let mut simulated = Vec::new();
        for _ in 0..7 {
            simulated.push(next(&copy, &members).await);
        }
        assert_eq!(crate::onair_sim::dump(&pool).await, before);
        let mut actual = Vec::new();
        for _ in 0..7 {
            actual.push(next(&pool, &members).await);
        }
        assert_eq!(actual, simulated);
        assert!(!actual[..3].contains(&"renamed.mp3".into()));
    }

    #[tokio::test]
    async fn discarded_preparation_returns_but_aired_media_does_not() {
        let (_d, pool, members) = setup(3).await;
        let path = next(&pool, &members).await;
        let id = log(&pool, &path, false).await;
        cancel(&pool, id).await.unwrap();
        assert_eq!(next(&pool, &members).await, path);
        let id = log(&pool, &path, true).await;
        cancel(&pool, id).await.unwrap();
        assert_ne!(next(&pool, &members).await, path);
    }

    #[tokio::test]
    async fn concurrent_pulls_reserve_distinct_members() {
        let (_d, pool, members) = setup(4).await;
        let (a, b, c, d) = tokio::join!(
            next(&pool, &members),
            next(&pool, &members),
            next(&pool, &members),
            next(&pool, &members)
        );
        assert_eq!(HashSet::from([a, b, c, d]).len(), 4);
    }

    #[tokio::test]
    async fn missing_unlogged_media_can_return_without_being_consumed() {
        let (_d, pool, members) = setup(3).await;
        let (path, reservation) = pick_reserved(&pool, "music", &members, &members, None)
            .await
            .unwrap();
        cancel_pick(&pool, reservation).await.unwrap();
        assert_eq!(next(&pool, &members).await, path);
    }

    #[tokio::test]
    async fn permanently_removed_pending_member_does_not_hold_cycle_open() {
        let (_d, pool, members) = setup(3).await;
        pick(&pool, "music", &members, &members[..1], None)
            .await
            .unwrap();
        let retained = &members[..2];
        assert_eq!(next(&pool, retained).await, "1.mp3");
        next(&pool, retained).await;
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT cycle FROM shuffle_cycle WHERE playlist_ref = 'music'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM shuffle_member WHERE media_uuid = ?"
            )
            .bind(&members[2].media_uuid)
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn no_fit_at_cycle_end_does_not_advance_the_rng_or_cycle() {
        let (_d, pool, members) = setup(2).await;
        next(&pool, &members).await;
        next(&pool, &members).await;
        let before = crate::onair_sim::dump(&pool).await;
        assert!(matches!(
            pick(&pool, "music", &members, &members, Some(1)).await,
            Err(SelectionError::NoFit)
        ));
        assert_eq!(crate::onair_sim::dump(&pool).await, before);
    }
    #[tokio::test]
    async fn simulation_after_prefetch_accounts_for_its_first_air_before_rollover() {
        let (dir, pool, members) = setup(2).await;
        let media: Vec<_> = members
            .iter()
            .map(|c| crate::media::ScannedMedia {
                rel_path: c.rel_path.clone(),
                title: None,
                artist: None,
                album: None,
                year: None,
                genres: vec![],
                duration_ms: 60_000,
                size_bytes: 1,
                mtime_ns: 0,
            })
            .collect();
        crate::media_index::replace_library(&pool, &media, 100)
            .await
            .unwrap();
        let toml = "name = 'music'\n[selection]\nmode = 'static'\norder = 'shuffle'\nfiles = ['0.mp3', '1.mp3']\n";
        let playlist = crate::playlist::Playlist::parse(toml).unwrap();
        crate::store::upsert(&pool, "music", &playlist, toml, Some("music"))
            .await
            .unwrap();
        crate::grid_index::insert_rule(
            &pool,
            &crate::resolver::Rule {
                id: "floor".into(),
                enabled: true,
                validity: Default::default(),
                kind: crate::resolver::RuleKind::BaseRotation {
                    playlist_ref: "music".into(),
                },
            },
        )
        .await
        .unwrap();
        let engine = crate::grid_engine::GridEngine::new(pool.clone(), "UTC");
        engine.sync_grid().await.unwrap();
        let old = pick_reserved(&pool, "music", &members, &members[..1], None)
            .await
            .unwrap();
        let from = Provenance {
            leaf_ref: Some("music"),
            ..Default::default()
        };
        let old_log =
            broadcast_log::record_pick(&pool, &old.0, None, Epoch(100), from, Some(old.1))
                .await
                .unwrap();
        broadcast_log::mark_aired(&pool, old_log, Epoch(100))
            .await
            .unwrap();
        let prefetch = pick_reserved(&pool, "music", &members, &members[1..], None)
            .await
            .unwrap();
        let log = broadcast_log::record_pick(
            &pool,
            &prefetch.0,
            None,
            Epoch(10_000),
            from,
            Some(prefetch.1),
        )
        .await
        .unwrap();
        let before = crate::onair_sim::dump(&pool).await;
        let control = crate::station_control::StationControl::new_in_memory();
        let simulated = crate::onair_sim::simulate_after_prepared(
            crate::onair_sim::SimStart {
                live_db: &dir.path().join("bag.db"),
                tz: "UTC",
                control: &control,
                plugins: None,
                at: Epoch(10_060),
                at_known: true,
                count: 6,
            },
            Some((log, Epoch(10_000))),
        )
        .await;
        assert_eq!(crate::onair_sim::dump(&pool).await, before);
        engine.mark_aired(log, Epoch(10_000)).await.unwrap();
        engine.on_track_completed().await.unwrap();
        let mut actual = Vec::new();
        for n in 0..6 {
            let at = Epoch(10_060 + n * 60);
            let r = engine.next_media(at).await.unwrap();
            engine.mark_aired(r.log_id.unwrap(), at).await.unwrap();
            engine.on_track_completed().await.unwrap();
            actual.push(r.media_path.unwrap());
        }
        assert_eq!(
            actual,
            simulated
                .tracks
                .into_iter()
                .map(|t| t.media)
                .collect::<Vec<_>>()
        );
    }
    #[tokio::test]
    async fn exact_tokens_survive_out_of_order_logging_and_a_rename() {
        let (_d, pool, members) = setup(1).await;
        let (path, older) = pick_reserved(&pool, "music", &members, &members, None)
            .await
            .unwrap();
        let (_, newer) = pick_reserved(&pool, "music", &members, &members, None)
            .await
            .unwrap();
        sqlx::query("UPDATE media_identity SET uri = 'renamed.mp3'")
            .execute(&pool)
            .await
            .unwrap();
        let from = Provenance {
            leaf_ref: Some("music"),
            ..Default::default()
        };
        let newer_log =
            broadcast_log::record_pick(&pool, &path, None, Epoch(100), from, Some(newer))
                .await
                .unwrap();
        let older_log =
            broadcast_log::record_pick(&pool, &path, None, Epoch(100), from, Some(older))
                .await
                .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT log_id FROM shuffle_pick WHERE id = ?")
                .bind(older)
                .fetch_one(&pool)
                .await
                .unwrap(),
            older_log
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT log_id FROM shuffle_pick WHERE id = ?")
                .bind(newer)
                .fetch_one(&pool)
                .await
                .unwrap(),
            newer_log
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM media_identity")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1,
            "a stale URI must not create another UUID"
        );
        cancel(&pool, older_log).await.unwrap();
        assert!(
            sqlx::query_scalar::<_, bool>("SELECT used FROM shuffle_member")
                .fetch_one(&pool)
                .await
                .unwrap()
        );
        broadcast_log::mark_aired(&pool, newer_log, Epoch(101))
            .await
            .unwrap();
        cancel(&pool, older_log).await.unwrap();
        cancel(&pool, newer_log).await.unwrap();
        assert!(
            sqlx::query_scalar::<_, bool>("SELECT used FROM shuffle_member")
                .fetch_one(&pool)
                .await
                .unwrap()
        );
    }
    #[tokio::test]
    async fn cancelled_last_reservation_survives_rollover() {
        let (_d, pool, members) = setup(2).await;
        let a = next(&pool, &members).await;
        log(&pool, &a, true).await;
        let b = next(&pool, &members).await;
        let cancelled = log(&pool, &b, false).await;
        // A different UUID starts the following cycle before b is discarded.
        let just_a = members
            .iter()
            .filter(|m| m.rel_path == a)
            .cloned()
            .collect::<Vec<_>>();
        pick(&pool, "music", &members, &just_a, None).await.unwrap();
        log(&pool, &a, true).await;
        cancel(&pool, cancelled).await.unwrap();
        assert_eq!(next(&pool, &members).await, b);
    }
}
