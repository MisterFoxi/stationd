//! gRPC transport for the station journal (events_v1.proto): a thin
//! translator over [`crate::events`]. Read-only.
//!
//! A followed `Watch` ends when the daemon starts to shut down (`stopping`),
//! like the on-air stream, so a TUI left open never holds the shutdown.

use tokio::sync::{broadcast, mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use crate::events::{self, Code, Component, Event, Level};
pub use crate::proto::events as proto;
use proto::event_service_server::EventService;

pub struct EventsGrpc {
    stopping: watch::Receiver<bool>,
}

impl EventsGrpc {
    pub fn new(stopping: watch::Receiver<bool>) -> Self {
        Self { stopping }
    }
}

pub fn to_proto(e: &Event) -> proto::Event {
    use proto::event::{Code as C, Component as P, Level as L};
    let level = match e.level {
        Level::Info => L::Info,
        Level::Warn => L::Warn,
        Level::Error => L::Error,
    };
    let component = match e.component {
        Component::Station => P::Station,
        Component::Broadcast => P::Broadcast,
        Component::Grid => P::Grid,
        Component::Library => P::Library,
        Component::Plugin => P::Plugin,
        Component::Live => P::Live,
        Component::Liquidsoap => P::Liquidsoap,
        Component::Icecast => P::Icecast,
    };
    let code = match e.code {
        Code::Log => C::Log,
        Code::Started => C::Started,
        Code::Stopping => C::Stopping,
        Code::BroadcastState => C::BroadcastState,
        Code::Listeners => C::Listeners,
        Code::AudienceUnknown => C::AudienceUnknown,
        Code::ConnectionStarted => C::ConnectionStarted,
        Code::ConnectionEnded => C::ConnectionEnded,
        Code::TrackChosen => C::TrackChosen,
        Code::OverridePushed => C::OverridePushed,
        Code::OverrideDropped => C::OverrideDropped,
        Code::GridApplied => C::GridApplied,
        Code::GridRefused => C::GridRefused,
        Code::GridIncident => C::GridIncident,
        Code::LiveStarted => C::LiveStarted,
        Code::LiveEnded => C::LiveEnded,
        Code::ScanStarted => C::ScanStarted,
        Code::ScanFinished => C::ScanFinished,
        Code::ScanFailed => C::ScanFailed,
        Code::TagsWritten => C::TagsWritten,
        Code::TagRenamed => C::TagRenamed,
        Code::PluginFailed => C::PluginFailed,
        Code::PluginQuarantined => C::PluginQuarantined,
        Code::PluginState => C::PluginState,
        Code::BpmAnalyzed => C::BpmAnalyzed,
    };
    proto::Event {
        seq: e.seq,
        at_ms: e.at_ms,
        level: level as i32,
        component: component as i32,
        code: code as i32,
        params: e.params.iter().map(|(name, value)| proto::Param { name: name.clone(), value: value.clone() }).collect(),
    }
}

#[tonic::async_trait]
impl EventService for EventsGrpc {
    type WatchStream = ReceiverStream<Result<proto::Event, Status>>;

    async fn watch(&self, request: Request<proto::WatchEventsRequest>) -> Result<Response<Self::WatchStream>, Status> {
        let r = request.into_inner();
        let backlog = if r.backlog == 0 { 200 } else { (r.backlog as usize).min(events::CAPACITY) };
        let (past, mut rx) = events::subscribe(backlog);
        let (tx, out) = mpsc::channel(64);
        let mut stopping = self.stopping.clone();
        let follow = r.follow;
        tokio::spawn(async move {
            let mut last = 0;
            for e in &past {
                last = e.seq;
                if tx.send(Ok(to_proto(e))).await.is_err() {
                    return;
                }
            }
            if !follow {
                return;
            }
            loop {
                tokio::select! {
                    _ = async { stopping.wait_for(|s| *s).await.is_ok() } => break,
                    got = rx.recv() => match got {
                        // Already sent with the backlog (recorded between the
                        // two halves of `subscribe`): skip.
                        Ok(e) if e.seq <= last => {}
                        Ok(e) => {
                            last = e.seq;
                            if tx.send(Ok(to_proto(&e))).await.is_err() {
                                break;
                            }
                        }
                        // A slow watcher: say it lost some, carry on.
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            let gap = proto::Event {
                                seq: last,
                                at_ms: 0,
                                level: proto::event::Level::Warn as i32,
                                component: proto::event::Component::Station as i32,
                                code: proto::event::Code::Log as i32,
                                params: vec![
                                    proto::Param { name: "message".into(), value: format!("{n} events skipped (watcher too slow)") },
                                    proto::Param { name: "target".into(), value: "stationd::events_grpc".into() },
                                ],
                            };
                            if tx.send(Ok(gap)).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                }
            }
        });
        Ok(Response::new(ReceiverStream::new(out)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_stream::StreamExt;

    #[tokio::test]
    async fn watch_sends_the_backlog_then_follows() {
        events::record(Level::Info, Component::Library, Code::ScanStarted, [("mark", "grpc-a")]);
        let (stop_tx, stop_rx) = watch::channel(false);
        let svc = EventsGrpc::new(stop_rx);
        let mut s = svc
            .watch(Request::new(proto::WatchEventsRequest { backlog: 2000, follow: true }))
            .await
            .unwrap()
            .into_inner();
        events::record(Level::Warn, Component::Library, Code::ScanFailed, [("mark", "grpc-b")]);
        let mut seen = Vec::new();
        while seen.len() < 2 {
            let e = tokio::time::timeout(std::time::Duration::from_secs(2), s.next()).await.unwrap().unwrap().unwrap();
            if let Some(p) = e.params.iter().find(|p| p.name == "mark" && p.value.starts_with("grpc-")) {
                seen.push((p.value.clone(), e.code));
            }
        }
        assert_eq!(
            seen,
            vec![
                ("grpc-a".to_string(), proto::event::Code::ScanStarted as i32),
                ("grpc-b".to_string(), proto::event::Code::ScanFailed as i32)
            ]
        );
        // Shutdown ends the stream.
        stop_tx.send(true).unwrap();
        let end = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while s.next().await.is_some() {}
        })
        .await;
        assert!(end.is_ok());
    }
}
