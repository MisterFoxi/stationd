fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto = "../stationd-admin/proto/icecast_admin.proto";
    println!("cargo:rerun-if-changed={proto}");
    tonic_build::configure().build_server(std::env::var_os("CARGO_FEATURE_TEST_FIXTURE").is_some()).compile_protos(&[proto], &["../stationd-admin/proto"])?;
    Ok(())
}
