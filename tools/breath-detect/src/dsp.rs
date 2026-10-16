//! STFT, banc de filtres mel (Slaney, comme librosa), log-mel, flatness, RMS.
//! Paramètres figés par la spec v1.

use rustfft::{num_complex::Complex, FftPlanner};

pub const SR: u32 = 48000;
pub const N_FFT: usize = 2048;
pub const HOP: usize = 256;
pub const N_MELS: usize = 64;
pub const FMIN: f64 = 200.0;
pub const FMAX: f64 = 12000.0;
pub const WIN_S: f64 = 0.20;
pub const MATCH_FLO: f64 = 2000.0; // bande de matching
pub const FLAT_LO: f64 = 1500.0;   // bande flatness (garde-fou)
pub const FLAT_HI: f64 = 9000.0;

pub fn fps() -> f64 { SR as f64 / HOP as f64 }
pub fn win_frames() -> usize { (WIN_S * fps()) as usize } // = 37

// ── échelle mel Slaney (htk=False dans librosa) ─────────────────────────────
fn hz_to_mel(f: f64) -> f64 {
    let f_sp = 200.0 / 3.0;
    let min_log_hz = 1000.0;
    let min_log_mel = min_log_hz / f_sp;
    let logstep = (6.4f64).ln() / 27.0;
    if f >= min_log_hz { min_log_mel + (f / min_log_hz).ln() / logstep } else { f / f_sp }
}
fn mel_to_hz(m: f64) -> f64 {
    let f_sp = 200.0 / 3.0;
    let min_log_hz = 1000.0;
    let min_log_mel = min_log_hz / f_sp;
    let logstep = (6.4f64).ln() / 27.0;
    if m >= min_log_mel { min_log_hz * (logstep * (m - min_log_mel)).exp() } else { m * f_sp }
}

/// Fréquences centrales des bins mel (pour les masques de bande).
pub fn mel_centers() -> Vec<f64> {
    let m_lo = hz_to_mel(FMIN);
    let m_hi = hz_to_mel(FMAX);
    (0..N_MELS)
        .map(|i| {
            // points 1..=N_MELS parmi N_MELS+2 points équirépartis en mel
            let m = m_lo + (m_hi - m_lo) * (i as f64 + 1.0) / (N_MELS as f64 + 1.0);
            mel_to_hz(m)
        })
        .collect()
}

/// Banc de filtres mel Slaney normalisé, creux : (bin_départ, poids non nuls).
fn mel_filterbank_sparse() -> Vec<(usize, Vec<f64>)> {
    let n_bins = N_FFT / 2 + 1;
    let fft_freqs: Vec<f64> = (0..n_bins).map(|k| k as f64 * SR as f64 / N_FFT as f64).collect();
    let m_lo = hz_to_mel(FMIN);
    let m_hi = hz_to_mel(FMAX);
    let pts: Vec<f64> = (0..N_MELS + 2)
        .map(|i| mel_to_hz(m_lo + (m_hi - m_lo) * i as f64 / (N_MELS as f64 + 1.0)))
        .collect();
    let mut fb = Vec::with_capacity(N_MELS);
    for m in 0..N_MELS {
        let (lo, ctr, hi) = (pts[m], pts[m + 1], pts[m + 2]);
        let enorm = 2.0 / (hi - lo);
        let mut start = None;
        let mut weights = Vec::new();
        for (k, &f) in fft_freqs.iter().enumerate() {
            let w = ((f - lo) / (ctr - lo)).min((hi - f) / (hi - ctr)).max(0.0) * enorm;
            if w > 0.0 {
                if start.is_none() { start = Some(k); }
                weights.push(w);
            } else if start.is_some() && !weights.is_empty() {
                break; // filtre triangulaire : contigu
            }
        }
        fb.push((start.unwrap_or(0), weights));
    }
    fb
}

/// mel-puissance (linéaire), banc creux. (n_mels x n_frames)
pub fn melpower(power: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let fb = mel_filterbank_sparse();
    let n_frames = power[0].len();
    let mut out = vec![vec![0.0f64; n_frames]; N_MELS];
    for m in 0..N_MELS {
        let (start, w) = &fb[m];
        for t in 0..n_frames {
            let mut s = 0.0;
            for (j, &wj) in w.iter().enumerate() { s += wj * power[start + j][t]; }
            out[m][t] = s;
        }
    }
    out
}

/// log-mel depuis la mel-puissance (évite un 2e passage du banc).
pub fn logmel_from(mp: &[Vec<f64>]) -> Vec<Vec<f64>> {
    mp.iter().map(|row| row.iter().map(|&v| 10.0 * (v + 1e-10).log10()).collect()).collect()
}

fn hann(n: usize) -> Vec<f64> {
    (0..n).map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()).collect()
}

/// Spectrogramme de puissance |STFT|^2, center=True, reflect pad. (n_bins x n_frames)
pub fn power_spectrogram(x: &[f64]) -> Vec<Vec<f64>> {
    let pad = N_FFT / 2;
    let mut sig = Vec::with_capacity(x.len() + 2 * pad);
    for i in 0..pad { sig.push(x[(pad - i).min(x.len() - 1)]); }      // reflect gauche
    sig.extend_from_slice(x);
    for i in 0..pad { sig.push(x[x.len().saturating_sub(2 + i)]); }   // reflect droite
    let n_frames = 1 + (sig.len() - N_FFT) / HOP;
    let win = hann(N_FFT);
    let mut planner = FftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(N_FFT);
    let n_bins = N_FFT / 2 + 1;
    let mut out = vec![vec![0.0f64; n_frames]; n_bins];
    let mut buf = vec![Complex { re: 0.0, im: 0.0 }; N_FFT];
    for t in 0..n_frames {
        let start = t * HOP;
        for i in 0..N_FFT {
            buf[i] = Complex { re: sig[start + i] * win[i], im: 0.0 };
        }
        fft.process(&mut buf);
        for k in 0..n_bins {
            out[k][t] = buf[k].norm_sqr();
        }
    }
    out
}

/// Log-mel direct depuis le spectre de puissance (mel-puissance -> dB).
pub fn logmel(power: &[Vec<f64>]) -> Vec<Vec<f64>> {
    logmel_from(&melpower(power))
}

/// Flatness (geo/arith) bande 1.5-9k, par trame.
pub fn flatness(mp: &[Vec<f64>], centers: &[f64]) -> Vec<f64> {
    let band: Vec<usize> = (0..N_MELS).filter(|&m| centers[m] >= FLAT_LO && centers[m] <= FLAT_HI).collect();
    let n_frames = mp[0].len();
    (0..n_frames).map(|t| {
        let mut log_sum = 0.0; let mut arith = 0.0;
        for &m in &band { let v = mp[m][t] + 1e-10; log_sum += v.ln(); arith += v; }
        let n = band.len() as f64;
        let geo = (log_sum / n).exp();
        geo / (arith / n)
    }).collect()
}

/// RMS par trame (fenêtre n_fft, hop), aligné sur le spectrogramme centré.
pub fn rms(x: &[f64], n_frames: usize) -> Vec<f64> {
    let pad = N_FFT / 2;
    let mut sig = vec![0.0f64; x.len() + 2 * pad];
    sig[pad..pad + x.len()].copy_from_slice(x);
    (0..n_frames).map(|t| {
        let start = t * HOP;
        let mut s = 0.0;
        for i in 0..N_FFT { let v = sig[start + i]; s += v * v; }
        (s / N_FFT as f64).sqrt()
    }).collect()
}

pub fn percentile(v: &[f64], p: f64) -> f64 {
    let mut s: Vec<f64> = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = ((p / 100.0) * (s.len() as f64 - 1.0)).round() as usize;
    s[idx.min(s.len() - 1)]
}
