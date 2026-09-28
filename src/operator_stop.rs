//! The operator's stop: `stationctl station stop` stops **stationd itself**,
//! and its supervisor (s6) does not start it again — not even after a
//! container or host restart — until `stationctl station start`.
//!
//! The switch is a marker file, [`MARKER_PATH`] (relative to stationd's
//! working directory, i.e. `$STATIOND_ROOT/data/stationd.stopped` in the
//! container), visible from the host. stationd writes it (fsync'd) before
//! exiting; the s6 `run` script finds it and parks the service (declared
//! ready, `s6-pause`) instead of launching stationd — Liquidsoap and Icecast,
//! which depend on it, still start. `stationctl station start` removes it and
//! restarts the service.
//!
//! This is not a broadcast state: no listener, no plugin, no core rule can
//! lift it (the idle stop is `sleeping`, see `station_control`).

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::resolver::Epoch;

/// Where the marker lives, relative to stationd's working directory (next to
/// the default database, `./data/stationd.db`). The s6 `run` script and
/// `stationctl station start|state` look for it at the same place under
/// `$STATIOND_ROOT`.
pub const MARKER_PATH: &str = "./data/stationd.stopped";

/// The marker under `root` (stationctl's view: `$STATIOND_ROOT`).
pub fn marker_under(root: &Path) -> PathBuf {
    root.join("data").join("stationd.stopped")
}

/// Write the marker durably: temp file + fsync + rename + fsync of the
/// directory. An error means NOTHING was stopped: the caller must not exit
/// (s6 would start stationd again at once — a ghost stop).
pub fn write_marker(path: &Path, at: Epoch, by: &str) -> std::io::Result<()> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let tmp = dir.join(".stationd.stopped.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        writeln!(f, "# stationd stopped by the operator — restart: stationctl station start")?;
        writeln!(f, "stopped_at = {}", at.0)?;
        writeln!(f, "by = {by}")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    std::fs::File::open(&dir)?.sync_all()?;
    Ok(())
}

/// The marker, if present: `Some(stopped_at)` (epoch s; `None` inside when
/// unreadable — the marker still stands).
pub fn read_marker(path: &Path) -> Option<Option<i64>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(None),
    };
    Some(text.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k.trim() == "stopped_at").then(|| v.trim().parse().ok()).flatten()
    }))
}

/// Remove the marker. Absent = fine (idempotent).
pub fn remove_marker(path: &Path) -> std::io::Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = marker_under(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        assert_eq!(read_marker(&path), None);
        write_marker(&path, Epoch(1_790_000_000), "cli").unwrap();
        assert_eq!(read_marker(&path), Some(Some(1_790_000_000)));
        assert!(!path.parent().unwrap().join(".stationd.stopped.tmp").exists());
        assert!(remove_marker(&path).unwrap());
        assert!(!remove_marker(&path).unwrap(), "idempotent");
        assert_eq!(read_marker(&path), None);
    }

    #[test]
    fn an_unwritable_marker_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        // No data/ directory: the write fails, loudly.
        let path = marker_under(dir.path());
        assert!(write_marker(&path, Epoch(1), "cli").is_err());
        assert_eq!(read_marker(&path), None);
    }

    #[test]
    fn an_unparsable_marker_still_stands() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m");
        std::fs::write(&path, "garbage").unwrap();
        assert_eq!(read_marker(&path), Some(None));
    }
}
