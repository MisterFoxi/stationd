//! breath-detect — détecteur de respirations Suno (spec v1), CLI autonome.

mod dsp;
mod yin;
mod lib_store;
mod detect;
mod cue;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "breath-detect", about = "Détecteur de respirations Suno sur stem vocal (v1)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Construit/enrichit la bibliothèque depuis un stem + des centres (s).
    BuildLib {
        #[arg(long)] stem: PathBuf,
        /// Centres des souffles en secondes, séparés par des virgules.
        #[arg(long)] centers: String,
        #[arg(long, default_value = "breath_lib.json")] lib: PathBuf,
        #[arg(long, default_value = "")] note: String,
        /// Ajoute à une biblio existante au lieu d'en créer une.
        #[arg(long)] append: bool,
    },
    /// Détecte les souffles d'un stem, écrit les régions en JSON.
    Detect {
        #[arg(long)] stem: PathBuf,
        #[arg(long, default_value = "breath_lib.json")] lib: PathBuf,
        #[arg(long, default_value_t = 0.45)] thr: f64,
        #[arg(long)] out: Option<PathBuf>,
    },
    /// Écrit un WAV marqué (cue + regions) pour RX, depuis un JSON de régions.
    Cue {
        #[arg(long)] stem: PathBuf,
        #[arg(long)] regions: PathBuf,
        #[arg(long)] out: Option<PathBuf>,
    },
    /// Debug : apériodicité / flatness / rms à des instants donnés.
    Probe {
        #[arg(long)] stem: PathBuf,
        #[arg(long)] times: String,
    },
}

fn load_wav_mono(path: &PathBuf) -> (Vec<f64>, u32) {
    let mut r = hound::WavReader::open(path).expect("ouverture WAV");
    let spec = r.spec();
    let ch = spec.channels as usize;
    let samples: Vec<f64> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap() as f64).collect(),
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f64;
            r.samples::<i32>().map(|s| s.unwrap() as f64 / scale).collect()
        }
    };
    let mono: Vec<f64> = if ch > 1 {
        samples.chunks(ch).map(|c| c.iter().sum::<f64>() / ch as f64).collect()
    } else { samples };
    (mono, spec.sample_rate)
}

fn check_sr(sr: u32) {
    if sr != dsp::SR {
        eprintln!("ATTENTION: {} Hz, le modèle est calé pour {} Hz. Ré-échantillonne d'abord.", sr, dsp::SR);
    }
}

fn main() {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::BuildLib { stem, centers, lib, note, append } => {
            let (x, sr) = load_wav_mono(&stem); check_sr(sr);
            let src = stem.file_name().unwrap().to_string_lossy().to_string();
            let cs: Vec<f64> = centers.split(',').filter_map(|c| c.trim().parse().ok()).collect();
            let mut library = if append && lib.exists() {
                lib_store::Library::load(&lib).expect("chargement biblio")
            } else {
                lib_store::Library { version: 1, dim: 0, exemplars: vec![] }
            };
            let mut added = 0;
            for c in cs {
                match detect::make_exemplar(&x, c, &src, &note) {
                    Some(ex) => { library.dim = ex.vector.len(); library.exemplars.push(ex); added += 1; }
                    None => eprintln!("  centre {}s hors bornes, ignoré", c),
                }
            }
            if append { library.version += 1; }
            library.save(&lib).expect("écriture biblio");
            println!("biblio v{} : +{} exemplaires, total {} -> {}",
                     library.version, added, library.exemplars.len(), lib.display());
        }
        Cmd::Detect { stem, lib, thr, out } => {
            let (x, sr) = load_wav_mono(&stem); check_sr(sr);
            let library = lib_store::Library::load(&lib).expect("chargement biblio");
            let regions = detect::detect(&x, &library, thr);
            let json = serde_json::json!({"schema": 1, "regions": regions});
            let txt = serde_json::to_string_pretty(&json).unwrap();
            let path = out.unwrap_or_else(|| {
                let stem_name = stem.file_stem().unwrap().to_string_lossy();
                PathBuf::from(format!("{}_breath.json", stem_name))
            });
            std::fs::write(&path, &txt).expect("écriture JSON");
            println!("{} souffles (seuil {}) -> {}", regions.len(), thr, path.display());
            for rg in &regions {
                println!("  {:7.2}-{:7.2}s  c={:.2}  {}-{}Hz", rg.s, rg.e, rg.c, rg.lo, rg.hi);
            }
        }
        Cmd::Cue { stem, regions, out } => {
            let out = out.unwrap_or_else(|| {
                let s = stem.file_stem().unwrap().to_string_lossy();
                stem.with_file_name(format!("{}_cue.wav", s))
            });
            match cue::write_cue_wav(&stem, &out, &regions) {
                Ok(n) => println!("{} marqueurs écrits -> {}", n, out.display()),
                Err(e) => eprintln!("erreur cue: {e}"),
            }
        }
        Cmd::Probe { stem, times } => {
            let (x, sr) = load_wav_mono(&stem); check_sr(sr);
            let power = dsp::power_spectrogram(&x);
            let mp = dsp::melpower(&power);
            let centers = dsp::mel_centers();
            let flat = dsp::flatness(&mp, &centers);
            let n = mp[0].len();
            let aper = yin::aperiodicity(&x, n);
            let r = dsp::rms(&x, n);
            let fps = dsp::fps();
            println!("          temps   aper   flat    rms");
            for ts in times.split(',') {
                if let Ok(t) = ts.trim().parse::<f64>() {
                    let i = ((t * fps) as usize).min(n - 1);
                    println!("  {:9.2}  {:.2}   {:.2}   {:.4}", t, aper[i], flat[i], r[i]);
                }
            }
        }
    }
}
