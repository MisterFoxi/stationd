//! Opt-in control messages for Webmin's local browser audio player.
//! Hex encoding prevents file metadata from injecting terminal controls.
use std::io::{self, Write};
pub fn available() -> bool {
    std::env::var("STATIOND_WEBMIN_MEDIA_PREVIEW").as_deref() == Ok("1")
}
fn command(path: Option<&str>) -> io::Result<String> {
    let payload = match path {
        Some(path) if !path.is_empty() && path.len() <= 4096 && !path.contains('\0') => path
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        Some(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid preview path",
            ))
        }
        None => "stop".into(),
    };
    Ok(format!("\x1b]778;stationd-preview;{payload}\x07"))
}
fn write_request(path: Option<&str>, connected: bool, out: &mut impl Write) -> io::Result<()> {
    if !connected {
        return if path.is_none() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no Webmin audio player connected",
            ))
        };
    }
    let data = command(path)?;
    out.write_all(data.as_bytes())?;
    out.flush()
}
pub fn request(path: Option<&str>) -> io::Result<()> {
    write_request(path, available(), &mut io::stdout().lock())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_without_a_browser_returns_an_error_instead_of_silent_success() {
        let mut output = Vec::new();
        let error = write_request(Some("song.mp3"), false, &mut output).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(output.is_empty());
        write_request(None, false, &mut output).unwrap();
        write_request(Some("song.mp3"), true, &mut output).unwrap();
        assert_eq!(output, command(Some("song.mp3")).unwrap().as_bytes());
    }
    #[test]
    fn preview_command_cannot_inject_terminal_sequences() {
        let command = command(Some("été/evil\x1b]52;secret.mp3")).unwrap();
        assert_eq!(command.matches('\x1b').count(), 1);
        assert_eq!(command.matches('\x07').count(), 1);
        assert!(command.starts_with("\x1b]778;stationd-preview;"));
        assert!(!command.contains("secret"));
        assert_eq!(
            super::command(None).unwrap(),
            "\x1b]778;stationd-preview;stop\x07"
        );
        assert!(super::command(Some("")).is_err());
    }
}
