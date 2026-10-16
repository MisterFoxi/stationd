//! Conversion is done before enqueueing the library import, so FFmpeg never
//! holds the shared-library writer lock or blocks scans and tag edits.
use super::StagedUpload;
use serde::Deserialize;
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command};

/// A closed output profile: extending it requires an explicit encoder/muxer
/// and a writable tag implementation. Only MP3 is enabled for broadcast today.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum OutputFormat {
    #[default]
    Mp3,
}
impl OutputFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
        }
    }
    fn encoder(self) -> &'static str {
        match self {
            Self::Mp3 => "libmp3lame",
        }
    }
    fn muxer(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ConversionConfig {
    output_format: OutputFormat,
    ffmpeg: String,
    mp3_bitrate_kbps: u32,
    max_output_mb: u64,
    timeout_seconds: u64,
}
impl Default for ConversionConfig {
    fn default() -> Self {
        Self {
            output_format: OutputFormat::Mp3,
            ffmpeg: "ffmpeg".into(),
            mp3_bitrate_kbps: 320,
            max_output_mb: 512,
            timeout_seconds: 900,
        }
    }
}
impl ConversionConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.ffmpeg.trim().is_empty() || self.ffmpeg.len() > 4096 || self.ffmpeg.contains('\0') {
            return Err("media-uploader: conversion.ffmpeg must name an executable".into());
        }
        if ![
            32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ]
        .contains(&self.mp3_bitrate_kbps)
            || !(1..=2048).contains(&self.max_output_mb)
            || !(1..=3600).contains(&self.timeout_seconds)
        {
            return Err("media-uploader: invalid MP3 bitrate, output size (1..2048 MiB) or conversion timeout (1..3600 seconds)".into());
        }
        Ok(())
    }
}

fn input_demuxer(name: &str) -> Option<&'static str> {
    let ext = Path::new(name).extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "mp3" => "mp3",
        "wav" => "wav",
        "aif" | "aiff" => "aiff",
        "flac" => "flac",
        "ogg" | "oga" | "opus" => "ogg",
        "m4a" => "mov",
        "aac" => "aac",
        "wma" => "asf",
        _ => return None,
    })
}
impl StagedUpload {
    pub(crate) fn staging_id(&self) -> &str {
        &self.id
    }

    /// A completed MP3 is passed through unchanged. Other allowed audio
    /// containers are decoded by a forced demuxer and converted in quarantine.
    pub(crate) async fn prepare(&mut self, config: &ConversionConfig) -> Result<(), String> {
        config.validate()?;
        let extension = config.output_format.extension();
        if Path::new(&self.name)
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.eq_ignore_ascii_case(extension))
        {
            return Ok(());
        }
        self.retain = true;
        let result = self.convert(config).await;
        if let Err(why) = &result {
            let _ = tokio::fs::write(self.directory.join("error.txt"), why).await;
        }
        result
    }

    async fn convert(&mut self, config: &ConversionConfig) -> Result<(), String> {
        let demuxer = input_demuxer(&self.name).ok_or("unsupported input audio format")?;
        let output = self.directory.join(format!(
            ".converted.{}.part",
            config.output_format.extension()
        ));
        self.conversion_output = Some(output.clone());
        let limit = config.max_output_mb * 1024 * 1024;
        let mut command = Command::new(&config.ffmpeg);
        command
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-n",
                "-protocol_whitelist",
                "file",
                "-threads",
                "1",
                "-f",
                demuxer,
                "-i",
            ])
            .arg(&self.file)
            .args([
                "-map",
                "0:a:0",
                "-vn",
                "-sn",
                "-dn",
                "-map_metadata",
                "0",
                "-c:a",
                config.output_format.encoder(),
                "-b:a",
                &format!("{}k", config.mp3_bitrate_kbps),
                "-ar",
                "44100",
                "-ac",
                "2",
                "-id3v2_version",
                "3",
                "-write_id3v1",
                "0",
                "-threads",
                "1",
                "-fs",
                &limit.to_string(),
                "-f",
                config.output_format.muxer(),
            ])
            .arg(&output)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|e| format!("cannot start FFmpeg: {e}"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or("FFmpeg diagnostics unavailable")?;
        // Drain stderr throughout conversion, retaining at most 16 KiB.
        // A malformed input cannot grow memory or fill an unbounded log file.
        let diagnostics = async {
            let mut saved = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let count = stderr.read(&mut buffer).await.map_err(|e| e.to_string())?;
                if count == 0 {
                    break;
                }
                let keep = count.min(16384usize.saturating_sub(saved.len()));
                saved.extend_from_slice(&buffer[..keep]);
            }
            Ok::<_, String>(saved)
        };
        let processing = async { tokio::join!(child.wait(), diagnostics) };
        let (status, log) =
            match tokio::time::timeout(Duration::from_secs(config.timeout_seconds), processing)
                .await
            {
                Ok(result) => result,
                Err(_) => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    return Err("conversion timeout; original retained in staging".into());
                }
            };
        let log = log?;
        tokio::fs::write(self.directory.join("conversion.log"), &log)
            .await
            .map_err(|e| e.to_string())?;
        let status = status.map_err(|e| format!("cannot wait for FFmpeg: {e}"))?;
        if !status.success() {
            return Err(format!(
                "FFmpeg conversion failed ({status}); original retained in staging"
            ));
        }
        let size = tokio::fs::metadata(&output)
            .await
            .map_err(|e| e.to_string())?
            .len();
        if size == 0 || size >= limit {
            return Err("converted file is empty or reaches the output size limit; original retained in staging".into());
        }
        // Validate the actual encoded file before changing the staged identity.
        let check_root = self.directory.clone();
        let check_file = output.clone();
        tokio::task::spawn_blocking(move || {
            let (parsed, _) = crate::media::read_tagged(&check_file).map_err(|e| e.to_string())?;
            use lofty::file::TaggedFileExt;
            if parsed.file_type() != lofty::file::FileType::Mpeg {
                return Err("converter did not produce MP3".into());
            }
            crate::media::scan_file(&check_root, &check_file)
                .map_err(|e| format!("invalid converted audio: {e:?}"))?;
            std::fs::File::open(&check_file)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())??;
        self.source = Some(std::mem::replace(&mut self.file, output));
        self.name = Path::new(&self.name)
            .with_extension(config.output_format.extension())
            .to_string_lossy()
            .into_owned();
        Ok(())
    }
}

#[cfg(test)]
#[path = "conversion_tests.rs"]
mod tests;
