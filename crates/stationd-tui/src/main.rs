//! stationd-tui — outil d'administration de stationd en terminal.
//!
//! Client gRPC pur, au même rang que `stationctl` : toute donnée vient d'un
//! RPC, toute action est un RPC ; aucune logique métier, aucun fichier écrit.
//! Référence : `Doc/StationD-TUI-Dossier-technique.md`.
//!
//! Lot 0 : socle (boucle rat-salsa + tokio, registre d'écrans, bandeau,
//! connexion). Les écrans sont des emplacements remplis lot par lot.

mod app;
mod banner;
mod fit;
mod rpc;
mod screen;
mod screens;
mod store;
mod style;

use anyhow::Result;
use clap::Parser;
use rat_salsa::poll::{PollCrossterm, PollRendered, PollTimers, PollTokio};
use rat_salsa::{RunConfig, run_tui};

#[derive(Parser, Debug)]
#[command(name = "stationd-tui", about = "Administration de stationd en terminal")]
pub struct Args {
    /// Adresse gRPC de stationd (même défaut que stationctl)
    #[arg(long, default_value = "http://127.0.0.1:50051")]
    pub addr: String,

    /// Thème rat-theme4 (ex. « Imperial », « Nord », « Imperial Shell »)
    #[arg(long, default_value = "Imperial")]
    pub theme: String,

    /// Liste les thèmes disponibles et quitte
    #[arg(long)]
    pub list_themes: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.list_themes {
        for name in rat_theme4::salsa_themes() {
            println!("{name}");
        }
        return Ok(());
    }

    let rt = tokio::runtime::Runtime::new()?;
    // Le canal tonic se crée dans le contexte du runtime (connexion paresseuse :
    // stationd absent au lancement n'empêche pas la TUI de démarrer).
    let channel = {
        let _guard = rt.enter();
        rpc::lazy_channel(&args.addr)?
    };

    let mut global = app::Global::new(&args, channel);
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
