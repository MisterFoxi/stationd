use crate::{
    store::Bootstrap,
    wire::{configuration_client::ConfigurationClient, PullRequest, ReportRequest},
};
use serde::{Deserialize, Serialize};
use stationdeck_services::config::*;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use tonic::{
    transport::{Channel, ClientTlsConfig, Endpoint},
    Request,
};
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub authority: String,
    pub target: String,
    pub endpoint: Option<String>,
    pub discovery_address: String,
    pub allow_plaintext: bool,
    pub tls_server_name: Option<String>,
    pub ca_file: Option<PathBuf>,
    pub token_file: PathBuf,
    pub catalog_file: PathBuf,
    pub state_dir: PathBuf,
    // The program is local bootstrap policy, never supplied by a publication.
    // It receives one candidate JSON path; zero must confirm actual application.
    pub apply_program: Option<PathBuf>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cache {
    format: u32,
    bundle: Bundle,
}
pub fn validate_bundle(
    config: &AgentConfig,
    bootstrap: &Bootstrap,
    b: &Bundle,
) -> Result<(), &'static str> {
    if b.authority != config.authority || b.target != config.target || b.revision == 0 {
        return Err("Configuration identity or revision mismatch.");
    }
    let target = bootstrap
        .targets
        .iter()
        .find(|t| t.id == config.target)
        .ok_or("Agent target is not provisioned.")?;
    validate(&bootstrap.schemas, &target.plugins, &b.documents)
}
/// Explicit endpoint is selected before any UDP request is attempted.
pub async fn resolve(config: &AgentConfig) -> Result<String, &'static str> {
    let raw = match &config.endpoint {
        Some(value) if !value.trim().is_empty() => value.clone(),
        _ => {
            crate::discovery::discover(
                &config.discovery_address,
                &config.authority,
                Duration::from_millis(750),
            )
            .await?
        }
    };
    let address = if raw.contains("://") {
        raw
    } else {
        format!(
            "{}://{raw}",
            if config.allow_plaintext {
                "http"
            } else {
                "https"
            }
        )
    };
    let endpoint =
        Endpoint::from_shared(address.clone()).map_err(|_| "Invalid configuration endpoint.")?;
    let uri = endpoint.uri();
    if !matches!(uri.scheme_str(), Some("https"))
        && !(uri.scheme_str() == Some("http") && config.allow_plaintext)
    {
        return Err("TLS is required unless private-network plaintext is explicitly enabled.");
    }
    if uri.authority().is_none_or(|a| a.as_str().contains('@'))
        || uri.path() != "/"
        || uri.query().is_some()
    {
        return Err("Invalid configuration endpoint.");
    }
    Ok(address)
}
async fn client(config: &AgentConfig) -> Result<ConfigurationClient<Channel>, &'static str> {
    let address = resolve(config).await?;
    let mut endpoint = Endpoint::from_shared(address.clone())
        .map_err(|_| "Invalid configuration endpoint.")?
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(4));
    if address.starts_with("https:") {
        let mut tls = ClientTlsConfig::new().with_native_roots();
        if let Some(name) = &config.tls_server_name {
            tls = tls.domain_name(name);
        }
        if let Some(path) = &config.ca_file {
            tls = tls.ca_certificate(tonic::transport::Certificate::from_pem(
                fs::read(path).map_err(|_| "Cannot read authority CA.")?,
            ));
        }
        endpoint = endpoint
            .tls_config(tls)
            .map_err(|_| "Invalid authority TLS configuration.")?;
    }
    let channel = endpoint
        .connect()
        .await
        .map_err(|_| "Configuration authority is unavailable.")?;
    Ok(ConfigurationClient::new(channel)
        .max_decoding_message_size(MAX_BYTES + 8192)
        .max_encoding_message_size(4096))
}
fn authenticated<T>(value: T, token: &str) -> Result<Request<T>, &'static str> {
    let mut request = Request::new(value);
    request.metadata_mut().insert(
        "authorization",
        format!("Bearer {token}")
            .parse()
            .map_err(|_| "Invalid agent credential.")?,
    );
    request.set_timeout(Duration::from_secs(4));
    Ok(request)
}
async fn apply(
    config: &AgentConfig,
    bootstrap: &Bootstrap,
    bundle: &Bundle,
) -> Result<(), &'static str> {
    validate_bundle(config, bootstrap, bundle).map_err(|_| "validation_failed")?;
    let bytes = serde_json::to_vec(bundle).map_err(|_| "validation_failed")?;
    let candidate = config.state_dir.join("candidate.json");
    crate::atomic_write(&candidate, &bytes).map_err(|_| "cache_failed")?;
    if let Some(program) = &config.apply_program {
        if !program.is_absolute() {
            return Err("apply_failed");
        }
        let mut cmd = tokio::process::Command::new(program);
        cmd.arg(&candidate)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let status = tokio::time::timeout(Duration::from_secs(15), cmd.status()).await;
        if !matches!(status,Ok(Ok(s)) if s.success()) {
            return Err("apply_failed");
        }
    } else {
        // A real built-in demo consumer; never claim a daemon/plugin was applied
        // merely because a configuration file was downloaded.
        if bundle.documents.len() != 1 || bundle.documents[0].plugin != "demo.settings" {
            return Err("apply_failed");
        }
        crate::atomic_write(&config.state_dir.join("effective.json"), &bytes)
            .map_err(|_| "apply_failed")?;
    }
    Ok(())
}
pub async fn run(config: AgentConfig) -> Result<(), &'static str> {
    let bootstrap: Bootstrap = serde_json::from_slice(
        &fs::read(&config.catalog_file).map_err(|_| "Agent catalog unavailable.")?,
    )
    .map_err(|_| "Invalid agent catalog.")?;
    bootstrap.validate()?;
    if bootstrap.authority != config.authority {
        return Err("Agent authority does not match its trusted catalog.");
    }
    let token = crate::read_token(&config.token_file)?;
    fs::create_dir_all(&config.state_dir).map_err(|_| "Cannot initialize agent state.")?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(config.state_dir.join("agent.lock"))
        .map_err(|_| "Cannot open agent lock.")?;
    lock.try_lock()
        .map_err(|_| "Agent state is already in use.")?;
    let cache_path = config.state_dir.join("last-valid.json");
    let mut applied: Option<Bundle> = None;
    if cache_path.exists() {
        let bytes = fs::read(&cache_path).map_err(|_| "Cannot read last valid configuration.")?;
        if bytes.len() > MAX_BYTES + 8192 {
            return Err("Last valid configuration exceeds the size limit.");
        }
        let cache: Cache = serde_json::from_slice(&bytes)
            .map_err(|_| "Last valid configuration is corrupt; refusing to reset it.")?;
        if cache.format != 1 {
            return Err("Unsupported agent cache format.");
        }
        apply(&config, &bootstrap, &cache.bundle)
            .await
            .map_err(|_| "Cannot restore the last valid configuration.")?;
        applied = Some(cache.bundle);
        eprintln!("Restored last valid configuration.");
    } else {
        eprintln!("Waiting for the first published configuration.");
    }
    let (runtime_tx, mut runtime_rx) = tokio::sync::watch::channel(applied.clone());
    let runtime_path = config.state_dir.join("runtime.json");
    let demo_runtime = config.apply_program.is_none();
    let runtime_task = tokio::spawn(async move {
        let mut ticks = 0u64;
        loop {
            let current = runtime_rx.borrow().clone();
            let Some(bundle) = current else {
                if runtime_rx.changed().await.is_err() {
                    break;
                }
                continue;
            };
            let doc = &bundle.documents[0];
            let interval = doc
                .values
                .get("interval_seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(1)
                .clamp(1, 3600);
            tokio::select! {
                changed=runtime_rx.changed()=>{if changed.is_err(){break;}},
                _=tokio::time::sleep(Duration::from_secs(interval))=>{
                    ticks=ticks.saturating_add(1);
                    let value=serde_json::json!({"revision":bundle.revision,"ticks":ticks,"message":doc.values.get("message"),"observed_at":crate::now()});
                    if let Ok(bytes)=serde_json::to_vec(&value) {let _=crate::atomic_write(&runtime_path,&bytes);}
                },
            }
        }
    });
    if !demo_runtime {
        runtime_task.abort();
    }
    let mut connection = None;
    let mut failures = 0u32;
    loop {
        let result=async {
            if connection.is_none() {connection=Some(client(&config).await?);}
            let grpc=connection.as_mut().unwrap();
            let reply=grpc.pull(authenticated(PullRequest{},&token)?).await.map_err(|_|"Configuration fetch failed.")?.into_inner();
            if reply.bundle_json.is_empty() {return Ok::<(),&'static str>(());}
            let bundle:Bundle=serde_json::from_str(&reply.bundle_json).map_err(|_|"Invalid configuration reply.")?;
            if bundle.authority!=config.authority || bundle.target!=config.target || bundle.revision==0 {return Err("Configuration identity or revision mismatch.");}
            if applied.as_ref().is_some_and(|old|old.revision>bundle.revision || (old.revision==bundle.revision && old!=&bundle)) {return Err("Authority returned an obsolete or conflicting revision.");}
            let code=if applied.as_ref()!=Some(&bundle) || config.apply_program.is_some() {
                match apply(&config,&bootstrap,&bundle).await {
                    Ok(())=>{
                        let cache=Cache{format:1,bundle:bundle.clone()};
                        if crate::atomic_write(&cache_path,&serde_json::to_vec(&cache).map_err(|_|"Cannot encode agent cache.")?).is_ok() {
                            applied=Some(bundle.clone());let _=runtime_tx.send(applied.clone());"applied"
                        } else {"cache_failed"}
                    },
                    Err(code)=>code,
                }
            }else{"applied"};
            grpc.report(authenticated(ReportRequest{revision:bundle.revision,applied:code=="applied",code:code.into()},&token)?).await.map_err(|_|"Application report could not be delivered.")?;
            if code!="applied" {return Err("Published configuration is not confirmed applied; last valid cache retained.");}
            Ok(())
        }.await;
        match result {
            Ok(()) => {
                failures = 0;
            }
            Err(message) => {
                eprintln!("{message}");
                connection = None;
                failures = failures.saturating_add(1).min(4);
            }
        }
        tokio::time::sleep(Duration::from_secs(if failures == 0 {
            3
        } else {
            2u64.pow(failures)
        }))
        .await;
    }
}
pub fn read_config(path: &Path) -> Result<AgentConfig, &'static str> {
    serde_json::from_slice(&fs::read(path).map_err(|_| "Agent bootstrap is unavailable.")?)
        .map_err(|_| "Invalid agent bootstrap.")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(root: &Path) -> AgentConfig {
        AgentConfig {
            authority: "stationdeck-dev".into(),
            target: "demo-1".into(),
            endpoint: Some("control.example:50070".into()),
            discovery_address: "invalid-discovery-address".into(),
            allow_plaintext: false,
            tls_server_name: None,
            ca_file: None,
            token_file: root.join("token"),
            catalog_file: root.join("catalog"),
            state_dir: root.into(),
            apply_program: None,
        }
    }
    #[tokio::test]
    async fn explicit_endpoint_precedes_discovery_and_requires_tls() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(dir.path());
        assert_eq!(
            resolve(&config).await.unwrap(),
            "https://control.example:50070"
        );
        config.endpoint = Some("http://control.example:50070".into());
        assert!(resolve(&config).await.is_err());
        config.allow_plaintext = true;
        assert_eq!(
            resolve(&config).await.unwrap(),
            "http://control.example:50070"
        );
        config.endpoint = Some("http://user:password@host:50070".into());
        assert!(resolve(&config).await.is_err());
    }
    #[tokio::test]
    async fn failed_local_adapter_preserves_the_effective_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(dir.path());
        let bootstrap: Bootstrap =
            serde_json::from_str(include_str!("../../../config/stationdeck/catalog.json")).unwrap();
        let docs = vec![Document {
            plugin: "demo.settings".into(),
            instance: "default".into(),
            version: 1,
            values: [
                ("message".into(), serde_json::json!("first")),
                ("interval_seconds".into(), serde_json::json!(3)),
            ]
            .into(),
        }];
        let mut bundle = Bundle {
            authority: config.authority.clone(),
            target: config.target.clone(),
            revision: 1,
            documents: docs,
        };
        apply(&config, &bootstrap, &bundle).await.unwrap();
        let old = fs::read(dir.path().join("effective.json")).unwrap();
        config.apply_program = Some("/bin/false".into());
        bundle.revision = 2;
        assert_eq!(
            apply(&config, &bootstrap, &bundle).await,
            Err("apply_failed")
        );
        assert_eq!(fs::read(dir.path().join("effective.json")).unwrap(), old);
        bundle.target = "foreign-target".into();
        assert_eq!(
            apply(&config, &bootstrap, &bundle).await,
            Err("validation_failed")
        );
    }
}
