use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Use the vendored protoc so users don't need it on PATH.
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    // Convert to absolute string and set for tonic-build / prost-build.
    unsafe {
        env::set_var("PROTOC", protoc.as_os_str());
    }

    let proto_root = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("proto");
    let proto_file = proto_root.join("jarvis.proto");
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);

    tonic_prost_build::configure()
        .file_descriptor_set_path(out_dir.join("jarvis_descriptor.bin"))
        .build_client(true)
        .build_server(true)
        .compile_protos(&[proto_file], &[proto_root])?;

    Ok(())
}
