//! Output rendering: each Jupyter output becomes one [`CellOutput`] with ready-to-insert HTML.
//!
//! Contract: `docs/spec/notebook-mapping.md` §4 and `docs/spec/ir.md` (`CellOutput`). The entry
//! point is [`render_outputs`], called by the converter (`notebook::convert`, T31) once per
//! code cell.
//!
//! - **Streams**: consecutive `stream` outputs with the same `name` are merged (Jupyter splits
//!   them arbitrarily). Like nbconvert's `CoalesceStreamsPreprocessor`, a line's text up to its
//!   last `\r` that is followed by more text is dropped (progress bars), then [`ansi_to_html`]
//!   escapes the text and turns ANSI colors into `<span>`s.
//! - **Errors**: the traceback lines joined with `\n`, through [`ansi_to_html`].
//! - **Rich outputs** (`display_data`, `execute_result`): [`choose_mime`] picks the first MIME
//!   type of `[notebook] mime_priority` present in the bundle and [`render_display`] renders it
//!   per the §4 table; `text/markdown` goes through the caller's Markdown closure instead.
//!   When the chosen payload cannot be rendered (invalid base64, a non-text payload, a MIME
//!   type without a renderer), a [`codes::OUTPUT_INVALID`] warning is recorded and the next
//!   MIME type of the priority list present in the bundle is tried, so a broken PNG falls back
//!   to its `text/plain` repr exactly as a bundle without the PNG would. When nothing renders,
//!   the output is [`CellOutput::Unsupported`] with every MIME type of the bundle and a
//!   [`codes::OUTPUT_UNSUPPORTED`] warning.
//!
//! Diagnostics are pushed without `path` or `span` (this module does not know them); their
//! messages start with `output <i>: ` (`i` is the 0-based index in the `outputs` slice). The
//! caller attaches the notebook path and the cell span.

use std::borrow::Cow;
use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_PAD_INDIFFERENT;
use regex::Regex;
use serde_json::Value;

use crate::assets::registry::AssetRegistry;
use crate::config::NotebookConfig;
use crate::error::{Diagnostics, Error, Result};
use crate::ir::{CellOutput, Features};
use crate::notebook::nbformat::{MimeBundle, Output, mime_text};

/// The diagnostic codes [`render_outputs`] records (all `Level::Warning`).
pub mod codes {
    /// A `text/html` output contains `<script`; it is kept verbatim and runs when the deck
    /// loads.
    pub const OUTPUT_SCRIPT: &str = "output-script";
    /// No MIME type of the bundle could be rendered (e.g. a widget view without a
    /// `text/plain` fallback); the output became `CellOutput::Unsupported`.
    pub const OUTPUT_UNSUPPORTED: &str = "output-unsupported";
    /// One MIME type of a bundle could not be rendered (invalid base64, a payload that is not
    /// text, no renderer for a type listed in `mime_priority`); the next type was tried.
    pub const OUTPUT_INVALID: &str = "output-invalid";
}

/// Renders the outputs of one code cell (spec §4).
///
/// `md` renders Markdown to HTML (T12's renderer in notebook mode); it is called for each
/// output whose chosen MIME type is `text/markdown`, and its errors are returned unchanged.
/// Images are registered in `assets` and referenced as `asset:<id>`. Warnings (codes in
/// [`codes`]) are appended to `diags` without path or span, see the module docs.
///
/// The returned [`Features`] only ever sets `math`: when a `text/latex` output was rendered
/// or any `Display` HTML contains `data-math-style` (e.g. Markdown with `$x$`). The caller
/// sets `notebook` and `code`.
///
/// # Errors
/// Only the errors of `md`. Malformed payloads are warnings, never errors.
pub fn render_outputs(
    outputs: &[Output],
    cfg: &NotebookConfig,
    assets: &mut AssetRegistry,
    diags: &mut Diagnostics,
    md: &mut dyn FnMut(&str) -> Result<String>,
) -> Result<(Vec<CellOutput>, Features)> {
    let mut renderer = Renderer {
        priority: &cfg.mime_priority,
        assets,
        diags,
        md,
        features: Features::default(),
    };
    let mut rendered = Vec::with_capacity(outputs.len());
    // The stream being merged: (name, text so far).
    let mut stream: Option<(&str, String)> = None;
    for (index, output) in outputs.iter().enumerate() {
        let (data, metadata, execution_count) = match output {
            Output::Stream { name, text } => {
                match &mut stream {
                    Some((open, buffer)) if *open == name.as_str() => {
                        buffer.push_str(text.as_str());
                    }
                    _ => {
                        flush_stream(&mut stream, &mut rendered);
                        stream = Some((name, text.as_str().to_owned()));
                    }
                }
                continue;
            }
            Output::Error {
                ename,
                evalue,
                traceback,
            } => {
                flush_stream(&mut stream, &mut rendered);
                rendered.push(CellOutput::Error {
                    ename: ename.clone(),
                    evalue: evalue.clone(),
                    html: ansi_to_html(&traceback.join("\n")),
                });
                continue;
            }
            Output::DisplayData { data, metadata } => (data, metadata, None),
            Output::ExecuteResult {
                data,
                metadata,
                execution_count,
            } => (data, metadata, *execution_count),
        };
        flush_stream(&mut stream, &mut rendered);
        rendered.push(renderer.bundle(index, data, metadata, execution_count)?);
    }
    flush_stream(&mut stream, &mut rendered);
    Ok((rendered, renderer.features))
}

/// Pushes the merged stream, if any, as a [`CellOutput::Stream`].
fn flush_stream(stream: &mut Option<(&str, String)>, rendered: &mut Vec<CellOutput>) {
    if let Some((name, text)) = stream.take() {
        rendered.push(CellOutput::Stream {
            name: name.to_owned(),
            html: ansi_to_html(&apply_carriage_returns(&text)),
        });
    }
}

/// Everything rendering a MIME bundle needs, so the helpers stay small.
struct Renderer<'a> {
    priority: &'a [String],
    assets: &'a mut AssetRegistry,
    diags: &'a mut Diagnostics,
    md: &'a mut dyn FnMut(&str) -> Result<String>,
    features: Features,
}

impl Renderer<'_> {
    /// Output `index` of the cell, a `display_data` (`execution_count` `None`) or an
    /// `execute_result`.
    fn bundle(
        &mut self,
        index: usize,
        data: &MimeBundle,
        metadata: &Value,
        execution_count: Option<i64>,
    ) -> Result<CellOutput> {
        for (mime, payload) in candidates(data, self.priority) {
            let Some((html, sets_math)) = self.try_mime(index, mime, payload, metadata)? else {
                continue;
            };
            if mime == "text/html" && html.to_ascii_lowercase().contains("<script") {
                self.diags.warn(
                    codes::OUTPUT_SCRIPT,
                    format!(
                        "output {index}: text/html contains <script>; \
                         it is kept and runs when the slides load"
                    ),
                );
            }
            self.features.math |= sets_math || html.contains("data-math-style");
            return Ok(CellOutput::Display {
                mime: mime.to_owned(),
                html,
                execution_count,
            });
        }
        let mimes: Vec<String> = data.keys().cloned().collect();
        let found = if mimes.is_empty() {
            "none".to_owned()
        } else {
            mimes.join(", ")
        };
        self.diags.warn(
            codes::OUTPUT_UNSUPPORTED,
            format!(
                "output {index}: no renderable MIME type (found: {found}); \
                 [notebook] mime_priority decides what is rendered"
            ),
        );
        Ok(CellOutput::Unsupported { mimes })
    }

    /// Renders one MIME type; `None` (with an [`codes::OUTPUT_INVALID`] warning) when its
    /// payload cannot be rendered. Errors are only those of the Markdown closure.
    fn try_mime(
        &mut self,
        index: usize,
        mime: &str,
        payload: &Value,
        metadata: &Value,
    ) -> Result<Option<(String, bool)>> {
        let attempt = if mime == "text/markdown" {
            match payload_text(payload) {
                Ok(text) => Ok(((self.md)(&text)?, false)),
                Err(err) => Err(err),
            }
        } else {
            render_display(mime, payload, metadata, self.assets)
        };
        match attempt {
            Ok(rendered) => Ok(Some(rendered)),
            Err(err) => {
                let reason = match err {
                    Error::Other(message) => message,
                    other => other.to_string(),
                };
                self.diags.warn(
                    codes::OUTPUT_INVALID,
                    format!("output {index}: {mime} not rendered: {reason}"),
                );
                Ok(None)
            }
        }
    }
}

/// The MIME types of `priority` present in `bundle`, in priority order, with their payloads.
fn candidates<'a>(
    bundle: &'a MimeBundle,
    priority: &[String],
) -> impl Iterator<Item = (&'a str, &'a Value)> {
    priority
        .iter()
        .filter_map(|mime| bundle.get_key_value(mime.as_str()))
        .map(|(mime, payload)| (mime.as_str(), payload))
}

/// The first entry of `priority` present in `bundle`, or `None` when no entry is (spec §4).
pub fn choose_mime<'a>(bundle: &'a MimeBundle, priority: &[String]) -> Option<&'a str> {
    candidates(bundle, priority).next().map(|(mime, _)| mime)
}

/// Renders one MIME payload per the spec §4 table; the `bool` is true when the result needs
/// KaTeX (`text/latex`), i.e. sets `Features.math`.
///
/// `metadata` is the output's whole `metadata` object. Image and SVG sizes come from
/// `metadata[mime].width|height`, falling back to `metadata.width|height` (nbconvert's
/// `get_metadata`), and are used only when numeric and non-negative.
///
/// | MIME | HTML |
/// |---|---|
/// | `text/html` | the payload verbatim |
/// | `image/svg+xml` | `<figure class="output-image">` + the payload, with `width`/`height` set on its root `<svg>` start tag when known |
/// | `image/png`, `image/jpeg`, `image/gif`, `image/webp` | base64-decoded (ASCII whitespace ignored, padding optional), registered as `output.<ext>`: `<img class="output-image" src="asset:<id>" alt="">` + `width`/`height` |
/// | `text/latex` | one surrounding `$$…$$`, `$…$`, `\[…\]` or `\(…\)` stripped, trimmed: `<div data-math-style="display">` + escaped TeX |
/// | `application/json` | `<pre class="output-json">` + pretty-printed, escaped JSON |
/// | `text/plain` | `<pre class="output-text">` + [`ansi_to_html`] of the text (escaped; ANSI colors kept, as nbconvert does) |
///
/// `text/markdown` needs the Markdown renderer, which only [`render_outputs`] has.
///
/// # Errors
/// [`Error::Other`] for a payload that is not text (a string or list of strings) where text
/// is required, invalid or empty base64, `text/markdown`, and any MIME type not in the table.
pub fn render_display(
    mime: &str,
    payload: &Value,
    metadata: &Value,
    assets: &mut AssetRegistry,
) -> Result<(String, bool)> {
    let html = match mime {
        "text/html" => payload_text(payload)?,
        "image/svg+xml" => svg_html(&payload_text(payload)?, metadata, mime),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" => {
            image_html(mime, &payload_text(payload)?, metadata, assets)?
        }
        "text/latex" => {
            let tex = strip_math_delimiters(&payload_text(payload)?).to_owned();
            let html = format!(
                "<div data-math-style=\"display\">{}</div>",
                html_escape::encode_text(&tex)
            );
            return Ok((html, true));
        }
        "application/json" => format!(
            "<pre class=\"output-json\">{}</pre>",
            html_escape::encode_text(&serde_json::to_string_pretty(payload)?)
        ),
        "text/plain" => format!(
            "<pre class=\"output-text\">{}</pre>",
            ansi_to_html(&payload_text(payload)?)
        ),
        "text/markdown" => {
            return Err(Error::Other(
                "text/markdown needs the Markdown renderer (render_outputs' `md`)".to_owned(),
            ));
        }
        _ => return Err(Error::Other(format!("no renderer for {mime}"))),
    };
    Ok((html, false))
}

/// The text of a payload (a string, or a list of strings joined as-is).
fn payload_text(payload: &Value) -> Result<String> {
    mime_text(payload)
        .ok_or_else(|| Error::Other("payload is not a string or a list of strings".to_owned()))
}

/// Decodes a base64 image, registers it and returns its `<img>` tag.
fn image_html(
    mime: &str,
    base64: &str,
    metadata: &Value,
    assets: &mut AssetRegistry,
) -> Result<String> {
    // Jupyter wraps base64 payloads in lines (a trailing "\n", or a list of lines).
    let compact: Vec<u8> = base64
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if compact.is_empty() {
        return Err(Error::Other("empty base64 payload".to_owned()));
    }
    let bytes = STANDARD_PAD_INDIFFERENT
        .decode(&compact)
        .map_err(|err| Error::Other(format!("invalid base64: {err}")))?;
    let ext = match mime {
        "image/jpeg" => "jpg",
        other => other.trim_start_matches("image/"),
    };
    let id = assets.register_bytes(bytes, mime, &format!("output.{ext}"));
    let mut html = format!(
        "<img class=\"output-image\" src=\"{}\" alt=\"\"",
        AssetRegistry::placeholder(&id)
    );
    for key in ["width", "height"] {
        if let Some(value) = dimension(metadata, mime, key) {
            html.push_str(&format!(" {key}=\"{value}\""));
        }
    }
    html.push('>');
    Ok(html)
}

/// `metadata[mime][key]`, else `metadata[key]`, as attribute text when it is a non-negative
/// number (`64`, `64.5`); `None` otherwise.
fn dimension(metadata: &Value, mime: &str, key: &str) -> Option<String> {
    let value = metadata
        .get(mime)
        .and_then(|per_mime| per_mime.get(key))
        .filter(|value| !value.is_null())
        .or_else(|| metadata.get(key))?;
    if let Some(integer) = value.as_u64() {
        return Some(integer.to_string());
    }
    let number = value.as_f64()?;
    // `Display` for f64 prints integral values without a fraction ("64", not "64.0").
    (number.is_finite() && number >= 0.0).then(|| number.to_string())
}

/// The SVG wrapped in `<figure class="output-image">`, with metadata sizes on the root tag.
fn svg_html(svg: &str, metadata: &Value, mime: &str) -> String {
    let sizes: Vec<(&str, String)> = ["width", "height"]
        .into_iter()
        .filter_map(|key| dimension(metadata, mime, key).map(|value| (key, value)))
        .collect();
    let svg = if sizes.is_empty() {
        Cow::Borrowed(svg)
    } else {
        set_root_svg_attrs(svg, &sizes)
    };
    format!("<figure class=\"output-image\">{svg}</figure>")
}

/// The first `<svg` start tag (the root; an XML declaration, doctype or comments may precede
/// it), parsed quote-aware: group `attrs` is its attribute list, the rest is `\s*/?>`.
static SVG_ROOT_TAG: LazyLock<Regex> = LazyLock::new(|| {
    // A constant pattern covered by the unit tests: compiling it cannot fail.
    Regex::new(concat!(
        r#"\A<svg(?P<attrs>(?:\s+[^\s=/>"']+(?:\s*=\s*(?:"[^"]*"|'[^']*'|[^\s"'=<>`]+))?)*)"#,
        r"\s*/?>",
    ))
    .expect("SVG root tag regex is valid")
});

/// One attribute inside [`SVG_ROOT_TAG`]'s `attrs`, with its leading whitespace.
static SVG_ATTR: LazyLock<Regex> = LazyLock::new(|| {
    // A constant pattern covered by the unit tests: compiling it cannot fail.
    Regex::new(r#"\s+(?P<name>[^\s=/>"']+)(?:\s*=\s*(?:"[^"]*"|'[^']*'|[^\s"'=<>`]+))?"#)
        .expect("SVG attribute regex is valid")
});

/// Where the root `<svg` start tag begins: `<svg` followed by whitespace, `/` or `>`.
static SVG_OPEN: LazyLock<Regex> = LazyLock::new(|| {
    // A constant pattern covered by the unit tests: compiling it cannot fail.
    Regex::new(r"<svg[\s/>]").expect("SVG open regex is valid")
});

/// Sets (replaces or appends) `attrs` on the root `<svg>` start tag. Best effort: when the
/// root tag is not found or not well-formed, the SVG is returned unchanged.
fn set_root_svg_attrs<'a>(svg: &'a str, attrs: &[(&str, String)]) -> Cow<'a, str> {
    let Some(open) = SVG_OPEN.find(svg) else {
        return Cow::Borrowed(svg);
    };
    let tail = &svg[open.start()..];
    let Some(tag) = SVG_ROOT_TAG.captures(tail) else {
        return Cow::Borrowed(svg);
    };
    let (Some(whole), Some(list)) = (tag.get(0), tag.name("attrs")) else {
        return Cow::Borrowed(svg);
    };
    let mut pending: Vec<&(&str, String)> = attrs.iter().collect();
    let mut rebuilt = String::with_capacity(whole.len() + 32);
    rebuilt.push_str("<svg");
    for attr in SVG_ATTR.captures_iter(list.as_str()) {
        let name = attr.name("name").map_or("", |name| name.as_str());
        match pending.iter().position(|(key, _)| *key == name) {
            Some(at) => {
                let (key, value) = pending.remove(at);
                rebuilt.push_str(&format!(" {key}=\"{value}\""));
            }
            None => rebuilt.push_str(attr.get(0).map_or("", |all| all.as_str())),
        }
    }
    for (key, value) in pending {
        rebuilt.push_str(&format!(" {key}=\"{value}\""));
    }
    rebuilt.push_str(&whole.as_str()[list.end()..]);
    let mut out = String::with_capacity(svg.len() + 32);
    out.push_str(&svg[..open.start()]);
    out.push_str(&rebuilt);
    out.push_str(&tail[whole.end()..]);
    Cow::Owned(out)
}

/// Trims `tex` and strips one surrounding `$$…$$`, `\[…\]`, `\(…\)` or `$…$` pair, then trims
/// again. A pair is only stripped around non-blank content, and a `$`/`$$` pair only when the
/// inside has no other unescaped `$`, so `$a$ and $b$` (text with two formulas) stays whole.
fn strip_math_delimiters(tex: &str) -> &str {
    let tex = tex.trim();
    for (open, close) in [("$$", "$$"), ("\\[", "\\]"), ("\\(", "\\)"), ("$", "$")] {
        let Some(inner) = tex
            .strip_prefix(open)
            .and_then(|rest| rest.strip_suffix(close))
            .map(str::trim)
        else {
            continue;
        };
        if inner.is_empty() || (open.starts_with('$') && has_dollar(inner)) {
            continue;
        }
        return inner;
    }
    tex
}

/// True if `text` contains a `$` not preceded by a backslash.
fn has_dollar(text: &str) -> bool {
    let mut escaped = false;
    for c in text.chars() {
        match c {
            '$' if !escaped => return true,
            '\\' => escaped = !escaped,
            _ => escaped = false,
        }
    }
    false
}

/// nbconvert's carriage-return rule (`CoalesceStreamsPreprocessor`, regex `.*\r(?=[^\n])`):
/// on each line, everything up to and including the last `\r` that is followed by more
/// text of that line is dropped, so progress bars show their final state.
fn apply_carriage_returns(text: &str) -> Cow<'_, str> {
    if !text.contains('\r') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        // `\r` is ASCII, so `at + 1` is a char boundary.
        let keep_from = content.as_bytes()[..content.len().saturating_sub(1)]
            .iter()
            .rposition(|&byte| byte == b'\r')
            .map_or(0, |at| at + 1);
        out.push_str(&line[keep_from..]);
    }
    Cow::Owned(out)
}

/// CSI sequences other than SGR (`…m`): cursor movement, erase line, … nbconvert drops
/// them; `ansi-to-html` would keep the ones it does not recognize as visible text.
static NON_SGR_CSI: LazyLock<Regex> = LazyLock::new(|| {
    // A constant pattern covered by the unit tests: compiling it cannot fail.
    Regex::new(r"\x1b\[[0-?]*[ -/]*[@-ln-~]").expect("CSI regex is valid")
});

/// Every CSI sequence, for the fallback path of [`ansi_to_html`].
static ANY_CSI: LazyLock<Regex> = LazyLock::new(|| {
    // A constant pattern covered by the unit tests: compiling it cannot fail.
    Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]").expect("CSI regex is valid")
});

/// Escapes `text` for HTML and turns ANSI SGR codes (colors, bold, …) into `<span>`/`<b>`/…
/// tags via `ansi_to_html::convert`, which escapes `& < > " '` itself. Other CSI sequences
/// (cursor movement, erase line) are dropped first, as nbconvert does.
///
/// Never fails: when the converter rejects a code (e.g. `\x1b[38;5;999m`), all escape
/// sequences are removed and the plain text is escaped with `html_escape::encode_text`.
pub fn ansi_to_html(text: &str) -> String {
    let text = NON_SGR_CSI.replace_all(text, "");
    match ::ansi_to_html::convert(&text) {
        Ok(html) => html,
        Err(err) => {
            tracing::debug!(%err, "ANSI conversion failed; rendering the text without colors");
            let plain = ANY_CSI.replace_all(&text, "").replace('\u{1b}', "");
            html_escape::encode_text(&plain).into_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn carriage_returns_follow_nbconvert() {
        let cases = [
            ("plain\n", "plain\n"),
            ("10%\r50%\r100%\n", "100%\n"),
            ("a\rb\nc\rd", "b\nd"),
            // A `\r` at the end of a line (Windows line endings) is kept.
            ("x\r\ny\r\n", "x\r\ny\r\n"),
            ("a\r\r\n", "\r\n"),
            ("\rtail", "tail"),
            ("é\rü", "ü"),
        ];
        for (input, expected) in cases {
            assert_eq!(apply_carriage_returns(input), expected, "{input:?}");
        }
    }

    #[test]
    fn math_delimiters() {
        let cases = [
            ("$$x^2$$", "x^2"),
            ("  $$ x^2 $$\n", "x^2"),
            ("$x$", "x"),
            ("\\[a+b\\]", "a+b"),
            ("\\(a\\)", "a"),
            ("$\\displaystyle x$", "\\displaystyle x"),
            ("$\\$5$", "\\$5"),
            ("x^2", "x^2"),
            ("$a$ and $b$", "$a$ and $b$"),
            ("$$", "$$"),
            ("$", "$"),
            ("$$$", "$$$"),
            ("$ $", "$ $"),
            ("$$\\$$$", "\\$"),
            ("\\begin{align}a\\end{align}", "\\begin{align}a\\end{align}"),
        ];
        for (input, expected) in cases {
            assert_eq!(strip_math_delimiters(input), expected, "{input:?}");
        }
    }

    #[test]
    fn dimensions_are_numeric_only() {
        let metadata = json!({
            "image/png": { "width": 64, "height": 32.5 },
            "image/gif": { "width": "50%", "height": -3 },
            "image/webp": { "width": null },
            "width": 10
        });
        assert_eq!(
            dimension(&metadata, "image/png", "width").as_deref(),
            Some("64")
        );
        assert_eq!(
            dimension(&metadata, "image/png", "height").as_deref(),
            Some("32.5")
        );
        assert_eq!(dimension(&metadata, "image/gif", "width"), None);
        assert_eq!(dimension(&metadata, "image/gif", "height"), None);
        // Absent or null per-MIME values fall back to the top level (nbconvert).
        assert_eq!(
            dimension(&metadata, "image/webp", "width").as_deref(),
            Some("10")
        );
        assert_eq!(
            dimension(&metadata, "image/jpeg", "width").as_deref(),
            Some("10")
        );
        assert_eq!(dimension(&metadata, "image/jpeg", "height"), None);
        assert_eq!(dimension(&json!(64.0), "image/png", "width"), None);
        assert_eq!(
            dimension(&json!({"width": 64.0}), "image/png", "width").as_deref(),
            Some("64")
        );
    }

    fn sizes(width: &str, height: &str) -> Vec<(&'static str, String)> {
        vec![("width", width.to_owned()), ("height", height.to_owned())]
    }

    #[test]
    fn root_svg_attrs_are_replaced_or_appended() {
        let cases = [
            (
                r#"<svg viewBox="0 0 1 1"><rect width="1" height="1"/></svg>"#,
                r#"<svg viewBox="0 0 1 1" width="12" height="6"><rect width="1" height="1"/></svg>"#,
            ),
            (
                "<?xml version=\"1.0\"?>\n<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"x\">\n<svg\n  width=\"460.8pt\" height='345.6pt'\n  stroke-width=\"2\" style=\"a width=3\">x</svg>",
                "<?xml version=\"1.0\"?>\n<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"x\">\n<svg width=\"12\" height=\"6\"\n  stroke-width=\"2\" style=\"a width=3\">x</svg>",
            ),
            ("<svg/>", r#"<svg width="12" height="6"/>"#),
            ("<svg>", r#"<svg width="12" height="6">"#),
            (
                "<svg data-x=\"a>b\" width=1 >",
                "<svg data-x=\"a>b\" width=\"12\" height=\"6\" >",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(
                set_root_svg_attrs(input, &sizes("12", "6")),
                expected,
                "{input}"
            );
        }
    }

    #[test]
    fn malformed_svg_is_left_alone() {
        for input in [
            "no svg here",
            "<svgx width=\"1\">",
            "<svg width=\"1></svg>",
            "",
        ] {
            assert_eq!(
                set_root_svg_attrs(input, &sizes("12", "6")),
                input,
                "{input}"
            );
        }
    }

    #[test]
    fn non_sgr_sequences_are_dropped() {
        assert_eq!(
            ansi_to_html("\u{1b}[2K\u{1b}[1Aline\u{1b}[?25l"),
            "line",
            "erase line, cursor up, hide cursor"
        );
        assert!(NON_SGR_CSI.find("\u{1b}[31m").is_none());
        assert!(ANY_CSI.find("\u{1b}[31m").is_some());
    }
}
