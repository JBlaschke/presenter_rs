//! Crate-wide error type, `Result` alias and non-fatal diagnostics.
//!
//! Contract: `docs/spec/ir.md` §4–§5. [`Error`] is for failures that stop an operation;
//! [`Diagnostic`]s are warnings (and non-fatal errors) that frontends collect in
//! [`Diagnostics`] and hand to the pipeline and `check` through `ir::Parsed`.

use std::fmt;
use std::path::PathBuf;

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

/// TOML errors become [`Error::Config`]. The message is the full `toml` rendering (which
/// includes `line N, column M` and the offending line when the input is known), followed
/// by a `did you mean '…'?` hint for a misspelled key or value (see
/// [`crate::config::unknown_key_hint`]).
impl From<toml::de::Error> for Error {
    fn from(err: toml::de::Error) -> Self {
        Error::Config(crate::config::with_unknown_key_hint(err.to_string()))
    }
}

/// minijinja errors become [`Error::Template`] without debug info; use [`template_error`]
/// with `env.debug()` where the environment's debug flag should decide.
impl From<minijinja::Error> for Error {
    fn from(err: minijinja::Error) -> Self {
        template_error(err, false)
    }
}

/// Converts a minijinja error to [`Error::Template`].
///
/// minijinja reports an error raised inside an included template (or a `super()` block)
/// as an error located at the include site whose source is the original error. The
/// conversion walks that chain and reports the innermost located error: `template` and
/// `line` are where the problem is (e.g. `layouts/title.html`, line 4; `<unknown>` when
/// minijinja does not know, as for a top-level `get_template` of a missing name), and
/// `message` is its kind and detail, any further causes (`: cause`), then the include
/// sites from the inside out (`(via slide.html line 3, deck.html line 12)`). minijinja's
/// own `(in name:line)` suffix is left out since the location is in the other fields.
///
/// With `debug` (serve mode: pass `env.debug()`), the message also contains that error's
/// `display_debug_info()`: the template source around the line and the referenced
/// variables, when minijinja captured them.
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
        let _ = write!(message, " (via {})", sites.join(", "));
    }
    if debug {
        let _ = write!(message, "{}", origin.display_debug_info());
    }
    Error::Template {
        template: origin.name().unwrap_or("<unknown>").to_string(),
        line: origin.line(),
        message: message.trim_end().to_string(),
    }
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
}
