//! Resources and linker settings.

/// Rarely needed DLLs, loaded on first use to keep start-up short.
const LAZY: &[&str] =
    &["pdh", "wlanapi", "iphlpapi", "version", "ole32", "powrprof", "shell32", "wtsapi32", "netapi32", "combase"];

fn main() {
    for watched in ["build.rs", "resources", "assets/btm.ico", "src"] {
        println!("cargo:rerun-if-changed={watched}");
    }
    embed_resource::compile("resources/btm.rc", embed_resource::NONE);

    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs());
    println!("cargo:rustc-env=BTM_BUILD_ID={stamp}");

    let link = |argument: String| println!("cargo:rustc-link-arg-bins={argument}");
    for dll in LAZY {
        link(format!("/DELAYLOAD:{dll}.dll"));
    }
    link("delayimp.lib".into());
    // Static imports resolve from System32 only.
    link("/DEPENDENTLOADFLAG:0x800".into());
    // Keep the builder's folder names out of the executable.
    link("/PDBALTPATH:%_PDB%".into());
}
