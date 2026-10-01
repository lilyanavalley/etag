use std::{env, fs, path::PathBuf};

fn main() {
    // Copy `memory.x` to the linker search path so that the linker can find it.
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::copy("memory.x", out.join("memory.x")).expect("failed to copy memory.x");
    println!("cargo:rustc-link-search={}", out.display());

    // Re-run this build script only when the relevant files change.
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=build.rs");
}
