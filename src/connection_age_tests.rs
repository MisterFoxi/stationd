use super::*;
use crate::listener_snapshot::Connection;
use std::time::Duration;

fn client(mount: &str, id: &str, age: u64) -> Connection {
    Connection { mount: mount.into(), id: id.into(), connected_seconds: age }
}
fn sample(c: &StationControl, clients: Vec<Connection>) {
    c.sample_connections(Some(clients), Duration::from_secs(30));
}
fn sleeping() -> StationControl {
    let c = StationControl::new_in_memory();
    c.sample_listeners(2);
    sample(&c, vec![client("/a", "1", 100), client("/b", "1", 120)]);
    c.stop_when_connections_old(100, "plugin").unwrap();
    assert_eq!(c.gate(), Gate::Halt(BroadcastState::Sleeping));
    c
}

#[test]
fn sleeps_with_several_old_clients_and_wakes_on_a_new_mount_scoped_id() {
    let c = sleeping();
    sample(&c, vec![client("/a", "1", 110), client("/b", "1", 130)]);
    assert_eq!(c.state(), BroadcastState::Sleeping);
    // Departures alone do not wake.
    sample(&c, vec![client("/a", "1", 115)]);
    assert_eq!(c.state(), BroadcastState::Sleeping);
    sample(&c, vec![client("/a", "1", 116), client("/b", "2", 0)]);
    assert_eq!(c.state(), BroadcastState::Running);
}

#[test]
fn rechecks_new_arrivals_and_unknown_data_at_the_boundary() {
    let c = StationControl::new_in_memory();
    c.sample_listeners(1);
    sample(&c, vec![client("/a", "1", 100)]);
    c.stop_when_connections_old(100, "plugin").unwrap();
    sample(&c, vec![client("/a", "1", 101), client("/b", "2", 1)]);
    assert_eq!(c.gate(), Gate::Play);
    c.sample_connections(None, Duration::from_secs(30));
    assert_eq!(c.gate(), Gate::Play);
    sample(&c, vec![client("/a", "1", 102)]);
    c.clear_listeners();
    assert_eq!(c.gate(), Gate::Play);
    c.sample_listeners(1);
    assert_eq!(c.gate(), Gate::Halt(BroadcastState::Sleeping));
}

#[test]
fn stale_snapshots_cannot_stop_the_air() {
    let c = StationControl::new_in_memory();
    c.sample_listeners(1);
    c.sample_connections(Some(vec![client("/a", "1", 100)]), Duration::ZERO);
    c.stop_when_connections_old(100, "plugin").unwrap();
    assert_eq!(c.listener_connections(), None);
    assert_eq!(c.gate(), Gate::Play);
}

#[test]
fn detail_failure_or_reused_id_with_younger_age_wakes() {
    let c = sleeping();
    c.sample_connections(None, Duration::from_secs(30));
    assert_eq!(c.state(), BroadcastState::Running);
    let c = sleeping();
    sample(&c, vec![client("/a", "1", 1)]);
    assert_eq!(c.state(), BroadcastState::Running);
}

#[test]
fn operator_resume_clears_the_age_guard_and_pause_is_respected() {
    let c = sleeping();
    c.apply(ControlAction::Resume, "cli").unwrap();
    c.apply(ControlAction::StopWhenIdle, "cli").unwrap();
    assert_eq!(c.gate(), Gate::Play, "operator drain still needs zero listeners");
    c.apply(ControlAction::Pause, "cli").unwrap();
    sample(&c, vec![client("/b", "3", 0)]);
    assert_eq!(c.state(), BroadcastState::Paused);
    assert!(c.stop_when_connections_old(100, "plugin").is_err());
    assert!(c.stop_when_connections_old(0, "plugin").is_err());
}

#[test]
fn empty_snapshot_is_eligible_and_manual_sleep_has_legacy_semantics() {
    let c = StationControl::new_in_memory();
    c.sample_listeners(0);
    sample(&c, vec![]);
    c.stop_when_connections_old(100, "plugin").unwrap();
    assert_eq!(c.gate(), Gate::Halt(BroadcastState::Sleeping));
    sample(&c, vec![client("/a", "1", 0)]);
    assert_eq!(c.state(), BroadcastState::Running);
}
