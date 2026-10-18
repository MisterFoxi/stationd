use crate::{listener_snapshot, rpc::*, Admin};
use station_icecast_config::{
    geoip::{is_public, DATA_ATTRIBUTION},
    revision, Config,
};
use std::collections::BTreeMap;
use tonic::Status;

impl Admin {
    pub(crate) fn configuration(&self) -> Result<ConfigDocument, Status> {
        let text = match &self.effective_config {
            Some(path) => std::fs::read_to_string(path)
                .map_err(|_| Status::unavailable("effective Icecast configuration unavailable"))?,
            None => self.raw_config.clone(),
        };
        Config::parse(&text)
            .map_err(|_| Status::unavailable("effective Icecast configuration is invalid"))?;
        Ok(ConfigDocument {
            revision: revision(&text),
            toml: text,
        })
    }
    pub(crate) fn validate_configuration(&self, text: &str) -> Result<ConfigCheck, Status> {
        let config = Config::parse(text).map_err(|e| Status::invalid_argument(e.to_string()))?;
        config
            .open_geoip()
            .map_err(|_| Status::failed_precondition("GeoIP database unavailable or invalid"))?;
        let mut warnings = Vec::new();
        for mount in &config.mounts {
            if !self
                .config
                .mounts
                .iter()
                .any(|m| m.password_file == mount.password_file)
            {
                warnings.push(format!(
                    "source secret {} must be provisioned before application",
                    mount.password_file
                ));
            }
        }
        if config.admin_password_file != self.config.admin_password_file {
            warnings.push("new admin secret must be provisioned before application".into());
        }
        warnings.push("source secrets are fully verified by icecast-apply before restart".into());
        Ok(ConfigCheck {
            revision: revision(text),
            warnings,
        })
    }
    pub(crate) async fn listeners_snapshot(&self, mount: &str) -> Result<ListenerList, Status> {
        self.mount(mount)?;
        let body = self.get("listclients", &[("mount", mount)]).await?;
        let raw = listener_snapshot::parse_clients(&body, mount)
            .map_err(|_| Status::unavailable("invalid or incomplete Icecast listener snapshot"))?;
        let mut listeners = Vec::with_capacity(raw.len());
        for listener in raw {
            let (lookup_status, country, region, city) = if !is_public(listener.ip) {
                (
                    "private".to_owned(),
                    String::new(),
                    String::new(),
                    String::new(),
                )
            } else if let Some(geoip) = &self.geoip {
                match geoip.lookup(listener.ip) {
                    Ok(location) => (
                        location.status.to_owned(),
                        location.country.unwrap_or_default(),
                        location.region.unwrap_or_default(),
                        location.city.unwrap_or_default(),
                    ),
                    Err(_) => (
                        "error".to_owned(),
                        String::new(),
                        String::new(),
                        String::new(),
                    ),
                }
            } else {
                (
                    "unavailable".to_owned(),
                    String::new(),
                    String::new(),
                    String::new(),
                )
            };
            listeners.push(Listener {
                id: listener
                    .id
                    .parse()
                    .map_err(|_| Status::internal("invalid listener ID"))?,
                ip: listener.ip.to_string(),
                user_agent: listener.user_agent.unwrap_or_default(),
                connected_seconds: listener.connected_seconds,
                lookup_status,
                country,
                region,
                city,
            });
        }
        Ok(ListenerList { listeners })
    }
    pub(crate) async fn geography_snapshot(&self) -> GeographySnapshot {
        let mut result = GeographySnapshot {
            observed_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            complete: true,
            geoip_status: if self.geoip.is_some() {
                "ready"
            } else {
                "disabled"
            }
            .into(),
            database_type: self
                .geoip
                .as_ref()
                .map(|g| g.database_type().to_owned())
                .unwrap_or_default(),
            database_build_epoch: self
                .geoip
                .as_ref()
                .map(|g| g.database_build_epoch())
                .unwrap_or(0),
            attribution: DATA_ATTRIBUTION.to_owned(),
            ..Default::default()
        };
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
        let mut buckets = BTreeMap::<(String, String, String, String, String), u64>::new();
        for mount in self.config.mounts.iter().filter(|m| m.audience) {
            match tokio::time::timeout_at(deadline, self.listeners_snapshot(&mount.name)).await {
                Ok(Ok(list)) => {
                    result.mounts.push(GeographyMount {
                        mount: mount.name.clone(),
                        available: true,
                        listeners: list.listeners.len() as u64,
                        problem: String::new(),
                    });
                    for l in list.listeners {
                        *buckets
                            .entry((
                                mount.name.clone(),
                                l.lookup_status,
                                l.country,
                                l.region,
                                l.city,
                            ))
                            .or_default() += 1;
                    }
                }
                other => {
                    result.complete = false;
                    let problem = match other {
                        Ok(Err(e)) => e.message().to_owned(),
                        _ => "snapshot deadline exceeded".into(),
                    };
                    result.mounts.push(GeographyMount {
                        mount: mount.name.clone(),
                        available: false,
                        listeners: 0,
                        problem,
                    });
                }
            }
        }
        result.buckets = buckets
            .into_iter()
            .map(
                |((mount, lookup_status, country, region, city), listeners)| GeographyBucket {
                    mount,
                    lookup_status,
                    country,
                    region,
                    city,
                    listeners,
                },
            )
            .collect();
        result
    }
}
