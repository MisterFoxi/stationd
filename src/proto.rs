//! Shared generated contract: daemon, CLI and TUI use the same types/clients.
pub mod station {
    tonic::include_proto!("station");
}

pub mod schedule {
    tonic::include_proto!("webradio.schedule.v1");
}

pub mod library {
    tonic::include_proto!("webradio.library.v1");
}

pub mod plugin {
    tonic::include_proto!("webradio.plugin.v1");
}
