//! Forces a rebuild when the embedded asset trees change.
//!
//! `rust_embed`'s derive expands to one `include_bytes!` per file that exists
//! at compile time, so cargo only tracks the files it saw on the last build:
//! *adding* an asset does not invalidate this crate. Debug builds hide the
//! problem (without `debug-embed` they read the files from disk at runtime),
//! so a newly added asset renders fine under `cargo run` and then silently
//! resolves to "no asset exists at path" in a bundled release build.
//!
//! Cargo scans `rerun-if-changed` directories recursively, so watching the
//! embedded roots catches adds, removes and renames.
fn main() {
    // Keep in sync with the `#[folder]` / `#[include]` attributes in `lib.rs`.
    for dir in ["../../app/assets/bundled", "../../app/assets/async"] {
        println!("cargo:rerun-if-changed={dir}");
    }
}
