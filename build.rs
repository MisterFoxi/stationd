fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::compile_protos("proto/station.proto")?;
    // The dedicated scheduling service (grid rules + ResolveNext). Its
    // google.protobuf.Timestamp/Duration imports are resolved by tonic-build's
    // bundled well-known types and map to `::prost_types::*`.
    // protoc 3.12-3.14 requires this flag for proto3 field presence.
    // Newer versions also accept it; keep optional counts distinct from zero.
    tonic_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .compile_protos(&["proto/schedule_v1.proto"], &["proto"])?;
    // The media library service (scan, list, search, tags). Uses proto3
    // `optional` (SetTags: absent field = unchanged).
    tonic_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .compile_protos(&["proto/library_v1.proto"], &["proto"])?;
    // The plugin service (list + lifecycle control).
    tonic_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .compile_protos(&["proto/plugin_v1.proto"], &["proto"])?;
    // The broadcast control service (state, overrides, listener injection).
    // Uses proto3 `optional` (listeners) → same flag as schedule_v1.
    tonic_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .compile_protos(&["proto/broadcast_v1.proto"], &["proto"])?;
    // The Liquidsoap service (script render + bridge status, stationctl ls).
    tonic_build::compile_protos("proto/liquidsoap_v1.proto")?;
    // The Icecast service (audience + mount health, stationctl icecast).
    // Uses proto3 `optional` (audience, listeners, read_kbps).
    tonic_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .compile_protos(&["proto/icecast_v1.proto"], &["proto"])?;
    // The live DJ service (status, kick, password hash — stationctl live / dj).
    tonic_build::compile_protos("proto/live_v1.proto")?;
    // Broadcast statistics (plays by playlist / rule / media — stationctl stats).
    tonic_build::compile_protos("proto/stats_v1.proto")?;
    // The on-air view (snapshot stream + history — stationctl onair, TUI).
    // Uses proto3 `optional` (listeners, durations, instants).
    tonic_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .compile_protos(&["proto/onair_v1.proto"], &["proto"])?;
    // The playlists (list, export, validate, pool preview, save, remove,
    // sync, reload, add). Uses proto3 `optional` (pool counts).
    tonic_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .compile_protos(&["proto/playlist_v1.proto"], &["proto"])?;
    // The station journal (typed facts + warn/error lines — stationctl
    // events, the TUI's Système screen).
    tonic_build::compile_protos("proto/events_v1.proto")?;

    // Force a rebuild whenever a migration file is added, changed, or
    // removed. `sqlx::migrate!` embeds the migrations into the binary at
    // COMPILE time, but Cargo does not, on its own, treat a new *.sql file
    // as a reason to recompile the crate — so a freshly added migration
    // could silently fail to be embedded (the daemon then logs "migrations
    // applied" while missing the newest one). Watching the directory closes
    // that gap: touch the folder and any change invalidates the build.
    println!("cargo:rerun-if-changed=migrations");

    // Also re-run this build script itself if a proto changes.
    println!("cargo:rerun-if-changed=proto/station.proto");
    println!("cargo:rerun-if-changed=proto/schedule_v1.proto");
    println!("cargo:rerun-if-changed=proto/library_v1.proto");
    println!("cargo:rerun-if-changed=proto/plugin_v1.proto");
    println!("cargo:rerun-if-changed=proto/broadcast_v1.proto");
    println!("cargo:rerun-if-changed=proto/liquidsoap_v1.proto");
    println!("cargo:rerun-if-changed=proto/icecast_v1.proto");
    println!("cargo:rerun-if-changed=proto/live_v1.proto");
    println!("cargo:rerun-if-changed=proto/stats_v1.proto");
    println!("cargo:rerun-if-changed=proto/onair_v1.proto");
    println!("cargo:rerun-if-changed=proto/playlist_v1.proto");
    println!("cargo:rerun-if-changed=proto/events_v1.proto");

    Ok(())
}
