fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── Tauri build metadata ──────────────────────────────────────────────────
    tauri_build::build();

    // ── gRPC code generation ──────────────────────────────────────────────────
    //
    // protoc-bin-vendored provides a pre-built `protoc` binary so no system
    // installation is needed on the build machine.
    let protoc = protoc_bin_vendored::protoc_bin_path()
        .expect("protoc-bin-vendored: could not locate protoc binary");

    std::env::set_var("PROTOC", protoc);

    tonic_prost_build::compile_protos("../../proto/etag_bridge.proto")?;

    println!("cargo:rerun-if-changed=../../proto/etag_bridge.proto");
    println!("cargo:rerun-if-changed=build.rs");

    Ok(())
}
