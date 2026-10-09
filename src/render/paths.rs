//! Helper paths (`docs/spec/theme-format.md` §4): the strings `style()`, `script()` and
//! `asset()` take, and the `{*path}` of the dev server's `/__presenter/asset/{*path}` route.
//!
//! A helper path is normalized first (`/` and `\` separate segments, `.` and empty segments
//! are dropped, `..` removes the previous segment; a path that climbs out finds nothing),
//! then resolved by its first segment: `vendor/…` and `presenter/…` in the embedded tree,
//! `@<theme>/<path>` relative to that theme's root directory, anything else in the chain's
//! `assets/` directories.
//!
//! In serve mode, the URLs of theme-chain files that a page loads through `asset()` or
//! that a stylesheet's `url()` names carry the file's content version
//! ([`versioned_serve_url`]; live-reload.md §4, audit F03), remembered while the file's
//! size and modification time stay the same.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use super::html::SERVE_HELPER_PREFIX;
use crate::error::{Error, Result};
use crate::theme::{Theme, ThemeDir, ThemeSource, normalize_rel};

/// Resolves a helper path (`style()`, `script()`, `asset()`, and the dev server's
/// `/__presenter/asset/{*path}` route) to its bytes and MIME type, or `Ok(None)` if nothing
/// is there. The path is normalized first (`/` and `\` separate segments, `.` and empty
/// segments are dropped, `..` removes the previous segment; a path that climbs out is
/// `None`), then by its first segment (theme-format.md §4):
///
/// - `vendor/…` → the embedded vendor tree ([`crate::vendor::get`]);
/// - `presenter/…` → our own scripts in `assets/presenter/`;
/// - `@<theme>/<path>` → `<path>` relative to the **root directory** of that theme of the
///   chain (no `..` escapes; an unknown theme name finds nothing);
/// - anything else → the chain's `assets/` directories, child first ([`Theme::read_asset`]).
///
/// The MIME type is guessed from the extension (`application/octet-stream` otherwise).
/// Errors only when an existing file cannot be read. The dev server percent-decodes the
/// route's path before calling this (the renderer percent-encodes every byte other than
/// ASCII letters, digits and `-._~/@` in the URLs it writes).
pub fn resolve_helper_path(theme: &Theme, path: &str) -> Result<Option<(Vec<u8>, String)>> {
    let Some(key) = normalize_helper_path(path) else {
        return Ok(None);
    };
    let bytes = match path_form(&key) {
        PathForm::Builtin => crate::vendor::get(&key).map(Cow::into_owned),
        PathForm::Theme { name, rel } => match theme.dir(name) {
            Some(dir) => dir.read(rel)?,
            None => None,
        },
        PathForm::Asset => theme.read_asset(&key)?,
    };
    Ok(bytes.map(|bytes| (bytes, mime_of(&key))))
}

/// `path` normalized as in [`resolve_helper_path`]; `None` if it is empty or climbs out,
/// and for `@<theme>/<path>` if `<path>` climbs out of that theme (`@t/../x.css` would
/// otherwise name `x.css` in the chain's `assets/`; theme-format.md §4, audit F10).
pub fn normalize_helper_path(path: &str) -> Option<String> {
    let key = normalize_rel(path).filter(|key| !key.is_empty())?;
    let mut segments = path
        .split(['/', '\\'])
        .filter(|segment| !segment.is_empty() && *segment != ".");
    if segments.next().is_some_and(|first| first.starts_with('@')) {
        let mut depth = 0usize;
        for segment in segments {
            depth = match segment {
                ".." => depth.checked_sub(1)?,
                _ => depth + 1,
            };
        }
    }
    Some(key)
}

/// True if [`resolve_helper_path`] finds a file at the normalized helper path `key`, without
/// reading it (release builds do not copy embedded files; disk files are only stat'ed).
pub(super) fn helper_path_exists(theme: &Theme, key: &str) -> bool {
    match path_form(key) {
        PathForm::Builtin => crate::vendor::get(key).is_some(),
        PathForm::Theme { name, rel } => theme.dir(name).is_some_and(|dir| dir.exists(rel)),
        PathForm::Asset => theme
            .chain
            .iter()
            .any(|dir| dir.exists(&format!("assets/{key}"))),
    }
}

/// The form of a normalized helper path.
pub(super) enum PathForm<'k> {
    /// `vendor/…` or `presenter/…`: the embedded tree.
    Builtin,
    /// `@<name>/<rel>`.
    Theme {
        /// The theme's name.
        name: &'k str,
        /// The path relative to that theme's root directory.
        rel: &'k str,
    },
    /// Below the chain's `assets/` directories.
    Asset,
}

/// The form of the normalized helper path `key`.
pub(super) fn path_form(key: &str) -> PathForm<'_> {
    let (first, rest) = key.split_once('/').unwrap_or((key, ""));
    match first.strip_prefix('@') {
        Some(name) => PathForm::Theme { name, rel: rest },
        None if !rest.is_empty() && matches!(first, "vendor" | "presenter") => PathForm::Builtin,
        None => PathForm::Asset,
    }
}

/// True for `vendor/…` and `presenter/…` keys.
pub(super) fn is_builtin(key: &str) -> bool {
    matches!(path_form(key), PathForm::Builtin)
}

/// The MIME type guessed from `key`'s extension.
pub(super) fn mime_of(key: &str) -> String {
    mime_guess::from_path(key)
        .first_or_octet_stream()
        .essence_str()
        .to_string()
}

/// The helper path that the relative reference `rel` (a CSS `url()` without its
/// `?query#fragment`) in the file at helper path `base` names: `rel` joined to `base`'s
/// directory and normalized. This is exactly how a browser resolves `rel` against the
/// file's dev-server URL `/__presenter/asset/<base>`, so every output mode finds the same
/// file. `None` when the result is empty or climbs above the helper-path root (in serve
/// mode, such a URL leaves `/__presenter/asset/`).
pub(super) fn join_relative(base: &str, rel: &str) -> Option<String> {
    let dir = base.rsplit_once('/').map_or("", |(dir, _)| dir);
    // Like a browser, so `..` may leave `@<theme>/` here (unlike in a helper's argument).
    normalize_rel(&format!("{dir}/{rel}")).filter(|key| !key.is_empty())
}

/// Why `path` (as given to a helper) resolves to nothing, for "unknown path" messages.
pub(super) fn unknown_reason(theme: &Theme, path: &str) -> String {
    let Some(key) = normalize_rel(path) else {
        return "it climbs out of its root directory with '..'".into();
    };
    if key.is_empty() {
        return "the path is empty".into();
    }
    if normalize_helper_path(path).is_none() {
        return "it climbs out of the theme it names with '..'; an @<theme>/ path stays inside \
                that theme"
            .into();
    }
    match path_form(&key) {
        PathForm::Builtin if key.starts_with("vendor/") => {
            "looked in the embedded vendor tree".into()
        }
        PathForm::Builtin => "looked in assets/presenter/".into(),
        PathForm::Theme { name, .. } if theme.dir(name).is_none() => {
            let chain: Vec<&str> = theme.chain.iter().map(|dir| dir.name.as_str()).collect();
            format!(
                "there is no theme '{name}' in the chain {}",
                chain.join(" → ")
            )
        }
        PathForm::Theme { name, .. } => format!("looked in the directory of theme '{name}'"),
        PathForm::Asset => "looked in the assets/ directories of the theme chain".into(),
    }
}

/// The content version of a file in serve-mode URLs (`?v=<version>`; live-reload.md §4,
/// audit F03): the first 8 bytes of the BLAKE3 hash of `bytes`, as a decimal number. It
/// changes whenever the file's bytes do, so a browser that swaps a stylesheet or the slides
/// in place loads an edited image instead of reusing the one it has for the old URL; and it
/// is digits only, which the live-reload client disregards when it compares pages.
pub(super) fn content_version(bytes: &[u8]) -> u64 {
    let hash = blake3::hash(bytes);
    let mut first = [0u8; 8];
    first.copy_from_slice(&hash.as_bytes()[..8]);
    u64::from_le_bytes(first)
}

/// The dev server URL a page or stylesheet uses for the file at the normalized helper path
/// `key`: [`serve_url`], plus `?v=<`[`content_version`]`>` for a file of the theme chain
/// (`@<theme>/…` or the `assets/` form; the files of the embedded `vendor/…` and
/// `presenter/…` trees do not change while presenter runs). `Ok(None)` when no file is
/// there; errors only when an existing file cannot be read.
///
/// The version of a file on disk is remembered with the file's size and modification
/// time, and computed again only when they change ([`VERSIONS`]): a rebuild stats the
/// files it names instead of reading and hashing them, however often a template names one
/// (a logo on every slide). An edit that keeps both (within the file system's timestamp
/// granularity) keeps the old version, as for deck assets.
pub(super) fn versioned_serve_url(theme: &Theme, key: &str) -> Result<Option<String>> {
    if is_builtin(key) {
        return Ok(helper_path_exists(theme, key).then(|| serve_url(key)));
    }
    let Some((dir, rel)) = theme_file(theme, key) else {
        return Ok(None);
    };
    let version = match &dir.source {
        ThemeSource::Disk { dir: root } => disk_version(dir, &root.join(&rel))?,
        // Debug builds read the built-in themes from disk: never remembered.
        ThemeSource::Embedded { .. } => dir.read(&rel)?.map(|bytes| content_version(&bytes)),
    };
    Ok(version.map(|version| format!("{}?v={version}", serve_url(key))))
}

/// The theme of the chain that has the file of the theme-chain helper path `key` (the
/// `@<theme>/…` or the `assets/` form, [`resolve_helper_path`]), with the file's path
/// relative to that theme's root, normalized.
fn theme_file<'t>(theme: &'t Theme, key: &str) -> Option<(&'t ThemeDir, String)> {
    let (dir, rel) = match path_form(key) {
        PathForm::Builtin => return None,
        PathForm::Theme { name, rel } => (theme.dir(name)?, rel.to_string()),
        PathForm::Asset => {
            let rel = format!("assets/{}", normalize_helper_path(key)?);
            (theme.chain.iter().find(|dir| dir.exists(&rel))?, rel)
        }
    };
    let rel = normalize_helper_path(&rel)?;
    dir.exists(&rel).then_some((dir, rel))
}

/// A file's size and modification time, with its [`content_version`] ([`VERSIONS`]).
type Stamped = (u64, SystemTime, u64);

/// The content versions of the theme files on disk that serve-mode URLs named, by path
/// ([`versioned_serve_url`]).
static VERSIONS: Mutex<BTreeMap<PathBuf, Stamped>> = Mutex::new(BTreeMap::new());

/// The [`content_version`] of the file at `path` of the theme `dir`: remembered while its
/// size and modification time stay the same. `Ok(None)` if it is gone.
fn disk_version(dir: &ThemeDir, path: &Path) -> Result<Option<u64>> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(None);
    };
    let stamp = meta.modified().ok().map(|time| (meta.len(), time));
    let versions = || VERSIONS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((len, time)) = stamp
        && let Some(&(old_len, old_time, version)) = versions().get(path)
        && (old_len, old_time) == (len, time)
    {
        return Ok(Some(version));
    }
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(Error::Theme {
                name: dir.name.clone(),
                message: format!("cannot read {}: {err}", path.display()),
            });
        }
    };
    let version = content_version(&bytes);
    // Stamped before reading: a write in between gives a new stamp next time.
    if let Some((len, time)) = stamp {
        versions().insert(path.to_path_buf(), (len, time, version));
    }
    Ok(Some(version))
}

/// The dev server URL of helper path `key`: [`SERVE_HELPER_PREFIX`] + `key`, with every byte
/// other than ASCII letters, digits and `-._~/@` percent-encoded.
pub(super) fn serve_url(key: &str) -> String {
    let mut url = String::from(SERVE_HELPER_PREFIX);
    for byte in key.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/@".contains(&byte) {
            url.push(char::from(byte));
        } else {
            let _ = write!(url, "%{byte:02X}");
        }
    }
    url
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn path_forms() {
        assert!(matches!(path_form("vendor/a.js"), PathForm::Builtin));
        assert!(matches!(path_form("presenter/math.js"), PathForm::Builtin));
        assert!(matches!(path_form("vendor"), PathForm::Asset));
        assert!(matches!(
            path_form("@default/styles/theme.css"),
            PathForm::Theme {
                name: "default",
                rel: "styles/theme.css"
            }
        ));
        assert!(matches!(path_form("img/logo.png"), PathForm::Asset));
    }

    #[test]
    fn relative_references_join_like_urls() {
        let cases = [
            (
                "vendor/katex/katex.min.css",
                "fonts/a.woff2",
                Some("vendor/katex/fonts/a.woff2"),
            ),
            // `/__presenter/asset/vendor/katex/x.css` + `../../a.png` → `/__presenter/asset/a.png`
            ("vendor/katex/katex.min.css", "../../a.png", Some("a.png")),
            ("vendor/katex/katex.min.css", "../../../a.png", None),
            (
                "@t/styles/theme.css",
                "../assets/bg.png",
                Some("@t/assets/bg.png"),
            ),
            ("@t/styles/theme.css", "../../x.png", Some("x.png")),
            ("@t/theme.css", "x.png", Some("@t/x.png")),
            ("css/a.css", "../img/b.png", Some("img/b.png")),
            (
                "css/a.css",
                "../vendor/reveal/dist/reveal.js",
                Some("vendor/reveal/dist/reveal.js"),
            ),
            ("css/a.css", "img\\b.png", Some("css/img/b.png")),
            ("a.css", "./b.png", Some("b.png")),
            ("a.css", "..", None),
            ("a.css", ".", None),
        ];
        for (base, rel, expected) in cases {
            assert_eq!(
                join_relative(base, rel).as_deref(),
                expected,
                "{base} + {rel}"
            );
        }
    }

    #[test]
    fn theme_files_get_a_content_version_and_builtin_files_none() {
        let theme = crate::theme::resolve("default", &[]).unwrap();
        let css = "@default/styles/theme.css";
        let bytes = theme.chain[0].read("styles/theme.css").unwrap().unwrap();
        assert_eq!(
            versioned_serve_url(&theme, css).unwrap(),
            Some(format!(
                "/__presenter/asset/{css}?v={}",
                content_version(&bytes)
            ))
        );
        let reveal = "vendor/reveal/dist/reveal.js";
        assert_eq!(
            versioned_serve_url(&theme, reveal).unwrap(),
            Some(format!("/__presenter/asset/{reveal}"))
        );
        assert_eq!(versioned_serve_url(&theme, "nope.png").unwrap(), None);
        assert_eq!(versioned_serve_url(&theme, "vendor/nope.js").unwrap(), None);
        assert_ne!(content_version(b"a"), content_version(b"b"));
        assert_eq!(content_version(b"a"), content_version(b"a"));
    }

    #[test]
    fn disk_versions_are_remembered_while_size_and_time_stay() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("v");
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(
            dir.join("theme.toml"),
            "name = \"v\"\nextends = \"default\"\n",
        )
        .unwrap();
        let file = dir.join("assets/a.png");
        std::fs::write(&file, b"aaaa").unwrap();
        let theme = crate::theme::resolve(dir.to_str().unwrap(), &[]).unwrap();
        let url = |key: &str| versioned_serve_url(&theme, key).unwrap();
        let first = format!("/__presenter/asset/a.png?v={}", content_version(b"aaaa"));
        assert_eq!(url("a.png"), Some(first.clone()));
        assert_eq!(
            url("@v/assets/a.png"),
            Some(format!(
                "/__presenter/asset/@v/assets/a.png?v={}",
                content_version(b"aaaa")
            ))
        );
        let set_time = |time| {
            let handle = std::fs::File::options().write(true).open(&file).unwrap();
            handle.set_modified(time).unwrap();
        };
        let time = std::fs::metadata(&file).unwrap().modified().unwrap();
        // Same size and time, other bytes: the remembered version (the file is not read).
        std::fs::write(&file, b"bbbb").unwrap();
        set_time(time);
        assert_eq!(url("a.png"), Some(first));
        // Another time: read again.
        set_time(time + Duration::from_secs(2));
        let second = format!("/__presenter/asset/a.png?v={}", content_version(b"bbbb"));
        assert_eq!(url("a.png"), Some(second));
        std::fs::remove_file(&file).unwrap();
        assert_eq!(url("a.png"), None);
    }

    #[test]
    fn serve_urls_are_percent_encoded() {
        assert_eq!(
            serve_url("@my theme/scripts/x+y.js"),
            "/__presenter/asset/@my%20theme/scripts/x%2By.js"
        );
        assert_eq!(
            serve_url("vendor/a-b_c.~d"),
            "/__presenter/asset/vendor/a-b_c.~d"
        );
        assert_eq!(serve_url("asset:x"), "/__presenter/asset/asset%3Ax");
    }
}
