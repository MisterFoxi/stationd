//! stationd-tui — outil d'administration de stationd en terminal.
//!
//! Client gRPC pur, au même rang que `stationctl` : toute donnée vient d'un
//! RPC, toute action est un RPC ; aucune logique métier, aucun fichier écrit.
//! Référence : `Doc/StationD-TUI-Dossier-technique.md`.
//!
//! Lot 0 : socle (boucle rat-salsa + tokio, registre d'écrans, bandeau,
//! connexion). Les écrans sont des emplacements remplis lot par lot.

mod action;
mod agenda;
mod app;
mod webmin_diagnostics;
mod banner;
mod operator_notice;
mod dialog;
mod draft;
mod fit;
mod gridraft;
mod i18n;
mod rpc;
mod screen;
mod screens;
mod store;
mod style;

use anyhow::Result;
use clap::{CommandFactory, FromArgMatches, Parser};
use rat_salsa::poll::{PollCrossterm, PollRendered, PollTimers, PollTokio};
use rat_salsa::{RunConfig, run_tui};

// L'aide de la ligne de commande (`--help`) reste en anglais, comme celle de
// stationctl : l'interface, elle, est traduite (`i18n`, `--lang`).
#[derive(Parser, Debug)]
#[command(name = "stationd-tui", about = "stationd administration in a terminal")]
pub struct Args {
    /// stationd host, host:port or URL (default: local TOML grpc_bind, else loopback)
    #[arg(long, default_value = "http://127.0.0.1:50051")]
    pub addr: String,

    /// Interface language: fr (default), en, de. Default: from LC_ALL / LC_MESSAGES / LANG
    #[arg(long)]
    pub lang: Option<String>,

    /// rat-theme4 theme (e.g. "Imperial", "Nord", "Imperial Shell")
    #[arg(long, default_value = "Imperial")]
    pub theme: String,

    /// List the available themes and exit
    #[arg(long)]
    pub list_themes: bool,
}

fn main() -> Result<()> {
    let matches = Args::command().get_matches();
    let mut args = Args::from_arg_matches(&matches)?;
    i18n::init(args.lang.as_deref());
    if args.list_themes {
        for name in rat_theme4::salsa_themes() {
            println!("{name}");
        }
        return Ok(());
    }

    let root = std::env::var_os("STATIOND_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    args.addr = resolve_address(&args, &matches, &root)?;

    let rt = tokio::runtime::Runtime::new()?;
    // Le canal tonic se crée dans le contexte du runtime (connexion paresseuse :
    // stationd absent au lancement n'empêche pas la TUI de démarrer).
    let (channel, long_channel) = {
        let _guard = rt.enter();
        (rpc::lazy_channel(&args.addr)?, rpc::lazy_channel_long(&args.addr)?)
    };

    let mut global = app::Global::new(&args, channel, long_channel);
    let mut state = app::Scenery::new();

    run_tui(
        app::init,
        app::render,
        webmin_diagnostics::event,
        app::error,
        &mut global,
        &mut state,
        RunConfig::default()?
            .poll(PollCrossterm)
            .poll(PollTimers::default())
            .poll(PollRendered)
            .poll(PollTokio::new(rt)),
    )?;
    Ok(())
}

/// Distinguish an explicit --addr from clap's display/default value.
fn resolve_address(args: &Args, matches: &clap::ArgMatches, root: &std::path::Path) -> Result<String> {
    let explicit = (matches.value_source("addr") == Some(clap::parser::ValueSource::CommandLine))
        .then_some(args.addr.as_str());
    rpc::normalize_address(&stationd_client_config::resolve(explicit, root)?)
}

#[cfg(test)]
mod address_tests {
    use super::*;

    #[test]
    fn tui_address_prefers_explicit_argument_then_local_toml_then_loopback() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("stationd.toml");
        let matches = Args::command().try_get_matches_from(["stationd-tui"]).unwrap();
        let args = Args::from_arg_matches(&matches).unwrap();
        assert_eq!(resolve_address(&args, &matches, root.path()).unwrap(), "http://127.0.0.1:50051/");
        std::fs::write(&config, "[server]\ngrpc_bind = '192.168.1.135:50051'\n").unwrap();
        assert_eq!(resolve_address(&args, &matches, root.path()).unwrap(), "http://192.168.1.135:50051/");
        let matches = Args::command().try_get_matches_from(["stationd-tui", "--addr", "other.lan:6000"]).unwrap();
        let args = Args::from_arg_matches(&matches).unwrap();
        assert_eq!(resolve_address(&args, &matches, root.path()).unwrap(), "http://other.lan:6000/");
        std::fs::write(&config, "not TOML").unwrap();
        assert_eq!(resolve_address(&args, &matches, root.path()).unwrap(), "http://other.lan:6000/");
    }
}
