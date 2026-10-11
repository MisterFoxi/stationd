//! Voisement par YIN (remplace pYIN du proto). On ne veut qu'une apériodicité
//! grossière pour le garde-fou (souffle = non-voisé = apériodique).
//! Calculé à un hop interne large (vitesse), puis ré-échantillonné.

use crate::dsp::{N_FFT, SR};

const YIN_HOP: usize = 1024;
const W_INT: usize = 512;       // fenêtre d'intégration (voisement grossier suffit)
const FMIN_PITCH: f64 = 80.0;
const FMAX_PITCH: f64 = 1000.0;
const THRESH: f64 = 0.10;

/// Apériodicité par trame (0 = périodique/voisé, ~1 = bruité/non-voisé),
/// ré-échantillonnée sur `n_frames` (grille du spectrogramme, hop=HOP).
pub fn aperiodicity(x: &[f64], n_frames: usize) -> Vec<f64> {
    let pad = N_FFT / 2;
    let mut sig = vec![0.0f64; x.len() + 2 * pad];
    sig[pad..pad + x.len()].copy_from_slice(x);

    let frame_len = W_INT + (SR as f64 / FMIN_PITCH) as usize + 1; // doit contenir tau_max
    let tau_min = (SR as f64 / FMAX_PITCH) as usize;               // 48
    let tau_max = (SR as f64 / FMIN_PITCH) as usize;               // 600

    let n_yin = if sig.len() > frame_len { 1 + (sig.len() - frame_len) / YIN_HOP } else { 1 };
    let mut aper = vec![1.0f64; n_yin];
    let mut d = vec![0.0f64; tau_max + 1];
    let mut dp = vec![0.0f64; tau_max + 1];

    for t in 0..n_yin {
        let s = t * YIN_HOP;
        if s + frame_len > sig.len() { aper[t] = 1.0; continue; }
        // fonction de différence
        for tau in 1..=tau_max {
            let mut sum = 0.0;
            for j in 0..W_INT {
                let diff = sig[s + j] - sig[s + j + tau];
                sum += diff * diff;
            }
            d[tau] = sum;
        }
        // CMND
        dp[0] = 1.0;
        let mut run = 0.0;
        for tau in 1..=tau_max {
            run += d[tau];
            dp[tau] = if run > 0.0 { d[tau] * tau as f64 / run } else { 1.0 };
        }
        // premier creux sous le seuil, sinon minimum global
        let mut chosen = tau_min;
        let mut best = dp[tau_min];
        let mut found = false;
        for tau in tau_min..=tau_max {
            if dp[tau] < THRESH {
                // descendre au minimum local
                let mut tt = tau;
                while tt + 1 <= tau_max && dp[tt + 1] < dp[tt] { tt += 1; }
                chosen = tt; found = true; break;
            }
            if dp[tau] < best { best = dp[tau]; chosen = tau; }
        }
        let _ = found;
        aper[t] = dp[chosen].clamp(0.0, 1.5);
    }

    // ré-échantillonnage linéaire sur la grille cible
    resample(&aper, n_frames)
}

fn resample(src: &[f64], n: usize) -> Vec<f64> {
    if src.len() == 1 { return vec![src[0]; n]; }
    (0..n).map(|i| {
        let pos = i as f64 * (src.len() - 1) as f64 / (n as f64 - 1.0).max(1.0);
        let a = pos.floor() as usize;
        let b = (a + 1).min(src.len() - 1);
        let f = pos - a as f64;
        src[a] * (1.0 - f) + src[b] * f
    }).collect()
}
