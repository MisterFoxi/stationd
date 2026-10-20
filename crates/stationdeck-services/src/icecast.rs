//! Wire-independent Icecast stream snapshots; unavailable is distinct from disconnected.
use crate::{registry::ServiceKey, RequestState};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StreamStatus {
    pub mount: String,
    pub connected: bool,
    pub listeners: u64,
    pub title: String,
    pub content_type: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct IcecastStatus {
    pub server_id: String,
    pub streams: Vec<StreamStatus>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum IcecastResponse {
    Connected { status: IcecastStatus },
    Unavailable { code: String, message: String },
}
/// A refresh is nonblocking; progress and availability are read through snapshot.
/// No UI, transport types, endpoints or upstream credentials cross this boundary.
pub trait IcecastService {
    fn snapshot(&self) -> RequestState<IcecastStatus>;
    fn refresh(&self) -> Result<(), String>;
}
pub const ICECAST: ServiceKey<dyn IcecastService> =
    ServiceKey::new("stationdeck.icecast.streams", "1.0.0");

/// Management has a separate service and credential from stream supervision.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum IcecastCommand {
    Route,
    Listeners {
        mount: String,
    },
    Metadata {
        mount: String,
        title: String,
    },
    KickSource {
        mount: String,
    },
    KickListener {
        mount: String,
        id: u64,
    },
    Switch {
        input: String,
    },
    MigrationPlan {
        mount: String,
        destination: String,
        #[serde(default)]
        continuous: bool,
    },
    MigrationRun {
        id: u64,
    },
    MigrationReturn {
        id: u64,
    },
    MigrationCancel {
        id: u64,
    },
    Migrations,
}
impl IcecastCommand {
    pub fn validate(&self) -> Result<(), &'static str> {
        fn mount_ok(mount: &str) -> bool {
            mount.starts_with('/') && mount.len() <= 256 && !mount.chars().any(char::is_control)
        }
        match self {
            Self::Route | Self::Migrations => Ok(()),
            Self::MigrationPlan {
                mount, destination, ..
            } if mount_ok(mount) && mount_ok(destination) && mount != destination => Ok(()),
            Self::MigrationRun { id }
            | Self::MigrationReturn { id }
            | Self::MigrationCancel { id }
                if *id > 0 =>
            {
                Ok(())
            }
            Self::Switch { input }
                if !input.is_empty()
                    && input.len() <= 128
                    && !input.chars().any(char::is_control) =>
            {
                Ok(())
            }
            Self::Metadata { mount, title }
                if mount_ok(mount)
                    && title.len() <= 1024
                    && !title.chars().any(char::is_control) =>
            {
                Ok(())
            }
            Self::Listeners { mount }
            | Self::KickSource { mount }
            | Self::KickListener { mount, .. }
                if mount_ok(mount) =>
            {
                Ok(())
            }
            _ => Err("Invalid Icecast management request."),
        }
    }
    pub fn changes_stream(&self) -> bool {
        !matches!(
            self,
            Self::Route | Self::Listeners { .. } | Self::Migrations | Self::MigrationPlan { .. }
        )
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RouteInput {
    pub id: String,
    pub mount: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RouteStatus {
    pub output_mount: String,
    pub input: String,
    pub connected: bool,
    pub inputs: Vec<String>,
    pub problem: String,
    #[serde(default)]
    pub input_streams: Vec<RouteInput>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Listener {
    pub id: u64,
    pub ip: String,
    pub user_agent: String,
    pub connected_seconds: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IcecastResult {
    Migration {
        operation: ListenerMigration,
    },
    Migrations {
        operations: Vec<ListenerMigration>,
    },
    Route {
        route: RouteStatus,
    },
    Listeners {
        mount: String,
        listeners: Vec<Listener>,
    },
    Done {
        message: String,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ControlResponse {
    Completed { result: IcecastResult },
    Unavailable { code: String, message: String },
}
pub trait IcecastControlService {
    fn available(&self) -> bool;
    fn snapshot(&self) -> RequestState<IcecastResult>;
    fn submit(&self, command: IcecastCommand) -> Result<(), String>;
}
pub const ICECAST_CONTROL: ServiceKey<dyn IcecastControlService> =
    ServiceKey::new("stationdeck.icecast.control", "1.0.0");
#[cfg(test)]
mod control_tests {
    use super::*;
    #[test]
    fn route_mapping_is_optional_for_older_administration_services() {
        let route: RouteStatus = serde_json::from_str(r#"{"output_mount":"/radio.mp3","input":"b","connected":true,"inputs":["a","b"],"problem":""}"#).unwrap();
        assert!(route.input_streams.is_empty());
    }
    #[test]
    fn requests_reject_invalid_targets_and_control_characters() {
        assert!(IcecastCommand::KickSource {
            mount: "http://arbitrary/".into()
        }
        .validate()
        .is_err());
        assert!(IcecastCommand::Metadata {
            mount: "/live".into(),
            title: "bad\ntitle".into()
        }
        .validate()
        .is_err());
        assert!(IcecastCommand::Switch {
            input: String::new()
        }
        .validate()
        .is_err());
        assert!(IcecastCommand::Metadata {
            mount: "/live".into(),
            title: "Artist — Title".into()
        }
        .validate()
        .is_ok());
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ListenerMigration {
    #[serde(default)]
    pub continuous: bool,
    pub id: u64,
    pub mount: String,
    pub destination: String,
    pub state: String,
    pub total: u32,
    pub moved: u32,
    pub returned: u32,
    pub missing: u32,
    pub uncertain: u32,
    pub created_at: u64,
    pub note: String,
}
