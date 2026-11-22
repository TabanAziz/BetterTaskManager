//! Embeds the already built BTM.exe. `tools/release.ps1` builds both in the right order.

use std::path::PathBuf;

fn main() {
    let here = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let payload = here.join("../target/release/BTM.exe");
    assert!(payload.is_file(), "build BTM first: cargo build --release (in the repository root)");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("set by cargo"));
    std::fs::copy(&payload, out.join("BTM.exe")).expect("copy the payload");

    for watched in ["build.rs", "setup.rc", "../resources/btm.manifest", "../assets/btm.ico"] {
        println!("cargo:rerun-if-changed={watched}");
    }
    println!("cargo:rerun-if-changed={}", payload.display());
    embed_resource::compile("setup.rc", embed_resource::NONE);
    // Setup usually runs from Downloads: never load a DLL that lies next to it.
    println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
    println!("cargo:rustc-link-arg-bins=/PDBALTPATH:%_PDB%");
}
