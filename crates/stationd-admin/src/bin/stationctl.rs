// tonic 0.12 uses Status as its transport error, including generated RPC code.
#![allow(clippy::result_large_err)]
use anyhow::Result;
use clap::{Parser, Subcommand};
use stationd_admin::{
    read_secret,
    rpc::{icecast_admin_client::IcecastAdminClient, *},
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "V1 stationctl — central administration via gRPC", version)]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:50061")]
    addr: String,
    #[arg(long, default_value = "/run/secrets/admin-api-token")]
    token_file: PathBuf,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    #[command(subcommand)]
    Icecast(Icecast),
}
#[derive(Subcommand)]
enum Icecast {
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Current country/region/city aggregates on audience mounts only
    Geography,
    HistoryStatus,
    History {
        #[arg(long, default_value = "listener-stats")]
        plugin: String,
        #[arg(long, default_value = "24h", value_parser = stationd_admin::listener_history::parse_window)]
        since: u64,
        #[arg(long, default_value = "hour")]
        by: String,
        #[arg(long, default_value = "")]
        mount: String,
    },
    Status,
    /// Stable output and selected input
    Route,
    /// Immediate switch of the stable output to a declared input
    Switch {
        input: String,
    },
    Listeners {
        mount: String,
    },
    Metadata {
        mount: String,
        title: String,
    },
    KickListener {
        mount: String,
        id: u64,
    },
    KickSource {
        mount: String,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    Show,
    Revision,
    Check { file: String },
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token = format!("Bearer {}", read_secret(&args.token_file)?)
        .parse::<tonic::metadata::MetadataValue<_>>()?;
    let channel = tonic::transport::Endpoint::from_shared(args.addr)?
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(15))
        .connect()
        .await?;
    let mut client =
        IcecastAdminClient::with_interceptor(channel, move |mut req: tonic::Request<()>| {
            req.metadata_mut().insert("authorization", token.clone());
            req.set_timeout(std::time::Duration::from_secs(15));
            Ok(req)
        });
    match args.command {
        Command::Icecast(Icecast::HistoryStatus) => {
            let json = client.get_history_status(Empty {}).await?.into_inner().json;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::from_str::<serde_json::Value>(&json)?)?
            );
        }
        Command::Icecast(Icecast::History {
            plugin,
            since,
            by,
            mount,
        }) => {
            let until = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            let reply = client
                .query_history(HistoryRequest {
                    plugin,
                    since: until.saturating_sub(since),
                    until,
                    period: by,
                    mount,
                })
                .await?
                .into_inner();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::from_str::<serde_json::Value>(
                    &reply.json
                )?)?
            );
        }
        Command::Icecast(Icecast::Config(command)) => match command {
            ConfigCommand::Show => {
                print!("{}", client.get_config(Empty {}).await?.into_inner().toml)
            }
            ConfigCommand::Revision => println!(
                "{}",
                client.get_config(Empty {}).await?.into_inner().revision
            ),
            ConfigCommand::Check { file } => {
                let text = if file == "-" {
                    use std::io::Read;
                    let mut text = String::new();
                    std::io::stdin()
                        .take(256 * 1024 + 1)
                        .read_to_string(&mut text)?;
                    text
                } else {
                    use std::io::Read;
                    let mut text = String::new();
                    std::fs::File::open(file)?
                        .take(256 * 1024 + 1)
                        .read_to_string(&mut text)?;
                    text
                };
                let checked = client
                    .check_config(ConfigText { toml: text })
                    .await?
                    .into_inner();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "valid":true,"revision":checked.revision,"warnings":checked.warnings
                    }))?
                );
            }
        },
        Command::Icecast(Icecast::Geography) => {
            let snapshot = client.get_geography(Empty {}).await?.into_inner();
            let mounts:Vec<_> = snapshot.mounts.into_iter().map(|m|serde_json::json!({
                "mount":m.mount,"available":m.available,"listeners":if m.available {Some(m.listeners)} else {None},
                "problem":m.problem
            })).collect();
            let buckets: Vec<_> = snapshot
                .buckets
                .into_iter()
                .map(|b| {
                    serde_json::json!({
                        "mount":b.mount,"lookup_status":b.lookup_status,"country":b.country,
                        "region":b.region,"city":b.city,"listeners":b.listeners
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "observed_at":snapshot.observed_at,"complete":snapshot.complete,
                    "geoip_status":snapshot.geoip_status,"database_type":snapshot.database_type,
                    "database_build_epoch":snapshot.database_build_epoch,"attribution":snapshot.attribution,
                    "mounts":mounts,"buckets":buckets
                }))?
            );
        }

        Command::Icecast(Icecast::Route) | Command::Icecast(Icecast::Switch { .. }) => {
            let state = match args.command {
                Command::Icecast(Icecast::Switch { input }) => client
                    .switch_route(SwitchRequest { input })
                    .await?
                    .into_inner(),
                _ => client.get_route(Empty {}).await?.into_inner(),
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "output_mount": state.output_mount, "input": state.input,
                    "connected": state.connected, "inputs": state.inputs, "problem": state.problem
                }))?
            );
        }

        Command::Icecast(Icecast::Status) => {
            let status = client.get_status(Empty {}).await?.into_inner();
            let mounts: Vec<_> = status
                .mounts
                .into_iter()
                .map(|m| {
                    serde_json::json!({
                        "mount": m.mount, "connected": m.connected, "listeners": m.listeners,
                        "title": m.title, "content_type": m.content_type
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "server_id": status.server_id, "mounts": mounts
                }))?
            );
        }
        Command::Icecast(Icecast::Listeners { mount }) => {
            let result = client
                .list_listeners(MountRequest { mount })
                .await?
                .into_inner();
            let listeners: Vec<_> = result.listeners.into_iter().map(|l| serde_json::json!({
                "id": l.id, "ip": l.ip, "user_agent": l.user_agent, "connected_seconds": l.connected_seconds, "lookup_status":l.lookup_status,"country":l.country,"region":l.region,"city":l.city
            })).collect();
            println!("{}", serde_json::to_string_pretty(&listeners)?);
        }
        Command::Icecast(Icecast::Metadata { mount, title }) => {
            println!(
                "{}",
                client
                    .set_metadata(MetadataRequest { mount, title })
                    .await?
                    .into_inner()
                    .message
            );
        }
        Command::Icecast(Icecast::KickListener { mount, id }) => {
            println!(
                "{}",
                client
                    .kick_listener(KickListenerRequest { mount, id })
                    .await?
                    .into_inner()
                    .message
            );
        }
        Command::Icecast(Icecast::KickSource { mount }) => {
            println!(
                "{}",
                client
                    .kick_source(MountRequest { mount })
                    .await?
                    .into_inner()
                    .message
            );
        }
    }
    Ok(())
}
