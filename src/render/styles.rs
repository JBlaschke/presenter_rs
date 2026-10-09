//! Stylesheets: the compiled theme stylesheet (`docs/spec/theme-format.md` §5) and the
//! relative `url()`s of every stylesheet the renderer processes.
//!
//! **`url()` resolution.** A relative `url(rel)` in the stylesheet at helper path `key`
//! names the helper path `rel` joined to `key`'s directory ([`super::paths`]): the file a
//! browser loads in serve mode, where the stylesheet is served at
//! `/__presenter/asset/<key>`. So a stylesheet finds the same files in every output mode.
//! Per stylesheet:
//! - theme `[styles] entries` (compiled into the theme stylesheet): next to the
//!   stylesheet, then (§5) `assets/<rel>` in the chain's themes, child first (the lookup of
//!   [`Theme::read_asset`], for any `rel`: `url(vendor/x.png)` finds `assets/vendor/x.png`
//!   too), and last, for a `vendor/…` or `presenter/…` `rel`, the embedded file of that
//!   builtin helper path (§3: "`vendor/...` names come from the embedded vendor tree"; a
//!   theme's own `assets/vendor/…` file wins); every reference must resolve. A chain hit is
//!   named by its theme-root helper path `@<theme>/assets/<rel>`, which names that one file
//!   whatever `rel` starts with (as a helper path, `vendor/x.png` alone names the embedded
//!   tree). Last of all, a `reveal/…` or `fonts/…` `rel` names reveal.js's theme sources
//!   ([`reveal_fallback`], T42). The theme stylesheet is processed by the renderer in every
//!   mode, so the fallback works in serve mode too;
//! - a stylesheet inlined by a local `@import` of an entry (T42,
//!   [`crate::theme::styles::inline_imports`]): against its own helper path; a theme file
//!   with the rules of an entry, an embedded file (reveal's font CSS, KaTeX's CSS) leniently
//!   like `style()` of a vendor stylesheet. Each stylesheet's own text is processed once,
//!   before anything is spliced into it, so what a lenient file leaves as written stays so;
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
//! theme chain on each request, like every other helper path), with `?v=<content version>`
//! for a file of the theme chain (live-reload.md §4, audit F03: the browser reuses the
//! image it has for a URL, so an edited one needs a new URL). A query the reference has
//! itself follows it: `url(logo.png?x=1#y)` becomes
//! `url(/__presenter/asset/@t/assets/logo.png?v=<version>&x=1#y)`.
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
    is_builtin, join_relative, normalize_helper_path, resolve_helper_path, versioned_serve_url,
};
use crate::assets::emit::register_css_urls;
use crate::assets::registry::{AssetData, AssetRegistry};
use crate::error::{Error, Result};
use crate::theme::Theme;
use crate::theme::styles::{compile_entry, inline_imports, reveal_fallback};

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

    /// The rules for the stylesheet `key` of a theme entry's text (T42): the entry's own
    /// and those of a theme file it imports are [`UrlRules::THEME_ENTRY`], an embedded
    /// file's (imported) are those of `style(key)` (lenient).
    fn for_entry_text(key: &str) -> UrlRules {
        if is_builtin(key) {
            UrlRules::for_style(key)
        } else {
            UrlRules::THEME_ENTRY
        }
    }

    /// True when unresolvable references stay as written.
    pub(super) fn is_lenient(self) -> bool {
        self.lenient
    }

    /// The same rules, leaving unresolvable references as written.
    pub(super) fn leniently(self) -> UrlRules {
        UrlRules {
            lenient: true,
            ..self
        }
    }
}

/// What resolved `url()` references become.
pub(super) enum Targets<'r> {
    /// `asset:<id>` placeholders of this registry, which receives the files' bytes.
    Placeholders(&'r mut AssetRegistry),
    /// `/__presenter/asset/<helper path>` URLs, versioned for theme files (module docs);
    /// nothing is copied.
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
/// Each entry is first compiled by [`crate::theme::styles::compile_entry`] (`.css` as
/// written, `.scss`/`.sass` with grass, T42), then its local `@import`s are inlined
/// ([`crate::theme::styles::inline_imports`]); the `url()`s of each stylesheet's own text
/// are resolved against its own helper path before anything is spliced into it (the entry
/// and theme files with the rules of an entry, embedded files leniently, like `style()` of
/// a vendor stylesheet).
///
/// Errors: [`Error::Style`] (`file` = the entry as a normalized helper path,
/// `@<theme>/<path>`, or for a Sass error the file it is in) for another extension, a file
/// that is not UTF-8, a Sass error, a local `@import` that cannot be inlined or a `url()`
/// that resolves to nothing; [`Error::Theme`] for an entry that does not exist.
pub fn compile_theme_css(theme: &Theme, registry: &mut AssetRegistry) -> Result<String> {
    compile(theme, Targets::Placeholders(registry), &mut Vec::new())
}

/// [`compile_theme_css`] with the `url()`s turned into `targets`. Every absolute `@import`
/// of an entry or of a stylesheet inlined into it ([`remote_imports`]) is appended to
/// `remote` as `(entry key, URL)`.
///
/// The top-level `@import` rules left after inlining (absolute URLs,
/// [`crate::theme::styles::Inlined::imports`]) are moved to the top of the stylesheet, in
/// entry order, each after a `/* hoisted from <stylesheet> */` comment naming the file
/// that has it (T42; the entries keep their own `/* <entry key> */` comments): CSS ignores
/// an `@import` that follows other rules, as every import of an entry after the first, or
/// of an inlined file, would otherwise do. A rule from a file inlined by an import with
/// media queries carries them.
pub(super) fn compile(
    theme: &Theme,
    mut targets: Targets<'_>,
    remote: &mut Vec<(String, String)>,
) -> Result<String> {
    let mut css = String::new();
    let mut imports = Vec::new();
    for entry in theme.style_entries()? {
        let written = format!("@{}/{}", entry.theme, entry.rel_path);
        // Normalized like every helper path, so a `\`-separated entry joins correctly.
        let key = normalize_helper_path(&written).unwrap_or(written);
        let text = compile_entry(&entry, theme)?;
        let inlined = inline_imports(theme, &key, &text, &mut |file, css| {
            let rules = UrlRules::for_entry_text(file);
            rewrite_urls(theme, file, &css, rules, targets.reborrow())
        })?;
        let rules = inlined.imports.iter().map(|(_, rule)| rule.as_str());
        for text in rules.chain([inlined.css.as_str()]) {
            remote.extend(
                remote_imports(text)
                    .into_iter()
                    .map(|url| (key.clone(), url)),
            );
        }
        // The imports taken out go first: after other rules CSS ignores them.
        imports.extend(inlined.imports);
        let _ = writeln!(css, "/* {key} */");
        css.push_str(&inlined.css);
        if !css.ends_with('\n') {
            css.push('\n');
        }
    }
    let mut head = String::new();
    for (file, rule) in imports {
        let _ = writeln!(head, "/* hoisted from {file} */\n{rule}");
    }
    Ok(head + &css)
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
    let chain: Vec<&str> = theme.chain.iter().map(|dir| dir.name.as_str()).collect();
    let lookup = Lookup {
        key,
        rules,
        chain: &chain,
    };
    match targets {
        Targets::Placeholders(registry) => {
            let mut fetch =
                |path: &str| Ok(resolve_helper_path(theme, path)?.map(|(bytes, _)| bytes));
            // Registered in a scratch registry first, then moved into `registry` with the
            // standard font MIME types (T42; ids hash the bytes only, so the placeholders
            // stay valid).
            let mut found = AssetRegistry::new();
            let css = register_urls(lookup, css, &mut found, &mut fetch)?;
            for asset in found.iter() {
                match &asset.data {
                    AssetData::Bytes(bytes) => {
                        let mime = standard_font_mime(&asset.mime);
                        registry.register_bytes(bytes.clone(), mime, &asset.file_name_hint)
                    }
                    AssetData::File(path) => registry.register_file(path),
                };
            }
            Ok(css)
        }
        Targets::ServeUrls => {
            // A scratch registry is the id → URL table: each target is registered with its
            // versioned URL as its "bytes" (ids are content hashes, so one URL is one id),
            // and the placeholders are then replaced by the URLs.
            let mut table = AssetRegistry::new();
            let mut fetch =
                |path: &str| Ok(versioned_serve_url(theme, path)?.map(String::into_bytes));
            let mut css = register_urls(lookup, css, &mut table, &mut fetch)?;
            for asset in table.iter() {
                if let AssetData::Bytes(url) = &asset.data {
                    let placeholder = AssetRegistry::placeholder(&asset.id);
                    css = replace_placeholder(&css, &placeholder, &String::from_utf8_lossy(url));
                }
            }
            Ok(css)
        }
    }
}

/// `css` with every `placeholder` replaced by `url`. When `url` has a query and the
/// reference its own (the suffix after the placeholder, `?x=1#y`), they are joined:
/// `<url>&x=1#y`.
fn replace_placeholder(css: &str, placeholder: &str, url: &str) -> String {
    let mut out = String::with_capacity(css.len() + url.len());
    let mut rest = css;
    while let Some(at) = rest.find(placeholder) {
        out.push_str(&rest[..at]);
        out.push_str(url);
        rest = &rest[at + placeholder.len()..];
        if url.contains('?')
            && let Some(query) = rest.strip_prefix('?')
        {
            out.push('&');
            rest = query;
        }
    }
    out.push_str(rest);
    out
}

/// The RFC 8081 type for the legacy font MIME types `mime_guess` reports
/// (`application/font-woff` for `.woff`, `application/font-sfnt` for `.otf`), so inlined
/// reveal.js theme fonts are `data:font/woff` like in reveal's own `dist/theme/*.css`
/// (vendored-assets.md §2); any other type unchanged.
fn standard_font_mime(mime: &str) -> &str {
    match mime {
        "application/font-woff" => "font/woff",
        "application/font-sfnt" => "font/otf",
        other => other,
    }
}

/// Where the references of one stylesheet are looked up.
#[derive(Clone, Copy)]
struct Lookup<'l> {
    /// The stylesheet's normalized helper path.
    key: &'l str,
    /// Its rules.
    rules: UrlRules,
    /// The names of the chain's themes, child first.
    chain: &'l [&'l str],
}

impl Lookup<'_> {
    /// The helper paths the reference `rel` may name, in lookup order: `rel` joined to the
    /// stylesheet's directory, then (with the `assets/` fallback) `@<theme>/assets/<rel>`
    /// for each theme of the chain, child first, and last, for a `vendor/…` or
    /// `presenter/…` `rel`, that builtin helper path (theme-format.md §3: such names come
    /// from the embedded tree; a theme's own `assets/vendor/…` file comes first).
    fn candidates(self, rel: &str) -> Vec<String> {
        let mut paths: Vec<String> = join_relative(self.key, rel).into_iter().collect();
        let fallback = self
            .rules
            .assets_fallback
            .then(|| normalize_helper_path(rel))
            .flatten();
        if let Some(rel) = fallback {
            let builtin = is_builtin(&rel).then(|| rel.clone());
            // `reveal/…` and `fonts/…` among reveal.js's theme sources (T42).
            let reveal = reveal_fallback(&rel);
            let chain = self
                .chain
                .iter()
                .map(|name| format!("@{name}/assets/{rel}"));
            for path in chain.chain(builtin).chain(reveal) {
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }
        paths
    }

    /// Where a reference `rel` that resolved to nothing was looked for, for the error.
    fn looked_in(self, rel: &str) -> &'static str {
        if !self.rules.assets_fallback {
            ""
        } else if normalize_helper_path(rel).is_some_and(|rel| is_builtin(&rel)) {
            ", in the theme chain's assets/ directories or among the built-in files"
        } else if reveal_fallback(rel).is_some() {
            ", in the theme chain's assets/ directories or among reveal.js's theme sources"
        } else {
            " or in the theme chain's assets/ directories"
        }
    }
}

/// [`register_css_urls`] with a resolver that tries the candidate helper paths of each
/// reference in order and registers what `fetch` returns for the first one it finds.
fn register_urls(
    lookup: Lookup<'_>,
    css: &str,
    registry: &mut AssetRegistry,
    fetch: &mut dyn FnMut(&str) -> Result<Option<Vec<u8>>>,
) -> Result<String> {
    let mut failure: Option<Error> = None;
    let result = {
        let mut resolve = |rel: &str| -> Option<AssetData> {
            for candidate in lookup.candidates(rel) {
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
        register_css_urls(css, registry, &mut resolve, lookup.rules.lenient)
    };
    if let Some(err) = failure {
        return Err(err);
    }
    result.map_err(|err| match err {
        Error::MissingAsset { path, .. } => Error::Style {
            file: lookup.key.to_string(),
            message: format!(
                "url({}) not found next to the stylesheet{}",
                path.display(),
                lookup.looked_in(&path.to_string_lossy())
            ),
        },
        other => other,
    })
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
    fn placeholders_take_the_versioned_url_and_keep_their_own_query() {
        let placeholder = "asset:0123456789ab";
        let css = format!(
            "a {{ b: url({placeholder}); c: url('{placeholder}?x=1#y'); d: url({placeholder}#z) }}"
        );
        assert_eq!(
            replace_placeholder(&css, placeholder, "/u.png?v=5"),
            "a { b: url(/u.png?v=5); c: url('/u.png?v=5&x=1#y'); d: url(/u.png?v=5#z) }"
        );
        // A URL without a query (a builtin file) keeps the reference's query as it is.
        assert_eq!(
            replace_placeholder(&css, placeholder, "/v.woff"),
            "a { b: url(/v.woff); c: url('/v.woff?x=1#y'); d: url(/v.woff#z) }"
        );
    }

    #[test]
    fn legacy_font_mime_types_are_standardized() {
        assert_eq!(standard_font_mime("application/font-woff"), "font/woff");
        assert_eq!(standard_font_mime("application/font-sfnt"), "font/otf");
        assert_eq!(standard_font_mime("font/woff2"), "font/woff2");
        assert_eq!(standard_font_mime("image/png"), "image/png");
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
        let chain = ["t", "default"];
        let lookup = |key, rules| Lookup {
            key,
            rules,
            chain: &chain,
        };
        let entry = lookup("@t/styles/a.css", UrlRules::THEME_ENTRY);
        assert_eq!(
            entry.candidates("logo.png"),
            [
                "@t/styles/logo.png",
                "@t/assets/logo.png",
                "@default/assets/logo.png"
            ]
        );
        // Every `rel` falls back, named in the theme-root form whatever its first segment;
        // a builtin-form `rel` then names the embedded file (§3).
        assert_eq!(
            entry.candidates("vendor/x.png"),
            [
                "@t/styles/vendor/x.png",
                "@t/assets/vendor/x.png",
                "@default/assets/vendor/x.png",
                "vendor/x.png"
            ]
        );
        assert_eq!(
            entry.candidates("./presenter/../presenter/y.js"),
            [
                "@t/styles/presenter/y.js",
                "@t/assets/presenter/y.js",
                "@default/assets/presenter/y.js",
                "presenter/y.js"
            ]
        );
        assert_eq!(
            entry.looked_in("logo.png"),
            " or in the theme chain's assets/ directories"
        );
        assert_eq!(
            entry.looked_in("./vendor/x.png"),
            ", in the theme chain's assets/ directories or among the built-in files"
        );
        assert_eq!(
            entry.candidates("./presenter/../@u\\x.png"),
            [
                "@t/styles/@u/x.png",
                "@t/assets/@u/x.png",
                "@default/assets/@u/x.png"
            ]
        );
        // T42: `reveal/…` and `fonts/…` finally name reveal.js's theme sources.
        assert_eq!(
            entry.candidates("./fonts/a/a.woff"),
            [
                "@t/styles/fonts/a/a.woff",
                "@t/assets/fonts/a/a.woff",
                "@default/assets/fonts/a/a.woff",
                "vendor/reveal/css/theme/fonts/a/a.woff"
            ]
        );
        assert_eq!(
            entry
                .candidates("reveal/theme/fonts/a/a.woff")
                .last()
                .unwrap(),
            "vendor/reveal/css/theme/fonts/a/a.woff"
        );
        assert_eq!(
            entry.looked_in("fonts/a.woff"),
            ", in the theme chain's assets/ directories or among reveal.js's theme sources"
        );
        // A `rel` that climbs out of its root has no fallback; duplicates are dropped.
        assert_eq!(entry.candidates("../x.png"), ["@t/x.png"]);
        assert_eq!(
            lookup("@t/assets/a.css", UrlRules::THEME_ENTRY).candidates("x.png"),
            ["@t/assets/x.png", "@default/assets/x.png"]
        );
        let style = UrlRules::for_style("@t/styles/a.css");
        assert!(!style.is_lenient());
        assert_eq!(
            lookup("@t/styles/a.css", style).candidates("logo.png"),
            ["@t/styles/logo.png"]
        );
        assert_eq!(
            lookup("@t/styles/a.css", style).candidates("vendor/x.png"),
            ["@t/styles/vendor/x.png"]
        );
        assert_eq!(
            lookup("@t/styles/a.css", style).looked_in("vendor/x.png"),
            ""
        );
        assert!(UrlRules::for_style("vendor/katex/katex.min.css").is_lenient());
    }
}
