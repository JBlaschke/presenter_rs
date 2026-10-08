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
//! data: {"status":"error","build_id":20,"error":{"message":"…","file":"slides.md","line":41,"kind":"parse"}}
//! ```
//!
//! When the latest build failed, `hello` carries the same `error` object as a failed
//! `build` event (§2 addition, 2026-10-08).

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

/// A build error as shown in the browser overlay: `{"message","file","line","kind"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorPayload {
    /// The full error message: for a parse error `file:line: message` (live-reload.md §2),
    /// for every other error its `Display` form.
    pub message: String,
    /// The file the error is located in (source file, template or stylesheet), if known.
    /// A source file is relative to the current directory when it is below it.
    pub file: Option<String>,
    /// 1-based line in `file`, if known.
    pub line: Option<usize>,
    /// The error's category in kebab-case: `parse`, `front-matter`, `config`, `theme`,
    /// `template`, `style`, `missing-asset`, `io`, `json` or `other`.
    pub kind: String,
}

/// The overlay payload of a library error, with the source file of a parse error relative
/// to the current directory (as the terminal shows it): [`ErrorPayload::relative_to`]`(err,
/// current_dir)`.
impl From<&Error> for ErrorPayload {
    fn from(err: &Error) -> Self {
        let cwd = std::env::current_dir().ok();
        ErrorPayload::relative_to(err, cwd.as_deref())
    }
}

impl ErrorPayload {
    /// The overlay payload of `err`. `file`/`line` locate the error where it says where: the
    /// source file of [`Error::Parse`] (relative to `base` when it is below it, also when
    /// only one of the two is spelled through a symlink), the template of
    /// [`Error::Template`] and the stylesheet of [`Error::Style`]. A
    /// [`Error::MissingAsset`] has no `file` (its path is the missing asset, named in the
    /// message) and keeps the Markdown line of its span in `line`.
    ///
    /// `message` of a parse error is `file:line: message` (`file: message` without a line),
    /// as in live-reload.md §2; every other error's is its `Display` form.
    pub fn relative_to(err: &Error, base: Option<&Path>) -> Self {
        let (file, line, message) = match err {
            Error::Parse {
                path,
                line,
                message,
            } => {
                let file = display_relative(path, base);
                let message = match line {
                    Some(line) => format!("{file}:{line}: {message}"),
                    None => format!("{file}: {message}"),
                };
                (Some(file), *line, message)
            }
            Error::Template { template, line, .. } => {
                (Some(template.clone()), *line, err.to_string())
            }
            Error::Style { file, .. } => (Some(file.clone()), None, err.to_string()),
            Error::MissingAsset { span, .. } => (
                None,
                span.as_ref()
                    .filter(|span| span.cell.is_none())
                    .map(|span| span.start_line),
                err.to_string(),
            ),
            _ => (None, None, err.to_string()),
        };
        ErrorPayload {
            message,
            file,
            line,
            kind: error_kind(err).to_string(),
        }
    }
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
    base.and_then(|base| {
        below(path, base).or_else(|| below(&path.canonicalize().ok()?, &base.canonicalize().ok()?))
    })
    .unwrap_or_else(|| path.to_path_buf())
    .display()
    .to_string()
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
