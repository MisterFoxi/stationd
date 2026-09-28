//! Accès gRPC. Toutes les lectures sont bornées dans le temps et rendent
//! `Result<_, String>` : une erreur est une donnée à afficher, jamais avalée.
//!
//! Lot 0 : le bandeau est alimenté par interrogation périodique, en attendant
//! le flux `OnAirService.Watch` (dossier §3.2, lot 1).

use std::future::Future;
use std::time::{Duration, Instant};

use stationd_proto::{broadcast, library, liquidsoap, live, onair, plugin, station};
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
