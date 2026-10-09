//! Crate-wide error type, `Result` alias and non-fatal diagnostics.
//!
//! Contract: `docs/spec/ir.md` §4–§5. [`Error`] is for failures that stop an operation;
//! [`Diagnostic`]s are warnings (and non-fatal errors) that frontends collect in
//! [`Diagnostics`] and hand to the pipeline and `check` through `ir::Parsed`.
//!
//! Where an error is located (T44): [`Error::path`], [`Error::span`] and [`Error::column`]
//! read the location an error carries, in its fields or in the message conventions of the
//! frontends (a notebook cell's `cell N, line M: …`, YAML's `at line L column C`);
//! [`Error::excerpt`] cuts the offending source line out of the file for display, and
//! [`Error::quoted_line`] finds it in a message that a library already rendered with an
//! excerpt (minijinja's debug info, `toml`, `grass`).

use std::borrow::Cow;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::ir::SourceSpan;

/// Errors produced by the library.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// A source file could not be parsed.
    #[error("{}:{} {message}", .path.display(), .line.map(|l| l.to_string()).unwrap_or_default())]
    Parse {
        /// The file that failed to parse.
        path: PathBuf,
        /// 1-based line, when known.
        line: Option<usize>,
        /// What went wrong.
        message: String,
    },
    /// Invalid front matter (YAML syntax, wrong shapes).
    #[error("front matter: {0}")]
    FrontMatter(String),
    /// Invalid configuration (`presenter.toml`, config overrides, plugin names).
    #[error("config: {0}")]
    Config(String),
    /// A theme could not be resolved or loaded.
    #[error("theme '{name}': {message}")]
    Theme {
        /// Theme name or path as requested.
        name: String,
        /// What went wrong.
        message: String,
    },
    /// A template failed to load or render.
    #[error("template '{template}'{}: {message}", .line.map(|l| format!(" line {l}")).unwrap_or_default())]
    Template {
        /// Template name, e.g. `layouts/title.html`.
        template: String,
        /// 1-based line, when known.
        line: Option<usize>,
        /// What went wrong.
        message: String,
    },
    /// A stylesheet failed to compile or resolve.
    #[error("style '{file}': {message}")]
    Style {
        /// The stylesheet, as named in the theme.
        file: String,
        /// What went wrong.
        message: String,
    },
    /// A local file referenced by the deck does not exist.
    #[error("asset not found: {}", .path.display())]
    MissingAsset {
        /// The path as resolved (normally absolute).
        path: PathBuf,
        /// Where in the source it was referenced.
        span: Option<SourceSpan>,
    },
    /// I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// JSON (de)serialization failure.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Anything without a more specific variant.
    #[error("{0}")]
    Other(String),
}

/// Result alias used throughout the library.
pub type Result<T> = std::result::Result<T, Error>;

/// Where an error is located and what it says apart from that (T44: terminal reports in
/// `cli::report`, the browser overlay's payload in `serve::events`).
impl Error {
    /// The file this error's [`span`](Self::span) is in, when the error names it: the source
    /// file of [`Error::Parse`] (as the frontend was given it; `None` when empty). Every
    /// other variant gives `None`: the path of [`Error::MissingAsset`] is the missing file,
    /// while its span is in the deck source, which only the caller knows (the CLI adds it
    /// as context, `cli::locate`); a template's name is not a file path.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Error::Parse { path, .. } if !path.as_os_str().is_empty() => Some(path),
            _ => None,
        }
    }

    /// Where the error is located, as far as it says (one line; `cell` set for a line of a
    /// notebook cell's source):
    ///
    /// | variant | span |
    /// |---|---|
    /// | `Parse` with `line` | that line of [`path`](Self::path) |
    /// | `Parse` whose message starts `cell N, line M: ` (a notebook cell) | line M of cell N |
    /// | `MissingAsset` | its `span` (in the deck source) |
    /// | `FrontMatter` with `at line L column C` | line L of the deck source; of cell N for a notebook (`cell N: …`) |
    /// | `Template` with `line` | that line of the template |
    ///
    /// Every other error gives `None`.
    pub fn span(&self) -> Option<SourceSpan> {
        let at = |line: usize, cell: Option<usize>| SourceSpan {
            start_line: line,
            end_line: line,
            cell,
        };
        match self {
            Error::Parse {
                line: Some(line), ..
            } => Some(at(*line, None)),
            Error::Parse { message, .. } => {
                let (cell, line, _) = cell_location(message)?;
                Some(at(line?, Some(cell)))
            }
            Error::MissingAsset { span, .. } => span.clone(),
            Error::FrontMatter(message) => {
                let (line, _) = line_column(message)?;
                Some(at(line, cell_location(message).map(|(cell, ..)| cell)))
            }
            Error::Template { line, .. } => line.map(|line| at(line, None)),
            _ => None,
        }
    }

    /// The 1-based column on the [`span`](Self::span)'s line, when the message gives one:
    /// `at line L column C` of a front matter (YAML) or notebook JSON parse error, counted
    /// as the parser counts it: in characters for YAML, in bytes for JSON (`serde_json`;
    /// [`Error::excerpt`] converts it to a character). A message with two positions (YAML's
    /// `… at line 4 column 7, while parsing a flow sequence at line 3 column 7`) is located
    /// at the first, the problem; the second is its context.
    pub fn column(&self) -> Option<usize> {
        let message = match self {
            Error::Parse { message, .. } | Error::FrontMatter(message) => message,
            _ => return None,
        };
        let (line, column) = line_column(message)?;
        (self.span()?.start_line == line).then_some(column.max(1))
    }

    /// What went wrong without the location that [`path`](Self::path) and
    /// [`span`](Self::span) give: a parse error's message without its file, line or
    /// `cell N, line M: ` prefix; a front matter error located in a notebook cell without
    /// its `cell N: ` prefix (`front matter: …`). Every other error's `Display` form (a
    /// template error keeps its name and line, a missing asset names the missing file).
    pub fn message(&self) -> Cow<'_, str> {
        match self {
            Error::Parse { message, .. } => match cell_location(message) {
                Some((_, Some(_), rest)) => Cow::Borrowed(rest),
                _ => Cow::Borrowed(message),
            },
            Error::FrontMatter(message) => match (cell_location(message), self.span()) {
                (Some((_, None, rest)), Some(SourceSpan { cell: Some(_), .. })) => {
                    Cow::Owned(format!("front matter: {rest}"))
                }
                _ => Cow::Owned(self.to_string()),
            },
            other => Cow::Owned(other.to_string()),
        }
    }

    /// The offending line of the source `file`, cut for display: the line of
    /// [`span`](Self::span) (of the cell's source when the span names a cell: `file` is
    /// then the notebook), read through `source_cache`, with the part to underline: the
    /// character at the [`column`](Self::column) (a notebook JSON error's byte column is
    /// converted), the reference to the missing file of a [`Error::MissingAsset`] when the
    /// line contains it, else the line's text. A missing asset located in a notebook code
    /// cell whose line does not mention the file has no excerpt: a code cell's source names
    /// no files the build reads, so it is a file an output of the cell names, and the cell's
    /// source line would be unrelated (audit F38). In a Markdown or raw cell the line is
    /// where the author wrote the reference (maybe encoded, `my%20pic.png`) and is quoted.
    ///
    /// `file` is the file the span is in: [`path`](Self::path), or the deck source for a
    /// missing asset or front matter error, which the caller knows (`cli::locate`,
    /// `serve::events::ErrorPayload::for_input`). A template error's span is a line of the
    /// template, whose file only a caller that resolves the theme chain can name; the CLI's
    /// report and the overlay payload do not, and use [`quoted_line`](Self::quoted_line).
    /// `None` without a span, when `source_cache` has no text for `file`, or when the line
    /// does not exist (the file changed).
    pub fn excerpt(
        &self,
        file: &Path,
        source_cache: &dyn Fn(&Path) -> Option<String>,
    ) -> Option<Excerpt> {
        let span = self.span()?;
        let (line, cell_type) = located_line(&source_cache(file)?, &span)?;
        let target = match (self, self.column()) {
            // serde_json counts bytes (notebook JSON); YAML counts characters.
            (Error::Parse { .. }, Some(column)) => {
                let start = char_index(&line, column - 1);
                Some((start, start + 1))
            }
            (_, Some(column)) => Some((column - 1, column)),
            (Error::MissingAsset { path, .. }, None) => match reference_range(path, &line) {
                Some(range) => Some(range),
                // In a notebook code cell, a missing file that its line does not mention is
                // named by an output of the cell (whose span is the whole cell): the cell's
                // source line is unrelated, so none is quoted (audit F38).
                None if cell_type.as_deref() == Some("code") => return None,
                None => text_range(&line),
            },
            _ => text_range(&line),
        };
        Some(Excerpt::cut(span.start_line, &line, target))
    }

    /// The offending source line as a library already quoted it in the message (no file is
    /// read): the `  12 > …` line of minijinja's debug info (template errors in serve mode,
    /// syntax errors converted with `?`), else the only numbered line of a `toml` excerpt
    /// (`3 | …`: `theme.toml`, `presenter.toml`) or a `grass` one (`3 │ …`). `None` when
    /// the message quotes no line, or a blank one.
    pub fn quoted_line(&self) -> Option<&str> {
        match self {
            Error::Parse { message, .. }
            | Error::Template { message, .. }
            | Error::Theme { message, .. }
            | Error::Style { message, .. }
            | Error::FrontMatter(message)
            | Error::Config(message)
            | Error::Other(message) => quoted_line_in(message),
            Error::MissingAsset { .. } | Error::Io(_) | Error::Json(_) => None,
        }
    }
}

/// The widest [`Excerpt`] line, in characters: a longer line (minified HTML, a notebook's
/// JSON on one line) is cut around the error.
pub const EXCERPT_WIDTH: usize = 120;

/// The source line an error points at, as shown under the error ([`Error::excerpt`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Excerpt {
    /// The line's 1-based number: in the file, or in the cell's source for a notebook.
    pub line: usize,
    /// The line as shown: tabs expanded to four spaces, trailing whitespace removed, cut to
    /// [`EXCERPT_WIDTH`] characters around the error with `…` marking a cut end.
    pub text: String,
    /// The part of `text` to underline: its start column (0-based, in characters) and its
    /// width (at least 1). `None` for a blank line.
    pub marker: Option<(usize, usize)>,
}

/// The display width of a tab in an [`Excerpt`].
const TAB_WIDTH: usize = 4;

impl Excerpt {
    /// Line `number` (`line`) with the characters `target` (`start..end`, character indices
    /// into `line`) to underline, cut and expanded as [`Excerpt::text`] says.
    fn cut(number: usize, line: &str, target: Option<(usize, usize)>) -> Excerpt {
        let chars: Vec<char> = line.trim_end().chars().collect();
        let len = chars.len();
        let (from, to) = if len <= EXCERPT_WIDTH {
            (0, len)
        } else {
            // Keep the start of the target in the first third of the window.
            let focus = target.map_or(0, |(start, _)| start.min(len));
            let from = focus
                .saturating_sub(EXCERPT_WIDTH / 3)
                .min(len - EXCERPT_WIDTH);
            (from, from + EXCERPT_WIDTH)
        };
        let width = |part: &[char]| -> usize {
            part.iter()
                .map(|&c| if c == '\t' { TAB_WIDTH } else { 1 })
                .sum()
        };
        let lead = usize::from(from > 0);
        let mut text = String::from(if from > 0 { "…" } else { "" });
        for &c in &chars[from..to] {
            match c {
                '\t' => text.push_str(&" ".repeat(TAB_WIDTH)),
                c => text.push(c),
            }
        }
        if to < len {
            text.push('…');
        }
        let marker = target.map(|(start, end)| {
            let start = start.clamp(from, to);
            let end = end.clamp(start, to);
            let column = lead + width(&chars[from..start]);
            (column, width(&chars[start..end]).max(1))
        });
        Excerpt {
            line: number,
            text,
            marker,
        }
    }
}

/// Line `span.start_line` (1-based) of the source `text`, without its line break: of the
/// file, or, when `span.cell` is set, of that cell's `source` in the notebook JSON `text`
/// (cell indices count every cell of the notebook, as in [`SourceSpan`]). `None` when the
/// line (or the cell) does not exist.
pub fn source_line(text: &str, span: &SourceSpan) -> Option<String> {
    located_line(text, span).map(|(line, _)| line)
}

/// [`source_line`], with the `cell_type` of the cell when `span.cell` is set (the notebook
/// is parsed once: an excerpt is made for each missing file of a failed build).
fn located_line(text: &str, span: &SourceSpan) -> Option<(String, Option<String>)> {
    let index = span.start_line.checked_sub(1)?;
    let Some(cell) = span.cell else {
        let line = without_bom(text).lines().nth(index)?;
        return Some((line.to_string(), None));
    };
    let cell = notebook_cell(text, cell)?;
    let source: String = match cell.get("source")? {
        serde_json::Value::String(source) => source.clone(),
        serde_json::Value::Array(parts) => parts.iter().filter_map(|p| p.as_str()).collect(),
        _ => return None,
    };
    let cell_type = cell
        .get("cell_type")
        .and_then(|t| t.as_str())
        .map(str::to_string);
    Some((source.lines().nth(index)?.to_string(), cell_type))
}

/// `text` without one leading byte order mark, which the frontends ignore (audit F43).
fn without_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// Cell `cell` of the notebook JSON `text`; `None` when it is not there.
fn notebook_cell(text: &str, cell: usize) -> Option<serde_json::Value> {
    #[cfg(test)]
    NOTEBOOK_PARSES.set(NOTEBOOK_PARSES.get() + 1);
    let mut notebook: serde_json::Value = serde_json::from_str(without_bom(text)).ok()?;
    Some(notebook.get_mut("cells")?.get_mut(cell)?.take())
}

#[cfg(test)]
thread_local! {
    /// How many notebooks [`notebook_cell`] parsed on this thread (tests count them).
    static NOTEBOOK_PARSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// `cell N, line M: rest` or `cell N: rest` at the start of `message` (the notebook
/// frontend's location prefix): `(N, Some(M) | None, rest)`.
fn cell_location(message: &str) -> Option<(usize, Option<usize>, &str)> {
    let (cell, rest) = leading_number(message.strip_prefix("cell ")?)?;
    match rest.strip_prefix(", line ") {
        Some(rest) => {
            let (line, rest) = leading_number(rest)?;
            Some((cell, Some(line), rest.strip_prefix(": ")?))
        }
        None => Some((cell, None, rest.strip_prefix(": ")?)),
    }
}

/// The position of the problem in `message`, as serde_norway and serde_json locate their
/// errors (`at line L column C`, both counting from 1): the first such position that ends
/// the message or a clause (followed by `,`, as libyaml's problem position is by
/// `, while parsing … at line …`, its context; by ` (` and a frontend's hint; or by a line
/// break), else the first one. (A quoted value that reads `at line 1 column 1` is followed
/// by its closing quote and skipped.)
fn line_column(message: &str) -> Option<(usize, usize)> {
    const AT: &str = "at line ";
    let mut first = None;
    for (at, _) in message.match_indices(AT) {
        let Some((line, rest)) = leading_number(&message[at + AT.len()..]) else {
            continue;
        };
        let Some((column, rest)) = rest.strip_prefix(" column ").and_then(leading_number) else {
            continue;
        };
        if rest.is_empty() || rest.starts_with([',', '\n']) || rest.starts_with(" (") {
            return Some((line, column));
        }
        first.get_or_insert((line, column));
    }
    first
}

/// The index of the character of `line` that contains byte `offset`; the number of
/// characters when `offset` is past the end.
fn char_index(line: &str, offset: usize) -> usize {
    if offset >= line.len() {
        return line.chars().count();
    }
    line.char_indices()
        .take_while(|&(at, _)| at <= offset)
        .count()
        .saturating_sub(1)
}

/// The decimal number at the start of `text`, and the rest of `text`.
fn leading_number(text: &str) -> Option<(usize, &str)> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    Some((text[..digits].parse().ok()?, &text[digits..]))
}

/// See [`Error::quoted_line`].
fn quoted_line_in(message: &str) -> Option<&str> {
    let mut numbered = Vec::new();
    for line in message.lines() {
        let Some((_, rest)) = leading_number(line.trim_start()) else {
            continue;
        };
        if let Some(text) = rest.strip_prefix(" > ") {
            return Some(text).filter(|text| !text.trim().is_empty());
        }
        if let Some(text) = rest
            .strip_prefix(" | ")
            .or_else(|| rest.strip_prefix(" │ "))
        {
            numbered.push(text);
        }
    }
    match numbered.as_slice() {
        [only] if !only.trim().is_empty() => Some(only),
        _ => None,
    }
}

/// The characters of `line` (as `start..end` character indices) that refer to the missing
/// file `path`: the last occurrence of the longest trailing part of `path` (`plot.png`,
/// `figures/plot.png`, …) that `line` contains.
fn reference_range(path: &Path, line: &str) -> Option<(usize, usize)> {
    let names: Vec<&str> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect();
    (1..=names.len()).rev().find_map(|count| {
        let needle = names[names.len() - count..].join("/");
        let at = line.rfind(&needle)?;
        let start = line[..at].chars().count();
        Some((start, start + needle.chars().count()))
    })
}

/// The characters of `line` from its first to its last non-blank one; `None` when blank.
fn text_range(line: &str) -> Option<(usize, usize)> {
    let start = line.chars().position(|c| !c.is_whitespace())?;
    let len = line.trim_end().chars().count();
    Some((start, len))
}

/// TOML errors become [`Error::Config`]. The message is the full `toml` rendering (which
/// includes `line N, column M` and the offending line when the input is known), followed
/// by a `did you mean '…'?` hint for a misspelled key or value (see
/// [`crate::config::unknown_key_hint`]).
impl From<toml::de::Error> for Error {
    fn from(err: toml::de::Error) -> Self {
        Error::Config(crate::config::with_unknown_key_hint(err.to_string()))
    }
}

/// minijinja errors become [`Error::Template`] via [`template_error`]`(err, true)`: the
/// message includes minijinja's debug info whenever it was captured. minijinja captures it
/// for runtime errors only in a debug environment (`Environment::set_debug(true)`, serve
/// mode), so `?` gives "debug info in debug mode"; syntax errors always carry their source
/// excerpt, in build mode too. Call [`template_error`] with `false` to leave it out.
impl From<minijinja::Error> for Error {
    fn from(err: minijinja::Error) -> Self {
        template_error(err, true)
    }
}

/// Converts a minijinja error to [`Error::Template`].
///
/// minijinja reports an error raised inside an included template (or a `super()` block)
/// as an error located at the include site whose source is the original error. The
/// conversion walks that chain and reports the innermost located error: `template` and
/// `line` are where the problem is (e.g. `layouts/title.html`, line 4). For a top-level
/// "template not found" (`get_template` of a missing name) `template` is the missing name;
/// `<unknown>` only when minijinja names no template at all. `message` is the error's kind
/// and detail, any further causes (`: cause`), then the include sites from the inside out
/// (`(via slide.html line 3, deck.html line 12)`; repeated sites, as in a recursive
/// include, are collapsed to `site ×N`, and a list longer than four entries is shortened
/// to the first three, `… N more` and the last). minijinja's own `(in name:line)` suffix
/// is left out since the location is in the other fields.
///
/// With `debug`, the message also contains that error's `display_debug_info()`: the
/// template source around the line and the referenced variables, when minijinja captured
/// them (see `From<minijinja::Error>`).
pub fn template_error(err: minijinja::Error, debug: bool) -> Error {
    use std::fmt::Write as _;

    // The chain of minijinja errors, outermost first, and the first other cause after it.
    let mut frames: Vec<&minijinja::Error> = vec![&err];
    let mut cause = std::error::Error::source(&err);
    while let Some(inner) = cause.and_then(|c| c.downcast_ref::<minijinja::Error>()) {
        frames.push(inner);
        cause = std::error::Error::source(inner);
    }
    let at = frames
        .iter()
        .rposition(|frame| frame.line().is_some())
        .unwrap_or(0);
    let origin = frames[at];
    let describe = |frame: &minijinja::Error| match frame.detail() {
        Some(detail) => format!("{}: {detail}", frame.kind()),
        None => frame.kind().to_string(),
    };
    let mut message = frames[at..]
        .iter()
        .map(|frame| describe(frame))
        .collect::<Vec<_>>()
        .join(": ");
    while let Some(inner) = cause {
        // Writing to a String cannot fail.
        let _ = write!(message, ": {inner}");
        cause = inner.source();
    }
    let sites: Vec<String> = frames[..at]
        .iter()
        .rev()
        .filter_map(|frame| Some(format!("{} line {}", frame.name()?, frame.line()?)))
        .collect();
    if !sites.is_empty() {
        let _ = write!(message, " (via {})", include_sites(&sites));
    }
    if debug {
        let _ = write!(message, "{}", origin.display_debug_info());
    }
    let template = match origin.name() {
        Some(name) => name.to_string(),
        None => missing_template_name(origin).unwrap_or_else(|| "<unknown>".to_string()),
    };
    Error::Template {
        template,
        line: origin.line(),
        message: message.trim_end().to_string(),
    }
}

/// The most include sites [`include_sites`] lists before shortening the list.
const MAX_INCLUDE_SITES: usize = 4;

/// Formats include sites (innermost first) for the `(via …)` suffix: consecutive repeats
/// collapse to `site ×N`, and more than [`MAX_INCLUDE_SITES`] entries are shortened to the
/// first three, `… N more`, and the last.
fn include_sites(sites: &[String]) -> String {
    let mut runs: Vec<(&str, usize)> = Vec::new();
    for site in sites {
        match runs.last_mut() {
            Some((last, count)) if *last == site.as_str() => *count += 1,
            _ => runs.push((site, 1)),
        }
    }
    let mut shown: Vec<String> = runs
        .iter()
        .map(|&(site, count)| match count {
            1 => site.to_string(),
            n => format!("{site} ×{n}"),
        })
        .collect();
    if shown.len() > MAX_INCLUDE_SITES {
        // Keep the first MAX - 1 and the last entry.
        let hidden = shown.len() - MAX_INCLUDE_SITES;
        let end = shown.len() - 1;
        shown.splice(MAX_INCLUDE_SITES - 1..end, [format!("… {hidden} more")]);
    }
    shown.join(", ")
}

/// The name in minijinja's `template "<name>" does not exist`, for a "template not found"
/// error that carries no template name of its own (a top-level `get_template`).
fn missing_template_name(err: &minijinja::Error) -> Option<String> {
    if err.kind() != minijinja::ErrorKind::TemplateNotFound {
        return None;
    }
    let quoted = err
        .detail()?
        .strip_prefix("template ")?
        .strip_suffix(" does not exist")?;
    // minijinja formats the name with `{:?}`, which matches JSON string syntax for every
    // name without unusual escapes; fall back to stripping the quotes.
    serde_json::from_str::<String>(quoted).ok().or_else(|| {
        quoted
            .strip_prefix('"')?
            .strip_suffix('"')
            .map(str::to_string)
    })
}

/// Severity of a [`Diagnostic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Reported, does not fail the build (unless `--deny-warnings`).
    Warning,
    /// Reported; the operation's result is unusable.
    Error,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Level::Warning => "warning",
            Level::Error => "error",
        })
    }
}

/// One finding about a source file: a warning or a non-fatal error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    /// Severity.
    pub level: Level,
    /// Stable kebab-case identifier, e.g. `"missing-asset"`, `"empty-slide"`.
    pub code: &'static str,
    /// Human-readable message.
    pub message: String,
    /// File the finding is about (usually the source file).
    pub path: Option<PathBuf>,
    /// Where in that file.
    pub span: Option<SourceSpan>,
}

impl Diagnostic {
    /// A warning with no path or span.
    pub fn warning(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(Level::Warning, code, message)
    }

    /// An error-level diagnostic with no path or span.
    pub fn error(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(Level::Error, code, message)
    }

    /// A diagnostic with no path or span.
    pub fn new(level: Level, code: &'static str, message: impl Into<String>) -> Self {
        Diagnostic {
            level,
            code,
            message: message.into(),
            path: None,
            span: None,
        }
    }

    /// Builder: sets `path`.
    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Builder: sets `span`.
    pub fn with_span(mut self, span: SourceSpan) -> Self {
        self.span = Some(span);
        self
    }
}

/// The `docs/spec/cli.md` text format `path:line: level: message`, leaving out what is
/// unknown:
///
/// | path | span | output |
/// |---|---|---|
/// | yes | Markdown | `deck.md:12: warning: msg` |
/// | yes | notebook | `talk.ipynb: cell 7, line 2: warning: msg` |
/// | yes | none | `deck.md: warning: msg` |
/// | none | Markdown | `line 12: warning: msg` |
/// | none | notebook | `cell 7, line 2: warning: msg` |
/// | none | none | `warning: msg` |
///
/// A span's line is `start_line`; for notebooks (`span.cell` set) it counts within the
/// cell's source, so it is never printed as a line of the `.ipynb` file. The alternate form
/// (`{:#}`) appends the code: `deck.md:12: warning: msg [missing-asset]`.
impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.path, &self.span) {
            (Some(path), Some(span @ SourceSpan { cell: None, .. })) => {
                write!(f, "{}:{}: ", path.display(), span.start_line)?;
            }
            (Some(path), Some(span)) => write!(f, "{}: {}: ", path.display(), SpanText(span))?,
            (Some(path), None) => write!(f, "{}: ", path.display())?,
            (None, Some(span)) => write!(f, "{}: ", SpanText(span))?,
            (None, None) => {}
        }
        write!(f, "{}: {}", self.level, self.message)?;
        if f.alternate() {
            write!(f, " [{}]", self.code)?;
        }
        Ok(())
    }
}

/// A span without a path: `line N`, or `cell C, line N` for notebooks.
struct SpanText<'a>(&'a SourceSpan);

impl fmt::Display for SpanText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(cell) = self.0.cell {
            write!(f, "cell {cell}, ")?;
        }
        write!(f, "line {}", self.0.start_line)
    }
}

/// An ordered collection of [`Diagnostic`]s.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Diagnostics(Vec<Diagnostic>);

impl Diagnostics {
    /// An empty collection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `diagnostic`.
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.0.push(diagnostic);
    }

    /// Appends a warning and returns it so the caller can set `path`/`span`.
    pub fn warn(&mut self, code: &'static str, message: impl Into<String>) -> &mut Diagnostic {
        self.push_last(Diagnostic::warning(code, message))
    }

    /// Appends an error-level diagnostic and returns it so the caller can set `path`/`span`.
    pub fn error(&mut self, code: &'static str, message: impl Into<String>) -> &mut Diagnostic {
        self.push_last(Diagnostic::error(code, message))
    }

    /// Iterates in insertion order.
    pub fn iter(&self) -> std::slice::Iter<'_, Diagnostic> {
        self.0.iter()
    }

    /// True if any diagnostic has [`Level::Error`].
    pub fn has_errors(&self) -> bool {
        self.0.iter().any(|d| d.level == Level::Error)
    }

    /// Number of diagnostics.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True if there are none.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The diagnostics as a `Vec`, in insertion order.
    pub fn into_vec(self) -> Vec<Diagnostic> {
        self.0
    }

    fn push_last(&mut self, diagnostic: Diagnostic) -> &mut Diagnostic {
        self.0.push(diagnostic);
        let last = self.0.len() - 1;
        &mut self.0[last]
    }
}

/// `diagnostics.extend(other_diagnostics)` appends in order.
impl Extend<Diagnostic> for Diagnostics {
    fn extend<I: IntoIterator<Item = Diagnostic>>(&mut self, iter: I) {
        self.0.extend(iter);
    }
}

impl FromIterator<Diagnostic> for Diagnostics {
    fn from_iter<I: IntoIterator<Item = Diagnostic>>(iter: I) -> Self {
        Diagnostics(iter.into_iter().collect())
    }
}

impl From<Vec<Diagnostic>> for Diagnostics {
    fn from(diagnostics: Vec<Diagnostic>) -> Self {
        Diagnostics(diagnostics)
    }
}

impl IntoIterator for Diagnostics {
    type Item = Diagnostic;
    type IntoIter = std::vec::IntoIter<Diagnostic>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a Diagnostics {
    type Item = &'a Diagnostic;
    type IntoIter = std::slice::Iter<'a, Diagnostic>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(line: usize) -> SourceSpan {
        SourceSpan {
            start_line: line,
            end_line: line,
            cell: None,
        }
    }

    #[test]
    fn error_display_formats_per_spec() {
        let parse = Error::Parse {
            path: PathBuf::from("deck.md"),
            line: Some(12),
            message: "bad".into(),
        };
        assert_eq!(parse.to_string(), "deck.md:12 bad");
        let parse_no_line = Error::Parse {
            path: PathBuf::from("deck.md"),
            line: None,
            message: "bad".into(),
        };
        assert_eq!(parse_no_line.to_string(), "deck.md: bad");
        let template = Error::Template {
            template: "deck.html".into(),
            line: Some(3),
            message: "oops".into(),
        };
        assert_eq!(template.to_string(), "template 'deck.html' line 3: oops");
        let template_no_line = Error::Template {
            template: "deck.html".into(),
            line: None,
            message: "oops".into(),
        };
        assert_eq!(template_no_line.to_string(), "template 'deck.html': oops");
        let missing = Error::MissingAsset {
            path: PathBuf::from("/a/b.png"),
            span: Some(span(4)),
        };
        assert_eq!(missing.to_string(), "asset not found: /a/b.png");
        assert_eq!(
            Error::Theme {
                name: "nope".into(),
                message: "not found".into()
            }
            .to_string(),
            "theme 'nope': not found"
        );
        assert_eq!(
            Error::Style {
                file: "styles/x.css".into(),
                message: "m".into()
            }
            .to_string(),
            "style 'styles/x.css': m"
        );
        assert_eq!(
            Error::FrontMatter("x".into()).to_string(),
            "front matter: x"
        );
        assert_eq!(Error::Config("x".into()).to_string(), "config: x");
        assert_eq!(Error::Other("x".into()).to_string(), "x");
    }

    #[test]
    fn io_and_json_convert_transparently() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let err: Error = io.into();
        assert!(matches!(err, Error::Io(_)));
        assert_eq!(err.to_string(), "gone");
        let json = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let msg = json.to_string();
        let err: Error = json.into();
        assert!(matches!(err, Error::Json(_)));
        assert_eq!(err.to_string(), msg);
    }

    #[test]
    fn toml_errors_become_config_errors_with_line_numbers() {
        let err: Error = toml::from_str::<toml::Table>("a = 1\nb = = 2\n")
            .unwrap_err()
            .into();
        let Error::Config(message) = &err else {
            panic!("expected Error::Config, got {err:?}");
        };
        assert!(message.contains("line 2"), "{message}");
    }

    #[test]
    fn diagnostics_collect_in_order() {
        let mut diags = Diagnostics::new();
        assert!(diags.is_empty());
        diags.warn("empty-slide", "slide 2 is empty").span = Some(span(7));
        diags.push(Diagnostic::warning("missing-asset", "x.png").with_path("deck.md"));
        assert!(!diags.has_errors());
        diags.error("bad", "really bad");
        assert!(diags.has_errors());
        assert_eq!(diags.len(), 3);
        let codes: Vec<&str> = diags.iter().map(|d| d.code).collect();
        assert_eq!(codes, ["empty-slide", "missing-asset", "bad"]);
        assert_eq!(diags.iter().next().unwrap().span, Some(span(7)));

        let mut all = Diagnostics::from(vec![Diagnostic::error("first", "1")]);
        all.extend(diags.clone());
        assert_eq!(all.len(), 4);
        assert_eq!((&all).into_iter().next().unwrap().code, "first");
        let owned: Vec<Diagnostic> = all.into_iter().collect();
        assert_eq!(owned.len(), 4);
        assert_eq!(diags.clone().into_vec().len(), 3);
        let collected: Diagnostics = diags.into_iter().collect();
        assert_eq!(collected.len(), 3);
    }

    #[test]
    fn diagnostic_display_and_json() {
        let d = Diagnostic::warning("missing-asset", "image not found: x.png")
            .with_path("deck.md")
            .with_span(span(12));
        assert_eq!(d.to_string(), "deck.md:12: warning: image not found: x.png");
        assert_eq!(
            format!("{d:#}"),
            "deck.md:12: warning: image not found: x.png [missing-asset]"
        );
        assert_eq!(Diagnostic::error("e", "m").to_string(), "error: m");
        assert_eq!(format!("{:#}", Diagnostic::error("e", "m")), "error: m [e]");
        let json = serde_json::to_value(Diagnostics::from(vec![d])).unwrap();
        assert_eq!(
            json,
            serde_json::json!([{
                "level": "warning",
                "code": "missing-asset",
                "message": "image not found: x.png",
                "path": "deck.md",
                "span": { "start_line": 12, "end_line": 12, "cell": null }
            }])
        );
    }

    #[test]
    fn diagnostic_display_keeps_whatever_location_is_known() {
        let cell_span = SourceSpan {
            start_line: 2,
            end_line: 4,
            cell: Some(7),
        };
        let md = Diagnostic::warning("empty-slide", "slide 3 is empty");
        let cases = [
            (
                md.clone().with_span(span(3)),
                "line 3: warning: slide 3 is empty",
            ),
            (
                md.clone().with_path("deck.md"),
                "deck.md: warning: slide 3 is empty",
            ),
            (
                md.clone().with_path("deck.md").with_span(span(3)),
                "deck.md:3: warning: slide 3 is empty",
            ),
            (
                md.clone().with_span(cell_span.clone()),
                "cell 7, line 2: warning: slide 3 is empty",
            ),
            (
                md.with_path("talk.ipynb").with_span(cell_span),
                "talk.ipynb: cell 7, line 2: warning: slide 3 is empty",
            ),
        ];
        for (diagnostic, expected) in cases {
            assert_eq!(diagnostic.to_string(), expected);
        }
        let mut diags = Diagnostics::new();
        diags
            .warn("unknown-slide-attr", "unknown attribute foo")
            .span = Some(span(9));
        assert_eq!(
            format!("{:#}", diags.iter().next().unwrap()),
            "line 9: warning: unknown attribute foo [unknown-slide-attr]"
        );
    }

    #[test]
    fn include_sites_collapse_repeats_and_shorten_long_lists() {
        let sites =
            |names: &[&str]| -> Vec<String> { names.iter().map(|name| name.to_string()).collect() };
        assert_eq!(include_sites(&sites(&["a", "b"])), "a, b");
        assert_eq!(include_sites(&sites(&["a", "b", "b", "c"])), "a, b ×2, c");
        assert_eq!(
            include_sites(&sites(&["a", "a", "a", "b", "c", "d"])),
            "a ×3, b, c, d"
        );
        assert_eq!(
            include_sites(&sites(&["a", "b", "c", "c", "d", "e", "f"])),
            "a, b, c ×2, … 2 more, f"
        );
    }

    #[test]
    fn top_level_missing_templates_are_named() {
        let env = minijinja::Environment::new();
        let err = env.get_template("layouts/no \"pe\".html").unwrap_err();
        match Error::from(err) {
            Error::Template { template, line, .. } => {
                assert_eq!((template.as_str(), line), ("layouts/no \"pe\".html", None))
            }
            other => panic!("expected Error::Template, got {other:?}"),
        }
        let err = minijinja::Error::new(minijinja::ErrorKind::InvalidOperation, "x");
        let Error::Template { template, .. } = Error::from(err) else {
            panic!("expected Error::Template");
        };
        assert_eq!(template, "<unknown>");
    }

    fn parse_error(path: &str, line: Option<usize>, message: &str) -> Error {
        Error::Parse {
            path: PathBuf::from(path),
            line,
            message: message.into(),
        }
    }

    fn cell_span(line: usize, cell: usize) -> SourceSpan {
        SourceSpan {
            cell: Some(cell),
            ..span(line)
        }
    }

    #[test]
    fn errors_say_where_they_are() {
        let markdown = parse_error("deck.md", Some(12), "bad");
        assert_eq!(markdown.path(), Some(Path::new("deck.md")));
        assert_eq!(markdown.span(), Some(span(12)));
        assert_eq!(
            (markdown.column(), markdown.message()),
            (None, "bad".into())
        );
        // The notebook frontend puts a cell's location in front of the message.
        let cell = parse_error("talk.ipynb", None, "cell 4, line 5: unbalanced");
        assert_eq!(cell.span(), Some(cell_span(5, 4)));
        assert_eq!(cell.message(), "unbalanced");
        let whole_cell = parse_error("talk.ipynb", None, "cell 4: missing source");
        assert_eq!(whole_cell.span(), None);
        assert_eq!(whole_cell.message(), "cell 4: missing source");
        // Notebook JSON: the line and column of the file.
        let json = parse_error("talk.ipynb", Some(2), "invalid: key at line 2 column 28");
        assert_eq!((json.span(), json.column()), (Some(span(2)), Some(28)));
        assert_eq!(parse_error("", Some(3), "x").path(), None);

        let missing = Error::MissingAsset {
            path: PathBuf::from("/deck/x.png"),
            span: Some(cell_span(2, 7)),
        };
        assert_eq!(
            (missing.path(), missing.span()),
            (None, Some(cell_span(2, 7)))
        );
        assert_eq!(missing.message(), "asset not found: /deck/x.png");

        let front = Error::FrontMatter("mapping values are not allowed at line 2 column 9".into());
        assert_eq!((front.span(), front.column()), (Some(span(2)), Some(9)));
        assert_eq!(front.path(), None);
        let in_cell = Error::FrontMatter(
            "cell 0: bad at line 3 column 1 (a `---` block at the start of …)".into(),
        );
        assert_eq!(
            (in_cell.span(), in_cell.column()),
            (Some(cell_span(3, 0)), Some(1))
        );
        // The cell is in the span: the message does not repeat it.
        assert_eq!(
            in_cell.message(),
            "front matter: bad at line 3 column 1 (a `---` block at the start of …)"
        );
        let no_position = Error::FrontMatter("math: unknown field `enable`".into());
        assert_eq!((no_position.span(), no_position.column()), (None, None));
        // Without a line the cell prefix is the only location: it stays.
        let cell_only = Error::FrontMatter("cell 2: math: unknown field `enable`".into());
        assert_eq!(cell_only.span(), None);
        assert_eq!(
            cell_only.message(),
            "front matter: cell 2: math: unknown field `enable`"
        );

        let template = Error::Template {
            template: "layouts/title.html".into(),
            line: Some(4),
            message: "oops".into(),
        };
        assert_eq!((template.path(), template.span()), (None, Some(span(4))));
        assert_eq!(
            template.message(),
            "template 'layouts/title.html' line 4: oops"
        );
        for other in [
            Error::Config("at line 1 column 2".into()),
            Error::Other("cell 1, line 2: x".into()),
        ] {
            assert_eq!(
                (other.path(), other.span(), other.column()),
                (None, None, None)
            );
        }
    }

    #[test]
    fn errors_with_two_positions_are_located_at_the_problem() {
        // libyaml: the problem's position, then its context's.
        let front = |message: &str| Error::FrontMatter(message.into());
        let two = front(
            "did not find expected key at line 2 column 16, while parsing a block mapping at \
             line 2 column 1",
        );
        assert_eq!((two.span(), two.column()), (Some(span(2)), Some(16)));
        let flow = front(
            "did not find expected ',' or ']' at line 4 column 7, while parsing a flow \
             sequence at line 3 column 7",
        );
        assert_eq!((flow.span(), flow.column()), (Some(span(4)), Some(7)));
        // A quoted value that reads like a position is not one.
        let quoted = front(
            "title: invalid type: string \"at line 9 column 9\", expected a mapping at line 3 \
             column 8",
        );
        assert_eq!((quoted.span(), quoted.column()), (Some(span(3)), Some(8)));
        // A notebook's hint follows the position.
        let hinted = front("cell 0: bad at line 3 column 2 (a `---` block …)");
        assert_eq!(hinted.column(), Some(2));
        // Nothing ends a clause: the first position.
        let odd = front("x at line 5 column 1 y at line 6 column 2 z");
        assert_eq!(odd.span(), Some(span(5)));
        assert_eq!(front("at line x column 1").span(), None);
    }

    #[test]
    fn json_byte_columns_mark_the_character() {
        // serde_json counts bytes: `é` is two, `世` three.
        let line = "  {\"source\": \"Grüße, 世界\", , }";
        let byte = line.rfind(", }").unwrap() + 1;
        let json = parse_error(
            "talk.ipynb",
            Some(1),
            &format!("invalid notebook JSON: expected value at line 1 column {byte}"),
        );
        let excerpt = excerpt_of(&json, line).unwrap();
        let (column, width) = excerpt.marker.unwrap();
        assert_eq!(
            (excerpt.text.chars().nth(column), width),
            (Some(','), 1),
            "{excerpt:?}"
        );
        assert_eq!(column, line.chars().count() - 3);
        // YAML counts characters: no conversion.
        let yaml = "title: \"éééééé\": b";
        let front = Error::FrontMatter("mapping values are not allowed at line 1 column 16".into());
        let (column, _) = excerpt_of(&front, yaml).unwrap().marker.unwrap();
        assert_eq!(yaml.chars().nth(column), Some(':'));
        // A byte inside a character marks that character; past the end, the end.
        assert_eq!(char_index("aéb", 2), 1);
        assert_eq!(char_index("aéb", 3), 2);
        assert_eq!(char_index("aéb", 9), 3);
        assert_eq!(char_index("", 0), 0);
    }

    #[test]
    fn source_lines_of_files_and_notebook_cells() {
        let text = "\u{feff}one\r\ntwo\n\nfour";
        assert_eq!(source_line(text, &span(1)).as_deref(), Some("one"));
        assert_eq!(source_line(text, &span(3)).as_deref(), Some(""));
        assert_eq!(source_line(text, &span(4)).as_deref(), Some("four"));
        assert_eq!(source_line(text, &span(5)), None);
        assert_eq!(source_line(text, &span(0)), None);
        let notebook = r##"{"cells": [
            {"cell_type": "code", "source": "a = 1\nb = 2"},
            {"cell_type": "markdown", "source": ["# T\n", "\n", "![x](y.png)"]}]}"##;
        assert_eq!(
            source_line(notebook, &cell_span(2, 0)).as_deref(),
            Some("b = 2")
        );
        assert_eq!(
            source_line(notebook, &cell_span(3, 1)).as_deref(),
            Some("![x](y.png)")
        );
        assert_eq!(source_line(notebook, &cell_span(4, 1)), None);
        assert_eq!(source_line(notebook, &cell_span(1, 2)), None);
        assert_eq!(source_line("not json", &cell_span(1, 0)), None);
        // A notebook with a byte order mark, which the frontend accepts (audit F43).
        let bom = format!("\u{feff}{notebook}");
        assert_eq!(
            source_line(&bom, &cell_span(3, 1)).as_deref(),
            Some("![x](y.png)")
        );
    }

    #[test]
    fn missing_files_of_notebook_outputs_quote_no_unrelated_line() {
        let notebook = r##"{"cells": [
            {"cell_type": "code", "source": "x\ny = 2"},
            {"cell_type": "code", "source": "display(Image('plot.png'))"}]}"##;
        let missing = |path: &str, line, cell| Error::MissingAsset {
            path: PathBuf::from(path),
            span: Some(cell_span(line, cell)),
        };
        // The cell's line does not mention the file: an output names it, nothing is quoted.
        assert_eq!(
            excerpt_of(&missing("/nb/missing.png", 1, 0), notebook),
            None
        );
        // A line that mentions it is quoted with the reference underlined.
        let excerpt = excerpt_of(&missing("/nb/plot.png", 1, 1), notebook).unwrap();
        assert_eq!(excerpt.text, "display(Image('plot.png'))");
        assert_eq!(excerpt.marker, Some((15, 8)));
    }

    /// Wave-8 review of the A4 fixes: in a Markdown (or raw) cell the line is the author's
    /// reference, quoted whole when it spells the file another way (`my%20pic.png`, as in a
    /// Markdown deck); only a code cell's line is unrelated to a missing file.
    #[test]
    fn missing_files_of_markdown_cells_quote_their_line() {
        let notebook = r##"{"cells": [
            {"cell_type": "markdown", "source": "# T\n\n![x](my%20pic.png)"},
            {"cell_type": "raw", "source": "<img src=\"a&amp;b.png\">"},
            {"cell_type": "code", "source": "show()"}]}"##;
        let missing = |path: &str, line, cell| Error::MissingAsset {
            path: PathBuf::from(path),
            span: Some(cell_span(line, cell)),
        };
        let excerpt = excerpt_of(&missing("/nb/my pic.png", 3, 0), notebook).unwrap();
        assert_eq!(excerpt.line, 3);
        assert_eq!(excerpt.text, "![x](my%20pic.png)");
        assert_eq!(excerpt.marker, Some((0, 18)));
        let excerpt = excerpt_of(&missing("/nb/a&b.png", 1, 1), notebook).unwrap();
        assert_eq!(excerpt.text, "<img src=\"a&amp;b.png\">");
        // The notebook is parsed once for each excerpt (wave-8 review of W8FIX).
        let before = NOTEBOOK_PARSES.get();
        assert_eq!(excerpt_of(&missing("/nb/out.png", 1, 2), notebook), None);
        assert_eq!(NOTEBOOK_PARSES.get() - before, 1);
        // With a byte order mark too (audit F43).
        let bom = format!("\u{feff}{notebook}");
        assert!(excerpt_of(&missing("/nb/my pic.png", 3, 0), &bom).is_some());
        assert_eq!(excerpt_of(&missing("/nb/out.png", 1, 2), &bom), None);
    }

    /// The excerpt of `err` in a file with `text`.
    fn excerpt_of(err: &Error, text: &str) -> Option<Excerpt> {
        err.excerpt(Path::new("f"), &|_| Some(text.to_string()))
    }

    #[test]
    fn excerpts_underline_the_column_the_reference_or_the_text() {
        let err = parse_error("f", Some(2), "bad");
        let excerpt = excerpt_of(&err, "one\n  <!-- .slide: x -->  \n").unwrap();
        assert_eq!(excerpt.line, 2);
        assert_eq!(excerpt.text, "  <!-- .slide: x -->");
        assert_eq!(excerpt.marker, Some((2, 18)));
        // A blank line has nothing to underline; a missing line no excerpt.
        assert_eq!(excerpt_of(&err, "one\n \t\n").unwrap().marker, None);
        assert_eq!(excerpt_of(&err, "one\n"), None);
        assert_eq!(err.excerpt(Path::new("f"), &|_| None), None);

        let front = Error::FrontMatter("bad at line 1 column 3".into());
        assert_eq!(excerpt_of(&front, "\ta: b").unwrap().marker, Some((5, 1)));
        assert_eq!(excerpt_of(&front, "\ta: b").unwrap().text, "    a: b");

        let missing = |path: &str| Error::MissingAsset {
            path: PathBuf::from(path),
            span: Some(span(1)),
        };
        let line = "![plot.png](figures/plot.png) and more";
        let marker = |path| excerpt_of(&missing(path), line).unwrap().marker;
        assert_eq!(marker("/deck/figures/plot.png"), Some((12, 16)));
        assert_eq!(marker("/elsewhere/plot.png"), Some((20, 8)));
        assert_eq!(
            marker("/deck/other.png"),
            Some((0, 38)),
            "not referenced: the text"
        );
    }

    #[test]
    fn long_lines_are_cut_around_the_error() {
        let line = format!("{}X{}", "a".repeat(300), "b".repeat(300));
        let front = Error::FrontMatter("bad at line 1 column 301".into());
        let excerpt = excerpt_of(&front, &line).unwrap();
        assert_eq!(excerpt.text.chars().count(), EXCERPT_WIDTH + 2);
        assert!(excerpt.text.starts_with('…') && excerpt.text.ends_with('…'));
        let (column, width) = excerpt.marker.unwrap();
        assert_eq!(excerpt.text.chars().nth(column), Some('X'));
        assert_eq!(width, 1);
        // The start of a line is kept without a mark.
        let start = Error::FrontMatter("bad at line 1 column 2".into());
        let excerpt = excerpt_of(&start, &line).unwrap();
        assert!(excerpt.text.starts_with("aaa") && excerpt.text.ends_with('…'));
        assert_eq!(excerpt.marker, Some((1, 1)));
        // A column past the end of the line marks the end.
        let past = Error::FrontMatter("bad at line 1 column 9".into());
        assert_eq!(excerpt_of(&past, "ab").unwrap().marker, Some((2, 1)));
    }

    #[test]
    fn quoted_lines_come_from_minijinja_toml_and_grass_excerpts() {
        // minijinja's debug info: the `>` line.
        let mut env = minijinja::Environment::new();
        env.set_debug(true);
        let err = env
            .add_template("t.html", "<a>\n<h1>{{ x </h1>\n</a>\n")
            .unwrap_err();
        let err = Error::from(err);
        assert_eq!(err.quoted_line(), Some("<h1>{{ x </h1>"));
        // toml (theme.toml, presenter.toml): its one numbered line.
        let err: Error = toml::from_str::<toml::Table>("a = 1\nb = = 2\n")
            .unwrap_err()
            .into();
        assert_eq!(err.quoted_line(), Some("b = = 2"));
        let grass = Error::Style {
            file: "styles/x.scss".into(),
            message: "Error: expected \";\".\n  ╷\n2 │ a { color: red }\n  │ ^\n  ╵\n".into(),
        };
        assert_eq!(grass.quoted_line(), Some("a { color: red }"));
        for message in ["no excerpt", "1 | one\n2 | two", "3 |  \n"] {
            assert_eq!(
                Error::Other(message.into()).quoted_line(),
                None,
                "{message}"
            );
        }
        let blank = Error::Other("   3 > \n".into());
        assert_eq!(blank.quoted_line(), None);
    }
}
