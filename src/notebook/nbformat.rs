//! nbformat 4 data model with lenient deserialization.
//!
//! Contract: `docs/spec/notebook-mapping.md` §1. The entry point is [`parse_notebook`]: it
//! rejects anything older than nbformat 4, deserializes every cell, and **drops** outputs
//! and attachments that do not deserialize (an unknown `output_type`, a stream without
//! `text`, …), reporting each as a warning [`Diagnostic`] instead of failing the deck.
//! Other recoverable oddities (an unknown `slideshow.slide_type` or `cell_type`, a
//! `metadata.presenter` that is not an object) are reported the same way; the codes are in
//! [`codes`]. Unknown keys anywhere are ignored, and `null` counts as absent everywhere.
//!
//! The accessors on [`Cell`] and [`Notebook`] read the nbformat metadata only. Tags are
//! returned as written; mapping them to effects (`hide-input`, `fragment`, …, configurable
//! through `[notebook] hide_input_tags` and friends) is the converter's job
//! (`notebook::convert`, spec §3).

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::Deserialize;
use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde_json::{Map, Value};

use crate::error::{Diagnostic, Diagnostics, Error};
use crate::ir::SourceSpan;

/// The diagnostic codes of the warnings [`parse_notebook`] returns (all `Level::Warning`).
pub mod codes {
    /// `metadata.slideshow.slide_type` is not a known value; the cell is read as `"-"`.
    pub const UNKNOWN_SLIDE_TYPE: &str = "unknown-slide-type";
    /// `cell_type` is not `markdown`, `code` or `raw` (a cell from a newer nbformat minor
    /// version); the cell is kept as `CellType::Unknown`, which the converter skips.
    pub const UNKNOWN_CELL_TYPE: &str = "unknown-cell-type";
    /// An output that does not deserialize (unknown `output_type`, missing field, …) was
    /// dropped, or `outputs` was not a list and was ignored.
    pub const OUTPUT_SKIPPED: &str = "notebook-output-skipped";
    /// An attachment that does not deserialize was dropped, or `attachments` was not an
    /// object and was ignored.
    pub const ATTACHMENT_SKIPPED: &str = "notebook-attachment-skipped";
    /// `metadata.presenter` (notebook or cell) is not an object and was ignored.
    pub const PRESENTER_NOT_OBJECT: &str = "presenter-not-object";
    /// `nbformat` is newer than 4; the notebook is read as nbformat 4.
    pub const NBFORMAT_NEWER: &str = "nbformat-newer";
}

/// A parsed notebook. Build one with [`parse_notebook`].
///
/// `Deserialize` is derived for completeness, but deserializing a `Notebook` directly is
/// *strict*: one malformed output fails the whole notebook and `nbformat` is not checked.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Notebook {
    /// The cells in document order. A cell's index in this list is the `cell` of its
    /// `SourceSpan` and the number used in warning messages (0-based).
    pub cells: Vec<Cell>,
    /// Notebook metadata (`kernelspec`, `language_info`, `title`, `presenter`, …) as JSON.
    #[serde(default)]
    pub metadata: Value,
    /// Major format version; at least 4 after [`parse_notebook`].
    pub nbformat: u32,
    /// Minor format version (cell `id`s exist from 4.5).
    #[serde(default)]
    pub nbformat_minor: u32,
}

/// One notebook cell.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Cell {
    /// `markdown`, `code` or `raw`.
    pub cell_type: CellType,
    /// Cell id (nbformat ≥ 4.5).
    #[serde(default)]
    pub id: Option<String>,
    /// Cell metadata (`slideshow`, `tags`, `jupyter`, `raw_mimetype`, `presenter`, …) as JSON.
    #[serde(default)]
    pub metadata: Value,
    /// The cell source.
    #[serde(default)]
    pub source: MultilineString,
    /// Outputs (code cells only; empty otherwise). [`parse_notebook`] keeps only the outputs
    /// that deserialize.
    #[serde(default)]
    pub outputs: Vec<Output>,
    /// Execution count of a code cell; `None` when never executed.
    #[serde(default)]
    pub execution_count: Option<i64>,
    /// Markdown/raw cell attachments: file name → MIME type → base64 payload. Referenced from
    /// the source as `attachment:<file name>`.
    #[serde(default)]
    pub attachments: BTreeMap<String, BTreeMap<String, MultilineString>>,
}

/// The kind of a [`Cell`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CellType {
    /// Markdown text.
    Markdown,
    /// Source code, with outputs.
    Code,
    /// Raw content passed through by `nbconvert` (HTML when `raw_mimetype` says so).
    Raw,
    /// Any other `cell_type` string: nbformat's "unrecognized cell from a future
    /// minor-revision". The cell is kept so indices stay aligned with `SourceSpan::cell`;
    /// [`parse_notebook`] warns ([`codes::UNKNOWN_CELL_TYPE`]) and converters skip it.
    #[serde(other)]
    Unknown,
}

/// One output of a code cell, tagged by `output_type`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "output_type", rename_all = "snake_case")]
pub enum Output {
    /// Text written to `stdout` or `stderr`. Jupyter splits streams arbitrarily, so
    /// consecutive streams with the same `name` belong together.
    Stream {
        /// `"stdout"` or `"stderr"`.
        name: String,
        /// The text, possibly with ANSI escape codes.
        text: MultilineString,
    },
    /// A rich display (`display(...)`).
    DisplayData {
        /// MIME type → payload.
        data: MimeBundle,
        /// Per-MIME metadata, e.g. `{"image/png": {"width": 64}}`.
        #[serde(default)]
        metadata: Value,
    },
    /// The value of the cell's last expression.
    ExecuteResult {
        /// MIME type → payload.
        data: MimeBundle,
        /// Per-MIME metadata.
        #[serde(default)]
        metadata: Value,
        /// The `Out [n]` number.
        execution_count: Option<i64>,
    },
    /// An exception.
    Error {
        /// Exception class name, e.g. `ZeroDivisionError`.
        ename: String,
        /// Exception message.
        evalue: String,
        /// Traceback lines, with ANSI escape codes.
        traceback: Vec<String>,
    },
}

/// MIME type → payload. A payload is a string or a list of strings (text, base64), or
/// arbitrary JSON (`application/json`, `…+json`); keep it as [`Value`] and convert lazily,
/// e.g. with [`mime_text`].
pub type MimeBundle = BTreeMap<String, Value>;

/// Text that nbformat stores either as `"text"` or as `["li", "ne", "s"]`; both become one
/// `String` with the segments concatenated as-is (nothing inserted). `null` becomes `""`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MultilineString(pub String);

impl MultilineString {
    /// The text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for MultilineString {
    fn from(text: &str) -> Self {
        MultilineString(text.to_owned())
    }
}

impl<'de> Deserialize<'de> for MultilineString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(MultilineVisitor)
    }
}

/// Accepts a string, a list of strings or `null`.
struct MultilineVisitor;

impl<'de> Visitor<'de> for MultilineVisitor {
    type Value = MultilineString;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a string or a list of strings")
    }

    fn visit_str<E: de::Error>(self, text: &str) -> Result<Self::Value, E> {
        Ok(MultilineString(text.to_owned()))
    }

    fn visit_string<E: de::Error>(self, text: String) -> Result<Self::Value, E> {
        Ok(MultilineString(text))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(MultilineString::default())
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(MultilineString::default())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut text = String::new();
        while let Some(segment) = seq.next_element::<String>()? {
            text.push_str(&segment);
        }
        Ok(MultilineString(text))
    }
}

/// The text of a MIME payload: a string as is, a list of strings concatenated (nothing
/// inserted). `None` for anything else (objects, numbers, lists with non-strings).
pub fn mime_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(segments) => segments.iter().map(Value::as_str).collect(),
        _ => None,
    }
}

/// `metadata.slideshow.slide_type` of a cell (the Jupyter "Slideshow" cell toolbar).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SlideType {
    /// `"slide"`: starts a new top-level slide.
    Slide,
    /// `"subslide"`: starts a new vertical slide under the current top-level slide.
    Subslide,
    /// `"fragment"`: appended to the current slide as a fragment.
    Fragment,
    /// `"skip"`: not shown.
    Skip,
    /// `"notes"`: speaker notes of the current slide.
    Notes,
    /// `"-"`, `""` or absent: appended to the current slide.
    #[default]
    Continue,
}

impl SlideType {
    /// Parses a `slide_type` value: `"slide"`, `"subslide"`, `"fragment"`, `"skip"`,
    /// `"notes"`, or `"-"`/`""` (→ [`SlideType::Continue`]). `None` for anything else.
    ///
    /// `""` is what JupyterLab 4's property inspector writes for `"-"` (its metadata form
    /// schema lists `{"const": "", "title": "-"}`), and nbconvert treats it as a plain cell.
    pub fn parse(name: &str) -> Option<SlideType> {
        Some(match name {
            "slide" => SlideType::Slide,
            "subslide" => SlideType::Subslide,
            "fragment" => SlideType::Fragment,
            "skip" => SlideType::Skip,
            "notes" => SlideType::Notes,
            "-" | "" => SlideType::Continue,
            _ => return None,
        })
    }
}

/// `value` unless it is JSON `null` (which nbformat writers use for "absent").
fn non_null(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| !value.is_null())
}

impl Cell {
    /// `metadata.slideshow.slide_type`. Absent, `null`, `"-"`, `""`, an unknown string or a
    /// non-string → [`SlideType::Continue`] ([`parse_notebook`] warns about the unknown
    /// ones). Tags (`fragment`, `notes`) are not considered here.
    pub fn slide_type(&self) -> SlideType {
        self.metadata
            .pointer("/slideshow/slide_type")
            .and_then(Value::as_str)
            .and_then(SlideType::parse)
            .unwrap_or_default()
    }

    /// `metadata.tags` in order; non-string entries are ignored. Empty when absent.
    pub fn tags(&self) -> Vec<String> {
        self.metadata
            .get("tags")
            .and_then(Value::as_array)
            .map(|tags| {
                tags.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `metadata.jupyter.source_hidden == true` (the input was collapsed in JupyterLab).
    /// Tags such as `hide-input` are not considered here.
    pub fn is_hidden_source(&self) -> bool {
        self.metadata.pointer("/jupyter/source_hidden") == Some(&Value::Bool(true))
    }

    /// `metadata.jupyter.outputs_hidden == true` (the outputs were collapsed in JupyterLab).
    /// Tags such as `hide-output` are not considered here.
    pub fn is_hidden_outputs(&self) -> bool {
        self.metadata.pointer("/jupyter/outputs_hidden") == Some(&Value::Bool(true))
    }

    /// The MIME type of a raw cell: `metadata.raw_mimetype`, else the older
    /// `metadata.format`, else `None`.
    pub fn raw_mimetype(&self) -> Option<&str> {
        self.metadata
            .get("raw_mimetype")
            .and_then(Value::as_str)
            .or_else(|| self.metadata.get("format").and_then(Value::as_str))
    }

    /// `metadata.presenter` when it is an object (`{ "layout", "classes", "attrs" }` per
    /// spec §3). A non-object value is ignored ([`parse_notebook`] warns unless it is
    /// `null`).
    pub fn presenter_overrides(&self) -> Option<&Value> {
        self.metadata
            .get("presenter")
            .filter(|value| value.is_object())
    }

    /// The span of this whole cell when it is cell `index` of its notebook: lines 1 to the
    /// number of source lines (at least 1, so an empty cell is `1..=1`), `cell: Some(index)`.
    /// This is the span of every cell-level warning from [`parse_notebook`] and the
    /// `Slide.span` shape of spec §2.
    pub fn span(&self, index: usize) -> SourceSpan {
        SourceSpan {
            start_line: 1,
            end_line: self.source.as_str().lines().count().max(1),
            cell: Some(index),
        }
    }
}

impl Notebook {
    /// The kernel language: `metadata.language_info.name`, else
    /// `metadata.kernelspec.language`, else `"text"`. Empty strings count as absent.
    pub fn language(&self) -> &str {
        let non_empty = |pointer: &str| {
            self.metadata
                .pointer(pointer)
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
        };
        non_empty("/language_info/name")
            .or_else(|| non_empty("/kernelspec/language"))
            .unwrap_or("text")
    }

    /// `metadata.title` (nbformat 4.5 convention) when it is a string.
    pub fn title(&self) -> Option<&str> {
        self.metadata.get("title").and_then(Value::as_str)
    }

    /// `metadata.presenter` when it is an object: deck metadata with the same keys as
    /// Markdown front matter (spec §5). A non-object value is ignored ([`parse_notebook`]
    /// warns unless it is `null`).
    pub fn presenter_meta(&self) -> Option<&Value> {
        self.metadata
            .get("presenter")
            .filter(|value| value.is_object())
    }
}

/// Parses an `.ipynb` document leniently.
///
/// Returns the notebook and its warnings: [`Diagnostic`]s with `Level::Warning`, a code from
/// [`codes`], `path` set to `path`, and a message without any location prefix. Cell-level
/// warnings carry `span = cell.span(index)` (`cell: Some(index)`, 0-based, lines
/// `1..=source lines`), so they print as `talk.ipynb: cell 3, line 1: warning: …`;
/// notebook-level ones have no span. Callers append them unchanged to their
/// `Diagnostics` (`diags.extend(warnings)`).
///
/// | Code | Cause | Effect |
/// |---|---|---|
/// | [`codes::OUTPUT_SKIPPED`] | an output that does not deserialize; `outputs` not a list | output dropped / `outputs` ignored, rest of the cell kept |
/// | [`codes::ATTACHMENT_SKIPPED`] | an attachment that does not deserialize; `attachments` not an object | dropped / ignored |
/// | [`codes::UNKNOWN_SLIDE_TYPE`] | `slideshow.slide_type` not one of `slide`, `subslide`, `fragment`, `skip`, `notes`, `-`, `""` | read as [`SlideType::Continue`] |
/// | [`codes::UNKNOWN_CELL_TYPE`] | `cell_type` not `markdown`/`code`/`raw` | kept as [`CellType::Unknown`] |
/// | [`codes::PRESENTER_NOT_OBJECT`] | notebook or cell `metadata.presenter` neither an object nor `null` | ignored |
/// | [`codes::NBFORMAT_NEWER`] | `nbformat` > 4 | read as nbformat 4 |
///
/// # Errors
/// [`Error::Parse`] (with `path`) for invalid JSON (with its line), a document that is not
/// an object, `nbformat` < 4 (message exactly `"nbformat 4 required"`, spec §1), `nbformat`
/// missing or not an integer, `cells` missing or not a list, and a cell that does not
/// deserialize (not an object, no `cell_type`, wrongly typed `source`, …; the message
/// starts with `cell <index>: `).
pub fn parse_notebook(path: &Path, json: &str) -> crate::error::Result<(Notebook, Diagnostics)> {
    let parse_error = |line: Option<usize>, message: String| Error::Parse {
        path: path.to_path_buf(),
        line,
        message,
    };
    let value: Value = serde_json::from_str(json)
        .map_err(|err| parse_error(Some(err.line()), format!("invalid notebook JSON: {err}")))?;
    let Value::Object(mut top) = value else {
        return Err(parse_error(
            None,
            "not a Jupyter notebook: expected a JSON object".to_owned(),
        ));
    };
    let mut warnings = Diagnostics::new();
    let newer = check_nbformat(&top).map_err(|message| parse_error(None, message))?;
    if let Some(message) = newer {
        warnings.push(Diagnostic::warning(codes::NBFORMAT_NEWER, message).with_path(path));
    }
    let raw_cells = take_cells(&mut top).map_err(|message| parse_error(None, message))?;
    let mut notebook: Notebook = serde_json::from_value(Value::Object(top))
        .map_err(|err| parse_error(None, format!("invalid notebook: {err}")))?;
    if non_null(notebook.metadata.get("presenter")).is_some() && notebook.presenter_meta().is_none()
    {
        warnings.push(
            Diagnostic::warning(codes::PRESENTER_NOT_OBJECT, PRESENTER_NOT_OBJECT_MESSAGE)
                .with_path(path),
        );
    }
    let mut cells = Vec::with_capacity(raw_cells.len());
    for (index, raw) in raw_cells.into_iter().enumerate() {
        let (cell, findings) =
            parse_cell(raw).map_err(|err| parse_error(None, format!("cell {index}: {err}")))?;
        let span = cell.span(index);
        warnings.extend(findings.into_iter().map(|(code, message)| {
            Diagnostic::warning(code, message)
                .with_path(path)
                .with_span(span.clone())
        }));
        cells.push(cell);
    }
    notebook.cells = cells;
    Ok((notebook, warnings))
}

/// A cell-level warning before it gets a path and span: (code from [`codes`], message).
type Finding = (&'static str, String);

const PRESENTER_NOT_OBJECT_MESSAGE: &str = "metadata.presenter is not an object, ignored";

/// Requires `nbformat` ≥ 4; returns a warning message for a newer major version.
fn check_nbformat(top: &Map<String, Value>) -> Result<Option<String>, String> {
    match top.get("nbformat").and_then(Value::as_u64) {
        Some(4) => Ok(None),
        Some(major) if major > 4 => Ok(Some(format!(
            "nbformat {major} is newer than this tool knows; reading it as nbformat 4"
        ))),
        Some(major) => {
            tracing::debug!(
                nbformat = major,
                "notebook too old; re-save it with Jupyter"
            );
            Err("nbformat 4 required".to_owned())
        }
        None => Err("not a Jupyter notebook: `nbformat` is missing or not an integer".to_owned()),
    }
}

/// Moves the raw `cells` out of the top-level object (leaving an empty list behind so the
/// rest still deserializes as a [`Notebook`]).
fn take_cells(top: &mut Map<String, Value>) -> Result<Vec<Value>, String> {
    match top.insert("cells".to_owned(), Value::Array(Vec::new())) {
        Some(Value::Array(cells)) => Ok(cells),
        Some(_) => Err("`cells` is not a list".to_owned()),
        None => Err("not a Jupyter notebook: `cells` is missing".to_owned()),
    }
}

/// Deserializes one cell; outputs and attachments leniently (see [`parse_outputs`]).
fn parse_cell(mut raw: Value) -> Result<(Cell, Vec<Finding>), serde_json::Error> {
    let cell_type = raw.get("cell_type").cloned();
    let (outputs, attachments) = match raw.as_object_mut() {
        Some(object) => (object.remove("outputs"), object.remove("attachments")),
        None => (None, None),
    };
    let mut cell: Cell = serde_json::from_value(raw)?;
    let mut findings = Vec::new();
    if cell.cell_type == CellType::Unknown {
        let name = cell_type.map(|name| name.to_string()).unwrap_or_default();
        findings.push((
            codes::UNKNOWN_CELL_TYPE,
            format!("unknown cell_type {name}, cell ignored"),
        ));
    }
    cell.outputs = lenient_outputs(outputs, &mut findings);
    cell.attachments = lenient_attachments(attachments, &mut findings);
    check_cell_metadata(&cell, &mut findings);
    Ok((cell, findings))
}

/// Deserializes each raw output on its own, keeping those that succeed in order and
/// describing each failure (`"output <i> (<output_type>) skipped: <reason>"`, `i` 0-based
/// within the list). One odd output never affects the others.
pub fn parse_outputs(raw: Vec<Value>) -> (Vec<Output>, Vec<String>) {
    let mut outputs = Vec::with_capacity(raw.len());
    let mut warnings = Vec::new();
    for (index, value) in raw.into_iter().enumerate() {
        let kind = match value.get("output_type") {
            Some(Value::String(kind)) => kind.clone(),
            Some(other) => other.to_string(),
            None => "no output_type".to_owned(),
        };
        match serde_json::from_value::<Output>(value) {
            Ok(output) => outputs.push(output),
            Err(err) => warnings.push(format!("output {index} ({kind}) skipped: {err}")),
        }
    }
    (outputs, warnings)
}

/// `outputs` as a list of [`Output`]s; absent or `null` → empty.
fn lenient_outputs(raw: Option<Value>, findings: &mut Vec<Finding>) -> Vec<Output> {
    match raw {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            let (outputs, warnings) = parse_outputs(items);
            findings.extend(
                warnings
                    .into_iter()
                    .map(|message| (codes::OUTPUT_SKIPPED, message)),
            );
            outputs
        }
        Some(_) => {
            findings.push((
                codes::OUTPUT_SKIPPED,
                "`outputs` is not a list, ignored".to_owned(),
            ));
            Vec::new()
        }
    }
}

/// `attachments` per file name; a file whose bundle does not deserialize is dropped.
fn lenient_attachments(
    raw: Option<Value>,
    findings: &mut Vec<Finding>,
) -> BTreeMap<String, BTreeMap<String, MultilineString>> {
    let mut attachments = BTreeMap::new();
    match raw {
        None | Some(Value::Null) => {}
        Some(Value::Object(files)) => {
            for (name, bundle) in files {
                match serde_json::from_value(bundle) {
                    Ok(bundle) => {
                        attachments.insert(name, bundle);
                    }
                    Err(err) => findings.push((
                        codes::ATTACHMENT_SKIPPED,
                        format!("attachment '{name}' skipped: {err}"),
                    )),
                }
            }
        }
        Some(_) => findings.push((
            codes::ATTACHMENT_SKIPPED,
            "`attachments` is not an object, ignored".to_owned(),
        )),
    }
    attachments
}

/// Warns about metadata the accessors silently replace by a default (`null` is absent).
fn check_cell_metadata(cell: &Cell, findings: &mut Vec<Finding>) {
    if let Some(slide_type) = non_null(cell.metadata.pointer("/slideshow/slide_type"))
        && slide_type.as_str().and_then(SlideType::parse).is_none()
    {
        findings.push((
            codes::UNKNOWN_SLIDE_TYPE,
            format!("unknown slideshow.slide_type {slide_type}, treated as \"-\""),
        ));
    }
    if non_null(cell.metadata.get("presenter")).is_some() && cell.presenter_overrides().is_none() {
        findings.push((
            codes::PRESENTER_NOT_OBJECT,
            PRESENTER_NOT_OBJECT_MESSAGE.to_owned(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn multiline(value: Value) -> MultilineString {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn multiline_string_accepts_string_list_and_null() {
        assert_eq!(multiline(json!("a\nb")), MultilineString::from("a\nb"));
        assert_eq!(
            multiline(json!(["a\n", "b"])),
            MultilineString::from("a\nb")
        );
        assert_eq!(multiline(json!(["a", "", "b"])).as_str(), "ab");
        assert_eq!(multiline(json!([])), MultilineString::default());
        assert_eq!(multiline(json!(null)), MultilineString::default());
        assert!(serde_json::from_value::<MultilineString>(json!(3)).is_err());
        assert!(serde_json::from_value::<MultilineString>(json!(["a", 1])).is_err());
    }

    #[test]
    fn mime_text_joins_strings_only() {
        assert_eq!(mime_text(&json!("x")).as_deref(), Some("x"));
        assert_eq!(
            mime_text(&json!(["<b>", "x</b>\n"])).as_deref(),
            Some("<b>x</b>\n")
        );
        assert_eq!(mime_text(&json!([])).as_deref(), Some(""));
        assert_eq!(mime_text(&json!({"a": 1})), None);
        assert_eq!(mime_text(&json!(["a", 1])), None);
        assert_eq!(mime_text(&json!(1)), None);
    }

    #[test]
    fn slide_type_names() {
        let cases = [
            ("slide", SlideType::Slide),
            ("subslide", SlideType::Subslide),
            ("fragment", SlideType::Fragment),
            ("skip", SlideType::Skip),
            ("notes", SlideType::Notes),
            ("-", SlideType::Continue),
            ("", SlideType::Continue), // JupyterLab 4's value for "-"
        ];
        for (name, expected) in cases {
            assert_eq!(SlideType::parse(name), Some(expected), "{name}");
        }
        assert_eq!(SlideType::parse("Slide"), None);
        assert_eq!(SlideType::parse(" "), None);
        assert_eq!(SlideType::default(), SlideType::Continue);
    }

    fn parse_cells(cells: Value) -> (Notebook, Diagnostics) {
        let notebook =
            json!({ "cells": cells, "metadata": {}, "nbformat": 4, "nbformat_minor": 5 });
        parse_notebook(Path::new("nb.ipynb"), &notebook.to_string()).unwrap()
    }

    #[test]
    fn jupyterlab_4_empty_slide_type_is_continue_without_warning() {
        // What JupyterLab 4's property inspector saves after choosing "-".
        let metadata = json!({ "editable": true, "slideshow": { "slide_type": "" }, "tags": [] });
        let (notebook, warnings) = parse_cells(json!([
            { "cell_type": "markdown", "source": "a", "metadata": metadata },
            { "cell_type": "code", "source": "b", "metadata": metadata, "outputs": [] }
        ]));
        assert!(warnings.is_empty(), "{warnings:?}");
        let types: Vec<SlideType> = notebook.cells.iter().map(Cell::slide_type).collect();
        assert_eq!(types, [SlideType::Continue, SlideType::Continue]);
    }

    #[test]
    fn null_metadata_values_count_as_absent() {
        let source = json!({
            "cells": [ { "cell_type": "markdown", "source": "",
                         "metadata": { "presenter": null, "slideshow": { "slide_type": null } } } ],
            "metadata": { "presenter": null },
            "nbformat": 4
        });
        let (notebook, warnings) =
            parse_notebook(Path::new("nb.ipynb"), &source.to_string()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(notebook.presenter_meta(), None);
        assert_eq!(notebook.cells[0].presenter_overrides(), None);
        assert_eq!(notebook.cells[0].slide_type(), SlideType::Continue);
    }

    #[test]
    fn cell_type_other_is_unknown() {
        let cell_type =
            |name: &str| serde_json::from_value::<CellType>(json!(name)).map_err(|e| e.to_string());
        assert_eq!(cell_type("raw"), Ok(CellType::Raw));
        assert_eq!(cell_type("chart"), Ok(CellType::Unknown));
        assert_eq!(cell_type("Markdown"), Ok(CellType::Unknown));
        assert!(serde_json::from_value::<CellType>(json!(1)).is_err());
        assert!(serde_json::from_value::<CellType>(json!(null)).is_err());
    }

    #[test]
    fn cell_span_covers_the_source_lines() {
        let (notebook, _) = parse_cells(json!([
            { "cell_type": "markdown", "source": "" },
            { "cell_type": "markdown", "source": ["a\n", "b\n", "c"] },
            { "cell_type": "markdown", "source": "a\nb\n" }
        ]));
        let spans: Vec<(usize, usize, Option<usize>)> = notebook
            .cells
            .iter()
            .enumerate()
            .map(|(index, cell)| {
                let span = cell.span(index);
                (span.start_line, span.end_line, span.cell)
            })
            .collect();
        assert_eq!(spans, [(1, 1, Some(0)), (1, 3, Some(1)), (1, 2, Some(2))]);
    }

    #[test]
    fn output_variants_deserialize_with_multiline_text() {
        let stream: Output = serde_json::from_value(
            json!({"output_type": "stream", "name": "stdout", "text": ["a\n", "b"]}),
        )
        .unwrap();
        assert_eq!(
            stream,
            Output::Stream {
                name: "stdout".into(),
                text: "a\nb".into()
            }
        );
        let result: Output = serde_json::from_value(
            json!({"output_type": "execute_result", "data": {"text/plain": "1"}, "metadata": {}, "execution_count": null}),
        )
        .unwrap();
        assert!(matches!(
            result,
            Output::ExecuteResult {
                execution_count: None,
                ..
            }
        ));
    }

    #[test]
    fn parse_outputs_reports_index_and_kind() {
        let (outputs, warnings) = parse_outputs(vec![
            json!({"output_type": "weird"}),
            json!({"output_type": "stream", "name": "stdout"}),
            json!({"name": "stdout", "text": "x"}),
            json!({"output_type": "stream", "name": "stdout", "text": "ok"}),
        ]);
        assert_eq!(outputs.len(), 1);
        assert_eq!(warnings.len(), 3);
        assert!(warnings[0].starts_with("output 0 (weird) skipped: unknown variant `weird`"));
        assert!(warnings[1].starts_with("output 1 (stream) skipped: missing field `text`"));
        assert!(warnings[2].starts_with("output 2 (no output_type) skipped"));
    }
}
