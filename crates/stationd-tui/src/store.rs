//! Dernières données reçues de stationd, horodatées par source.
//!
//! Règles (dossier §2) : une valeur jamais reçue est « inconnue » (`—`), une
//! valeur ancienne reste affichée avec son âge, une erreur n'efface pas la
//! dernière valeur connue. Le rendu ne fait que lire ce magasin.

use std::time::{Duration, Instant};

use jiff::tz::TimeZone;
use stationd_proto::{broadcast, liquidsoap, live, onair, plugin, station};

use crate::rpc::BannerRead;

/// Au-delà, une mesure est présentée comme ancienne (`~`).
pub const STALE_AFTER: Duration = Duration::from_secs(15);

/// Une source de données : dernière valeur reçue, quand, et dernière erreur.
#[derive(Debug)]
pub struct Sourced<T> {
    pub value: Option<T>,
    pub ok_at: Option<Instant>,
    pub error: Option<String>,
}

impl<T> Default for Sourced<T> {
    fn default() -> Self {
        Self { value: None, ok_at: None, error: None }
    }
}

impl<T> Sourced<T> {
    /// Un succès remplace la valeur et efface l'erreur ; un échec garde la
    /// dernière valeur connue et note l'erreur.
    pub fn apply(&mut self, read: Result<T, String>, at: Instant) {
        match read {
            Ok(v) => {
                self.value = Some(v);
                self.ok_at = Some(at);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Âge de la dernière valeur reçue.
    pub fn age(&self, now: Instant) -> Option<Duration> {
        self.ok_at.map(|t| now.saturating_duration_since(t))
    }

    pub fn is_stale(&self, now: Instant) -> bool {
        self.error.is_some() || self.age(now).is_none_or(|a| a > STALE_AFTER)
    }
}

/// État de la liaison avec stationd, déduit de `Station.Status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// Aucune réponse encore (premier essai en cours).
    Connecting,
    Connected,
    /// Plus de réponse. `since` = premier échec de la série.
    Lost { since: Instant, error: String },
}

#[derive(Debug)]
pub struct Store {
    pub addr: String,
    pub link: Link,
    pub status: Sourced<station::StatusReply>,
    pub broadcast: Sourced<broadcast::BroadcastStatus>,
    pub liquidsoap: Sourced<liquidsoap::LiquidsoapStatus>,
    pub live: Sourced<live::LiveStatus>,
    pub overrides: Sourced<Vec<broadcast::Override>>,
    pub plugins: Sourced<Vec<plugin::PluginInfo>>,
    /// Fuseau de la station, résolu depuis `StatusReply.timezone`.
    pub tz: Option<TimeZone>,
    /// Nom du fuseau tel que reçu (affiché même s'il n'a pas pu être résolu).
    pub tz_name: Option<String>,
    /// Dernier instantané de l'antenne (flux `OnAirService.Watch`).
    pub onair: Option<onair::OnAirSnapshot>,
    /// Le flux de l'antenne est-il ouvert ? `Err` = pourquoi il ne l'est pas.
    pub onair_link: Result<(), String>,
}

impl Store {
    pub fn new(addr: &str) -> Self {
        Self {
            addr: addr.to_string(),
            link: Link::Connecting,
            status: Sourced::default(),
            broadcast: Sourced::default(),
            liquidsoap: Sourced::default(),
            live: Sourced::default(),
            overrides: Sourced::default(),
            plugins: Sourced::default(),
            tz: None,
            tz_name: None,
            onair: None,
            onair_link: Err(crate::tr!("onair-stream-opening")),
        }
    }

    /// Intègre une lecture du bandeau. Rend `true` si stationd a répondu.
    pub fn apply_banner(&mut self, read: BannerRead) -> bool {
        let at = read.at;
        let ok = read.status.is_ok();

        self.link = match (&read.status, &self.link) {
            (Ok(_), _) => Link::Connected,
            (Err(e), Link::Lost { since, .. }) => Link::Lost { since: *since, error: e.clone() },
            (Err(e), _) => Link::Lost { since: at, error: e.clone() },
        };

        if let Ok(s) = &read.status
            && self.tz_name.as_deref() != Some(s.timezone.as_str())
        {
            self.tz_name = Some(s.timezone.clone());
            self.tz = TimeZone::get(&s.timezone).ok();
        }

        self.status.apply(read.status, at);
        self.broadcast.apply(read.broadcast, at);
        self.liquidsoap.apply(read.liquidsoap, at);
        self.live.apply(read.live, at);
        self.overrides.apply(read.overrides, at);
        self.plugins.apply(read.plugins, at);
        ok
    }

    /// Un instantané de l'antenne arrive : on ignore un instantané plus
    /// ancien que celui qu'on a (révision croissante), sauf après une
    /// reconnexion où stationd a pu redémarrer (révision repartie de 1).
    pub fn apply_onair(&mut self, snap: onair::OnAirSnapshot, fresh_stream: bool) {
        let older = self.onair.as_ref().is_some_and(|o| o.revision >= snap.revision);
        if older && !fresh_stream {
            return;
        }
        self.onair_link = Ok(());
        self.onair = Some(snap);
    }

    /// Uptime de stationd extrapolé depuis la dernière observation. `None`
    /// tant qu'aucun `Status` n'a été reçu ou si la liaison est perdue.
    pub fn uptime(&self, now: Instant) -> Option<Duration> {
        if self.link != Link::Connected {
            return None;
        }
        let s = self.status.value.as_ref()?;
        let age = self.status.age(now)?;
        Some(Duration::from_secs(s.uptime_seconds) + age)
    }

    /// Un plugin déclaré et chargé (`state == "loaded"`).
    pub fn plugin_loaded(&self, name: &str) -> bool {
        self.plugins
            .value
            .as_ref()
            .is_some_and(|ps| ps.iter().any(|p| p.name == name && p.state == "loaded"))
    }
}

/// Heure locale `HH:MM:SS` d'un instant epoch (s) dans le fuseau station ;
/// en UTC suffixé si le fuseau est inconnu (jamais un faux local).
pub fn local_hms(tz: Option<&TimeZone>, epoch_s: i64) -> Option<String> {
    let ts = jiff::Timestamp::from_second(epoch_s).ok()?;
    Some(match tz {
        Some(tz) => ts.to_zoned(tz.clone()).strftime("%H:%M:%S").to_string(),
        None => format!("{} UTC", ts.to_zoned(TimeZone::UTC).strftime("%H:%M:%S")),
    })
}

/// `2j 04h 18m`, `4h 02m`, `12m 05s` (unités traduites).
pub fn human_duration(d: Duration) -> String {
    let s = d.as_secs();
    let (days, hours, mins, secs) = (s / 86_400, (s / 3600) % 24, (s / 60) % 60, s % 60);
    let p2 = |v: u64| format!("{v:02}");
    if days > 0 {
        crate::tr!("duration-days", d = days.to_string(), h = p2(hours), m = p2(mins))
    } else if hours > 0 {
        crate::tr!("duration-hours", h = hours.to_string(), m = p2(mins))
    } else {
        crate::tr!("duration-minutes", m = mins.to_string(), s = p2(secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(status: Result<station::StatusReply, String>, at: Instant) -> BannerRead {
        BannerRead {
            at,
            status,
            broadcast: Err("x".into()),
            liquidsoap: Err("x".into()),
            live: Err("x".into()),
            overrides: Err("x".into()),
            plugins: Err("x".into()),
        }
    }

    fn status(tz: &str, up: u64) -> station::StatusReply {
        station::StatusReply {
            station_name: "radio".into(),
            uptime_seconds: up,
            pid: 1,
            timezone: tz.into(),
        }
    }

    #[test]
    fn error_keeps_last_value() {
        let mut s: Sourced<u32> = Sourced::default();
        let t = Instant::now();
        s.apply(Ok(3), t);
        s.apply(Err("panne".into()), t);
        assert_eq!(s.value, Some(3));
        assert_eq!(s.error.as_deref(), Some("panne"));
        assert!(s.is_stale(t));
    }

    #[test]
    fn never_received_is_stale_and_unknown() {
        let s: Sourced<u32> = Sourced::default();
        assert!(s.value.is_none());
        assert!(s.is_stale(Instant::now()));
    }

    #[test]
    fn link_lost_keeps_first_failure_instant() {
        let mut st = Store::new("http://x");
        let t0 = Instant::now();
        st.apply_banner(read(Err("a".into()), t0));
        let t1 = t0 + Duration::from_secs(4);
        st.apply_banner(read(Err("b".into()), t1));
        match &st.link {
            Link::Lost { since, error } => {
                assert_eq!(*since, t0);
                assert_eq!(error, "b");
            }
            other => panic!("attendu Lost, reçu {other:?}"),
        }
        st.apply_banner(read(Ok(status("UTC", 10)), t1));
        assert_eq!(st.link, Link::Connected);
    }

    #[test]
    fn timezone_resolved_and_unknown_kept_by_name() {
        let mut st = Store::new("http://x");
        let t = Instant::now();
        st.apply_banner(read(Ok(status("UTC", 10)), t));
        assert!(st.tz.is_some());
        st.apply_banner(read(Ok(status("Nulle/Part", 10)), t));
        assert!(st.tz.is_none());
        assert_eq!(st.tz_name.as_deref(), Some("Nulle/Part"));
    }

    #[test]
    fn uptime_extrapolated_and_hidden_when_lost() {
        let mut st = Store::new("http://x");
        let t = Instant::now();
        st.apply_banner(read(Ok(status("UTC", 100)), t));
        assert_eq!(st.uptime(t + Duration::from_secs(5)), Some(Duration::from_secs(105)));
        st.apply_banner(read(Err("x".into()), t));
        assert_eq!(st.uptime(t), None);
    }

    #[test]
    fn local_hms_uses_zone_or_says_utc() {
        let paris = TimeZone::get("Europe/Paris").unwrap();
        // 2026-09-28T12:00:00Z = 14:00 à Paris (heure d'été).
        assert_eq!(local_hms(Some(&paris), 1_790_596_800).as_deref(), Some("14:00:00"));
        assert_eq!(local_hms(None, 1_790_596_800).as_deref(), Some("12:00:00 UTC"));
    }

    #[test]
    fn human_duration_formats() {
        assert_eq!(human_duration(Duration::from_secs(2 * 86_400 + 4 * 3600 + 18 * 60)), "2j 04h 18m");
        assert_eq!(human_duration(Duration::from_secs(4 * 3600 + 2 * 60)), "4h 02m");
        assert_eq!(human_duration(Duration::from_secs(12 * 60 + 5)), "12m 05s");
    }

    #[test]
    fn an_older_onair_snapshot_is_ignored_unless_the_stream_restarted() {
        let mut st = Store::new("http://x");
        let snap = |rev| onair::OnAirSnapshot { revision: rev, ..Default::default() };
        st.apply_onair(snap(5), true);
        st.apply_onair(snap(4), false);
        assert_eq!(st.onair.as_ref().unwrap().revision, 5);
        st.apply_onair(snap(1), true);
        assert_eq!(st.onair.as_ref().unwrap().revision, 1);
    }
}
