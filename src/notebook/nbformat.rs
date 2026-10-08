//! nbformat 4 data model with lenient deserialization.
//!
//! Contract: `docs/spec/notebook-mapping.md` §1. The entry point is [`parse_notebook`]: it
//! rejects anything older than nbformat 4, deserializes every cell, and **drops** outputs
//! and attachments that do not deserialize (an unknown `output_type`, a stream without
//! `text`, …), reporting each as a warning message instead of failing the deck. Other
//! recoverable oddities (an unknown `slideshow.slide_type`, a `metadata.presenter` that is
//! not an object) are reported the same way. Unknown keys anywhere are ignored.
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

use crate::error::Error;

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
    /// `"-"` or absent: appended to the current slide.
    #[default]
    Continue,
}

impl SlideType {
    /// Parses a `slide_type` value: `"slide"`, `"subslide"`, `"fragment"`, `"skip"`,
    /// `"notes"`, or `"-"` (→ [`SlideType::Continue`]). `None` for anything else.
    pub fn parse(name: &str) -> Option<SlideType> {
        Some(match name {
            "slide" => SlideType::Slide,
            "subslide" => SlideType::Subslide,
            "fragment" => SlideType::Fragment,
            "skip" => SlideType::Skip,
            "notes" => SlideType::Notes,
            "-" => SlideType::Continue,
            _ => return None,
        })
    }
}

impl Cell {
    /// `metadata.slideshow.slide_type`. Absent, `"-"`, an unknown string or a non-string
    /// → [`SlideType::Continue`] ([`parse_notebook`] warns about the unknown ones). Tags
    /// (`fragment`, `notes`) are not considered here.
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
    /// spec §3). A non-object value is ignored ([`parse_notebook`] warns).
    pub fn presenter_overrides(&self) -> Option<&Value> {
        self.metadata
            .get("presenter")
            .filter(|value| value.is_object())
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
    /// warns).
    pub fn presenter_meta(&self) -> Option<&Value> {
        self.metadata
            .get("presenter")
            .filter(|value| value.is_object())
    }
}

/// Parses an `.ipynb` document leniently. `path` is used only in error messages.
///
/// Returns the notebook and the warnings (human-readable, cell-level ones prefixed with
/// `cell <index>: `, index 0-based) that the caller should turn into `Level::Warning`
/// diagnostics. Warnings are produced for: outputs and attachments that do not deserialize
/// (they are dropped; the rest of the cell is kept), an `outputs`/`attachments` value of
/// the wrong shape (ignored), an unknown or non-string `slideshow.slide_type` (read as
/// [`SlideType::Continue`]), a `metadata.presenter` that is not an object (ignored), and
/// `nbformat` > 4 (read as 4).
///
/// # Errors
/// [`Error::Parse`] for invalid JSON (with its line), a document that is not an object,
/// `nbformat` missing or < 4 (`"nbformat 4 required …"`), `cells` missing or not a list,
/// and a cell that does not deserialize (e.g. an unknown `cell_type`).
pub fn parse_notebook(path: &Path, json: &str) -> crate::error::Result<(Notebook, Vec<String>)> {
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
    let mut warnings = check_nbformat(&top).map_err(|message| parse_error(None, message))?;
    let raw_cells = take_cells(&mut top).map_err(|message| parse_error(None, message))?;
    let mut notebook: Notebook = serde_json::from_value(Value::Object(top))
        .map_err(|err| parse_error(None, format!("invalid notebook: {err}")))?;
    if notebook.metadata.get("presenter").is_some() && notebook.presenter_meta().is_none() {
        warnings.push("metadata.presenter is not an object, ignored".to_owned());
    }
    notebook.cells = raw_cells
        .into_iter()
        .enumerate()
        .map(|(index, raw)| parse_cell(index, raw, &mut warnings))
        .collect::<Result<_, _>>()
        .map_err(|message| parse_error(None, message))?;
    Ok((notebook, warnings))
}

/// Requires `nbformat` ≥ 4; returns a warning for a newer major version.
fn check_nbformat(top: &Map<String, Value>) -> Result<Vec<String>, String> {
    match top.get("nbformat").and_then(Value::as_u64) {
        Some(4) => Ok(Vec::new()),
        Some(major) if major > 4 => Ok(vec![format!(
            "nbformat {major} is newer than this tool knows; reading it as nbformat 4"
        )]),
        Some(major) => Err(format!(
            "nbformat 4 required, found nbformat {major} (re-save the notebook with Jupyter to upgrade it)"
        )),
        None => Err(
            "nbformat 4 required: `nbformat` is missing or not an integer (is this a Jupyter notebook?)"
                .to_owned(),
        ),
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
fn parse_cell(index: usize, mut raw: Value, warnings: &mut Vec<String>) -> Result<Cell, String> {
    let (outputs, attachments) = match raw.as_object_mut() {
        Some(object) => (object.remove("outputs"), object.remove("attachments")),
        None => (None, None),
    };
    let mut cell: Cell =
        serde_json::from_value(raw).map_err(|err| format!("cell {index}: {err}"))?;
    let mut cell_warnings = Vec::new();
    cell.outputs = lenient_outputs(outputs, &mut cell_warnings);
    cell.attachments = lenient_attachments(attachments, &mut cell_warnings);
    check_cell_metadata(&cell, &mut cell_warnings);
    warnings.extend(
        cell_warnings
            .into_iter()
            .map(|message| format!("cell {index}: {message}")),
    );
    Ok(cell)
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
fn lenient_outputs(raw: Option<Value>, warnings: &mut Vec<String>) -> Vec<Output> {
    match raw {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            let (outputs, output_warnings) = parse_outputs(items);
            warnings.extend(output_warnings);
            outputs
        }
        Some(_) => {
            warnings.push("`outputs` is not a list, ignored".to_owned());
            Vec::new()
        }
    }
}

/// `attachments` per file name; a file whose bundle does not deserialize is dropped.
fn lenient_attachments(
    raw: Option<Value>,
    warnings: &mut Vec<String>,
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
                    Err(err) => warnings.push(format!("attachment '{name}' skipped: {err}")),
                }
            }
        }
        Some(_) => warnings.push("`attachments` is not an object, ignored".to_owned()),
    }
    attachments
}

/// Warns about metadata the accessors silently replace by a default.
fn check_cell_metadata(cell: &Cell, warnings: &mut Vec<String>) {
    if let Some(slide_type) = cell.metadata.pointer("/slideshow/slide_type")
        && slide_type.as_str().and_then(SlideType::parse).is_none()
    {
        warnings.push(format!(
            "unknown slideshow.slide_type {slide_type}, treated as \"-\""
        ));
    }
    if cell.metadata.get("presenter").is_some() && cell.presenter_overrides().is_none() {
        warnings.push("metadata.presenter is not an object, ignored".to_owned());
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
        ];
        for (name, expected) in cases {
            assert_eq!(SlideType::parse(name), Some(expected), "{name}");
        }
        assert_eq!(SlideType::parse("Slide"), None);
        assert_eq!(SlideType::parse(""), None);
        assert_eq!(SlideType::default(), SlideType::Continue);
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
