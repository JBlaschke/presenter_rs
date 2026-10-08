//! Theme stylesheet sources (`docs/spec/theme-format.md` §5, T42): `.css` entries pass
//! through, `.scss`/`.sass` entries are compiled with [`grass`], and local `@import`s of
//! the result are inlined.
//!
//! **The Sass file system ([`VirtualFs`]).** Sass sees a virtual tree named by helper
//! paths (`docs/spec/theme-format.md` §4), never the real file system:
//! - `@<theme>/<path>`: `<path>` relative to the root directory of that theme of the chain
//!   (disk or embedded, read through [`super::ThemeDir::read`]);
//! - `vendor/<path>`: the embedded vendor tree ([`crate::vendor`]);
//! - `reveal/<path>`: an alias of `vendor/reveal/css/<path>`, reveal.js's Sass sources, so
//!   `@use "reveal/theme/template/settings" with (…)` loads
//!   `vendor/reveal/css/theme/template/settings.scss`.
//!
//! An entry is compiled as the file at its helper path `@<theme>/<rel_path>`. Sass resolves
//! a `@use`/`@forward`/`@import` URL as usual: next to the file that loads it, then in the
//! load paths, which are the chain's `styles/` directories (`@<theme>/styles`, child
//! first) and last the root of the tree (for `reveal/…`, `vendor/…` and `@<theme>/…`
//! URLs). grass does the partial (`_name.scss`), extension (`.scss`, `.sass`, `.css`) and
//! `index` probing like dart-sass. Files are identified by their canonical helper path
//! (`reveal/x` becomes `vendor/reveal/css/x`), so a module is loaded once however it is
//! named, and errors name files by helper path.
//!
//! **Local `@import`s ([`inline_imports`]).** Sass passes CSS imports (`@import url(…)`,
//! `@import "x.css"`) through verbatim, and a standalone document cannot load a relative
//! stylesheet, so after compiling, every `@import` of a relative URL is replaced by the
//! imported file (compiled the same way, its own imports inlined first; at most
//! [`MAX_IMPORT_DEPTH`] levels, no cycles). An import with a media query list becomes an
//! `@media` block. The target is looked up next to the importing stylesheet, then (for
//! stylesheets compiled from reveal's theme sources, whose output keeps reveal's
//! `@import url('./fonts/…')` but not its location) via [`reveal_fallback`]. Absolute
//! (`https://…`, `//…`, `/…`) and `data:` imports stay as written.
//!
//! The caller rewrites each imported file's relative `url()`s against that file's own
//! helper path before it is spliced in (the `finish` callback of [`inline_imports`]), so
//! `url('./source-sans-pro-regular.woff')` in reveal's font CSS finds the font next to that
//! CSS file.
//!
//! **Logging.** grass's `@debug` and `@warn` output goes to `tracing` (debug and warn);
//! each compilation logs its duration at debug level.

use std::cell::OnceCell;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Instant;

use regex::{Captures, Regex};

use super::{Theme, ThemeDir, ThemeFile, ThemeSource, exact_names, normalize_rel};
use crate::error::{Error, Result};

/// Where reveal.js's Sass sources live in the embedded tree (`reveal/<path>` names
/// `vendor/reveal/css/<path>`).
pub const REVEAL_CSS_DIR: &str = "vendor/reveal/css/";

/// How deeply local `@import`s may nest below a stylesheet ([`inline_imports`]).
pub const MAX_IMPORT_DEPTH: usize = 5;

/// What kind of stylesheet a file is, by its extension (any case).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// `.css`: used as written.
    Css,
    /// `.scss` or `.sass`: compiled with grass.
    Sass,
}

impl SourceKind {
    /// The kind of the file at `path`, or `None` for another extension.
    pub fn of(path: &str) -> Option<SourceKind> {
        let (_, ext) = path.rsplit_once('.')?;
        if ext.eq_ignore_ascii_case("css") {
            Some(SourceKind::Css)
        } else if ext.eq_ignore_ascii_case("scss") || ext.eq_ignore_ascii_case("sass") {
            Some(SourceKind::Sass)
        } else {
            None
        }
    }
}

/// The helper path of a theme stylesheet entry: `@<theme>/<rel_path>`, normalized.
pub fn entry_key(file: &ThemeFile) -> String {
    let written = format!("@{}/{}", file.theme, file.rel_path);
    normalize_rel(&written)
        .filter(|key| !key.is_empty())
        .unwrap_or(written)
}

/// Compiles one `[styles] entries` file of the chain: `.css` as written (UTF-8, one leading
/// byte order mark removed), `.scss`/`.sass` with grass over a [`VirtualFs`] of `theme`
/// (the entry is the file at [`entry_key`]). Local `@import`s are **not** inlined here (see
/// [`inline_imports`]).
///
/// Errors: [`Error::Style`] for another extension, a file that is not UTF-8 (`file` = the
/// entry's helper path) or a Sass error (`file` = the helper path of the file the error is
/// in, the message starts with `line L, column C:`, names the entry when the error is in
/// another file, and quotes the source line with the span underlined).
pub fn compile_entry(file: &ThemeFile, theme: &Theme) -> Result<String> {
    let key = entry_key(file);
    match SourceKind::of(&file.rel_path) {
        Some(SourceKind::Css) => decode(&key, file.bytes.clone()),
        Some(SourceKind::Sass) => compile_sass_source(theme, &key, &file.bytes),
        None => Err(Error::Style {
            file: key,
            message: "a style entry must be a .css, .scss or .sass file".into(),
        }),
    }
}

/// Compiles the Sass file at `path` of the [`VirtualFs`] of `theme` (a helper path such as
/// `@mytheme/styles/theme.scss`, or `reveal/theme/white.scss`). Errors as in
/// [`compile_entry`]; a path outside the tree or a missing file is an [`Error::Style`].
pub fn compile_scss(theme: &Theme, path: &str) -> Result<String> {
    let fs = VirtualFs::new(theme);
    let key = VirtualFs::key(Path::new(path)).ok_or_else(|| Error::Style {
        file: path.to_string(),
        message: "not a file of the stylesheet tree (@<theme>/…, vendor/… or reveal/…)".into(),
    })?;
    compile_sass(&fs, &key)
}

/// Compiles `source`, the Sass file at the canonical key `key` (an entry or an imported
/// file whose bytes are already read).
fn compile_sass_source(theme: &Theme, key: &str, source: &[u8]) -> Result<String> {
    // grass's own UTF-8 error names neither the file nor the offending byte.
    if let Err(e) = std::str::from_utf8(source) {
        return Err(Error::Style {
            file: key.to_string(),
            message: format!("not valid UTF-8: {e}"),
        });
    }
    compile_sass(&VirtualFs::new(theme).with_file(key, source), key)
}

/// Runs grass on the file `key` of `fs`, logging the time it took.
fn compile_sass(fs: &VirtualFs<'_>, key: &str) -> Result<String> {
    let load_paths: Vec<PathBuf> = fs
        .theme
        .chain
        .iter()
        .map(|dir| PathBuf::from(format!("@{}/styles", dir.name)))
        .chain([PathBuf::new()])
        .collect();
    let options = grass::Options::default()
        .fs(fs)
        .logger(&TracingLogger)
        .load_paths(&load_paths)
        // The result is concatenated into one stylesheet, where a `@charset` would be
        // misplaced; it is UTF-8 in every output mode anyway.
        .allows_charset(false);
    let started = Instant::now();
    let result = grass::from_path(key, &options);
    let elapsed = started.elapsed();
    tracing::debug!(
        stylesheet = key,
        elapsed_ms = format_args!("{:.1}", elapsed.as_secs_f64() * 1000.0),
        "compiled Sass"
    );
    result.map_err(|err| sass_error(key, *err))
}

/// [`Error::Style`] for a grass error while compiling `entry`.
fn sass_error(entry: &str, err: grass::Error) -> Error {
    let display = err.to_string();
    let style = |file: &str, message: String| Error::Style {
        file: file.to_string(),
        message,
    };
    match err.kind() {
        grass::ErrorKind::ParseError { message, loc, .. } => {
            let name = loc.file.name();
            let file = VirtualFs::key(Path::new(name)).unwrap_or_else(|| name.to_string());
            let (line, column) = (loc.begin.line + 1, loc.begin.column + 1);
            let mut text = format!("line {line}, column {column}: {message}");
            if file != entry {
                let _ = write!(text, " (compiling '{entry}')");
            }
            // A tab is one column (one byte) wide: show it as one space so the marks align.
            let source = loc.file.source_line(loc.begin.line).replace('\t', " ");
            let width = line.to_string().len();
            let marks = match loc.end.line == loc.begin.line {
                true => loc.end.column.saturating_sub(loc.begin.column).max(1),
                false => 1,
            };
            // Writing to a String cannot fail.
            let _ = write!(
                text,
                "\n {line} | {source}\n {:width$} | {}{}",
                "",
                " ".repeat(loc.begin.column),
                "^".repeat(marks)
            );
            style(&file, text)
        }
        grass::ErrorKind::IoError(err) => style(entry, format!("cannot read: {err}")),
        grass::ErrorKind::FromUtf8Error(_) => style(
            entry,
            "a stylesheet it loads (@use, @forward or @import) is not valid UTF-8".into(),
        ),
        _ => style(entry, display.trim_end().to_string()),
    }
}

/// `bytes` as UTF-8 text without one leading byte order mark; [`Error::Style`] naming `key`
/// otherwise.
fn decode(key: &str, bytes: Vec<u8>) -> Result<String> {
    let mut text = String::from_utf8(bytes).map_err(|e| Error::Style {
        file: key.to_string(),
        message: format!("not valid UTF-8: {e}"),
    })?;
    if text.starts_with('\u{feff}') {
        text.drain(..'\u{feff}'.len_utf8());
    }
    Ok(text)
}

// ---------------------------------------------------------------------------------------
// The virtual file system
// ---------------------------------------------------------------------------------------

/// The file tree Sass sees while compiling a theme stylesheet (see the module docs):
/// `@<theme>/…` (the chain's theme directories), `vendor/…` (the embedded vendor tree) and
/// `reveal/…` (= `vendor/reveal/css/…`). Paths are `/`- or `\`-separated helper paths,
/// relative (a leading `/` is ignored) and normalized lexically; anything else, including
/// every absolute path of the real file system, does not exist.
#[derive(Debug)]
pub struct VirtualFs<'t> {
    theme: &'t Theme,
    /// A file whose contents are given instead of read (the entry being compiled).
    file: Option<(&'t str, &'t [u8])>,
    /// Every embedded file, listed on first use: grass probes a dozen names per import, and
    /// in debug builds each [`crate::vendor::get`] lists the asset directory on disk.
    embedded: OnceCell<BTreeSet<String>>,
}

/// Where a key of the [`VirtualFs`] lives.
enum Location<'k, 't> {
    /// `rel` in the directory of a theme of the chain.
    Theme(&'t ThemeDir, &'k str),
    /// The embedded file at this key.
    Embedded(&'k str),
}

impl<'t> VirtualFs<'t> {
    /// The tree of `theme`'s chain.
    pub fn new(theme: &'t Theme) -> Self {
        VirtualFs {
            theme,
            file: None,
            embedded: OnceCell::new(),
        }
    }

    /// The same tree, with `bytes` as the contents of the file at helper path `key`
    /// (normalized as in [`VirtualFs::key`]).
    pub fn with_file(mut self, key: &'t str, bytes: &'t [u8]) -> Self {
        self.file = Some((key, bytes));
        self
    }

    /// The canonical helper path `path` names in the tree: normalized lexically, with
    /// `reveal/<rest>` written `vendor/reveal/css/<rest>`. `None` if it is not below
    /// `@<name>/`, `vendor/` or `reveal/` (it may still not exist).
    pub fn key(path: &Path) -> Option<String> {
        let key = normalize_rel(&path.to_string_lossy())?;
        let (first, rest) = key.split_once('/')?;
        match first {
            "reveal" => Some(format!("{REVEAL_CSS_DIR}{rest}")),
            "vendor" => Some(key),
            name if name.len() > 1 && name.starts_with('@') => Some(key),
            _ => None,
        }
    }

    fn locate<'k>(&self, key: &'k str) -> Option<Location<'k, 't>> {
        let (first, rest) = key.split_once('/')?;
        match first.strip_prefix('@') {
            Some(name) => Some(Location::Theme(self.theme.dir(name)?, rest)),
            None => Some(Location::Embedded(key)),
        }
    }

    /// True if the embedded tree has the file `path` (exact, like [`crate::vendor::get`]).
    fn embedded_file(&self, path: &str) -> bool {
        self.embedded_list().contains(path)
    }

    /// True if the embedded tree has a file below the directory `path`.
    fn embedded_dir(&self, path: &str) -> bool {
        let prefix = format!("{path}/");
        let list = self.embedded_list();
        list.range(prefix.clone()..)
            .next()
            .is_some_and(|file| file.starts_with(&prefix))
    }

    fn embedded_list(&self) -> &BTreeSet<String> {
        self.embedded
            .get_or_init(|| crate::vendor::list("").into_iter().collect())
    }

    /// The given contents, if `key` is the file given to [`VirtualFs::with_file`].
    fn given(&self, key: &str) -> Option<&'t [u8]> {
        let (file, bytes) = self.file?;
        (Self::key(Path::new(file)).as_deref() == Some(key)).then_some(bytes)
    }

    /// The contents of the file at the canonical key `key` ([`VirtualFs::key`]), `Ok(None)`
    /// if there is none. Fails only when an existing theme file cannot be read.
    pub fn read_key(&self, key: &str) -> Result<Option<Vec<u8>>> {
        if let Some(bytes) = self.given(key) {
            return Ok(Some(bytes.to_vec()));
        }
        Ok(match self.locate(key) {
            Some(Location::Theme(dir, rel)) => dir.read(rel)?,
            Some(Location::Embedded(key)) => crate::vendor::get(key).map(|data| data.into_owned()),
            None => None,
        })
    }
}

impl grass::Fs for VirtualFs<'_> {
    fn is_dir(&self, path: &Path) -> bool {
        let Some(key) = Self::key(path) else {
            return false;
        };
        match self.locate(&key) {
            Some(Location::Theme(dir, rel)) => match &dir.source {
                ThemeSource::Disk { dir: root } => {
                    root.join(rel).is_dir() && exact_names(root, rel)
                }
                ThemeSource::Embedded { prefix } => self.embedded_dir(&format!("{prefix}{rel}")),
            },
            Some(Location::Embedded(key)) => self.embedded_dir(key),
            None => false,
        }
    }

    fn is_file(&self, path: &Path) -> bool {
        let Some(key) = Self::key(path) else {
            return false;
        };
        if self.given(&key).is_some() {
            return true;
        }
        match self.locate(&key) {
            Some(Location::Theme(dir, rel)) => match &dir.source {
                ThemeSource::Disk { .. } => dir.exists(rel),
                ThemeSource::Embedded { prefix } => self.embedded_file(&format!("{prefix}{rel}")),
            },
            Some(Location::Embedded(key)) => self.embedded_file(key),
            None => false,
        }
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let bytes = match Self::key(path) {
            Some(key) => self.read_key(&key).map_err(io::Error::other)?,
            None => None,
        };
        bytes.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} is not in the stylesheet tree", path.display()),
            )
        })
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        Ok(Self::key(path).map_or_else(|| path.to_path_buf(), PathBuf::from))
    }
}

/// Sends grass's `@debug` and `@warn` output to `tracing`.
#[derive(Debug)]
struct TracingLogger;

impl grass::Logger for TracingLogger {
    fn debug(&self, location: codemap::SpanLoc, message: &str) {
        tracing::debug!(
            "{}:{}: @debug: {message}",
            location_name(&location),
            location.begin.line + 1
        );
    }

    fn warn(&self, location: codemap::SpanLoc, message: &str) {
        tracing::warn!(
            "{}:{}:{}: Sass warning: {message}",
            location_name(&location),
            location.begin.line + 1,
            location.begin.column + 1
        );
    }
}

/// The helper path of the file of a grass location.
fn location_name(location: &codemap::SpanLoc) -> String {
    let name = location.file.name();
    VirtualFs::key(Path::new(name)).unwrap_or_else(|| name.to_string())
}

// ---------------------------------------------------------------------------------------
// Local @import inlining
// ---------------------------------------------------------------------------------------

/// The helper path a reference `rel` names among reveal.js's theme sources when it is not
/// found where it was written: `reveal/<rest>` → `vendor/reveal/css/<rest>`, and
/// `fonts/<rest>` (reveal's `@import url('./fonts/…')`, relative to `css/theme/`) →
/// `vendor/reveal/css/theme/fonts/<rest>`. `rel` is normalized first; `None` otherwise.
pub fn reveal_fallback(rel: &str) -> Option<String> {
    let rel = normalize_rel(rel)?;
    if let Some(rest) = rel.strip_prefix("reveal/") {
        Some(format!("{REVEAL_CSS_DIR}{rest}"))
    } else if rel.starts_with("fonts/") {
        Some(format!("{REVEAL_CSS_DIR}theme/{rel}"))
    } else {
        None
    }
}

/// Replaces every `@import` of a relative URL in `css`, the stylesheet at helper path
/// `key`, by the imported stylesheet (see the module docs): the target is looked up next
/// to `key` (browser semantics), then via [`reveal_fallback`]; it is compiled like an entry
/// (`.css` as written, `.scss`/`.sass` with grass), its own imports are inlined (at most
/// [`MAX_IMPORT_DEPTH`] levels below `key`), then `finish(target key, css)` turns its
/// relative `url()`s into whatever the caller needs, and the result replaces the `@import`
/// (wrapped in `@media <queries> { … }` when the import has a media query list).
/// Comments and string literals are skipped; imports with a scheme (`https:`, `data:`, …),
/// protocol-relative or root-absolute URLs stay as written.
///
/// Errors: [`Error::Style`] naming the importing stylesheet for a target that resolves to
/// nothing, an import cycle, nesting deeper than [`MAX_IMPORT_DEPTH`], or a `layer(…)` or
/// `supports(…)` condition (which cannot be inlined); compile errors of the imported file;
/// whatever `finish` returns.
pub fn inline_imports(
    theme: &Theme,
    key: &str,
    css: &str,
    finish: &mut dyn FnMut(&str, String) -> Result<String>,
) -> Result<String> {
    let mut inliner = Inliner {
        theme,
        finish,
        stack: vec![key.to_string()],
    };
    inliner.inline(key, css)
}

/// The state of one [`inline_imports`] call.
struct Inliner<'a> {
    theme: &'a Theme,
    finish: &'a mut dyn FnMut(&str, String) -> Result<String>,
    /// The stylesheets being inlined, outermost first (the cycle guard).
    stack: Vec<String>,
}

impl Inliner<'_> {
    /// `css` (the stylesheet `key`, the last of the stack) with its local imports inlined.
    fn inline(&mut self, key: &str, css: &str) -> Result<String> {
        let mut out = String::with_capacity(css.len());
        let mut last = 0;
        for caps in CSS_IMPORT.captures_iter(css) {
            let (Some(whole), Some(import)) = (caps.get(0), LocalImport::of(&caps)) else {
                continue;
            };
            out.push_str(&css[last..whole.start()]);
            out.push_str(&self.splice(key, &import)?);
            last = whole.end();
        }
        out.push_str(&css[last..]);
        Ok(out)
    }

    /// The text that replaces `import` in the stylesheet `key`.
    fn splice(&mut self, key: &str, import: &LocalImport<'_>) -> Result<String> {
        let error = |message: String| Error::Style {
            file: key.to_string(),
            message,
        };
        let condition = import.condition.trim();
        let lower = condition.to_ascii_lowercase();
        if lower.starts_with("layer") || lower.starts_with("supports") {
            return Err(error(format!(
                "@import url({}) {condition}: imports with layer() or supports() cannot be inlined",
                import.url
            )));
        }
        let (target, bytes) = self.resolve(key, import.url)?.ok_or_else(|| {
            error(format!(
                "@import url({}) not found next to the stylesheet or among reveal.js's theme sources",
                import.url
            ))
        })?;
        if let Some(at) = self.stack.iter().position(|open| *open == target) {
            let mut cycle: Vec<&str> = self.stack[at..].iter().map(String::as_str).collect();
            cycle.push(&target);
            return Err(error(format!("@import cycle: {}", cycle.join(" → "))));
        }
        if self.stack.len() > MAX_IMPORT_DEPTH {
            return Err(error(format!(
                "@import url({}): imports are nested more than {MAX_IMPORT_DEPTH} levels deep ({})",
                import.url,
                self.stack.join(" → ")
            )));
        }
        let text = self.compile(&target, bytes)?;
        self.stack.push(target.clone());
        let inlined = self.inline(&target, &text);
        self.stack.pop();
        let mut text = (self.finish)(&target, inlined?)?;
        if !text.ends_with('\n') {
            text.push('\n');
        }
        let text = format!("/* inlined {target} */\n{text}");
        Ok(if condition.is_empty() {
            text
        } else {
            format!("@media {condition} {{\n{text}}}\n")
        })
    }

    /// The canonical key and bytes of the import `url` (without its `?query#fragment`) of
    /// the stylesheet `key` in the [`VirtualFs`]: next to it, else [`reveal_fallback`].
    fn resolve(&self, key: &str, url: &str) -> Result<Option<(String, Vec<u8>)>> {
        let url = url.split(['?', '#']).next().unwrap_or(url);
        let fs = VirtualFs::new(self.theme);
        let dir = key.rsplit_once('/').map_or("", |(dir, _)| dir);
        let next_to = VirtualFs::key(Path::new(&format!("{dir}/{url}")));
        for candidate in next_to.into_iter().chain(reveal_fallback(url)) {
            if let Some(bytes) = fs.read_key(&candidate)? {
                return Ok(Some((candidate, bytes)));
            }
        }
        Ok(None)
    }

    /// The CSS of the imported file `key`: `.scss`/`.sass` compiled, anything else as
    /// written (a browser would apply it as CSS whatever its name).
    fn compile(&self, key: &str, bytes: Vec<u8>) -> Result<String> {
        match SourceKind::of(key) {
            Some(SourceKind::Sass) => compile_sass_source(self.theme, key, &bytes),
            _ => decode(key, bytes),
        }
    }
}

/// Splits the `@import` rules off `css` (outside comments and strings): returns them in
/// order, each ending with `;`, and `css` without them. After [`inline_imports`] only
/// imports of absolute URLs are left; a caller that concatenates stylesheets puts them
/// first, because CSS ignores an `@import` that follows other rules.
pub fn take_imports(css: &str) -> (Vec<String>, String) {
    let mut imports = Vec::new();
    let mut rest = String::with_capacity(css.len());
    let mut last = 0;
    for caps in CSS_IMPORT.captures_iter(css) {
        let Some(whole) = caps.get(0) else {
            continue;
        };
        if !whole.as_str().starts_with('@') {
            continue; // a comment or a string
        }
        let rule = whole.as_str().trim_end();
        imports.push(match rule.ends_with(';') {
            true => rule.to_string(),
            false => format!("{rule};"),
        });
        rest.push_str(&css[last..whole.start()]);
        last = whole.end();
    }
    rest.push_str(&css[last..]);
    (imports, rest)
}

/// A local `@import` found by [`CSS_IMPORT`].
struct LocalImport<'t> {
    /// The URL as written (quotes removed).
    url: &'t str,
    /// What follows the URL up to the `;` (media queries, `layer(…)`, `supports(…)`).
    condition: &'t str,
}

impl<'t> LocalImport<'t> {
    /// The import of a [`CSS_IMPORT`] match, if it is one with a relative URL.
    fn of(caps: &Captures<'t>) -> Option<LocalImport<'t>> {
        let url = ["dq", "sq", "uq", "sdq", "ssq"]
            .iter()
            .find_map(|group| caps.name(group))?
            .as_str();
        let trimmed = url.trim();
        let local =
            !trimmed.is_empty() && !trimmed.starts_with(['/', '\\']) && !SCHEME.is_match(trimmed);
        local.then(|| LocalImport {
            url: trimmed,
            condition: caps.name("cond").map_or("", |m| m.as_str()),
        })
    }
}

/// A URL scheme (`https:`, `data:`, …) at the start of a reference.
static SCHEME: LazyLock<Regex> = LazyLock::new(|| {
    // A constant pattern covered by the unit tests: compiling it cannot fail.
    Regex::new(r"^[A-Za-z][A-Za-z0-9+.\-]*:").expect("scheme regex is valid")
});

/// The CSS tokens [`inline_imports`] cares about, tried in this order at each position: a
/// comment (to the end of the text when unterminated) or a string literal, both matched to
/// be skipped, or an `@import` rule up to its `;` (or the end of the text) whose URL is in
/// `url()` (`dq`/`sq` quoted, `uq` unquoted) or a string (`sdq`/`ssq`), followed by an
/// optional condition (`cond`).
static CSS_IMPORT: LazyLock<Regex> = LazyLock::new(|| {
    // A constant pattern covered by the unit tests: compiling it cannot fail.
    Regex::new(concat!(
        r"(?s)/\*.*?(?:\*/|\z)",
        r#"|"(?:[^"\\\n]|\\.)*(?:"|\n|\z)|'(?:[^'\\\n]|\\.)*(?:'|\n|\z)"#,
        r"|(?i:@import)\s*(?:(?i:url)\(\s*",
        r#"(?:"(?P<dq>[^"\\\n]*)"|'(?P<sq>[^'\\\n]*)'|(?P<uq>[^"'()\\\s]*))\s*\)"#,
        r#"|"(?P<sdq>[^"\\\n]*)"|'(?P<ssq>[^'\\\n]*)')"#,
        r"(?P<cond>[^;{}]*)(?:;|\z)",
    ))
    .expect("CSS import regex is valid")
});

#[cfg(test)]
mod tests {
    use super::*;

    fn imports(css: &str) -> Vec<(String, String)> {
        CSS_IMPORT
            .captures_iter(css)
            .filter_map(|caps| {
                let import = LocalImport::of(&caps)?;
                Some((import.url.to_string(), import.condition.trim().to_string()))
            })
            .collect()
    }

    #[test]
    fn local_imports_are_found_in_every_form() {
        let css = r#"@import url("a.css");
@IMPORT url( 'b.css' ) screen and (min-width: 10px);
@import url(./c.css);
@import "d.css" print;
@import 'e.css'
;/* @import url(comment.css); */
a::before { content: "@import url(string.css);" }
@import url(https://fonts.example/x.css);
@import "//cdn.example/y.css";
@import url(/root.css);
@import url(data:text/css,a%7Bcolor:red%7D);
@import url("");
@import url(last.css)"#;
        assert_eq!(
            imports(css),
            [
                ("a.css", ""),
                ("b.css", "screen and (min-width: 10px)"),
                ("./c.css", ""),
                ("d.css", "print"),
                ("e.css", ""),
                ("last.css", ""),
            ]
            .map(|(u, c)| (u.to_string(), c.to_string()))
        );
    }

    #[test]
    fn take_imports_splits_off_every_import_rule() {
        let css = "/* @import url(c.css); */\n@import url(https://a.example/x.css) print;\n\
                   a { content: \"@import 'no';\" }\n@IMPORT \"//b.example/y.css\"";
        let (imports, rest) = take_imports(css);
        assert_eq!(
            imports,
            [
                "@import url(https://a.example/x.css) print;",
                "@IMPORT \"//b.example/y.css\";"
            ]
        );
        assert_eq!(
            rest,
            "/* @import url(c.css); */\n\na { content: \"@import 'no';\" }\n"
        );
    }

    #[test]
    fn reveal_fallback_maps_aliases_and_font_paths() {
        assert_eq!(
            reveal_fallback("reveal/theme/fonts/a/a.css").as_deref(),
            Some("vendor/reveal/css/theme/fonts/a/a.css")
        );
        assert_eq!(
            reveal_fallback("./fonts/source-sans-pro/source-sans-pro.css").as_deref(),
            Some("vendor/reveal/css/theme/fonts/source-sans-pro/source-sans-pro.css")
        );
        assert_eq!(reveal_fallback("x/fonts/a.css"), None);
        assert_eq!(reveal_fallback("../fonts/a.css"), None);
        assert_eq!(reveal_fallback("theme.css"), None);
    }

    #[test]
    fn virtual_keys_stay_inside_the_tree() {
        let key = |p: &str| VirtualFs::key(Path::new(p));
        assert_eq!(
            key("reveal/theme/template/_settings.scss").as_deref(),
            Some("vendor/reveal/css/theme/template/_settings.scss")
        );
        assert_eq!(
            key("@t/styles/./x/../theme.scss").as_deref(),
            Some("@t/styles/theme.scss")
        );
        assert_eq!(
            key("/@t/styles\\a.scss").as_deref(),
            Some("@t/styles/a.scss")
        );
        assert_eq!(
            key("vendor/reveal/css/theme/white.scss").as_deref(),
            Some("vendor/reveal/css/theme/white.scss")
        );
        assert_eq!(key("styles/theme.scss"), None);
        assert_eq!(key("/etc/passwd"), None);
        assert_eq!(key("@/x.scss"), None);
        assert_eq!(key("../@t/x.scss"), None);
        assert_eq!(key("reveal"), None);
    }

    #[test]
    fn source_kinds_follow_the_extension() {
        assert_eq!(SourceKind::of("a/b.CSS"), Some(SourceKind::Css));
        assert_eq!(SourceKind::of("b.scss"), Some(SourceKind::Sass));
        assert_eq!(SourceKind::of("b.Sass"), Some(SourceKind::Sass));
        assert_eq!(SourceKind::of("b.less"), None);
        assert_eq!(SourceKind::of("css"), None);
    }
}
