//! Accès gRPC. Toutes les lectures sont bornées dans le temps et rendent
//! `Result<_, String>` : une erreur est une donnée à afficher, jamais avalée.
//!
//! Lot 0 : le bandeau est alimenté par interrogation périodique, en attendant
//! le flux `OnAirService.Watch` (dossier §3.2, lot 1).

use std::future::Future;
use std::time::{Duration, Instant};

use stationd_proto::{broadcast, library, liquidsoap, live, onair, playlist, plugin, stats, station};
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
}

pub async fn read_banner(channel: Channel) -> BannerRead {
    let mut st = station::station_client::StationClient::new(channel.clone());
    let mut bc = broadcast::broadcast_service_client::BroadcastServiceClient::new(channel.clone());
    let mut bc2 = bc.clone();
    let mut ls = liquidsoap::liquidsoap_service_client::LiquidsoapServiceClient::new(channel.clone());
    let mut lv = live::live_service_client::LiveServiceClient::new(channel.clone());
    let mut pl = plugin::plugin_service_client::PluginServiceClient::new(channel);

    let (status, state, ls_status, live_status, overrides, plugins) = tokio::join!(
        bounded(st.status(station::StatusRequest {})),
        bounded(bc.get_state(broadcast::GetStateRequest {})),
        bounded(ls.get_status(liquidsoap::GetStatusRequest {})),
        bounded(lv.get_status(live::GetStatusRequest {})),
        bounded(bc2.list_overrides(broadcast::ListOverridesRequest {})),
        bounded(pl.list(plugin::PluginListRequest {})),
    );

    BannerRead {
        at: Instant::now(),
        status,
        broadcast: state,
        liquidsoap: ls_status,
        live: live_status,
        overrides: overrides.map(|r| r.overrides),
        plugins: plugins.map(|r| r.plugins),
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
}

pub async fn media_card(channel: Channel, path: String) -> MediaCard {
    let mut pl = playlist::playlist_service_client::PlaylistServiceClient::new(channel.clone());
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
    let (playlists, a, b, c, d) = tokio::join!(
        bounded(pl.containing(playlist::ContainingRequest { media_path: path.clone() })),
        plays(CARD_WINDOWS[0]),
        plays(CARD_WINDOWS[1]),
        plays(CARD_WINDOWS[2]),
        plays(CARD_WINDOWS[3]),
    );
    MediaCard { playlists: playlists.map(|r| r.playlists), plays: vec![a, b, c, d] }
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
