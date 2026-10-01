//! Connection lifecycle facts from complete, privacy-reduced Icecast polls.
//! A missing poll is not a departure. End times are observation times.

use std::collections::BTreeMap;
use crate::events::{self, Code, Component, Level};
use crate::listener_snapshot::Connection;

#[derive(Default)]
pub(super) struct Tracker {
    previous: BTreeMap<(String, String), Seen>,
}

struct Seen {
    connected_seconds: u64,
    started_at: i64,
    last_seen_at: i64,
}

pub(super) struct Change {
    pub code: Code,
    pub mount: String,
    pub id: String,
    pub connected_seconds: u64,
    pub started_at: i64,
    pub last_seen_at: i64,
    pub observed_at: i64,
}

impl Change {
    pub fn record(self) {
        events::record(Level::Info, Component::Icecast, self.code, [
            ("mount", self.mount), ("id", self.id),
            ("connected_seconds", self.connected_seconds.to_string()),
            ("started_at", self.started_at.to_string()),
            ("last_seen_at", self.last_seen_at.to_string()),
            ("observed_at", self.observed_at.to_string()),
        ]);
    }
}

impl Tracker {
    pub fn observe(&mut self, at: i64, clients: Option<&[Connection]>) -> Vec<Change> {
        let Some(clients) = clients else { return Vec::new() };
        let current: BTreeMap<_, _> = clients.iter()
            .map(|c| ((c.mount.clone(), c.id.clone()), c)).collect();
        let mut changes = Vec::new();
        // End the old session before announcing a reused id.
        for ((mount, id), old) in &self.previous {
            if current.get(&(mount.clone(), id.clone()))
                .is_none_or(|c| c.connected_seconds < old.connected_seconds)
            {
                changes.push(Change {
                    code: Code::ConnectionEnded, mount: mount.clone(), id: id.clone(),
                    connected_seconds: old.connected_seconds, started_at: old.started_at,
                    last_seen_at: old.last_seen_at, observed_at: at,
                });
            }
        }
        let mut next = BTreeMap::new();
        for ((mount, id), c) in current {
            let old = self.previous.get(&(mount.clone(), id.clone()))
                .filter(|old| c.connected_seconds >= old.connected_seconds);
            let started_at = old.map(|old| old.started_at).unwrap_or_else(||
                at.saturating_sub(i64::try_from(c.connected_seconds).unwrap_or(i64::MAX)));
            if old.is_none() {
                changes.push(Change {
                    code: Code::ConnectionStarted, mount: mount.clone(), id: id.clone(),
                    connected_seconds: c.connected_seconds, started_at, last_seen_at: at, observed_at: at,
                });
            }
            next.insert((mount, id), Seen { connected_seconds: c.connected_seconds, started_at, last_seen_at: at });
        }
        self.previous = next;
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(mount: &str, id: &str, seconds: u64) -> Connection {
        Connection { mount: mount.into(), id: id.into(), connected_seconds: seconds }
    }

    #[test]
    fn tracks_estimated_start_and_observed_end_without_repeating() {
        let mut tracker = Tracker::default();
        let begin = tracker.observe(1000, Some(&[c("/a", "1", 600)]));
        assert_eq!(begin.len(), 1);
        assert_eq!(begin[0].code, Code::ConnectionStarted);
        assert_eq!(begin[0].started_at, 400, "already active when stationd starts");
        assert!(tracker.observe(1015, Some(&[c("/a", "1", 615)])).is_empty());
        let end = tracker.observe(1030, Some(&[]));
        assert_eq!(end.len(), 1);
        assert_eq!(end[0].code, Code::ConnectionEnded);
        assert_eq!(end[0].started_at, 400);
        assert_eq!(end[0].last_seen_at, 1015);
        assert_eq!(end[0].observed_at, 1030);
        assert_eq!(end[0].connected_seconds, 615, "last reported duration, not a guessed duration");
        assert!(tracker.observe(1045, Some(&[])).is_empty());
    }

    #[test]
    fn unknown_polls_do_not_invent_ends_or_duplicate_beginnings() {
        let mut tracker = Tracker::default();
        tracker.observe(1000, Some(&[c("/a", "1", 10)]));
        assert!(tracker.observe(1015, None).is_empty());
        assert!(tracker.observe(1100, Some(&[c("/a", "1", 110)])).is_empty());
        assert!(tracker.observe(1115, None).is_empty());
        let end = tracker.observe(1200, Some(&[]));
        assert_eq!(end.len(), 1);
        assert_eq!(end[0].last_seen_at, 1100);
        assert_eq!(end[0].observed_at, 1200);
    }


    #[test]
    fn connection_events_reach_the_journal_and_rpc_without_client_metadata() {
        use crate::events::{self, Code};
        use crate::resolver::Epoch;
        use crate::station_control::StationControl;
        use std::time::Duration;
        let c = StationControl::new_in_memory();
        c.set_clock(Some(Epoch(1000)));
        c.sample_connections(Some(vec![Connection {
            mount: "/connection-events-test".into(), id: "42".into(), connected_seconds: 600,
        }]), Duration::from_secs(30));
        c.set_clock(Some(Epoch(1015)));
        c.sample_connections(None, Duration::from_secs(30));
        c.set_clock(Some(Epoch(1030)));
        c.sample_connections(Some(vec![]), Duration::from_secs(30));
        let (backlog, _) = events::subscribe(events::CAPACITY);
        let mine: Vec<_> = backlog.into_iter()
            .filter(|e| e.param("mount") == Some("/connection-events-test")).collect();
        assert_eq!(mine.len(), 2, "no end during an unknown poll");
        assert_eq!(mine[0].code, Code::ConnectionStarted);
        assert_eq!(mine[1].code, Code::ConnectionEnded);
        assert_eq!(mine[1].param("started_at"), Some("400"));
        assert_eq!(mine[1].param("last_seen_at"), Some("1000"));
        assert_eq!(mine[1].param("observed_at"), Some("1030"));
        for event in &mine {
            assert_eq!(event.params.len(), 6);
            assert!(event.params.iter().all(|(name, _)| matches!(name.as_str(),
                "mount" | "id" | "connected_seconds" | "started_at" | "last_seen_at" | "observed_at")));
            let wire = crate::events_grpc::to_proto(event);
            let expected = if event.code == Code::ConnectionStarted {
                crate::proto::events::event::Code::ConnectionStarted
            } else {
                crate::proto::events::event::Code::ConnectionEnded
            };
            assert_eq!(wire.code, expected as i32);
            assert_eq!(wire.params.len(), 6);
        }
    }

    #[test]
    fn mount_scopes_ids_and_reused_ids_end_then_start() {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.observe(1000, Some(&[c("/a", "1", 100), c("/b", "1", 200)])).len(), 2);
        let changes = tracker.observe(1015, Some(&[c("/a", "1", 1), c("/b", "1", 215)]));
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].code, Code::ConnectionEnded);
        assert_eq!(changes[1].code, Code::ConnectionStarted);
        assert_eq!(changes[1].started_at, 1014);
        assert_eq!(changes[0].mount, "/a");
        assert_eq!(changes[1].mount, "/a");
    }
}
