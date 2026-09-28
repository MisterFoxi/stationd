//! gRPC transport for the on-air view (onair_v1.proto).
//!
//! Thin translator over `onair::OnAirHub`: each `Watch` stream follows the
//! hub's snapshots and cuts them to what its client asked for; `History`
//! reads the broadcast log. Read-only.
//!
//! A `Watch` stream never ends on its own: it ends when the daemon starts to
//! shut down (`stopping`), otherwise the server's graceful shutdown would
//! wait for the watching clients (a TUI left open) forever.

use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use crate::onair::{self, Note, OnAirHub, Outcome, Slot, SlotIssue, Snapshot, Track};
pub use crate::proto::onair as proto;
use proto::on_air_service_server::OnAirService;
use proto::{HistoryRequest, HistoryResponse, OnAirSnapshot, WatchRequest};

pub struct OnAirGrpc {
    hub: OnAirHub,
    /// `true` once the daemon shuts down: open `Watch` streams end.
    stopping: watch::Receiver<bool>,
}

impl OnAirGrpc {
    pub fn new(hub: OnAirHub, stopping: watch::Receiver<bool>) -> Self {
        Self { hub, stopping }
    }
}

/// `0` → default, then capped.
fn wanted(v: u32, default: usize, max: usize) -> usize {
    if v == 0 { default } else { (v as usize).min(max) }
}

#[tonic::async_trait]
impl OnAirService for OnAirGrpc {
    type WatchStream = ReceiverStream<Result<OnAirSnapshot, Status>>;

    async fn watch(&self, request: Request<WatchRequest>) -> Result<Response<Self::WatchStream>, Status> {
        let r = request.into_inner();
        let cut = Cut {
            upcoming: wanted(r.upcoming, 10, onair::UPCOMING_MAX),
            history: wanted(r.history, 20, onair::HISTORY_MAX),
            playlists: wanted(r.playlists_ahead, 5, onair::PLAYLISTS_MAX),
        };
        let mut rx = self.hub.subscribe();
        // What the hub holds may predate this watcher: wait for the fresh
        // snapshot the subscription triggered.
        rx.borrow_and_update();
        let (tx, out) = mpsc::channel(4);
        let mut stopping = self.stopping.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    // Shutdown: end the stream (its sender dropped), the
                    // client sees a clean end and reconnects later.
                    _ = async { stopping.wait_for(|s| *s).await.is_ok() } => break,
                    r = rx.changed() => {
                        if r.is_err() {
                            break;
                        }
                        let snap = rx.borrow_and_update().clone();
                        if let Some(s) = snap {
                            if tx.send(Ok(to_proto(&s, cut))).await.is_err() {
                                break; // client gone
                            }
                        }
                    }
                }
            }
        });
        Ok(Response::new(ReceiverStream::new(out)))
    }

    async fn history(&self, request: Request<HistoryRequest>) -> Result<Response<HistoryResponse>, Status> {
        let r = request.into_inner();
        let before = r.before.unwrap_or(i64::MAX);
        let limit = wanted(r.limit, 50, 500);
        let tracks = self
            .hub
            .history(before, limit)
            .await
            .map_err(|e| Status::internal(format!("could not read the broadcast log: {e}")))?;
        Ok(Response::new(HistoryResponse { tracks: tracks.iter().map(track).collect() }))
    }
}

#[derive(Debug, Clone, Copy)]
struct Cut {
    upcoming: usize,
    history: usize,
    playlists: usize,
}

fn to_proto(s: &Snapshot, cut: Cut) -> OnAirSnapshot {
    // The prepared track is the first of the `upcoming` count.
    let sim_room = cut.upcoming.saturating_sub(usize::from(s.prefetched.is_some()));
    OnAirSnapshot {
        revision: s.revision,
        observed_at: s.observed_at,
        state: s.state.clone(),
        listeners: s.listeners,
        on_air_kind: s.on_air_kind.clone(),
        on_air: s.on_air.as_ref().map(track),
        prefetched: s.prefetched.as_ref().map(track),
        upcoming: s.upcoming.iter().take(sim_room).map(track).collect(),
        notes: s.notes.iter().map(note).collect(),
        history: s.history.iter().take(cut.history).map(track).collect(),
        current_playlist: s.current_playlist.as_ref().map(slot),
        next_playlists: s.next_playlists.iter().take(cut.playlists).map(slot).collect(),
        indicative: s.indicative.iter().map(slot).collect(),
        live_dj: s.live_dj.clone().unwrap_or_default(),
        pending_overrides: s.pending_overrides,
        liquidsoap: s.liquidsoap,
        timezone: s.timezone.clone(),
    }
}

fn track(t: &Track) -> proto::Track {
    use proto::track::Outcome as P;
    proto::Track {
        rel_path: t.rel_path.clone(),
        title: t.title.clone().unwrap_or_default(),
        artist: t.artist.clone().unwrap_or_default(),
        album: t.album.clone().unwrap_or_default(),
        duration_ms: t.duration_ms.filter(|d| *d > 0).map(|d| d as u64),
        started_at: t.started_at,
        estimated_at: t.estimated_at,
        playlist_ref: t.playlist_ref.clone().unwrap_or_default(),
        leaf_ref: t.leaf_ref.clone().unwrap_or_default(),
        rule_id: t.rule_id.clone().unwrap_or_default(),
        origin: t.origin.clone().unwrap_or_default(),
        override_source: t.override_source.clone().unwrap_or_default(),
        outcome: match t.outcome {
            Outcome::None => P::Unspecified,
            Outcome::Aired => P::Aired,
            Outcome::Cut => P::Cut,
            Outcome::Unknown => P::Unknown,
        } as i32,
        stream: t.stream,
        cut_at: t.cut_at,
    }
}

fn slot(s: &Slot) -> proto::PlaylistSlot {
    proto::PlaylistSlot {
        playlist_ref: s.playlist_ref.clone(),
        rule_id: s.rule_id.clone().unwrap_or_default(),
        origin: s.origin.clone().unwrap_or_default(),
        from: s.from,
        at_local: s.at_local.clone().unwrap_or_default(),
        issue: match s.issue {
            SlotIssue::None => proto::playlist_slot::Issue::None,
            SlotIssue::PoolEmpty => proto::playlist_slot::Issue::PoolEmpty,
            SlotIssue::NothingPlayable => proto::playlist_slot::Issue::NothingPlayable,
        } as i32,
    }
}

/// Domain note → opcode + typed parameters (no text leaves stationd).
fn note(n: &Note) -> proto::Note {
    use proto::note::Code as C;
    let mut p = proto::Note::default();
    let code = match n {
        Note::StationPaused => C::StationPaused,
        Note::StationSleeping => C::StationSleeping,
        Note::SleepAtTrackEnd => C::SleepAtTrackEnd,
        Note::SleepArmed => C::SleepArmed,
        Note::LiveOnAir { dj } => {
            p.dj = dj.clone();
            C::LiveOnAir
        }
        Note::NoLiquidsoap => C::NoLiquidsoap,
        Note::Simulated => C::Simulated,
        Note::PoolEmpty => C::PoolEmpty,
        Note::Fallback => C::Fallback,
        Note::StreamUnknownDuration { media } => {
            p.media = media.clone();
            C::StreamUnknownDuration
        }
        Note::UnknownDuration { media } => {
            p.media = media.clone();
            C::UnknownDuration
        }
        Note::SimulationFailed { reason } => {
            p.reason = reason.clone();
            C::SimulationFailed
        }
        Note::PluginFilterFailed { plugin, reason } => {
            p.plugin = plugin.clone();
            p.reason = reason.clone();
            C::PluginFilterFailed
        }
        Note::GridProjectionFailed { reason } => {
            p.reason = reason.clone();
            C::GridProjectionFailed
        }
        Note::HistoryUnreadable { reason } => {
            p.reason = reason.clone();
            C::HistoryUnreadable
        }
        Note::RendezvousWillNotCut { rule, playlist, at } => {
            (p.rule, p.playlist, p.at) = (rule.clone(), playlist.clone(), Some(*at));
            C::RendezvousWillNotCut
        }
        Note::SourceWillBeEmpty { rule, playlist, at } => {
            (p.rule, p.playlist, p.at) = (rule.clone(), playlist.clone(), Some(*at));
            C::SourceWillBeEmpty
        }
        Note::RendezvousNotCut { rule, playlist, at, count } => {
            (p.rule, p.playlist, p.at, p.count) = (rule.clone(), playlist.clone(), Some(*at), *count);
            C::RendezvousNotCut
        }
        Note::SourceWasEmpty { rule, playlist, at, count } => {
            (p.rule, p.playlist, p.at, p.count) = (rule.clone(), playlist.clone(), Some(*at), *count);
            C::SourceWasEmpty
        }
    };
    p.code = code as i32;
    p
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio_stream::StreamExt;

    use super::*;

    #[tokio::test]
    async fn a_watch_stream_ends_when_the_daemon_shuts_down() {
        let (_d, hub, _control) = crate::onair::tests::hub().await;
        let (stop, stopping) = watch::channel(false);
        let svc = OnAirGrpc::new(hub, stopping);
        let req = WatchRequest { upcoming: 1, history: 1, playlists_ahead: 1 };
        let mut stream = svc.watch(Request::new(req)).await.unwrap().into_inner();
        let first = tokio::time::timeout(Duration::from_secs(10), stream.next()).await.expect("a snapshot");
        assert!(matches!(first, Some(Ok(_))));
        stop.send(true).unwrap();
        let end = tokio::time::timeout(Duration::from_secs(5), async {
            while stream.next().await.is_some() {}
        })
        .await;
        assert!(end.is_ok(), "the stream must end at shutdown, not hold the server");
    }
}
