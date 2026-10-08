//! Document rendering and the template helpers of `docs/spec/theme-format.md` §4.
//!
//! [`render`] builds the context, compiles the theme stylesheet, renders `deck.html` in a
//! fresh environment of the theme chain (so disk edits are always seen) and resolves every
//! `asset:<id>` placeholder of the result with one [`Emitter`] pass.
//!
//! **Helpers.** minijinja 2.24 stores functions as `Send + Sync + 'static` boxes, so a helper
//! cannot hold an `Emitter<'a>` borrowing the caller's registry. The helpers share an
//! `Arc<Mutex<HelperState>>` that owns a copy of the deck's registry (plus everything the
//! helpers register), the output mode and the site-mode file list; each call that emits
//! creates a short-lived emitter with [`Emitter::with_files`] and puts the files back
//! (pattern 2 of the `assets::emit` module docs). What each helper returns:
//!
//! | helper | standalone | site | serve |
//! |---|---|---|---|
//! | `style(path)` | `<style>` with `url()`s inlined as `data:` | `<link rel="stylesheet" href="assets/<name>-<hash>.css">`, file emitted (its `url()`s emitted next to it) | `<link rel="stylesheet" href="/__presenter/asset/<path>">` |
//! | `script(path)` | `<script>` with the file inlined | `<script src="assets/<name>-<hash>.js">`, file emitted | `<script src="/__presenter/asset/<path>">` |
//! | `asset(path)` | `data:` URI | `assets/<name>-<hash>.<ext>`, file emitted | `/__presenter/asset/<path>` |
//! | `theme_style()` | `<style id="presenter-theme">` | `<link id="presenter-theme" … href="assets/theme-<hash>.css">` | `<link id="presenter-theme" … href="/__presenter/theme.css?v=<build id>">` |
//! | `reveal_init()` | `<script>Reveal.initialize(Object.assign(<config_json>, { plugins: [...] }));</script>` | same | same |
//! | `live_reload()` | empty | empty | `<script src="/__presenter/live-reload.js"></script>` (when `build.live_reload`) |
//!
//! Every helper fails with a template error naming the path when [`resolve_helper_path`]
//! finds nothing, in every mode. Inlined text never closes its element early: `</style`
//! and `</script` inside inlined files are written `<\/style` / `<\/script`.
//! Stylesheets from `vendor/` and `presenter/` resolve `url()`s leniently (KaTeX's
//! `.woff`/`.ttf` fallbacks stay as written); theme stylesheets must resolve every local
//! `url()` (relative to the stylesheet, then the chain's `assets/` directories, §5).
//!
//! **Filters.** `attrs` renders a map as ` k="v"` pairs (escaped; `none` values skipped);
//! `markdown` renders a string with comrak and the markdown-dialect §1 options (raw HTML
//! allowed, no front matter); a result that is one paragraph is unwrapped, so the filter
//! works inline (`<p class="subtitle">{{ meta.subtitle|markdown }}</p>`). Images in
//! template-rendered Markdown are not registered as assets (not supported). T16 switches the
//! filter to the Markdown frontend's `render_fragment`.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use minijinja::value::ValueKind;
use minijinja::{Environment, ErrorKind, Value};

use super::reveal::{self, PluginInfo};
use super::{BuildMode, RenderInput, RenderedDocument, context};
use crate::assets::emit::{EmittedFile, Emitter, OutputMode, data_uri, register_css_urls};
use crate::assets::registry::{AssetData, AssetRegistry};
use crate::error::{Diagnostic, Diagnostics, Error, Result, template_error};
use crate::ir::{Deck, Slide};
use crate::theme::{Theme, normalize_rel};

/// URL prefix of the dev server route that serves helper paths (live-reload.md §1).
pub const SERVE_HELPER_PREFIX: &str = "/__presenter/asset/";

/// URL of the dev server's theme stylesheet (live-reload.md §1).
pub const SERVE_THEME_CSS: &str = "/__presenter/theme.css";

/// URL of the live-reload client script (live-reload.md §1).
pub const SERVE_LIVE_RELOAD_JS: &str = "/__presenter/live-reload.js";

/// Renders `input` into the final document (see the module docs).
///
/// Errors: template errors ([`Error::Template`], with the source excerpt in serve mode), a
/// helper path that resolves to nothing (an [`Error::Template`] naming the path and the
/// template), theme stylesheets ([`Error::Style`]: SCSS before T42, an unresolvable
/// `url()`), theme manifest entries ([`Error::Theme`]), invalid front matter config tables
/// ([`Error::FrontMatter`]) and placeholders naming unregistered assets.
pub fn render(input: RenderInput<'_>) -> Result<RenderedDocument> {
    let config = context::effective_config(input.config, &input.deck.meta, input.theme)?;
    let input = RenderInput {
        config: &config,
        ..input
    };
    let mut diagnostics = Diagnostics::new();
    let ctx = context::build_context(&input, &mut diagnostics)?;
    check_background_videos(&input, &mut diagnostics);

    let mut state = HelperState {
        theme: input.theme.clone(),
        mode: input.output.clone(),
        registry: input.assets.clone(),
        files: Vec::new(),
    };
    let raw_css = compile_theme_css(input.theme, &mut state.registry)?;
    let theme_css = state.with_emitter(|e| e.resolve_css_in_assets_dir(&raw_css))?;
    let extras = Extras {
        theme_css: Arc::from(theme_css.as_str()),
        reveal_init: Arc::from(init_script(&input).as_str()),
        live_reload: input.build.live_reload,
        build_id: input.build.id,
    };
    let state = Arc::new(Mutex::new(state));
    let html = render_deck(&input, &state, extras, &ctx)?;

    let mut guard = state.lock().map_err(|_| poisoned())?;
    let registry = std::mem::take(&mut guard.registry);
    let mut emitter =
        Emitter::with_files(&registry, &input.output, std::mem::take(&mut guard.files));
    let html = emitter.resolve_html(&html)?;
    let files = emitter.files;
    Ok(RenderedDocument {
        html,
        theme_css,
        files,
        slide_count: slide_count(input.deck),
        diagnostics,
        assets: registry,
    })
}

/// Number of slides: top-level slides plus their vertical children.
fn slide_count(deck: &Deck) -> usize {
    deck.slides
        .iter()
        .map(|slide| 1 + slide.children.len())
        .sum()
}

/// The `reveal_init()` script for `input` (whose config is already effective).
fn init_script(input: &RenderInput<'_>) -> String {
    let config = input.config;
    let features = &input.deck.features;
    let plugins: Vec<PluginInfo> = reveal::plugins(config, features);
    let config_json = reveal::reveal_config_json(config, &input.deck.meta, input.theme);
    let katex = reveal::math_enabled(config, features).then_some(&config.math.katex_options);
    reveal::reveal_init_script(&config_json, &plugins, katex)
}

/// Renders `deck.html` in a fresh environment with the helpers and filters registered.
fn render_deck(
    input: &RenderInput<'_>,
    state: &Arc<Mutex<HelperState>>,
    extras: Extras,
    ctx: &Value,
) -> Result<String> {
    let mut env = input
        .theme
        .environment(input.build.mode == BuildMode::Serve)?;
    register_helpers(&mut env, state, extras);
    let template = env
        .get_template("deck.html")
        .map_err(|e| template_error(e, env.debug()))?;
    template
        .render(ctx)
        .map_err(|e| template_error(e, env.debug()))
}

/// Warns about local background videos in standalone mode: the placeholder becomes a
/// `data:` URI, which reveal.js splits on `,` and cannot play.
fn check_background_videos(input: &RenderInput<'_>, diagnostics: &mut Diagnostics) {
    if !matches!(input.output, OutputMode::Standalone) {
        return;
    }
    let deck = input.deck;
    let mut check = |slide: &Slide, label: String| {
        let Some(video) = slide.attrs.get("data-background-video") else {
            return;
        };
        if !video.contains("asset:") {
            return;
        }
        let mut diagnostic = Diagnostic::warning(
            "standalone-background-video",
            format!(
                "{label}: the local background video cannot play in a standalone build \
                 (reveal.js cannot read it from a data: URI); build with --site or use presenter serve"
            ),
        )
        .with_path(&deck.source.path);
        diagnostic.span = slide.span.clone();
        diagnostics.push(diagnostic);
    };
    for (i, slide) in deck.slides.iter().enumerate() {
        check(slide, format!("slide {}", i + 1));
        for (j, child) in slide.children.iter().enumerate() {
            check(child, format!("slide {}.{}", i + 1, j + 1));
        }
    }
}

// ---------------------------------------------------------------------------------------
// Helper paths
// ---------------------------------------------------------------------------------------

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
/// Errors only when an existing file cannot be read.
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

/// The form of a normalized helper path.
enum PathForm<'k> {
    /// `vendor/…` or `presenter/…`: the embedded tree.
    Builtin,
    /// `@<name>/<rel>`.
    Theme { name: &'k str, rel: &'k str },
    /// Below the chain's `assets/` directories.
    Asset,
}

fn path_form(key: &str) -> PathForm<'_> {
    let (first, rest) = key.split_once('/').unwrap_or((key, ""));
    match first.strip_prefix('@') {
        Some(name) => PathForm::Theme { name, rel: rest },
        None if !rest.is_empty() && matches!(first, "vendor" | "presenter") => PathForm::Builtin,
        None => PathForm::Asset,
    }
}

fn mime_of(key: &str) -> String {
    mime_guess::from_path(key)
        .first_or_octet_stream()
        .essence_str()
        .to_string()
}

/// The helper path of `rel` written in a file whose helper path is `base`: `rel` joined to
/// `base`'s directory, staying inside `base`'s root (`vendor/`, `presenter/`, `@<theme>/`,
/// or the `assets/` directories). `None` when it would leave that root.
fn join_helper_path(base: &str, rel: &str) -> Option<String> {
    let (root, inner) = match path_form(base) {
        PathForm::Asset => ("", base),
        _ => base.split_once('/').unwrap_or((base, "")),
    };
    let dir = inner.rsplit_once('/').map_or("", |(dir, _)| dir);
    let joined = normalize_rel(&format!("{dir}/{rel}")).filter(|j| !j.is_empty())?;
    if root.is_empty() {
        // Stay below assets/: a path that now looks like another form does not count.
        matches!(path_form(&joined), PathForm::Asset).then_some(joined)
    } else {
        Some(format!("{root}/{joined}"))
    }
}

/// Where [`resolve_helper_path`] looked for `key`, for "unknown path" messages.
fn looked_in(key: &str) -> String {
    match path_form(key) {
        PathForm::Builtin if key.starts_with("vendor/") => "the embedded vendor tree".into(),
        PathForm::Builtin => "assets/presenter/".into(),
        PathForm::Theme { name, .. } => format!("the directory of theme '{name}'"),
        PathForm::Asset => "the assets/ directories of the theme chain".into(),
    }
}

// ---------------------------------------------------------------------------------------
// Theme stylesheet
// ---------------------------------------------------------------------------------------

/// Compiles the theme stylesheet (theme-format.md §5): every `[styles] entries` file of the
/// chain, parent first, concatenated (each preceded by a `/* @<theme>/<path> */` comment).
/// Relative `url()`s are resolved against the stylesheet's own directory, then the chain's
/// `assets/` directories, registered in `registry` as bytes and rewritten to `asset:<id>`
/// placeholders (the caller resolves them for its output mode).
///
/// Errors: [`Error::Style`] (`file` = the entry as a helper path, `@<theme>/<path>`) for a
/// `.scss`/`.sass` entry ("SCSS support arrives in T42"), another extension, a file that is
/// not UTF-8 or a `url()` that resolves to nothing; [`Error::Theme`] for an entry that does
/// not exist.
pub fn compile_theme_css(theme: &Theme, registry: &mut AssetRegistry) -> Result<String> {
    let mut css = String::new();
    for entry in theme.style_entries()? {
        let key = format!("@{}/{}", entry.theme, entry.rel_path);
        let style_error = |message: String| Error::Style {
            file: key.clone(),
            message,
        };
        let lower = entry.rel_path.to_ascii_lowercase();
        if lower.ends_with(".scss") || lower.ends_with(".sass") {
            return Err(style_error("SCSS support arrives in T42".into()));
        }
        if !lower.ends_with(".css") {
            return Err(style_error("a style entry must be a .css file".into()));
        }
        let text = String::from_utf8(entry.bytes)
            .map_err(|e| style_error(format!("not valid UTF-8: {e}")))?;
        let text = register_stylesheet_urls(theme, &key, &text, registry)?;
        let _ = writeln!(css, "/* {key} */");
        css.push_str(&text);
        if !css.ends_with('\n') {
            css.push('\n');
        }
    }
    Ok(css)
}

/// Rewrites the relative `url()`s of the stylesheet at helper path `key` to placeholders
/// (see [`compile_theme_css`]). Stylesheets from `vendor/`/`presenter/` are lenient and do
/// not fall back to the theme's `assets/`; others must resolve every reference.
fn register_stylesheet_urls(
    theme: &Theme,
    key: &str,
    css: &str,
    registry: &mut AssetRegistry,
) -> Result<String> {
    let lenient = matches!(path_form(key), PathForm::Builtin);
    let mut failure: Option<Error> = None;
    let result = {
        let mut resolve = |rel: &str| -> Option<AssetData> {
            let found = match join_helper_path(key, rel) {
                Some(joined) => resolve_helper_path(theme, &joined),
                None => Ok(None),
            };
            let found = match found {
                Ok(None) if !lenient => {
                    theme.read_asset(rel).map(|b| b.map(|b| (b, String::new())))
                }
                other => other,
            };
            match found {
                Ok(found) => found.map(|(bytes, _)| AssetData::Bytes(bytes)),
                Err(err) => {
                    failure.get_or_insert(err);
                    None
                }
            }
        };
        register_css_urls(css, registry, &mut resolve, lenient)
    };
    if let Some(err) = failure {
        return Err(err);
    }
    result.map_err(|err| match err {
        Error::MissingAsset { path, .. } => Error::Style {
            file: key.to_string(),
            message: format!(
                "url({}) not found next to the stylesheet or in the theme chain's assets/ directories",
                path.display()
            ),
        },
        other => other,
    })
}

// ---------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------

/// State shared by the helpers of one render (see the module docs).
struct HelperState {
    theme: Theme,
    mode: OutputMode,
    /// The deck's registry plus everything helpers registered.
    registry: AssetRegistry,
    /// Site-mode files emitted so far (the emitters' dedupe state).
    files: Vec<EmittedFile>,
}

/// Per-render values the helpers need besides the shared state.
struct Extras {
    theme_css: Arc<str>,
    reveal_init: Arc<str>,
    live_reload: bool,
    build_id: u64,
}

/// A file a helper path resolved to.
struct Found {
    /// The normalized helper path.
    key: String,
    bytes: Vec<u8>,
    mime: String,
}

impl HelperState {
    /// Runs `f` with an emitter that continues this render's file list.
    fn with_emitter<T>(&mut self, f: impl FnOnce(&mut Emitter<'_>) -> Result<T>) -> Result<T> {
        let files = std::mem::take(&mut self.files);
        let mut emitter = Emitter::with_files(&self.registry, &self.mode, files);
        let result = f(&mut emitter);
        self.files = emitter.files;
        result
    }

    /// Registers `bytes` and returns their URL for this mode (site: emits the file
    /// `<name>-<hash>.<ext>` named after `key`'s last segment).
    fn emit_bytes(&mut self, bytes: Vec<u8>, mime: &str, key: &str) -> Result<String> {
        let hint = key.rsplit('/').next().unwrap_or(key);
        let id = self.registry.register_bytes(bytes, mime, hint);
        self.with_emitter(|emitter| emitter.url_for(&id))
    }

    /// The file behind helper path `path`, or an error naming `helper` and `path`.
    fn lookup(&self, helper: &str, path: &str) -> Result<Found> {
        let unknown = |key: &str| {
            Error::Other(format!(
                "{helper}(\"{path}\"): unknown path (looked in {})",
                looked_in(key)
            ))
        };
        let key = normalize_helper_path(path).ok_or_else(|| unknown(path))?;
        let (bytes, mime) = resolve_helper_path(&self.theme, &key)?.ok_or_else(|| unknown(&key))?;
        Ok(Found { key, bytes, mime })
    }

    fn style(&mut self, path: &str) -> Result<String> {
        let found = self.lookup("style", path)?;
        if matches!(self.mode, OutputMode::Serve) {
            return Ok(format!(
                r#"<link rel="stylesheet" href="{}">"#,
                serve_url(&found.key)
            ));
        }
        let css = utf8(found.bytes, "style", path)?;
        let css = register_stylesheet_urls(&self.theme, &found.key, &css, &mut self.registry)?;
        if matches!(self.mode, OutputMode::Site { .. }) {
            let css = self.with_emitter(|e| e.resolve_css_in_assets_dir(&css))?;
            let url = self.emit_bytes(css.into_bytes(), "text/css", &found.key)?;
            return Ok(format!(r#"<link rel="stylesheet" href="{}">"#, attr(&url)));
        }
        let css = self.with_emitter(|e| e.resolve_css(&css))?;
        Ok(format!("<style>\n{}</style>", raw_text(&css, "style")))
    }

    fn script(&mut self, path: &str) -> Result<String> {
        let found = self.lookup("script", path)?;
        match &self.mode {
            OutputMode::Serve => Ok(format!(
                r#"<script src="{}"></script>"#,
                serve_url(&found.key)
            )),
            OutputMode::Site { .. } => {
                let url = self.emit_bytes(found.bytes, &found.mime, &found.key)?;
                Ok(format!(r#"<script src="{}"></script>"#, attr(&url)))
            }
            OutputMode::Standalone => {
                let js = utf8(found.bytes, "script", path)?;
                Ok(format!("<script>\n{}</script>", raw_text(&js, "script")))
            }
        }
    }

    fn asset(&mut self, path: &str) -> Result<String> {
        let found = self.lookup("asset", path)?;
        match &self.mode {
            OutputMode::Serve => Ok(serve_url(&found.key)),
            OutputMode::Site { .. } => {
                let url = self.emit_bytes(found.bytes, &found.mime, &found.key)?;
                Ok(attr(&url).into_owned())
            }
            OutputMode::Standalone => Ok(data_uri(&found.mime, &found.bytes)),
        }
    }

    fn theme_style(&mut self, css: &str, build_id: u64) -> Result<String> {
        match &self.mode {
            OutputMode::Serve => Ok(format!(
                r#"<link id="presenter-theme" rel="stylesheet" href="{SERVE_THEME_CSS}?v={build_id}">"#
            )),
            OutputMode::Site { .. } => {
                let url = self.emit_bytes(css.as_bytes().to_vec(), "text/css", "theme.css")?;
                Ok(format!(
                    r#"<link id="presenter-theme" rel="stylesheet" href="{}">"#,
                    attr(&url)
                ))
            }
            OutputMode::Standalone => Ok(format!(
                "<style id=\"presenter-theme\">\n{}</style>",
                raw_text(css, "style")
            )),
        }
    }
}

/// Registers the §4 helpers and filters on `env`.
fn register_helpers(
    env: &mut Environment<'static>,
    state: &Arc<Mutex<HelperState>>,
    extras: Extras,
) {
    let st = Arc::clone(state);
    env.add_function("style", move |path: String| call(&st, |s| s.style(&path)));
    let st = Arc::clone(state);
    env.add_function("script", move |path: String| call(&st, |s| s.script(&path)));
    let st = Arc::clone(state);
    env.add_function("asset", move |path: String| call(&st, |s| s.asset(&path)));
    let st = Arc::clone(state);
    let Extras {
        theme_css,
        reveal_init,
        live_reload,
        build_id,
    } = extras;
    env.add_function("theme_style", move || {
        call(&st, |s| s.theme_style(&theme_css, build_id))
    });
    env.add_function("reveal_init", move || {
        Value::from_safe_string(reveal_init.to_string())
    });
    env.add_function("live_reload", move || {
        let tag = if live_reload {
            format!(r#"<script src="{SERVE_LIVE_RELOAD_JS}"></script>"#)
        } else {
            String::new()
        };
        Value::from_safe_string(tag)
    });
    env.add_filter("attrs", attrs_filter);
    env.add_filter("markdown", markdown_filter);
}

/// Runs a helper on the locked state; its HTML result is marked safe.
fn call(
    state: &Mutex<HelperState>,
    helper: impl FnOnce(&mut HelperState) -> Result<String>,
) -> std::result::Result<Value, minijinja::Error> {
    let mut guard = state
        .lock()
        .map_err(|_| helper_error(poisoned().to_string()))?;
    helper(&mut guard)
        .map(Value::from_safe_string)
        .map_err(|err| helper_error(err.to_string()))
}

fn helper_error(message: String) -> minijinja::Error {
    minijinja::Error::new(ErrorKind::InvalidOperation, message)
}

fn poisoned() -> Error {
    Error::Other("template helper state is unusable after a panic in a helper".into())
}

/// `bytes` as text, or an error naming the helper call.
fn utf8(bytes: Vec<u8>, helper: &str, path: &str) -> Result<String> {
    String::from_utf8(bytes)
        .map_err(|e| Error::Other(format!("{helper}(\"{path}\"): not valid UTF-8: {e}")))
}

/// `text` for inlining in a `<tag>` raw-text element: every `</tag` (ASCII case-insensitive)
/// becomes `<\/tag` (an escaped `/` in JS strings, regexes and CSS strings), and the text
/// ends with a line break.
fn raw_text(text: &str, tag: &str) -> String {
    let mut out = String::with_capacity(text.len() + 1);
    let mut last = 0;
    for (at, _) in text.match_indices("</") {
        let name = text.as_bytes().get(at + 2..at + 2 + tag.len());
        if name.is_some_and(|name| name.eq_ignore_ascii_case(tag.as_bytes())) {
            out.push_str(&text[last..at]);
            out.push_str("<\\/");
            last = at + 2;
        }
    }
    out.push_str(&text[last..]);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// The dev server URL of helper path `key`: [`SERVE_HELPER_PREFIX`] + `key`, with every byte
/// other than ASCII letters, digits and `-._~/@` percent-encoded.
fn serve_url(key: &str) -> String {
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

/// `text` escaped for a double-quoted attribute value.
fn attr(text: &str) -> Cow<'_, str> {
    html_escape::encode_double_quoted_attribute(text)
}

// ---------------------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------------------

/// `attrs`: a map → ` k="v"` pairs in map order, keys and values escaped; `none`/undefined
/// values are skipped and an undefined or `none` map renders nothing.
fn attrs_filter(map: Value) -> std::result::Result<Value, minijinja::Error> {
    let mut out = String::new();
    if map.is_undefined() || map.is_none() {
        return Ok(Value::from_safe_string(out));
    }
    if map.kind() != ValueKind::Map {
        return Err(minijinja::Error::new(
            ErrorKind::InvalidOperation,
            format!("attrs filter expects a map, got {}", map.kind()),
        ));
    }
    for key in map.try_iter()? {
        let value = map.get_item(&key)?;
        if value.is_undefined() || value.is_none() {
            continue;
        }
        let key = text_of(&key);
        let value = text_of(&value);
        let _ = write!(out, " {}=\"{}\"", attr(&key), attr(&value));
    }
    Ok(Value::from_safe_string(out))
}

/// A value's plain text: strings as they are, anything else in its display form.
fn text_of(value: &Value) -> String {
    match value.as_str() {
        Some(text) => text.to_string(),
        None => value.to_string(),
    }
}

/// `markdown`: renders a string with comrak (see the module docs); none/undefined → empty.
fn markdown_filter(text: Value) -> Value {
    if text.is_undefined() || text.is_none() {
        return Value::from_safe_string(String::new());
    }
    let html = comrak::markdown_to_html(&text_of(&text), &markdown_options());
    Value::from_safe_string(unwrap_paragraph(&html).to_string())
}

/// The markdown-dialect §1 comrak options, without front matter (the filter renders
/// fragments).
fn markdown_options() -> comrak::Options<'static> {
    let mut o = comrak::Options::default();
    o.extension.table = true;
    o.extension.strikethrough = true;
    o.extension.tasklist = true;
    o.extension.autolink = true;
    o.extension.footnotes = true;
    o.extension.description_lists = true;
    o.extension.alerts = true;
    o.extension.math_dollars = true;
    o.extension.math_code = true;
    o.extension.block_directive = true;
    o.extension.header_attributes = true;
    o.extension.fenced_code_attributes = true;
    o.extension.link_attributes = true;
    o.extension.inline_code_attributes = true;
    o.parse.smart = false;
    o.parse.relaxed_autolinks = false;
    o.render.r#unsafe = true;
    o.render.escape = false;
    o.render.hardbreaks = false;
    o.render.github_pre_lang = false;
    o.render.full_info_string = false;
    o.render.sourcepos = false;
    o
}

/// The content of `html` when it is exactly one `<p>…</p>` paragraph, else `html`.
fn unwrap_paragraph(html: &str) -> &str {
    match html
        .trim_end()
        .strip_prefix("<p>")
        .and_then(|rest| rest.strip_suffix("</p>"))
    {
        Some(inner) if !inner.contains("</p>") => inner,
        _ => html,
    }
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
    fn joined_paths_stay_in_their_root() {
        let cases = [
            (
                "vendor/katex/katex.min.css",
                "fonts/a.woff2",
                Some("vendor/katex/fonts/a.woff2"),
            ),
            ("vendor/katex/katex.min.css", "../../themes/x", None),
            (
                "@t/styles/theme.css",
                "../assets/bg.png",
                Some("@t/assets/bg.png"),
            ),
            ("@t/styles/theme.css", "../../x.png", None),
            ("@t/theme.css", "x.png", Some("@t/x.png")),
            ("css/a.css", "../img/b.png", Some("img/b.png")),
            ("css/a.css", "../vendor/reveal/dist/reveal.js", None),
            ("a.css", "./b.png", Some("b.png")),
            ("a.css", "..", None),
        ];
        for (base, rel, expected) in cases {
            assert_eq!(
                join_helper_path(base, rel).as_deref(),
                expected,
                "{base} + {rel}"
            );
        }
    }

    #[test]
    fn raw_text_never_closes_its_element() {
        assert_eq!(
            raw_text("a</script>b</SCRIPT x", "script"),
            "a<\\/script>b<\\/SCRIPT x\n"
        );
        assert_eq!(raw_text("a</scrip", "script"), "a</scrip\n");
        assert_eq!(raw_text("p{}\n", "style"), "p{}\n");
        assert_eq!(raw_text("x='</style>'", "style"), "x='<\\/style>'\n");
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
    }

    #[test]
    fn markdown_filter_unwraps_single_paragraphs() {
        let render = |s: &str| markdown_filter(Value::from(s)).to_string();
        assert_eq!(render("*Fast* tools"), "<em>Fast</em> tools");
        assert_eq!(render("a\n\nb"), "<p>a</p>\n<p>b</p>\n");
        assert_eq!(render("$x$"), "<span data-math-style=\"inline\">x</span>");
        assert_eq!(render("<b>raw</b>"), "<b>raw</b>");
        assert_eq!(markdown_filter(Value::UNDEFINED).to_string(), "");
    }

    #[test]
    fn attrs_filter_escapes_and_skips_none() {
        let map = Value::from_serialize(serde_json::json!({
            "data-a": "x\"<y>&",
            "data-b": "",
            "data-c": null,
            "data-n": 3
        }));
        let out = attrs_filter(map).unwrap();
        assert!(out.is_safe());
        assert_eq!(
            out.to_string(),
            r#" data-a="x&quot;&lt;y&gt;&amp;" data-b="" data-n="3""#
        );
        assert_eq!(attrs_filter(Value::UNDEFINED).unwrap().to_string(), "");
        assert!(attrs_filter(Value::from("x")).is_err());
    }
}
