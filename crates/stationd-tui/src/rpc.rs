//! Accès gRPC. Toutes les lectures sont bornées dans le temps et rendent
//! `Result<_, String>` : une erreur est une donnée à afficher, jamais avalée.
//!
//! Lot 0 : le bandeau est alimenté par interrogation périodique, en attendant
//! le flux `OnAirService.Watch` (dossier §3.2, lot 1).

use std::future::Future;
use std::time::{Duration, Instant};

use stationd_proto::{broadcast, events, icecast, library, liquidsoap, live, onair, playlist, plugin, schedule, stats, station};
use tonic::transport::{Channel, Endpoint};

/// Délai maximal d'une lecture simple (dossier §18 de la v1, conservé).
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Période d'interrogation quand stationd répond.
pub const POLL_OK: Duration = Duration::from_secs(2);
/// Plafond du délai croissant quand stationd ne répond pas.
pub const POLL_MAX: Duration = Duration::from_secs(10);

pub type Read<T> = Result<T, String>;

/// Canal paresseux : la connexion s'établit au premier appel et se rétablit
/// seule après une coupure (tonic). Aucun appel n'est fait ici.
pub fn lazy_channel(addr: &str) -> anyhow::Result<Channel> {
    let endpoint = Endpoint::from_shared(addr.to_string())
        .map_err(|e| anyhow::anyhow!(crate::tr!("rpc-bad-address", addr = addr.to_string(), reason = e.to_string())))?
        .connect_timeout(Duration::from_secs(3))
        .timeout(READ_TIMEOUT);
    Ok(endpoint.connect_lazy())
}

/// Canal pour les opérations longues (scan de la bibliothèque) : même
/// adresse, sans délai maximal par appel.
pub fn lazy_channel_long(addr: &str) -> anyhow::Result<Channel> {
    let endpoint = Endpoint::from_shared(addr.to_string())
        .map_err(|e| anyhow::anyhow!(crate::tr!("rpc-bad-address", addr = addr.to_string(), reason = e.to_string())))?
        .connect_timeout(Duration::from_secs(3));
    Ok(endpoint.connect_lazy())
}

/// Borne un appel et aplatit l'erreur en texte lisible (code + message).
async fn bounded<T>(
    call: impl Future<Output = Result<tonic::Response<T>, tonic::Status>>,
) -> Read<T> {
    match tokio::time::timeout(READ_TIMEOUT, call).await {
        Ok(Ok(reply)) => Ok(reply.into_inner()),
        Ok(Err(status)) => Err(status_text(&status)),
        Err(_) => Err(crate::tr!("rpc-timeout", s = READ_TIMEOUT.as_secs())),
    }
}

/// Texte d'erreur gRPC. Une erreur de transport (stationd injoignable) est
/// dite comme telle plutôt que par le code brut `Unavailable`.
pub fn status_text(status: &tonic::Status) -> String {
    match status.code() {
        tonic::Code::Unavailable => crate::tr!("rpc-unreachable", reason = status.message().to_string()),
        code => crate::tr!("rpc-status", code = format!("{code:?}"), reason = status.message().to_string()),
    }
}

/// Une lecture complète des sources du bandeau, faites en parallèle.
/// Chaque source a son propre résultat : l'échec de l'une n'efface pas les
/// autres (Icecast en panne ≠ stationd en panne).
#[derive(Debug, Clone)]
pub struct BannerRead {
    pub at: Instant,
    pub status: Read<station::StatusReply>,
    pub broadcast: Read<broadcast::BroadcastStatus>,
    pub liquidsoap: Read<liquidsoap::LiquidsoapStatus>,
    pub live: Read<live::LiveStatus>,
    pub overrides: Read<Vec<broadcast::Override>>,
    pub plugins: Read<Vec<plugin::PluginInfo>>,
    pub icecast: Read<icecast::IcecastStatus>,
}

pub async fn read_banner(channel: Channel) -> BannerRead {
    let mut st = station::station_client::StationClient::new(channel.clone());
    let mut bc = broadcast::broadcast_service_client::BroadcastServiceClient::new(channel.clone());
    let mut bc2 = bc.clone();
    let mut ls = liquidsoap::liquidsoap_service_client::LiquidsoapServiceClient::new(channel.clone());
    let mut lv = live::live_service_client::LiveServiceClient::new(channel.clone());
    let mut pl = plugin::plugin_service_client::PluginServiceClient::new(channel.clone());
    let mut ic = icecast::icecast_service_client::IcecastServiceClient::new(channel);

    let (status, state, ls_status, live_status, overrides, plugins, ic_status) = tokio::join!(
        bounded(st.status(station::StatusRequest {})),
        bounded(bc.get_state(broadcast::GetStateRequest {})),
        bounded(ls.get_status(liquidsoap::GetStatusRequest {})),
        bounded(lv.get_status(live::GetStatusRequest {})),
        bounded(bc2.list_overrides(broadcast::ListOverridesRequest {})),
        bounded(pl.list(plugin::PluginListRequest {})),
        bounded(ic.get_status(icecast::GetStatusRequest {})),
    );

    BannerRead {
        at: Instant::now(),
        status,
        broadcast: state,
        liquidsoap: ls_status,
        live: live_status,
        overrides: overrides.map(|r| r.overrides),
        plugins: plugins.map(|r| r.plugins),
        icecast: ic_status,
    }
}

/// Une page de `LibraryService.SearchMedia`.
pub async fn search_media(channel: Channel, req: library::SearchMediaRequest) -> Read<library::SearchMediaResponse> {
    let mut cli = library::library_service_client::LibraryServiceClient::new(channel);
    bounded(cli.search_media(req)).await
}

/// `PlaylistService.List` : la vue des playlists, avec qui les référence.
pub async fn list_playlists(channel: Channel) -> Read<Vec<playlist::PlaylistSummary>> {
    let mut cli = playlist::playlist_service_client::PlaylistServiceClient::new(channel);
    bounded(cli.list(playlist::ListRequest {})).await.map(|r| r.playlists)
}

/// `PlaylistService.Export` : TOML appliqué + fichier et sa révision.
pub async fn export_playlist(channel: Channel, reference: String) -> Read<playlist::ExportResponse> {
    let mut cli = playlist::playlist_service_client::PlaylistServiceClient::new(channel);
    bounded(cli.export(playlist::ExportRequest { reference })).await
}

/// `PlaylistService.PreviewPool` : pool d'un brouillon + ses diagnostics.
pub async fn preview_pool(
    channel: Channel,
    toml: String,
    reference: String,
    sample: u32,
) -> Read<playlist::PreviewPoolResponse> {
    let mut cli = playlist::playlist_service_client::PlaylistServiceClient::new(channel);
    bounded(cli.preview_pool(playlist::PreviewPoolRequest { toml, reference, sample })).await
}

/// `PlaylistService.Save` : écrit et applique, ou dit pourquoi non.
pub async fn save_playlist(
    channel: Channel,
    reference: String,
    toml: String,
    expected_revision: String,
) -> Read<playlist::SaveResponse> {
    let mut cli = playlist::playlist_service_client::PlaylistServiceClient::new(channel);
    bounded(cli.save(playlist::SaveRequest { reference, toml, expected_revision })).await
}

/// `LibraryService.ListGenres` (médias disponibles) : valeurs proposées
/// dans les filtres de genre.
pub async fn list_genres(channel: Channel) -> Read<library::ListGenresResponse> {
    let mut cli = library::library_service_client::LibraryServiceClient::new(channel);
    bounded(cli.list_genres(library::ListGenresRequest { only_available: true })).await
}

/// Ce qu'il faut pour ouvrir l'éditeur de tags : les tags du fichier et
/// les genres connus (toute la bibliothèque, pour les proposer).
pub async fn tag_form_data(
    channel: Channel,
    rel_path: String,
) -> Read<(library::MediaTags, Vec<library::GenreCount>)> {
    let mut a = library::library_service_client::LibraryServiceClient::new(channel);
    let mut b = a.clone();
    let (t, g) = tokio::join!(
        bounded(a.get_tags(library::GetTagsRequest { rel_path })),
        bounded(b.list_genres(library::ListGenresRequest { only_available: false })),
    );
    Ok((t?, g?.genres))
}

/// Fenêtres des statistiques d'une fiche média (la dernière vaut « depuis
/// toujours » : dix ans d'historique).
pub const CARD_WINDOWS: [&str; 4] = ["24h", "7d", "30d", "3650d"];

/// Ce que la fiche d'un média ajoute à sa ligne : les playlists qui peuvent
/// le diffuser et ses diffusions par fenêtre.
#[derive(Debug, Clone)]
pub struct MediaCard {
    pub playlists: Read<Vec<playlist::PlaylistSummary>>,
    /// Une entrée par `CARD_WINDOWS` ; `Ok(None)` = jamais choisi dans la fenêtre.
    pub plays: Vec<Read<Option<stats::PlaysRow>>>,
    /// Les tags lus dans le fichier (BPM, tempo, date de création, sources) ;
    /// une erreur pour un format que stationd ne lit pas.
    pub tags: Read<library::MediaTags>,
}

pub async fn media_card(channel: Channel, path: String) -> MediaCard {
    let mut pl = playlist::playlist_service_client::PlaylistServiceClient::new(channel.clone());
    let mut lib = library::library_service_client::LibraryServiceClient::new(channel.clone());
    let st = stats::stats_service_client::StatsServiceClient::new(channel);
    let plays = |since: &'static str| {
        let mut st = st.clone();
        let key = path.clone();
        async move {
            let req = stats::PlaysRequest {
                since: since.to_string(),
                by: stats::plays_request::By::Media as i32,
                limit: 0,
                key,
            };
            bounded(st.plays(req)).await.map(|r| r.rows.into_iter().next())
        }
    };
    let (playlists, tags, a, b, c, d) = tokio::join!(
        bounded(pl.containing(playlist::ContainingRequest { media_path: path.clone() })),
        bounded(lib.get_tags(library::GetTagsRequest { rel_path: path.clone() })),
        plays(CARD_WINDOWS[0]),
        plays(CARD_WINDOWS[1]),
        plays(CARD_WINDOWS[2]),
        plays(CARD_WINDOWS[3]),
    );
    MediaCard { playlists: playlists.map(|r| r.playlists), plays: vec![a, b, c, d], tags }
}

/// Ce que lit l'agenda : la projection de la grille sur une fenêtre, les
/// règles (leur description) et la couverture (les verdicts). Chaque source
/// a son propre résultat.
#[derive(Debug, Clone)]
pub struct AgendaRead {
    pub preview: Read<schedule::PreviewResponse>,
    pub rules: Read<Vec<schedule::Rule>>,
    pub coverage: Read<schedule::CheckCoverageResponse>,
    /// Les grilles du nœud (laquelle est active).
    pub grids: Read<schedule::ListGridsResponse>,
}

/// `Preview` de `[from, from + window)` (secondes), `ListRules`,
/// `CheckCoverage` (toute la grille), en parallèle — de la grille `grid`
/// (un fichier du nœud ; vide = la grille appliquée).
pub async fn agenda_read(channel: Channel, from: i64, window: i64, grid: String) -> AgendaRead {
    use stationd_proto::prost_types::{Duration as PDuration, Timestamp};
    let mut a = schedule::schedule_service_client::ScheduleServiceClient::new(channel);
    let mut b = a.clone();
    let mut c = a.clone();
    let mut d = a.clone();
    let req = schedule::PreviewRequest {
        from: Some(Timestamp { seconds: from, nanos: 0 }),
        window: Some(PDuration { seconds: window, nanos: 0 }),
        grid: grid.clone(),
        draft_toml: String::new(),
    };
    let (preview, rules, coverage, grids) = tokio::join!(
        bounded(a.preview(req)),
        bounded(b.list_rules(schedule::ListRulesRequest { grid: grid.clone(), draft_toml: String::new() })),
        bounded(c.check_coverage(schedule::CheckCoverageRequest { rule_ids: Vec::new(), grid, draft_toml: String::new() })),
        bounded(d.list_grids(schedule::ListGridsRequest {})),
    );
    AgendaRead { preview, rules: rules.map(|r| r.rules), coverage, grids }
}

fn sched(channel: Channel) -> schedule::schedule_service_client::ScheduleServiceClient<Channel> {
    schedule::schedule_service_client::ScheduleServiceClient::new(channel)
}

/// Le texte d'une grille à modifier : le fichier (commentaires compris) et
/// sa révision ; sans fichier, la grille appliquée (création à l'écriture).
pub async fn grid_text(channel: Channel, name: String) -> Read<schedule::GetGridResponse> {
    let mut g = bounded(sched(channel.clone()).get_grid(schedule::GetGridRequest { name })).await?;
    if !g.exists && g.active {
        let x = bounded(sched(channel).export_grid(schedule::ExportGridRequest { rule_ids: Vec::new() })).await?;
        g.toml = x.files.into_iter().map(|f| f.toml).collect::<Vec<_>>().join("\n");
    }
    Ok(g)
}

/// Ce que l'éditeur de règle demande après une frappe : les diagnostics du
/// brouillon et la projection de la journée avec la modification.
#[derive(Debug, Clone)]
pub struct DraftCheck {
    pub validate: Read<schedule::ValidateGridResponse>,
    pub preview: Read<schedule::PreviewResponse>,
}

pub async fn check_draft(channel: Channel, name: String, toml: String, from: i64, window: i64) -> DraftCheck {
    use stationd_proto::prost_types::{Duration as PDuration, Timestamp};
    let mut a = sched(channel);
    let mut b = a.clone();
    let files = vec![schedule::GridFile { path: name, toml: toml.clone() }];
    let (validate, preview) = tokio::join!(
        bounded(a.validate_grid(schedule::ApplyGridRequest { files })),
        bounded(b.preview(schedule::PreviewRequest {
            from: Some(Timestamp { seconds: from, nanos: 0 }),
            window: Some(PDuration { seconds: window, nanos: 0 }),
            grid: String::new(),
            draft_toml: toml,
        })),
    );
    DraftCheck { validate, preview }
}

/// `SaveGrid` : écrit (et applique si active), ou dit pourquoi non.
pub async fn save_grid(channel: Channel, name: String, toml: String, expected_revision: String) -> Read<schedule::SaveGridResponse> {
    bounded(sched(channel).save_grid(schedule::SaveGridRequest { name, toml, expected_revision })).await
}

/// `ActivateGrid`.
pub async fn activate_grid(channel: Channel, name: String) -> Read<schedule::ActivateGridResponse> {
    bounded(sched(channel).activate_grid(schedule::ActivateGridRequest { name })).await
}

/// Ce que la TUI demande au flux de l'antenne : le maximum servi, chaque
/// écran coupe à sa guise (pas de réabonnement pour changer d'affichage).
pub const ONAIR_REQUEST: onair::WatchRequest =
    onair::WatchRequest { upcoming: 30, history: 50, playlists_ahead: 10 };

/// Ouvre le flux `OnAirService.Watch`. Rend le flux, ou le texte d'erreur.
pub async fn watch_onair(
    channel: Channel,
) -> Result<tonic::Streaming<onair::OnAirSnapshot>, String> {
    let mut cli = onair::on_air_service_client::OnAirServiceClient::new(channel);
    match tokio::time::timeout(READ_TIMEOUT, cli.watch(ONAIR_REQUEST)).await {
        Ok(Ok(r)) => Ok(r.into_inner()),
        Ok(Err(status)) if status.code() == tonic::Code::Unimplemented => {
            Err(crate::tr!("rpc-no-onair"))
        }
        Ok(Err(status)) => Err(status_text(&status)),
        Err(_) => Err(crate::tr!("rpc-timeout", s = READ_TIMEOUT.as_secs())),
    }
}

/// Délai avant la prochaine lecture : régulier si stationd répond, croissant
/// (doublé, plafonné) tant qu'il ne répond pas.
pub fn next_delay(previous: Duration, stationd_ok: bool) -> Duration {
    if stationd_ok {
        POLL_OK
    } else {
        (previous * 2).clamp(POLL_OK, POLL_MAX)
    }
}

/// Ouvre un flux serveur : délai borné pour l'ouverture seulement.
async fn open_stream<T>(
    call: impl Future<Output = Result<tonic::Response<tonic::Streaming<T>>, tonic::Status>>,
) -> Result<tonic::Streaming<T>, String> {
    match tokio::time::timeout(READ_TIMEOUT, call).await {
        Ok(Ok(r)) => Ok(r.into_inner()),
        Ok(Err(status)) if status.code() == tonic::Code::Unimplemented => Err(crate::tr!("rpc-too-old")),
        Ok(Err(status)) => Err(status_text(&status)),
        Err(_) => Err(crate::tr!("rpc-timeout", s = READ_TIMEOUT.as_secs())),
    }
}

/// Le journal de la station : les derniers événements, puis au fil de l'eau.
pub async fn watch_events(channel: Channel) -> Result<tonic::Streaming<events::Event>, String> {
    let mut cli = events::event_service_client::EventServiceClient::new(channel);
    open_stream(cli.watch(events::WatchEventsRequest { backlog: 2000, follow: true })).await
}

/// L'avancement du scan de la bibliothèque.
pub async fn watch_scan(channel: Channel) -> Result<tonic::Streaming<library::ScanStatus>, String> {
    let mut cli = library::library_service_client::LibraryServiceClient::new(channel);
    open_stream(cli.watch_scan(library::WatchScanRequest {})).await
}

/// Les valeurs par origine (genre du fichier, sources `custom-tags`).
pub async fn tag_values(channel: Channel) -> Read<library::ListTagValuesResponse> {
    let mut cli = library::library_service_client::LibraryServiceClient::new(channel);
    bounded(cli.list_tag_values(library::ListTagValuesRequest {})).await
}

/// Renommer une valeur : aperçu (`dry_run`) ou exécution, en flux.
pub async fn rename_value(
    channel: Channel,
    req: library::RenameTagValueRequest,
) -> Result<tonic::Streaming<library::RenameTagValueEvent>, String> {
    let mut cli = library::library_service_client::LibraryServiceClient::new(channel);
    open_stream(cli.rename_tag_value(req)).await
}

/// `StatsService.Plays`.
pub async fn plays(channel: Channel, req: stats::PlaysRequest) -> Read<stats::PlaysResponse> {
    let mut cli = stats::stats_service_client::StatsServiceClient::new(channel);
    bounded(cli.plays(req)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_is_regular_when_ok() {
        assert_eq!(next_delay(POLL_MAX, true), POLL_OK);
    }

    #[test]
    fn delay_grows_then_caps_when_down() {
        let d1 = next_delay(POLL_OK, false);
        assert_eq!(d1, POLL_OK * 2);
        let mut d = d1;
        for _ in 0..10 {
            d = next_delay(d, false);
        }
        assert_eq!(d, POLL_MAX);
    }
}
