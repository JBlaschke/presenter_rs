//! Embedded assets: vendored reveal.js/KaTeX, our own scripts and the built-in themes.
//!
//! Contract: `docs/spec/vendored-assets.md` §4. Paths are relative to `assets/`, e.g.
//! `vendor/reveal/dist/reveal.js`. Debug builds read the files from disk at runtime (so
//! editing `assets/themes/default/*` under `cargo run` is picked up); release builds embed
//! them in the binary.

use std::borrow::Cow;

use crate::error::{Error, Result};

/// The embedded `assets/` directory (vendor files, presenter scripts, built-in themes).
#[derive(rust_embed::Embed)]
#[folder = "assets/"]
#[include = "vendor/**/*"]
#[include = "presenter/**/*"]
#[include = "themes/**/*"]
#[exclude = "**/*.map"]
pub struct Assets;

/// Returns the bytes of the embedded file at `path` (relative to `assets/`), if present.
pub fn get(path: &str) -> Option<Cow<'static, [u8]>> {
    Assets::get(path).map(|file| file.data)
}

/// Returns the embedded file at `path` as UTF-8 text.
///
/// Fails with [`Error::Other`] when the file does not exist or is not valid UTF-8.
pub fn get_str(path: &str) -> Result<String> {
    let data =
        get(path).ok_or_else(|| Error::Other(format!("embedded asset not found: {path}")))?;
    String::from_utf8(data.into_owned())
        .map_err(|e| Error::Other(format!("embedded asset {path} is not valid UTF-8: {e}")))
}

/// Lists every embedded file whose path starts with `prefix`, sorted lexicographically.
///
/// The match is a plain string prefix, so pass a trailing `/` to list a directory
/// (`list("vendor/reveal/dist/theme/")`); nested files are included.
pub fn list(prefix: &str) -> Vec<String> {
    let mut paths: Vec<String> = Assets::iter()
        .filter(|path| path.starts_with(prefix))
        .map(Cow::into_owned)
        .collect();
    paths.sort();
    paths
}
