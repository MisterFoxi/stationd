use anyhow::{bail, Context, Result};
use station_icecast_config::{render, revision, Config};
use std::path::Path;

fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() < 3
        || !["check", "render"].contains(&args[0].as_str())
        || (args[0] == "render" && args.len() != 4)
        || (args[0] == "check" && args.len() != 3)
    {
        bail!("usage: station-icecast-config check <config.toml> <secrets-dir> | render <config.toml> <secrets-dir> <output.xml>");
    }
    let text = std::fs::read_to_string(&args[1]).context("cannot read Icecast TOML")?;
    let config = Config::parse(&text)?;
    if args[0] == "check" {
        config.check_assets(Path::new(&args[2]))?;
        println!("valid {}", revision(&text));
        return Ok(());
    }
    let rendered = render(&text, Path::new(&args[2]))?;
    let output = Path::new(&args[3]);
    let temporary = output.with_extension(format!("{}.tmp", std::process::id()));
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts
        .open(&temporary)
        .context("cannot create private output")?;
    let result = (|| -> Result<()> {
        file.write_all(rendered.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, output).context("cannot replace output")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
