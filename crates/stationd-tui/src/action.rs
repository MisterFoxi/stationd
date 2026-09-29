//! Actions sur la station : chacune est UN appel gRPC, exactement celui de
//! `stationctl`. La TUI ne décide rien : elle envoie, puis affiche ce que
//! stationd répond (le vrai effet se lit ensuite dans le bandeau et l'antenne).
//!
//! Une action mutante n'est jamais rejouée automatiquement (dossier §4.2).

use stationd_proto::{broadcast, library, live, playlist, plugin, schedule, station};
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
    /// Plusieurs médias mis en file, dans l'ordre ; s'arrête au premier refus.
    EnqueueMany { playlist: String, media: Vec<String> },
    Scan,
    /// Supprime une playlist (fichier puis vue), si son fichier est encore à
    /// `revision` (vide = sans contrôle).
    RemovePlaylist { reference: String, revision: String },
    /// Relit toute la racine des playlists (`PlaylistService.Reload`).
    ReloadPlaylists,
    /// Écrit des tags dans des fichiers (`LibraryService.SetTags`), un par
    /// un. Révision vide = relue juste avant d'écrire (lot).
    SetTags { targets: Vec<(String, String)>, edit: Box<TagChanges> },
    Plugin { name: String, verb: PluginVerb },
    Shutdown { force: bool },
}

/// Une liste de valeurs à écrire (genres, `Type`…).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListEdit {
    /// La liste devient exactement celle-ci (vide = retirée).
    Replace(Vec<String>),
    /// Lot : ces valeurs ajoutées / retirées, le reste de chaque fichier gardé.
    Merge { add: Vec<String>, remove: Vec<String> },
}

impl ListEdit {
    /// La liste d'un fichier après cette modification (casse ignorée pour
    /// les doublons et le retrait, graphie existante gardée).
    pub fn apply(&self, current: &[String]) -> Vec<String> {
        let same = |a: &str, b: &str| a.trim().to_lowercase() == b.trim().to_lowercase();
        let mut out: Vec<String> = Vec::new();
        let push = |out: &mut Vec<String>, v: &str| {
            if !v.trim().is_empty() && !out.iter().any(|o| same(o, v)) {
                out.push(v.trim().to_string());
            }
        };
        match self {
            ListEdit::Replace(v) => v.iter().for_each(|x| push(&mut out, x)),
            ListEdit::Merge { add, remove } => {
                for x in current.iter().chain(add.iter()) {
                    if !remove.iter().any(|r| same(r, x)) {
                        push(&mut out, x);
                    }
                }
            }
        }
        out
    }
}

/// Tags à écrire : `None` = inchangé, `Some("")` / `Some(0)` = retiré.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagChanges {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<u32>,
    /// Genres du fichier (`TCON`).
    pub genres: Option<ListEdit>,
    /// Tags qui deviennent des genres (`Type`…), par nom.
    pub sources: Vec<(String, ListEdit)>,
    pub bpm: Option<u32>,
    /// Tempo choisi à la main (`""` = revenir au tempo tiré du BPM).
    pub tempo: Option<String>,
    /// Date de création saisie (RFC 3339, `""` = revenir à la date dérivée).
    pub creation: Option<String>,
}

impl TagChanges {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Une fusion (lot) demande de relire chaque fichier avant d'écrire.
    fn merges(&self) -> bool {
        matches!(self.genres, Some(ListEdit::Merge { .. })) || self.sources.iter().any(|(_, e)| matches!(e, ListEdit::Merge { .. }))
    }

    /// La requête pour un fichier dont les tags actuels sont `current`.
    pub fn request(&self, path: &str, revision: String, current: Option<&library::MediaTags>) -> library::SetTagsRequest {
        let empty = Vec::new();
        let genres_now = current.map(|c| &c.genres).unwrap_or(&empty);
        library::SetTagsRequest {
            rel_path: path.to_string(),
            revision,
            title: self.title.clone(),
            artist: self.artist.clone(),
            album: self.album.clone(),
            year: self.year,
            genres: self.genres.as_ref().map(|g| library::StringList { values: g.apply(genres_now) }),
            sources: self
                .sources
                .iter()
                .map(|(name, e)| {
                    let now: Vec<String> = current
                        .and_then(|c| c.sources.iter().find(|s| s.name.eq_ignore_ascii_case(name)))
                        .map(|s| s.values.clone())
                        .unwrap_or_default();
                    library::TagValues { name: name.clone(), values: e.apply(&now) }
                })
                .collect(),
            bpm: self.bpm,
            tempo_manual: self.tempo.clone(),
            creation_manual: self.creation.clone(),
        }
    }
}

/// Les champs de `req` que les tags relus `got` ne reflètent pas (libellés
/// traduits). Même normalisation que stationd : valeurs rognées, vides
/// ignorées, doublons à la casse près retirés, comparaison sans la casse.
pub fn not_applied(req: &library::SetTagsRequest, got: &library::MediaTags) -> Vec<String> {
    fn norm(values: &[String]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for v in values.iter().map(|v| v.trim().to_lowercase()).filter(|v| !v.is_empty()) {
            if !out.contains(&v) {
                out.push(v);
            }
        }
        out
    }
    let text = |want: &Option<String>, have: &str| want.as_ref().is_some_and(|w| w.trim() != have.trim());
    let mut out = Vec::new();
    if text(&req.title, &got.title) {
        out.push(tr!("media-field-title"));
    }
    if text(&req.artist, &got.artist) {
        out.push(tr!("media-field-artist"));
    }
    if text(&req.album, &got.album) {
        out.push(tr!("media-field-album"));
    }
    if req.year.is_some_and(|y| y != got.year) {
        out.push(tr!("media-field-year"));
    }
    if req.genres.as_ref().is_some_and(|g| norm(&g.values) != norm(&got.genres)) {
        out.push(tr!("tags-genres"));
    }
    for src in &req.sources {
        let have = got.sources.iter().find(|s| s.name.eq_ignore_ascii_case(&src.name)).map(|s| s.values.as_slice()).unwrap_or(&[]);
        if norm(&src.values) != norm(have) {
            out.push(src.name.clone());
        }
    }
    if req.bpm.is_some_and(|b| b != got.bpm) {
        out.push(tr!("tags-bpm"));
    }
    if text(&req.tempo_manual, &got.tempo_manual) {
        out.push(tr!("tags-tempo"));
    }
    if text(&req.creation_manual, &got.creation_manual) {
        out.push(tr!("tags-creation"));
    }
    out
}

impl Action {
    /// Une opération qui peut durer (scan, écriture de fichiers sur NFS) :
    /// canal sans délai maximal.
    pub fn is_long(&self) -> bool {
        matches!(self, Action::Scan | Action::SetTags { .. })
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
        Action::EnqueueMany { playlist, media } => {
            let mut cli = schedule::schedule_service_client::ScheduleServiceClient::new(channel);
            let total = media.len();
            let mut len = 0;
            for (done, m) in media.into_iter().enumerate() {
                let r = cli
                    .enqueue(schedule::EnqueueRequest { playlist_ref: playlist.clone(), media_path: m })
                    .await
                    .map_err(|e| tr!("done-enqueue-partial", n = done, total = total, reason = err(e)))?
                    .into_inner();
                if !r.accepted {
                    return Err(tr!("done-enqueue-many-full", playlist = playlist, n = done, total = total, len = r.len));
                }
                len = r.len;
            }
            Ok(Done::msg(tr!("done-enqueued-many", playlist = playlist, n = total, len = len)))
        }
        Action::RemovePlaylist { reference, revision } => {
            let r = playlist::playlist_service_client::PlaylistServiceClient::new(channel)
                .remove(playlist::RemoveRequest { reference: reference.clone(), expected_revision: revision })
                .await
                .map_err(err)?
                .into_inner();
            Ok(Done::msg(if r.file.is_empty() {
                tr!("done-playlist-removed", playlist = reference)
            } else {
                tr!("done-playlist-removed-file", playlist = reference, file = r.file)
            }))
        }
        Action::ReloadPlaylists => {
            let r = playlist::playlist_service_client::PlaylistServiceClient::new(channel)
                .reload(playlist::ReloadRequest {})
                .await
                .map_err(err)?
                .into_inner();
            let msg = tr!("done-playlists-reloaded", added = r.added, removed = r.removed.len(), errors = r.errors.len());
            if r.errors.is_empty() {
                Ok(Done::msg(msg))
            } else {
                let first = r.errors.first().map(|e| format!("{} : {}", e.path, e.message)).unwrap_or_default();
                Err(format!("{msg} — {first}"))
            }
        }
        Action::SetTags { targets, edit } => {
            let mut cli = library::library_service_client::LibraryServiceClient::new(channel);
            let total = targets.len();
            let (mut written, mut conflicts, mut failed) = (0, Vec::new(), Vec::new());
            for (path, revision) in targets {
                // Un lot relit chaque fichier : sa révision, et ses listes
                // pour y ajouter / en retirer des valeurs.
                let (revision, current) = if revision.is_empty() || edit.merges() {
                    match cli.get_tags(library::GetTagsRequest { rel_path: path.clone() }).await {
                        Ok(r) => {
                            let t = r.into_inner();
                            (if revision.is_empty() { t.revision.clone() } else { revision }, Some(t))
                        }
                        Err(e) => {
                            failed.push(format!("{path} : {}", err(e)));
                            continue;
                        }
                    }
                } else {
                    (revision, None)
                };
                let req = edit.request(&path, revision, current.as_ref());
                match cli.set_tags(req.clone()).await {
                    Ok(r) if r.get_ref().conflict => conflicts.push(path),
                    // Relu dans le fichier après écriture : ce qui a été
                    // demandé doit y être. Un stationd plus ancien que la TUI
                    // ignore les champs qu'il ne connaît pas et répond OK.
                    Ok(r) => match &r.get_ref().tags {
                        Some(t) => {
                            let missing = not_applied(&req, t);
                            if missing.is_empty() {
                                written += 1;
                            } else {
                                failed.push(tr!("done-tags-not-applied", path = path, fields = missing.join(", ")));
                            }
                        }
                        None => failed.push(tr!("done-tags-no-readback", path = path)),
                    },
                    Err(e) => failed.push(format!("{path} : {}", err(e))),
                }
            }
            if conflicts.is_empty() && failed.is_empty() {
                return Ok(Done::msg(tr!("done-tags-written", n = written, total = total)));
            }
            let mut msg = tr!("done-tags-written", n = written, total = total);
            if !conflicts.is_empty() {
                msg.push_str(&format!(" · {}", tr!("done-tags-conflicts", list = conflicts.join(", "))));
            }
            if let Some(f) = failed.first() {
                msg.push_str(&format!(" · {}", tr!("done-tags-failed", n = failed.len(), first = f.clone())));
            }
            Err(msg)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_stationd_did_not_write_is_reported() {
        let req = library::SetTagsRequest {
            rel_path: "a.mp3".into(),
            title: Some(" Veridis Quo ".into()),
            year: Some(0),
            genres: Some(library::StringList { values: vec!["House".into(), "house".into(), "électro".into()] }),
            sources: vec![library::TagValues { name: "Type".into(), values: vec!["song".into()] }],
            bpm: Some(120),
            tempo_manual: Some("fast".into()),
            ..Default::default()
        };
        let mut got = library::MediaTags {
            title: "Veridis Quo".into(),
            genres: vec!["house".into(), "Électro".into()],
            sources: vec![library::TagValues { name: "type".into(), values: vec!["Song".into()] }],
            bpm: 120,
            tempo_manual: "fast".into(),
            ..Default::default()
        };
        assert!(not_applied(&req, &got).is_empty(), "{:?}", not_applied(&req, &got));
        // Un stationd qui ignore les nouveaux champs : seul le titre passe.
        got.genres.clear();
        got.sources.clear();
        got.bpm = 0;
        got.tempo_manual.clear();
        assert_eq!(not_applied(&req, &got), vec![tr!("tags-genres"), "Type".to_string(), tr!("tags-bpm"), tr!("tags-tempo")]);
    }

    #[test]
    fn a_list_edit_replaces_or_merges_ignoring_case() {
        let now = vec!["Électro".to_string(), "house".to_string()];
        assert_eq!(ListEdit::Replace(vec!["a".into(), "A".into(), " ".into()]).apply(&now), ["a"]);
        let m = ListEdit::Merge { add: vec!["électro".into(), "talks".into()], remove: vec!["HOUSE".into()] };
        assert_eq!(m.apply(&now), ["Électro", "talks"], "graphie existante gardée, retrait sans casse");
    }

    #[test]
    fn a_batch_request_merges_into_each_file() {
        let c = TagChanges {
            sources: vec![("Type".into(), ListEdit::Merge { add: vec!["news".into()], remove: vec![] })],
            ..Default::default()
        };
        let cur = library::MediaTags {
            sources: vec![library::TagValues { name: "type".into(), values: vec!["talks".into()] }],
            genres: vec!["jazz".into()],
            ..Default::default()
        };
        let r = c.request("a.mp3", "tags:1".into(), Some(&cur));
        assert_eq!(r.sources[0].values, ["talks", "news"]);
        assert!(r.genres.is_none(), "genres non touchés");
        assert!(r.title.is_none());
    }
}
