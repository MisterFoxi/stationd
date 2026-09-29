//! Actions sur la station : chacune est UN appel gRPC, exactement celui de
//! `stationctl`. La TUI ne décide rien : elle envoie, puis affiche ce que
//! stationd répond (le vrai effet se lit ensuite dans le bandeau et l'antenne).
//!
//! Une action mutante n'est jamais rejouée automatiquement (dossier §4.2).

use stationd_proto::{broadcast, library, live, plugin, schedule, station};
use tonic::transport::Channel;

use crate::rpc;
use crate::tr;

/// Contenu d'un override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverrideContent {
    Media(String),
    Playlist(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginVerb {
    Start,
    Stop,
    Restart,
    Reload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Pause,
    Resume,
    StopWhenIdle,
    Wake,
    Skip,
    PushOverride { content: OverrideContent, hard: bool, expiry: String, tracks: u32 },
    /// `None` = toute la file.
    ClearOverrides(Option<u64>),
    LiveKick,
    LiveOpen { dj: String, duration: String },
    LiveClose { dj: String },
    Enqueue { playlist: String, media: String },
    Scan,
    Plugin { name: String, verb: PluginVerb },
    Shutdown { force: bool },
}

impl Action {
    /// Une opération qui peut durer (scan de la bibliothèque) : canal sans
    /// délai maximal.
    pub fn is_long(&self) -> bool {
        matches!(self, Action::Scan)
    }
}

/// Ce qu'une action a donné : message traduit pour la ligne de statut, et,
/// pour un scan, son rapport.
#[derive(Debug, Clone)]
pub struct Done {
    pub message: String,
    pub scan: Option<library::ScanResponse>,
    /// stationd s'arrête (arrêt opérateur) : la perte de liaison qui suit est
    /// attendue, elle ne doit pas effacer ce message.
    pub exits: bool,
}

impl Done {
    fn msg(message: String) -> Self {
        Self { message, scan: None, exits: false }
    }
}

fn state_label(v: i32) -> String {
    match broadcast::State::try_from(v).unwrap_or(broadcast::State::Unspecified) {
        broadcast::State::Running => tr!("state-running"),
        broadcast::State::Paused => tr!("state-paused"),
        broadcast::State::Draining => tr!("state-draining"),
        broadcast::State::Sleeping => tr!("state-sleeping"),
        broadcast::State::Unspecified => tr!("state-unknown"),
    }
}

fn err(status: tonic::Status) -> String {
    rpc::status_text(&status)
}

/// Exécute `action`. `Err` = texte d'erreur (déjà traduit ou relayé de stationd).
pub async fn run(action: Action, channel: Channel, tz: Option<jiff::tz::TimeZone>) -> Result<Done, String> {
    use broadcast::broadcast_service_client::BroadcastServiceClient as Bc;
    match action {
        Action::Pause | Action::Resume | Action::StopWhenIdle | Action::Wake => {
            let a = match action {
                Action::Pause => broadcast::control_request::Action::Pause,
                Action::Resume => broadcast::control_request::Action::Resume,
                Action::StopWhenIdle => broadcast::control_request::Action::StopWhenIdle,
                _ => broadcast::control_request::Action::Wake,
            };
            let r = Bc::new(channel)
                .control(broadcast::ControlRequest { action: a as i32 })
                .await
                .map_err(err)?
                .into_inner();
            Ok(Done::msg(if r.changed {
                tr!("done-state", from = state_label(r.from), to = state_label(r.to))
            } else {
                tr!("done-state-unchanged", state = state_label(r.to))
            }))
        }
        Action::Skip => {
            Bc::new(channel).skip(broadcast::SkipRequest {}).await.map_err(err)?;
            Ok(Done::msg(tr!("done-skip")))
        }
        Action::PushOverride { content, hard, expiry, tracks } => {
            use broadcast::push_override_request::{Content, Mode};
            let content = match content {
                OverrideContent::Media(m) => Content::MediaPath(m),
                OverrideContent::Playlist(p) => Content::PlaylistRef(p),
            };
            let r = Bc::new(channel)
                .push_override(broadcast::PushOverrideRequest {
                    content: Some(content),
                    mode: if hard { Mode::Hard } else { Mode::Soft } as i32,
                    expiry,
                    tracks,
                })
                .await
                .map_err(err)?
                .into_inner();
            Ok(Done::msg(if r.degraded {
                tr!("done-override-degraded", id = r.id, pending = r.pending)
            } else {
                tr!("done-override", id = r.id, pending = r.pending)
            }))
        }
        Action::ClearOverrides(id) => {
            let r = Bc::new(channel)
                .clear_overrides(broadcast::ClearOverridesRequest { id: id.unwrap_or(0) })
                .await
                .map_err(err)?
                .into_inner();
            Ok(Done::msg(tr!("done-overrides-cleared", n = r.removed)))
        }
        Action::LiveKick => {
            let r = live::live_service_client::LiveServiceClient::new(channel)
                .kick(live::KickRequest {})
                .await
                .map_err(err)?
                .into_inner();
            Ok(Done::msg(tr!("done-live-kicked", dj = r.dj)))
        }
        Action::LiveOpen { dj, duration } => {
            let r = live::live_service_client::LiveServiceClient::new(channel)
                .open(live::OpenRequest { dj: dj.clone(), duration })
                .await
                .map_err(err)?
                .into_inner();
            let until = r.opening.map(|o| o.until).unwrap_or_default();
            Ok(Done::msg(tr!("done-live-opened", dj = dj, until = crate::store::local_hms(tz.as_ref(), until).unwrap_or_default())))
        }
        Action::LiveClose { dj } => {
            live::live_service_client::LiveServiceClient::new(channel)
                .close(live::CloseRequest { dj: dj.clone() })
                .await
                .map_err(err)?;
            Ok(Done::msg(tr!("done-live-closed", dj = dj)))
        }
        Action::Enqueue { playlist, media } => {
            let r = schedule::schedule_service_client::ScheduleServiceClient::new(channel)
                .enqueue(schedule::EnqueueRequest { playlist_ref: playlist.clone(), media_path: media })
                .await
                .map_err(err)?
                .into_inner();
            if r.accepted {
                Ok(Done::msg(tr!("done-enqueued", playlist = playlist, len = r.len)))
            } else {
                Err(tr!("done-enqueue-full", playlist = playlist, len = r.len))
            }
        }
        Action::Scan => {
            let r = library::library_service_client::LibraryServiceClient::new(channel)
                .scan(library::ScanRequest {})
                .await
                .map_err(err)?
                .into_inner();
            Ok(Done {
                message: tr!("done-scan", found = r.found, skipped = r.skipped, vanished = r.vanished, unavailable = r.unavailable),
                scan: Some(r),
                exits: false,
            })
        }
        Action::Plugin { name, verb } => {
            use plugin::plugin_control_request::Action as A;
            let a = match verb {
                PluginVerb::Start => A::Start,
                PluginVerb::Stop => A::Stop,
                PluginVerb::Restart => A::Restart,
                PluginVerb::Reload => A::Reload,
            };
            let r = plugin::plugin_service_client::PluginServiceClient::new(channel)
                .control(plugin::PluginControlRequest { name: name.clone(), action: a as i32 })
                .await
                .map_err(err)?
                .into_inner();
            let p = r.plugin.unwrap_or_default();
            Ok(Done::msg(if p.reason.is_empty() {
                tr!("done-plugin", name = name, state = p.state)
            } else {
                tr!("done-plugin-reason", name = name, state = p.state, reason = p.reason)
            }))
        }
        Action::Shutdown { force } => {
            let r = station::station_client::StationClient::new(channel)
                .shutdown(station::ShutdownRequest { force })
                .await
                .map_err(err)?
                .into_inner();
            Ok(Done {
                exits: true,
                ..Done::msg(if r.parked { tr!("done-shutdown") } else { tr!("done-shutdown-fallback") })
            })
        }
    }
}
