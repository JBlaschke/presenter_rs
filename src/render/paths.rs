//! Helper paths (`docs/spec/theme-format.md` §4): the strings `style()`, `script()` and
//! `asset()` take, and the `{*path}` of the dev server's `/__presenter/asset/{*path}` route.
//!
//! A helper path is normalized first (`/` and `\` separate segments, `.` and empty segments
//! are dropped, `..` removes the previous segment; a path that climbs out finds nothing),
//! then resolved by its first segment: `vendor/…` and `presenter/…` in the embedded tree,
//! `@<theme>/<path>` relative to that theme's root directory, anything else in the chain's
//! `assets/` directories.

use std::borrow::Cow;
use std::fmt::Write as _;

use super::html::SERVE_HELPER_PREFIX;
use crate::error::Result;
use crate::theme::{Theme, normalize_rel};

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

/// `path` normalized as in [`resolve_helper_path`]; `None` if it is empty or climbs out.
pub fn normalize_helper_path(path: &str) -> Option<String> {
    normalize_rel(path).filter(|key| !key.is_empty())
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
    normalize_helper_path(&format!("{dir}/{rel}"))
}

/// Why `path` (as given to a helper) resolves to nothing, for "unknown path" messages.
pub(super) fn unknown_reason(theme: &Theme, path: &str) -> String {
    let Some(key) = normalize_rel(path) else {
        return "it climbs out of its root directory with '..'".into();
    };
    if key.is_empty() {
        return "the path is empty".into();
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
