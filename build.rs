//! Build script: keeps the embedded `assets/` directory in sync in release builds.
//!
//! `src/vendor.rs` embeds `assets/` with rust-embed. With debug assertions off it expands
//! to one `include_bytes!` per file that existed at expansion time, so Cargo notices edits
//! to those files but not files that were added or removed. Watching the directory makes
//! Cargo rebuild the crate in that case. Debug builds read `assets/` from disk at runtime
//! and need no rebuild. (T47 may add the git-sha embedding here.)

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    // Set by Cargo for the target configuration: present exactly when rust-embed reads
    // from disk instead of embedding.
    if std::env::var_os("CARGO_CFG_DEBUG_ASSERTIONS").is_none() {
        // A directory is scanned recursively, so new and deleted files count as changes.
        println!("cargo::rerun-if-changed=assets");
    }
}
