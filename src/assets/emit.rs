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
//! | [`OutputMode::Site`] | `url_prefix` + [`hashed_file_name`] of the asset's file-name hint and a hash of its **bytes**, e.g. `assets/plot-5e0c7b1f9a2d.png` | read now, one [`EmittedFile`] per distinct name |
//! | [`OutputMode::Serve`] | `/__presenter/deck-asset/<id>` | not read (the dev server reads them on request) |
//!
//! **Site-mode file names are content-addressed** (ir.md §3, audit F20): the hash in the name
//! is [`content_hash`] of the bytes written, never the registry id (which, for a file asset,
//! hashes its absolute path). The same deck built from two checkouts gives the same names, and
//! an edited image gets a new name, so a host may cache `assets/` forever. Two assets with the
//! same bytes and the same file-name hint share one file.
//!
//! **Site-mode URLs depend on where the text ends up.** [`Emitter::resolve_html`],
//! [`Emitter::resolve_css`] and [`Emitter::url_for`] return URLs relative to the output
//! *document* (`url_prefix` + file name, e.g. `assets/plot-<id>.png`): right for the document
//! itself and for CSS inlined into it (`<style>`). A browser resolves a `url()` inside a
//! stylesheet *file* against that file's own URL, so a stylesheet written into `assets_dir`
//! (the site-mode theme CSS `assets/theme-<hash>.css`, the copies made by `style(path)`)
//! must be resolved with [`Emitter::resolve_css_in_assets_dir`], which writes bare file names
//! (`plot-<id>.png`). Both methods share [`Emitter::files`]. Standalone and serve URLs do not
//! depend on the location (`data:` URIs and root-absolute routes).
//!
//! **One output, one file list.** Site mode emits each asset once per output: before
//! emitting a file the emitter looks for its path in [`Emitter::files`], so that list *is*
//! the deduplication state. An [`Emitter`] borrows the registry and the mode, so it cannot
//! be stored in a `'static` closure such as a minijinja template function (minijinja 2.24
//! requires `Send + Sync + 'static`). Two patterns keep the once-per-id guarantee:
//! 1. template helpers only write `asset:<id>` placeholders (registering into a registry
//!    they own) and one emitter resolves the finished document and stylesheets at the end;
//! 2. helpers keep an owned (or `Arc`) [`AssetRegistry`], an owned [`OutputMode`] and the
//!    `Vec<EmittedFile>` behind a mutex, create a short-lived emitter per call with
//!    [`Emitter::with_files`], and put [`Emitter::files`] back afterwards.
//!
//! **Placeholder scanning.** The placeholder grammar is `asset:` followed by exactly 12
//! `[0-9a-f]`; a 13th hex digit (`[0-9a-fA-F]`) means the text is not a placeholder (so a
//! longer hash is never cut to its first 12 chars), any other following character or the end
//! of the text ends it. Unique ids are collected first, each is resolved once, then all
//! occurrences are replaced. An id that is not registered is an [`Error::Other`] naming it
//! (and, in a document, the attribute or element it was found in).
//!
//! In an HTML document ([`Emitter::resolve_html`]) placeholders are replaced only where the
//! frontends and the notebook attachment resolver write them (ir.md §3, audit F16): inside the
//! **attribute values** of start tags (`src`, `href`, `srcset`, `poster`,
//! `data-background-*`, inline `style` `url()`, …) and inside the content of **`<style>` and
//! `<script>` elements** (attachments resolved in a notebook cell's CSS or script). Text
//! content is never rewritten: a literal `asset:` + 12 hex digits in prose, inline code, a
//! code block or speaker notes is left alone. The document is tokenized like a browser does
//! (the Markdown renderer's HTML tokenizer: comments, raw-text elements and quoted values are
//! honoured), so `<p>asset:…</p>` and `<code>&lt;img src="asset:…"&gt;</code>` are text. In a
//! stylesheet ([`Emitter::resolve_css`]) the whole text is scanned.
//!
//! **Query strings and fragments.** [`register_css_urls`] keeps a reference's `?query#frag`
//! after the placeholder (`url(asset:<id>?#iefix)`), so site and serve URLs keep it. A
//! `data:` URI cannot carry a query (it would become part of the payload), so in standalone
//! mode a `?query` directly after a placeholder is dropped while a `#fragment` is kept
//! (fragments are valid on `data:` URIs, e.g. for SVG views). The dropped query ends before
//! `#`, whitespace, a quote, `(`, `)`, `<`, `>`, `\`, an HTML character reference for a
//! quote or `#` (`&quot;`, `&#39;`, `&#x27;`, `&apos;`, `&#35;`, …, so an entity-escaped
//! inline `style` keeps its closing quote; other references such as `&amp;` or `&#38;` are
//! part of the query) or the end of the text, and never includes trailing commas (`srcset`
//! separators).
//!
//! **Known limitation: `data-background-video` in standalone mode.** reveal.js splits the
//! value of `data-background-video` on `,` (one `<source>` per URL), which cuts a
//! `data:video/…;base64,…` URI in two, so a background video does not play in a standalone
//! build. Site and serve URLs contain no comma and work. The emitter cannot fix this (a
//! `data:` URI needs its comma); a renderer that wants standalone background videos has to
//! emit them differently (e.g. as a `<video>` element).
//!
//! **CSS `url()` registration.** [`register_css_urls`] is the step *before* emission for
//! stylesheets: it turns relative `url(...)` references into placeholders, registering what
//! the caller's resolver finds (theme chain or vendor tree lookup is the caller's job). It
//! tokenizes just enough CSS to be safe: comments and string literals are skipped (so
//! `content: "url(x.png)"` and a `/*` inside a string are text), a `url(` glued to an
//! identifier (`myurl(`) is not a URL, and CSS escapes are undone and `%XX` sequences
//! decoded before the resolver sees a path.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use base64::Engine as _;
use regex::{Captures, Regex};

use crate::assets::registry::{AssetData, AssetRegistry, content_hash, unknown_id};
use crate::error::{Error, Result};
use crate::markdown::render::{html_attr_values, html_start_tags};

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
        /// Prepended verbatim to the file name to form a URL relative to the output
        /// *document*; normally `"assets/"`, so it should end with `/`. Not used for
        /// stylesheets written into `assets_dir`
        /// ([`Emitter::resolve_css_in_assets_dir`] writes bare file names).
        url_prefix: String,
    },
    /// `presenter serve`: assets are served by id from `/__presenter/deck-asset/<id>`.
    Serve,
}

/// A file the caller must write next to the document (site mode only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmittedFile {
    /// `assets_dir.join(hashed_file_name(hint, &content_hash(&bytes)))`.
    pub path: PathBuf,
    /// The asset's content.
    pub bytes: Vec<u8>,
}

/// Where resolved text will live; only site-mode URLs depend on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Referrer {
    /// The output document, or CSS inlined into it: `url_prefix` + file name.
    Document,
    /// A stylesheet file written into `assets_dir`: the bare file name.
    AssetsDir,
}

/// Rewrites placeholders for one output. Use one emitter per output (or carry its
/// [`Emitter::files`] into the next one with [`Emitter::with_files`]) so that site mode
/// writes each file once, however often it is referenced.
#[derive(Debug)]
pub struct Emitter<'a> {
    registry: &'a AssetRegistry,
    mode: &'a OutputMode,
    /// Files emitted so far (site mode; always empty otherwise), one per distinct file name
    /// (at most one per id), in order of first use. This list is also the deduplication
    /// state: an asset whose path is already in it is not emitted again.
    pub files: Vec<EmittedFile>,
}

impl<'a> Emitter<'a> {
    /// An emitter resolving ids against `registry` for `mode`.
    pub fn new(registry: &'a AssetRegistry, mode: &'a OutputMode) -> Self {
        Self::with_files(registry, mode, Vec::new())
    }

    /// An emitter that continues an earlier emission for the same output: `files` are the
    /// [`Emitter::files`] of a previous emitter, and assets already in them are not emitted
    /// again. For callers that cannot keep one emitter alive for the whole output (e.g.
    /// `'static` template helpers; see the module docs).
    pub fn with_files(
        registry: &'a AssetRegistry,
        mode: &'a OutputMode,
        files: Vec<EmittedFile>,
    ) -> Self {
        Emitter {
            registry,
            mode,
            files,
        }
    }

    /// Replace every `asset:<id>` in the attribute values and the `<style>` and `<script>`
    /// elements of an HTML document; text content is left alone (see the module docs).
    ///
    /// Site-mode URLs are relative to the document. Errors: an unknown id ([`Error::Other`],
    /// naming the attribute or element), a file asset that disappeared
    /// ([`Error::MissingAsset`], standalone and site modes only) or another read failure.
    pub fn resolve_html(&mut self, html: &str) -> Result<String> {
        self.resolve_placeholders(html, Referrer::Document, Scope::Html)
    }

    /// Replace `asset:<id>` inside a CSS text (typically the output of
    /// [`register_css_urls`]) that is inlined into the output document (`<style>`): site-mode
    /// URLs are relative to the document. For a stylesheet written as a file into
    /// `assets_dir` use [`Emitter::resolve_css_in_assets_dir`]. The whole text is scanned;
    /// errors as [`Emitter::resolve_html`].
    pub fn resolve_css(&mut self, css: &str) -> Result<String> {
        self.resolve_placeholders(css, Referrer::Document, Scope::Css)
    }

    /// Like [`Emitter::resolve_css`], for a stylesheet that the caller writes as a file into
    /// the site-mode `assets_dir` (e.g. `assets/theme-<hash>.css`): site-mode URLs are bare
    /// file names (`bg-<hash>.png`), because a browser resolves them against the
    /// stylesheet's own URL. Emitted files are shared with the other methods. In standalone
    /// and serve mode this is exactly [`Emitter::resolve_css`].
    pub fn resolve_css_in_assets_dir(&mut self, css: &str) -> Result<String> {
        self.resolve_placeholders(css, Referrer::AssetsDir, Scope::Css)
    }

    /// URL for one id in the current mode (data URI / "assets/plot-5e0c7b1f9a2d.png" /
    /// "/__presenter/deck-asset/3f9a1c2b7e0d"); site-mode URLs are relative to the document
    /// and name the file by a hash of its bytes ([`content_hash`]), not by the id.
    ///
    /// Site mode reads the asset and pushes an [`EmittedFile`] unless [`Emitter::files`]
    /// already holds its path. Serve mode only checks that the id is registered and reads
    /// nothing.
    pub fn url_for(&mut self, id: &str) -> Result<String> {
        self.url_from(id, Referrer::Document)
    }

    /// [`Emitter::url_for`] for text that will live at `referrer`.
    fn url_from(&mut self, id: &str, referrer: Referrer) -> Result<String> {
        let asset = self.registry.get(id).ok_or_else(|| unknown_id(id))?;
        match self.mode {
            OutputMode::Standalone => Ok(data_uri(&asset.mime, &self.registry.read(id)?)),
            OutputMode::Site {
                assets_dir,
                url_prefix,
            } => {
                // The name hashes the bytes (module docs), so they are read even when the
                // file was emitted before: once per distinct id and resolve call.
                let bytes = self.registry.read(id)?;
                let name = hashed_file_name(&asset.file_name_hint, &content_hash(&bytes));
                let path = assets_dir.join(&name);
                // Linear, but outputs hold at most a few hundred files.
                if !self.files.iter().any(|file| file.path == path) {
                    let bytes = bytes.into_owned();
                    tracing::debug!(id, file = %name, len = bytes.len(), "emitting asset file");
                    self.files.push(EmittedFile { path, bytes });
                }
                Ok(match referrer {
                    Referrer::Document => format!("{url_prefix}{name}"),
                    Referrer::AssetsDir => name,
                })
            }
            OutputMode::Serve => Ok(format!("{SERVE_ASSET_PREFIX}{id}")),
        }
    }

    /// Shared implementation of the `resolve_*` methods.
    fn resolve_placeholders(
        &mut self,
        text: &str,
        referrer: Referrer,
        scope: Scope,
    ) -> Result<String> {
        let mut found = find_placeholders(text);
        if scope == Scope::Html && !found.is_empty() {
            found = in_html_values(text, found);
        }
        if found.is_empty() {
            return Ok(text.to_string());
        }
        let mut urls: BTreeMap<&str, String> = BTreeMap::new();
        for placeholder in &found {
            if urls.contains_key(placeholder.id) {
                continue;
            }
            if self.registry.get(placeholder.id).is_none() {
                return Err(match &placeholder.place {
                    Some(place) => unknown_id_in(placeholder.id, place),
                    None => unknown_id(placeholder.id),
                });
            }
            let url = self.url_from(placeholder.id, referrer)?;
            urls.insert(placeholder.id, url);
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

/// Deterministic site-mode file name: `"plot.png"` + hash → `"plot-<hash>.png"`; a name
/// without extension → `"<name>-<hash>"`. The emitter passes [`content_hash`] of the
/// asset's bytes, so names are content-addressed.
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
///   whitespace inside the parentheses and CSS escapes (`url(a\(1\).png)`). The rewritten
///   reference keeps its quote style: `url("fonts/a.woff2")` → `url("asset:<id>")`.
/// - Left untouched: references with a scheme (`data:`, `http:`, `https:`, `asset:`, …),
///   protocol-relative `//…`, root-absolute `/…` (or `\…`, which URL parsers read as `/`),
///   fragment-only `#…`, empty references, `@import url(…)` (imports are not followed here;
///   T42 inlines local imports before calling this function), and everything inside
///   `/* comments */` and string literals (`content: "url(x.png)"` is text). A function whose
///   name merely ends in `url` (`myurl(…)`) is not a `url()`. These checks apply to the path
///   as written (CSS escapes undone); percent-decoding cannot get around them (below).
/// - `resolve_rel` receives the path without its `?query#fragment`, with CSS escapes undone
///   and `%XX` sequences percent-decoded (kept as written when they do not decode to UTF-8):
///   `fonts/My Font.woff2` for `url("fonts/My%20Font.woff2?v=1#x")`. A path that decoding
///   would change in structure (a decoded `/`, `\` or NUL, or a decoded path that is
///   root-absolute or has a scheme: `url(..%2Fx.png)`, `url(%2Fetc%2Fx.png)`,
///   `url(C%3A/x.png)`) is passed undecoded instead, as the literal names a browser sees
///   (`%2F` stays inside its path segment), so it can only name a file where a plain relative
///   path could. The resolver is called once per distinct path. The suffix follows the
///   placeholder: the fragment as written, the query as written unless it has CSS escapes
///   or characters a `url()` must escape, which are undone and percent-encoded (`?v=\31` →
///   `?v=1`, `?x=\(1\)` → `?x=%281%29`; also a `&` that would form an HTML reference with
///   the fragment: `?a&#1x` → `?a%26#1x`), so a standalone [`Emitter`] can drop it whole.
/// - [`AssetData::File`] is registered with [`AssetRegistry::register_file`];
///   [`AssetData::Bytes`] with the MIME type guessed from the path's extension and its last
///   segment as file-name hint.
/// - An unresolvable reference with `lenient = false` is
///   [`Error::MissingAsset`]` { path: <the path given to resolve_rel>, span: None }`. The
///   path is relative and the error does not name the stylesheet; callers add that context
///   (e.g. by mapping it to [`Error::Style`] with the stylesheet's name).
pub fn register_css_urls(
    css: &str,
    registry: &mut AssetRegistry,
    resolve_rel: &mut dyn FnMut(&str) -> Option<AssetData>,
    lenient: bool,
) -> Result<String> {
    let mut out = String::with_capacity(css.len());
    let mut last = 0;
    // Decoded path → id (None: unresolvable), so each is resolved once per call.
    let mut seen: BTreeMap<String, Option<String>> = BTreeMap::new();
    for caps in CSS_TOKEN.captures_iter(css) {
        let (Some(whole), Some((quote, reference))) = (caps.get(0), url_argument(&caps)) else {
            continue;
        };
        let Some(LocalRef { path, suffix }) = local_reference(reference) else {
            continue;
        };
        let id = match seen.get(&path) {
            Some(id) => id.clone(),
            None => {
                let id = resolve_rel(&path).map(|data| register_resolved(registry, &path, data));
                seen.insert(path.clone(), id.clone());
                id
            }
        };
        let Some(id) = id else {
            if lenient {
                tracing::debug!(reference, "leaving unresolvable CSS url() untouched");
                continue;
            }
            return Err(Error::MissingAsset {
                path: PathBuf::from(path),
                span: None,
            });
        };
        out.push_str(&css[last..whole.start()]);
        let suffix = plain_query(suffix);
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

/// The CSS tokens [`register_css_urls`] cares about, tried in this order at each position:
/// a comment (to the end of the text when unterminated), a string literal (ended by its
/// quote, a newline or the end of the text), or a `url(…)` preceded by an optional
/// `@import` and by any identifier characters it is glued to. Only a `url(…)` without
/// `import` and with an empty `prefix` is a reference; the rest is matched to be skipped.
/// The argument is `dq`, `sq` (inside double or single quotes) or `uq` (unquoted), escapes
/// included.
static CSS_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    // A constant pattern covered by the unit tests: compiling it cannot fail.
    Regex::new(concat!(
        r"(?s)(?P<comment>/\*.*?(?:\*/|\z))",
        r#"|(?P<string>"(?:[^"\\\n]|\\.)*(?:"|\n|\z)|'(?:[^'\\\n]|\\.)*(?:'|\n|\z))"#,
        r"|(?P<import>(?i:@import)\s+)?(?P<prefix>[-\w]*)(?i:url)\(\s*",
        r#"(?:"(?P<dq>(?:[^"\\\n]|\\.)*)"|'(?P<sq>(?:[^'\\\n]|\\.)*)'"#,
        r#"|(?P<uq>(?:[^"'()\\\s]|\\(?:[0-9a-fA-F]{1,6}\s?|[^\n0-9a-fA-F]))*))\s*\)"#,
    ))
    .expect("CSS token regex is valid")
});

/// The quote style and the raw argument of a `CSS_TOKEN` match that is a `url()` reference;
/// `None` for comments, strings, `@import`s and `<identifier>url(` functions.
fn url_argument<'t>(caps: &Captures<'t>) -> Option<(&'static str, &'t str)> {
    if caps.name("import").is_some() || caps.name("prefix").is_none_or(|p| !p.is_empty()) {
        return None;
    }
    [("\"", "dq"), ("'", "sq"), ("", "uq")]
        .into_iter()
        .find_map(|(quote, group)| Some((quote, caps.name(group)?.as_str())))
}

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
    /// Where it was found in an HTML document (an attribute's name, or `<style>` /
    /// `<script>`), for error messages.
    place: Option<String>,
}

/// Which parts of a text the `resolve_*` methods rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// An HTML document: attribute values and `<style>`/`<script>` contents only.
    Html,
    /// A stylesheet: the whole text.
    Css,
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
                place: None,
            })
        })
        .collect()
}

/// The placeholders of `found` (all of `html`'s, in order) that lie in a region of
/// [`html_regions`], each with its region as `place` and its query cut at the region's end.
fn in_html_values<'t>(html: &str, found: Vec<Placeholder<'t>>) -> Vec<Placeholder<'t>> {
    let regions = html_regions(html);
    found
        .into_iter()
        .filter_map(|mut placeholder| {
            let after = regions.partition_point(|(range, _)| range.start <= placeholder.start);
            let (range, place) = regions.get(after.checked_sub(1)?)?;
            if placeholder.id_end > range.end {
                return None;
            }
            placeholder.query_end = placeholder.query_end.min(range.end);
            placeholder.place = Some(place.clone());
            Some(placeholder)
        })
        .collect()
}

/// The byte ranges of `html` where placeholders are replaced (module docs), sorted and
/// disjoint, each with where it is for messages (an attribute's name, or `<style>` /
/// `<script>`): the value of every attribute of a start tag (outside comments and raw-text
/// elements, quotes excluded) and the content of every `<style>` and `<script>` element (up
/// to its end tag, or the end of `html`).
fn html_regions(html: &str) -> Vec<(Range<usize>, String)> {
    let mut regions: Vec<(Range<usize>, String)> = html_attr_values(html)
        .into_iter()
        .map(|(name, range)| (range, name))
        .collect();
    // ASCII lowercasing keeps every byte offset.
    let lower = html.to_ascii_lowercase();
    for (name, end) in html_start_tags(html) {
        if matches!(name.as_str(), "style" | "script") {
            let close = format!("</{name}");
            let stop = lower[end..].find(&close).map_or(html.len(), |at| end + at);
            regions.push((end..stop, format!("<{name}>")));
        }
    }
    regions.sort_by_key(|(range, _)| range.start);
    regions
}

/// The error for an unregistered id found in a document, at `place` of [`html_regions`].
fn unknown_id_in(id: &str, place: &str) -> Error {
    let place = if place.starts_with('<') {
        format!("a {place} element")
    } else {
        format!("a `{place}` attribute")
    };
    Error::Other(format!(
        "unknown asset id '{id}': the placeholder 'asset:{id}' in {place} of the document \
         does not name a registered asset"
    ))
}

/// End of a `?query` starting at byte `pos` (`pos` itself when there is none). The query
/// stops before `#`, whitespace, quotes, parentheses, `<`, `>`, `\` or an HTML character
/// reference for a quote or `#` (other references such as `&#38;` are part of the query),
/// and excludes trailing commas (a `srcset` candidate separator).
fn query_end(text: &str, pos: usize) -> usize {
    let rest = &text[pos..];
    if !rest.starts_with('?') {
        return pos;
    }
    let mut len = 0;
    while let Some(c) = rest[len..].chars().next() {
        if c.is_whitespace() || matches!(c, '#' | '"' | '\'' | '(' | ')' | '<' | '>' | '\\') {
            break;
        }
        if c == '&'
            && let Some((decoded, reference_len)) = char_reference(&rest[len..])
        {
            if matches!(decoded, '"' | '\'' | '#') {
                break;
            }
            len += reference_len;
            continue;
        }
        len += c.len_utf8();
    }
    pos + rest[..len].trim_end_matches(',').len()
}

/// The character an HTML character reference at the start of `s` stands for, and the
/// reference's byte length. Decoded: numeric references, decimal or hexadecimal with an
/// optional `;` (`&#39;`, `&#x27`; out-of-range values are U+FFFD), and the named
/// references that can end a query: `&quot;`/`&QUOT;` (also without `;` unless followed by
/// an alphanumeric or `=`, the HTML rule for legacy references in attributes), `&apos;` and
/// `&num;`. Anything else (including `&amp;`) is `None`: plain text to the query scanner.
fn char_reference(s: &str) -> Option<(char, usize)> {
    let rest = s.strip_prefix('&')?;
    for (name, decoded) in [
        ("quot;", '"'),
        ("QUOT;", '"'),
        ("apos;", '\''),
        ("num;", '#'),
    ] {
        if rest.starts_with(name) {
            return Some((decoded, 1 + name.len()));
        }
    }
    if let Some(after) = rest
        .strip_prefix("quot")
        .or_else(|| rest.strip_prefix("QUOT"))
        && !after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '=')
    {
        return Some(('"', 5));
    }
    let number = rest.strip_prefix('#')?;
    let (digits, radix, prefix_len) = match number.strip_prefix(['x', 'X']) {
        Some(hex) => (hex, 16, 3),
        None => (number, 10, 2),
    };
    let end = digits
        .find(|c: char| !c.is_digit(radix))
        .unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    let decoded = u32::from_str_radix(&digits[..end], radix)
        .ok()
        .and_then(char::from_u32)
        .unwrap_or(char::REPLACEMENT_CHARACTER);
    let semicolon = usize::from(digits[end..].starts_with(';'));
    Some((decoded, prefix_len + end + semicolon))
}

/// A relative local reference of a CSS `url()`.
#[derive(Debug, PartialEq, Eq)]
struct LocalRef<'r> {
    /// What the resolver gets: the path without `?query#fragment`, CSS escapes undone,
    /// percent-decoded.
    path: String,
    /// The `?query#fragment`, exactly as written.
    suffix: &'r str,
}

/// Splits a raw `url()` argument into a [`LocalRef`] when it is a relative local reference;
/// `None` for references that stay untouched. The suffix starts at the first `?` or `#`
/// that is not escaped (an escaped one counts as part of the path).
///
/// The path must be relative and local ([`is_relative_local`]) as written, CSS escapes
/// undone. It is then percent-decoded, unless decoding would change its structure: produce a
/// `/`, a `\` or a NUL, or a path that is not relative and local (`..%2Fx.png`, `a%5Cb.png`,
/// `a%00.png`, `%2Fetc%2Fx.png`, `C%3A/x.png`). Such a path is kept undecoded, one literal
/// name per written segment, as a browser reads it (`%2F` stays inside its path segment), so
/// decoding can never make it root-absolute, give it a scheme or move it to another
/// directory; the resolver sees it like any other relative path (and, unresolvable, it is
/// [`Error::MissingAsset`] when not lenient).
fn local_reference(reference: &str) -> Option<LocalRef<'_>> {
    let reference = reference.trim();
    let (raw_path, suffix) = reference.split_at(first_unescaped(reference, &['?', '#']));
    let unescaped = css_unescape(raw_path);
    if !is_relative_local(&unescaped) {
        return None;
    }
    let decoded = percent_decode(&unescaped);
    let separators = |p: &str| p.matches(['/', '\\']).count();
    let same_structure = separators(&decoded) == separators(&unescaped)
        && !decoded.contains('\0')
        && is_relative_local(&decoded);
    let path = if same_structure { decoded } else { unescaped };
    Some(LocalRef {
        path: path.into_owned(),
        suffix,
    })
}

/// A path the resolver may see: not empty, not root-absolute (`/…`, `//…`, or `\…`, which
/// URL parsers read as `/`) and without a URL scheme.
fn is_relative_local(path: &str) -> bool {
    !path.is_empty() && !path.starts_with(['/', '\\']) && !has_scheme(path)
}

/// Byte offset of the first of `targets` in a raw CSS text that is not escaped with `\`
/// (its length if none).
fn first_unescaped(text: &str, targets: &[char]) -> usize {
    let mut chars = text.char_indices();
    while let Some((i, c)) = chars.next() {
        if c == '\\' {
            chars.next();
        } else if targets.contains(&c) {
            return i;
        }
    }
    text.len()
}

/// The `?query#fragment` suffix of a CSS reference as [`register_css_urls`] writes it after
/// the placeholder: the fragment as written, the query with CSS escapes undone and every
/// character percent-encoded that would need a CSS escape in some quote style or would end
/// the emitter's query scan anywhere but at the query's end (whitespace, controls, quotes,
/// parentheses, `\`, `#`, `<`, `>`, a `&` starting an HTML reference for a quote or `#` or
/// one that runs on into the fragment, trailing commas). A standalone emitter can then drop
/// exactly the whole query instead of gluing the rest of `?v=\31` onto the `data:` URI. A
/// query without escapes or such characters is kept as written.
fn plain_query(suffix: &str) -> Cow<'_, str> {
    let Some(rest) = suffix.strip_prefix('?') else {
        return Cow::Borrowed(suffix);
    };
    let (query, fragment) = rest.split_at(first_unescaped(rest, &['#']));
    let unescaped = css_unescape(query);
    // Would `c` at byte `i` of the unescaped query need encoding? An HTML reference may run
    // on into the fragment (`?a&#35;b` is the query `a&` and the fragment `#35;b`; `?a&#1\31`
    // is `a&` and `#1\31`): the emitter's scan would read it as part of the query and go on
    // into the fragment, so such a `&` is encoded whatever the reference stands for.
    let encode = |i: usize, c: char| {
        let rest = &unescaped[i..];
        let ends_the_scan_badly = || {
            let text = format!("{rest}{fragment}");
            char_reference(&text).is_some_and(|(d, len)| "\"'#".contains(d) || len > rest.len())
        };
        c.is_whitespace()
            || c.is_control()
            || "\"'()\\#<>".contains(c)
            || (c == '&' && ends_the_scan_badly())
            || (c == ',' && rest.trim_end_matches(',').is_empty())
    };
    if !query.contains('\\') && !unescaped.char_indices().any(|(i, c)| encode(i, c)) {
        return Cow::Borrowed(suffix);
    }
    let mut out = String::from("?");
    for (i, c) in unescaped.char_indices() {
        if encode(i, c) {
            let mut buf = [0; 4];
            for byte in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        } else {
            out.push(c);
        }
    }
    out.push_str(fragment);
    Cow::Owned(out)
}

/// Undoes CSS escapes: `\` + 1–6 hex digits (+ one optional whitespace) is that code point
/// (U+FFFD for zero, surrogates and values past U+10FFFF), `\` + newline is a line
/// continuation (removed), `\` at the end is dropped, `\` + any other character is that
/// character.
fn css_unescape(s: &str) -> Cow<'_, str> {
    if !s.contains('\\') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let mut code: Option<u32> = None;
        for _ in 0..6 {
            let Some(digit) = chars.next_if(char::is_ascii_hexdigit) else {
                break;
            };
            // Six hex digits fit a u32; `to_digit` cannot fail on a hex digit.
            code = Some(code.unwrap_or(0) * 16 + digit.to_digit(16).unwrap_or(0));
        }
        match code {
            Some(code) => {
                let ws = chars.next_if(|&w| matches!(w, ' ' | '\t' | '\n' | '\r' | '\x0c'));
                if ws == Some('\r') {
                    chars.next_if_eq(&'\n');
                }
                let decoded = char::from_u32(code).filter(|&ch| ch != '\0');
                out.push(decoded.unwrap_or(char::REPLACEMENT_CHARACTER));
            }
            None => match chars.next() {
                Some('\n') | None => {}
                Some(other) => out.push(other),
            },
        }
    }
    Cow::Owned(out)
}

/// Decodes `%XX` sequences. Malformed sequences stay as written; when the decoded bytes are
/// not UTF-8 the input is returned unchanged.
fn percent_decode(s: &str) -> Cow<'_, str> {
    if !s.contains('%') {
        return Cow::Borrowed(s);
    }
    let hex = |b: Option<&u8>| b.and_then(|&b| char::from(b).to_digit(16));
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%'
            && let (Some(hi), Some(lo)) = (hex(bytes.get(i + 1)), hex(bytes.get(i + 2)))
        {
            // Two hex digits are at most 255.
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8(out).map_or(Cow::Borrowed(s), Cow::Owned)
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

/// Registers what a resolver returned for the (decoded) CSS reference `path`; returns the id.
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
            // An HTML character reference for a quote ends the query (entity-escaped
            // inline styles); other references are part of it.
            ("asset:3f9a1c2b7e0d?v=1&quot;)", "?v=1"),
            ("asset:3f9a1c2b7e0d?v=1&QUOT;)", "?v=1"),
            ("asset:3f9a1c2b7e0d?v=1&quot)", "?v=1"),
            ("asset:3f9a1c2b7e0d?v=1&#39;)", "?v=1"),
            ("asset:3f9a1c2b7e0d?v=1&#039;)", "?v=1"),
            ("asset:3f9a1c2b7e0d?v=1&#34;)", "?v=1"),
            ("asset:3f9a1c2b7e0d?v=1&#x27;)", "?v=1"),
            ("asset:3f9a1c2b7e0d?v=1&#X22;)", "?v=1"),
            ("asset:3f9a1c2b7e0d?v=1&apos;)", "?v=1"),
            ("asset:3f9a1c2b7e0d?a=1&amp;b=2&#38;c", "?a=1&amp;b=2&#38;c"),
            ("asset:3f9a1c2b7e0d?a=1&#x26c=3", "?a=1&#x26c=3"),
            ("asset:3f9a1c2b7e0d?a=1&#35;frag", "?a=1"),
            ("asset:3f9a1c2b7e0d?a=1&num;frag", "?a=1"),
            ("asset:3f9a1c2b7e0d?a=1&quotation=2", "?a=1&quotation=2"),
            ("asset:3f9a1c2b7e0d?a=1&quot=2", "?a=1&quot=2"),
            ("asset:3f9a1c2b7e0d?a=1&#3", "?a=1&#3"),
            ("asset:3f9a1c2b7e0d?a=1&#", "?a=1&"),
            ("asset:3f9a1c2b7e0d?a=1&", "?a=1&"),
        ] {
            let p = &find_placeholders(text)[0];
            assert_eq!(&text[p.id_end..p.query_end], query, "{text}");
        }
    }

    #[test]
    fn char_references() {
        for (s, expected) in [
            ("&quot;", Some(('"', 6))),
            ("&QUOT;x", Some(('"', 6))),
            ("&quot", Some(('"', 5))),
            ("&quot)", Some(('"', 5))),
            ("&apos;", Some(('\'', 6))),
            ("&num;", Some(('#', 5))),
            ("&#34;", Some(('"', 5))),
            ("&#39;", Some(('\'', 5))),
            ("&#0039;", Some(('\'', 7))),
            ("&#39", Some(('\'', 4))),
            ("&#x22;", Some(('"', 6))),
            ("&#X27;", Some(('\'', 6))),
            ("&#x0027", Some(('\'', 7))),
            ("&#35;", Some(('#', 5))),
            ("&#38;b", Some(('&', 5))),
            ("&#x26;", Some(('&', 6))),
            (
                "&#99999999999999999999;",
                Some((char::REPLACEMENT_CHARACTER, 23)),
            ),
            ("&#xD800;", Some((char::REPLACEMENT_CHARACTER, 8))),
            ("", None),
            ("&", None),
            ("&amp;", None),
            ("&Quot;", None),
            ("&quotx", None),
            ("&quot=", None),
            ("&apos", None),
            ("&#;", None),
            ("&#x;", None),
            ("quot;", None),
        ] {
            assert_eq!(char_reference(s), expected, "{s}");
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
            // CSS escapes are undone for the path; the suffix stays as written.
            ("a\\(1\\).png", Some(("a(1).png", ""))),
            ("a\\28 1\\29 .png?v=\\31", Some(("a(1).png", "?v=\\31"))),
            ("a\\ b.png", Some(("a b.png", ""))),
            ("a\\?b.png?q", Some(("a?b.png", "?q"))),
            ("a\\#b.png", Some(("a#b.png", ""))),
            // Percent-decoded after unescaping; undecodable sequences stay as written.
            (
                "fonts/My%20Font.woff2?v=1",
                Some(("fonts/My Font.woff2", "?v=1")),
            ),
            ("%C3%A9t%C3%A9.png", Some(("été.png", ""))),
            ("100%.png", Some(("100%.png", ""))),
            ("a%2.png", Some(("a%2.png", ""))),
            ("%FF.png", Some(("%FF.png", ""))),
            ("data:font/woff2;base64,AAAA", None),
            ("http://example.com/a.png", None),
            ("HTTPS://example.com/a.png", None),
            ("//cdn.example.com/a.png", None),
            ("/abs/a.png", None),
            ("\\2f abs/a.png", None),
            ("d\\61ta:x", None),
            ("#filter", None),
            ("asset:3f9a1c2b7e0d", None),
            ("blob:xyz", None),
            ("", None),
            ("   ", None),
            ("?only-query", None),
        ] {
            let actual = local_reference(reference);
            let actual = actual.as_ref().map(|r| (r.path.as_str(), r.suffix));
            assert_eq!(actual, expected, "{reference:?}");
        }
    }

    /// Percent-decoding cannot smuggle a reference past the local-path checks: a path that
    /// would decode to a root-absolute path, a scheme, another directory (an encoded `/` or
    /// `\`) or a NUL goes to the resolver undecoded, as one literal name per written segment.
    /// It is never left out silently: unresolvable, it is `MissingAsset` when not lenient.
    #[test]
    fn decoded_references_are_checked_like_written_ones() {
        for (reference, expected) in [
            ("%2Fetc%2Fx.png", "%2Fetc%2Fx.png"),
            ("%2fetc%2fx.png", "%2fetc%2fx.png"),
            ("%2F%2Fexample.com%2Fbg.png", "%2F%2Fexample.com%2Fbg.png"),
            (
                "%5C%5Cserver%5Cshare%5Cx.png",
                "%5C%5Cserver%5Cshare%5Cx.png",
            ),
            ("..%2F..%2Fbg.png", "..%2F..%2Fbg.png"),
            ("..%2Fx.png", "..%2Fx.png"),
            ("fonts%2Fa.woff2", "fonts%2Fa.woff2"),
            ("a%5Cb.png", "a%5Cb.png"),
            ("a%00.png", "a%00.png"),
            // the whole path stays undecoded, also its harmless sequences
            ("my%20dir%2Fa.png", "my%20dir%2Fa.png"),
            ("C%3A/x/bg.png", "C%3A/x/bg.png"),
            ("C%3a%5Cx.png", "C%3a%5Cx.png"),
            ("d%61ta:x", "d%61ta:x"),
            ("h%74tps://example.com/a.png", "h%74tps://example.com/a.png"),
            // CSS escapes are still undone
            ("\\2e\\2e %2Fx.png", "..%2Fx.png"),
            // what decodes to an ordinary relative path is decoded
            ("%2E%2E/img/a.png", "../img/a.png"),
            ("a%2Bb.png", "a+b.png"),
            ("100%25.png", "100%.png"),
            ("fonts/a%3Ab.woff", "fonts/a:b.woff"),
        ] {
            let actual = local_reference(reference);
            let actual = actual.as_ref().map(|r| (r.path.as_str(), r.suffix));
            assert_eq!(actual, Some((expected, "")), "{reference:?}");
        }
        // Written forms the checks leave untouched stay untouched.
        for reference in [
            "/etc/x.png",
            "\\\\server/x.png",
            "\\2f etc/x.png",
            "C:/x.png",
        ] {
            assert_eq!(local_reference(reference), None, "{reference:?}");
        }
        // The suffix is split off first, as written.
        let r = local_reference("..%2Fx.png?v=%2F1#a%2Fb").expect("relative");
        assert_eq!((r.path.as_str(), r.suffix), ("..%2Fx.png", "?v=%2F1#a%2Fb"));
    }

    #[test]
    fn plain_queries_have_no_escapes_and_nothing_that_ends_the_query_scan() {
        for (suffix, expected) in [
            ("", ""),
            ("#frag", "#frag"),
            ("?v=1", "?v=1"),
            ("?#iefix", "?#iefix"),
            ("?a=1&amp;b=2", "?a=1&amp;b=2"),
            ("?é=1", "?é=1"),
            ("?v=\\31", "?v=1"),
            ("?x=\\(1\\)", "?x=%281%29"),
            ("?a\\ b", "?a%20b"),
            ("?a b", "?a%20b"),
            ("?x='1'\"", "?x=%271%27%22"),
            ("?a\\\\b", "?a%5Cb"),
            ("?a<b>", "?a%3Cb%3E"),
            ("?a\\#b#c", "?a%23b#c"),
            ("?a=1&quot;b&#39;c", "?a=1%26quot;b%26#39;c"),
            // `&#35;` is a query `…&` and a fragment `#35;…` that the scan must not join
            ("?a&#35;b", "?a%26#35;b"),
            // any `&` whose numeric reference runs on into the fragment, whatever it stands for
            ("?v=\\31&#1\\(x\\)", "?v=1%26#1\\(x\\)"),
            ("?&#1\\31", "?%26#1\\31"),
            ("?a&#2(", "?a%26#2("),
            ("?v=1&#1&#35;x", "?v=1%26#1&#35;x"),
            ("?a&#x1;b", "?a%26#x1;b"),
            // a `&` that forms no reference with the fragment stays
            ("?a&#frag", "?a&#frag"),
            ("?a,b,", "?a,b%2C"),
            ("?a\\9 b", "?a%09b"),
            // the fragment stays as written
            ("?v=\\31#ic\\on", "?v=1#ic\\on"),
        ] {
            assert_eq!(plain_query(suffix), expected, "{suffix:?}");
            // what the standalone emitter drops is the whole query
            let text = format!("asset:{ID}{}", plain_query(suffix));
            let p = &find_placeholders(&text)[0];
            let query = expected.split('#').next().unwrap_or_default();
            assert_eq!(&text[p.id_end..p.query_end], query, "{suffix:?}");
        }
    }

    #[test]
    fn css_escapes() {
        for (escaped, expected) in [
            ("plain", "plain"),
            ("\\(\\)\\\"\\'\\\\", "()\"'\\"),
            ("\\41 B", "AB"),
            ("\\41\tB", "AB"),
            ("\\41\r\nB", "AB"),
            ("\\41  B", "A B"),
            ("\\000041B", "AB"),
            ("\\0000411", "A1"),
            ("\\0", "\u{FFFD}"),
            ("\\d800", "\u{FFFD}"),
            ("\\110000", "\u{FFFD}"),
            ("\\1F600x", "\u{1F600}x"),
            ("a\\\nb", "ab"),
            ("trailing\\", "trailing"),
            ("\\é", "é"),
        ] {
            assert_eq!(css_unescape(escaped), expected, "{escaped:?}");
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
            (
                "x{b:-webkit-image-set(url(a.png) 1x)}",
                format!("x{{b:-webkit-image-set(url(asset:{a}) 1x)}}"),
            ),
            (
                "x{--bg:url(a.png);b:var(--bg)}",
                format!("x{{--bg:url(asset:{a});b:var(--bg)}}"),
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

    /// Strict mode with a resolver that knows only `bg.png`: text that merely looks like a
    /// `url()` is not a reference, and real references after tricky strings are found.
    #[test]
    fn css_strings_and_identifiers_are_not_urls() {
        let strict = |css: &str| {
            let mut registry = AssetRegistry::new();
            register_css_urls(css, &mut registry, &mut known(&["bg.png"]), false)
                .map(|out| (out, registry.len()))
        };
        let bg = id_of("bg.png");
        // `/*` inside a string does not start a comment.
        for (css, expected) in [
            (
                ".a::before{content:\"/*\"} .b{background:url(bg.png)}",
                format!(".a::before{{content:\"/*\"}} .b{{background:url(asset:{bg})}}"),
            ),
            (
                "a[href$='/*']{} .b{background:url(bg.png)}",
                format!("a[href$='/*']{{}} .b{{background:url(asset:{bg})}}"),
            ),
            (
                ".a{content:\"\\\"/*\"} .b{background:url('bg.png')}",
                format!(".a{{content:\"\\\"/*\"}} .b{{background:url('asset:{bg}')}}"),
            ),
            // A bad string ends at the newline; what follows is CSS again.
            (
                ".a{content:\"oops\n} .b{background:url(bg.png)}",
                format!(".a{{content:\"oops\n}} .b{{background:url(asset:{bg})}}"),
            ),
        ] {
            assert_eq!(strict(css).unwrap(), (expected, 1), "{css}");
        }
        // `url(` inside strings or glued to an identifier is not a reference.
        for css in [
            ".a::after{content:\"url(x.png)\"}",
            ".a::after{content:'see url(x.png)'}",
            ".a::after{content:\"\\\"url(x.png)\"}",
            ".a{--x:myurl(a.png)}",
            ".a{--x:my-url(\"a.png\")}",
            ".a{content:\"unterminated url(x.png)",
        ] {
            assert_eq!(strict(css).unwrap(), (css.to_string(), 0), "{css}");
        }
        // Escaped unquoted references are recognized (and missing ones reported).
        let err = strict(".a{background:url(a\\(1\\).png)}").unwrap_err();
        assert!(
            matches!(&err, Error::MissingAsset { path, .. } if path == Path::new("a(1).png")),
            "{err:?}"
        );
    }

    #[test]
    fn css_escaped_and_percent_encoded_references_are_decoded_for_the_resolver() {
        let mut calls = Vec::new();
        let mut resolver = |r: &str| {
            calls.push(r.to_string());
            Some(AssetData::Bytes(r.as_bytes().to_vec()))
        };
        let mut registry = AssetRegistry::new();
        let css = "a{b:url(img\\(1\\).png)} c{d:url(\"fonts/My%20Font.woff2?v=1\")} \
                   e{f:url(img\\28 1\\29 .png)} g{h:url('it\\'s.svg')}";
        let out = register_css_urls(css, &mut registry, &mut resolver, false).unwrap();
        assert_eq!(calls, ["img(1).png", "fonts/My Font.woff2", "it's.svg"]);
        let (img, font, svg) = (
            id_of("img(1).png"),
            id_of("fonts/My Font.woff2"),
            id_of("it's.svg"),
        );
        assert_eq!(
            out,
            format!(
                "a{{b:url(asset:{img})}} c{{d:url(\"asset:{font}?v=1\")}} \
                 e{{f:url(asset:{img})}} g{{h:url('asset:{svg}')}}"
            )
        );
        let font = registry.get(&font).unwrap();
        assert_eq!(
            (font.mime.as_str(), font.file_name_hint.as_str()),
            ("font/woff2", "My Font.woff2")
        );
        assert_eq!(registry.get(&svg).unwrap().mime, "image/svg+xml");

        // A missing reference is reported with the decoded path.
        let err = register_css_urls(
            "@font-face{src:url(fonts/My%20Font.woff2)}",
            &mut AssetRegistry::new(),
            &mut known(&[]),
            false,
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::MissingAsset { path, .. } if path == Path::new("fonts/My Font.woff2")),
            "{err:?}"
        );
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
        let out = Emitter::new(&registry, &serve).resolve_css(&text).unwrap();
        assert!(
            out.contains(&format!("url(/__presenter/deck-asset/{id}?#iefix)")),
            "{out}"
        );
    }

    #[test]
    fn standalone_query_drop_keeps_entity_escaped_closing_quotes() {
        let mut registry = AssetRegistry::new();
        let id = registry.register_bytes(b"x".to_vec(), "image/png", "a.png");
        let uri = data_uri("image/png", b"x");
        let standalone = OutputMode::Standalone;
        for quote in ["&quot;", "&#39;", "&#x27;", "&#34;", "&apos;"] {
            let html = format!("<div style=\"background:url({quote}asset:{id}?v=1{quote})\">");
            let out = Emitter::new(&registry, &standalone)
                .resolve_html(&html)
                .unwrap();
            assert_eq!(
                out,
                format!("<div style=\"background:url({quote}{uri}{quote})\">"),
                "{quote}"
            );
        }
        // Site and serve keep the query and the entity.
        let site = site();
        let html = format!("<div style=\"background:url(&quot;asset:{id}?v=1&quot;)\">");
        let out = Emitter::new(&registry, &site).resolve_html(&html).unwrap();
        assert_eq!(
            out,
            format!("<div style=\"background:url(&quot;assets/a-{id}.png?v=1&quot;)\">")
        );
    }

    #[test]
    fn a_placeholder_inside_a_dropped_query_goes_with_it() {
        let mut registry = AssetRegistry::new();
        let a = registry.register_bytes(vec![1], "image/png", "a.png");
        let b = registry.register_bytes(vec![2], "image/png", "b.png");
        let text = format!("<a href=\"asset:{a}?next=asset:{b}\">y</a>");
        let standalone = OutputMode::Standalone;
        let out = Emitter::new(&registry, &standalone)
            .resolve_html(&text)
            .unwrap();
        assert_eq!(
            out,
            format!("<a href=\"{}\">y</a>", data_uri("image/png", &[1]))
        );
        let serve = OutputMode::Serve;
        let out = Emitter::new(&registry, &serve).resolve_html(&text).unwrap();
        assert_eq!(
            out,
            format!(
                "<a href=\"/__presenter/deck-asset/{a}?next=/__presenter/deck-asset/{b}\">y</a>"
            )
        );
    }

    /// ir.md §3 (audit F16): in a document, placeholders are replaced in attribute values and
    /// in `<style>`/`<script>` elements only; the same text in prose, code or notes stays.
    #[test]
    fn html_text_content_is_never_rewritten() {
        let mut registry = AssetRegistry::new();
        let id = registry.register_bytes(vec![7], "image/png", "a.png");
        let uri = data_uri("image/png", &[7]);
        let standalone = OutputMode::Standalone;
        let resolve = |html: &str| {
            Emitter::new(&registry, &standalone)
                .resolve_html(html)
                .unwrap()
        };
        for text in [
            format!("<p>The id asset:{id} is text.</p>"),
            format!("<p><code>asset:{id}</code></p>"),
            format!("<pre><code>&lt;img src=&quot;asset:{id}&quot;&gt;</code></pre>"),
            format!("<aside class=\"notes\">asset:{id}</aside>"),
            format!("<!-- <img src=\"asset:{id}\"> -->"),
            format!("<textarea><img src=\"asset:{id}\"></textarea>"),
            format!("<p title='x'>asset:{id}</p>"),
            // Unregistered ids in text are not errors either.
            "<p>asset:0123456789ab and `asset:3f9a1c2b7e0d`</p>".to_string(),
        ] {
            assert_eq!(resolve(&text), text);
        }
        // Attribute values (any quoting), inline styles and <style>/<script> contents are.
        for (html, expected) in [
            (
                format!("<img src=\"asset:{id}\" alt=\"asset:{id}\">"),
                format!("<img src=\"{uri}\" alt=\"{uri}\">"),
            ),
            (
                format!("<img src=asset:{id}><p>asset:{id}</p>"),
                format!("<img src={uri}><p>asset:{id}</p>"),
            ),
            (
                format!("<div style='background:url(asset:{id}?v=1)'>asset:{id}</div>"),
                format!("<div style='background:url({uri})'>asset:{id}</div>"),
            ),
            (
                format!("<STYLE>a{{b:url(asset:{id})}}</Style><p>asset:{id}</p>"),
                format!("<STYLE>a{{b:url({uri})}}</Style><p>asset:{id}</p>"),
            ),
            (
                format!("<script>show('asset:{id}')</script>asset:{id}"),
                format!("<script>show('{uri}')</script>asset:{id}"),
            ),
        ] {
            assert_eq!(resolve(&html), expected, "{html}");
        }
    }

    #[test]
    fn unknown_ids_in_a_document_name_where_they_are() {
        let registry = AssetRegistry::new();
        let standalone = OutputMode::Standalone;
        let mut emitter = Emitter::new(&registry, &standalone);
        let err = emitter
            .resolve_html("<p>asset:0123456789ab</p><img src=\"asset:0123456789ab\">")
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "unknown asset id '0123456789ab': the placeholder 'asset:0123456789ab' in a `src` \
             attribute of the document does not name a registered asset"
        );
        let err = emitter
            .resolve_html("<style>x{y:url(asset:0123456789ab)}</style>")
            .unwrap_err();
        assert!(err.to_string().contains("in a <style> element"), "{err}");
        // Stylesheets are scanned whole.
        let err = emitter.resolve_css("/* asset:0123456789ab */").unwrap_err();
        assert!(err.to_string().contains("'asset:0123456789ab'"), "{err}");
    }

    /// ir.md §3 (audit F20): a site-mode file name hashes the bytes, not the id (which for a
    /// file asset hashes its absolute path).
    #[test]
    fn site_file_names_hash_the_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let (one, two) = (tmp.path().join("one"), tmp.path().join("two"));
        for dir in [&one, &two] {
            std::fs::create_dir(dir).unwrap();
            std::fs::write(dir.join("plot.png"), b"same bytes").unwrap();
        }
        let mut registry = AssetRegistry::new();
        let a = registry.register_file(&one.join("plot.png"));
        let b = registry.register_file(&two.join("plot.png"));
        assert_ne!(a, b, "file ids hash the path");
        let mode = site();
        let mut emitter = Emitter::new(&registry, &mode);
        let hash = content_hash(b"same bytes");
        let name = format!("plot-{hash}.png");
        assert_eq!(emitter.url_for(&a).unwrap(), format!("assets/{name}"));
        assert_eq!(emitter.url_for(&b).unwrap(), format!("assets/{name}"));
        assert_eq!(emitter.files.len(), 1, "same bytes and hint: one file");
        assert_eq!(emitter.files[0].path, Path::new("out/assets").join(&name));
        // Changed bytes, new name.
        std::fs::write(one.join("plot.png"), b"edited").unwrap();
        let mut emitter = Emitter::new(&registry, &mode);
        let edited = format!("assets/plot-{}.png", content_hash(b"edited"));
        assert_eq!(emitter.url_for(&a).unwrap(), edited);
    }

    #[test]
    fn text_without_placeholders_is_returned_unchanged() {
        let registry = AssetRegistry::new();
        for mode in [OutputMode::Standalone, site(), OutputMode::Serve] {
            let mut emitter = Emitter::new(&registry, &mode);
            let html = "<p>asset:3f9a1c2b7e0d1 is a 13-digit hash, not a placeholder</p>";
            assert_eq!(emitter.resolve_html(html).unwrap(), html);
            assert_eq!(emitter.resolve_css("").unwrap(), "");
            assert_eq!(emitter.resolve_css_in_assets_dir("").unwrap(), "");
            assert!(emitter.files.is_empty());
        }
    }

    #[test]
    fn stylesheets_in_assets_dir_get_bare_file_names_and_share_files() {
        let mut registry = AssetRegistry::new();
        let a = registry.register_bytes(vec![1], "image/png", "a.png");
        let b = registry.register_bytes(vec![2], "font/woff2", "b.woff2");
        let css = format!("x{{b:url(asset:{a}?v=1)}} @font-face{{src:url('asset:{b}')}}");
        let mode = site();
        let mut emitter = Emitter::new(&registry, &mode);
        assert_eq!(emitter.url_for(&a).unwrap(), format!("assets/a-{a}.png"));
        assert_eq!(
            emitter.resolve_css_in_assets_dir(&css).unwrap(),
            format!("x{{b:url(a-{a}.png?v=1)}} @font-face{{src:url('b-{b}.woff2')}}")
        );
        assert_eq!(
            emitter.resolve_css(&css).unwrap(),
            format!("x{{b:url(assets/a-{a}.png?v=1)}} @font-face{{src:url('assets/b-{b}.woff2')}}")
        );
        let paths: Vec<&Path> = emitter.files.iter().map(|f| f.path.as_path()).collect();
        let (pa, pb) = (
            format!("out/assets/a-{a}.png"),
            format!("out/assets/b-{b}.woff2"),
        );
        assert_eq!(paths, [Path::new(&pa), Path::new(&pb)]);
        // Location does not matter in the other modes.
        for mode in [OutputMode::Standalone, OutputMode::Serve] {
            let mut emitter = Emitter::new(&registry, &mode);
            assert_eq!(
                emitter.resolve_css_in_assets_dir(&css).unwrap(),
                emitter.resolve_css(&css).unwrap()
            );
            assert!(emitter.files.is_empty());
        }
    }

    #[test]
    fn carried_files_are_not_emitted_again() {
        let mut registry = AssetRegistry::new();
        let a = registry.register_bytes(vec![1], "image/png", "a.png");
        let b = registry.register_bytes(vec![2], "image/png", "b.png");
        let mode = site();
        let mut carried = Vec::new();
        for id in [&a, &b, &a, &a, &b] {
            let mut emitter = Emitter::with_files(&registry, &mode, std::mem::take(&mut carried));
            emitter.url_for(id).unwrap();
            emitter.resolve_html(&format!("asset:{id}")).unwrap();
            carried = emitter.files;
        }
        // Assigning the public field works the same as `with_files`.
        let mut emitter = Emitter::new(&registry, &mode);
        emitter.files = carried;
        emitter
            .resolve_css_in_assets_dir(&format!("url(asset:{a}) url(asset:{b})"))
            .unwrap();
        let paths: Vec<&Path> = emitter.files.iter().map(|f| f.path.as_path()).collect();
        let (pa, pb) = (
            format!("out/assets/a-{a}.png"),
            format!("out/assets/b-{b}.png"),
        );
        assert_eq!(paths, [Path::new(&pa), Path::new(&pb)]);
        assert_eq!(emitter.files[1].bytes, [2]);
    }

    #[test]
    fn output_mode_is_clone_debug_eq() {
        let mode = site();
        assert_eq!(mode.clone(), mode);
        assert_ne!(mode, OutputMode::Serve);
        assert!(format!("{:?}", OutputMode::Standalone).contains("Standalone"));
    }
}
