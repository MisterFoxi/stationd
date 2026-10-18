//! Wire-independent Icecast stream snapshots; unavailable is distinct from disconnected.
use serde::{Deserialize, Serialize};
use crate::{registry::ServiceKey, RequestState};

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
pub const ICECAST: ServiceKey<dyn IcecastService> = ServiceKey::new("stationdeck.icecast.streams", "1.0.0");
