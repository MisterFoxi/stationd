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

/// The tempo window estimates are folded into by default (half / double
/// time): a steady 60 BPM reads as 120, a 245 as 122.5.
pub const DEFAULT_RANGE: (f64, f64) = (70.0, 180.0);

/// Why a file got no BPM, by kind (counted in the scan's tally) with the
/// detail for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FailKind {
    /// FFmpeg could not decode it (missing, failed, timed out) or the path is wrong.
    Decode,
    /// Under 30 s of audio.
    TooShort,
    /// Silence, invalid samples, no transients.
    NoRhythm,
    /// A periodicity too weak to trust.
    Weak,
    /// Two rhythms that are not multiples of one another.
    Competing,
    /// The three sections do not agree, even folded into the window.
    Disagree,
}

impl FailKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FailKind::Decode => "decode",
            FailKind::TooShort => "too_short",
            FailKind::NoRhythm => "no_rhythm",
            FailKind::Weak => "weak",
            FailKind::Competing => "competing",
            FailKind::Disagree => "disagree",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub kind: FailKind,
    pub detail: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

fn fail<T>(kind: FailKind, detail: impl Into<String>) -> Result<T, Failure> {
    Err(Failure { kind, detail: detail.into() })
}

/// Bring `bpm` into `[lo, hi)` by doubling / halving (`hi` ≥ 2 × `lo`, so
/// every tempo has exactly one place there).
pub fn fold(mut bpm: f64, (lo, hi): (f64, f64)) -> f64 {
    while bpm < lo {
        bpm *= 2.0;
    }
    while bpm >= hi {
        bpm /= 2.0;
    }
    bpm
}

/// A folding window must hold a whole octave.
pub fn check_range((lo, hi): (f64, f64)) -> Result<(), String> {
    if !(lo.is_finite() && hi.is_finite() && lo >= 20.0 && hi <= 400.0) {
        return Err(format!("tempo.analyze_range [{lo}, {hi}]: bounds must lie within 20..400 BPM"));
    }
    if hi < 2.0 * lo {
        return Err(format!("tempo.analyze_range [{lo}, {hi}]: the upper bound must be at least twice the lower one (a whole octave)"));
    }
    Ok(())
}

/// [`estimate_in`] with [`DEFAULT_RANGE`].
pub fn estimate(samples: &[f32]) -> Result<u32, Failure> {
    estimate_in(samples, DEFAULT_RANGE)
}

/// Pure signal analysis, separated from decoding for reproducible tests.
/// This is a conservative periodicity heuristic, not a calibrated probability.
/// Each of three sections gives a period; octave errors between them (one
/// read at double or half time) are expected, so each is folded into
/// `range` before they must agree.
pub fn estimate_in(samples: &[f32], range: (f64, f64)) -> Result<u32, Failure> {
    if samples.len() < RATE * 30 { return fail(FailKind::TooShort, "less than 30 seconds of audio"); }
    if samples.iter().any(|v| !v.is_finite()) { return fail(FailKind::NoRhythm, "invalid decoded samples"); }
    let rms = (samples.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / samples.len() as f64).sqrt();
    if rms < 0.00001 { return fail(FailKind::NoRhythm, "silence or insufficient audio energy"); }
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
    let mut raw = Vec::new();
    for section in smooth.chunks_exact(length).take(3) {
        raw.push(section_bpm(section)?);
    }
    // Align each section on the first one, an octave up or down if that is
    // closer (the same tempo read at half / double time), then all must
    // agree within 4 %; the agreed tempo is folded into the window.
    let reference = fold(raw[0], range);
    let mut aligned: Vec<f64> = raw
        .iter()
        .map(|v| {
            let v = fold(*v, range);
            [v / 2.0, v, v * 2.0]
                .into_iter()
                .min_by(|a, b| (a - reference).abs().total_cmp(&(b - reference).abs()))
                .unwrap_or(v)
        })
        .collect();
    aligned.sort_by(f64::total_cmp);
    let median = aligned[1];
    if aligned.iter().any(|v| (v - median).abs() / median > 0.04) {
        return fail(
            FailKind::Disagree,
            format!("sections disagree: {:.1}, {:.1}, {:.1} BPM", raw[0], raw[1], raw[2]),
        );
    }
    // Rounded first, so a tempo just under the upper bound does not round onto it.
    Ok(fold(median.round(), range).round() as u32)
}

fn section_bpm(onset: &[f64]) -> Result<f64, Failure> {
    let hz = RATE as f64 / HOP as f64;
    let low = (60.0 * hz / 240.0).floor() as usize;
    let high = (60.0 * hz / 45.0).ceil() as usize;
    let peak = onset.iter().copied().fold(0.0, f64::max);
    if peak < 1e-6 { return fail(FailKind::NoRhythm, "no rhythmic transients"); }
    let hits = onset.windows(3).filter(|w| w[1] > peak * 0.2 && w[1] >= w[0] && w[1] > w[2]).count();
    if hits < 8 { return fail(FailKind::NoRhythm, "too few rhythmic transients"); }
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
    let Some(best) = peaks.iter().copied().max_by(|&a, &b| scores[a].total_cmp(&scores[b])) else {
        return fail(FailKind::NoRhythm, "no stable rhythmic peak");
    };
    if scores[best] < 0.40 { return fail(FailKind::Weak, format!("weak rhythmic periodicity ({:.2})", scores[best])); }
    // Multiples of the fundamental beat also correlate. Prefer the shortest
    // equally strong period; do not claim this resolves perceptual half/double time.
    let chosen = peaks.iter().copied().filter(|&i| i <= best && scores[i] >= scores[best] * 0.90
        && ((best as f64 / i as f64) - (best as f64 / i as f64).round()).abs() < 0.08)
        .min().unwrap_or(best);
    for &i in &peaks {
        if i == chosen { continue; }
        let ratio = i.max(chosen) as f64 / i.min(chosen) as f64;
        if (ratio-ratio.round()).abs() > 0.10 && scores[i] > scores[chosen] * 0.95 {
            return fail(FailKind::Competing, "competing non-harmonic rhythms");
        }
    }
    // Quadratic interpolation reduces the quantization of short beat periods.
    let (a,b,c) = (scores[chosen-1], scores[chosen], scores[chosen+1]);
    let shift = if (a-2.0*b+c).abs() > 1e-9 { (0.5*(a-c)/(a-2.0*b+c)).clamp(-0.5,0.5) } else { 0.0 };
    Ok(60.0 * hz / (chosen as f64 + shift))
}

/// The FFmpeg program: `STATIOND_FFMPEG`, else `ffmpeg` in the PATH.
fn ffmpeg() -> std::ffi::OsString {
    std::env::var_os("STATIOND_FFMPEG").unwrap_or_else(|| "ffmpeg".into())
}

/// Can FFmpeg be started? `Err` says which program and why not, with the
/// remedy. Checked before an analysis, so a missing FFmpeg is said once
/// rather than once per file.
pub fn ffmpeg_available() -> Result<(), String> {
    ffmpeg_runs(&ffmpeg())
}

fn ffmpeg_runs(exe: &std::ffi::OsStr) -> Result<(), String> {
    let out = Command::new(exe)
        .args(["-hide_banner", "-version"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match out {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("`{}` -version failed ({s})", exe.to_string_lossy())),
        Err(e) => Err(format!(
            "cannot start `{}` ({e}): install FFmpeg (the images from docker/ ship it: rebuild) or set STATIOND_FFMPEG",
            exe.to_string_lossy()
        )),
    }
}

fn decode(full: &Path) -> Result<Vec<f32>, String> {
    let mut cmd = Command::new(ffmpeg());
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
    // Both pipes are read to their end, what is beyond the kept part
    // discarded: a pipe left full would block FFmpeg until the timeout (a
    // damaged file can print many errors).
    let reader = std::thread::spawn(move || {
        let mut stdout = stdout;
        let mut bytes = Vec::new();
        (&mut stdout).take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes)?;
        std::io::copy(&mut stdout, &mut std::io::sink())?;
        Ok::<_, std::io::Error>(bytes)
    });
    let errors = std::thread::spawn(move || {
        let mut stderr = stderr;
        let mut bytes = Vec::new();
        let _ = (&mut stderr).take(8192).read_to_end(&mut bytes);
        let _ = std::io::copy(&mut stderr, &mut std::io::sink());
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

/// What an analysis did: BPMs written, files left without one (and why).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tally {
    pub estimated: usize,
    pub failed: Vec<(String, Failure)>,
}

impl Tally {
    /// Failures per kind, in a stable order.
    pub fn by_kind(&self) -> Vec<(FailKind, usize)> {
        let mut out: std::collections::BTreeMap<FailKind, usize> = Default::default();
        for (_, f) in &self.failed {
            *out.entry(f.kind).or_default() += 1;
        }
        out.into_iter().collect()
    }
}

/// The mp3 files of the report without a BPM: the ones to analyse.
pub fn missing(report: &ScanReport) -> Vec<String> {
    report
        .media
        .iter()
        .filter(|m| Path::new(&m.rel_path).extension().is_some_and(|e| e.eq_ignore_ascii_case("mp3")))
        .filter(|m| report.custom_tags.get(&m.rel_path).is_none_or(|t| existing_bpm(t).is_none()))
        .map(|m| m.rel_path.clone())
        .collect()
}

/// [`analyze_missing_in`] with [`DEFAULT_RANGE`] and no progress.
pub fn analyze_missing(root: &Path, report: &mut ScanReport) -> Tally {
    analyze_missing_in(root, report, DEFAULT_RANGE, &mut |_, _| {})
}

/// One file at a time, off the async runtime. Preserve valid source BPM and
/// leave an uncertain file playable; each file is logged, the tally says
/// how it went. `progress(done, total)` after each file.
pub fn analyze_missing_in(
    root: &Path,
    report: &mut ScanReport,
    range: (f64, f64),
    progress: &mut dyn FnMut(usize, usize),
) -> Tally {
    let todo = missing(report);
    let total = todo.len();
    let mut tally = Tally::default();
    progress(0, total);
    for (i, rel_path) in todo.into_iter().enumerate() {
        let result = crate::media_tags::resolve(root, &rel_path)
            .map_err(|e| Failure { kind: FailKind::Decode, detail: e.to_string() })
            .and_then(|p| p.canonicalize().map_err(|e| Failure { kind: FailKind::Decode, detail: e.to_string() }))
            .and_then(|p| {
                let base = root.canonicalize().map_err(|e| Failure { kind: FailKind::Decode, detail: e.to_string() })?;
                if !p.starts_with(base) {
                    return fail(FailKind::Decode, "audio resolves outside media root");
                }
                decode(&p).map_err(|detail| Failure { kind: FailKind::Decode, detail })
            })
            .and_then(|samples| estimate_in(&samples, range));
        match result {
            Ok(bpm) => {
                // Keep classification and the integer TBPM value consistent.
                let value = bpm.to_string();
                report.custom_tags.entry(rel_path.clone()).or_default().push(CustomTag { name: "BPM".into(), value: value.clone() });
                report.metadata.entry(rel_path.clone()).or_default().insert("bpm".into(), value);
                tracing::info!(media = %rel_path, bpm, "estimated missing BPM from audio");
                tally.estimated += 1;
            }
            Err(f) => {
                // One line per file in the log; the journal gets the tally.
                tracing::warn!(event = "bpm_not_estimated", media = %rel_path, reason = %f, "BPM not estimated; no BPM written");
                tally.failed.push((rel_path, f));
            }
        }
        progress(i + 1, total);
    }
    tally
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
        // Read in the default window (70..180): 60 → 120, 180 and 200 → 90, 100.
        for bpm in [60, 75, 90, 100, 120, 140, 160, 180, 200] {
            let actual = estimate(&clicks(bpm as f64, 60)).unwrap();
            let want = fold(bpm as f64, DEFAULT_RANGE) as i32;
            assert!((actual as i32-want).abs() <= 2, "{bpm}: expected {want}, got {actual}");
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
    fn octave_errors_between_sections_are_folded_into_the_window() {
        // Half / double time are the same tempo once folded.
        assert_eq!(fold(245.8, DEFAULT_RANGE).round(), 123.0);
        assert_eq!(fold(60.5, DEFAULT_RANGE).round(), 121.0);
        assert_eq!(fold(122.9, DEFAULT_RANGE).round(), 123.0);
        assert_eq!(fold(179.9, DEFAULT_RANGE).round(), 180.0);
        assert_eq!(fold(180.0, DEFAULT_RANGE), 90.0);
        // A steady 60 BPM reads 120 with the default window, 60 with one that holds it.
        let slow = clicks(60.0, 60);
        assert!((estimate(&slow).unwrap() as i32 - 120).abs() <= 2);
        assert!((estimate_in(&slow, (50.0, 100.0)).unwrap() as i32 - 60).abs() <= 2);
        // The window must hold a whole octave.
        assert!(check_range((70.0, 180.0)).is_ok());
        assert!(check_range((70.0, 120.0)).is_err());
        assert!(check_range((5.0, 180.0)).is_err());
    }

    #[test]
    fn a_missing_ffmpeg_is_said_with_its_remedy() {
        let why = ffmpeg_runs("/nonexistent/ffmpeg-for-test".as_ref()).unwrap_err();
        assert!(why.contains("/nonexistent/ffmpeg-for-test") && why.contains("STATIOND_FFMPEG"), "{why}");
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
    fn a_damaged_file_that_floods_stderr_fails_fast() {
        // Tens of KB of errors: more than a pipe holds. Read to the end, the
        // decoder never blocks until the 45 s timeout.
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.mp3");
        let mut x: u32 = 12345;
        let junk: Vec<u8> = (0..3_000_000).map(|_| { x ^= x << 13; x ^= x >> 17; x ^= x << 5; x as u8 }).collect();
        std::fs::write(&bad, junk).unwrap();
        let started = Instant::now();
        let _ = decode(&bad);
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    }

    #[test]
    #[ignore = "requires FFmpeg (PATH or STATIOND_FFMPEG)"]
    fn real_mp3_analysis_writeback_and_rescan() {
        let dir = tempfile::tempdir().unwrap();
        let mp3 = make_mp3(dir.path());
        let mut report = crate::media::scan_library(dir.path()).unwrap();
        assert_eq!(analyze_missing(dir.path(), &mut report).estimated, 1);
        let bpm = existing_bpm(&report.custom_tags["rhythm.mp3"]).unwrap();
        assert!((bpm - 140.0).abs() <= 2.0, "got {bpm}");
        report.metadata.get_mut("rhythm.mp3").unwrap().insert("tempo".into(), "fast".into());
        assert_eq!(crate::scan_writeback::apply(dir.path(), &mut report).unwrap().len(), 1);
        let before = std::fs::read(&mp3).unwrap();
        let mut rescanned = crate::media::scan_library(dir.path()).unwrap();
        assert_eq!(existing_bpm(&rescanned.custom_tags["rhythm.mp3"]), Some(bpm));
        assert!(rescanned.custom_tags["rhythm.mp3"].iter().any(|t| t.name == "tempo" && t.value == "fast"));
        assert_eq!(analyze_missing(dir.path(), &mut rescanned), Tally::default());
        // No fresh estimate on a second scan: the written TBPM is authoritative.
        assert!(rescanned.metadata.is_empty());
        assert!(crate::scan_writeback::apply(dir.path(), &mut rescanned).unwrap().is_empty());
        assert_eq!(std::fs::read(&mp3).unwrap(), before);
    }
}
