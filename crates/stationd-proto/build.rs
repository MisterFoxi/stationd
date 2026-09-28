//! Génère les clients gRPC depuis les .proto de la racine du dépôt.
//! Seuls les contrats réellement servis par stationd sont compilés (même liste
//! que le build.rs du daemon) ; `playlist_v1.proto` n'est pas servi.

const PROTO_DIR: &str = "../../proto";

/// Contrats servis, et s'ils utilisent `optional` proto3 (drapeau protoc requis
/// par protoc 3.12–3.14, accepté par les versions récentes).
const PROTOS: &[(&str, bool)] = &[
    ("station.proto", false),
    ("schedule_v1.proto", true),
    ("library_v1.proto", false),
    ("plugin_v1.proto", false),
    ("broadcast_v1.proto", true),
    ("liquidsoap_v1.proto", false),
    ("icecast_v1.proto", true),
    ("live_v1.proto", false),
    ("stats_v1.proto", false),
    ("onair_v1.proto", true),
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for (file, optional) in PROTOS {
        let path = format!("{PROTO_DIR}/{file}");
        let mut builder = tonic_build::configure().build_server(false);
        if *optional {
            builder = builder.protoc_arg("--experimental_allow_proto3_optional");
        }
        builder.compile_protos(&[path.as_str()], &[PROTO_DIR])?;
        println!("cargo:rerun-if-changed={path}");
    }
    Ok(())
}
