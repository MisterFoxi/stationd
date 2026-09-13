//! Shared generated contract: daemon, CLI and TUI use the same types/clients.
pub mod station {
    tonic::include_proto!("station");
}

pub mod schedule {
    tonic::include_proto!("webradio.schedule.v1");
}
