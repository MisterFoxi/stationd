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
    Status,
    Listeners { mount: String },
    Metadata { mount: String, title: String },
    KickListener { mount: String, id: u64 },
    KickSource { mount: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token = format!("Bearer {}", read_secret(&args.token_file)?)
        .parse::<tonic::metadata::MetadataValue<_>>()?;
    let channel = tonic::transport::Endpoint::from_shared(args.addr)?
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(10))
        .connect()
        .await?;
    let mut client =
        IcecastAdminClient::with_interceptor(channel, move |mut req: tonic::Request<()>| {
            req.metadata_mut().insert("authorization", token.clone());
            req.set_timeout(std::time::Duration::from_secs(10));
            Ok(req)
        });
    match args.command {
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
                "id": l.id, "ip": l.ip, "user_agent": l.user_agent, "connected_seconds": l.connected_seconds
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
