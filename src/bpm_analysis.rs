//! Offline BPM analysis: bounded FFmpeg decoding, spectral-flux periodicity,
//! and agreement across three independent sections. No playback or network I/O.
use std::path::Path;
use std::process::{Command, Stdio};
use std::io::Read;
use std::time::{Duration, Instant};
use rustfft::{num_complex::Complex, FftPlanner};
use crate::media::{CustomTag, ScanReport};

const RATE: usize = 11025;
const FFT: usize = 1024;
const HOP: usize = 128;
const SECONDS: usize = 120;
const MAX_BYTES: usize = RATE * SECONDS * 4;

/// An existing finite positive BPM is authoritative, including custom BPM tags.
pub fn existing_bpm(tags: &[CustomTag]) -> Option<f64> {
    tags.iter().filter(|t| t.name.trim().eq_ignore_ascii_case("bpm"))
        .filter_map(|t| t.value.trim().parse::<f64>().ok())
        .find(|v| v.is_finite() && *v > 0.0)
}

/// Pure signal analysis, separated from decoding for reproducible tests.
/// This is a conservative periodicity heuristic, not a calibrated probability.
pub fn estimate(samples: &[f32]) -> Result<u32, String> {
    if samples.len() < RATE * 30 { return Err("less than 30 seconds of audio".into()); }
    if samples.iter().any(|v| !v.is_finite()) { return Err("invalid decoded samples".into()); }
    let rms = (samples.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / samples.len() as f64).sqrt();
    if rms < 0.00001 { return Err("silence or insufficient audio energy".into()); }
    let fft = FftPlanner::<f32>::new().plan_fft_forward(FFT);
    let window: Vec<f32> = (0..FFT).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / FFT as f32).cos()).collect();
    let mut previous = vec![0.0f32; FFT / 2];
    let mut buffer = vec![Complex::new(0.0, 0.0); FFT];
    let mut flux = Vec::new();
    for frame in samples.windows(FFT).step_by(HOP) {
        for i in 0..FFT { buffer[i] = Complex::new(frame[i] * window[i], 0.0); }
        fft.process(&mut buffer);
        let mut onset = 0.0;
        for k in 4..FFT / 2 {
            let magnitude = buffer[k].norm().ln_1p();
            onset += (magnitude - previous[k]).max(0.0);
            previous[k] = magnitude;
        }
        flux.push(onset as f64);
    }
    // Remove the local noise floor, then smooth one hop to tolerate beat jitter.
    let radius = RATE / HOP / 2;
    let onset: Vec<f64> = (0..flux.len()).map(|i| {
        let a = i.saturating_sub(radius);
        let b = (i + radius + 1).min(flux.len());
        (flux[i] - flux[a..b].iter().sum::<f64>() / (b-a) as f64).max(0.0)
    }).collect();
    let smooth: Vec<f64> = onset.windows(3).map(|w| (w[0] + 2.0*w[1] + w[2])/4.0).collect();
    let length = smooth.len() / 3;
    let mut estimates = Vec::new();
    for section in smooth.chunks_exact(length).take(3) {
        estimates.push(section_bpm(section)?);
    }
    estimates.sort_by(f64::total_cmp);
    let median = estimates[1];
    if estimates.iter().any(|v| (v-median).abs() / median > 0.04) {
        return Err(format!("sections disagree: {:.1}, {:.1}, {:.1} BPM", estimates[0], estimates[1], estimates[2]));
    }
    let bpm = median.round() as u32;
    if !(45..=240).contains(&bpm) { return Err("estimated BPM outside 45..240".into()); }
    Ok(bpm)
}

fn section_bpm(onset: &[f64]) -> Result<f64, String> {
    let hz = RATE as f64 / HOP as f64;
    let low = (60.0 * hz / 240.0).floor() as usize;
    let high = (60.0 * hz / 45.0).ceil() as usize;
    let peak = onset.iter().copied().fold(0.0, f64::max);
    if peak < 1e-6 { return Err("no rhythmic transients".into()); }
    let hits = onset.windows(3).filter(|w| w[1] > peak * 0.2 && w[1] >= w[0] && w[1] > w[2]).count();
    if hits < 8 { return Err("too few rhythmic transients".into()); }
    let mut scores = vec![0.0; high + 2];
    for lag in low.saturating_sub(1)..=high+1 {
        let mut cross = 0.0;
        let mut left = 0.0;
        let mut right = 0.0;
        for i in lag..onset.len() {
            cross += onset[i] * onset[i-lag];
            left += onset[i].powi(2);
            right += onset[i-lag].powi(2);
        }
        scores[lag] = cross / (left*right).sqrt().max(1e-12);
    }
    let peaks: Vec<usize> = (low..=high).filter(|&i| scores[i] >= scores[i-1] && scores[i] > scores[i+1]).collect();
    let best = peaks.iter().copied().max_by(|&a, &b| scores[a].total_cmp(&scores[b]))
        .ok_or("no stable rhythmic peak")?;
    if scores[best] < 0.40 { return Err(format!("weak rhythmic periodicity ({:.2})", scores[best])); }
    // Multiples of the fundamental beat also correlate. Prefer the shortest
    // equally strong period; do not claim this resolves perceptual half/double time.
    let chosen = peaks.iter().copied().filter(|&i| i <= best && scores[i] >= scores[best] * 0.90
        && ((best as f64 / i as f64) - (best as f64 / i as f64).round()).abs() < 0.08)
        .min().unwrap_or(best);
    for &i in &peaks {
        if i == chosen { continue; }
        let ratio = i.max(chosen) as f64 / i.min(chosen) as f64;
        if (ratio-ratio.round()).abs() > 0.10 && scores[i] > scores[chosen] * 0.95 {
            return Err("competing non-harmonic rhythms".into());
        }
    }
    // Quadratic interpolation reduces the quantization of short beat periods.
    let (a,b,c) = (scores[chosen-1], scores[chosen], scores[chosen+1]);
    let shift = if (a-2.0*b+c).abs() > 1e-9 { (0.5*(a-c)/(a-2.0*b+c)).clamp(-0.5,0.5) } else { 0.0 };
    Ok(60.0 * hz / (chosen as f64 + shift))
}

fn decode(full: &Path) -> Result<Vec<f32>, String> {
    let executable = std::env::var_os("STATIOND_FFMPEG").unwrap_or_else(|| "ffmpeg".into());
    let mut cmd = Command::new(executable);
    cmd.args(["-nostdin", "-hide_banner", "-loglevel", "error", "-threads", "1", "-i"])
        .arg(full).args(["-t", "120", "-map", "0:a:0", "-vn", "-sn", "-dn", "-ac", "1", "-ar", "11025", "-f", "f32le", "pipe:1"])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)] {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    let mut child = cmd.spawn().map_err(|e| format!("cannot start FFmpeg: {e}"))?;
    let stdout = child.stdout.take().ok_or("missing FFmpeg stdout")?;
    let stderr = child.stderr.take().ok_or("missing FFmpeg stderr")?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes).map(|_| bytes)
    });
    let errors = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.take(8192).read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).to_string()
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Ok(s),
            Ok(None) if started.elapsed() < Duration::from_secs(45) => std::thread::sleep(Duration::from_millis(25)),
            Ok(None) => { let _ = child.kill(); let _ = child.wait(); break Err("FFmpeg timed out after 45 seconds".to_string()); }
            Err(e) => { let _ = child.kill(); let _ = child.wait(); break Err(format!("FFmpeg wait: {e}")); }
        }
    };
    let bytes = reader.join().map_err(|_| "decoder reader panicked")?.map_err(|e| e.to_string())?;
    let error = errors.join().unwrap_or_default();
    if !status?.success() { return Err(format!("FFmpeg decoding failed: {error}")); }
    if bytes.len() > MAX_BYTES || bytes.len() % 4 != 0 { return Err("invalid or oversized decoded audio".into()); }
    Ok(bytes.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect())
}

/// One file at a time, off the async runtime. Preserve valid source BPM and
/// leave an uncertain file playable; the warning names it and explains why.
pub fn analyze_missing(root: &Path, report: &mut ScanReport) {
    for m in &report.media {
        if !Path::new(&m.rel_path).extension().is_some_and(|e| e.eq_ignore_ascii_case("mp3")) { continue; }
        let tags = report.custom_tags.entry(m.rel_path.clone()).or_default();
        if existing_bpm(tags).is_some() { continue; }
        let result = crate::media_tags::resolve(root, &m.rel_path).map_err(|e| e.to_string())
            .and_then(|p| p.canonicalize().map_err(|e| e.to_string()))
            .and_then(|p| {
                let base = root.canonicalize().map_err(|e| e.to_string())?;
                if !p.starts_with(base) { return Err("audio resolves outside media root".into()); }
                decode(&p)
            }).and_then(|samples| estimate(&samples));
        match result {
            Ok(bpm) => {
                // Keep classification and the integer TBPM value consistent.
                let value = bpm.to_string();
                tags.push(CustomTag { name: "BPM".into(), value: value.clone() });
                report.metadata.entry(m.rel_path.clone()).or_default().insert("bpm".into(), value);
                tracing::info!(media = %m.rel_path, bpm, "estimated missing BPM from audio");
            }
            Err(reason) => tracing::warn!(media = %m.rel_path, %reason, "BPM not estimated; no BPM written"),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    fn clicks(bpm: f64, seconds: usize) -> Vec<f32> {
        let mut audio = vec![0.0; RATE*seconds];
        let step = 60.0*RATE as f64/bpm;
        let mut tick = RATE as f64;
        while (tick as usize)+400 < audio.len() {
            for j in 0..400 {
                audio[tick as usize+j] += (j as f32 * 0.37).sin() * (-(j as f32)/80.0).exp() * 0.8;
            }
            tick += step;
        }
        audio
    }
    #[test]
    fn estimates_regular_rhythms() {
        for bpm in [60, 75, 90, 100, 120, 140, 160, 180, 200] {
            let actual = estimate(&clicks(bpm as f64, 60)).unwrap();
            assert!((actual as i32-bpm).abs() <= 2, "expected {bpm}, got {actual}");
        }
    }
    #[test]
    fn rejects_silence_short_audio_and_tempo_changes() {
        assert!(estimate(&vec![0.0; RATE*60]).is_err());
        assert!(estimate(&clicks(120.0, 10)).is_err());
        let mut audio = clicks(90.0, 20);
        audio.extend(clicks(130.0, 20));
        audio.extend(clicks(170.0, 20));
        assert!(estimate(&audio).is_err());
    }
    #[test]
    fn existing_bpm_is_preserved_and_bad_values_are_ignored() {
        let tags = |values: &[&str]| values.iter().map(|s| CustomTag { name: "bpm".into(), value: s.to_string() }).collect::<Vec<_>>();
        assert_eq!(existing_bpm(&tags(&["bad", "0", "NaN", "inf", "125.5"])), Some(125.5));
        assert_eq!(existing_bpm(&tags(&["-12", "0", "NaN"])), None);
    }

    pub(crate) fn make_mp3(root: &Path) -> std::path::PathBuf {
        use lofty::{config::WriteOptions, id3::v2::Id3v2Tag, tag::TagExt};
        let raw = root.join("clicks.f32");
        let mp3 = root.join("rhythm.mp3");
        let bytes: Vec<u8> = clicks(140.0, 60).iter().flat_map(|v| v.to_le_bytes()).collect();
        std::fs::write(&raw, bytes).unwrap();
        let executable = std::env::var_os("STATIOND_FFMPEG").unwrap_or_else(|| "ffmpeg".into());
        let result = Command::new(executable).args(["-nostdin", "-v", "error", "-f", "f32le", "-ar", "11025", "-ac", "1", "-i"])
            .arg(&raw).args(["-c:a", "libmp3lame", "-ar", "44100", "-b:a", "128k"])
            .arg(&mp3).output().unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        let mut tag = Id3v2Tag::default();
        tag.insert_user_text("type".into(), "song".into());
        tag.insert_user_text("comment".into(), "made with suno; created=2026-06-14T06:36:48Z; id=test".into());
        tag.save_to_path(&mp3, WriteOptions::default()).unwrap();
        mp3
    }

    #[test]
    #[ignore = "requires FFmpeg (PATH or STATIOND_FFMPEG)"]
    fn real_mp3_analysis_writeback_and_rescan() {
        let dir = tempfile::tempdir().unwrap();
        let mp3 = make_mp3(dir.path());
        let mut report = crate::media::scan_library(dir.path()).unwrap();
        analyze_missing(dir.path(), &mut report);
        let bpm = existing_bpm(&report.custom_tags["rhythm.mp3"]).unwrap();
        assert!((bpm - 140.0).abs() <= 2.0, "got {bpm}");
        report.metadata.get_mut("rhythm.mp3").unwrap().insert("tempo".into(), "fast".into());
        assert_eq!(crate::scan_writeback::apply(dir.path(), &mut report).unwrap().len(), 1);
        let before = std::fs::read(&mp3).unwrap();
        let mut rescanned = crate::media::scan_library(dir.path()).unwrap();
        assert_eq!(existing_bpm(&rescanned.custom_tags["rhythm.mp3"]), Some(bpm));
        assert!(rescanned.custom_tags["rhythm.mp3"].iter().any(|t| t.name == "tempo" && t.value == "fast"));
        analyze_missing(dir.path(), &mut rescanned);
        // No fresh estimate on a second scan: the written TBPM is authoritative.
        assert!(rescanned.metadata.is_empty());
        assert!(crate::scan_writeback::apply(dir.path(), &mut rescanned).unwrap().is_empty());
        assert_eq!(std::fs::read(&mp3).unwrap(), before);
    }
}
