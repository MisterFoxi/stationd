//! Extraction de features, construction de biblio, et détection.

use crate::dsp::*;
use crate::lib_store::{Exemplar, Library};
use crate::yin;

pub const APER_THR: f64 = 0.50; // apériodicité mini pour "non-voisé" (calé §task3)
pub const FLAT_THR: f64 = 0.40;
pub const RMS_FLOOR_FRAC: f64 = 0.12;

/// Indices des bins mel gardés pour le matching (>= MATCH_FLO).
pub fn band_indices(centers: &[f64]) -> Vec<usize> {
    (0..N_MELS).filter(|&m| centers[m] >= MATCH_FLO).collect()
}

/// Vecteur normalisé (bande x W) centré sur la trame `i`. None si hors bornes.
pub fn patch_vector(logmel: &[Vec<f64>], band: &[usize], i: usize) -> Option<Vec<f64>> {
    let w = win_frames();
    let h = w / 2;
    if i < h || i + (w - h) > logmel[0].len() { return None; }
    let a = i - h;
    let mut v: Vec<f64> = Vec::with_capacity(band.len() * w);
    for &m in band {
        for t in a..a + w { v.push(logmel[m][t]); }
    }
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    for x in v.iter_mut() { *x -= mean; }
    let norm = v.iter().map(|x| x * x).sum::<f64>().sqrt();
    if norm < 1e-9 { return None; }
    for x in v.iter_mut() { *x /= norm; }
    Some(v)
}

pub fn build_patch_at_time(x: &[f64], center_s: f64) -> Option<Vec<f64>> {
    let power = power_spectrogram(x);
    let lm = logmel(&power);
    let centers = mel_centers();
    let band = band_indices(&centers);
    let i = (center_s * fps()).round() as usize;
    patch_vector(&lm, &band, i)
}

#[derive(serde::Serialize)]
pub struct Region {
    pub s: f64,
    pub e: f64,
    pub lo: u32,
    pub hi: u32,
    pub c: f64,
}

pub fn detect(x: &[f64], lib: &Library, thr: f64) -> Vec<Region> {
    let power = power_spectrogram(x);
    let mp = melpower(&power);
    let lm = logmel_from(&mp);
    let centers = mel_centers();
    let band = band_indices(&centers);
    let n = lm[0].len();
    let w = win_frames();
    let h = w / 2;

    // score = max cosine sur la biblio
    let mut score = vec![f64::NAN; n];
    for i in h..n.saturating_sub(w - h) {
        if let Some(v) = patch_vector(&lm, &band, i) {
            let mut best = -2.0;
            for ex in lib.vectors() {
                let mut dot = 0.0;
                for k in 0..v.len().min(ex.vector.len()) { dot += v[k] * ex.vector[k]; }
                if dot > best { best = dot; }
            }
            score[i] = best;
        }
    }

    // garde-fou
    let flat = flatness(&mp, &centers);
    let aper = yin::aperiodicity(x, n);
    let r = rms(x, n);
    let floor = percentile(&r, 90.0) * RMS_FLOOR_FRAC;

    let hot: Vec<bool> = (0..n).map(|i| {
        let sc = score[i];
        !sc.is_nan() && sc >= thr && aper[i] > APER_THR && flat[i] > FLAT_THR && r[i] > floor
    }).collect();

    // régions : fusion trous < 0.1s, durée mini 0.08s
    let t = |i: usize| i as f64 * HOP as f64 / SR as f64;
    let mut raw: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        if hot[i] {
            let mut j = i;
            while j < n && hot[j] { j += 1; }
            raw.push((i, j - 1));
            i = j;
        } else { i += 1; }
    }
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (a, b) in raw {
        if let Some(last) = merged.last_mut() {
            if t(a) - t(last.1) < 0.10 { last.1 = b; continue; }
        }
        merged.push((a, b));
    }

    let mut out = Vec::new();
    for (a, b) in merged {
        if t(b) - t(a) < 0.08 { continue; }
        let mut pk = a; let mut pv = f64::MIN;
        for k in a..=b { if !score[k].is_nan() && score[k] > pv { pv = score[k]; pk = k; } }
        let (lo, hi) = band_of(&power, t(a), t(b), n);
        out.push(Region { s: round3(t(a)), e: round3(t(b)), lo, hi, c: round3(score[pk]) });
    }
    out
}

fn band_of(power: &[Vec<f64>], t0: f64, t1: f64, n: usize) -> (u32, u32) {
    let fps = fps();
    let a = (t0 * fps) as usize;
    let b = (t1 * fps).min(n as f64 - 1.0) as usize;
    let na = a.saturating_sub((0.4 * fps) as usize);
    let nb = (b + (0.4 * fps) as usize).min(n - 1);
    let n_bins = power.len();
    let freq = |k: usize| k as f64 * SR as f64 / N_FFT as f64;
    let mut lo = 1500u32; let mut hi = 11000u32; let mut got = false;
    let mut first = None; let mut last = None;
    for k in 0..n_bins {
        let f = freq(k);
        if f < 500.0 || f > 12000.0 { continue; }
        let reg: f64 = (a..=b).map(|t| power[k][t]).sum::<f64>() / (b - a + 1) as f64;
        let bcnt = (na..a).count() + (b + 1..=nb).count();
        if bcnt == 0 { continue; }
        let base: f64 = ((na..a).map(|t| power[k][t]).sum::<f64>()
            + (b + 1..=nb).map(|t| power[k][t]).sum::<f64>()) / bcnt as f64;
        let excess = 10.0 * ((reg + 1e-10) / (base + 1e-10)).log10();
        if excess >= 2.0 {
            if first.is_none() { first = Some(f); }
            last = Some(f);
            got = true;
        }
    }
    if got { lo = first.unwrap().max(500.0) as u32; hi = last.unwrap().min(12000.0) as u32; }
    (lo, hi)
}

fn round3(x: f64) -> f64 { (x * 1000.0).round() / 1000.0 }

/// Construit un exemplaire (pour build-lib / enrich).
pub fn make_exemplar(x: &[f64], center_s: f64, source: &str, note: &str) -> Option<Exemplar> {
    build_patch_at_time(x, center_s).map(|v| Exemplar {
        source: source.to_string(),
        center_s,
        note: note.to_string(),
        vector: v,
    })
}
