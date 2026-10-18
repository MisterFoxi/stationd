pub mod agent;
pub mod discovery;
pub mod sd_adapter;
pub mod server;
pub mod store;
// tonic 0.12 generates an ABI returning Status by value.
#[allow(clippy::result_large_err)]
pub mod wire {
    tonic::include_proto!("stationdeck.config.v1");
}
use std::os::unix::fs::OpenOptionsExt;
use std::{fs, io::Write, path::Path};
/// Same-directory atomic replacement, private file and durable parent entry.
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<(), &'static str> {
    let parent = path.parent().ok_or("Invalid state path.")?;
    fs::create_dir_all(parent).map_err(|_| "Cannot create state directory.")?;
    let tmp = parent.join(format!(".next-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|_| "Cannot create private state file.")?;
        file.write_all(data)
            .and_then(|_| file.sync_all())
            .map_err(|_| "Cannot persist state file.")?;
        fs::rename(&tmp, path).map_err(|_| "Cannot replace state file.")?;
        fs::File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|_| "Cannot persist state directory.")
    })();
    let _ = fs::remove_file(tmp);
    result
}
pub fn read_token(path: &Path) -> Result<String, &'static str> {
    let token = fs::read_to_string(path).map_err(|_| "Credential file is unavailable.")?;
    let token = token.trim();
    if !(32..=512).contains(&token.len()) || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("Invalid credential file.");
    }
    Ok(token.into())
}
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
