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
//! `index` probing like dart-sass, except that it looks a URL with an explicit extension
//! (`@use "tokens.scss"`) up next to the loading file only; [`VirtualFs`] adds the load
//! paths for those (see [`VirtualFs`]'s `canonicalize`). Files are
//! identified by their canonical helper path (`reveal/x` becomes `vendor/reveal/css/x`), so
//! a module is loaded once however it is named, and errors name files by helper path.
//!
//! A file Sass loads that cannot be read or is not UTF-8 is an [`Error::Style`] naming it
//! (grass 0.13.4 would panic on it, so [`VirtualFs`] never reports a read failure to grass;
//! as a backstop, a panic inside grass is caught and becomes an [`Error::Style`] as well).
//!
//! **Isolation (live-reload.md §4, audit F02/F06).** grass has no limit on recursion or
//! loop iterations, and a stack overflow cannot be caught: it aborts the process (and with
//! it `presenter serve`). So every Sass compilation runs on a thread of its own with a
//! [`SASS_STACK_SIZE`] stack (deep but finite nesting and recursion compile, whatever the
//! caller's stack), and the caller waits at most [`SASS_TIME_LIMIT`] for it:
//! - a compilation that takes longer (an endless `@while`, a huge `@for`) is an
//!   [`Error::Style`] saying so. grass cannot be stopped, so **its thread keeps running**
//!   (using a CPU core, and memory when the loop emits CSS) until it ends by itself, which
//!   for an endless loop means until the process exits. While it runs, compiling the same
//!   entry of the same theme chain again with the files it read unchanged fails at once
//!   with the same error instead of starting another such thread; any edit compiles anew;
//! - grass offers no recursion limit, so before grass sees an SCSS file (`.scss`, read
//!   through [`VirtualFs`]), [`unconditional_recursion`] looks for a `@mixin` that always
//!   includes itself or a `@function` that always calls itself before any `@return`
//!   (directly or through others of the same file), which would recurse until even that
//!   stack overflows. Such a file is an [`Error::Style`] pointing at the call. Recursion
//!   that only a condition guards is left to grass: one whose condition never ends it, or
//!   one through several files or the indented `.sass` syntax, still overflows the stack.
//!
//! **Local `@import`s ([`inline_imports`]).** Sass passes CSS imports (`@import url(…)`,
//! `@import "x.css"`) through verbatim, and a standalone document cannot load a relative
//! stylesheet, so after compiling, every top-level `@import` of a relative URL is replaced
//! by the imported file (compiled the same way, its own imports inlined first; at most
//! [`MAX_IMPORT_DEPTH`] levels, no cycles). An import with a media query list becomes an
//! `@media` block. The target is looked up next to the importing stylesheet (for Sass, the
//! file compiled: Sass does not say which partial a CSS import came from), then (for
//! stylesheets compiled from reveal's theme sources, whose output keeps reveal's
//! `@import url('./fonts/…')` but not its location) via [`reveal_fallback`]. The other
//! top-level imports (`https://…`, `//…`, `/…`, `data:`) are taken out, with the media
//! queries of the imports that led to them, for the caller to put first ([`Inlined`]).
//! An `@import` inside a block is invalid CSS that browsers ignore; it is left as written.
//!
//! The caller's `finish` callback turns the relative `url()`s of each stylesheet's own text
//! into whatever it needs, against that stylesheet's helper path, before anything is
//! spliced in, so `url('./source-sans-pro-regular.woff')` in reveal's font CSS finds the
//! font next to that CSS file and no text is processed twice.
//!
//! **Logging.** grass's `@debug` and `@warn` output goes to `tracing` (debug and warn;
//! not to the build's diagnostics); each compilation logs its duration at debug level.

use std::any::Any;
use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use regex::{Captures, Regex};

use super::{Theme, ThemeDir, ThemeFile, ThemeSource, exact_names, normalize_rel};
use crate::error::{Error, Result};

/// Where reveal.js's Sass sources live in the embedded tree (`reveal/<path>` names
/// `vendor/reveal/css/<path>`).
pub const REVEAL_CSS_DIR: &str = "vendor/reveal/css/";

/// How long one Sass compilation may run before it is an [`Error::Style`] (module docs,
/// "Isolation"; live-reload.md §4).
pub const SASS_TIME_LIMIT: Duration = Duration::from_secs(10);

/// The stack size of the thread each Sass compilation runs on (module docs, "Isolation"):
/// 256 MiB, reserved but only used as deep as grass recurses. It holds tens of thousands
/// of nested blocks or `@include`s, where the 2 MiB of a worker thread holds a few hundred.
pub const SASS_STACK_SIZE: usize = 256 << 20;

/// How deeply local `@import`s may nest below a stylesheet ([`inline_imports`]).
pub const MAX_IMPORT_DEPTH: usize = 5;

/// Ends the directory part of the names grass gets from [`VirtualFs`] for the files it
/// loads (`@t/styles\u{1}/theme.scss`), so a path grass builds from one
/// (`<directory>/<URL as written>`) says where the URL starts. [`VirtualFs::key`] drops it.
const DIR_MARK: char = '\u{1}';

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
/// Errors: [`Error::Style`] for another extension, a file that is not UTF-8 or cannot be
/// read (`file` = the helper path of that file: the entry or a file Sass loads) or a Sass
/// error (`file` = the helper path of the file the error is in, the message starts with
/// `line L, column C:` and quotes the source line with the span underlined). A message
/// about another file than the entry ends with `(compiling '<entry>')`.
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
    let key = VirtualFs::key(Path::new(path)).ok_or_else(|| Error::Style {
        file: path.to_string(),
        message: "not a file of the stylesheet tree (@<theme>/…, vendor/… or reveal/…)".into(),
    })?;
    let bytes = VirtualFs::new(theme)
        .read_key(&key)?
        .ok_or_else(|| Error::Style {
            file: key.clone(),
            message: "no such file".into(),
        })?;
    compile_sass_source(theme, &key, &bytes)
}

/// Compiles `source`, the Sass file at the canonical key `key` (an entry or an imported
/// file whose bytes are already read), on a thread of its own within [`SASS_TIME_LIMIT`].
fn compile_sass_source(theme: &Theme, key: &str, source: &[u8]) -> Result<String> {
    compile_sass_within(theme, key, source, SASS_TIME_LIMIT)
}

/// [`compile_sass_source`] with the time limit `limit` (module docs, "Isolation"): grass
/// runs on a new thread with a [`SASS_STACK_SIZE`] stack; when it has not finished after
/// `limit`, the thread is left running and the result is an [`Error::Style`] saying so.
fn compile_sass_within(theme: &Theme, key: &str, source: &[u8], limit: Duration) -> Result<String> {
    // grass's own UTF-8 error names neither the file nor the offending byte.
    if let Err(e) = std::str::from_utf8(source) {
        return Err(Error::Style {
            file: key.to_string(),
            message: format!("not valid UTF-8: {e}"),
        });
    }
    if unchanged_since_timeout(theme, key, source) {
        return Err(timeout_error(key, limit, true));
    }
    let reads = Reads::default();
    let (tx, rx) = mpsc::channel();
    // The caller's subscriber (a test's, or the CLI's global one) gets grass's logs.
    let dispatch = tracing::dispatcher::get_default(tracing::Dispatch::clone);
    let job = (
        theme.clone(),
        key.to_string(),
        source.to_vec(),
        Arc::clone(&reads),
    );
    let spawned = std::thread::Builder::new()
        .name("presenter-sass".into())
        .stack_size(SASS_STACK_SIZE)
        .spawn(move || {
            let (theme, key, source, reads) = job;
            let result = tracing::dispatcher::with_default(&dispatch, || {
                let fs = VirtualFs::new(&theme)
                    .with_file(&key, &source)
                    .recording(reads);
                compile_sass(&fs, &key)
            });
            // The caller may have stopped waiting (timed out): nobody to tell then.
            let _ = tx.send(result);
        });
    let thread = spawned.map_err(|err| Error::Style {
        file: key.to_string(),
        message: format!("cannot start a thread for the Sass compiler: {err}"),
    })?;
    match rx.recv_timeout(limit) {
        Ok(result) => {
            // It has sent its result: the thread is ending, and its stack is freed.
            let _ = thread.join();
            result
        }
        Err(RecvTimeoutError::Timeout) => {
            remember_timeout(theme, key, reads, thread);
            Err(timeout_error(key, limit, false))
        }
        Err(RecvTimeoutError::Disconnected) => Err(Error::Style {
            file: key.to_string(),
            message: "the Sass compiler (grass) stopped unexpectedly".into(),
        }),
    }
}

/// The [`Error::Style`] of a compilation of `key` that did not finish within `limit`
/// (`again`: one with the same inputs as a compilation that timed out and still runs).
fn timeout_error(key: &str, limit: Duration, again: bool) -> Error {
    let when = match again {
        true => "an earlier compilation of the same files did not finish within",
        false => "the Sass compilation did not finish within",
    };
    Error::Style {
        file: key.to_string(),
        message: format!(
            "{when} {} s (an endless @while or @for loop?). It cannot be stopped and runs on \
             in the background (a CPU core, and memory if it emits CSS) until presenter \
             exits: fix the stylesheet, and restart a running `presenter serve` or \
             `build --watch` to free them",
            limit.as_secs_f64()
        ),
    }
}

/// The files a compilation read through its [`VirtualFs`], by canonical key, with a hash
/// of their contents, in the order read.
type Reads = Arc<Mutex<Vec<(String, blake3::Hash)>>>;

/// A compilation that did not finish within its time limit (module docs, "Isolation").
struct TimedOut {
    /// The locations of its theme chain, child first: which theme it compiled for.
    chain: Vec<String>,
    /// Its entry's canonical key.
    key: String,
    /// What it has read so far (it may read more while it runs).
    reads: Reads,
    /// Its thread, still running unless finished.
    thread: JoinHandle<()>,
}

/// The compilations that timed out, for [`unchanged_since_timeout`].
static TIMED_OUT: Mutex<Vec<TimedOut>> = Mutex::new(Vec::new());

/// The locations of `theme`'s chain, child first.
fn chain_locations(theme: &Theme) -> Vec<String> {
    theme.chain.iter().map(ThemeDir::location).collect()
}

/// Records a compilation of `key` for `theme` that did not finish in time.
fn remember_timeout(theme: &Theme, key: &str, reads: Reads, thread: JoinHandle<()>) {
    let mut timed_out = TIMED_OUT.lock().unwrap_or_else(PoisonError::into_inner);
    timed_out.push(TimedOut {
        chain: chain_locations(theme),
        key: key.to_string(),
        reads,
        thread,
    });
}

/// True when a compilation of `key` (whose contents are now `source`) for the chain of
/// `theme` timed out and still runs, and every file it read is unchanged: compiling again
/// would run into the same endless loop. Forgets the compilations that have ended.
fn unchanged_since_timeout(theme: &Theme, key: &str, source: &[u8]) -> bool {
    let mut timed_out = TIMED_OUT.lock().unwrap_or_else(PoisonError::into_inner);
    timed_out.retain(|run| !run.thread.is_finished());
    if timed_out.is_empty() {
        return false;
    }
    let chain = chain_locations(theme);
    let fs = VirtualFs::new(theme);
    let unchanged = |(file, hash): &(String, blake3::Hash)| {
        let now = match file == key {
            true => Some(source.to_vec()),
            false => fs.read_key(file).ok().flatten(),
        };
        now.is_some_and(|bytes| blake3::hash(&bytes) == *hash)
    };
    timed_out.iter().any(|run| {
        let reads = run.reads.lock().unwrap_or_else(PoisonError::into_inner);
        run.key == key && run.chain == chain && !reads.is_empty() && reads.iter().all(unchanged)
    })
}

/// Runs grass on the file `key` of `fs`, logging the time it took.
fn compile_sass(fs: &VirtualFs<'_>, key: &str) -> Result<String> {
    let load_paths: Vec<PathBuf> = fs.load_paths().into_iter().map(PathBuf::from).collect();
    let options = grass::Options::default()
        .fs(fs)
        .logger(&TracingLogger)
        .load_paths(&load_paths)
        // The result is concatenated into one stylesheet, where a `@charset` would be
        // misplaced; it is UTF-8 in every output mode anyway.
        .allows_charset(false);
    let entry = VirtualFs::marked(key);
    let started = Instant::now();
    // A backstop: grass has internal panics (`unreachable!`, `todo!`) that user input may
    // reach; a theme must not take `build` or `serve` down with it.
    let result = catch_unwind(AssertUnwindSafe(|| grass::from_path(&entry, &options)));
    let elapsed = started.elapsed();
    tracing::debug!(
        stylesheet = key,
        elapsed_ms = format_args!("{:.1}", elapsed.as_secs_f64() * 1000.0),
        "compiled Sass"
    );
    // A file that could not be loaded was read as empty: its error comes first.
    if let Some(err) = fs.failure.take() {
        return Err(naming_entry(err, key));
    }
    match result {
        Ok(result) => result.map_err(|err| sass_error(key, *err)),
        Err(payload) => Err(Error::Style {
            file: key.to_string(),
            message: format!(
                "the Sass compiler (grass) failed unexpectedly: {}",
                panic_message(payload.as_ref())
            ),
        }),
    }
}

/// `err` with `(compiling '<entry>')` added when it is an [`Error::Style`] about another
/// file than `entry`.
fn naming_entry(err: Error, entry: &str) -> Error {
    match err {
        Error::Style { file, message } if file != entry => Error::Style {
            message: format!("{message} (compiling '{entry}')"),
            file,
        },
        other => other,
    }
}

/// The text of a panic payload.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "panic".to_string()
    }
}

/// [`Error::Style`] for a grass error while compiling `entry`.
fn sass_error(entry: &str, err: grass::Error) -> Error {
    let display = err.to_string();
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
            Error::Style {
                file,
                message: text.replace(DIR_MARK, ""),
            }
        }
        // [`VirtualFs::read`] never fails, so neither an I/O nor a UTF-8 error is expected.
        _ => Error::Style {
            file: entry.to_string(),
            message: display.trim_end().replace(DIR_MARK, ""),
        },
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
///
/// [`grass::Fs::read`] never fails: grass 0.13.4 panics when reading a file it loads fails
/// or gives invalid UTF-8, so the first such problem is recorded instead (an
/// [`Error::Style`] naming the file), the file reads as empty, and the compilation reports
/// the recorded error. An `.scss` file that recurses unconditionally
/// ([`unconditional_recursion`]) is such a problem too: grass would overflow its stack.
#[derive(Debug)]
pub struct VirtualFs<'t> {
    theme: &'t Theme,
    /// A file whose contents are given instead of read (the entry being compiled).
    file: Option<(&'t str, &'t [u8])>,
    /// Every embedded file, listed on first use: grass probes a dozen names per import, and
    /// in debug builds each [`crate::vendor::get`] lists the asset directory on disk.
    embedded: OnceCell<BTreeSet<String>>,
    /// The first file grass could not read (see the type docs).
    failure: RefCell<Option<Error>>,
    /// Where the files grass reads are recorded, if anywhere ([`compile_sass_within`]).
    reads: Option<Reads>,
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
            failure: RefCell::new(None),
            reads: None,
        }
    }

    /// The same tree, with `bytes` as the contents of the file at helper path `key`
    /// (normalized as in [`VirtualFs::key`]).
    pub fn with_file(mut self, key: &'t str, bytes: &'t [u8]) -> Self {
        self.file = Some((key, bytes));
        self
    }

    /// The same tree, recording each file grass reads in `reads`.
    fn recording(mut self, reads: Reads) -> Self {
        self.reads = Some(reads);
        self
    }

    /// The canonical helper path `path` names in the tree: normalized lexically, with
    /// `reveal/<rest>` written `vendor/reveal/css/<rest>` (and the marker this type adds to
    /// the names it gives grass removed). `None` if it is not below `@<name>/`, `vendor/` or
    /// `reveal/` (it may still not exist).
    pub fn key(path: &Path) -> Option<String> {
        let key = normalize_rel(&path.to_string_lossy().replace(DIR_MARK, ""))?;
        let (first, rest) = key.split_once('/')?;
        match first {
            "reveal" => Some(format!("{REVEAL_CSS_DIR}{rest}")),
            "vendor" => Some(key),
            name if name.len() > 1 && name.starts_with('@') => Some(key),
            _ => None,
        }
    }

    /// The name grass gets for the file at the canonical key `key`: [`DIR_MARK`] after its
    /// directory part.
    fn marked(key: &str) -> String {
        match key.rsplit_once('/') {
            Some((dir, name)) => format!("{dir}{DIR_MARK}/{name}"),
            None => key.to_string(),
        }
    }

    /// Sass's load paths: the chain's `styles/` directories, child first, then the root.
    fn load_paths(&self) -> Vec<String> {
        let chain = self.theme.chain.iter();
        chain
            .map(|dir| format!("@{}/styles", dir.name))
            .chain([String::new()])
            .collect()
    }

    fn locate<'k>(&self, key: &'k str) -> Option<Location<'k, 't>> {
        let (first, rest) = key.split_once('/')?;
        match first.strip_prefix('@') {
            Some(name) => Some(Location::Theme(self.theme.dir(name)?, rest)),
            None => Some(Location::Embedded(key)),
        }
    }

    /// True if the file at the canonical key `key` exists.
    fn has_file(&self, key: &str) -> bool {
        if self.given(key).is_some() {
            return true;
        }
        match self.locate(key) {
            Some(Location::Theme(dir, rel)) => match &dir.source {
                ThemeSource::Disk { .. } => dir.exists(rel),
                ThemeSource::Embedded { prefix } => self.embedded_file(&format!("{prefix}{rel}")),
            },
            Some(Location::Embedded(key)) => self.embedded_file(key),
            None => false,
        }
    }

    /// The canonical key of the file at `path`, if there is one.
    fn existing(&self, path: &str) -> Option<String> {
        Self::key(Path::new(path)).filter(|key| self.has_file(key))
    }

    /// The load-path lookup grass skips for a URL with an explicit extension (grass 0.13.4,
    /// `find_import`: "todo: consider load paths"). For such a URL, grass first probes
    /// `<dir of the loading file>/<stem>..import<ext>` (a `with_extension` slip) and its
    /// partial `_<stem>..import<ext>`, then the URL and its partial next to the loading
    /// file. For one of those first probes, when the URL is not next to the loading file,
    /// this returns the file the load paths give (`<path>/<URL>`, then `<path>/_<name>`,
    /// for each of [`VirtualFs::load_paths`]); grass then takes the probe as found and
    /// [`grass::Fs::canonicalize`] names that file.
    ///
    /// The partial probe of `x.scss` looks like the first probe of `_x.scss`: when
    /// `x.scss` is next to the loading file, a `_x..import<ext>` probe finds nothing, so
    /// grass goes on to that file (`@use "_x.scss"` then misses a `_x.scss` that only the
    /// load paths have; a name nobody writes next to a file of the same name).
    fn extension_fallback(&self, path: &Path) -> Option<String> {
        let raw = path.to_string_lossy();
        let (dir, url) = raw.split_once(DIR_MARK)?;
        let (url_dir, name) = url.rsplit_once(['/', '\\'])?;
        let (stem, ext) = ["scss", "sass", "css"].into_iter().find_map(|ext| {
            let stem = name.strip_suffix(ext)?.strip_suffix("..import")?;
            Some((stem, ext))
        })?;
        let file = |base: &str, stem: &str| self.existing(&format!("{base}{url_dir}/{stem}.{ext}"));
        let partial = format!("_{stem}");
        let next_to = [stem, partial.as_str()]
            .into_iter()
            .chain(stem.strip_prefix('_'))
            .any(|stem| file(dir, stem).is_some());
        if next_to {
            return None; // grass's next probes find it
        }
        self.load_paths()
            .iter()
            .find_map(|base| file(base, stem).or_else(|| file(base, &partial)))
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

    /// The UTF-8 contents of the file at `path`, recorded when [`VirtualFs::recording`];
    /// [`Error::Style`] naming it otherwise, and for an `.scss` file that recurses
    /// unconditionally ([`unconditional_recursion`]).
    fn read_text(&self, path: &Path) -> Result<Vec<u8>> {
        let (key, bytes) = self.read_utf8(path)?;
        if let Some(reads) = &self.reads {
            let mut reads = reads.lock().unwrap_or_else(PoisonError::into_inner);
            reads.push((key.clone(), blake3::hash(&bytes)));
        }
        let scss = key
            .rsplit_once('.')
            .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("scss"));
        // `read_utf8` checked the bytes.
        let text = std::str::from_utf8(&bytes).unwrap_or_default();
        match scss.then(|| unconditional_recursion(text)).flatten() {
            Some(found) => Err(Error::Style {
                file: key,
                message: found.message(text),
            }),
            None => Ok(bytes),
        }
    }

    /// The canonical key and UTF-8 contents of the file at `path`; [`Error::Style`] naming
    /// it otherwise.
    fn read_utf8(&self, path: &Path) -> Result<(String, Vec<u8>)> {
        let Some(key) = Self::key(path) else {
            return Err(Error::Style {
                file: path.to_string_lossy().replace(DIR_MARK, ""),
                message: "not a file of the stylesheet tree".into(),
            });
        };
        let error = |message: String| Error::Style {
            file: key.clone(),
            message,
        };
        let bytes = match self.read_key(&key) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Err(error("no such file".into())),
            Err(Error::Theme { message, .. }) => return Err(error(message)),
            Err(other) => return Err(error(format!("cannot read: {other}"))),
        };
        match std::str::from_utf8(&bytes) {
            Ok(_) => Ok((key, bytes)),
            Err(e) => Err(error(format!("not valid UTF-8: {e}"))),
        }
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
        Self::key(path).is_some_and(|key| self.has_file(&key))
            || self.extension_fallback(path).is_some()
    }

    /// Never fails (see the type docs).
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        Ok(self.read_text(path).unwrap_or_else(|err| {
            let mut failure = self.failure.borrow_mut();
            if failure.is_none() {
                *failure = Some(err);
            }
            Vec::new()
        }))
    }

    /// The canonical key of the file `path` names, with a marker after its directory part
    /// (so a URL that grass joins to it can be told from the directory); for grass's first
    /// probe of a URL with an explicit extension (`@use "tokens.scss"`), the file Sass's
    /// load paths give, which grass looks up next to the loading file only.
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        let key = match Self::key(path) {
            Some(key) if self.has_file(&key) => Some(key),
            key => self.extension_fallback(path).or(key),
        };
        Ok(key.map_or_else(|| path.to_path_buf(), |key| Self::marked(&key).into()))
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
    VirtualFs::key(Path::new(name)).unwrap_or_else(|| name.replace(DIR_MARK, ""))
}

// ---------------------------------------------------------------------------------------
// Unconditional recursion
// ---------------------------------------------------------------------------------------

/// A `@mixin` or `@function` that always recurses ([`unconditional_recursion`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recursion {
    /// `"mixin"` or `"function"`.
    pub kind: &'static str,
    /// The names on the cycle, starting and ending with the one defined first (`[a, a]`
    /// for a mixin that includes itself).
    pub cycle: Vec<String>,
    /// The 1-based line of the `@include` or call in the first definition that starts the
    /// cycle.
    pub line: usize,
}

impl Recursion {
    /// The message of the [`Error::Style`] for this recursion in the file `text`:
    /// `line L: …` and the line itself, numbered (as grass quotes a line).
    pub fn message(&self, text: &str) -> String {
        let (name, calls) = match self.kind {
            "mixin" => ("@mixin", "includes itself"),
            _ => ("@function", "calls itself before any @return"),
        };
        let through = match self.cycle.as_slice() {
            [_, _] => String::new(),
            cycle => format!(" through {}", cycle[1..cycle.len() - 1].join(", ")),
        };
        let mut message = format!(
            "line {}: {name} {} always {calls}{through}, so Sass would recurse until it runs \
             out of stack; make the recursion depend on an @if that ends it",
            self.line, self.cycle[0]
        );
        if let Some(source) = text.lines().nth(self.line - 1) {
            let _ = write!(message, "\n {} | {}", self.line, source.trim_end());
        }
        message
    }
}

/// The first `@mixin` (in source order) that always `@include`s itself, directly or through
/// other mixins of `scss`, else the first such `@function` that always calls itself; `None`
/// when there is neither. A mixin includes another always when the `@include` is not inside
/// a control directive (`@if`, `@else`, `@each`, `@for`, `@while`) or a content block; a
/// function calls another always when the call is outside control directives and the lazy
/// `if()`, and comes before any `@return` (an `@if` that may return makes what follows
/// conditional). Only names without a namespace count (`@include pad`, not
/// `@include m.pad`), compared as Sass does (`-` and `_` are the same). The SCSS syntax is
/// read leniently: comments, strings and unquoted `url()`s are skipped; invalid input finds
/// nothing rather than failing.
pub fn unconditional_recursion(scss: &str) -> Option<Recursion> {
    let statements = parse_statements(scss);
    let mut mixins = Vec::new();
    let mut functions = Vec::new();
    collect_definitions(&statements, &mut mixins, &mut functions);
    first_cycle("mixin", &mixins).or_else(|| first_cycle("function", &functions))
}

/// One SCSS statement: its text up to `;` or `{` (comments removed, strings emptied) and
/// its block, if it has one.
#[derive(Debug, Default)]
struct Statement {
    /// The statement's text, trimmed.
    head: String,
    /// The 1-based line where it starts.
    line: usize,
    /// The statements of its `{ … }` block.
    block: Option<Vec<Statement>>,
}

impl Statement {
    /// The at-rule's name (`include` for `@include pad;`), lowercase; `""` for anything
    /// else.
    fn at_rule(&self) -> String {
        let Some(rest) = self.head.strip_prefix('@') else {
            return String::new();
        };
        rest.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    }

    /// The text after the at-rule's name.
    fn at_rule_rest(&self) -> &str {
        let name = 1 + self.at_rule().len();
        self.head.get(name..).unwrap_or_default().trim_start()
    }
}

/// A mixin or function definition: its name (normalized), and the names it always
/// includes or calls, with the line of each.
type Definition = (String, Vec<(String, usize)>);

/// The control directives: what is inside them may not run.
const CONTROL: [&str; 5] = ["if", "else", "each", "for", "while"];

/// The SCSS statements of `text`, as a tree (see [`Statement`]).
fn parse_statements(text: &str) -> Vec<Statement> {
    let mut stack = vec![Statement::default()];
    let mut head = Statement::default();
    let mut chars = text.chars().peekable();
    let mut line = 1;
    while let Some(c) = chars.next() {
        let comment = c == '/' && matches!(chars.peek(), Some('/' | '*'));
        if !comment
            && !c.is_whitespace()
            && !matches!(c, '{' | ';' | '}')
            && head.head.trim().is_empty()
        {
            head.line = line;
            head.head.clear();
        }
        match c {
            '/' if chars.peek() == Some(&'/') => while chars.next_if(|&c| c != '\n').is_some() {},
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut last = ' ';
                for c in chars.by_ref() {
                    line += usize::from(c == '\n');
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
            }
            '"' | '\'' => {
                line += skip_string(&mut chars, c);
                head.head.push_str("\"\"");
            }
            '(' if head.head.to_ascii_lowercase().ends_with("url") => {
                head.head.push('(');
                line += skip_unquoted_url(&mut chars, &mut head.head);
            }
            '#' if chars.peek() == Some(&'{') => {
                head.head.push('#');
                line += copy_interpolation(&mut chars, &mut head.head);
            }
            '{' | ';' | '}' => {
                let done = std::mem::take(&mut head);
                if c == '{' {
                    stack.push(done);
                    continue;
                }
                if !done.head.trim().is_empty()
                    && let Some(open) = stack.last_mut()
                {
                    open.block.get_or_insert_default().push(trimmed(done));
                }
                if c == '}' && stack.len() > 1 {
                    close_block(&mut stack);
                }
            }
            _ => {
                line += usize::from(c == '\n');
                head.head.push(c);
            }
        }
    }
    while stack.len() > 1 {
        close_block(&mut stack);
    }
    stack.pop().and_then(|top| top.block).unwrap_or_default()
}

/// `statement` with its head trimmed.
fn trimmed(mut statement: Statement) -> Statement {
    statement.head = statement.head.trim().to_string();
    statement
}

/// Ends the innermost open block of `stack` (more than one entry): it becomes a statement
/// of the block around it.
fn close_block(stack: &mut Vec<Statement>) {
    if let Some(mut closed) = stack.pop()
        && let Some(open) = stack.last_mut()
    {
        closed.block.get_or_insert_default();
        open.block.get_or_insert_default().push(trimmed(closed));
    }
}

/// Skips a string literal opened by `quote` (escapes included, ended by the quote or a
/// line break); returns the number of line breaks consumed.
fn skip_string(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, quote: char) -> usize {
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '\n' => return 1,
            c if c == quote => return 0,
            _ => {}
        }
    }
    0
}

/// Copies an unquoted `url(` argument up to its `)` into `head` (a quoted one is left to
/// the caller); returns the number of line breaks consumed.
fn skip_unquoted_url(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    head: &mut String,
) -> usize {
    while chars.next_if(|c| c.is_whitespace() && *c != '\n').is_some() {}
    if matches!(chars.peek(), Some('"' | '\'')) {
        return 0;
    }
    let mut lines = 0;
    for c in chars.by_ref() {
        lines += usize::from(c == '\n');
        head.push(c);
        if c == ')' {
            break;
        }
    }
    lines
}

/// Copies an interpolation `{…}` (after its `#`) with nested braces into `head`; returns
/// the number of line breaks consumed.
fn copy_interpolation(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    head: &mut String,
) -> usize {
    let (mut depth, mut lines) = (0usize, 0);
    for c in chars.by_ref() {
        lines += usize::from(c == '\n');
        head.push(c);
        match c {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
    }
    lines
}

/// A name as Sass compares it: `-` and `_` are the same.
fn sass_name(name: &str) -> String {
    name.replace('_', "-")
}

/// The leading identifier of `text` (letters, digits, `-`, `_`), normalized.
fn leading_name(text: &str) -> Option<String> {
    let end = text
        .find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(text.len());
    (end > 0).then(|| sass_name(&text[..end]))
}

/// Collects the `@mixin` and `@function` definitions of `statements` (at any depth outside
/// definitions) with what each always includes or calls.
fn collect_definitions(
    statements: &[Statement],
    mixins: &mut Vec<Definition>,
    functions: &mut Vec<Definition>,
) {
    for statement in statements {
        let Some(block) = &statement.block else {
            continue;
        };
        let at = statement.at_rule();
        let name = leading_name(statement.at_rule_rest());
        match (at.as_str(), name) {
            ("mixin", Some(name)) => {
                let mut includes = Vec::new();
                mixin_includes(block, &mut includes);
                mixins.push((name, includes));
            }
            ("function", Some(name)) => {
                let mut calls = Vec::new();
                function_calls(block, &mut calls);
                functions.push((name, calls));
            }
            _ => collect_definitions(block, mixins, functions),
        }
    }
}

/// The mixins `block` (of a mixin) always includes, with their lines.
fn mixin_includes(block: &[Statement], includes: &mut Vec<(String, usize)>) {
    for statement in block {
        let at = statement.at_rule();
        if at == "include" {
            let rest = statement.at_rule_rest();
            if let Some(name) = leading_name(rest)
                && !rest[name.len()..].starts_with('.')
            {
                includes.push((name, statement.line));
            }
        } else if !CONTROL.contains(&at.as_str())
            && !matches!(at.as_str(), "mixin" | "function")
            && let Some(inner) = &statement.block
        {
            mixin_includes(inner, includes);
        }
    }
}

/// The functions `block` (of a function) always calls before any `@return`, with their
/// lines.
fn function_calls(block: &[Statement], calls: &mut Vec<(String, usize)>) {
    for statement in block {
        let at = statement.at_rule();
        if CONTROL.contains(&at.as_str()) {
            if statement.block.as_deref().is_some_and(returns) {
                return; // what follows runs only if that branch did not return
            }
            continue;
        }
        if at == "error" {
            return;
        }
        if at.is_empty() || matches!(at.as_str(), "return" | "debug" | "warn") {
            let expression = match at.is_empty() {
                true => statement.head.as_str(),
                false => statement.at_rule_rest(),
            };
            calls.extend(
                called_names(expression)
                    .into_iter()
                    .map(|name| (name, statement.line)),
            );
        }
        if at == "return" {
            return;
        }
    }
}

/// True if `block` contains a `@return` at any depth.
fn returns(block: &[Statement]) -> bool {
    block.iter().any(|statement| {
        statement.at_rule() == "return" || statement.block.as_deref().is_some_and(returns)
    })
}

/// The names of the functions `expression` calls (`name(`, without a namespace, not
/// inside the arguments of the lazy `if()`), normalized.
fn called_names(expression: &str) -> Vec<String> {
    let mut names = Vec::new();
    let bytes = expression.as_bytes();
    let mut at = 0;
    while at < expression.len() {
        let rest = &expression[at..];
        let Some(name) = leading_name(rest) else {
            at += rest.chars().next().map_or(1, char::len_utf8);
            continue;
        };
        let raw_len = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(rest.len());
        let before = expression[..at].chars().next_back();
        let call = bytes.get(at + raw_len) == Some(&b'(')
            && !matches!(before, Some('.' | '$' | '@' | '%' | '-' | '_'))
            && !before.is_some_and(char::is_alphanumeric);
        at += raw_len;
        if call && name == "if" {
            at += closing_paren(&expression[at..]).map_or(expression.len() - at, |end| end + 1);
        } else if call {
            names.push(name);
        }
    }
    names
}

/// The first definition of `definitions` (in order) that reaches itself through the
/// always-taken edges, as a [`Recursion`] of `kind`.
fn first_cycle(kind: &'static str, definitions: &[Definition]) -> Option<Recursion> {
    let edges: BTreeMap<&str, &[(String, usize)]> = definitions
        .iter()
        .rev()
        .map(|(name, calls)| (name.as_str(), calls.as_slice()))
        .collect();
    definitions.iter().find_map(|(start, calls)| {
        calls.iter().find_map(|(next, line)| {
            let mut path = vec![start.clone()];
            let mut seen = BTreeSet::new();
            let found = cycle_back(start, next, &edges, &mut path, &mut seen);
            found.then_some(Recursion {
                kind,
                cycle: path,
                line: *line,
            })
        })
    })
}

/// True if `start` is reached from `name` along `edges`; `path` then ends with the names
/// from `name` to `start`. `seen` holds the names already explored.
fn cycle_back(
    start: &str,
    name: &str,
    edges: &BTreeMap<&str, &[(String, usize)]>,
    path: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
) -> bool {
    path.push(name.to_string());
    if name == start {
        return true;
    }
    if seen.insert(name.to_string())
        && let Some(calls) = edges.get(name)
        && calls
            .iter()
            .any(|(next, _)| cycle_back(start, next, edges, path, seen))
    {
        return true;
    }
    path.pop();
    false
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

/// What [`inline_imports`] returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inlined {
    /// The stylesheet with its top-level local `@import`s replaced by the imported files and
    /// its other top-level `@import`s taken out.
    pub css: String,
    /// The `@import` rules taken out (of absolute or `data:` URLs), in order, as
    /// `(stylesheet, rule)`: the helper path of the stylesheet that has the rule, and the
    /// rule ending with `;`. A rule from a file inlined by an import with media queries
    /// carries them (`@import url(https://…) print;`). CSS ignores an `@import` that follows
    /// other rules, so a caller that concatenates stylesheets puts these first.
    pub imports: Vec<(String, String)>,
}

/// Replaces every top-level `@import` of a relative URL in `css`, the stylesheet at helper
/// path `key`, by the imported stylesheet and takes out the other top-level `@import`s
/// (see the module docs and [`Inlined`]).
///
/// The target of an import is looked up next to `key` (browser semantics), then via
/// [`reveal_fallback`]; it is compiled like an entry (`.scss`/`.sass` with grass, anything
/// else as written), its own imports are handled the same way (at most
/// [`MAX_IMPORT_DEPTH`] levels below `key`), and the result replaces the `@import`
/// (wrapped in `@media <queries> { … }` when the import has a media query list).
/// `finish(stylesheet, text)` is called for each run of a stylesheet's own text between
/// its imports (`key`'s and every imported file's, never text spliced in from another
/// file) and returns what goes into the result, e.g. the text with its `url()`s resolved.
/// Comments and string literals are skipped; imports inside a block (`@media … { … }`) are
/// left as written (browsers ignore them).
///
/// Errors: [`Error::Style`] naming the importing stylesheet (with the line of the import in
/// a CSS file) for a target that resolves to nothing, an import cycle, nesting deeper than
/// [`MAX_IMPORT_DEPTH`], a local import with a `layer(…)` or `supports(…)` condition (which
/// cannot be inlined), or an absolute import with media queries in a file inlined by an
/// import with media queries (the lists are not combined); compile errors of the imported
/// file; whatever `finish` returns.
pub fn inline_imports(
    theme: &Theme,
    key: &str,
    css: &str,
    finish: &mut dyn FnMut(&str, String) -> Result<String>,
) -> Result<Inlined> {
    let mut inliner = Inliner {
        theme,
        finish,
        stack: vec![key.to_string()],
        media: Vec::new(),
        imports: Vec::new(),
    };
    let css = inliner.inline(key, css)?;
    Ok(Inlined {
        css,
        imports: inliner.imports,
    })
}

/// The state of one [`inline_imports`] call.
struct Inliner<'a> {
    theme: &'a Theme,
    finish: &'a mut dyn FnMut(&str, String) -> Result<String>,
    /// The stylesheets being inlined, outermost first (the cycle guard).
    stack: Vec<String>,
    /// The media query lists of the imports being inlined (the non-empty ones).
    media: Vec<String>,
    /// The imports taken out so far ([`Inlined::imports`]).
    imports: Vec<(String, String)>,
}

impl Inliner<'_> {
    /// `css` (the stylesheet `key`, the last of the stack) with its own text finished and
    /// its top-level imports inlined or taken out.
    fn inline(&mut self, key: &str, css: &str) -> Result<String> {
        let mut out = String::with_capacity(css.len());
        let mut last = 0;
        let mut depth = 0usize;
        for caps in CSS_IMPORT.captures_iter(css) {
            let Some(whole) = caps.get(0) else {
                continue;
            };
            if let Some(brace) = caps.name("brace") {
                depth = match brace.as_str() {
                    "{" => depth + 1,
                    _ => depth.saturating_sub(1),
                };
                continue;
            }
            let Some(import) = ImportRule::of(&caps) else {
                continue; // a comment, a string or a url()
            };
            if depth > 0 {
                tracing::warn!(
                    stylesheet = key,
                    "{}: an @import inside a block is ignored by browsers; left as written",
                    import.text.trim_end()
                );
                continue;
            }
            self.finish_own(&mut out, key, &css[last..whole.start()])?;
            if import.is_local() {
                // Line numbers of compiled Sass would not match the source.
                let line = (SourceKind::of(key) != Some(SourceKind::Sass))
                    .then(|| css[..whole.start()].matches('\n').count() + 1);
                out.push_str(&self.splice(key, line, &import)?);
            } else {
                self.take_out(key, &import)?;
            }
            last = whole.end();
        }
        self.finish_own(&mut out, key, &css[last..])?;
        Ok(out)
    }

    /// Appends `text`, a run of the stylesheet `key`'s own text, after `finish`.
    fn finish_own(&mut self, out: &mut String, key: &str, text: &str) -> Result<()> {
        if !text.is_empty() {
            out.push_str(&(self.finish)(key, text.to_string())?);
        }
        Ok(())
    }

    /// The text that replaces the local `import` (on line `line`, if known) of the
    /// stylesheet `key`.
    fn splice(
        &mut self,
        key: &str,
        line: Option<usize>,
        import: &ImportRule<'_>,
    ) -> Result<String> {
        let error = |message: String| Error::Style {
            file: key.to_string(),
            message: match line {
                Some(line) => format!("line {line}: {message}"),
                None => message,
            },
        };
        let condition = import.condition.trim();
        if !split_condition(condition).0.is_empty() {
            return Err(error(format!(
                "@import url({}) {condition}: imports with layer() or supports() cannot be inlined",
                import.url
            )));
        }
        let Some((target, bytes)) = self.resolve(key, import.url)? else {
            return Err(error(not_found(key, import.url)));
        };
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
        let conditional = !condition.is_empty();
        if conditional {
            self.media.push(condition.to_string());
        }
        let inlined = self.inline(&target, &text);
        if conditional {
            self.media.pop();
        }
        self.stack.pop();
        let mut text = inlined?;
        if !text.ends_with('\n') {
            text.push('\n');
        }
        let text = format!("/* inlined {target} */\n{text}");
        Ok(match conditional {
            true => format!("@media {condition} {{\n{text}}}\n"),
            false => text,
        })
    }

    /// Takes the non-local `import` of the stylesheet `key` out ([`Inlined::imports`]),
    /// with the media queries of the imports being inlined.
    fn take_out(&mut self, key: &str, import: &ImportRule<'_>) -> Result<()> {
        let written = import.text.trim_end();
        let written = written.strip_suffix(';').unwrap_or(written).trim_end();
        let rule = match self.media.as_slice() {
            [] => written.to_string(),
            [outer] => {
                let condition = import.condition.trim();
                let (prefix, media) = split_condition(condition);
                if !media.is_empty() {
                    return Err(Error::Style {
                        file: key.to_string(),
                        message: format!(
                            "@import url({}) {condition}: its media queries cannot be combined \
                             with those of the import that inlines this stylesheet ({outer})",
                            import.url
                        ),
                    });
                }
                let head = import.head.trim_end();
                [head, prefix, outer.as_str()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
            }
            outer => {
                return Err(Error::Style {
                    file: key.to_string(),
                    message: format!(
                        "@import url({}): the media queries of the imports that inline this \
                         stylesheet ({}) cannot be combined",
                        import.url,
                        outer.join("; ")
                    ),
                });
            }
        };
        self.imports.push((key.to_string(), format!("{rule};")));
        Ok(())
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

/// The message for a local import `url` of the stylesheet `key` that resolves to nothing.
fn not_found(key: &str, url: &str) -> String {
    let mut message = format!("@import url({url}) not found next to the stylesheet");
    if reveal_fallback(url).is_some() {
        message.push_str(" or among reveal.js's theme sources");
    }
    if SourceKind::of(key) == Some(SourceKind::Sass) {
        message.push_str(
            " (Sass passes CSS imports through without their location: one written in a \
             partial is relative to the file compiled, too)",
        );
    }
    message
}

/// Splits the condition of an `@import` (trimmed) into its leading `layer`, `layer(…)` and
/// `supports(…)` part and its media query list.
fn split_condition(condition: &str) -> (&str, &str) {
    let mut rest = condition;
    loop {
        let len = if starts_with_ignore_case(rest, "layer(")
            || starts_with_ignore_case(rest, "supports(")
        {
            closing_paren(rest).map_or(rest.len(), |end| end + 1)
        } else if starts_with_ignore_case(rest, "layer")
            && rest[5..].chars().next().is_none_or(char::is_whitespace)
        {
            5
        } else {
            break;
        };
        rest = rest[len..].trim_start();
    }
    let prefix = condition[..condition.len() - rest.len()].trim_end();
    (prefix, rest.trim_end())
}

/// True if `text` starts with the ASCII `prefix`, ignoring ASCII case.
fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

/// The byte index of the `)` that closes the first `(` of `text`.
fn closing_paren(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (at, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

/// An `@import` rule found by [`CSS_IMPORT`].
struct ImportRule<'t> {
    /// The rule as written, up to and including its `;`.
    text: &'t str,
    /// The rule up to the end of its URL.
    head: &'t str,
    /// The URL as written (quotes removed, trimmed).
    url: &'t str,
    /// What follows the URL up to the `;` (media queries, `layer(…)`, `supports(…)`).
    condition: &'t str,
}

impl<'t> ImportRule<'t> {
    /// The import of a [`CSS_IMPORT`] match, if it is one.
    fn of(caps: &Captures<'t>) -> Option<ImportRule<'t>> {
        let whole = caps.get(0)?;
        let condition = caps.name("cond")?;
        let url = ["dq", "sq", "uq", "sdq", "ssq"]
            .iter()
            .find_map(|group| caps.name(group))?
            .as_str();
        Some(ImportRule {
            text: whole.as_str(),
            head: &whole.as_str()[..condition.start() - whole.start()],
            url: url.trim(),
            condition: condition.as_str(),
        })
    }

    /// True if the URL is relative (not empty, without a scheme, not `/…` or `//…`).
    fn is_local(&self) -> bool {
        !self.url.is_empty() && !self.url.starts_with(['/', '\\']) && !SCHEME.is_match(self.url)
    }
}

/// A URL scheme (`https:`, `data:`, …) at the start of a reference.
static SCHEME: LazyLock<Regex> = LazyLock::new(|| {
    // A constant pattern covered by the unit tests: compiling it cannot fail.
    Regex::new(r"^[A-Za-z][A-Za-z0-9+.\-]*:").expect("scheme regex is valid")
});

/// The CSS tokens [`inline_imports`] cares about, tried in this order at each position: a
/// comment (to the end of the text when unterminated) or a string literal, both matched to
/// be skipped; an `@import` rule up to its `;` (or the end of the text) whose URL is in
/// `url()` (`dq`/`sq` quoted, `uq` unquoted) or a string (`sdq`/`ssq`), followed by an
/// optional condition (`cond`); an unquoted `url(…)`, skipped (its argument may contain
/// braces); a `brace`, `{` or `}`, to tell top-level rules from nested ones.
static CSS_IMPORT: LazyLock<Regex> = LazyLock::new(|| {
    // A constant pattern covered by the unit tests: compiling it cannot fail.
    Regex::new(concat!(
        r"(?s)/\*.*?(?:\*/|\z)",
        r#"|"(?:[^"\\\n]|\\.)*(?:"|\n|\z)|'(?:[^'\\\n]|\\.)*(?:'|\n|\z)"#,
        r"|(?i:@import)\s*(?:(?i:url)\(\s*",
        r#"(?:"(?P<dq>[^"\\\n]*)"|'(?P<sq>[^'\\\n]*)'|(?P<uq>[^"'()\\\s]*))\s*\)"#,
        r#"|"(?P<sdq>[^"\\\n]*)"|'(?P<ssq>[^'\\\n]*)')"#,
        r"(?P<cond>[^;{}]*)(?:;|\z)",
        r#"|(?i:url)\((?:[^"'()\\]|\\.)*\)"#,
        r"|(?P<brace>[{}])",
    ))
    .expect("CSS import regex is valid")
});

#[cfg(test)]
mod tests {
    use super::*;

    fn imports(css: &str) -> Vec<(String, String)> {
        CSS_IMPORT
            .captures_iter(css)
            .filter_map(|caps| ImportRule::of(&caps))
            .filter(ImportRule::is_local)
            .map(|import| (import.url.to_string(), import.condition.trim().to_string()))
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
    fn top_level_imports_are_taken_out_and_nested_ones_left() {
        let theme = crate::theme::resolve("default", &[]).unwrap();
        let css = "/* @import url(c.css); */\n@import url(https://a.example/x.css) print;\n\
                   a { content: \"@import 'no';\"; background: url(data:x,{) }\n\
                   @media print { @import url(https://b.example/y.css); @import url(n.css); }\n\
                   @IMPORT \"//c.example/z.css\"";
        let mut seen = Vec::new();
        let inlined = inline_imports(&theme, "@default/styles/k.css", css, &mut |file, text| {
            seen.push((file.to_string(), text.clone()));
            Ok(text)
        })
        .unwrap();
        let key = "@default/styles/k.css".to_string();
        assert_eq!(
            inlined.imports,
            [
                (
                    key.clone(),
                    "@import url(https://a.example/x.css) print;".into()
                ),
                (key.clone(), "@IMPORT \"//c.example/z.css\";".into())
            ]
        );
        // The unquoted url() with a brace does not open a block; the imports inside
        // `@media` are invalid CSS and stay as written.
        assert_eq!(
            inlined.css,
            "/* @import url(c.css); */\n\na { content: \"@import 'no';\"; background: \
             url(data:x,{) }\n@media print { @import url(https://b.example/y.css); \
             @import url(n.css); }\n"
        );
        // `finish` saw every run of the stylesheet's own text once.
        let joined: String = seen.iter().map(|(_, text)| text.as_str()).collect();
        assert_eq!(joined, inlined.css);
        assert!(seen.iter().all(|(file, _)| *file == key));
    }

    #[test]
    fn conditions_split_into_layer_supports_and_media() {
        assert_eq!(split_condition(""), ("", ""));
        assert_eq!(split_condition("print"), ("", "print"));
        assert_eq!(split_condition("layer"), ("layer", ""));
        assert_eq!(split_condition("layers"), ("", "layers"));
        assert_eq!(
            split_condition("LAYER(a.b) supports(selector(a:has(b))) screen and (x)"),
            ("LAYER(a.b) supports(selector(a:has(b)))", "screen and (x)")
        );
        assert_eq!(
            split_condition("supports(display: grid"),
            ("supports(display: grid", "")
        );
        assert_eq!(closing_paren("a(b(c)d)e"), Some(7));
        assert_eq!(closing_paren("a)("), None);
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
        // The names grass gets carry a marker after their directory.
        let marked = VirtualFs::marked("@t/styles/theme.scss");
        assert_eq!(marked, "@t/styles\u{1}/theme.scss");
        assert_eq!(key(&marked).as_deref(), Some("@t/styles/theme.scss"));
        assert_eq!(
            key(&format!("{}/../x.scss", "@t/styles\u{1}")).as_deref(),
            Some("@t/x.scss")
        );
    }

    /// The kind, cycle and line [`unconditional_recursion`] finds in `scss`.
    fn recursion(scss: &str) -> Option<(&'static str, String, usize)> {
        unconditional_recursion(scss).map(|found| (found.kind, found.cycle.join(" > "), found.line))
    }

    #[test]
    fn unconditional_recursion_is_found() {
        let found = |kind, cycle: &str, line| Some((kind, cycle.to_string(), line));
        // The audit's examples (F02): a mixin without its base case, a function.
        let pad =
            "@mixin pad($n) { padding: $n; @include pad($n - 1); }\nbody { @include pad(3px); }\n";
        assert_eq!(recursion(pad), found("mixin", "pad > pad", 1));
        assert_eq!(
            recursion("@function f(){@return f();}"),
            found("function", "f > f", 1)
        );
        // Through others, on the line of the @include; `-` and `_` are the same name.
        let mutual = "@mixin a {\n  color: red;\n  @include b;\n}\n@mixin b { @include my_c(); }\n\
                      @mixin my-c { x: y; @include a }\n";
        assert_eq!(recursion(mutual), found("mixin", "a > b > my-c > a", 3));
        // Nested rules and @media blocks always run; so does a call before any @return.
        let nested = "@mixin m {\n  #{$sel} a { @media print { @include m; } }\n}";
        assert_eq!(recursion(nested), found("mixin", "m > m", 2));
        let before = "@function f($n) {\n  $x: 1 + f($n);\n  @return $x;\n}";
        assert_eq!(recursion(before), found("function", "f > f", 2));
        let debug = "@function g() { @debug g(); @return 1; }";
        assert_eq!(recursion(debug), found("function", "g > g", 1));
        // An unquoted url() with `//` is no comment.
        let url = "@mixin u { background: url(http://x.example/y.png); @include u; }";
        assert_eq!(recursion(url), found("mixin", "u > u", 1));
        // Mixins come first.
        let both = "@function f() { @return f(); }\n@mixin m { @include m; }";
        assert_eq!(recursion(both), found("mixin", "m > m", 2));
    }

    #[test]
    fn guarded_or_foreign_recursion_is_left_to_sass() {
        for scss in [
            // A base case.
            "@mixin pad($n) { @if $n > 0 { padding: $n; @include pad($n - 1); } }",
            "@mixin pad($n) { @if $n == 0 { a: b; } @else { @include pad($n - 1); } }",
            "@mixin l($list) { @each $x in $list { @include l(nth($x, 1)); } }",
            "@function f($n) {\n  @if $n <= 0 { @return 0; }\n  @return f($n - 1);\n}",
            "@function f($n) { @while $n > 0 { @return f($n - 1); } @return 0; }",
            "@function f($n) { @return if($n > 0, f($n - 1), 0); }",
            "@function f() { @error \"no\"; @return f(); }",
            // A content block may not run.
            "@mixin m { @include other { @include m; } }",
            // Another module's mixin, a variable, text.
            "@use \"m\";\n@mixin pad { @include m.pad; }",
            "@function f() { @return $f + m.f(1) + -f(2); }",
            "/* @mixin x { @include x; } */ // @mixin y { @include y; }\n\
             @mixin z { content: \"@include z;\"; } @mixin w { content: '}'; }",
            // Not a definition, or invalid.
            "a { @include a; }",
            "@mixin { @include x; }",
            "@mixin m { @include m",
            "}}} @mixin m { a: b }",
            "",
        ] {
            assert_eq!(recursion(scss), None, "{scss}");
        }
    }

    #[test]
    fn recursion_messages_quote_the_call() {
        let scss = "@mixin a {\n  @include b;\n}\n@mixin b { @include a; }\n";
        let message = unconditional_recursion(scss).unwrap().message(scss);
        assert_eq!(
            message,
            "line 2: @mixin a always includes itself through b, so Sass would recurse until it \
             runs out of stack; make the recursion depend on an @if that ends it\n 2 |   \
             @include b;"
        );
        let scss = "@function f() { @return f(); }";
        let message = unconditional_recursion(scss).unwrap().message(scss);
        assert!(
            message.starts_with("line 1: @function f always calls itself before any @return, so"),
            "{message}"
        );
    }

    #[test]
    fn no_embedded_stylesheet_recurses_unconditionally() {
        let mut checked = 0;
        for path in crate::vendor::list("") {
            if !path.to_ascii_lowercase().ends_with(".scss") {
                continue;
            }
            let text = crate::vendor::get_str(&path).unwrap();
            assert_eq!(unconditional_recursion(&text), None, "{path}");
            checked += 1;
        }
        assert!(checked > 10, "only {checked} embedded .scss files");
    }

    #[test]
    fn an_endless_loop_times_out_and_the_same_files_fail_at_once() {
        let theme = crate::theme::resolve("default", &[]).unwrap();
        let key = "@default/styles/endless-loop-test.scss";
        // Emits nothing: the thread left behind only keeps a CPU core busy.
        let endless = b"$i: 0;\n@while $i < 1 { $x: 1; }\n";
        let limit = Duration::from_millis(300);
        let start = Instant::now();
        let err = compile_sass_within(&theme, key, endless, limit).unwrap_err();
        let took = start.elapsed();
        assert!(took >= limit && took < Duration::from_secs(5), "{took:?}");
        let Error::Style { file, message } = &err else {
            panic!("{err:?}");
        };
        assert_eq!(file, key);
        assert!(
            message.starts_with("the Sass compilation did not finish within 0.3 s (an endless"),
            "{message}"
        );
        // The same inputs again: at once, without another thread.
        let start = Instant::now();
        let again = compile_sass_within(&theme, key, endless, limit).unwrap_err();
        assert!(start.elapsed() < limit, "{:?}", start.elapsed());
        assert!(
            again
                .to_string()
                .contains("an earlier compilation of the same files did not finish"),
            "{again}"
        );
        // Edited: compiled anew.
        let fixed = compile_sass_within(&theme, key, b"a { b: c; }", limit).unwrap();
        assert!(fixed.contains("b: c;"), "{fixed}");
    }

    #[test]
    fn deep_recursion_compiles_on_the_big_stack() {
        // About 2000 nested @include calls: far beyond a 2 MiB thread's stack.
        let theme = crate::theme::resolve("default", &[]).unwrap();
        let scss = "@mixin pad($n) { @if $n > 0 { @include pad($n - 1); } @else { a { b: c; } } }\n\
                    @include pad(2000);\n";
        let css =
            compile_sass_source(&theme, "@default/styles/deep.scss", scss.as_bytes()).unwrap();
        assert_eq!(css.trim(), "a {\n  b: c;\n}");
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
