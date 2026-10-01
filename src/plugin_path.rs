use std::path::{Path, PathBuf};

pub(crate) fn resolve(name: &str, explicit: Option<&str>) -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    resolve_in(name, explicit, Path::new("/usr/lib/stationd/plugins"), &cwd)
}

fn resolve_in(name: &str, explicit: Option<&str>, installed: &Path, repo: &Path) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        if readable_metadata(Path::new(path))? { return Ok(PathBuf::from(path)); }
    }
    let safe = !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    let files: Vec<std::ffi::OsString> = if let Some(path) = explicit {
        vec![Path::new(path).file_name().ok_or_else(|| format!("plugin {name}: chemin WASM invalide"))?.to_os_string()]
    } else {
        if !safe { return Err(format!("plugin {name}: nom invalide")); }
        let stem = name.replace('-', "_");
        let mut files = vec![format!("{name}.wasm").into(), format!("{stem}.wasm").into()];
        if !stem.ends_with("_wasm") { files.push(format!("{stem}_wasm.wasm").into()); }
        files
    };
    let mut candidates = Vec::new();
    for file in &files { candidates.push(installed.join(file)); }
    for file in &files { candidates.push(repo.join("plugins").join(file)); }
    let mut crates = Vec::new();
    if safe { crates.extend([name.to_string(), format!("{name}-wasm")]); }
    for file in &files {
        if let Some(stem) = Path::new(file).file_stem().and_then(|s| s.to_str()) { crates.push(stem.replace('_', "-")); }
    }
    for krate in &crates {
        for file in &files { candidates.push(repo.join("plugins").join(krate).join("target/wasm32-unknown-unknown/release").join(file)); }
    }
    let mut access_errors = Vec::new();
    for path in &candidates {
        match readable_metadata(path) {
            Ok(true) => return Ok(path.clone()),
            Ok(false) => {},
            Err(error) => access_errors.push(error),
        }
    }
    if !access_errors.is_empty() {
        return Err(format!("plugin {name}: recherche WASM impossible : {}", access_errors.join("; ")));
    }
    let tried = candidates.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ");
    Err(format!("plugin {name}: module WASM introuvable ; recherchés: {tried}"))
}

fn readable_metadata(path: &Path) -> Result<bool, String> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(meta.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound || error.kind() == std::io::ErrorKind::NotADirectory => Ok(false),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolution_supports_deployment_development_and_overrides() {
        let root = std::env::temp_dir().join(format!("stationd-plugin-path-{}", std::process::id()));
        let installed = root.join("installed"); let repo = root.join("repo");
        std::fs::create_dir_all(&installed).unwrap();
        for (name, stem) in [("custom-tags", "custom_tags_wasm"), ("listener-stats", "listener_stats_wasm"), ("play-stats", "play_stats_wasm"), ("stop-when-idle-wasm", "stop_when_idle_wasm")] {
            let path = installed.join(format!("{stem}.wasm")); std::fs::write(&path, b"wasm").unwrap();
            assert_eq!(resolve_in(name, None, &installed, &repo).unwrap(), path);
            let old = format!("plugins/{name}/target/wasm32-unknown-unknown/release/{stem}.wasm");
            assert_eq!(resolve_in("alias", Some(&old), &installed, &repo).unwrap(), path);
        }
        let custom = root.join("custom.wasm"); std::fs::write(&custom, b"override").unwrap();
        assert_eq!(resolve_in("custom-tags", Some(custom.to_str().unwrap()), &installed, &repo).unwrap(), custom);
        assert!(resolve_in("custom-tags", Some("missing/other.wasm"), &installed, &repo).is_err());
        assert!(resolve_in("../outside", None, &installed, &repo).is_err());
        let artifact = repo.join("plugins/custom-tags-wasm/target/wasm32-unknown-unknown/release/custom_tags_wasm.wasm");
        std::fs::create_dir_all(artifact.parent().unwrap()).unwrap(); std::fs::write(&artifact, b"dev").unwrap();
        let shipped = installed.join("custom_tags_wasm.wasm");
        assert_eq!(resolve_in("custom-tags", None, &installed, &repo).unwrap(), shipped);
        std::fs::remove_file(shipped).unwrap();
        assert_eq!(resolve_in("custom-tags", None, &installed, &repo).unwrap(), artifact);
        let error = resolve_in("missing", None, &installed, &repo).unwrap_err();
        assert!(error.contains("missing_wasm.wasm"));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn inaccessible_parent_is_reported_as_access_error() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("stationd-plugin-denied-{}", std::process::id()));
        let installed = root.join("installed");
        std::fs::create_dir_all(&installed).unwrap();
        let module = installed.join("custom_tags_wasm.wasm");
        std::fs::write(&module, b"wasm").unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o600)).unwrap();
        let blocked = std::fs::metadata(&module).is_err();
        let result = resolve_in("custom-tags", None, &installed, &root.join("repo"));
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        // Root can bypass directory permissions.
        if blocked {
            let error = result.unwrap_err();
            assert!(error.contains("recherche WASM impossible"));
            assert!(!error.contains("introuvable"));
        }
    }

}

