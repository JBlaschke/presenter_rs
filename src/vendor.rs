//! Embedded assets: vendored reveal.js/KaTeX, our own scripts and the built-in themes.
//!
//! Contract: `docs/spec/vendored-assets.md` §4. Paths are relative to `assets/`, e.g.
//! `vendor/reveal/dist/reveal.js`. Debug builds read the files from disk at runtime (so
//! editing `assets/themes/default/*` under `cargo run` is picked up); release builds embed
//! them in the binary.
//!
//! The two modes look files up differently (debug: a file-system path, release: an exact
//! key), so [`get`] normalizes the path and requires an exact, case-sensitive match of a
//! listed file in both modes. That way a path that works in a debug test also works in the
//! shipped binary.
//!
//! OS and editor junk (`.DS_Store`, AppleDouble `._*`, `Thumbs.db`, `*.swp`, `*~`) is
//! excluded so a file Finder drops into `assets/` is never listed, served or embedded.
//! `build.rs` makes release builds re-embed `assets/` when files are added or removed.

use std::borrow::Cow;

use crate::error::{Error, Result};

/// The embedded `assets/` directory (vendor files, presenter scripts, built-in themes).
#[derive(rust_embed::Embed)]
#[folder = "assets/"]
#[include = "vendor/**/*"]
#[include = "presenter/**/*"]
#[include = "themes/**/*"]
#[exclude = "**/*.map"]
#[exclude = "**/.DS_Store"]
#[exclude = "**/._*"]
#[exclude = "**/Thumbs.db"]
#[exclude = "**/*.swp"]
#[exclude = "**/*~"]
pub struct Assets;

/// Returns the bytes of the embedded file at `path` (relative to `assets/`), if present.
///
/// The path is normalized lexically first: `/` and `\` both separate segments, empty and
/// `.` segments are dropped and `..` removes the previous segment, so
/// `vendor/reveal/css/theme/./fonts/../white.scss` finds `vendor/reveal/css/theme/white.scss`.
/// A path that is empty or climbs above `assets/` yields `None`. The final lookup is exact
/// and case-sensitive, in debug and release builds alike.
pub fn get(path: &str) -> Option<Cow<'static, [u8]>> {
    let key = normalize(path)?;
    // Debug builds resolve `key` on disk, which also matches `Reveal.js` on a
    // case-insensitive file system (macOS) and reaches files the embedded map would not
    // contain. Release builds look the key up exactly; insist on that here too.
    if cfg!(debug_assertions) && !Assets::iter().any(|name| name == key) {
        return None;
    }
    Assets::get(&key).map(|file| file.data)
}

/// Returns the embedded file at `path` as UTF-8 text.
///
/// `path` is normalized as in [`get`]. Fails with [`Error::Other`] when the file does not
/// exist or is not valid UTF-8.
pub fn get_str(path: &str) -> Result<String> {
    let data =
        get(path).ok_or_else(|| Error::Other(format!("embedded asset not found: {path}")))?;
    String::from_utf8(data.into_owned())
        .map_err(|e| Error::Other(format!("embedded asset {path} is not valid UTF-8: {e}")))
}

/// Lists every embedded file whose path starts with `prefix`, sorted lexicographically.
///
/// The match is a plain string prefix on the normalized paths (`/`-separated, no `.` or
/// `..` segments, no leading `/`), so pass a trailing `/` to list a directory
/// (`list("vendor/reveal/dist/theme/")`); nested files are included. Every listed path is
/// accepted by [`get`].
pub fn list(prefix: &str) -> Vec<String> {
    let mut paths: Vec<String> = Assets::iter()
        .filter(|path| path.starts_with(prefix))
        .map(Cow::into_owned)
        .collect();
    paths.sort();
    paths
}

/// Normalizes an asset path lexically (see [`get`]); `None` if empty or escaping `assets/`.
fn normalize(path: &str) -> Option<String> {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            name => segments.push(name),
        }
    }
    if segments.is_empty() {
        None
    } else {
        Some(segments.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn normalize_drops_empty_and_dot_segments() {
        let clean = Some("vendor/reveal/dist/reveal.js".to_string());
        for path in [
            "vendor/reveal/dist/reveal.js",
            "./vendor/reveal/dist/reveal.js",
            "vendor/./reveal/dist/./reveal.js",
            "vendor//reveal///dist/reveal.js",
            "/vendor/reveal/dist/reveal.js",
            "vendor/reveal/dist/reveal.js/",
            "vendor\\reveal\\dist\\reveal.js",
        ] {
            assert_eq!(normalize(path), clean, "{path}");
        }
    }

    #[test]
    fn normalize_resolves_parent_segments() {
        assert_eq!(
            normalize("vendor/katex/fonts/../katex.min.css").as_deref(),
            Some("vendor/katex/katex.min.css")
        );
        assert_eq!(
            normalize("presenter/../vendor/VERSIONS.toml").as_deref(),
            Some("vendor/VERSIONS.toml")
        );
        assert_eq!(
            normalize("vendor/reveal/css/theme/./fonts/../../theme/white.scss").as_deref(),
            Some("vendor/reveal/css/theme/white.scss")
        );
    }

    #[test]
    fn normalize_rejects_empty_and_escaping_paths() {
        for path in [
            "",
            "/",
            ".",
            "./",
            "..",
            "../assets/vendor/VERSIONS.toml",
            "vendor/../../Cargo.toml",
            "vendor/..",
        ] {
            assert_eq!(normalize(path), None, "{path}");
        }
    }
}
