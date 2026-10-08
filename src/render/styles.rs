//! Stylesheets: the compiled theme stylesheet (`docs/spec/theme-format.md` §5) and the
//! relative `url()`s of every stylesheet the renderer processes.
//!
//! **`url()` resolution.** A relative `url(rel)` in the stylesheet at helper path `key`
//! names the helper path `rel` joined to `key`'s directory ([`super::paths`]): the file a
//! browser loads in serve mode, where the stylesheet is served at
//! `/__presenter/asset/<key>`. So a stylesheet finds the same files in every output mode.
//! Per stylesheet:
//! - theme `[styles] entries` (compiled into the theme stylesheet): next to the
//!   stylesheet, then (§5) `rel` in the chain's `assets/` directories (`rel` read as an
//!   assets-form helper path, so `url(vendor/x.png)` never falls back); every reference
//!   must resolve. The theme stylesheet is processed by the renderer in every mode, so
//!   the fallback works in serve mode too;
//! - `style(path)` of a theme file (`@<theme>/…` or the `assets/` form): next to the
//!   stylesheet only, as a browser resolves it; every reference must resolve (checked in
//!   serve mode too, although the browser loads the files there);
//! - `style(path)` of `vendor/…`/`presenter/…`: next to the stylesheet; unresolvable
//!   references stay as written (KaTeX's `.woff`/`.ttf` fallbacks, vendored-assets.md §2).
//!
//! What a resolved reference becomes ([`Targets`]): an `asset:<id>` placeholder of a
//! registry that receives the file's bytes (standalone and site; the emitter inlines or
//! emits it), or in serve mode the dev server URL of its helper path,
//! `/__presenter/asset/<path>` (nothing is copied; the asset route serves it from the
//! theme chain on each request, like every other helper path).
//!
//! Text files are decoded as UTF-8 with one leading byte order mark removed
//! ([`decode_text`]): a BOM is only skipped at the very start of a stylesheet, so one that
//! ends up after the `/* @theme/path */` comment of the compiled stylesheet, or inside a
//! `<style>` element, would become part of the first selector and drop that rule.

use std::fmt::Write as _;
use std::string::FromUtf8Error;
use std::sync::LazyLock;

use regex::Regex;

use super::paths::{
    PathForm, helper_path_exists, is_builtin, join_relative, normalize_helper_path, path_form,
    resolve_helper_path, serve_url,
};
use crate::assets::emit::register_css_urls;
use crate::assets::registry::{AssetData, AssetRegistry};
use crate::error::{Error, Result};
use crate::theme::Theme;

/// How the relative `url()`s of one stylesheet are resolved (see the module docs).
#[derive(Debug, Clone, Copy)]
pub(super) struct UrlRules {
    /// Leave unresolvable references as written instead of failing.
    lenient: bool,
    /// Also look in the chain's `assets/` directories (theme `[styles] entries`, §5).
    assets_fallback: bool,
}

impl UrlRules {
    /// The rules for a theme `[styles] entries` file.
    const THEME_ENTRY: UrlRules = UrlRules {
        lenient: false,
        assets_fallback: true,
    };

    /// The rules for `style(key)`: next to the file only; lenient for the embedded tree.
    pub(super) fn for_style(key: &str) -> UrlRules {
        UrlRules {
            lenient: is_builtin(key),
            assets_fallback: false,
        }
    }

    /// True when unresolvable references stay as written.
    pub(super) fn is_lenient(self) -> bool {
        self.lenient
    }
}

/// What resolved `url()` references become.
pub(super) enum Targets<'r> {
    /// `asset:<id>` placeholders of this registry, which receives the files' bytes.
    Placeholders(&'r mut AssetRegistry),
    /// `/__presenter/asset/<helper path>` URLs; nothing is read.
    ServeUrls,
}

impl Targets<'_> {
    /// A shorter-lived copy, so one `Targets` can be used for several stylesheets.
    fn reborrow(&mut self) -> Targets<'_> {
        match self {
            Targets::Placeholders(registry) => Targets::Placeholders(registry),
            Targets::ServeUrls => Targets::ServeUrls,
        }
    }
}

/// Compiles the theme stylesheet (theme-format.md §5): every `[styles] entries` file of the
/// chain, parent first, concatenated (each preceded by a `/* @<theme>/<path> */` comment).
/// Relative `url()`s are resolved against the stylesheet's own directory, then the chain's
/// `assets/` directories (see the module docs), registered in `registry` as bytes and
/// rewritten to `asset:<id>` placeholders (the caller resolves them for its output mode).
///
/// Errors: [`Error::Style`] (`file` = the entry as a normalized helper path,
/// `@<theme>/<path>`) for a `.scss`/`.sass` entry ("SCSS support arrives in T42"), another
/// extension, a file that is not UTF-8 or a `url()` that resolves to nothing;
/// [`Error::Theme`] for an entry that does not exist.
pub fn compile_theme_css(theme: &Theme, registry: &mut AssetRegistry) -> Result<String> {
    compile(theme, Targets::Placeholders(registry), &mut Vec::new())
}

/// [`compile_theme_css`] with the `url()`s turned into `targets`. Every absolute `@import`
/// of an entry ([`remote_imports`]) is appended to `remote` as `(entry key, URL)`.
pub(super) fn compile(
    theme: &Theme,
    mut targets: Targets<'_>,
    remote: &mut Vec<(String, String)>,
) -> Result<String> {
    let mut css = String::new();
    for entry in theme.style_entries()? {
        let written = format!("@{}/{}", entry.theme, entry.rel_path);
        // Normalized like every helper path, so a `\`-separated entry joins correctly.
        let key = normalize_helper_path(&written).unwrap_or(written);
        let text = entry_text(&key, &entry.rel_path, entry.bytes)?;
        remote.extend(
            remote_imports(&text)
                .into_iter()
                .map(|url| (key.clone(), url)),
        );
        let text = rewrite_urls(
            theme,
            &key,
            &text,
            UrlRules::THEME_ENTRY,
            targets.reborrow(),
        )?;
        let _ = writeln!(css, "/* {key} */");
        css.push_str(&text);
        if !css.ends_with('\n') {
            css.push('\n');
        }
    }
    Ok(css)
}

/// The text of the theme stylesheet entry `key` (written as `rel_path` in its manifest).
fn entry_text(key: &str, rel_path: &str, bytes: Vec<u8>) -> Result<String> {
    let style_error = |message: String| Error::Style {
        file: key.to_string(),
        message,
    };
    let lower = rel_path.to_ascii_lowercase();
    if lower.ends_with(".scss") || lower.ends_with(".sass") {
        return Err(style_error("SCSS support arrives in T42".into()));
    }
    if !lower.ends_with(".css") {
        return Err(style_error("a style entry must be a .css file".into()));
    }
    decode_text(bytes).map_err(|e| style_error(format!("not valid UTF-8: {e}")))
}

/// `bytes` as UTF-8 text without a leading byte order mark (see the module docs).
pub(super) fn decode_text(bytes: Vec<u8>) -> std::result::Result<String, FromUtf8Error> {
    let mut text = String::from_utf8(bytes)?;
    if text.starts_with('\u{feff}') {
        text.drain(..'\u{feff}'.len_utf8());
    }
    Ok(text)
}

/// Rewrites the relative `url()`s of the stylesheet `css` at helper path `key` following
/// `rules` (see the module docs).
///
/// Errors: [`Error::Style`] naming `key` for a reference that resolves to nothing (unless
/// lenient); a read error of an existing file.
pub(super) fn rewrite_urls(
    theme: &Theme,
    key: &str,
    css: &str,
    rules: UrlRules,
    targets: Targets<'_>,
) -> Result<String> {
    match targets {
        Targets::Placeholders(registry) => {
            let mut fetch =
                |path: &str| Ok(resolve_helper_path(theme, path)?.map(|(bytes, _)| bytes));
            register_urls(key, css, rules, registry, &mut fetch)
        }
        Targets::ServeUrls => {
            // A scratch registry is the id → helper path table: each target is registered
            // with its helper path as its "bytes" (ids are content hashes, so one path is
            // one id), and the placeholders are then replaced by the paths' URLs.
            let mut table = AssetRegistry::new();
            let mut fetch =
                |path: &str| Ok(helper_path_exists(theme, path).then(|| path.as_bytes().to_vec()));
            let mut css = register_urls(key, css, rules, &mut table, &mut fetch)?;
            for asset in table.iter() {
                if let AssetData::Bytes(path) = &asset.data {
                    let url = serve_url(&String::from_utf8_lossy(path));
                    css = css.replace(&AssetRegistry::placeholder(&asset.id), &url);
                }
            }
            Ok(css)
        }
    }
}

/// [`register_css_urls`] with a resolver that tries the candidate helper paths of each
/// reference in order and registers what `fetch` returns for the first one it finds.
fn register_urls(
    key: &str,
    css: &str,
    rules: UrlRules,
    registry: &mut AssetRegistry,
    fetch: &mut dyn FnMut(&str) -> Result<Option<Vec<u8>>>,
) -> Result<String> {
    let mut failure: Option<Error> = None;
    let result = {
        let mut resolve = |rel: &str| -> Option<AssetData> {
            for candidate in candidates(key, rel, rules) {
                match fetch(&candidate) {
                    Ok(Some(bytes)) => return Some(AssetData::Bytes(bytes)),
                    Ok(None) => {}
                    Err(err) => {
                        failure.get_or_insert(err);
                        return None;
                    }
                }
            }
            None
        };
        register_css_urls(css, registry, &mut resolve, rules.lenient)
    };
    if let Some(err) = failure {
        return Err(err);
    }
    result.map_err(|err| match err {
        Error::MissingAsset { path, .. } => Error::Style {
            file: key.to_string(),
            message: format!(
                "url({}) not found next to the stylesheet{}",
                path.display(),
                if rules.assets_fallback {
                    " or in the theme chain's assets/ directories"
                } else {
                    ""
                }
            ),
        },
        other => other,
    })
}

/// The helper paths a reference `rel` in the stylesheet `key` may name, in lookup order.
fn candidates(key: &str, rel: &str, rules: UrlRules) -> Vec<String> {
    let mut paths: Vec<String> = join_relative(key, rel).into_iter().collect();
    if rules.assets_fallback
        && let Some(path) = normalize_helper_path(rel)
        && matches!(path_form(&path), PathForm::Asset)
        && !paths.contains(&path)
    {
        paths.push(path);
    }
    paths
}

/// Absolute targets of `@import` rules (`https://…`, `http://…` or `//…`, quoted or in
/// `url()`), in order. Comments are not skipped (a commented-out import is reported too).
pub(super) fn remote_imports(css: &str) -> Vec<String> {
    static REMOTE_IMPORT: LazyLock<Regex> = LazyLock::new(|| {
        // A constant pattern covered by the unit tests: compiling it cannot fail.
        Regex::new(r#"(?i)@import\s+(?:url\(\s*)?["']?\s*((?:https?:)?//[^"'\s);]+)"#)
            .expect("remote import regex is valid")
    });
    REMOTE_IMPORT
        .captures_iter(css)
        .map(|caps| caps[1].to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_imports_are_found_in_every_form() {
        let css = r#"@import "https://fonts.example/a?family=X:400,700";
@IMPORT url( 'http://x.example/b.css' ) screen;
@import url(//cdn.example/c.css);
@import "local.css";
@import url(fonts/d.css);
a { background: url(https://x.example/not-an-import.png) }"#;
        assert_eq!(
            remote_imports(css),
            [
                "https://fonts.example/a?family=X:400,700",
                "http://x.example/b.css",
                "//cdn.example/c.css"
            ]
        );
    }

    #[test]
    fn decode_text_drops_one_leading_bom() {
        assert_eq!(decode_text(b"\xEF\xBB\xBFa{}".to_vec()).unwrap(), "a{}");
        assert_eq!(
            decode_text(b"\xEF\xBB\xBF\xEF\xBB\xBFa".to_vec()).unwrap(),
            "\u{feff}a"
        );
        assert_eq!(decode_text(b"a\xEF\xBB\xBF".to_vec()).unwrap(), "a\u{feff}");
        assert!(decode_text(vec![0xff]).is_err());
    }

    #[test]
    fn candidates_follow_the_rules() {
        let entry = UrlRules::THEME_ENTRY;
        assert_eq!(
            candidates("@t/styles/a.css", "logo.png", entry),
            ["@t/styles/logo.png", "logo.png"]
        );
        // Only an assets-form `rel` falls back.
        assert_eq!(
            candidates("@t/styles/a.css", "vendor/x.png", entry),
            ["@t/styles/vendor/x.png"]
        );
        assert_eq!(
            candidates("@t/a.css", "x.png", entry),
            ["@t/x.png", "x.png"]
        );
        let style = UrlRules::for_style("@t/styles/a.css");
        assert!(!style.is_lenient());
        assert_eq!(
            candidates("@t/styles/a.css", "logo.png", style),
            ["@t/styles/logo.png"]
        );
        assert!(UrlRules::for_style("vendor/katex/katex.min.css").is_lenient());
    }
}
