//! Placeholder emission: the single place that turns `asset:<id>` placeholders into URLs.
//!
//! Contract: `docs/spec/ir.md` §3, `docs/spec/live-reload.md` §1, `docs/spec/theme-format.md`
//! §4–§5, `docs/spec/vendored-assets.md` §2. Implemented by T14.
//!
//! An [`Emitter`] rewrites every placeholder of a rendered document or stylesheet according
//! to the [`OutputMode`]:
//!
//! | mode | URL for an asset | bytes |
//! |---|---|---|
//! | [`OutputMode::Standalone`] | `data:<mime>;base64,…` ([`data_uri`]) | read now, inlined |
//! | [`OutputMode::Site`] | `url_prefix` + [`hashed_file_name`], e.g. `assets/plot-3f9a1c2b7e0d.png` | read now, one [`EmittedFile`] per id |
//! | [`OutputMode::Serve`] | `/__presenter/deck-asset/<id>` | not read (the dev server reads them on request) |
//!
//! **Placeholder scanning.** The placeholder grammar is `asset:` followed by exactly 12
//! `[0-9a-f]`; a 13th hex digit (`[0-9a-fA-F]`) means the text is not a placeholder (so a
//! longer hash is never cut to its first 12 chars), any other following character or the end
//! of the text ends it. The emitter scans the *whole* text with that pattern instead of
//! parsing HTML attributes and CSS `url()`s: the grammar is unambiguous, frontends only ever
//! write placeholders into attribute values (`src`, `href`, `srcset`, `poster`,
//! `data-background-*`, inline `style` `url()`) and CSS `url()`s, and text content never
//! legitimately contains `asset:` + 12 hex digits. (A slide that *shows* such a string as
//! text is therefore rewritten or rejected too; that is a documented limitation.) Unique ids
//! are collected first, each is resolved once, then all occurrences are replaced. An id that
//! is not registered is an [`Error::Other`] naming it.
//!
//! **Query strings and fragments.** [`register_css_urls`] keeps a reference's `?query#frag`
//! after the placeholder (`url(asset:<id>?#iefix)`), so site and serve URLs keep it. A
//! `data:` URI cannot carry a query (it would become part of the payload), so in standalone
//! mode a `?query` directly after a placeholder is dropped while a `#fragment` is kept
//! (fragments are valid on `data:` URIs, e.g. for SVG views).
//!
//! **CSS `url()` registration.** [`register_css_urls`] is the step *before* emission for
//! stylesheets: it turns relative `url(...)` references into placeholders, registering what
//! the caller's resolver finds (theme chain or vendor tree lookup is the caller's job).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use base64::Engine as _;
use regex::Regex;

use crate::assets::registry::{AssetData, AssetRegistry, unknown_id};
use crate::error::{Error, Result};

/// URL prefix under which `presenter serve` serves registered deck assets by id
/// (`docs/spec/live-reload.md` §1).
pub const SERVE_ASSET_PREFIX: &str = "/__presenter/deck-asset/";

/// How placeholders become URLs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputMode {
    /// One self-contained file: every asset is inlined as a `data:` URI.
    Standalone,
    /// A directory: assets are emitted as files with content-addressed names.
    Site {
        /// Where emitted files go: each [`EmittedFile::path`] is `assets_dir` joined with the
        /// hashed file name (relative or absolute exactly as given; the caller writes them).
        assets_dir: PathBuf,
        /// Prepended verbatim to the file name to form the URL; normally `"assets/"`, so it
        /// should end with `/`.
        url_prefix: String,
    },
    /// `presenter serve`: assets are served by id from `/__presenter/deck-asset/<id>`.
    Serve,
}

/// A file the caller must write next to the document (site mode only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmittedFile {
    /// `assets_dir.join(hashed_file_name(hint, id))`.
    pub path: PathBuf,
    /// The asset's content.
    pub bytes: Vec<u8>,
}

/// Rewrites placeholders for one output. Keep one emitter per output document so that
/// site mode emits each asset exactly once, however often it is referenced.
#[derive(Debug)]
pub struct Emitter<'a> {
    registry: &'a AssetRegistry,
    mode: &'a OutputMode,
    /// Files emitted so far (site mode; always empty otherwise), one per distinct id, in
    /// order of first use.
    pub files: Vec<EmittedFile>,
    /// Ids already in `files`.
    emitted: BTreeSet<String>,
}

impl<'a> Emitter<'a> {
    /// An emitter resolving ids against `registry` for `mode`.
    pub fn new(registry: &'a AssetRegistry, mode: &'a OutputMode) -> Self {
        Emitter {
            registry,
            mode,
            files: Vec::new(),
            emitted: BTreeSet::new(),
        }
    }

    /// Replace every `asset:<id>` in attribute values and CSS url() of an HTML document.
    ///
    /// The whole document is scanned (see the module docs for why that is sound). Errors:
    /// an unknown id ([`Error::Other`]), a file asset that disappeared
    /// ([`Error::MissingAsset`], standalone and site modes only) or another read failure.
    pub fn resolve_html(&mut self, html: &str) -> Result<String> {
        self.resolve_placeholders(html)
    }

    /// Replace `asset:<id>` inside a CSS text (typically the output of
    /// [`register_css_urls`]). Same scanning and errors as [`Emitter::resolve_html`].
    pub fn resolve_css(&mut self, css: &str) -> Result<String> {
        self.resolve_placeholders(css)
    }

    /// URL for one id in the current mode (data URI / "assets/plot-3f9a1c2b7e0d.png" /
    /// "/__presenter/deck-asset/3f9a1c2b7e0d").
    ///
    /// Site mode pushes an [`EmittedFile`] the first time an id is seen. Serve mode only
    /// checks that the id is registered and reads nothing.
    pub fn url_for(&mut self, id: &str) -> Result<String> {
        let asset = self.registry.get(id).ok_or_else(|| unknown_id(id))?;
        match self.mode {
            OutputMode::Standalone => Ok(data_uri(&asset.mime, &self.registry.read(id)?)),
            OutputMode::Site {
                assets_dir,
                url_prefix,
            } => {
                let name = hashed_file_name(&asset.file_name_hint, id);
                if !self.emitted.contains(id) {
                    let bytes = self.registry.read(id)?.into_owned();
                    tracing::debug!(id, file = %name, len = bytes.len(), "emitting asset file");
                    self.files.push(EmittedFile {
                        path: assets_dir.join(&name),
                        bytes,
                    });
                    self.emitted.insert(id.to_string());
                }
                Ok(format!("{url_prefix}{name}"))
            }
            OutputMode::Serve => Ok(format!("{SERVE_ASSET_PREFIX}{id}")),
        }
    }

    /// Shared implementation of [`Emitter::resolve_html`] and [`Emitter::resolve_css`].
    fn resolve_placeholders(&mut self, text: &str) -> Result<String> {
        let found = find_placeholders(text);
        if found.is_empty() {
            return Ok(text.to_string());
        }
        let mut urls: BTreeMap<&str, String> = BTreeMap::new();
        for placeholder in &found {
            if !urls.contains_key(placeholder.id) {
                let url = self.url_for(placeholder.id)?;
                urls.insert(placeholder.id, url);
            }
        }
        let drop_query = matches!(self.mode, OutputMode::Standalone);
        let mut out =
            String::with_capacity(text.len() + urls.values().map(String::len).sum::<usize>());
        let mut last = 0;
        for placeholder in &found {
            // A placeholder inside a query that was just dropped goes with it.
            if placeholder.start < last {
                continue;
            }
            out.push_str(&text[last..placeholder.start]);
            // Indexing cannot panic: every id of `found` was inserted above.
            out.push_str(&urls[placeholder.id]);
            last = if drop_query {
                placeholder.query_end
            } else {
                placeholder.id_end
            };
        }
        out.push_str(&text[last..]);
        Ok(out)
    }
}

/// `data:<mime>;base64,<bytes>` with the standard base64 alphabet, padded, no line breaks.
/// SVG is base64-encoded as well (simplest, and safe inside any attribute or `url()`).
pub fn data_uri(mime: &str, bytes: &[u8]) -> String {
    let mut uri = String::with_capacity(mime.len() + 13 + bytes.len().div_ceil(3) * 4);
    uri.push_str("data:");
    uri.push_str(mime);
    uri.push_str(";base64,");
    base64::engine::general_purpose::STANDARD.encode_string(bytes, &mut uri);
    uri
}

/// Deterministic, content-addressed site-mode file name: `"plot.png"` + id →
/// `"plot-<id>.png"`; a name without extension → `"<name>-<id>"`.
///
/// Only the last path segment of `hint` is used (`/` and `\` separate), and every character
/// other than ASCII letters, digits, `.`, `-` and `_` becomes `_`, so the result is a safe
/// single file name and needs no URL escaping. The extension is the part after the last `.`;
/// leading and trailing dots of the stem are trimmed, and an empty stem becomes `asset`
/// (`".png"` → `"asset-<id>.png"`, `""` → `"asset-<id>"`).
pub fn hashed_file_name(hint: &str, id: &str) -> String {
    let last_segment = hint.rsplit(['/', '\\']).next().unwrap_or_default();
    let name: String = last_segment
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name.as_str(), ""));
    let stem = match stem.trim_matches('.') {
        "" => "asset",
        stem => stem,
    };
    if ext.is_empty() {
        format!("{stem}-{id}")
    } else {
        format!("{stem}-{id}.{ext}")
    }
}

/// Rewrite relative url(...) references in a CSS file to placeholders, registering them.
/// `resolve_rel` maps a relative ref to an absolute path or bytes (theme chain lookup is
/// supplied by the caller). `lenient = true` leaves unresolvable refs untouched (vendor CSS
/// with woff/ttf fallbacks).
///
/// Details:
/// - Recognized forms: `url(x)`, `url("x")`, `url('x')`, any case of `url`, with optional
///   whitespace inside the parentheses. The rewritten reference keeps its quote style:
///   `url("fonts/a.woff2")` → `url("asset:<id>")`.
/// - Left untouched: references with a scheme (`data:`, `http:`, `https:`, `asset:`, …),
///   protocol-relative `//…`, root-absolute `/…`, fragment-only `#…`, empty references,
///   `url()`s inside `/* comments */`, and `@import url(…)` (imports are not followed here;
///   T42 inlines local imports before calling this function).
/// - `resolve_rel` receives the reference without its `?query#fragment` (not percent-decoded),
///   e.g. `fonts/a.woff2` for `url(fonts/a.woff2?v=1#x)`; the suffix is kept after the
///   placeholder. It is called once per distinct reference.
/// - [`AssetData::File`] is registered with [`AssetRegistry::register_file`];
///   [`AssetData::Bytes`] with the MIME type guessed from the reference's extension and its
///   last path segment as file-name hint.
/// - An unresolvable reference with `lenient = false` is
///   [`Error::MissingAsset`]` { path: <reference without suffix>, span: None }`.
pub fn register_css_urls(
    css: &str,
    registry: &mut AssetRegistry,
    resolve_rel: &mut dyn FnMut(&str) -> Option<AssetData>,
    lenient: bool,
) -> Result<String> {
    let mut out = String::with_capacity(css.len());
    let mut last = 0;
    // Reference path → id (None: unresolvable), so each is resolved once per call.
    let mut seen: BTreeMap<&str, Option<String>> = BTreeMap::new();
    for caps in CSS_URL.captures_iter(css) {
        let Some(whole) = caps.get(0) else { continue };
        // Comments and `@import url(...)` are matched only to be skipped.
        if caps.get(1).is_some() || caps.get(2).is_some() {
            continue;
        }
        let (quote, reference) = match (caps.get(3), caps.get(4), caps.get(5)) {
            (Some(m), _, _) => ("\"", m.as_str()),
            (_, Some(m), _) => ("'", m.as_str()),
            (_, _, Some(m)) => ("", m.as_str()),
            _ => continue,
        };
        let Some((path, suffix)) = split_local_reference(reference) else {
            continue;
        };
        let id = match seen.get(path) {
            Some(id) => id.clone(),
            None => {
                let id = resolve_rel(path).map(|data| register_resolved(registry, path, data));
                seen.insert(path, id.clone());
                id
            }
        };
        let Some(id) = id else {
            if lenient {
                tracing::debug!(reference = path, "leaving unresolvable CSS url() untouched");
                continue;
            }
            return Err(Error::MissingAsset {
                path: PathBuf::from(path),
                span: None,
            });
        };
        out.push_str(&css[last..whole.start()]);
        out.push_str(&format!("url({quote}asset:{id}{suffix}{quote})"));
        last = whole.end();
    }
    out.push_str(&css[last..]);
    Ok(out)
}

/// Placeholder pattern: group 1 is the id, group 2 a 13th hex digit that disqualifies it.
static PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| {
    // A constant pattern covered by the unit tests: compiling it cannot fail.
    Regex::new(r"asset:([0-9a-f]{12})([0-9a-fA-F])?").expect("placeholder regex is valid")
});

/// CSS `url()` pattern. Group 1: a comment (skipped); group 2: an `@import` prefix (the
/// whole import is skipped); groups 3/4/5: double-quoted, single-quoted, unquoted reference.
static CSS_URL: LazyLock<Regex> = LazyLock::new(|| {
    // A constant pattern covered by the unit tests: compiling it cannot fail.
    Regex::new(
        r#"(?s)(/\*.*?(?:\*/|\z))|((?i:@import)\s+)?(?i:url)\(\s*(?:"([^"]*)"|'([^']*)'|([^"'()\s]*))\s*\)"#,
    )
    .expect("CSS url() regex is valid")
});

/// One placeholder occurrence found in a text.
struct Placeholder<'t> {
    /// Byte offset of `asset:`.
    start: usize,
    /// The 12-char id.
    id: &'t str,
    /// Byte offset just after the id.
    id_end: usize,
    /// Byte offset just after a `?query` following the id (`== id_end` when there is none).
    query_end: usize,
}

/// Every placeholder of `text`, in order.
fn find_placeholders(text: &str) -> Vec<Placeholder<'_>> {
    PLACEHOLDER
        .captures_iter(text)
        .filter(|caps| caps.get(2).is_none())
        .filter_map(|caps| {
            let (whole, id) = (caps.get(0)?, caps.get(1)?);
            Some(Placeholder {
                start: whole.start(),
                id: id.as_str(),
                id_end: id.end(),
                query_end: query_end(text, id.end()),
            })
        })
        .collect()
}

/// End of a `?query` starting at byte `pos` (`pos` itself when there is none). The query
/// stops before `#`, whitespace, quotes, parentheses, `<`, `>` or `\`, and excludes trailing
/// commas (a `srcset` candidate separator).
fn query_end(text: &str, pos: usize) -> usize {
    let rest = &text[pos..];
    if !rest.starts_with('?') {
        return pos;
    }
    let len = rest
        .find(|c: char| {
            c.is_whitespace() || matches!(c, '#' | '"' | '\'' | '(' | ')' | '<' | '>' | '\\')
        })
        .unwrap_or(rest.len());
    pos + rest[..len].trim_end_matches(',').len()
}

/// Splits a CSS reference into `(path, "?query#fragment")` when it is a relative local
/// reference; `None` for references that stay untouched.
fn split_local_reference(reference: &str) -> Option<(&str, &str)> {
    let reference = reference.trim();
    if reference.is_empty() || reference.starts_with(['#', '/']) || has_scheme(reference) {
        return None;
    }
    let cut = reference.find(['?', '#']).unwrap_or(reference.len());
    let (path, suffix) = reference.split_at(cut);
    (!path.is_empty()).then_some((path, suffix))
}

/// True if `reference` starts with a URL scheme (`[A-Za-z][A-Za-z0-9+.-]*:`).
fn has_scheme(reference: &str) -> bool {
    let Some((scheme, _)) = reference.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Registers what a resolver returned for the CSS reference `path`; returns the id.
fn register_resolved(registry: &mut AssetRegistry, path: &str, data: AssetData) -> String {
    match data {
        AssetData::File(file) => registry.register_file(&file),
        AssetData::Bytes(bytes) => {
            let mime = mime_guess::from_path(Path::new(path)).first_or_octet_stream();
            let hint = match path.rsplit(['/', '\\']).next() {
                Some(name) if !name.is_empty() => name,
                _ => "asset",
            };
            registry.register_bytes(bytes, mime.essence_str(), hint)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "3f9a1c2b7e0d";

    fn ids(text: &str) -> Vec<&str> {
        find_placeholders(text).iter().map(|p| p.id).collect()
    }

    fn site() -> OutputMode {
        OutputMode::Site {
            assets_dir: PathBuf::from("out/assets"),
            url_prefix: "assets/".to_string(),
        }
    }

    #[test]
    fn placeholders_are_asset_colon_and_exactly_twelve_lowercase_hex() {
        assert_eq!(ids("asset:3f9a1c2b7e0d"), [ID]);
        assert_eq!(ids("<img src=\"asset:3f9a1c2b7e0d\">"), [ID]);
        assert_eq!(ids("url(asset:3f9a1c2b7e0d)"), [ID]);
        // Any non-hex character (or the end) terminates the id.
        assert_eq!(ids("asset:3f9a1c2b7e0dg"), [ID]);
        assert_eq!(ids("asset:3f9a1c2b7e0d?v=1"), [ID]);
        assert_eq!(ids("asset:3f9a1c2b7e0d.png"), [ID]);
        // A 13th hex digit, lowercase or uppercase, means it is not a placeholder at all.
        assert!(ids("asset:3f9a1c2b7e0d1").is_empty());
        assert!(ids("asset:3f9a1c2b7e0dA").is_empty());
        assert!(ids(&format!("asset:{}", "a".repeat(64))).is_empty());
        // Too short, uppercase, or no scheme separator.
        assert!(ids("asset:3f9a1c2b7e0").is_empty());
        assert!(ids("asset:3F9A1C2B7E0D").is_empty());
        assert!(ids("asset3f9a1c2b7e0d").is_empty());
        // Several, in order, including back to back in a srcset.
        assert_eq!(
            ids("asset:000000000000 1x, asset:111111111111 2x,asset:000000000000"),
            ["000000000000", "111111111111", "000000000000"]
        );
    }

    #[test]
    fn query_end_stops_at_delimiters_and_trailing_commas() {
        let text = "asset:3f9a1c2b7e0d?v=1&amp;x=2#frag";
        let p = &find_placeholders(text)[0];
        assert_eq!((p.start, p.id_end), (0, 18));
        assert_eq!(&text[p.id_end..p.query_end], "?v=1&amp;x=2");
        for (text, query) in [
            ("asset:3f9a1c2b7e0d", ""),
            ("asset:3f9a1c2b7e0d#x", ""),
            ("asset:3f9a1c2b7e0d?", "?"),
            ("asset:3f9a1c2b7e0d?a)", "?a"),
            ("asset:3f9a1c2b7e0d?a\"", "?a"),
            ("asset:3f9a1c2b7e0d?a'", "?a"),
            ("asset:3f9a1c2b7e0d?a 2x", "?a"),
            ("asset:3f9a1c2b7e0d?a,b, x", "?a,b"),
            ("asset:3f9a1c2b7e0d?a>", "?a"),
        ] {
            let p = &find_placeholders(text)[0];
            assert_eq!(&text[p.id_end..p.query_end], query, "{text}");
        }
    }

    #[test]
    fn hashed_file_names() {
        for (hint, expected) in [
            ("plot.png", "plot-3f9a1c2b7e0d.png"),
            ("output.svg", "output-3f9a1c2b7e0d.svg"),
            ("Makefile", "Makefile-3f9a1c2b7e0d"),
            ("archive.tar.gz", "archive.tar-3f9a1c2b7e0d.gz"),
            (
                "KaTeX_Main-Regular.woff2",
                "KaTeX_Main-Regular-3f9a1c2b7e0d.woff2",
            ),
            ("my plot (1).png", "my_plot__1_-3f9a1c2b7e0d.png"),
            ("größe.png", "gr__e-3f9a1c2b7e0d.png"),
            ("../../etc/passwd", "passwd-3f9a1c2b7e0d"),
            ("figures\\plot.png", "plot-3f9a1c2b7e0d.png"),
            (".png", "asset-3f9a1c2b7e0d.png"),
            ("..png", "asset-3f9a1c2b7e0d.png"),
            ("trailing.", "trailing-3f9a1c2b7e0d"),
            ("dir/", "asset-3f9a1c2b7e0d"),
            ("", "asset-3f9a1c2b7e0d"),
        ] {
            assert_eq!(hashed_file_name(hint, ID), expected, "{hint:?}");
        }
    }

    #[test]
    fn data_uri_is_unwrapped_standard_base64() {
        assert_eq!(
            data_uri("image/png", b"\x89PNG"),
            "data:image/png;base64,iVBORw=="
        );
        assert_eq!(data_uri("text/plain", b""), "data:text/plain;base64,");
        // Standard alphabet (`+` and `/`), padding, and no line breaks for long input.
        assert_eq!(data_uri("a/b", &[0xfb, 0xff]), "data:a/b;base64,+/8=");
        let long = data_uri("image/svg+xml", &[b'x'; 1000]);
        assert!(long.starts_with("data:image/svg+xml;base64,eHh4"));
        assert!(!long.contains(['\n', '\r']));
    }

    #[test]
    fn local_references_and_suffixes() {
        for (reference, expected) in [
            ("fonts/a.woff2", Some(("fonts/a.woff2", ""))),
            ("./a.png", Some(("./a.png", ""))),
            ("../img/a.png", Some(("../img/a.png", ""))),
            ("a.eot?#iefix", Some(("a.eot", "?#iefix"))),
            ("a.svg#icon", Some(("a.svg", "#icon"))),
            ("a.woff?v=4.7", Some(("a.woff", "?v=4.7"))),
            ("  spaced.png  ", Some(("spaced.png", ""))),
            ("fonts/a:b.woff", Some(("fonts/a:b.woff", ""))),
            ("data:font/woff2;base64,AAAA", None),
            ("http://example.com/a.png", None),
            ("HTTPS://example.com/a.png", None),
            ("//cdn.example.com/a.png", None),
            ("/abs/a.png", None),
            ("#filter", None),
            ("asset:3f9a1c2b7e0d", None),
            ("blob:xyz", None),
            ("", None),
            ("   ", None),
            ("?only-query", None),
        ] {
            assert_eq!(split_local_reference(reference), expected, "{reference:?}");
        }
    }

    /// Resolver that knows exactly the given references and returns their name as bytes.
    fn known(refs: &'static [&'static str]) -> impl FnMut(&str) -> Option<AssetData> {
        move |r: &str| {
            refs.contains(&r)
                .then(|| AssetData::Bytes(r.as_bytes().to_vec()))
        }
    }

    fn rewrite(css: &str, refs: &'static [&'static str]) -> (String, AssetRegistry) {
        let mut registry = AssetRegistry::new();
        let out = register_css_urls(css, &mut registry, &mut known(refs), true).unwrap();
        (out, registry)
    }

    fn id_of(reference: &str) -> String {
        blake3::hash(reference.as_bytes()).to_hex()[..12].to_string()
    }

    #[test]
    fn css_url_forms_keep_their_quotes() {
        let refs = &["a.png"];
        let a = id_of("a.png");
        for (css, expected) in [
            (
                "x{background:url(a.png)}",
                format!("x{{background:url(asset:{a})}}"),
            ),
            (
                "x{background:url( a.png )}",
                format!("x{{background:url(asset:{a})}}"),
            ),
            (
                "x{background:url(\"a.png\")}",
                format!("x{{background:url(\"asset:{a}\")}}"),
            ),
            (
                "x{background:url( 'a.png' )}",
                format!("x{{background:url('asset:{a}')}}"),
            ),
            (
                "x{background:URL(a.png)}",
                format!("x{{background:url(asset:{a})}}"),
            ),
            (
                "x{b:url(a.png?v=2#f)}",
                format!("x{{b:url(asset:{a}?v=2#f)}}"),
            ),
        ] {
            assert_eq!(rewrite(css, refs).0, expected, "{css}");
        }
    }

    #[test]
    fn css_skips_comments_imports_and_non_local_references() {
        let refs = &["a.png", "b.css"];
        for css in [
            "/* url(missing.png) */ x{}",
            "/* unterminated url(missing.png)",
            "@import url(b.css);",
            "@IMPORT  url('b.css') screen;",
            "x{background:url(data:image/png;base64,iVBORw==)}",
            "x{background:url('data:image/svg+xml;utf8,<svg>(x)</svg>')}",
            "x{background:url(https://example.com/a.png)}",
            "x{background:url(//example.com/a.png)}",
            "x{background:url(/a.png)}",
            "x{filter:url(#blur)}",
            "x{background:url()}",
            "x{background:url(asset:3f9a1c2b7e0d)}",
        ] {
            let (out, registry) = rewrite(css, refs);
            assert_eq!(out, css);
            assert!(registry.is_empty(), "{css}");
        }
        // Not skipped when the comment is closed before the url().
        let (out, registry) = rewrite("/* c */ x{background:url(a.png)}", refs);
        assert_eq!(
            out,
            format!("/* c */ x{{background:url(asset:{})}}", id_of("a.png"))
        );
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn css_resolves_each_reference_once_and_registers_bytes_with_guessed_mime() {
        let mut calls = Vec::new();
        let mut resolver = |r: &str| {
            calls.push(r.to_string());
            Some(AssetData::Bytes(r.as_bytes().to_vec()))
        };
        let mut registry = AssetRegistry::new();
        let css = "a{b:url(img/x.svg)} c{d:url('img/x.svg?v=1')} e{f:url(font.woff2)}";
        let out = register_css_urls(css, &mut registry, &mut resolver, false).unwrap();
        assert_eq!(calls, ["img/x.svg", "font.woff2"]);
        let (x, font) = (id_of("img/x.svg"), id_of("font.woff2"));
        assert_eq!(
            out,
            format!("a{{b:url(asset:{x})}} c{{d:url('asset:{x}?v=1')}} e{{f:url(asset:{font})}}")
        );
        let svg = registry.get(&x).unwrap();
        assert_eq!(
            (svg.mime.as_str(), svg.file_name_hint.as_str()),
            ("image/svg+xml", "x.svg")
        );
        assert_eq!(registry.get(&font).unwrap().mime, "font/woff2");
        // Idempotent: placeholders are references with a scheme.
        let again = register_css_urls(&out, &mut registry, &mut known(&[]), false).unwrap();
        assert_eq!(again, out);
    }

    #[test]
    fn css_file_results_use_register_file() {
        let mut registry = AssetRegistry::new();
        let mut resolver = |r: &str| Some(AssetData::File(Path::new("/theme/assets").join(r)));
        let out =
            register_css_urls("x{b:url(bg.jpg)}", &mut registry, &mut resolver, false).unwrap();
        let id = registry.register_file(Path::new("/theme/assets/bg.jpg"));
        assert_eq!(registry.len(), 1);
        assert_eq!(out, format!("x{{b:url(asset:{id})}}"));
        assert_eq!(registry.get(&id).unwrap().mime, "image/jpeg");
    }

    #[test]
    fn strict_css_names_the_unresolvable_reference() {
        let mut registry = AssetRegistry::new();
        let err = register_css_urls(
            "x{b:url(gone.png?v=1)}",
            &mut registry,
            &mut known(&[]),
            false,
        )
        .unwrap_err();
        match err {
            Error::MissingAsset { path, span } => {
                assert_eq!(path, PathBuf::from("gone.png"));
                assert_eq!(span, None);
            }
            other => panic!("expected MissingAsset, got {other:?}"),
        }
    }

    #[test]
    fn standalone_drops_queries_but_keeps_fragments() {
        let mut registry = AssetRegistry::new();
        let id = registry.register_bytes(b"\x89PNG".to_vec(), "image/png", "a.png");
        let text = format!(
            "url(asset:{id}?#iefix) url(\"asset:{id}?v=1\") <img srcset=\"asset:{id}?a=1, asset:{id} 2x\">"
        );
        let standalone = OutputMode::Standalone;
        let out = Emitter::new(&registry, &standalone)
            .resolve_css(&text)
            .unwrap();
        let uri = data_uri("image/png", b"\x89PNG");
        assert_eq!(
            out,
            format!("url({uri}#iefix) url(\"{uri}\") <img srcset=\"{uri}, {uri} 2x\">")
        );
        // Site and serve keep the whole suffix.
        let site = site();
        let out = Emitter::new(&registry, &site).resolve_css(&text).unwrap();
        let name = format!("assets/a-{id}.png");
        assert_eq!(
            out,
            format!(
                "url({name}?#iefix) url(\"{name}?v=1\") <img srcset=\"{name}?a=1, {name} 2x\">"
            )
        );
        let serve = OutputMode::Serve;
        let out = Emitter::new(&registry, &serve).resolve_html(&text).unwrap();
        assert!(
            out.contains(&format!("url(/__presenter/deck-asset/{id}?#iefix)")),
            "{out}"
        );
    }

    #[test]
    fn a_placeholder_inside_a_dropped_query_goes_with_it() {
        let mut registry = AssetRegistry::new();
        let a = registry.register_bytes(vec![1], "image/png", "a.png");
        let b = registry.register_bytes(vec![2], "image/png", "b.png");
        let text = format!("x asset:{a}?next=asset:{b} y");
        let standalone = OutputMode::Standalone;
        let out = Emitter::new(&registry, &standalone)
            .resolve_html(&text)
            .unwrap();
        assert_eq!(out, format!("x {} y", data_uri("image/png", &[1])));
        let serve = OutputMode::Serve;
        let out = Emitter::new(&registry, &serve).resolve_html(&text).unwrap();
        assert_eq!(
            out,
            format!("x /__presenter/deck-asset/{a}?next=/__presenter/deck-asset/{b} y")
        );
    }

    #[test]
    fn text_without_placeholders_is_returned_unchanged() {
        let registry = AssetRegistry::new();
        for mode in [OutputMode::Standalone, site(), OutputMode::Serve] {
            let mut emitter = Emitter::new(&registry, &mode);
            let html = "<p>asset:3f9a1c2b7e0d1 is a 13-digit hash, not a placeholder</p>";
            assert_eq!(emitter.resolve_html(html).unwrap(), html);
            assert_eq!(emitter.resolve_css("").unwrap(), "");
            assert!(emitter.files.is_empty());
        }
    }

    #[test]
    fn output_mode_is_clone_debug_eq() {
        let mode = site();
        assert_eq!(mode.clone(), mode);
        assert_ne!(mode, OutputMode::Serve);
        assert!(format!("{:?}", OutputMode::Standalone).contains("Standalone"));
    }
}
