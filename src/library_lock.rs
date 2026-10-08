//! Cooperative lock for all StationD writers, including hosts sharing NFS.
use crate::media_tags::TagError;
use std::path::{Path, PathBuf};

pub(crate) struct LibraryLock {
    path: PathBuf,
}
impl Drop for LibraryLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.path.join("owner"));
        let _ = std::fs::remove_dir(&self.path);
    }
}

pub(crate) async fn acquire(
    root: &Path,
) -> Result<std::sync::Arc<LibraryLock>, crate::library_actor::LibraryError> {
    if !root.is_dir() {
        return Err(crate::library_actor::LibraryError::BadRoot(root.into()));
    }
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || acquire_blocking(&root, std::time::Duration::from_secs(30)))
        .await
        .map_err(|e| crate::library_actor::LibraryError::Join(e.to_string()))?
        .map(std::sync::Arc::new)
        .map_err(Into::into)
}

fn acquire_blocking(root: &Path, timeout: std::time::Duration) -> Result<LibraryLock, TagError> {
    let path = root.join(".stationd-library.lock");
    let start = std::time::Instant::now();
    loop {
        match std::fs::create_dir(&path) {
            Ok(()) => {
                let lock = LibraryLock { path };
                let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into());
                std::fs::write(
                    lock.path.join("owner"),
                    format!(
                        "host={host} pid={} token={}\n",
                        std::process::id(),
                        uuid::Uuid::new_v4()
                    ),
                )
                .map_err(|e| TagError::Io(e.to_string()))?;
                return Ok(lock);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if start.elapsed() >= timeout {
                    return Err(TagError::Io(format!(
                        "shared library locked at {}; after a crashed writer, verify all writers have stopped before removing this directory",
                        path.display()
                    )));
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => {
                return Err(TagError::Io(format!(
                    "cannot lock shared library {}: {e}",
                    path.display()
                )));
            }
        }
    }
}

/// Keep ownership and ACL/xattrs on verified staged replacements. On root-squashed
/// exports or differing UIDs, fail before rename if ownership cannot be retained.
pub(crate) fn preserve_access(source: &Path, stage: &Path) -> Result<(), TagError> {
    let fail = |e: std::io::Error| {
        TagError::Io(format!(
            "cannot preserve access rights of {}: {e}",
            source.display()
        ))
    };
    let original = std::fs::metadata(source).map_err(fail)?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::MetadataExt;
        let target = std::fs::metadata(stage).map_err(fail)?;
        let path = std::ffi::CString::new(stage.as_os_str().as_bytes())
            .map_err(|e| TagError::Io(e.to_string()))?;
        if (target.uid(), target.gid()) != (original.uid(), original.gid()) {
            if unsafe { libc::chown(path.as_ptr(), original.uid(), original.gid()) } != 0 {
                return Err(fail(std::io::Error::last_os_error()));
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    std::fs::set_permissions(stage, original.permissions()).map_err(fail)?;
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let src = std::ffi::CString::new(source.as_os_str().as_bytes())
            .map_err(|e| TagError::Io(e.to_string()))?;
        let dst = std::ffi::CString::new(stage.as_os_str().as_bytes())
            .map_err(|e| TagError::Io(e.to_string()))?;
        // Copy every exposed attribute, including POSIX and NFSv4 ACLs. Refuse
        // on permission/IO errors; a filesystem with no xattrs has none to copy.
        let n = unsafe { libc::listxattr(src.as_ptr(), std::ptr::null_mut(), 0) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::ENOTSUP) {
                std::fs::set_permissions(stage, original.permissions()).map_err(fail)?;
                return Ok(());
            }
            return Err(TagError::Io(format!(
                "cannot list source attributes {}: {e}",
                source.display()
            )));
        }
        let mut names = vec![0u8; n as usize];
        let n = unsafe { libc::listxattr(src.as_ptr(), names.as_mut_ptr().cast(), names.len()) };
        if n < 0 {
            return Err(fail(std::io::Error::last_os_error()));
        }
        // The temporary inode may inherit a default ACL absent from the source.
        // Remove such extra attributes before installing the original attributes.
        let source_names: std::collections::HashSet<&[u8]> = names[..n as usize]
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .collect();
        let stage_len = unsafe { libc::listxattr(dst.as_ptr(), std::ptr::null_mut(), 0) };
        if stage_len < 0 {
            return Err(TagError::Io(format!(
                "cannot list staged attributes {}: {}",
                stage.display(),
                std::io::Error::last_os_error()
            )));
        }
        let mut stage_names = vec![0u8; stage_len as usize];
        let stage_len = unsafe {
            libc::listxattr(
                dst.as_ptr(),
                stage_names.as_mut_ptr().cast(),
                stage_names.len(),
            )
        };
        if stage_len < 0 {
            return Err(TagError::Io(format!(
                "cannot list staged attributes {}: {}",
                stage.display(),
                std::io::Error::last_os_error()
            )));
        }
        for name in stage_names[..stage_len as usize]
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
        {
            if !source_names.contains(name) {
                let name = std::ffi::CString::new(name).unwrap();
                if unsafe { libc::removexattr(dst.as_ptr(), name.as_ptr()) } != 0 {
                    return Err(TagError::Io(format!(
                        "cannot remove inherited attribute {} from {}: {}",
                        name.to_string_lossy(),
                        stage.display(),
                        std::io::Error::last_os_error()
                    )));
                }
            }
        }
        // NFSv4 ACLs may grant access through a group despite owner mode bits
        // being zero. List inherited attributes before chmod, which temporarily
        // rewrites that ACL; then restore the authoritative source ACL last.
        std::fs::set_permissions(stage, original.permissions()).map_err(fail)?;
        for name in names[..n as usize]
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
        {
            let name = std::ffi::CString::new(name).unwrap();
            let len =
                unsafe { libc::getxattr(src.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
            if len < 0 {
                return Err(TagError::Io(format!(
                    "cannot read attribute {} from {}: {}",
                    name.to_string_lossy(),
                    source.display(),
                    std::io::Error::last_os_error()
                )));
            }
            let mut value = vec![0u8; len as usize];
            let len = unsafe {
                libc::getxattr(
                    src.as_ptr(),
                    name.as_ptr(),
                    value.as_mut_ptr().cast(),
                    value.len(),
                )
            };
            if len < 0 {
                return Err(TagError::Io(format!(
                    "cannot read attribute {} from {}: {}",
                    name.to_string_lossy(),
                    source.display(),
                    std::io::Error::last_os_error()
                )));
            }
            if unsafe {
                libc::setxattr(
                    dst.as_ptr(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    len as usize,
                    0,
                )
            } != 0
            {
                return Err(TagError::Io(format!(
                    "cannot set attribute {} on {}: {}",
                    name.to_string_lossy(),
                    stage.display(),
                    std::io::Error::last_os_error()
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[test]
    #[ignore = "worker launched by process_lock test"]
    fn process_worker() {
        let root = PathBuf::from(std::env::var("STATIOND_TEST_LOCK_ROOT").unwrap());
        assert!(acquire_blocking(&root, std::time::Duration::ZERO).is_err());
        std::fs::write(root.join("ready"), "ready").unwrap();
        let start = std::time::Instant::now();
        while !root.join("release").exists() {
            assert!(start.elapsed() < std::time::Duration::from_secs(30));
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _lock = acquire_blocking(&root, std::time::Duration::from_secs(10)).unwrap();
        std::fs::write(root.join("acquired"), "acquired").unwrap();
    }

    pub(crate) fn verify_process_lock(root: &Path) {
        let lock = acquire_blocking(root, std::time::Duration::ZERO).unwrap();
        let mut worker = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "library_lock::tests::process_worker",
                "--ignored",
                "--quiet",
            ])
            .env("STATIOND_TEST_LOCK_ROOT", root)
            .spawn()
            .unwrap();
        let start = std::time::Instant::now();
        while !root.join("ready").exists() {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(30),
                "worker did not start"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!root.join("acquired").exists());
        drop(lock);
        std::fs::write(root.join("release"), "release").unwrap();
        assert!(worker.wait().unwrap().success());
        assert!(root.join("acquired").exists());
        std::fs::remove_file(root.join("release")).unwrap();
        std::fs::remove_file(root.join("ready")).unwrap();
        std::fs::remove_file(root.join("acquired")).unwrap();
    }

    #[test]
    fn process_lock() {
        let dir = tempfile::tempdir().unwrap();
        verify_process_lock(dir.path());
    }

    #[test]
    fn lock_excludes_other_writers_and_never_steals_abandoned_lock() {
        let dir = tempfile::tempdir().unwrap();
        let lock = acquire_blocking(dir.path(), std::time::Duration::ZERO).unwrap();
        assert!(acquire_blocking(dir.path(), std::time::Duration::ZERO).is_err());
        drop(lock);
        assert!(acquire_blocking(dir.path(), std::time::Duration::ZERO).is_ok());
        std::fs::create_dir(dir.path().join(".stationd-library.lock")).unwrap();
        assert!(acquire_blocking(dir.path(), std::time::Duration::ZERO).is_err());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn staged_replacement_preserves_owner_group_mode_and_xattrs() {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("source");
        let dst = dir.path().join("stage");
        std::fs::write(&src, "source").unwrap();
        std::fs::write(&dst, "stage").unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o640)).unwrap();
        let path = std::ffi::CString::new(src.to_str().unwrap()).unwrap();
        let name = c"user.stationd_test";
        assert_eq!(
            unsafe { libc::setxattr(path.as_ptr(), name.as_ptr(), b"value".as_ptr().cast(), 5, 0) },
            0
        );
        // POSIX ACL: named user in addition to owner/group/mask/other.
        let mut acl = 2u32.to_le_bytes().to_vec();
        for (tag, perm, id) in [
            (1u16, 6u16, u32::MAX),
            (2, 4, 12345),
            (4, 4, u32::MAX),
            (16, 4, u32::MAX),
            (32, 0, u32::MAX),
        ] {
            acl.extend(tag.to_le_bytes());
            acl.extend(perm.to_le_bytes());
            acl.extend(id.to_le_bytes());
        }
        let acl_name = c"system.posix_acl_access";
        assert_eq!(
            unsafe {
                libc::setxattr(
                    path.as_ptr(),
                    acl_name.as_ptr(),
                    acl.as_ptr().cast(),
                    acl.len(),
                    0,
                )
            },
            0
        );
        let staged_path = std::ffi::CString::new(dst.to_str().unwrap()).unwrap();
        let extra = c"user.inherited";
        assert_eq!(
            unsafe {
                libc::setxattr(
                    staged_path.as_ptr(),
                    extra.as_ptr(),
                    b"extra".as_ptr().cast(),
                    5,
                    0,
                )
            },
            0
        );
        preserve_access(&src, &dst).unwrap();
        assert_eq!(
            unsafe {
                libc::getxattr(
                    staged_path.as_ptr(),
                    extra.as_ptr(),
                    std::ptr::null_mut(),
                    0,
                )
            },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ENODATA)
        );
        let a = std::fs::metadata(src).unwrap();
        let b = std::fs::metadata(&dst).unwrap();
        assert_eq!((a.uid(), a.gid(), a.mode()), (b.uid(), b.gid(), b.mode()));
        let path = std::ffi::CString::new(dst.to_str().unwrap()).unwrap();
        let mut value = [0u8; 5];
        assert_eq!(
            unsafe { libc::getxattr(path.as_ptr(), name.as_ptr(), value.as_mut_ptr().cast(), 5) },
            5
        );
        assert_eq!(&value, b"value");
        let mut copied_acl = vec![0u8; acl.len()];
        assert_eq!(
            unsafe {
                libc::getxattr(
                    path.as_ptr(),
                    acl_name.as_ptr(),
                    copied_acl.as_mut_ptr().cast(),
                    copied_acl.len(),
                )
            },
            acl.len() as isize
        );
        assert_eq!(acl, copied_acl);
    }
}
