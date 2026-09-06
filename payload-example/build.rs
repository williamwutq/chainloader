//! Ensures the linker finds `link.ld` and relinks when it changes.
//!
//! The `-T link.ld` flag is supplied in `.cargo/config.toml`; adding the
//! manifest directory to the linker search path here lets the bare filename
//! resolve no matter what directory Cargo invokes the linker from.

fn main() {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo");
    println!("cargo:rustc-link-search={manifest_dir}");
    println!("cargo:rerun-if-changed=link.ld");
}
