fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Compile the shared gRPC proto file.
    //
    // protoc-bin-vendored ships a pre-built `protoc` binary so no system
    // installation is required on the build machine.
    let protoc = protoc_bin_vendored::protoc_bin_path()
        .expect("protoc-bin-vendored: could not locate protoc binary");

    // Point tonic-build at the vendored protoc.
    std::env::set_var("PROTOC", protoc);

    tonic_prost_build::compile_protos("../proto/etag_bridge.proto")?;

    println!("cargo:rerun-if-changed=../proto/etag_bridge.proto");
    println!("cargo:rerun-if-changed=build.rs");

    Ok(())
}
