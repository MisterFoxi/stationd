fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::compile_protos("proto/icecast_admin.proto")?;
    println!("cargo:rerun-if-changed=proto/icecast_admin.proto");
    Ok(())
}
