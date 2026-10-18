//! Companion process for services that do not embed the Rust announcement hook.
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), stationdeck_discovery::Error> {
    let Some(mut task) = stationdeck_discovery::start_from_env().await? else {
        return Err(stationdeck_discovery::Error::InvalidConfiguration);
    };
    tokio::select! {
        _ = &mut task => Err(stationdeck_discovery::Error::Io),
        _ = tokio::signal::ctrl_c() => { task.abort(); Ok(()) },
    }
}
