use serde::Deserialize;
use stationd_proto::plugin::{
    plugin_service_client::PluginServiceClient, PluginConfigRequest, PluginConfigResponse,
    PluginConfigUpdateRequest, PluginConfigValue, PluginListRequest,
};
use stationdeck_services::config::{Bundle, Document};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use tonic::{
    transport::{Channel, ClientTlsConfig, Endpoint},
    Request,
};

/// Trusted local routing; publications cannot select a daemon or a plugin name.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    endpoint: String,
    allow_plaintext: bool,
    tls_server_name: Option<String>,
    ca_file: Option<PathBuf>,
    token_file: Option<PathBuf>,
    plugin: String,
    instance: String,
    daemon_plugin: String,
}
fn request<T>(body: T, token: Option<&str>) -> Result<Request<T>, &'static str> {
    let mut req = Request::new(body);
    req.set_timeout(Duration::from_secs(2));
    if let Some(token) = token {
        req.metadata_mut().insert(
            "authorization",
            format!("Bearer {token}")
                .parse()
                .map_err(|_| "Invalid SD credential.")?,
        );
    }
    Ok(req)
}
fn edits(
    doc: &Document,
    current: &PluginConfigResponse,
) -> Result<Vec<PluginConfigValue>, &'static str> {
    doc.values
        .iter()
        .map(|(key, value)| {
            let field = current
                .fields
                .iter()
                .find(|f| &f.key == key)
                .ok_or("SD schema changed; review the local catalog.")?;
            if field.secret {
                return Err("Secret fields must be provisioned locally.");
            }
            let text = match field.kind.as_str() {
                "text" | "duration" => value.as_str().map(str::to_owned),
                "integer" => value
                    .as_i64()
                    .filter(|n| {
                        field.minimum.is_none_or(|v| *n >= v)
                            && field.maximum.is_none_or(|v| *n <= v)
                    })
                    .map(|n| n.to_string()),
                "boolean" => value.as_bool().map(|b| b.to_string()),
                _ => None,
            }
            .ok_or("Published field does not match the SD schema.")?;
            Ok(PluginConfigValue {
                key: key.clone(),
                value: text,
                present: true,
                redacted: false,
            })
        })
        .collect()
}
fn matches(current: &PluginConfigResponse, edits: &[PluginConfigValue]) -> bool {
    !current.pending
        && edits.iter().all(|edit| {
            current
                .values
                .iter()
                .any(|v| v.key == edit.key && v.present && !v.redacted && v.value == edit.value)
        })
}
async fn get(
    client: &mut PluginServiceClient<Channel>,
    name: &str,
    token: Option<&str>,
) -> Result<PluginConfigResponse, &'static str> {
    let result = client
        .get_config(request(PluginConfigRequest { name: name.into() }, token)?)
        .await
        .map_err(|_| "SD configuration read failed.")?
        .into_inner();
    if result.name != name || result.revision.is_empty() || result.schema_revision.is_empty() {
        return Err("SD returned an invalid configuration identity.");
    }
    Ok(result)
}
pub async fn apply(candidate: &Path) -> Result<(), &'static str> {
    let agent_path = std::env::var("STATIONDECK_AGENT_CONFIG")
        .unwrap_or_else(|_| "/bootstrap/sd-agent.json".into());
    let config = crate::agent::read_config(Path::new(&agent_path))?;
    let bootstrap = serde_json::from_slice(
        &fs::read(&config.catalog_file).map_err(|_| "Local catalog unavailable.")?,
    )
    .map_err(|_| "Invalid local catalog.")?;
    let bytes = fs::read(candidate).map_err(|_| "Candidate unavailable.")?;
    if bytes.len() > stationdeck_services::config::MAX_BYTES + 8192 {
        return Err("Candidate exceeds the size limit.");
    }
    let bundle: Bundle = serde_json::from_slice(&bytes).map_err(|_| "Invalid candidate.")?;
    crate::agent::validate_bundle(&config, &bootstrap, &bundle)?;
    // One independently transactional plugin per target. Never partially commit a bundle.
    if bundle.documents.len() != 1 {
        return Err("SD targets require exactly one plugin document.");
    }
    let policy_path = std::env::var("STATIONDECK_SD_POLICY")
        .unwrap_or_else(|_| "/bootstrap/sd-adapter.json".into());
    let policy: Policy =
        serde_json::from_slice(&fs::read(policy_path).map_err(|_| "SD policy unavailable.")?)
            .map_err(|_| "Invalid SD policy.")?;
    let doc = &bundle.documents[0];
    if doc.plugin != policy.plugin
        || doc.instance != policy.instance
        || policy.daemon_plugin.is_empty()
        || policy.daemon_plugin.starts_with('@')
    {
        return Err("SD plugin is not authorized by local policy.");
    }
    let address = std::env::var("STATIONDECK_SD_ENDPOINT")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(policy.endpoint);
    if address.is_empty() {
        return Err("Set STATIONDECK_SD_ENDPOINT to the existing SD gRPC endpoint.");
    }
    let address = if address.contains("://") {
        address
    } else {
        format!(
            "{}://{address}",
            if policy.allow_plaintext {
                "http"
            } else {
                "https"
            }
        )
    };
    let mut endpoint = Endpoint::from_shared(address)
        .map_err(|_| "Invalid SD endpoint.")?
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(2));
    let uri = endpoint.uri();
    if uri.authority().is_none_or(|a| a.as_str().contains('@'))
        || uri.path() != "/"
        || uri.query().is_some()
        || !(uri.scheme_str() == Some("https")
            || (uri.scheme_str() == Some("http") && policy.allow_plaintext))
    {
        return Err("Invalid or unauthorized SD transport.");
    }
    if uri.scheme_str() == Some("https") {
        let mut tls = ClientTlsConfig::new().with_native_roots();
        if let Some(name) = policy.tls_server_name {
            tls = tls.domain_name(name);
        }
        if let Some(path) = policy.ca_file {
            tls = tls.ca_certificate(tonic::transport::Certificate::from_pem(
                fs::read(path).map_err(|_| "SD CA unavailable.")?,
            ));
        }
        endpoint = endpoint
            .tls_config(tls)
            .map_err(|_| "Invalid SD TLS policy.")?;
    }
    let token = policy
        .token_file
        .as_deref()
        .map(crate::read_token)
        .transpose()?;
    let token = token.as_deref();
    let mut client =
        PluginServiceClient::new(endpoint.connect().await.map_err(|_| "SD is unavailable.")?)
            .max_decoding_message_size(256 * 1024);
    let current = get(&mut client, &policy.daemon_plugin, token).await?;
    let desired = edits(doc, &current)?;
    if matches(&current, &desired) {
        return Ok(());
    }
    let plugins = client
        .list(request(PluginListRequest {}, token)?)
        .await
        .map_err(|_| "SD plugin state is unavailable.")?
        .into_inner();
    if !plugins
        .plugins
        .iter()
        .any(|p| p.name == policy.daemon_plugin && p.state == "loaded")
    {
        return Err("SD plugin must already be loaded; automatic start is not authorized.");
    }
    let update = PluginConfigUpdateRequest {
        name: policy.daemon_plugin.clone(),
        revision: current.revision.clone(),
        schema_revision: current.schema_revision.clone(),
        edits: desired.clone(),
        mode: 0,
    };
    client
        .update_config(request(update.clone(), token)?)
        .await
        .map_err(|_| "SD preview refused; no configuration was saved.")?;
    // Keep the original revision guards: a concurrent local edit must cause a conflict.
    let response = client
        .update_config(request(
            PluginConfigUpdateRequest { mode: 2, ..update },
            token,
        )?)
        .await
        .map_err(|_| "SD save/reload failed or its outcome is unknown; reconciliation required.")?
        .into_inner();
    if !response.saved || !response.applied || !matches(&response, &desired) {
        restore(&mut client, &current, &response, &desired, token).await?;
        return Err("SD reload failed; previous values restored. Check plugin runtime state.");
    }
    let verified = get(&mut client, &policy.daemon_plugin, token).await?;
    if verified.revision != response.revision || !matches(&verified, &desired) {
        return Err("SD configuration changed during verification.");
    }
    Ok(())
}

/// Restore only this transaction's edited fields, with fresh revision guards.
/// A failed plugin is never silently started; the operator may need to repair it.
async fn restore(
    client: &mut PluginServiceClient<Channel>,
    before: &PluginConfigResponse,
    saved: &PluginConfigResponse,
    desired: &[PluginConfigValue],
    token: Option<&str>,
) -> Result<(), &'static str> {
    let current = get(client, &before.name, token).await?;
    if current.revision == before.revision {
        return Ok(());
    }
    if current.revision != saved.revision || current.schema_revision != before.schema_revision {
        return Err("SD changed after reload failure; refusing to overwrite concurrent edits.");
    }
    let previous: Vec<PluginConfigValue> = desired
        .iter()
        .map(|edit| {
            before
                .values
                .iter()
                .find(|v| v.key == edit.key && !v.redacted)
                .cloned()
                .ok_or("Previous SD field is unavailable for restoration.")
        })
        .collect::<Result<_, _>>()?;
    let restored = client
        .update_config(request(
            PluginConfigUpdateRequest {
                name: before.name.clone(),
                revision: current.revision,
                schema_revision: current.schema_revision,
                edits: previous.clone(),
                mode: 2,
            },
            token,
        )?)
        .await
        .map_err(|_| "SD restoration failed or its outcome is unknown.")?
        .into_inner();
    let verified = get(client, &before.name, token).await?;
    if !restored.saved
        || restored.revision != verified.revision
        || !previous.iter().all(|old| {
            verified.values.iter().any(|v| {
                v.key == old.key
                    && v.present == old.present
                    && !v.redacted
                    && (!v.present || v.value == old.value)
            })
        })
    {
        return Err("Previous SD configuration could not be restored.");
    }
    Ok(())
}
