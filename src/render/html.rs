//! Document rendering and the template helpers and filters of `docs/spec/theme-format.md` §4.
//!
//! [`render`] builds the context, compiles the theme stylesheet, renders `deck.html` in a
//! fresh environment of the theme chain (so disk edits are always seen) and resolves every
//! deck `asset:<id>` placeholder of the result with one [`Emitter`] pass over the deck's
//! registry.
//!
//! **Helpers.** minijinja 2.24 stores functions as `Send + Sync + 'static` boxes, so a helper
//! cannot hold an `Emitter<'a>` borrowing the caller's registry. The helpers share an
//! `Arc<Mutex<HelperState>>` that owns a registry of *their* files (theme stylesheet `url()`
//! targets, copied stylesheets, scripts and assets), the output mode, the site-mode file
//! list and their diagnostics. Each call that emits creates a short-lived emitter with
//! [`Emitter::with_files`], resolves its own placeholders at once and puts the files back
//! (pattern 2 of the `assets::emit` module docs). Helper output never contains
//! placeholders, so the final pass needs only the deck's registry (borrowed, not copied).
//! What each helper returns:
//!
//! | helper | standalone | site | serve |
//! |---|---|---|---|
//! | `style(path)` | `<style>` with `url()`s inlined as `data:` | `<link rel="stylesheet" href="assets/<name>-<hash>.css">`, file emitted (its `url()`s emitted next to it) | `<link rel="stylesheet" href="/__presenter/asset/<path>">` |
//! | `script(path)` | `<script>` with the file inlined | `<script src="assets/<name>-<hash>.js">`, file emitted | `<script src="/__presenter/asset/<path>">` |
//! | `asset(path)` | `data:` URI | `assets/<name>-<hash>.<ext>`, file emitted | `/__presenter/asset/<path>`, with `?v=<content version>` for a theme file |
//! | `theme_style()` | `<style id="presenter-theme">` | `<link id="presenter-theme" … href="assets/theme-<hash>.css">` | `<link id="presenter-theme" … href="/__presenter/theme.css?v=<build id>">` |
//! | `reveal_init()` | `<script>Reveal.initialize(Object.assign(<config_json>, { plugins: [...] }));</script>` | same | same |
//! | `live_reload()` | empty | empty | `<script src="/__presenter/live-reload.js"></script>` (when `build.live_reload`) |
//!
//! Every helper fails with a template error naming the path and why nothing was found
//! (an unknown `@<theme>`, a path that climbs out, or where it looked) when
//! [`resolve_helper_path`] finds nothing, in every mode. How
//! stylesheet `url()`s resolve (the same files in every mode) is described in the
//! `render::styles` module docs; in serve mode the theme stylesheet refers to its files as
//! `/__presenter/asset/<helper path>`, so the dev server needs no registry for them.
//!
//! **Content versions (serve mode; live-reload.md §4, audit F03).** A browser reuses the
//! image it has for a URL, also after it swapped a stylesheet or the slides in place, so the
//! URL of a theme-chain file that `asset()` returns or a stylesheet's `url()` names ends in
//! `?v=<content version>` (a number that changes with the file's bytes). The dev server
//! serves a theme stylesheet linked with `style()` through [`serve_stylesheet`], which
//! versions its `url()`s the same way. `script()` and `style()` URLs are not versioned: the
//! client re-requests the linked stylesheets on every build itself.
//!
//! **Inlined text** never ends its element early: `</style` and `</script` inside inlined
//! files are written `<\/style` / `<\/script`. Nor does it keep the element open: a script
//! that would leave the HTML tokenizer in the "script data double escaped" state (a `<!--`,
//! then `<script` with no `-->` after them) would make the element's own `</script>` fail
//! to close it, swallowing the scripts after it (`Reveal.initialize` included). Such a
//! script is inlined as `<script src="data:text/javascript;charset=utf-8;base64,…">`
//! instead, which is still self-contained. (The vendored scripts are inlined as text.)
//!
//! **Remote imports.** A standalone file promises to be self-contained, which a stylesheet
//! that `@import`s an absolute URL breaks: 8 of the 14 stock reveal.js themes import Google
//! Fonts (vendored-assets.md §2 says their fonts are inlined; they are not). The import is
//! kept (the theme's fonts load when online; offline the browser skips it) and a
//! `remote-import` warning names the stylesheet and the URL. Site and serve output load
//! such imports too, without a warning. In the theme stylesheet, imports of relative URLs
//! are inlined and the absolute ones moved to its top (T42, `render::styles`).
//!
//! **Filters.** `attrs` renders a map as ` k="v"` pairs (escaped; `none` values skipped);
//! `markdown` renders a string with the Markdown frontend's
//! [`render_fragment`] in notebook mode (§4: the
//! markdown-dialect §1 options without front matter, raw HTML allowed, directives, math and
//! code fences as in a deck, `parse.smart` = `config.markdown.smart_punctuation`;
//! `render.sourcepos` stays off because the positions would be relative to the template
//! value, not to the deck's source file); a result that is one paragraph is unwrapped, so
//! the filter works inline (`<p class="subtitle">{{ meta.subtitle|markdown }}</p>`).
//! Relative image paths resolve against the deck's directory and are registered with the
//! helpers' files, so they are inlined (standalone), emitted (site) or, in serve mode, also
//! inlined as `data:` URIs (the dev server only serves the deck's own assets). Its warnings
//! (e.g. `missing-asset`) join the render diagnostics with the deck's source file as their
//! path and without a span (the lines would be those of the template value), once each
//! however often the filter meets the same text; a missing image is never an error here
//! (`pipeline::build_deck` escalates it in `presenter build`, as it does the frontend's).
//! `json` is minijinja's `tojson` under the name §4 lists (its output is safe in HTML and
//! inside `<script>`).

use std::borrow::Cow;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use minijinja::value::ValueKind;
use minijinja::{Environment, ErrorKind, Value};

use super::paths::{
    helper_path_exists, normalize_helper_path, resolve_helper_path, serve_url, unknown_reason,
    versioned_serve_url,
};
use super::reveal::{self, PluginInfo};
use super::styles::{self, Targets, UrlRules};
use super::{BuildMode, RenderInput, RenderedDocument, context};
use crate::assets::emit::{EmittedFile, Emitter, OutputMode, data_uri};
use crate::assets::registry::AssetRegistry;
use crate::config::MarkdownConfig;
use crate::error::{Diagnostic, Diagnostics, Error, Result, template_error};
use crate::ir::{Deck, Slide};
use crate::markdown::comrak_options;
use crate::markdown::render::{RenderCtx, RenderMode, render_fragment};
use crate::theme::Theme;

/// URL prefix of the dev server route that serves helper paths (live-reload.md §1).
pub const SERVE_HELPER_PREFIX: &str = "/__presenter/asset/";

/// URL of the dev server's theme stylesheet (live-reload.md §1).
pub const SERVE_THEME_CSS: &str = "/__presenter/theme.css";

/// URL of the live-reload client script (live-reload.md §1).
pub const SERVE_LIVE_RELOAD_JS: &str = "/__presenter/live-reload.js";

/// The text the dev server serves for `css`, the stylesheet at the normalized helper path
/// `key` of `theme`'s chain (a file a template links with `style()`): each relative `url()`
/// that names a file (next to the stylesheet, as `style()` resolves it) becomes that file's
/// absolute dev-server URL, versioned like the theme stylesheet's (module docs, "Content
/// versions"); a reference that names nothing stays as written.
///
/// Errors only when an existing file cannot be read.
pub fn serve_stylesheet(theme: &Theme, key: &str, css: &str) -> Result<String> {
    let rules = UrlRules::for_style(key).leniently();
    styles::rewrite_urls(theme, key, css, rules, Targets::ServeUrls)
}

/// Renders `input` into the final document (see the module docs). `input.config` is used
/// as given: it must already be the effective config ([`super::RenderInput::config`]).
///
/// Errors: template errors ([`Error::Template`], with the source excerpt in serve mode), a
/// helper path that resolves to nothing or a stylesheet `url()` that does (an
/// [`Error::Template`] naming the path and the template), theme stylesheets
/// ([`Error::Style`]: a Sass error, a local `@import` that cannot be inlined, an
/// unresolvable `url()`), theme manifest entries
/// ([`Error::Theme`]) and placeholders naming unregistered assets.
pub fn render(input: RenderInput<'_>) -> Result<RenderedDocument> {
    let mut diagnostics = Diagnostics::new();
    let ctx = context::build_context(&input, &mut diagnostics)?;
    check_background_videos(&input, &mut diagnostics);

    let mut state = HelperState {
        theme: input.theme.clone(),
        mode: input.output.clone(),
        registry: AssetRegistry::new(),
        files: Vec::new(),
        diagnostics: Diagnostics::new(),
    };
    let theme_css = state.theme_css()?;
    let extras = Extras {
        theme_css: Arc::from(theme_css.as_str()),
        reveal_init: Arc::from(init_script(&input).as_str()),
        live_reload: input.build.live_reload,
        build_id: input.build.id,
        markdown: Arc::new(FilterDeck {
            smart_punctuation: input.config.markdown.smart_punctuation,
            base_dir: input.deck.source.base_dir.clone(),
            source: input.deck.source.path.clone(),
        }),
    };
    let state = Arc::new(Mutex::new(state));
    let html = render_deck(&input, &state, extras, &ctx)?;

    let (files, helper_diagnostics) = {
        let mut guard = state.lock().map_err(|_| poisoned())?;
        (
            std::mem::take(&mut guard.files),
            std::mem::take(&mut guard.diagnostics),
        )
    };
    diagnostics.extend(helper_diagnostics);
    let mut emitter = Emitter::with_files(input.assets, &input.output, files);
    let html = emitter.resolve_html(&html)?;
    Ok(RenderedDocument {
        html,
        theme_css,
        files: emitter.files,
        slide_count: slide_count(input.deck),
        diagnostics,
    })
}

/// Number of slides: top-level slides plus their vertical children.
fn slide_count(deck: &Deck) -> usize {
    deck.slides
        .iter()
        .map(|slide| 1 + slide.children.len())
        .sum()
}

/// The `reveal_init()` script for `input` (whose config is the effective one).
fn init_script(input: &RenderInput<'_>) -> String {
    let config = input.config;
    let features = &input.deck.features;
    let plugins: Vec<PluginInfo> = reveal::plugins(config, features);
    let katex = reveal::math_enabled(config, features).then_some(&config.math.katex_options);
    reveal::reveal_init_script(&config.reveal_options_json(), &plugins, katex)
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
// Helpers
// ---------------------------------------------------------------------------------------

/// State shared by the helpers of one render (see the module docs).
struct HelperState {
    theme: Theme,
    mode: OutputMode,
    /// What the helpers registered (never the deck's assets).
    registry: AssetRegistry,
    /// Site-mode files emitted so far (the emitters' dedupe state).
    files: Vec<EmittedFile>,
    /// `remote-import` warnings.
    diagnostics: Diagnostics,
}

/// Per-render values the helpers and filters need besides the shared state.
struct Extras {
    theme_css: Arc<str>,
    reveal_init: Arc<str>,
    live_reload: bool,
    build_id: u64,
    markdown: Arc<FilterDeck>,
}

/// What the `markdown` filter needs to know about the deck.
struct FilterDeck {
    /// `config.markdown.smart_punctuation`.
    smart_punctuation: bool,
    /// The deck's directory: relative image paths in the filter's input resolve here.
    base_dir: PathBuf,
    /// The deck's source file: the path of the filter's diagnostics.
    source: PathBuf,
}

impl HelperState {
    /// Runs `f` with an emitter over the helpers' registry that continues this render's
    /// file list.
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

    /// The compiled theme stylesheet, resolved for this mode (standalone: `data:` URIs;
    /// site: a file in `assets/`, so bare file names; serve: `/__presenter/asset/` URLs).
    fn theme_css(&mut self) -> Result<String> {
        let mut remote = Vec::new();
        let targets = match &self.mode {
            OutputMode::Serve => Targets::ServeUrls,
            _ => Targets::Placeholders(&mut self.registry),
        };
        let css = styles::compile(&self.theme, targets, &mut remote)?;
        for (key, url) in remote {
            self.warn_remote_import(&format!("theme stylesheet entry '{key}'"), &url);
        }
        // Serve: no placeholder is left, except `asset:<id>` text the theme wrote itself,
        // which is an unknown id here exactly as in the other modes.
        self.with_emitter(|e| e.resolve_css_in_assets_dir(&css))
    }

    /// A `remote-import` warning in standalone mode (see the module docs).
    fn warn_remote_import(&mut self, stylesheet: &str, url: &str) {
        if self.mode != OutputMode::Standalone {
            return;
        }
        self.diagnostics.push(Diagnostic::warning(
            "remote-import",
            format!(
                "{stylesheet} imports {url}: the standalone file is not self-contained, it \
                 loads that stylesheet from the network when opened (offline the browser \
                 skips it, e.g. falling back to other fonts)"
            ),
        ));
    }

    /// The normalized helper path `path` names if a file is there, else the "unknown path"
    /// error naming `helper`, `path` and the reason.
    fn lookup(&self, helper: &str, path: &str) -> Result<String> {
        match normalize_helper_path(path) {
            Some(key) if helper_path_exists(&self.theme, &key) => Ok(key),
            _ => Err(self.unknown(helper, path)),
        }
    }

    fn unknown(&self, helper: &str, path: &str) -> Error {
        Error::Other(format!(
            "{helper}(\"{path}\"): unknown path ({})",
            unknown_reason(&self.theme, path)
        ))
    }

    /// The bytes and MIME type at `key` (found by [`HelperState::lookup`]).
    fn read(&self, helper: &str, path: &str, key: &str) -> Result<(Vec<u8>, String)> {
        resolve_helper_path(&self.theme, key)?.ok_or_else(|| self.unknown(helper, path))
    }

    /// The text at `key` (UTF-8, a leading byte order mark removed).
    fn text(&self, helper: &str, path: &str, key: &str) -> Result<String> {
        let (bytes, _) = self.read(helper, path, key)?;
        styles::decode_text(bytes)
            .map_err(|e| Error::Other(format!("{helper}(\"{path}\"): not valid UTF-8: {e}")))
    }

    fn style(&mut self, path: &str) -> Result<String> {
        let key = self.lookup("style", path)?;
        let rules = UrlRules::for_style(&key);
        if self.mode == OutputMode::Serve {
            if !rules.is_lenient() {
                // The browser loads the url()s; check them as the other modes do.
                let css = self.text("style", path, &key)?;
                styles::rewrite_urls(&self.theme, &key, &css, rules, Targets::ServeUrls)?;
            }
            return Ok(format!(
                r#"<link rel="stylesheet" href="{}">"#,
                serve_url(&key)
            ));
        }
        let css = self.text("style", path, &key)?;
        for url in styles::remote_imports(&css) {
            self.warn_remote_import(&format!("style(\"{path}\")"), &url);
        }
        let targets = Targets::Placeholders(&mut self.registry);
        let css = styles::rewrite_urls(&self.theme, &key, &css, rules, targets)?;
        if matches!(self.mode, OutputMode::Site { .. }) {
            let css = self.with_emitter(|e| e.resolve_css_in_assets_dir(&css))?;
            let url = self.emit_bytes(css.into_bytes(), "text/css", &key)?;
            return Ok(format!(r#"<link rel="stylesheet" href="{}">"#, attr(&url)));
        }
        let css = self.with_emitter(|e| e.resolve_css(&css))?;
        Ok(format!("<style>\n{}</style>", raw_text(&css, "style")))
    }

    fn script(&mut self, path: &str) -> Result<String> {
        let key = self.lookup("script", path)?;
        match &self.mode {
            OutputMode::Serve => Ok(format!(r#"<script src="{}"></script>"#, serve_url(&key))),
            OutputMode::Site { .. } => {
                let (bytes, mime) = self.read("script", path, &key)?;
                let url = self.emit_bytes(bytes, &mime, &key)?;
                Ok(format!(r#"<script src="{}"></script>"#, attr(&url)))
            }
            OutputMode::Standalone => Ok(inline_script(&self.text("script", path, &key)?)),
        }
    }

    fn asset(&mut self, path: &str) -> Result<String> {
        let key = self.lookup("asset", path)?;
        match &self.mode {
            OutputMode::Serve => {
                versioned_serve_url(&self.theme, &key)?.ok_or_else(|| self.unknown("asset", path))
            }
            OutputMode::Site { .. } => {
                let (bytes, mime) = self.read("asset", path, &key)?;
                let url = self.emit_bytes(bytes, &mime, &key)?;
                Ok(attr(&url).into_owned())
            }
            OutputMode::Standalone => {
                let (bytes, mime) = self.read("asset", path, &key)?;
                Ok(data_uri(&mime, &bytes))
            }
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
    let Extras {
        theme_css,
        reveal_init,
        live_reload,
        build_id,
        markdown,
    } = extras;
    let st = Arc::clone(state);
    env.add_function("style", move |path: String| call(&st, |s| s.style(&path)));
    let st = Arc::clone(state);
    env.add_function("script", move |path: String| call(&st, |s| s.script(&path)));
    let st = Arc::clone(state);
    env.add_function("asset", move |path: String| call(&st, |s| s.asset(&path)));
    let st = Arc::clone(state);
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
    let st = Arc::clone(state);
    env.add_filter("markdown", move |text: Value| {
        markdown_filter(&st, text, &markdown)
    });
    // §4 lists `json` among the built-in filters; minijinja names its JSON filter `tojson`.
    env.add_filter("json", minijinja::filters::tojson);
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

/// A `<script>` element that runs `js` inline (see the module docs).
fn inline_script(js: &str) -> String {
    let text = raw_text(js, "script");
    if leaves_script_open(&text) {
        let uri = data_uri("text/javascript;charset=utf-8", js.as_bytes());
        return format!(r#"<script src="{uri}"></script>"#);
    }
    format!("<script>\n{text}</script>")
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

/// True when the content `text` of a `<script>` element (passed through [`raw_text`], so
/// without `</script`) ends with the HTML tokenizer in the "script data double escaped"
/// state, where the element's own `</script>` does not close it (HTML Living Standard
/// §13.2.5.4 and §13.2.5.20–31): `<!--` enters the escaped state, which `-->` leaves (the
/// dashes of `<!--` count, so `<!-->` leaves at once); in it, `<script` followed by
/// whitespace, `/`, `>` (or the end, as the element's `</script>` follows a line break)
/// enters the double escaped state, which only `-->` leaves. May err on the side of `true`.
fn leaves_script_open(text: &str) -> bool {
    enum State {
        Data,
        Escaped,
        DoubleEscaped,
    }
    // ASCII lowercasing keeps byte offsets.
    let lower = text.to_ascii_lowercase();
    let mut state = State::Data;
    let mut at = 0;
    loop {
        let rest = &lower[at..];
        match state {
            State::Data => match rest.find("<!--") {
                Some(i) => (state, at) = (State::Escaped, at + i + 2),
                None => return false,
            },
            State::Escaped => match (rest.find("-->"), script_start(rest)) {
                (Some(exit), Some(enter)) if exit < enter => {
                    (state, at) = (State::Data, at + exit + 3);
                }
                (_, Some(enter)) => (state, at) = (State::DoubleEscaped, at + enter + 7),
                (Some(exit), None) => (state, at) = (State::Data, at + exit + 3),
                (None, None) => return false,
            },
            State::DoubleEscaped => match rest.find("-->") {
                Some(exit) => (state, at) = (State::Data, at + exit + 3),
                None => return true,
            },
        }
    }
}

/// Byte offset of the first `<script` in the lowercase `text` that is followed by
/// whitespace, `/`, `>` or the end of `text`.
fn script_start(text: &str) -> Option<usize> {
    text.match_indices("<script").map(|(i, _)| i).find(|&i| {
        matches!(
            text.as_bytes().get(i + "<script".len()),
            None | Some(b'\t' | b'\n' | b'\x0c' | b'\r' | b' ' | b'/' | b'>')
        )
    })
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

/// `markdown`: renders a string with the Markdown frontend (see the module docs);
/// none/undefined → empty. Local images are registered with the helpers' files and their
/// placeholders resolved at once; warnings go to the helpers' diagnostics (the deck's
/// path, no span, no duplicates).
fn markdown_filter(
    state: &Mutex<HelperState>,
    text: Value,
    deck: &FilterDeck,
) -> std::result::Result<Value, minijinja::Error> {
    if text.is_undefined() || text.is_none() {
        return Ok(Value::from_safe_string(String::new()));
    }
    let opts = comrak_options(&MarkdownConfig {
        smart_punctuation: deck.smart_punctuation,
        sourcepos: false,
    });
    let mut guard = state
        .lock()
        .map_err(|_| helper_error(poisoned().to_string()))?;
    let mut diagnostics = Diagnostics::new();
    let mut ctx = RenderCtx {
        assets: &mut guard.registry,
        base_dir: &deck.base_dir,
        mode: RenderMode::Notebook,
        diagnostics: &mut diagnostics,
        missing_asset_is_error: false,
    };
    let rendered = render_fragment(&text_of(&text), &opts, &mut ctx)
        .map_err(|err| helper_error(format!("markdown filter: {err}")))?;
    let html = if guard.mode == OutputMode::Serve {
        // The dev server serves only the deck's own assets by id.
        Emitter::new(&guard.registry, &OutputMode::Standalone).resolve_html(&rendered.html)
    } else {
        guard.with_emitter(|emitter| emitter.resolve_html(&rendered.html))
    }
    .map_err(|err| helper_error(format!("markdown filter: {err}")))?;
    for mut diagnostic in diagnostics {
        diagnostic.span = None;
        diagnostic.path = Some(deck.source.clone());
        diagnostic.message = format!("markdown filter: {}", diagnostic.message);
        if !guard.diagnostics.iter().any(|seen| *seen == diagnostic) {
            guard.diagnostics.push(diagnostic);
        }
    }
    Ok(Value::from_safe_string(unwrap_paragraph(&html).to_string()))
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
    fn scripts_that_would_stay_open_are_detected() {
        let open = [
            "window.x = '<!-- <script>';",
            "var s = '<!--<SCRIPT/';",
            "'<!-- <script>' + 1",
            "<!-- <script\n",
            "<!--<script>",
            "<!-- <script> --> <!-- <script>",
            "<!-- --> <!-- x <script\t-",
        ];
        for text in open {
            assert!(leaves_script_open(&raw_text(text, "script")), "{text}");
        }
        let closed = [
            "",
            "let a = 1 < 2;",
            "a <!-- b",
            "'<script>'",
            "<!-- <script> -->",
            "<!-- --> <script>",
            "<!-- <scripts> <scriptx",
            // Only whitespace, `/` and `>` end the tag name the tokenizer compares.
            "'<!-- <script' + 1",
            "<!--><script>",
            "<!---><script>",
            "<!-- <script> --->",
            // `-- >` does not leave the escaped state; `-->` leaves the double escaped one.
            "<!-- -- > <script>--> a",
        ];
        for text in closed {
            assert!(!leaves_script_open(&raw_text(text, "script")), "{text}");
        }
    }

    #[test]
    fn open_scripts_are_inlined_as_data_uris() {
        let js = "window.x = '<!-- <script>';";
        let tag = inline_script(js);
        let uri = data_uri("text/javascript;charset=utf-8", js.as_bytes());
        assert_eq!(tag, format!(r#"<script src="{uri}"></script>"#));
        assert!(uri.starts_with("data:text/javascript;charset=utf-8;base64,"));
        assert_eq!(inline_script("a()"), "<script>\na()\n</script>");
    }

    #[test]
    fn vendored_scripts_are_inlined_as_text() {
        for path in [
            "vendor/reveal/dist/reveal.js",
            "vendor/reveal/dist/plugin/highlight.js",
            "vendor/reveal/dist/plugin/notes.js",
            "vendor/reveal/dist/plugin/zoom.js",
            "vendor/reveal/dist/plugin/search.js",
            "vendor/katex/katex.min.js",
            "presenter/math.js",
        ] {
            let js = crate::vendor::get_str(path).unwrap();
            assert!(!leaves_script_open(&raw_text(&js, "script")), "{path}");
        }
    }

    fn helper_state(mode: OutputMode) -> Mutex<HelperState> {
        Mutex::new(HelperState {
            theme: crate::theme::resolve("default", &[]).expect("built-in default theme"),
            mode,
            registry: AssetRegistry::new(),
            files: Vec::new(),
            diagnostics: Diagnostics::new(),
        })
    }

    /// A deck in `tests/fixtures/md` (where `images/ok.png` exists).
    fn filter_deck(smart_punctuation: bool) -> FilterDeck {
        let base_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/md");
        FilterDeck {
            smart_punctuation,
            source: base_dir.join("deck.md"),
            base_dir,
        }
    }

    #[test]
    fn markdown_filter_unwraps_single_paragraphs() {
        let state = helper_state(OutputMode::Standalone);
        let filter = |text: Value, smart: bool| {
            let out = markdown_filter(&state, text, &filter_deck(smart)).unwrap();
            assert!(out.is_safe());
            out.to_string()
        };
        let render = |s: &str| filter(Value::from(s), false);
        assert_eq!(render("*Fast* tools"), "<em>Fast</em> tools");
        assert_eq!(render("a\n\nb"), "<p>a</p>\n<p>b</p>\n");
        assert_eq!(render("$x$"), "<span data-math-style=\"inline\">x</span>");
        assert_eq!(render("<b>raw</b>"), "<b>raw</b>");
        assert_eq!(render("\"a\" -- b"), "&quot;a&quot; -- b");
        assert_eq!(
            render("---\ntitle: x\n---\n"),
            "<hr />\n<h2>title: x</h2>\n"
        );
        assert_eq!(filter(Value::UNDEFINED, false), "");
        assert_eq!(filter(Value::from("\"a\" -- b"), true), "“a” – b");
        assert!(state.lock().unwrap().diagnostics.is_empty());
    }

    #[test]
    fn markdown_filter_resolves_images_for_the_output_mode() {
        let deck = filter_deck(false);
        let render = |mode: OutputMode, text: &str| {
            let state = helper_state(mode);
            let html = markdown_filter(&state, Value::from(text), &deck)
                .unwrap()
                .to_string();
            let state = state.into_inner().unwrap();
            (html, state.files, state.diagnostics)
        };
        let image = "![logo](images/ok.png)";
        let (html, files, _) = render(OutputMode::Standalone, image);
        assert!(
            html.starts_with(r#"<img src="data:image/png;base64,"#),
            "{html}"
        );
        assert!(files.is_empty());
        let (html, _, _) = render(OutputMode::Serve, image);
        assert!(
            html.starts_with(r#"<img src="data:image/png;base64,"#),
            "{html}"
        );
        let site = OutputMode::Site {
            assets_dir: "out/assets".into(),
            url_prefix: "assets/".into(),
        };
        let (html, files, _) = render(site, image);
        assert!(html.starts_with(r#"<img src="assets/ok-"#), "{html}");
        assert_eq!(files.len(), 1);
        assert!(files[0].path.starts_with("out/assets"));

        let (html, _, diagnostics) = render(OutputMode::Standalone, "![x](images/missing.png)");
        assert_eq!(html, r#"<img src="images/missing.png" alt="x" />"#);
        let diagnostic = diagnostics.iter().next().expect("a missing-asset warning");
        assert_eq!(diagnostic.code, "missing-asset");
        assert!(diagnostic.span.is_none());
        assert_eq!(diagnostic.path.as_deref(), Some(deck.source.as_path()));
        assert!(
            diagnostic.message.starts_with("markdown filter: "),
            "{}",
            diagnostic.message
        );

        // The same text twice (e.g. a subtitle used in two places): one warning.
        let state = helper_state(OutputMode::Standalone);
        for _ in 0..2 {
            markdown_filter(&state, Value::from("![x](images/missing.png)"), &deck).unwrap();
        }
        markdown_filter(&state, Value::from("![y](images/gone.png)"), &deck).unwrap();
        assert_eq!(state.into_inner().unwrap().diagnostics.len(), 2);
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
