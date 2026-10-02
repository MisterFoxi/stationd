//! Sleep-policy continuity between consecutive complete Icecast polls.
//! Raw listener snapshots and the connection journal keep the real sessions.
use crate::listener_snapshot::{Connection, Listener};
use std::collections::HashMap;
use std::time::{Duration, Instant};

type Endpoint = (String, std::net::IpAddr, Option<String>);
struct Session {
    raw_id: String,
    raw_age: u64,
    logical_id: String,
    age: u64,
    since: Instant,
}
#[derive(Default)]
pub(super) struct Continuity {
    previous: HashMap<Endpoint, Vec<Session>>,
    observed: Option<Instant>,
    next_id: u64,
}
impl Continuity {
    pub(super) fn sample(
        &mut self,
        clients: Option<&[(String, Listener)]>,
        now: Instant,
        max_interval: Duration,
    ) -> Option<Vec<Connection>> {
        let Some(clients) = clients else {
            // Partial/failed polls cannot establish reconnect continuity.
            self.observed = None;
            return None;
        };
        let elapsed = self.observed.map(|at| now.saturating_duration_since(at));
        let fresh = elapsed.is_some_and(|dt| dt <= max_interval);
        let mut grouped: HashMap<Endpoint, Vec<&Listener>> = HashMap::new();
        for (mount, client) in clients {
            grouped
                .entry((mount.clone(), client.ip, client.user_agent.clone()))
                .or_default()
                .push(client);
        }
        let mut connections = Vec::with_capacity(clients.len());
        let mut next = HashMap::new();
        for (key, clients) in grouped {
            let mut previous = self.previous.remove(&key).unwrap_or_default();
            let unique = previous.len() == 1 && clients.len() == 1;
            let mut sessions = Vec::new();
            for client in clients {
                let same = previous
                    .iter()
                    .position(|s| s.raw_id == client.id && client.connected_seconds >= s.raw_age);
                let replaced = fresh && unique && same.is_none()
                    // Estimated start within the unobserved interval, with
                    // one second of tolerance for Icecast's integer ages.
                    && elapsed.is_some_and(|dt| client.connected_seconds <= dt.as_secs().saturating_add(1))
                    && key.2.as_ref().is_some_and(|ua| !ua.is_empty());
                let session = if let Some(index) = same.or_else(|| replaced.then_some(0)) {
                    let old = previous.remove(index);
                    Session {
                        raw_id: client.id.clone(),
                        raw_age: client.connected_seconds,
                        logical_id: old.logical_id,
                        age: old.age,
                        since: old.since,
                    }
                } else {
                    self.next_id += 1;
                    Session {
                        raw_id: client.id.clone(),
                        raw_age: client.connected_seconds,
                        logical_id: format!("sleep:{}", self.next_id),
                        age: client.connected_seconds,
                        since: now,
                    }
                };
                connections.push(Connection {
                    mount: key.0.clone(),
                    id: session.logical_id.clone(),
                    connected_seconds: client.connected_seconds.max(
                        session
                            .age
                            .saturating_add(now.saturating_duration_since(session.since).as_secs()),
                    ),
                });
                sessions.push(session);
            }
            next.insert(key, sessions);
        }
        // An observed departure ends continuity; no departed-client cache.
        self.previous = next;
        self.observed = Some(now);
        Some(connections)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::station_control::{BroadcastState, Gate, StationControl};
    fn client(id: &str, age: u64) -> (String, Listener) {
        (
            "/radio".into(),
            Listener {
                id: id.into(),
                ip: "192.0.2.1".parse().unwrap(),
                connected_seconds: age,
                user_agent: Some("SL audio".into()),
            },
        )
    }
    fn poll(t: &mut Continuity, clients: &[(String, Listener)], at: Instant) -> Vec<Connection> {
        t.sample(Some(clients), at, Duration::from_secs(30))
            .unwrap()
    }
    #[test]
    fn repeated_reconnects_preserve_age_and_do_not_wake_sleep() {
        let mut t = Continuity::default();
        let at = Instant::now();
        let c = StationControl::new_in_memory();
        c.sample_listeners(1);
        let first = poll(&mut t, &[client("1", 100)], at);
        c.sample_connections(Some(first.clone()), Duration::from_secs(30));
        c.stop_when_connections_old(100, "plugin").unwrap();
        assert_eq!(c.gate(), Gate::Halt(BroadcastState::Sleeping));
        for (seconds, id, age) in [(15, "2", 3), (30, "3", 0), (45, "3", 15)] {
            let view = poll(
                &mut t,
                &[client(id, age)],
                at + Duration::from_secs(seconds),
            );
            assert_eq!(view[0].id, first[0].id);
            assert_eq!(view[0].connected_seconds, 100 + seconds);
            c.sample_connections(Some(view), Duration::from_secs(30));
            assert_eq!(c.state(), BroadcastState::Sleeping);
        }
        let mut arrival = client("4", 0);
        arrival.1.ip = "192.0.2.2".parse().unwrap();
        let view = poll(
            &mut t,
            &[client("3", 16), arrival],
            at + Duration::from_secs(46),
        );
        c.sample_connections(Some(view), Duration::from_secs(30));
        assert_eq!(c.state(), BroadcastState::Running);
    }
    #[test]
    fn departures_failures_and_stale_polls_end_continuity() {
        let at = Instant::now();
        for mode in 0..3 {
            let mut t = Continuity::default();
            let first = poll(&mut t, &[client("1", 100)], at);
            let gap = match mode {
                0 => {
                    poll(&mut t, &[], at + Duration::from_secs(1));
                    2
                }
                1 => {
                    t.sample(None, at + Duration::from_secs(1), Duration::from_secs(30));
                    2
                }
                _ => 31,
            };
            let next = poll(&mut t, &[client("2", 0)], at + Duration::from_secs(gap));
            assert_ne!(next[0].id, first[0].id);
            assert_eq!(next[0].connected_seconds, 0);
        }
    }
    #[test]
    fn distinct_or_ambiguous_clients_do_not_inherit_age() {
        let at = Instant::now();
        for mode in 0..7 {
            let mut t = Continuity::default();
            let mut before = vec![client("1", 100)];
            let mut after = vec![client("2", 0)];
            match mode {
                0 => after[0].0 = "/other".into(),
                1 => after[0].1.ip = "192.0.2.2".parse().unwrap(),
                2 => after[0].1.user_agent = Some("other".into()),
                3 => {
                    before[0].1.user_agent = None;
                    after[0].1.user_agent = None;
                }
                4 => before.push(client("3", 120)),
                5 => after.push(client("3", 0)),
                _ => after[0].1.connected_seconds = 17,
            }
            let first = poll(&mut t, &before, at);
            let next = poll(&mut t, &after, at + Duration::from_secs(15));
            assert!(next.iter().all(|c| first.iter().all(|old| old.id != c.id)));
            assert!(next.iter().all(|c| c.connected_seconds < 100));
        }
    }
    #[test]
    fn unchanged_raw_session_survives_a_transient_failure() {
        let mut t = Continuity::default();
        let at = Instant::now();
        let first = poll(&mut t, &[client("1", 100)], at);
        t.sample(None, at + Duration::from_secs(15), Duration::from_secs(30));
        let next = poll(&mut t, &[client("1", 130)], at + Duration::from_secs(30));
        assert_eq!(first[0].id, next[0].id);
        assert_eq!(next[0].connected_seconds, 130);
    }
    #[test]
    fn reused_raw_id_can_continue_only_for_the_same_endpoint() {
        let mut t = Continuity::default();
        let at = Instant::now();
        let first = poll(&mut t, &[client("1", 100)], at);
        let next = poll(&mut t, &[client("1", 0)], at + Duration::from_secs(15));
        assert_eq!(first[0].id, next[0].id);
        assert_eq!(next[0].connected_seconds, 115);
    }
}
