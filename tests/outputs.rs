//! Notebook output rendering (T32; `docs/spec/notebook-mapping.md` §4, `docs/spec/ir.md`
//! `CellOutput`): MIME priority, images as assets, SVG sizes, ANSI streams and tracebacks,
//! LaTeX, Markdown through the caller's renderer, JSON, plain text, unsupported bundles and
//! the warnings they produce. Table-driven over `tests/fixtures/ipynb/outputs.ipynb` and
//! inline bundles.

use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use presenter::assets::registry::{AssetData, AssetRegistry};
use presenter::config::NotebookConfig;
use presenter::error::{Diagnostics, Error, Level};
use presenter::ir::{CellOutput, Features};
use presenter::notebook::nbformat::{MimeBundle, Output, parse_notebook};
use presenter::notebook::outputs::{
    ansi_to_html, choose_mime, codes, output_location, output_position, render_display,
    render_outputs,
};
use serde_json::{Value, json};

/// The 1×1 PNG of `outputs.ipynb` (cells `png` and `png-size`).
const PIXEL_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mPQqnr0HwAE3gKGkp+jugAAAABJRU5ErkJggg==";

/// What one `render_outputs` call produced.
struct Run {
    outputs: Vec<CellOutput>,
    features: Features,
    diags: Diagnostics,
    assets: AssetRegistry,
}

impl Run {
    /// (code, message) of every diagnostic, after checking what all of them share: warning
    /// level, no path or span (the caller adds those), an `output <i>: ` message prefix.
    fn warnings(&self) -> Vec<(&'static str, String)> {
        self.diags
            .iter()
            .map(|diag| {
                assert_eq!(diag.level, Level::Warning, "{diag}");
                assert!(diag.path.is_none() && diag.span.is_none(), "{diag:?}");
                assert!(diag.message.starts_with("output "), "{diag}");
                (diag.code, diag.message.clone())
            })
            .collect()
    }

    fn codes(&self) -> Vec<&'static str> {
        self.warnings().into_iter().map(|(code, _)| code).collect()
    }

    /// The single output's HTML (any variant that has one).
    fn html(&self) -> &str {
        assert_eq!(self.outputs.len(), 1, "{:?}", self.outputs);
        match &self.outputs[0] {
            CellOutput::Stream { html, .. }
            | CellOutput::Error { html, .. }
            | CellOutput::Display { html, .. } => html,
            CellOutput::Unsupported { mimes } => panic!("unsupported: {mimes:?}"),
        }
    }
}

/// Renders with `cfg` and a Markdown "renderer" that uppercases its input.
fn render_with(outputs: &[Output], cfg: &NotebookConfig) -> Run {
    let mut assets = AssetRegistry::new();
    let mut diags = Diagnostics::new();
    let mut md = |text: &str| Ok(text.to_uppercase());
    let (outputs, features) =
        render_outputs(outputs, cfg, &mut assets, &mut diags, &mut md).unwrap();
    Run {
        outputs,
        features,
        diags,
        assets,
    }
}

fn render(outputs: &[Output]) -> Run {
    render_with(outputs, &NotebookConfig::default())
}

/// Outputs written as nbformat JSON.
fn outputs(value: Value) -> Vec<Output> {
    serde_json::from_value(value).unwrap()
}

/// One `display_data` output with this bundle and metadata.
fn display(data: Value, metadata: Value) -> Vec<Output> {
    outputs(json!([{ "output_type": "display_data", "data": data, "metadata": metadata }]))
}

fn bundle(value: Value) -> MimeBundle {
    serde_json::from_value(value).unwrap()
}

fn priority(mimes: &[&str]) -> Vec<String> {
    mimes.iter().map(|mime| mime.to_string()).collect()
}

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ipynb/outputs.ipynb")
}

/// The outputs of the `outputs.ipynb` cell with this id.
fn fixture_cell(id: &str) -> Vec<Output> {
    let path = fixture_path();
    let json = std::fs::read_to_string(&path).unwrap();
    let (notebook, warnings) = parse_notebook(&path, &json).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let cell = notebook
        .cells
        .iter()
        .find(|cell| cell.id.as_deref() == Some(id));
    cell.unwrap_or_else(|| panic!("no cell {id}"))
        .outputs
        .clone()
}

fn display_output(mime: &str, html: &str, execution_count: Option<i64>) -> CellOutput {
    CellOutput::Display {
        mime: mime.to_owned(),
        html: html.to_owned(),
        execution_count,
    }
}

/// The id `register_bytes` gives the fixture PNG.
fn pixel_id() -> String {
    AssetRegistry::new().register_bytes(STANDARD.decode(PIXEL_PNG).unwrap(), "image/png", "x")
}

/// What one cell of `outputs.ipynb` renders to.
struct FixtureCase {
    id: &'static str,
    outputs: Vec<CellOutput>,
    /// `features.math`, the only feature `render_outputs` sets.
    math: bool,
    /// The warning codes, in order.
    codes: Vec<&'static str>,
}

/// A fixture case whose outputs render without math and without warnings.
fn plain_case(id: &'static str, outputs: Vec<CellOutput>) -> FixtureCase {
    FixtureCase {
        id,
        outputs,
        math: false,
        codes: vec![],
    }
}

/// The `text/html` payload of the `html-table` cell, joined.
fn fixture_table_html() -> String {
    let html_table = fixture_cell("html-table");
    let Output::ExecuteResult { data, .. } = &html_table[0] else {
        panic!("html-table is an execute_result");
    };
    serde_json::from_value::<Vec<String>>(data["text/html"].clone())
        .unwrap()
        .concat()
}

/// The fixture cells with markup outputs: HTML, images, LaTeX, Markdown.
fn markup_cases() -> Vec<FixtureCase> {
    let pixel = format!("asset:{}", pixel_id());
    let svg = concat!(
        "<figure class=\"output-image\">",
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 120 60\" width=\"120\" height=\"60\">\n",
        "  <rect width=\"120\" height=\"60\" fill=\"#2a7ae2\"/>\n",
        "  <text x=\"60\" y=\"36\" text-anchor=\"middle\" fill=\"white\">SVG</text>\n",
        "</svg></figure>",
    );
    let latex = r#"<div data-math-style="display">\int_0^1 x^2\,dx = \frac{1}{3}</div>"#;
    vec![
        plain_case("title", vec![]),
        plain_case(
            "html-table",
            vec![display_output("text/html", &fixture_table_html(), Some(1))],
        ),
        plain_case(
            "png",
            vec![display_output(
                "image/png",
                &format!(r#"<img class="output-image" src="{pixel}" alt="">"#),
                None,
            )],
        ),
        plain_case(
            "png-size",
            vec![display_output(
                "image/png",
                &format!(
                    r#"<img class="output-image" src="{pixel}" alt="" width="64" height="64">"#
                ),
                Some(3),
            )],
        ),
        plain_case("svg", vec![display_output("image/svg+xml", svg, None)]),
        FixtureCase {
            math: true,
            ..plain_case("latex", vec![display_output("text/latex", latex, None)])
        },
        plain_case(
            "markdown",
            vec![display_output(
                "text/markdown",
                "**BOLD** AND *ITALIC* FROM `MARKDOWN()`",
                None,
            )],
        ),
    ]
}

/// The fixture cells with escaped text outputs: JSON, plain text, streams, tracebacks and
/// widgets.
fn text_cases() -> Vec<FixtureCase> {
    let traceback = concat!(
        "<span style='color:var(--red,#a00)'>---------------------------------------------------------------------------</span>\n",
        "<span style='color:var(--red,#a00)'>ZeroDivisionError</span>                         Traceback (most recent call last)\n",
        "Cell <span style='color:var(--green,#0a0)'>In[10], line 1</span>\n",
        "<span style='color:var(--green,#0a0)'>----&gt; 1</span> <span style='color:#626262'><span style='background:var(--yellow,#a60)'>1/0</span></span>\n",
        "\n",
        "<span style='color:var(--red,#a00)'>ZeroDivisionError</span>: division by zero",
    );
    let json = "<pre class=\"output-json\">{\n  \"name\": \"presenter\",\n  \"nested\": {\n    \"ok\": true\n  },\n  \"values\": [\n    1,\n    2,\n    3\n  ]\n}</pre>";
    let plain = r#"<pre class="output-text">&#39;&lt;b&gt;not bold&lt;/b&gt; &amp; co&#39;</pre>"#;
    let stderr = "<span style='color:var(--red,#a00)'>red</span> warning: x &lt; y\n";
    let stream = |name: &str, html: &str| CellOutput::Stream {
        name: name.into(),
        html: html.into(),
    };
    let widget = r#"<pre class="output-text">IntSlider(value=5)</pre>"#;
    vec![
        plain_case("json", vec![display_output("application/json", json, None)]),
        plain_case("plain", vec![display_output("text/plain", plain, Some(8))]),
        plain_case(
            "streams",
            vec![stream("stdout", "a\nb\n"), stream("stderr", stderr)],
        ),
        plain_case(
            "error",
            vec![CellOutput::Error {
                ename: "ZeroDivisionError".into(),
                evalue: "division by zero".into(),
                html: traceback.into(),
            }],
        ),
        FixtureCase {
            codes: vec![codes::OUTPUT_UNSUPPORTED],
            ..plain_case(
                "widget-only",
                vec![CellOutput::Unsupported {
                    mimes: vec!["application/vnd.jupyter.widget-view+json".into()],
                }],
            )
        },
        FixtureCase {
            codes: vec![codes::OUTPUT_WIDGET],
            ..plain_case(
                "widget-plain",
                vec![display_output("text/plain", widget, Some(12))],
            )
        },
    ]
}

/// Every cell of `outputs.ipynb`: exact outputs, `features.math` and warning codes.
#[test]
fn fixture_outputs_render_per_spec_table() {
    let mut covered = Vec::new();
    for case in markup_cases().into_iter().chain(text_cases()) {
        let id = case.id;
        let run = render(&fixture_cell(id));
        assert_eq!(run.outputs, case.outputs, "{id}");
        assert_eq!(
            run.features,
            Features {
                math: case.math,
                ..Features::default()
            },
            "{id}: only math is ever set"
        );
        assert_eq!(run.codes(), case.codes, "{id}");
        covered.push(id);
    }
    let path = fixture_path();
    let (notebook, _) = parse_notebook(&path, &std::fs::read_to_string(&path).unwrap()).unwrap();
    let ids: Vec<&str> = notebook
        .cells
        .iter()
        .filter_map(|cell| cell.id.as_deref())
        .collect();
    assert_eq!(covered, ids, "every fixture cell has a case, in file order");
}

#[test]
fn priority_picks_the_first_configured_type_in_the_bundle() {
    let data = json!({ "text/plain": "<Figure>", "image/png": PIXEL_PNG });
    let defaults = NotebookConfig::default();
    assert_eq!(
        choose_mime(&bundle(data.clone()), &defaults.mime_priority),
        Some("image/png")
    );
    let plain_first = priority(&["text/plain"]);
    assert_eq!(
        choose_mime(&bundle(data.clone()), &plain_first),
        Some("text/plain")
    );
    // The list's order decides, not the bundle's (BTreeMap) order.
    assert_eq!(
        choose_mime(
            &bundle(data.clone()),
            &priority(&["text/plain", "image/png"])
        ),
        Some("text/plain")
    );
    assert_eq!(
        choose_mime(&bundle(data.clone()), &priority(&["text/html"])),
        None
    );
    assert_eq!(
        choose_mime(&bundle(json!({})), &defaults.mime_priority),
        None
    );

    let rendered = render(&display(data.clone(), json!({})));
    assert!(
        matches!(&rendered.outputs[0], CellOutput::Display { mime, .. } if mime == "image/png")
    );
    let cfg = NotebookConfig {
        mime_priority: plain_first,
        ..NotebookConfig::default()
    };
    let rendered = render_with(&display(data, json!({})), &cfg);
    assert_eq!(
        rendered.outputs,
        [display_output(
            "text/plain",
            r#"<pre class="output-text">&lt;Figure&gt;</pre>"#,
            None
        )]
    );
    assert!(rendered.assets.is_empty(), "the PNG was not chosen");
}

/// `<img class="output-image" src="asset:` + 12 lowercase hex + `"`, and that id.
fn img_asset_id(html: &str) -> String {
    let rest = html
        .strip_prefix(r#"<img class="output-image" src="asset:"#)
        .unwrap_or_else(|| panic!("{html}"));
    let id = &rest[..12];
    assert!(
        id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "{html}"
    );
    assert!(rest[12..].starts_with(r#"" alt="""#), "{html}");
    id.to_owned()
}

#[test]
fn png_is_registered_as_bytes_with_optional_size() {
    let original = STANDARD.decode(PIXEL_PNG).unwrap();
    assert!(original.starts_with(b"\x89PNG\r\n\x1a\n"));
    for id in ["png", "png-size"] {
        let run = render(&fixture_cell(id));
        let asset_id = img_asset_id(run.html());
        let asset = run.assets.get(&asset_id).expect("registered");
        assert_eq!(asset.data, AssetData::Bytes(original.clone()), "{id}");
        assert_eq!(asset.mime, "image/png");
        assert_eq!(asset.file_name_hint, "output.png");
        assert_eq!(run.assets.len(), 1);
        let sized = run.html().ends_with(r#" width="64" height="64">"#);
        assert_eq!(sized, id == "png-size", "{id}: {}", run.html());
    }
}

#[test]
fn base64_split_across_lines_and_unpadded_decodes_the_same() {
    let original = STANDARD.decode(PIXEL_PNG).unwrap();
    let (head, tail) = PIXEL_PNG.split_at(40);
    let payloads = [
        json!(format!("{head}\n{tail}\n")),
        json!([format!("{head}\n"), format!("{tail}\n")]),
        json!(format!(" {head}\r\n\t{tail} ")),
        json!(PIXEL_PNG.trim_end_matches('=')),
    ];
    for payload in payloads {
        let run = render(&display(json!({ "image/png": payload }), json!({})));
        let id = img_asset_id(run.html());
        assert_eq!(
            run.assets.get(&id).map(|asset| &asset.data),
            Some(&AssetData::Bytes(original.clone())),
            "{payload}"
        );
        assert!(run.diags.is_empty(), "{:?}", run.diags);
    }
}

#[test]
fn every_raster_type_gets_its_mime_hint_and_size() {
    let metadata = json!({
        "image/jpeg": { "width": 320 },
        "image/gif": { "height": 24.5 },
        "image/webp": { "width": "50%", "height": null },
        "width": 7
    });
    let cases = [
        ("image/jpeg", "output.jpg", r#" width="320">"#),
        (
            "image/gif",
            "output.gif",
            r#" alt="" width="7" height="24.5">"#,
        ),
        ("image/webp", "output.webp", r#" alt="">"#),
    ];
    for (mime, hint, suffix) in cases {
        // Distinct bytes per type so each registers on its own.
        let payload = STANDARD.encode(format!("{mime} bytes"));
        let run = render(&display(json!({ mime: payload }), metadata.clone()));
        let id = img_asset_id(run.html());
        let asset = run.assets.get(&id).unwrap();
        assert_eq!(
            (asset.mime.as_str(), asset.file_name_hint.as_str()),
            (mime, hint)
        );
        assert!(run.html().ends_with(suffix), "{mime}: {}", run.html());
    }
}

#[test]
fn invalid_base64_warns_and_falls_back_to_the_next_type() {
    let data = json!({ "image/png": "not base64!", "text/plain": "<Figure size 1x1>" });
    let run = render(&display(data, json!({})));
    assert_eq!(
        run.outputs,
        [display_output(
            "text/plain",
            r#"<pre class="output-text">&lt;Figure size 1x1&gt;</pre>"#,
            None
        )]
    );
    assert!(run.assets.is_empty());
    let warnings = run.warnings();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].0, codes::OUTPUT_INVALID);
    assert!(
        warnings[0]
            .1
            .starts_with("output 0: image/png not rendered: invalid base64"),
        "{}",
        warnings[0].1
    );

    // Without a fallback: Unsupported listing the bundle, after the invalid-payload warning.
    for payload in [
        json!("%%%"),
        json!(""),
        json!(" \n"),
        json!({ "not": "text" }),
    ] {
        let run = render(&display(json!({ "image/png": payload }), json!({})));
        assert_eq!(
            run.outputs,
            [CellOutput::Unsupported {
                mimes: vec!["image/png".into()]
            }],
            "{payload}"
        );
        assert_eq!(
            run.codes(),
            [codes::OUTPUT_INVALID, codes::OUTPUT_UNSUPPORTED],
            "{payload}"
        );
    }
}

#[test]
fn svg_sizes_touch_only_the_root_tag() {
    let svg = "<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"460.8pt\">\n<svg width=\"1\"/><rect width=\"2\"/></svg>\n";
    let run = render(&display(
        json!({ "image/svg+xml": svg }),
        json!({ "image/svg+xml": { "width": 120, "height": 60 } }),
    ));
    assert_eq!(
        run.html(),
        "<figure class=\"output-image\"><?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"120\" height=\"60\">\n<svg width=\"1\"/><rect width=\"2\"/></svg>\n</figure>"
    );
    // No size metadata: the payload verbatim inside the figure.
    let run = render(&display(json!({ "image/svg+xml": [svg] }), json!({})));
    assert_eq!(
        run.html(),
        format!("<figure class=\"output-image\">{svg}</figure>")
    );
}

#[test]
fn consecutive_streams_with_the_same_name_merge() {
    let run = render(&outputs(json!([
        { "output_type": "stream", "name": "stdout", "text": "a\n" },
        { "output_type": "stream", "name": "stdout", "text": ["b\n"] },
    ])));
    assert_eq!(
        run.outputs,
        [CellOutput::Stream {
            name: "stdout".into(),
            html: "a\nb\n".into()
        }]
    );

    // Only consecutive ones: another stream name or a display output in between splits them.
    let run = render(&outputs(json!([
        { "output_type": "stream", "name": "stdout", "text": "1" },
        { "output_type": "stream", "name": "stderr", "text": "2" },
        { "output_type": "stream", "name": "stdout", "text": "3" },
        { "output_type": "display_data", "data": { "text/plain": "4" }, "metadata": {} },
        { "output_type": "stream", "name": "stdout", "text": "5" },
        { "output_type": "stream", "name": "stdout", "text": "6" },
    ])));
    let summary: Vec<String> = run
        .outputs
        .iter()
        .map(|output| match output {
            CellOutput::Stream { name, html } => format!("{name}:{html}"),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(
        summary[..3],
        ["stdout:1".to_owned(), "stderr:2".into(), "stdout:3".into()]
    );
    assert!(matches!(run.outputs[3], CellOutput::Display { .. }));
    assert_eq!(summary[4], "stdout:56");
    assert_eq!(run.outputs.len(), 5);
}

#[test]
fn streams_merge_before_ansi_conversion_and_drop_carriage_return_overwrites() {
    // Jupyter may split an escape sequence's colored text across chunks.
    let run = render(&outputs(json!([
        { "output_type": "stream", "name": "stdout", "text": "\u{1b}[31mred" },
        { "output_type": "stream", "name": "stdout", "text": " still red\u{1b}[0m\n" },
    ])));
    assert_eq!(
        run.html(),
        "<span style='color:var(--red,#a00)'>red still red</span>\n"
    );
    // A progress bar keeps its final state (nbconvert's coalesce_streams rule).
    let run = render(&outputs(json!([
        { "output_type": "stream", "name": "stderr", "text": " 10%|#\r" },
        { "output_type": "stream", "name": "stderr", "text": "100%|##########\n" },
        { "output_type": "stream", "name": "stderr", "text": "done\r\n" },
    ])));
    assert_eq!(run.html(), "100%|##########\ndone\r\n");
}

#[test]
fn ansi_colors_become_spans_and_html_is_escaped() {
    let run = render(&outputs(json!([
        { "output_type": "stream", "name": "stdout", "text": "\u{1b}[31mred\u{1b}[0m <b>&" }
    ])));
    let html = run.html();
    assert!(html.contains("<span style='color:"), "{html}");
    assert!(html.contains(">red</span>"), "{html}");
    assert!(html.ends_with(" &lt;b&gt;&amp;"), "{html}");
    assert!(!html.contains('\u{1b}'), "{html}");

    // ansi-to-html 0.2.4's `convert` escapes HTML itself (no pre-escaping, no double escaping).
    assert_eq!(ansi_to_html("<script>&amp;"), "&lt;script&gt;&amp;amp;");
    assert_eq!(ansi_to_html("a \"q\" 'q'"), "a &quot;q&quot; &#39;q&#39;");
    assert_eq!(
        ansi_to_html("\u{1b}[1mbold\u{1b}[22m \u{1b}[38;5;211m#211\u{1b}[39m"),
        "<b>bold</b> <span style='color:#ff87af'>#211</span>"
    );
    assert_eq!(ansi_to_html(""), "");
}

#[test]
fn ansi_conversion_never_fails() {
    // Codes ansi-to-html rejects (an 8-bit color above 255, SGR 56): the text survives,
    // escaped, without any escape sequence.
    for text in [
        "\u{1b}[38;5;999mx < y\u{1b}[0m",
        "\u{1b}[56mx < y",
        "x\u{1b}[56m < y",
    ] {
        assert_eq!(ansi_to_html(text), "x &lt; y", "{text:?}");
    }
    // Only the rejected sequence goes; the colors around it stay.
    assert_eq!(
        ansi_to_html("\u{1b}[31mred\u{1b}[56m < y\u{1b}[0m"),
        "<span style='color:var(--red,#a00)'>red &lt; y</span>"
    );
}

/// Sequences ansi-to-html passes through as text (`ESC` byte included) unless they are
/// cleaned first: each renders as its text, with its styling when it is valid SGR.
#[test]
fn escape_sequences_the_converter_keeps_as_text_are_removed() {
    let red = |text: &str| format!("<span style='color:var(--red,#a00)'>{text}</span>");
    let cases = [
        // SGR with an empty parameter (ECMA-48 default 0) or colon sub-parameters.
        ("\u{1b}[;31mx", red("x")),
        ("\u{1b}[1;;31mx", red("x")),
        (
            "\u{1b}[38:5:241mcolon",
            "<span style='color:#626262'>colon</span>".into(),
        ),
        ("\u{1b}[4:3mcurly", "<u>curly</u>".into()),
        // Private (xterm modifyOtherKeys), truncated, two-byte and lone escapes.
        ("\u{1b}[>4;2mx", "x".into()),
        ("\u{1b}[31", String::new()),
        ("\u{1b}", String::new()),
        ("tail\u{1b}", "tail".into()),
        ("\u{1b}7save\u{1b}8", "save".into()),
        ("\u{1b}Mreverse-index", "reverse-index".into()),
        ("\u{1b}(Bplain", "plain".into()),
        // OSC titles and hyperlinks, also next to a rejected code.
        ("\u{1b}]0;title\u{7}after", "after".into()),
        (
            "\u{1b}]8;;http://x\u{1b}\\link\u{1b}]8;;\u{1b}\\",
            "link".into(),
        ),
        (
            "\u{1b}[38;5;999mbad\u{1b}]8;;http://x\u{1b}\\link",
            "badlink".into(),
        ),
        ("\u{1b}]8;;x\u{1b}[31mred", red("red")),
        // An OSC that never ends loses only its introducer.
        ("a\u{1b}]8;;x < y", "a8;;x &lt; y".into()),
    ];
    for (text, expected) in cases {
        let html = ansi_to_html(text);
        assert_eq!(html, expected, "{text:?}");
    }
}

/// Deterministic fuzzing: whatever the input, the HTML has no `ESC` byte and nothing but the
/// converter's own tags is markup.
#[test]
fn ansi_html_never_contains_escapes_or_stray_markup() {
    let fragments = [
        "\u{1b}", "[", "]", ";", ":", "0", "1", "2", "3", "5", "8", "9", "m", "K", "?", ">", "(",
        "B", "P", "\u{7}", "\\", "a", "é", "<", ">", "&", "'", "\"", "\n", "\r", " ", "38;5;",
        "48:2:", "4:3", "999",
    ];
    let tags = regex::Regex::new(r"<(?:span|u) style='[^'<>]*'>|</?(?:span|b|i|u|s)>").unwrap();
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..20_000 {
        let len = next() % 24;
        let text: String = (0..len)
            .map(|_| fragments[(next() % fragments.len() as u64) as usize])
            .collect();
        let html = ansi_to_html(&text);
        let rest = tags.replace_all(&html, "");
        assert!(
            !rest.contains(['\u{1b}', '<', '>', '\'', '"']),
            "{text:?} -> {html:?}"
        );
    }
}

#[test]
fn error_traceback_is_joined_and_ansi_converted() {
    let run = render(&outputs(json!([{
        "output_type": "error",
        "ename": "ValueError",
        "evalue": "bad <input>",
        "traceback": ["\u{1b}[0;31mValueError\u{1b}[0m: bad <input>", "line 2"]
    }])));
    assert_eq!(
        run.outputs,
        [CellOutput::Error {
            ename: "ValueError".into(),
            evalue: "bad <input>".into(),
            html:
                "<span style='color:var(--red,#a00)'>ValueError</span>: bad &lt;input&gt;\nline 2"
                    .into(),
        }]
    );
    assert!(run.diags.is_empty());
}

#[test]
fn latex_strips_one_delimiter_pair_and_sets_math() {
    let cases = [
        (json!("$$x^2$$"), "x^2"),
        (json!(["$x", "^2$\n"]), "x^2"),
        (json!("\\[ a < b \\]"), "a &lt; b"),
        (json!("\\(a & b\\)"), "a &amp; b"),
        (
            json!("$\\displaystyle \\frac{1}{2}$"),
            "\\displaystyle \\frac{1}{2}",
        ),
        (
            json!("\\begin{align} a &= b \\end{align}"),
            "\\begin{align} a &amp;= b \\end{align}",
        ),
        // A `$` inside `\text{}` does not keep the `$$` pair (KaTeX rejects a leftover `$$`).
        (json!("$$\\text{a $b$ c}$$"), "\\text{a $b$ c}"),
    ];
    for (payload, tex) in cases {
        let run = render(&display(
            json!({ "text/latex": payload, "text/plain": "x" }),
            json!({}),
        ));
        assert_eq!(
            run.outputs,
            [display_output(
                "text/latex",
                &format!(r#"<div data-math-style="display">{tex}</div>"#),
                None
            )],
            "{payload}"
        );
        assert!(run.features.math, "{payload}");
    }
    let (html, sets_math) = render_display(
        "text/latex",
        &json!("$$x^2$$"),
        &json!({}),
        &mut AssetRegistry::new(),
    )
    .unwrap();
    assert_eq!(html, r#"<div data-math-style="display">x^2</div>"#);
    assert!(sets_math);
}

#[test]
fn widget_only_bundle_is_unsupported_with_a_warning() {
    let widget = json!({ "model_id": "abc", "version_major": 2, "version_minor": 0 });
    let run = render(&display(
        json!({
            "application/vnd.jupyter.widget-view+json": widget,
            "application/vnd.jupyter.widget-state+json": {}
        }),
        json!({}),
    ));
    assert_eq!(
        run.outputs,
        [CellOutput::Unsupported {
            mimes: vec![
                "application/vnd.jupyter.widget-state+json".into(),
                "application/vnd.jupyter.widget-view+json".into(),
            ]
        }]
    );
    assert_eq!(
        run.warnings(),
        [(
            codes::OUTPUT_UNSUPPORTED,
            "output 0: no renderable MIME type (found: application/vnd.jupyter.widget-state+json, \
             application/vnd.jupyter.widget-view+json); [notebook] mime_priority decides what is \
             rendered"
                .to_owned()
        )]
    );
    // An empty bundle is unsupported too.
    let run = render(&display(json!({}), json!({})));
    assert_eq!(run.outputs, [CellOutput::Unsupported { mimes: vec![] }]);
    assert!(run.warnings()[0].1.contains("(found: none)"));
}

#[test]
fn widget_rendered_through_a_fallback_warns() {
    let widget = json!({ "model_id": "abc", "version_major": 2, "version_minor": 0 });
    let cases = [
        ("text/plain", json!("IntSlider(value=5)")),
        ("text/html", json!("<b>5</b>")),
    ];
    for (fallback, payload) in cases {
        let run = render(&display(
            json!({ "application/vnd.jupyter.widget-view+json": widget, fallback: payload }),
            json!({}),
        ));
        assert!(
            matches!(&run.outputs[0], CellOutput::Display { mime, .. } if mime == fallback),
            "{:?}",
            run.outputs
        );
        assert_eq!(
            run.warnings(),
            [(
                codes::OUTPUT_WIDGET,
                format!(
                    "output 0: interactive widget rendered as {fallback} \
                     (widgets need a live kernel)"
                )
            )]
        );
    }
    // Bundles without a widget view do not warn.
    let run = render(&display(
        json!({ "application/vnd.jupyter.widget-state+json": {}, "text/plain": "x" }),
        json!({}),
    ));
    assert!(run.diags.is_empty(), "{:?}", run.diags);
}

#[test]
fn only_markup_mentioning_math_sets_the_math_feature() {
    let span = r#"<span data-math-style="inline">x</span>"#;
    let cases = [
        (json!({ "text/html": span }), true),
        (
            json!({ "image/svg+xml": format!("<svg><foreignObject>{span}</foreignObject></svg>") }),
            true,
        ),
        // Escaped text that mentions the attribute is not a math element.
        (json!({ "text/plain": span }), false),
        (json!({ "text/plain": format!("'{span}'") }), false),
        (
            json!({ "application/json": { "html": "<span data-math-style='x'>" } }),
            false,
        ),
    ];
    for (data, math) in cases {
        let run = render(&display(data.clone(), json!({})));
        assert_eq!(run.features.math, math, "{data}");
    }
}

/// `output <i>` counts the outputs nbformat kept, so after a skipped output it differs from
/// the position in the `.ipynb` array (documented on `render_outputs`' module).
#[test]
fn diagnostic_index_counts_parsed_outputs() {
    let notebook = json!({
        "nbformat": 4, "nbformat_minor": 5, "metadata": {},
        "cells": [{
            "cell_type": "code", "source": "", "metadata": {}, "execution_count": 1,
            "outputs": [
                { "output_type": "bogus" },
                { "output_type": "display_data", "metadata": {},
                  "data": { "text/html": "<script>x()</script>" } }
            ]
        }]
    });
    let (notebook, parse_diags) =
        parse_notebook(Path::new("nb.ipynb"), &notebook.to_string()).unwrap();
    assert!(
        parse_diags
            .iter()
            .any(|diag| diag.message.contains("output 0 (bogus) skipped")),
        "{parse_diags:?}"
    );
    let run = render(&notebook.cells[0].outputs);
    assert_eq!(
        run.codes(),
        [codes::OUTPUT_SCRIPT],
        "the script output is in the file at index 1"
    );
    assert!(run.warnings()[0].1.starts_with("output 0: "));
}

#[test]
fn markdown_goes_through_the_supplied_renderer() {
    let run = render(&display(
        json!({ "text/markdown": ["# Title\n", "*x*"], "text/plain": "x" }),
        json!({}),
    ));
    assert_eq!(
        run.outputs,
        [display_output("text/markdown", "# TITLE\n*X*", None)]
    );
    assert!(!run.features.math);

    // The renderer's math makes the output need KaTeX.
    let mut md = |text: &str| {
        Ok(format!(
            "<p><span data-math-style=\"inline\">{text}</span></p>"
        ))
    };
    let (rendered, features) = render_outputs(
        &display(json!({ "text/markdown": "x" }), json!({})),
        &NotebookConfig::default(),
        &mut AssetRegistry::new(),
        &mut Diagnostics::new(),
        &mut md,
    )
    .unwrap();
    assert!(features.math);
    assert_eq!(
        rendered,
        [display_output(
            "text/markdown",
            "<p><span data-math-style=\"inline\">x</span></p>",
            None
        )]
    );

    // The renderer's errors are the caller's errors.
    let mut failing = |_: &str| Err(Error::Other("renderer failed".into()));
    let result = render_outputs(
        &display(
            json!({ "text/markdown": "x", "text/plain": "x" }),
            json!({}),
        ),
        &NotebookConfig::default(),
        &mut AssetRegistry::new(),
        &mut Diagnostics::new(),
        &mut failing,
    );
    assert!(matches!(result, Err(Error::Other(message)) if message == "renderer failed"));

    // A non-text Markdown payload is a warning and falls back.
    let run = render(&display(
        json!({ "text/markdown": { "x": 1 }, "text/plain": "x" }),
        json!({}),
    ));
    assert_eq!(run.codes(), [codes::OUTPUT_INVALID]);
    assert!(matches!(&run.outputs[0], CellOutput::Display { mime, .. } if mime == "text/plain"));
}

#[test]
fn html_is_verbatim_and_scripts_warn() {
    let page = "<div id=\"plot\"></div>\n<SCRIPT type=\"text/javascript\">draw('plot')</SCRIPT>";
    let run = render(&outputs(json!([{
        "output_type": "execute_result",
        "data": { "text/html": page, "text/plain": "<Plot>" },
        "metadata": {},
        "execution_count": 4
    }])));
    assert_eq!(run.outputs, [display_output("text/html", page, Some(4))]);
    assert_eq!(
        run.warnings(),
        [(
            codes::OUTPUT_SCRIPT,
            "output 0: text/html contains <script>; it is kept and runs when the slides load"
                .to_owned()
        )]
    );
    let run = render(&display(
        json!({ "text/html": "<b>no script</b>" }),
        json!({}),
    ));
    assert!(run.diags.is_empty());
}

#[test]
fn json_and_plain_text_are_escaped() {
    let run = render(&display(
        json!({ "application/json": { "html": "<b>&</b>", "n": [1] } }),
        json!({}),
    ));
    assert_eq!(
        run.html(),
        "<pre class=\"output-json\">{\n  \"html\": \"&lt;b&gt;&amp;&lt;/b&gt;\",\n  \"n\": [\n    1\n  ]\n}</pre>"
    );
    let run = render(&display(
        json!({ "text/plain": ["a < b\n", "\u{1b}[32mok\u{1b}[0m"] }),
        json!({}),
    ));
    assert_eq!(
        run.html(),
        "<pre class=\"output-text\">a &lt; b\n<span style='color:var(--green,#0a0)'>ok</span></pre>"
    );
}

#[test]
fn execution_count_only_on_execute_results() {
    let run = render(&outputs(json!([
        { "output_type": "display_data", "data": { "text/plain": "d" }, "metadata": {} },
        { "output_type": "execute_result", "data": { "text/plain": "e" }, "metadata": {},
          "execution_count": 7 },
        { "output_type": "execute_result", "data": { "text/plain": "n" }, "metadata": {},
          "execution_count": null },
    ])));
    let counts: Vec<Option<i64>> = run
        .outputs
        .iter()
        .map(|output| match output {
            CellOutput::Display {
                execution_count, ..
            } => *execution_count,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(counts, [None, Some(7), None]);
}

#[test]
fn unknown_types_in_the_priority_list_fall_through() {
    let cfg = NotebookConfig {
        mime_priority: priority(&["application/vnd.plotly.v1+json", "text/html", "text/plain"]),
        ..NotebookConfig::default()
    };
    let run = render_with(
        &display(
            json!({ "application/vnd.plotly.v1+json": { "data": [] }, "text/html": "<div>plot</div>" }),
            json!({}),
        ),
        &cfg,
    );
    assert_eq!(
        run.outputs,
        [display_output("text/html", "<div>plot</div>", None)]
    );
    assert_eq!(
        run.warnings(),
        [(
            codes::OUTPUT_INVALID,
            "output 0: application/vnd.plotly.v1+json not rendered: no renderer for \
             application/vnd.plotly.v1+json"
                .to_owned()
        )]
    );
}

#[test]
fn render_display_rejects_what_it_cannot_render() {
    let mut assets = AssetRegistry::new();
    let cases = [
        ("text/markdown", json!("*x*")),
        ("text/html", json!({ "not": "text" })),
        ("text/plain", json!(["a", 1])),
        ("image/png", json!("%%%")),
        ("video/mp4", json!("AAAA")),
    ];
    for (mime, payload) in cases {
        let result = render_display(mime, &payload, &json!({}), &mut assets);
        assert!(matches!(result, Err(Error::Other(_))), "{mime}: {result:?}");
    }
    assert!(assets.is_empty());
}

#[test]
fn no_outputs_render_nothing() {
    let run = render(&[]);
    assert!(run.outputs.is_empty());
    assert_eq!(run.features, Features::default());
    assert!(run.diags.is_empty());
}

/// Audit F38: a diagnostic about an output names it as `cell C, output N`, which orders it.
#[test]
fn output_locations_are_written_and_read_back() {
    assert_eq!(output_location(3, 1), "cell 3, output 1");
    assert_eq!(
        output_position("cell 3, output 1: image `x.png` not found"),
        Some((3, 1))
    );
    assert_eq!(
        output_position(&format!("{}:", output_location(12, 0))),
        Some((12, 0))
    );
    for message in [
        "cell 3, line 1: x",
        "cell 3, output 1 x",
        "cell , output 1: x",
        "output 1: x",
        "cell 3, output : x",
        "",
    ] {
        assert_eq!(output_position(message), None, "{message}");
    }
}
