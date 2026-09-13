fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::compile_protos("proto/station.proto")?;

    // Force a rebuild whenever a migration file is added, changed, or
    // removed. `sqlx::migrate!` embeds the migrations into the binary at
    // COMPILE time, but Cargo does not, on its own, treat a new *.sql file
    // as a reason to recompile the crate — so a freshly added migration
    // could silently fail to be embedded (the daemon then logs "migrations
    // applied" while missing the newest one). Watching the directory closes
    // that gap: touch the folder and any change invalidates the build.
    println!("cargo:rerun-if-changed=migrations");

    // Also re-run this build script itself if the proto changes.
    println!("cargo:rerun-if-changed=proto/station.proto");

    Ok(())
}
