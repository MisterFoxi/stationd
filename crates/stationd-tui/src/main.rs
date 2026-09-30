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
mod banner;
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
use clap::Parser;
use rat_salsa::poll::{PollCrossterm, PollRendered, PollTimers, PollTokio};
use rat_salsa::{RunConfig, run_tui};

// L'aide de la ligne de commande (`--help`) reste en anglais, comme celle de
// stationctl : l'interface, elle, est traduite (`i18n`, `--lang`).
#[derive(Parser, Debug)]
#[command(name = "stationd-tui", about = "stationd administration in a terminal")]
pub struct Args {
    /// stationd gRPC address (same default as stationctl)
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
    let args = Args::parse();
    i18n::init(args.lang.as_deref());
    if args.list_themes {
        for name in rat_theme4::salsa_themes() {
            println!("{name}");
        }
        return Ok(());
    }

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
        app::event,
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
