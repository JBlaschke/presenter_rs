//! Server-Sent Event payloads (`docs/spec/live-reload.md` §2): the `hello` event a browser
//! receives when it connects and the `build` event broadcast after every rebuild.
//!
//! The JSON shapes are the contract with the client script
//! (`assets/presenter/live-reload.js`):
//!
//! ```text
//! event: hello
//! data: {"build_id":17,"status":"ok"}
//!
//! event: build
//! data: {"status":"ok","build_id":18,"kind":"css","duration_ms":42}
//!
//! event: build
//! data: {"status":"error","build_id":20,"error":{"message":"…","file":"slides.md","line":41,"kind":"parse","excerpt":":::include other.md"}}
//! ```
//!
//! When the latest build failed, `hello` carries the same `error` object as a failed
//! `build` event (§2 addition, 2026-10-08). The error's `excerpt` (T44, additive) is the
//! offending source line, shown by the client's overlay; it is left out when unknown. A
//! missing asset or front matter error does not name the deck source it is located in:
//! [`ErrorPayload::for_input`] (with [`crate::serve::server::apply_error_payload`]) locates
//! and quotes it there, as the terminal report does; `presenter serve` stores every failed
//! build's error this way (audit F11). A `presenter.toml` error is located in that file
//! (audit F33).

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::Error;
use crate::serve::watch::ChangeKind;

/// One rebuild, as broadcast to every connected browser (`event: build`).
///
/// Serialized with the `status` tag: `{"status":"ok","build_id":18,"kind":"css",
/// "duration_ms":42}` or `{"status":"error","build_id":20,"error":{…}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum BuildEvent {
    /// The rebuild succeeded and the server now serves its document.
    Ok {
        /// The build's id (one per attempt, see [`crate::serve::server::apply_build`]).
        build_id: u64,
        /// What changed: only stylesheets (`css`) or anything else (`content`).
        kind: ChangeKind,
        /// How long the rebuild took, in milliseconds.
        duration_ms: u64,
    },
    /// The rebuild failed; the server keeps serving the last good document.
    Error {
        /// The attempt's id (see [`crate::serve::server::apply_error`]).
        build_id: u64,
        /// What went wrong, for the browser's error overlay.
        error: ErrorPayload,
    },
}

impl BuildEvent {
    /// The id of the build this event reports.
    pub fn build_id(&self) -> u64 {
        match self {
            BuildEvent::Ok { build_id, .. } | BuildEvent::Error { build_id, .. } => *build_id,
        }
    }
}

/// A build error as shown in the browser overlay: `{"message","file","line","kind"}`, plus
/// `"excerpt"` when the offending line is known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorPayload {
    /// The full error message: for a parse error `file:line: message` (live-reload.md §2),
    /// and so for a missing asset or front matter error located in the deck source
    /// ([`ErrorPayload::for_input`]) and a configuration error located in its `.toml` file;
    /// for every other error its `Display` form.
    pub message: String,
    /// The file the error is located in (source file, template, stylesheet or configuration
    /// file; the deck source of a missing asset or front matter error made
    /// [`ErrorPayload::for_input`]), if known. A source or configuration file is relative to
    /// the current directory when it is below it.
    pub file: Option<String>,
    /// 1-based line in `file`, if known.
    pub line: Option<usize>,
    /// The error's category in kebab-case: `parse`, `front-matter`, `config`, `theme`,
    /// `template`, `style`, `missing-asset`, `io`, `json` or `other`.
    pub kind: String,
    /// The offending source line as the overlay shows it (T44): for a parse error the line
    /// of the source file (of the cell's source for a notebook cell), and for a missing
    /// asset or front matter error the line of the deck source when the payload is made
    /// [`ErrorPayload::for_input`], read when the payload is made ([`Error::excerpt`]: tabs
    /// expanded, a long line cut around the error); else the line the message quotes
    /// ([`Error::quoted_line`]: a template error's minijinja debug info in serve mode, a
    /// `toml` or `grass` excerpt). Left out of the JSON when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
}

/// The overlay payload of a library error, with the source file of a parse error relative
/// to the current directory (as the terminal shows it): [`ErrorPayload::new`]`(err,
/// current_dir, None)`. A missing asset or front matter error is not located in the deck
/// source this way: use [`ErrorPayload::for_input`].
impl From<&Error> for ErrorPayload {
    fn from(err: &Error) -> Self {
        let cwd = std::env::current_dir().ok();
        ErrorPayload::new(err, cwd.as_deref(), None)
    }
}

impl ErrorPayload {
    /// The overlay payload of `err` from a build of the deck source `input`, with paths
    /// relative to the current directory: [`ErrorPayload::new`]`(err, current_dir,
    /// Some(input))`. What the rebuild loop of `presenter serve` stores, so that a missing
    /// asset or a front matter error (a YAML typo while editing) is located and quoted in
    /// the overlay as in the terminal.
    pub fn for_input(err: &Error, input: &Path) -> Self {
        let cwd = std::env::current_dir().ok();
        ErrorPayload::new(err, cwd.as_deref(), Some(input))
    }

    /// The overlay payload of `err` without a deck source: [`ErrorPayload::new`]`(err,
    /// base, None)`.
    pub fn relative_to(err: &Error, base: Option<&Path>) -> Self {
        ErrorPayload::new(err, base, None)
    }

    /// The overlay payload of `err`, from a build of the deck source `input` when given.
    /// `file`/`line` locate the error where it says where: the source file of
    /// [`Error::Parse`] (relative to `base` when it is below it, also when only one of the
    /// two is spelled through a symlink), the template of [`Error::Template`] and the
    /// stylesheet of [`Error::Style`]. A [`Error::MissingAsset`] or [`Error::FrontMatter`]
    /// is located in `input` (relative to `base` likewise); without `input` it has no
    /// `file` (a missing asset's path is the missing file, named in the message). A missing
    /// asset of a file the deck includes is located in that file, which its span names
    /// (`SourceSpan.file`, T48), with or without `input`. `line` is
    /// left out for a line of a notebook cell (it is not a line of the `.ipynb` file).
    ///
    /// `message` of a parse error is `file:line: message` (`file: message` without a line),
    /// as in live-reload.md §2; a missing asset or front matter error located in `input`
    /// gets the same prefix (`slides.md:12: …`, `talk.ipynb: cell 3, line 2: …`, or
    /// `slides.md: …` without a line); every other error's is its `Display` form, with a
    /// missing asset's path relative to `base`. `excerpt` is the offending line, see
    /// [`ErrorPayload::excerpt`].
    ///
    /// A configuration error that names its `.toml` file (`presenter.toml`, `--config`) is
    /// located there too (audit F33): `file` is that file relative to `base`, and for a TOML
    /// parse error `line` is its line and `message` `presenter.toml:23: <what is wrong>`,
    /// without the source lines `toml` quotes (the offending one is the `excerpt`);
    /// `presenter.toml: …` for another error about the file.
    pub fn new(err: &Error, base: Option<&Path>, input: Option<&Path>) -> Self {
        let (file, line, message) = match (err, input) {
            (
                Error::Parse {
                    path,
                    line,
                    message,
                },
                _,
            ) => {
                let file = display_relative(path, base);
                let message = match line {
                    Some(line) => format!("{file}:{line}: {message}"),
                    None => format!("{file}: {message}"),
                };
                (Some(file), *line, message)
            }
            (Error::Template { template, line, .. }, _) => {
                (Some(template.clone()), *line, err.to_string())
            }
            (Error::Style { file, .. }, _) => (Some(file.clone()), None, err.to_string()),
            (Error::MissingAsset { .. } | Error::FrontMatter(_), Some(input)) => {
                let file = display_relative(&in_file(err, input), base);
                let message = format!("{}: {}", in_input(&file, err), deck_message(err, base));
                (Some(file), file_line(err), message)
            }
            (Error::MissingAsset { .. }, None) => match included_file(err) {
                Some(file) => {
                    let file = display_relative(file, base);
                    let message = format!("{}: {}", in_input(&file, err), deck_message(err, base));
                    (Some(file), file_line(err), message)
                }
                None => (None, file_line(err), deck_message(err, base)),
            },
            (Error::Config(message), _) => match config_location(message, base) {
                Some((file, line, message)) => (Some(file), line, message),
                None => (None, None, err.to_string()),
            },
            _ => (None, None, err.to_string()),
        };
        ErrorPayload {
            message,
            file,
            line,
            kind: error_kind(err).to_string(),
            excerpt: excerpt(err, input),
        }
    }
}

/// The message of a missing asset (its path relative to `base`) or a front matter error
/// (without a cell prefix when its span names the cell, [`Error::message`]).
fn deck_message(err: &Error, base: Option<&Path>) -> String {
    match err {
        Error::MissingAsset { path, .. } => {
            format!("asset not found: {}", display_relative(path, base))
        }
        _ => err.message().into_owned(),
    }
}

/// The file (relative to `base`), line and overlay message of a configuration error whose
/// `message` names an existing `.toml` file (`<path>: …`, as `Config::load` writes it), see
/// [`ErrorPayload::new`]; `None` when it names none.
fn config_location(message: &str, base: Option<&Path>) -> Option<(String, Option<usize>, String)> {
    let at = message.find(".toml: ")? + ".toml".len();
    let (path, rest) = (&message[..at], &message[at + ": ".len()..]);
    if !Path::new(path).is_file() {
        return None; // e.g. `cannot read <path>: …`
    }
    let file = display_relative(Path::new(path), base);
    Some(match toml_parse_error(rest) {
        Some((line, text)) => (file.clone(), Some(line), format!("{file}:{line}: {text}")),
        None => (file.clone(), None, format!("{file}: {rest}")),
    })
}

/// The line and the explanation of a `toml` parse error (`TOML parse error at line L,
/// column C`, the source lines it quotes, then what is wrong); `None` for another message.
fn toml_parse_error(text: &str) -> Option<(usize, String)> {
    let rest = text.strip_prefix("TOML parse error at line ")?;
    let digits = rest.split(|c: char| !c.is_ascii_digit()).next()?;
    let line = digits.parse().ok()?;
    let quoted = |line: &&str| {
        let unnumbered = line
            .trim_start()
            .trim_start_matches(|c: char| c.is_ascii_digit());
        unnumbered.trim_start().starts_with('|')
    };
    let explanation: Vec<&str> = text.lines().skip(1).filter(|line| !quoted(line)).collect();
    Some((line, explanation.join("\n").trim().to_string()))
}

/// The line of `err`'s span when it is a line of the file (not of a notebook cell).
fn file_line(err: &Error) -> Option<usize> {
    err.span()
        .filter(|span| span.cell.is_none())
        .map(|span| span.start_line)
}

/// The file a missing asset of a file the deck includes is located in: its span's `file`
/// (`SourceSpan.file`, T48). `None` for any other error.
fn included_file(err: &Error) -> Option<&Path> {
    match err {
        Error::MissingAsset {
            span: Some(span), ..
        } => span.file.as_deref(),
        _ => None,
    }
}

/// The file a missing asset or front matter error of a build of `input` is located in: the
/// included file a missing asset's span names ([`included_file`]), else `input`.
fn in_file(err: &Error, input: &Path) -> PathBuf {
    included_file(err).unwrap_or(input).to_path_buf()
}

/// The location of `err` in the deck source shown as `file`: `file:line`,
/// `file: cell C, line L`, or `file` without a span.
fn in_input(file: &str, err: &Error) -> String {
    match err.span() {
        Some(span) => match span.cell {
            Some(cell) => format!("{file}: cell {cell}, line {}", span.start_line),
            None => format!("{file}:{}", span.start_line),
        },
        None => file.to_string(),
    }
}

/// The offending line of `err` for [`ErrorPayload::excerpt`]: read from the file it names
/// (or from `input` for a missing asset or front matter error), else quoted in its message.
/// A blank line is no excerpt.
fn excerpt(err: &Error, input: Option<&Path>) -> Option<String> {
    let read = |path: &Path| std::fs::read_to_string(path).ok();
    let file = match err {
        Error::MissingAsset { .. } | Error::FrontMatter(_) => included_file(err)
            .map(Path::to_path_buf)
            .or_else(|| input.map(Path::to_path_buf)),
        _ => err.path().map(Path::to_path_buf),
    };
    file.and_then(|file| err.excerpt(&file, &read))
        .map(|excerpt| excerpt.text)
        .filter(|text| !text.trim().is_empty())
        .or_else(|| err.quoted_line().map(|line| line.trim_end().to_string()))
}

/// `path` relative to `base` when it is strictly below it, else as is. The paths are
/// compared as given, then resolved through the filesystem (a source path can be given
/// through a symlink, e.g. `/var` for `/private/var` on macOS, while the current directory
/// is always resolved).
fn display_relative(path: &Path, base: Option<&Path>) -> String {
    let below = |path: &Path, base: &Path| -> Option<PathBuf> {
        let relative = path.strip_prefix(base).ok()?;
        (!relative.as_os_str().is_empty()).then(|| relative.to_path_buf())
    };
    match base.and_then(|base| {
        below(path, base).or_else(|| below(&path.canonicalize().ok()?, &base.canonicalize().ok()?))
    }) {
        // `/`-separated on every platform, as paths in messages are shown.
        Some(relative) => relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        None => path.display().to_string(),
    }
}

/// The `kind` of [`ErrorPayload`] for `err`.
fn error_kind(err: &Error) -> &'static str {
    match err {
        Error::Parse { .. } => "parse",
        Error::FrontMatter(_) => "front-matter",
        Error::Config(_) => "config",
        Error::Theme { .. } => "theme",
        Error::Template { .. } => "template",
        Error::Style { .. } => "style",
        Error::MissingAsset { .. } => "missing-asset",
        Error::Io(_) => "io",
        Error::Json(_) => "json",
        Error::Other(_) => "other",
    }
}

/// Whether the latest build attempt succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// The latest attempt succeeded (or none was made yet).
    Ok,
    /// The latest attempt failed.
    Error,
}

/// The `hello` event: the current build when a browser connects,
/// `{"build_id":17,"status":"ok"}`, plus `"error":{…}` when the latest build failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hello {
    /// The id of the latest build attempt (0 before the first).
    pub build_id: u64,
    /// Whether that attempt succeeded.
    pub status: Status,
    /// The latest attempt's error; left out of the JSON when it succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorPayload>,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::ir::SourceSpan;

    #[test]
    fn build_events_serialize_per_spec() {
        let ok = BuildEvent::Ok {
            build_id: 18,
            kind: ChangeKind::Css,
            duration_ms: 42,
        };
        assert_eq!(
            serde_json::to_value(&ok).unwrap(),
            json!({"build_id": 18, "status": "ok", "kind": "css", "duration_ms": 42})
        );
        assert_eq!(ok.build_id(), 18);
        // The spec's example error, converted from the library error.
        let parse = Error::Parse {
            path: PathBuf::from("slides.md"),
            line: Some(41),
            message: "unknown directive 'include'".into(),
        };
        let err = BuildEvent::Error {
            build_id: 20,
            error: ErrorPayload::from(&parse),
        };
        assert_eq!(
            serde_json::to_value(&err).unwrap(),
            json!({"build_id": 20, "status": "error", "error": {
                "message": "slides.md:41: unknown directive 'include'",
                "file": "slides.md", "line": 41, "kind": "parse"}})
        );
        assert_eq!(err.build_id(), 20);
    }

    #[test]
    fn hello_leaves_out_a_missing_error() {
        let hello = Hello {
            build_id: 17,
            status: Status::Ok,
            error: None,
        };
        assert_eq!(
            serde_json::to_string(&hello).unwrap(),
            r#"{"build_id":17,"status":"ok"}"#
        );
        let failed = Hello {
            build_id: 20,
            status: Status::Error,
            error: Some(ErrorPayload::from(&Error::Config("bad".into()))),
        };
        assert_eq!(
            serde_json::to_value(&failed).unwrap(),
            json!({"build_id": 20, "status": "error", "error": {
                "message": "config: bad", "file": null, "line": null, "kind": "config"}})
        );
    }

    #[test]
    fn payloads_locate_errors_where_known() {
        let parse = Error::Parse {
            path: PathBuf::from("slides.md"),
            line: Some(41),
            message: "unknown directive 'include'".into(),
        };
        assert_eq!(
            ErrorPayload::from(&parse),
            ErrorPayload {
                message: "slides.md:41: unknown directive 'include'".into(),
                file: Some("slides.md".into()),
                line: Some(41),
                kind: "parse".into(),
                excerpt: None,
            }
        );
        let template = Error::Template {
            template: "layouts/title.html".into(),
            line: Some(4),
            message: "undefined value".into(),
        };
        let payload = ErrorPayload::from(&template);
        assert_eq!(
            (payload.file.as_deref(), payload.line, payload.kind.as_str()),
            (Some("layouts/title.html"), Some(4), "template")
        );
        let style = Error::Style {
            file: "styles/theme.css".into(),
            message: "m".into(),
        };
        let payload = ErrorPayload::from(&style);
        assert_eq!(
            (payload.file.as_deref(), payload.line, payload.kind.as_str()),
            (Some("styles/theme.css"), None, "style")
        );
        let span = |cell| SourceSpan {
            start_line: 7,
            end_line: 7,
            cell,
            file: None,
        };
        let missing = |cell| Error::MissingAsset {
            path: PathBuf::from("/deck/x.png"),
            span: Some(span(cell)),
        };
        let payload = ErrorPayload::from(&missing(None));
        assert_eq!(
            (payload.file, payload.line, payload.kind.as_str()),
            (None, Some(7), "missing-asset")
        );
        assert_eq!(payload.message, "asset not found: /deck/x.png");
        // A notebook span's line counts within a cell, not the file: left out.
        assert_eq!(ErrorPayload::from(&missing(Some(3))).line, None);
    }

    #[test]
    fn parse_errors_name_the_source_relative_to_the_base() {
        let parse = |path: PathBuf, line| Error::Parse {
            path,
            line,
            message: "unbalanced \" quote".into(),
        };
        let base = Path::new("/work");
        let below = ErrorPayload::relative_to(
            &parse(PathBuf::from("/work/talk/slides.md"), Some(3)),
            Some(base),
        );
        assert_eq!(below.file.as_deref(), Some("talk/slides.md"));
        assert_eq!(below.message, "talk/slides.md:3: unbalanced \" quote");
        // Elsewhere, or without a base, the path stays as it is; no line, no `:line`.
        let elsewhere =
            ErrorPayload::relative_to(&parse(PathBuf::from("/other/s.md"), None), Some(base));
        assert_eq!(elsewhere.file.as_deref(), Some("/other/s.md"));
        assert_eq!(elsewhere.message, "/other/s.md: unbalanced \" quote");
        let unbased = ErrorPayload::relative_to(&parse(PathBuf::from("/work/s.md"), None), None);
        assert_eq!(unbased.file.as_deref(), Some("/work/s.md"));
        // The base itself is not shortened to an empty name.
        let same = ErrorPayload::relative_to(&parse(PathBuf::from("/work"), None), Some(base));
        assert_eq!(same.file.as_deref(), Some("/work"));
        // `From` uses the current directory, like the terminal report.
        let cwd = std::env::current_dir().unwrap();
        let from_cwd = ErrorPayload::from(&parse(cwd.join("talk/slides.md"), Some(41)));
        assert_eq!(from_cwd.file.as_deref(), Some("talk/slides.md"));
        assert!(from_cwd.message.ends_with(":41: unbalanced \" quote"));
    }

    #[test]
    fn payloads_carry_the_offending_line() {
        let tmp = tempfile::tempdir().unwrap();
        let deck = tmp.path().join("slides.md");
        std::fs::write(&deck, "# A\n\n--- {title=\"ab}\n").unwrap();
        let parse = Error::Parse {
            path: deck.clone(),
            line: Some(3),
            message: "unbalanced \" quote".into(),
        };
        let payload = ErrorPayload::relative_to(&parse, Some(tmp.path()));
        assert_eq!(payload.excerpt.as_deref(), Some("--- {title=\"ab}"));
        assert_eq!(
            serde_json::to_value(&payload).unwrap()["excerpt"],
            json!("--- {title=\"ab}")
        );
        // A notebook cell's line comes from the cell's source.
        let notebook = tmp.path().join("talk.ipynb");
        std::fs::write(
            &notebook,
            r##"{"cells": [{"cell_type": "markdown", "source": ["# T\n", "--- {x=\"y}"]}]}"##,
        )
        .unwrap();
        let in_cell = Error::Parse {
            path: notebook,
            line: None,
            message: "cell 0, line 2: unbalanced \" quote".into(),
        };
        assert_eq!(
            ErrorPayload::from(&in_cell).excerpt.as_deref(),
            Some("--- {x=\"y}")
        );
        // A template error in serve mode quotes the line in minijinja's debug info.
        let mut env = minijinja::Environment::new();
        env.set_debug(true);
        let err = env
            .add_template("slide.html", "<a>\n{{ x </a>\n")
            .unwrap_err();
        let template = ErrorPayload::from(&Error::from(err));
        assert_eq!(
            (template.file.as_deref(), template.line),
            (Some("slide.html"), Some(2))
        );
        assert_eq!(template.excerpt.as_deref(), Some("{{ x </a>"));
        // A blank line is no excerpt.
        std::fs::write(&deck, "# A\n\n   \n").unwrap();
        assert_eq!(
            ErrorPayload::relative_to(&parse, Some(tmp.path())).excerpt,
            None
        );
        // No line known (or the file is gone): no `excerpt` key.
        std::fs::remove_file(&deck).unwrap();
        let gone = ErrorPayload::relative_to(&parse, Some(tmp.path()));
        assert_eq!(gone.excerpt, None);
        let json = serde_json::to_value(&gone).unwrap();
        assert!(json.get("excerpt").is_none(), "{json}");
    }

    #[test]
    fn deck_errors_are_located_and_quoted_in_the_input() {
        let tmp = tempfile::tempdir().unwrap();
        let deck = tmp.path().join("slides.md");
        std::fs::write(&deck, "---\ntitle: a: b\n---\n\n![x](img/x.png)\n").unwrap();
        let base = Some(tmp.path());
        // A YAML typo while editing: the file, the line and the line itself.
        let front = Error::FrontMatter(
            "mapping values are not allowed in this context at line 2 column 9".into(),
        );
        let payload = ErrorPayload::new(&front, base, Some(&deck));
        assert_eq!(
            serde_json::to_value(&payload).unwrap(),
            json!({"message": "slides.md:2: front matter: mapping values are not allowed in \
                               this context at line 2 column 9",
                   "file": "slides.md", "line": 2, "kind": "front-matter",
                   "excerpt": "title: a: b"})
        );
        // Without the input: as before, the message only.
        let bare = ErrorPayload::relative_to(&front, base);
        assert_eq!(
            (bare.file, bare.line, bare.excerpt),
            (None, None, None),
            "no input"
        );
        // A missing asset: its path relative to the base, the reference's line.
        let missing = Error::MissingAsset {
            path: tmp.path().join("img/x.png"),
            span: Some(SourceSpan {
                start_line: 5,
                end_line: 5,
                cell: None,
                file: None,
            }),
        };
        let payload = ErrorPayload::new(&missing, base, Some(&deck));
        assert_eq!(payload.message, "slides.md:5: asset not found: img/x.png");
        assert_eq!(
            (payload.file.as_deref(), payload.line),
            (Some("slides.md"), Some(5))
        );
        assert_eq!(payload.excerpt.as_deref(), Some("![x](img/x.png)"));
        let bare = ErrorPayload::relative_to(&missing, base);
        assert_eq!(bare.message, "asset not found: img/x.png");
        assert_eq!((bare.file, bare.line), (None, Some(5)));
        // A missing asset of an included file (T48): located and quoted in that file.
        let part = tmp.path().join("parts/p.md");
        std::fs::create_dir_all(part.parent().unwrap()).unwrap();
        std::fs::write(&part, "# P\n\n![y](y.png)\n").unwrap();
        let included = Error::MissingAsset {
            path: tmp.path().join("parts/y.png"),
            span: Some(SourceSpan {
                start_line: 3,
                end_line: 3,
                cell: None,
                file: Some(part.clone()),
            }),
        };
        for input in [Some(deck.as_path()), None] {
            let payload = ErrorPayload::new(&included, base, input);
            assert_eq!(
                payload.message,
                "parts/p.md:3: asset not found: parts/y.png"
            );
            assert_eq!(
                (payload.file.as_deref(), payload.line),
                (Some("parts/p.md"), Some(3))
            );
            assert_eq!(payload.excerpt.as_deref(), Some("![y](y.png)"));
        }
        // An error without a position names the file.
        let unplaced = Error::FrontMatter("math: unknown field `enable`".into());
        let payload = ErrorPayload::new(&unplaced, base, Some(&deck));
        assert_eq!(
            payload.message,
            "slides.md: front matter: math: unknown field `enable`"
        );
        assert_eq!(
            (payload.file.as_deref(), payload.line, payload.excerpt),
            (Some("slides.md"), None, None)
        );
        // `for_input` is relative to the current directory.
        assert_eq!(
            ErrorPayload::for_input(&front, &deck),
            ErrorPayload::new(&front, std::env::current_dir().ok().as_deref(), Some(&deck))
        );
    }

    #[test]
    fn config_errors_are_located_in_their_file_without_the_quoted_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("presenter.toml");
        std::fs::write(
            &path,
            "[deck]\ntitle = \"x\"\n\n[slides]\nsplt = \"rules\"\n",
        )
        .unwrap();
        let err = crate::config::Config::load(&path).unwrap_err();
        let payload = ErrorPayload::relative_to(&err, Some(tmp.path()));
        assert_eq!(payload.file.as_deref(), Some("presenter.toml"));
        assert_eq!(payload.line, Some(5));
        assert_eq!(payload.kind, "config");
        assert!(
            payload
                .message
                .starts_with("presenter.toml:5: unknown field `splt`, expected one of `split`"),
            "{}",
            payload.message
        );
        assert!(
            payload.message.ends_with("(did you mean 'split'?)"),
            "{}",
            payload.message
        );
        assert!(!payload.message.contains('|'), "{}", payload.message);
        assert_eq!(payload.excerpt.as_deref(), Some("splt = \"rules\""));

        // Another error about the file: its name, no line; an unrelated one as it is.
        let plugins = Error::Config(format!(
            "{}: [reveal] plugins: unknown plugin",
            path.display()
        ));
        let payload = ErrorPayload::relative_to(&plugins, Some(tmp.path()));
        assert_eq!(
            payload.message,
            "presenter.toml: [reveal] plugins: unknown plugin"
        );
        assert_eq!(
            (payload.file.as_deref(), payload.line),
            (Some("presenter.toml"), None)
        );
        let unreadable = Error::Config(format!("cannot read {}: denied", path.display()));
        let payload = ErrorPayload::relative_to(&unreadable, Some(tmp.path()));
        assert_eq!(payload.message, unreadable.to_string());
        assert_eq!((payload.file, payload.line), (None, None));
        let bare = Error::Config("bad".into());
        assert_eq!(ErrorPayload::from(&bare).message, "config: bad");
    }

    #[test]
    fn notebook_front_matter_errors_name_the_cell_once() {
        let tmp = tempfile::tempdir().unwrap();
        let notebook = tmp.path().join("talk.ipynb");
        std::fs::write(
            &notebook,
            r##"{"cells": [{"cell_type": "markdown", "source": ["---\n", "title: a: b\n", "---\n"]}]}"##,
        )
        .unwrap();
        let err = Error::FrontMatter(
            "cell 0: mapping values are not allowed in this context at line 2 column 9 \
             (a `---` block …)"
                .into(),
        );
        let payload = ErrorPayload::new(&err, Some(tmp.path()), Some(&notebook));
        assert_eq!(
            payload.message,
            "talk.ipynb: cell 0, line 2: front matter: mapping values are not allowed in this \
             context at line 2 column 9 (a `---` block …)"
        );
        // A cell's line is not a line of the file.
        assert_eq!(
            (payload.file.as_deref(), payload.line),
            (Some("talk.ipynb"), None)
        );
        assert_eq!(payload.excerpt.as_deref(), Some("title: a: b"));
    }

    #[test]
    fn every_error_has_a_kind() {
        let io = Error::Io(std::io::Error::other("gone"));
        let json = Error::Json(serde_json::from_str::<serde_json::Value>("{").unwrap_err());
        let cases = [
            (Error::FrontMatter("x".into()), "front-matter"),
            (Error::Config("x".into()), "config"),
            (
                Error::Theme {
                    name: "t".into(),
                    message: "m".into(),
                },
                "theme",
            ),
            (io, "io"),
            (json, "json"),
            (Error::Other("x".into()), "other"),
        ];
        for (err, kind) in cases {
            let payload = ErrorPayload::from(&err);
            assert_eq!(payload.kind, kind);
            assert_eq!(payload.message, err.to_string());
            assert_eq!((payload.file, payload.line), (None, None));
        }
    }
}
