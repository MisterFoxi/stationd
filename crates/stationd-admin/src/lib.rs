// tonic 0.12 uses Status as its transport error, including generated RPC code.
#![allow(clippy::result_large_err)]
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;
use tonic::{Request, Response, Status};

pub mod rpc {
    tonic::include_proto!("stationd.admin.icecast.v1");
}
use rpc::icecast_admin_server::IcecastAdmin;
use rpc::*;

#[derive(Deserialize)]
pub struct Config {
    pub admin_user: String,
    pub admin_password_file: String,
    pub mounts: Vec<Mount>,
}
#[derive(Deserialize)]
pub struct Mount {
    pub name: String,
}

pub fn read_secret(path: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(path).context("cannot read secret file")?;
    let value = raw.trim_end_matches(['\r', '\n']);
    if value.is_empty() || value.chars().any(char::is_control) {
        bail!("empty or invalid secret file");
    }
    Ok(value.to_owned())
}

pub fn authorized(expected: &str, received: &str) -> bool {
    // Equal-length token comparison without data-dependent early exits.
    let mut difference = expected.len() ^ received.len();
    for (a, b) in expected.bytes().zip(received.bytes()) {
        difference |= usize::from(a ^ b);
    }
    difference == 0
}

pub struct Admin {
    client: reqwest::Client,
    upstream: reqwest::Url,
    config: Config,
    password: String,
}

fn child(node: roxmltree::Node<'_, '_>, name: &str) -> String {
    node.children()
        .find(|n| n.has_tag_name(name))
        .and_then(|n| n.text())
        .unwrap_or_default()
        .to_owned()
}

impl Admin {
    pub fn load(config: &Path, secrets: &Path, upstream: &str) -> Result<Self> {
        let text = std::fs::read_to_string(config).context("cannot read central Icecast TOML")?;
        let config: Config =
            toml::from_str(&text).map_err(|_| anyhow::anyhow!("invalid central Icecast TOML"))?;
        let name = &config.admin_password_file;
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            bail!("invalid admin secret name");
        }
        let password = read_secret(&secrets.join(name))?;
        let upstream = reqwest::Url::parse(upstream).context("invalid Icecast upstream URL")?;
        if upstream.scheme() != "http"
            || upstream.host_str().is_none()
            || !upstream.username().is_empty()
            || upstream.password().is_some()
            || upstream.path() != "/"
            || upstream.query().is_some()
            || upstream.fragment().is_some()
        {
            bail!("Icecast upstream must be an HTTP origin on the private container network");
        }
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()?;
        Ok(Self {
            client,
            upstream,
            config,
            password,
        })
    }

    fn mount(&self, mount: &str) -> Result<(), Status> {
        if !self.config.mounts.iter().any(|m| m.name == mount) {
            return Err(Status::invalid_argument(
                "mount not declared in central configuration",
            ));
        }
        Ok(())
    }

    async fn get(&self, endpoint: &str, query: &[(&str, &str)]) -> Result<String, Status> {
        let url = self
            .upstream
            .join(&format!("/admin/{endpoint}"))
            .map_err(|_| Status::internal("invalid upstream endpoint"))?;
        let mut response = self
            .client
            .get(url)
            .query(query)
            .basic_auth(&self.config.admin_user, Some(&self.password))
            .send()
            .await
            .map_err(|_| Status::unavailable("Icecast unreachable or timeout"))?;
        if !response.status().is_success() {
            return Err(match response.status().as_u16() {
                401 | 403 => Status::failed_precondition("Icecast rejected admin credentials"),
                404 => Status::not_found("Icecast mount or endpoint not found"),
                _ => Status::unavailable("Icecast rejected the operation"),
            });
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| Status::unavailable("incomplete Icecast response"))?
        {
            if body.len() + chunk.len() > 2 * 1024 * 1024 {
                return Err(Status::resource_exhausted("Icecast response too large"));
            }
            body.extend_from_slice(&chunk);
        }
        String::from_utf8(body).map_err(|_| Status::internal("invalid Icecast response encoding"))
    }

    async fn action(
        &self,
        endpoint: &str,
        query: &[(&str, &str)],
    ) -> Result<Response<ActionReply>, Status> {
        let body = self.get(endpoint, query).await?;
        let doc = roxmltree::Document::parse(&body)
            .map_err(|_| Status::internal("invalid Icecast action response"))?;
        if child(doc.root_element(), "return") != "1" {
            return Err(Status::failed_precondition(
                "Icecast did not apply the operation",
            ));
        }
        Ok(Response::new(ActionReply {
            message: "Applied by Icecast".to_owned(),
        }))
    }
}

#[tonic::async_trait]
impl IcecastAdmin for Admin {
    async fn get_status(&self, _: Request<Empty>) -> Result<Response<ServerStatus>, Status> {
        let body = self.get("stats", &[]).await?;
        let doc = roxmltree::Document::parse(&body)
            .map_err(|_| Status::internal("invalid Icecast stats"))?;
        let root = doc.root_element();
        let mounts = self
            .config
            .mounts
            .iter()
            .map(|mount| {
                let source = root.children().find(|n| {
                    n.has_tag_name("source") && n.attribute("mount") == Some(mount.name.as_str())
                });
                match source {
                    None => MountStatus {
                        mount: mount.name.clone(),
                        ..Default::default()
                    },
                    Some(source) => MountStatus {
                        mount: mount.name.clone(),
                        connected: true,
                        listeners: child(source, "listeners").parse().unwrap_or(0),
                        title: child(source, "title"),
                        content_type: child(source, "server_type"),
                    },
                }
            })
            .collect();
        Ok(Response::new(ServerStatus {
            server_id: child(root, "server_id"),
            mounts,
        }))
    }

    async fn list_listeners(
        &self,
        req: Request<MountRequest>,
    ) -> Result<Response<ListenerList>, Status> {
        let mount = req.into_inner().mount;
        self.mount(&mount)?;
        let body = self.get("listclients", &[("mount", &mount)]).await?;
        let doc = roxmltree::Document::parse(&body)
            .map_err(|_| Status::internal("invalid Icecast listener list"))?;
        let mut listeners = Vec::new();
        for node in doc.descendants().filter(|n| n.has_tag_name("listener")) {
            let id = child(node, "id")
                .parse()
                .or_else(|_| child(node, "ID").parse())
                .map_err(|_| Status::internal("invalid Icecast listener ID"))?;
            let ip = child(node, "ip");
            let agent = child(node, "useragent");
            let connected = child(node, "connected");
            listeners.push(Listener {
                id,
                ip: if ip.is_empty() { child(node, "IP") } else { ip },
                user_agent: if agent.is_empty() {
                    child(node, "UserAgent")
                } else {
                    agent
                },
                connected_seconds: connected
                    .parse()
                    .or_else(|_| child(node, "Connected").parse())
                    .unwrap_or(0),
            });
        }
        Ok(Response::new(ListenerList { listeners }))
    }

    async fn set_metadata(
        &self,
        req: Request<MetadataRequest>,
    ) -> Result<Response<ActionReply>, Status> {
        let req = req.into_inner();
        self.mount(&req.mount)?;
        if req.title.len() > 4096 || req.title.chars().any(char::is_control) {
            return Err(Status::invalid_argument("invalid title"));
        }
        self.action(
            "metadata",
            &[
                ("mount", &req.mount),
                ("mode", "updinfo"),
                ("song", &req.title),
                ("charset", "UTF-8"),
            ],
        )
        .await
    }

    async fn kick_listener(
        &self,
        req: Request<KickListenerRequest>,
    ) -> Result<Response<ActionReply>, Status> {
        let req = req.into_inner();
        self.mount(&req.mount)?;
        if req.id == 0 {
            return Err(Status::invalid_argument("listener ID must be positive"));
        }
        self.action(
            "killclient",
            &[("mount", &req.mount), ("id", &req.id.to_string())],
        )
        .await
    }

    async fn kick_source(
        &self,
        req: Request<MountRequest>,
    ) -> Result<Response<ActionReply>, Status> {
        let mount = req.into_inner().mount;
        self.mount(&mount)?;
        self.action("killsource", &[("mount", &mount)]).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn token_comparison_rejects_prefixes_empty_and_changed_values() {
        assert!(authorized("secret-token", "secret-token"));
        for wrong in ["", "secret", "secret-token-extra", "Secret-token"] {
            assert!(!authorized("secret-token", wrong));
        }
    }
    #[test]
    fn parses_current_and_legacy_listener_ids() {
        for xml in [
            "<listener><id>42</id></listener>",
            "<listener><ID>42</ID></listener>",
        ] {
            let doc = roxmltree::Document::parse(xml).unwrap();
            let node = doc.root_element();
            let id: u64 = child(node, "id")
                .parse()
                .or_else(|_| child(node, "ID").parse())
                .unwrap();
            assert_eq!(id, 42);
        }
    }
}
