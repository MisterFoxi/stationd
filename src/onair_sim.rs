//! Simulation of what airs next — « les X morceaux théoriques à suivre ».
//!
//! The REAL engine, run on a throwaway in-memory copy of the station database
//! (`db::memory_copy`) with a detached copy of the station control (pending
//! overrides included) and the plugins in simulation mode. It pulls track
//! after track exactly like Liquidsoap would, advancing its clock by each
//! track's indexed duration; approaching a hard rendez-vous constrains the
//! it, like the air ticker does. Every side effect (cursors, group state,
//! holds, `Every` counters, rendez-vous tokens, queue buffers, the broadcast
//! log that feeds anti-repetition, `unplayed_only` marks) lands in the copy —
//! so the simulation is faithful — and nowhere else.
//!
//! Random draws are reproducible (`draw`): the copy draws what the real pull
//! will draw, so a shuffle airs as simulated. What it is NOT: a promise — an
//! override or a DJ can come in, the grid can be re-applied, a rescan can
//! change a pool. `notes` say why a simulation
//! stopped short; the caller adds what it knows about the station itself.
//!
//! Logs of the simulated run are silenced (it runs at every track change).

use std::path::Path;

use tracing::instrument::WithSubscriber;

use crate::grid_engine::{EngineError, GridEngine, ResolvedDecision};
use crate::plugin::PluginHandle;
use crate::resolver::Epoch;
use crate::selection::SelectionError;
use crate::station_control::StationControl;

/// What the simulation starts from.
pub struct SimStart<'a> {
    /// The live station database (copied, never written).
    pub live_db: &'a Path,
    /// Station timezone (IANA).
    pub tz: &'a str,
    /// The real station control: its pending overrides are copied.
    pub control: &'a StationControl,
    /// The real plugins: run in simulation mode (see `PluginHandle::simulation`).
    pub plugins: Option<&'a PluginHandle>,
    /// Instant the first simulated track starts.
    pub at: Epoch,
    /// Is `at` a real estimate? `false` = the start is unknown (a track of
    /// unknown length before it): no track gets an estimated start.
    pub at_known: bool,
    /// How many tracks to simulate.
    pub count: usize,
}

/// One simulated track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimTrack {
    /// Media path (relative to the media root), or the URL of a relay.
    pub media: String,
    pub stream: bool,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Indexed duration; `None` = unknown (a relay, a media outside the index).
    pub duration_ms: Option<i64>,
    /// Estimated start; `None` as soon as one duration before it is unknown.
    pub starts_at: Option<Epoch>,
    /// Cut by a hard rendez-vous at this instant.
    pub cut_at: Option<Epoch>,
    pub playlist_ref: Option<String>,
    pub leaf_ref: Option<String>,
    pub rule_id: Option<String>,
    /// `AtClockHard` … `BaseRotation`, `Override`, `Fallback`.
    pub origin: String,
    pub override_source: Option<String>,
}

/// Why what follows may differ, or stops short. A closed set of OPCODES with
/// typed parameters — never a sentence: each client words it in its own
/// language (`onair_v1.proto`, `Note.Code`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    StationPaused,
    StationSleeping,
    SleepAtTrackEnd,
    SleepArmed,
    LiveOnAir { dj: String },
    NoLiquidsoap,
    Simulated,
    PoolEmpty,
    Fallback,
    StreamUnknownDuration { media: String },
    UnknownDuration { media: String },
    SimulationFailed { reason: String },
    PluginFilterFailed { plugin: String, reason: String },
    GridProjectionFailed { reason: String },
    HistoryUnreadable { reason: String },
    /// Predicted by the simulation: a hard rendez-vous will not cut (its
    /// source will produce nothing), first at `at`.
    RendezvousWillNotCut { rule: String, playlist: String, at: i64 },
    /// Predicted: a due rule will produce nothing (empty pool, or emptied by
    /// the constraints / plugins), first at `at`.
    SourceWillBeEmpty { rule: String, playlist: String, at: i64 },
    /// Seen on the air (last hour): a hard rendez-vous did not cut.
    RendezvousNotCut { rule: String, playlist: String, at: i64, count: u32 },
    /// Seen on the air (last hour): a due rule produced nothing.
    SourceWasEmpty { rule: String, playlist: String, at: i64, count: u32 },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SimOutcome {
    pub tracks: Vec<SimTrack>,
    /// Grid incidents met on the way (predicted): a rule that will produce
    /// nothing, a hard rendez-vous that will not cut.
    pub incidents: Vec<crate::station_control::Incident>,
    /// Why it stopped short, and what went wrong in the plugins.
    pub notes: Vec<Note>,
}

/// Run the simulation. Never fails: whatever prevents it is a note.
pub async fn simulate(start: SimStart<'_>) -> SimOutcome {
    let silent = tracing::subscriber::NoSubscriber::default();
    let plugins = start.plugins.map(PluginHandle::simulation);
    let mut out = run(&start, plugins.as_ref()).with_subscriber(silent).await;
    if let Some(p) = &plugins {
        out.notes
            .extend(p.simulation_notes().into_iter().map(|(plugin, reason)| Note::PluginFilterFailed { plugin, reason }));
    }
    out
}

async fn run(start: &SimStart<'_>, plugins: Option<&PluginHandle>) -> SimOutcome {
    let mut out = SimOutcome::default();
    if start.count == 0 {
        return out;
    }
    let pool = match crate::db::memory_copy(start.live_db).await {
        Ok(p) => p,
        Err(e) => {
            out.notes.push(Note::SimulationFailed { reason: e.to_string() });
            return out;
        }
    };
    // The copy airs from `start.at` on (its control is `running`): a halt the
    // real station is in ends there, or it would freeze the anti-repetition
    // windows of the whole simulation (`air_time`).
    if let Err(e) = crate::air_time::mark(&pool, false, start.at.0).await {
        out.notes.push(Note::SimulationFailed { reason: e.to_string() });
        return out;
    }
    let control = start.control.simulation_copy();
    let mut engine = GridEngine::new(pool.clone(), start.tz).with_control(control.clone());
    if let Some(p) = plugins {
        engine = engine.with_plugins(p.clone());
    }

    let mut t = start.at;
    let mut known = start.at_known;
    while out.tracks.len() < start.count {
        let r = match engine.next_media(t).await {
            Ok(r) => r,
            Err(EngineError::Selection(SelectionError::PoolEmpty)) => {
                out.notes.push(Note::PoolEmpty);
                break;
            }
            Err(e) => {
                out.notes.push(Note::SimulationFailed { reason: e.to_string() });
                break;
            }
        };
        if r.halted.is_some() {
            break; // cannot happen on the running copy; never loop on it
        }
        let Some(media) = r.media_path.clone() else {
            out.tracks.push(track(&r, String::new(), None, known.then_some(t)));
            out.notes.push(Note::Fallback);
            break;
        };
        let brief = if r.stream {
            None
        } else {
            crate::media_index::brief(&pool, &media).await.ok().flatten()
        };
        let tr = track(&r, media.clone(), brief, known.then_some(t));
        // A track starting: the `Every` track counters move, as on the air.
        let _ = engine.on_track_completed().await;

        let Some(d_ms) = tr.duration_ms.filter(|d| *d > 0) else {
            out.notes.push(if r.stream {
                Note::StreamUnknownDuration { media: media.clone() }
            } else {
                Note::UnknownDuration { media: media.clone() }
            });
            out.tracks.push(tr);
            known = false;
            // Keep simulating the ORDER from a nominal instant: better than
            // nothing, and every later start stays unestimated.
            t = Epoch(t.0 + 1);
            continue;
        };
        let end = Epoch(t.0 + (d_ms + 999) / 1000);

        out.tracks.push(tr);
        let _ = engine
            .track_left(r.log_id, &media, r.leaf_ref.as_deref(), end.0 - t.0, end)
            .await;
        t = end;
    }
    out.incidents = control.incidents_since(Epoch(0));
    pool.close().await;
    out
}

type Brief = (Option<String>, Option<String>, Option<String>, i64);

fn track(r: &ResolvedDecision, media: String, brief: Option<Brief>, starts_at: Option<Epoch>) -> SimTrack {
    let (title, artist, album, duration_ms) = match brief {
        Some((t, a, al, d)) => (t, a, al, Some(d)),
        None => (None, None, None, None),
    };
    let origin = if r.override_source.is_some() {
        "Override".to_string()
    } else {
        format!("{:?}", r.decision.origin)
    };
    SimTrack {
        media,
        stream: r.stream,
        title,
        artist,
        album,
        duration_ms,
        starts_at,
        cut_at: None,
        playlist_ref: r.decision.playlist_ref.clone(),
        leaf_ref: r.leaf_ref.clone(),
        rule_id: r.decision.rule_id.clone(),
        origin,
        override_source: r.override_source.clone(),
    }
}

/// Every table of `pool`, dumped in a stable order — the tests' proof that a
/// simulation wrote nothing.
#[cfg(test)]
pub(crate) async fn dump(pool: &sqlx::SqlitePool) -> Vec<(String, Vec<String>)> {
    let tables: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let mut out = Vec::new();
    for (t,) in tables {
        let cols: Vec<(String,)> = sqlx::query_as(&format!("SELECT name FROM pragma_table_info('{t}')"))
            .fetch_all(pool)
            .await
            .unwrap();
        let list = cols
            .iter()
            .map(|(c,)| format!("CASE WHEN typeof(\"{c}\") = 'blob' THEN hex(\"{c}\") ELSE \"{c}\" END"))
            .collect::<Vec<_>>()
            .join(", ");
        let rows: Vec<(String,)> =
            sqlx::query_as(&format!("SELECT json_array({list}) AS r FROM \"{t}\" ORDER BY r"))
                .fetch_all(pool)
                .await
                .unwrap();
        out.push((t, rows.into_iter().map(|(r,)| r).collect()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid_index::insert_rule;
    use crate::resolver::{ClockAnchor, Mode, Rule, RuleKind, Validity};
    use crate::station_control::{OverrideContent, OverrideRequest};

    fn rule(id: &str, kind: RuleKind) -> Rule {
        Rule { id: id.into(), enabled: true, validity: Validity::default(), kind }
    }

    fn at(h: i64, m: i64) -> Epoch {
        Epoch(h * 3600 + m * 60)
    }

    /// Music (5 tracks of `music_s` seconds) on the floor, news hard every 15 min.
    async fn station(music_s: u64) -> (tempfile::TempDir, std::path::PathBuf, sqlx::SqlitePool) {
        station_with(music_s, "music", &[]).await
    }

    /// Like [`station`], with `floor` on the floor and `extra` playlists (key, TOML).
    async fn station_with(
        music_s: u64,
        floor: &str,
        extra: &[(&str, &str)],
    ) -> (tempfile::TempDir, std::path::PathBuf, sqlx::SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.db");
        let pool = crate::db::init(&path).await.unwrap();
        let m = |p: &str, d: u64| crate::media::ScannedMedia {
            rel_path: p.into(),
            title: Some(format!("T {p}")),
            artist: Some(format!("A {p}")),
            album: None,
            year: None,
            genres: vec![],
            duration_ms: d * 1000,
            size_bytes: 1,
            mtime_ns: 0,
        };
        let mut lib: Vec<_> = (1..=5).map(|i| m(&format!("music/{i}.mp3"), music_s)).collect();
        lib.push(m("news/n.mp3", 60));
        crate::media_index::replace_library(&pool, &lib, 1000).await.unwrap();
        for r in ["music", "news"] {
            let toml = format!(
                "name = \"{r}\"\n[selection]\nmode = \"dynamic\"\norder = \"shuffle\"\n\
                 [[selection.filter]]\nfield = \"path\"\nop = \"prefix\"\nvalue = \"{r}/\"\n"
            );
            let pl = crate::playlist::Playlist::parse(&toml).unwrap();
            crate::store::upsert(&pool, r, &pl, &toml, Some(r)).await.unwrap();
        }
        for (r, toml) in extra {
            let pl = crate::playlist::Playlist::parse(toml).unwrap();
            crate::store::upsert(&pool, r, &pl, toml, Some(r)).await.unwrap();
        }
        insert_rule(&pool, &rule("floor", RuleKind::BaseRotation { playlist_ref: floor.into() }))
            .await
            .unwrap();
        insert_rule(
            &pool,
            &rule(
                "news",
                RuleKind::AtClock {
                    playlist_ref: "news".into(),
                    anchor: ClockAnchor::EveryMinutes(15),
                    mode: Mode::Hard,
                    // Short: a pull at 09:10 does not air the 09:00 mark late.
                    expiry_secs: Some(120),
                },
            ),
        )
        .await
        .unwrap();
        GridEngine::new(pool.clone(), "UTC").sync_grid().await.unwrap();
        (dir, path, pool)
    }

    #[tokio::test]
    async fn a_simulation_writes_nothing_anywhere() {
        let (_d, path, pool) = station(60).await;
        let control = StationControl::new_in_memory();
        control
            .push_override(
                OverrideRequest {
                    content: OverrideContent::Media("news/n.mp3".into()),
                    mode: Default::default(),
                    expiry: None,
                    tracks: None,
                },
                "cli",
            )
            .unwrap();
        let before = dump(&pool).await;
        let overrides = control.list_overrides();

        let out = simulate(SimStart {
            live_db: &path,
            tz: "UTC",
            control: &control,
            plugins: None,
            at: at(9, 3),
            at_known: true,
            count: 12,
        })
        .await;

        assert_eq!(dump(&pool).await, before, "the live database is untouched");
        assert_eq!(control.list_overrides(), overrides, "the override queue is untouched");
        assert_eq!(out.tracks.len(), 12, "notes: {:?}", out.notes);
        assert_eq!(out.tracks[0].origin, "Override", "the pending override airs first");
        assert_eq!(out.tracks[0].media, "news/n.mp3");
        let starts: Vec<i64> = out.tracks.iter().map(|t| t.starts_at.unwrap().0).collect();
        assert!(starts.windows(2).all(|w| w[1] > w[0]), "starts move forward: {starts:?}");
        assert_eq!(out.tracks[1].title.as_deref().map(|t| t.starts_with("T music/")), Some(true));
        // Anti-repetition inside the simulation is the engine's own: running
        // it twice from the same state gives the same length, and still no write.
        let again = simulate(SimStart {
            live_db: &path,
            tz: "UTC",
            control: &control,
            plugins: None,
            at: at(9, 3),
            at_known: true,
            count: 3,
        })
        .await;
        assert_eq!(again.tracks.len(), 3);
        assert_eq!(dump(&pool).await, before);
    }

    #[tokio::test]
    async fn hard_boundary_selects_tracks_that_fit_when_available() {
        let (_d, path, pool) = station(420).await;
        sqlx::query("UPDATE media SET duration_ms = 240000 WHERE rel_path = 'music/1.mp3'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE media SET duration_ms = 60000 WHERE rel_path = 'music/2.mp3'")
            .execute(&pool)
            .await
            .unwrap();
        let toml = "name = \"music\"\n[selection]\nmode = \"static\"\norder = \"sequential\"\nfiles = [\"music/1.mp3\", \"music/2.mp3\", \"music/3.mp3\", \"music/4.mp3\", \"music/5.mp3\"]\n";
        let pl = crate::playlist::Playlist::parse(toml).unwrap();
        crate::store::upsert(&pool, "music", &pl, toml, Some("music"))
            .await
            .unwrap();

        let control = StationControl::new_in_memory();
        let out = simulate(SimStart {
            live_db: &path,
            tz: "UTC",
            control: &control,
            plugins: None,
            at: at(9, 10),
            at_known: true,
            count: 3,
        })
        .await;

        assert_eq!(out.tracks.len(), 3, "notes: {:?}", out.notes);
        assert_eq!(out.tracks[0].media, "music/1.mp3");
        assert_eq!(out.tracks[0].starts_at, Some(at(9, 10)));
        assert_eq!(out.tracks[1].media, "music/2.mp3");
        assert_eq!(out.tracks[1].starts_at, Some(at(9, 14)));
        assert_eq!(out.tracks[2].origin, "AtClockHard");
        assert_eq!(out.tracks[2].media, "news/n.mp3");
        assert_eq!(out.tracks[2].starts_at, Some(at(9, 15)));
        assert!(out.tracks.iter().all(|t| t.cut_at.is_none()));
    }

    #[tokio::test]
    async fn a_hard_rendez_vous_never_cuts_the_simulated_track() {
        // 7-minute tracks from 09:10: none fits before 09:15, so continuity
        // wins. The hard rendez-vous airs at the next boundary, 09:17.
        let (_d, path, _pool) = station(420).await;
        let control = StationControl::new_in_memory();
        let out = simulate(SimStart {
            live_db: &path,
            tz: "UTC",
            control: &control,
            plugins: None,
            at: at(9, 10),
            at_known: true,
            count: 3,
        })
        .await;
        assert_eq!(out.tracks.len(), 3, "notes: {:?}", out.notes);
        assert_eq!(out.tracks[0].cut_at, None);
        assert_eq!(out.tracks[1].origin, "AtClockHard");
        assert_eq!(out.tracks[1].starts_at, Some(at(9, 17)));
        assert_eq!(out.tracks[1].media, "news/n.mp3");
        assert_eq!(out.tracks[2].starts_at, Some(Epoch(at(9, 17).0 + 60)), "back to music after the news");
    }

    #[tokio::test]
    async fn an_unknown_start_gives_no_estimated_times() {
        let (_d, path, _pool) = station(60).await;
        let control = StationControl::new_in_memory();
        let out = simulate(SimStart {
            live_db: &path,
            tz: "UTC",
            control: &control,
            plugins: None,
            at: at(9, 1),
            at_known: false,
            count: 2,
        })
        .await;
        assert_eq!(out.tracks.len(), 2);
        assert!(out.tracks.iter().all(|t| t.starts_at.is_none()));
    }

    #[tokio::test]
    async fn an_unreadable_database_is_a_note_not_a_failure() {
        let control = StationControl::new_in_memory();
        let out = simulate(SimStart {
            live_db: Path::new("/nonexistent/dir/live.db"),
            tz: "UTC",
            control: &control,
            plugins: None,
            at: at(9, 0),
            at_known: true,
            count: 5,
        })
        .await;
        assert!(out.tracks.is_empty());
        assert!(matches!(out.notes[..], [Note::SimulationFailed { .. }]), "{:?}", out.notes);
    }

    #[tokio::test]
    async fn a_rendez_vous_with_nothing_to_air_is_predicted() {
        // 7-minute music; the news playlist points at a file that is not
        // there (unavailable): the 09:15 hard mark will not cut.
        let (_d, path, pool) = station(420).await;
        sqlx::query("UPDATE media SET available = 0 WHERE rel_path = 'news/n.mp3'")
            .execute(&pool)
            .await
            .unwrap();
        let control = StationControl::new_in_memory();
        let out = simulate(SimStart {
            live_db: &path,
            tz: "UTC",
            control: &control,
            plugins: None,
            at: at(9, 10),
            at_known: true,
            count: 3,
        })
        .await;
        assert!(out.tracks.iter().all(|t| t.cut_at.is_none()), "no cut");
        let hard = out
            .incidents
            .iter()
            .find(|i| {
                i.kind == crate::station_control::IncidentKind::SourceEmpty
                    && i.rule_id.as_deref() == Some("news")
            })
            .expect("predicted");
        assert_eq!((hard.rule_id.as_deref(), hard.playlist_ref.as_str(), hard.first_at), (Some("news"), "news", at(9, 17)));
        assert!(control.incidents_since(Epoch(0)).is_empty(), "nothing recorded on the real station");
    }

    /// What the real station airs from `at`: the same engine calls as the air
    /// (pull, track start, track left at its end), on the LIVE database.
    async fn airs(pool: &sqlx::SqlitePool, at: Epoch, count: usize) -> Vec<String> {
        let engine = GridEngine::new(pool.clone(), "UTC");
        let mut t = at;
        let mut out = Vec::new();
        for _ in 0..count {
            let r = engine.next_media(t).await.unwrap();
            let media = r.media_path.clone().unwrap();
            engine.on_track_completed().await.unwrap();
            let (_, _, _, d) = crate::media_index::brief(pool, &media).await.unwrap().unwrap();
            let end = Epoch(t.0 + (d + 999) / 1000);
            engine.track_left(r.log_id, &media, r.leaf_ref.as_deref(), end.0 - t.0, end).await.unwrap();
            out.push(media);
            t = end;
        }
        out
    }

    async fn simulated(path: &Path, at: Epoch, count: usize) -> Vec<String> {
        let control = StationControl::new_in_memory();
        let out = simulate(SimStart { live_db: path, tz: "UTC", control: &control, plugins: None, at, at_known: true, count })
            .await;
        assert_eq!(out.tracks.len(), count, "notes: {:?}", out.notes);
        out.tracks.into_iter().map(|t| t.media).collect()
    }

    #[tokio::test]
    async fn what_is_simulated_is_what_airs_from_a_shuffle() {
        let (_d, path, pool) = station(60).await;
        // Some history first: the simulation starts mid-way, not from a fresh station.
        airs(&pool, at(9, 1), 2).await;
        let sim = simulated(&path, at(9, 3), 10).await;
        assert_eq!(airs(&pool, at(9, 3), 10).await, sim);
    }

    #[tokio::test]
    async fn what_is_simulated_is_what_airs_from_groups() {
        // A shuffle group (permutation drawn per cycle) holding a weighted group.
        let (_d, path, pool) = station_with(
            60,
            "mix",
            &[
                (
                    "mix",
                    "name = \"mix\"\n[selection]\nmode = \"group\"\nstrategy = \"shuffle\"\n\
                     members = [{ ref = \"w\", take = 2 }, { ref = \"music\", take = 1 }, { ref = \"news\", take = 1 }]\n",
                ),
                (
                    "w",
                    "name = \"w\"\n[selection]\nmode = \"group\"\nstrategy = \"weighted\"\n\
                     members = [{ ref = \"music\", weight = 30 }, { ref = \"news\", weight = 10 }]\n",
                ),
            ],
        )
        .await;
        airs(&pool, at(9, 1), 3).await;
        let sim = simulated(&path, at(9, 4), 12).await;
        assert_eq!(airs(&pool, at(9, 4), 12).await, sim);
    }
}
