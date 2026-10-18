fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::compile_protos("proto/config.proto")?;
    println!("cargo:rerun-if-changed=proto/config.proto");
    Ok(())
}
