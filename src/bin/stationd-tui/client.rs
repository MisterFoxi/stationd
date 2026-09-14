//! RPC adapter. Reads for polling; sync only on an explicit user action.
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

pub async fn sync_playlists(channel: Channel) -> ReadResult<station::PlaylistSyncReply> {
    let mut client = station::station_client::StationClient::new(channel);
    client.playlist_sync(station::PlaylistSyncRequest {}).await
        .map(|r| r.into_inner()).map_err(|e| format!("{}: {}", e.code(), e.message()))
}

pub async fn preview(channel: Channel, window: &super::agenda::Window) -> ReadResult<Vec<super::agenda::Entry>> {
    let mut client = schedule::schedule_service_client::ScheduleServiceClient::new(channel);
    let response = bounded(client.preview(schedule::PreviewRequest {
        from: Some(prost_types::Timestamp { seconds: window.from.as_second(), nanos: 0 }),
        window: Some(prost_types::Duration { seconds: window.to.as_second() - window.from.as_second(), nanos: 0 }),
    })).await?;
    super::agenda::entries(window, response)
}
