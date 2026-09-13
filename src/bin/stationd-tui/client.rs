//! Read-only RPC adapter. No filesystem, SQL, resolver or mutation calls here.
use std::time::Duration;

use stationd::proto::{schedule, station};
use tonic::transport::Channel;

pub type ReadResult<T> = Result<T, String>;

pub struct Snapshot {
    pub status: ReadResult<station::StatusReply>,
    pub playlists: ReadResult<Vec<station::PlaylistSummary>>,
    pub rules: ReadResult<Vec<schedule::Rule>>,
}

async fn bounded<T>(
    call: impl std::future::Future<Output = Result<tonic::Response<T>, tonic::Status>>,
) -> ReadResult<T> {
    match tokio::time::timeout(Duration::from_secs(5), call).await {
        Ok(Ok(reply)) => Ok(reply.into_inner()),
        Ok(Err(error)) => Err(format!("{}: {}", error.code(), error.message())),
        Err(_) => Err("RPC timed out after 5s".into()),
    }
}

pub async fn refresh(channel: Channel) -> Snapshot {
    let mut status_client = station::station_client::StationClient::new(channel.clone());
    let mut playlist_client = station::station_client::StationClient::new(channel.clone());
    let mut schedule_client = schedule::schedule_service_client::ScheduleServiceClient::new(channel);
    let (status, playlists, rules) = tokio::join!(
        bounded(status_client.status(station::StatusRequest {})),
        bounded(playlist_client.playlist_list(station::PlaylistListRequest {})),
        bounded(schedule_client.list_rules(schedule::ListRulesRequest {})),
    );
    Snapshot {
        status,
        playlists: playlists.map(|r| r.playlists),
        rules: rules.map(|r| r.rules),
    }
}
