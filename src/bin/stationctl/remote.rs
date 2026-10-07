//! Operator-only remote identity commands, transported through PluginService.
use clap::Subcommand;
use serde_json::{json, Value};
use stationd::proto::plugin::{plugin_service_client::PluginServiceClient, PluginAdminRequest};
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Role {
    Viewer,
    Helper,
    Admin,
}
impl Role {
    fn text(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Helper => "helper",
            Self::Admin => "admin",
        }
    }
}
#[derive(Debug, Subcommand)]
pub enum UserCommand {
    /// Create an account and print a temporary enrollment URL
    Add {
        name: String,
        #[arg(long, value_enum)]
        role: Role,
        #[arg(long = "station", required = true)]
        stations: Vec<String>,
    },
    List,
    Show {
        name: String,
    },
    /// Issue a fresh one-use URL to set or reset the password
    Enroll {
        name: String,
    },
    /// Grant/change a role on one station; revokes existing Web sessions
    Role {
        name: String,
        #[arg(long)]
        station: String,
        #[arg(value_enum)]
        role: Role,
    },
    /// Revoke the account, its credentials, enrollments and sessions
    Revoke {
        name: String,
    },
    /// Permanently delete an active or revoked account and all its access
    Purge {
        name: String,
    },
    /// Revoke one passkey (credential ID from remote-user show)
    RevokeDevice {
        name: String,
        credential: String,
    },
}
#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    List,
    Revoke { id: String },
    RevokeUser { name: String },
}
async fn send(addr: &str, payload: Value) -> anyhow::Result<()> {
    let mut client = PluginServiceClient::connect(addr.to_string()).await?;
    let response = client
        .admin(PluginAdminRequest {
            name: "remote-supervision".into(),
            payload: serde_json::to_string(&payload)?,
        })
        .await?
        .into_inner();
    let value: Value = serde_json::from_str(&response.payload)?;
    if let Some(url) = value.get("enrollment_url").and_then(Value::as_str) {
        println!("Account: {}", value["name"].as_str().unwrap_or_default());
        println!("Enrollment URL (one use):\n{url}");
        println!("Expires at: {} (Unix UTC)", value["expires_at"]);
    } else {
        println!("{}", serde_json::to_string_pretty(&value)?);
    }
    Ok(())
}
pub async fn user(addr: &str, command: UserCommand) -> anyhow::Result<()> {
    let payload = match command {
        UserCommand::Add {
            name,
            role,
            stations,
        } => json!({"action":"add","name":name,"role":role.text(),"stations":stations}),
        UserCommand::List => json!({"action":"list"}),
        UserCommand::Show { name } => json!({"action":"show","name":name}),
        UserCommand::Enroll { name } => json!({"action":"enroll","name":name}),
        UserCommand::Role {
            name,
            station,
            role,
        } => json!({"action":"role","name":name,"station":station,"role":role.text()}),
        UserCommand::Revoke { name } => json!({"action":"revoke","name":name}),
        UserCommand::Purge { name } => json!({"action":"purge","name":name}),
        UserCommand::RevokeDevice { name, credential } => {
            json!({"action":"revoke_device","name":name,"credential":credential})
        }
    };
    send(addr, payload).await
}
pub async fn session(addr: &str, command: SessionCommand) -> anyhow::Result<()> {
    let payload = match command {
        SessionCommand::List => json!({"action":"sessions"}),
        SessionCommand::Revoke { id } => json!({"action":"revoke_session","id":id}),
        SessionCommand::RevokeUser { name } => json!({"action":"revoke_sessions","name":name}),
    };
    send(addr, payload).await
}
pub async fn audit(addr: &str) -> anyhow::Result<()> {
    send(addr, json!({"action":"audit"})).await
}
