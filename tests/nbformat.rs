//! nbformat 4 model contract tests (docs/spec/notebook-mapping.md §1, §3, §7): every fixture
//! notebook parses, slide types and metadata accessors, lenient outputs and attachments,
//! nbformat version checks and the language precedence.

use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use presenter::error::{Diagnostics, Error, Level};
use presenter::ir::SourceSpan;
use presenter::notebook::nbformat::{
    CellType, MultilineString, Notebook, Output, SlideType, codes, mime_text, parse_notebook,
};
use serde_json::{Map, Value, json};

/// The fixtures this task owns; later tasks may add more files next to them.
const FIXTURES: [&str; 5] = [
    "frontmatter.ipynb",
    "minimal.ipynb",
    "outputs.ipynb",
    "slideshow.ipynb",
    "tags.ipynb",
];

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ipynb")
}

fn fixture(name: &str) -> Notebook {
    let path = fixture_dir().join(name);
    let json = std::fs::read_to_string(&path).unwrap();
    let (notebook, warnings) = parse_notebook(&path, &json).unwrap();
    assert!(warnings.is_empty(), "{name}: {warnings:?}");
    notebook
}

fn parse(value: Value) -> (Notebook, Diagnostics) {
    parse_notebook(Path::new("test.ipynb"), &value.to_string()).unwrap()
}

/// (cell index from the span, code, message) of every warning, after checking the parts
/// every warning shares: warning level, the path given to `parse_notebook`, and a
/// whole-cell span (lines 1..=source lines) when the warning is about a cell.
fn summary(
    notebook: &Notebook,
    warnings: &Diagnostics,
) -> Vec<(Option<usize>, &'static str, String)> {
    warnings
        .iter()
        .map(|warning| {
            assert_eq!(warning.level, Level::Warning, "{warning}");
            assert_eq!(
                warning.path.as_deref(),
                Some(Path::new("test.ipynb")),
                "{warning}"
            );
            let cell = warning.span.as_ref().map(|span| {
                let index = span.cell.expect("cell-level spans name their cell");
                assert_eq!(span, &notebook.cells[index].span(index), "{warning}");
                index
            });
            (cell, warning.code, warning.message.clone())
        })
        .collect()
}

/// A valid nbformat 4.5 notebook around `cells`.
fn notebook_with(cells: Value) -> Value {
    json!({ "cells": cells, "metadata": {}, "nbformat": 4, "nbformat_minor": 5 })
}

fn parse_error(json: &str) -> (PathBuf, Option<usize>, String) {
    match parse_notebook(Path::new("talk.ipynb"), json) {
        Err(Error::Parse {
            path,
            line,
            message,
        }) => (path, line, message),
        other => panic!("expected Error::Parse, got {other:?}"),
    }
}

#[test]
fn every_fixture_parses_without_warnings() {
    let found: Vec<String> = std::fs::read_dir(fixture_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".ipynb"))
        .collect();
    for name in FIXTURES {
        assert!(found.iter().any(|f| f == name), "missing fixture {name}");
    }
    // Fixtures added by later tasks (e.g. lint inputs that warn on purpose) only need to parse.
    for name in found.iter().filter(|f| !FIXTURES.contains(&f.as_str())) {
        let path = fixture_dir().join(name);
        let json = std::fs::read_to_string(&path).unwrap();
        assert!(parse_notebook(&path, &json).is_ok(), "{name}");
    }
    for name in FIXTURES {
        let notebook = fixture(name);
        assert_eq!(
            (notebook.nbformat, notebook.nbformat_minor),
            (4, 5),
            "{name}"
        );
        assert!(!notebook.cells.is_empty(), "{name}");
        for (index, cell) in notebook.cells.iter().enumerate() {
            assert!(cell.id.is_some(), "{name} cell {index} has no id");
            if cell.cell_type != CellType::Code {
                assert!(cell.outputs.is_empty(), "{name} cell {index}");
            }
        }
        assert_eq!(notebook.language(), "python", "{name}");
    }
}

#[test]
fn slideshow_reports_spec_section_6_slide_types_in_order() {
    let notebook = fixture("slideshow.ipynb");
    let types: Vec<SlideType> = notebook
        .cells
        .iter()
        .map(|cell| cell.slide_type())
        .collect();
    assert_eq!(
        types,
        [
            SlideType::Slide,
            SlideType::Continue,
            SlideType::Slide,
            SlideType::Fragment,
            SlideType::Subslide,
            SlideType::Skip,
            SlideType::Notes,
            SlideType::Continue,
        ]
    );
    let kinds: Vec<CellType> = notebook.cells.iter().map(|cell| cell.cell_type).collect();
    use CellType::{Code, Markdown, Raw};
    assert_eq!(
        kinds,
        [
            Markdown, Markdown, Code, Code, Markdown, Code, Markdown, Raw
        ]
    );
    let sources: Vec<&str> = notebook.cells.iter().map(|c| c.source.as_str()).collect();
    assert_eq!(
        sources,
        [
            "# Analysis {layout=title}",
            "Some intro",
            "df = load()",
            "df.describe()",
            "## Details",
            "debug()",
            "Remember to mention caching",
            "<video src=\"clip.mp4\" controls></video>",
        ]
    );
    let describe = &notebook.cells[3];
    assert_eq!(describe.tags(), ["hide-input"]);
    assert_eq!(describe.execution_count, Some(2));
    assert!(matches!(
        &describe.outputs[..],
        [Output::ExecuteResult {
            execution_count: Some(2),
            ..
        }]
    ));
    assert_eq!(notebook.cells[7].raw_mimetype(), Some("text/html"));
    assert_eq!(notebook.cells[5].execution_count, None);
    for cell in &notebook.cells {
        assert!(!cell.is_hidden_source() && !cell.is_hidden_outputs());
        assert_eq!(cell.presenter_overrides(), None);
    }
}

#[test]
fn source_as_list_and_as_string_are_identical() {
    let cell = |source: Value| json!({ "cell_type": "markdown", "metadata": {}, "source": source });
    let (from_list, _) = parse(notebook_with(json!([cell(json!(["# T\n", "\n", "body"]))])));
    let (from_string, _) = parse(notebook_with(json!([cell(json!("# T\n\nbody"))])));
    assert_eq!(from_list.cells[0].source, from_string.cells[0].source);
    assert_eq!(
        from_list.cells[0].source,
        MultilineString::from("# T\n\nbody")
    );
    // The fixtures use both forms.
    let minimal = fixture("minimal.ipynb");
    assert_eq!(
        minimal.cells[0].source.as_str(),
        "# Minimal notebook\n\nOne markdown cell and one code cell."
    );
    assert_eq!(minimal.cells[1].source.as_str(), "print(\"hello\")");
}

#[test]
fn unknown_output_type_is_skipped_with_a_warning_and_the_cell_survives() {
    let (notebook, warnings) = parse(notebook_with(json!([
        { "cell_type": "markdown", "metadata": {}, "source": "intro" },
        {
            "cell_type": "code", "id": "c1", "execution_count": 3, "metadata": { "tags": ["t"] },
            "source": ["x = 1\n", "x"],
            "outputs": [
                { "output_type": "stream", "name": "stdout", "text": "before\n" },
                { "output_type": "weird", "data": { "text/plain": "?" } },
                { "output_type": "execute_result", "execution_count": 3, "metadata": {},
                  "data": { "text/plain": "1" } }
            ]
        }
    ])));
    let found = summary(&notebook, &warnings);
    assert_eq!(found.len(), 1, "{warnings:?}");
    let (cell, code, message) = &found[0];
    assert_eq!((*cell, *code), (Some(1), codes::OUTPUT_SKIPPED));
    assert!(
        message.starts_with("output 1 (weird) skipped: unknown variant `weird`"),
        "{message}"
    );
    let warning = warnings.iter().next().unwrap();
    assert_eq!(
        warning.span,
        Some(SourceSpan {
            start_line: 1,
            end_line: 2,
            cell: Some(1),
            file: None
        })
    );
    assert!(
        warning
            .to_string()
            .starts_with("test.ipynb: cell 1, line 1: warning: output 1 (weird) skipped: "),
        "{warning}"
    );
    let cell = &notebook.cells[1];
    assert_eq!(cell.id.as_deref(), Some("c1"));
    assert_eq!(cell.source.as_str(), "x = 1\nx");
    assert_eq!(cell.execution_count, Some(3));
    assert_eq!(cell.tags(), ["t"]);
    assert_eq!(cell.outputs.len(), 2);
    assert_eq!(
        cell.outputs[0],
        Output::Stream {
            name: "stdout".into(),
            text: "before\n".into()
        }
    );
    let Output::ExecuteResult { data, .. } = &cell.outputs[1] else {
        panic!("{:?}", cell.outputs[1]);
    };
    assert_eq!(data["text/plain"], json!("1"));
}

#[test]
fn malformed_known_outputs_and_odd_shapes_are_warnings_too() {
    let (notebook, warnings) = parse(notebook_with(json!([
        {
            "cell_type": "code", "metadata": {}, "source": "",
            "outputs": [
                { "output_type": "stream", "name": "stdout" },
                { "output_type": "error", "ename": "E", "evalue": "v", "traceback": ["t"] }
            ]
        },
        { "cell_type": "code", "metadata": {}, "source": "", "outputs": { "not": "a list" } },
        {
            "cell_type": "markdown", "metadata": {}, "source": "![a](attachment:a.png)",
            "attachments": {
                "a.png": { "image/png": ["iVBORw0K", "GgoA"] },
                "bad.json": { "application/json": { "an": "object" } }
            }
        },
        { "cell_type": "raw", "metadata": {}, "source": "", "attachments": [] }
    ])));
    let expected = [
        (0, codes::OUTPUT_SKIPPED, "output 0 (stream) skipped: missing field `text`"),
        (1, codes::OUTPUT_SKIPPED, "`outputs` is not a list, ignored"),
        (
            2,
            codes::ATTACHMENT_SKIPPED,
            "attachment 'bad.json' skipped: invalid type: map, expected a string or a list of strings",
        ),
        (3, codes::ATTACHMENT_SKIPPED, "`attachments` is not an object, ignored"),
    ]
    .map(|(cell, code, message)| (Some(cell), code, message.to_owned()));
    assert_eq!(summary(&notebook, &warnings), expected);
    assert!(matches!(
        &notebook.cells[0].outputs[..],
        [Output::Error { .. }]
    ));
    assert!(notebook.cells[1].outputs.is_empty());
    let attachments = &notebook.cells[2].attachments;
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments["a.png"]["image/png"].as_str(), "iVBORw0KGgoA");
}

#[test]
fn nbformat_3_is_a_parse_error_mentioning_nbformat_4() {
    let v3 = json!({
        "metadata": { "name": "old" }, "nbformat": 3, "nbformat_minor": 0,
        "worksheets": [ { "cells": [], "metadata": {} } ]
    });
    let (path, line, message) = parse_error(&v3.to_string());
    assert_eq!(path, Path::new("talk.ipynb"));
    assert_eq!(line, None);
    assert_eq!(message, "nbformat 4 required"); // spec §1, verbatim
    let (_, _, message) = parse_error(r#"{"cells": [], "nbformat": 2}"#);
    assert_eq!(message, "nbformat 4 required");

    // Not a notebook version at all (spec §1 does not cover these).
    for json in [
        r#"{"cells": [], "metadata": {}}"#,
        r#"{"cells": [], "nbformat": "4"}"#,
        r#"{"cells": [], "nbformat": 4.5}"#,
        r#"{"cells": [], "nbformat": -4}"#,
    ] {
        let (_, _, message) = parse_error(json);
        assert_eq!(
            message, "not a Jupyter notebook: `nbformat` is missing or not an integer",
            "{json}"
        );
    }
}

#[test]
fn other_unusable_documents_are_parse_errors() {
    let (_, line, message) = parse_error("{\n \"cells\": [\n  {,\n ]\n}");
    assert_eq!(line, Some(3));
    assert!(message.starts_with("invalid notebook JSON: "), "{message}");

    let (_, _, message) = parse_error("[1, 2]");
    assert_eq!(message, "not a Jupyter notebook: expected a JSON object");
    let (_, _, message) = parse_error(r#"{"nbformat": 4}"#);
    assert_eq!(message, "not a Jupyter notebook: `cells` is missing");
    let (_, _, message) = parse_error(r#"{"nbformat": 4, "cells": {}}"#);
    assert_eq!(message, "`cells` is not a list");
    let cell_error = |cell: Value| {
        let notebook = notebook_with(json!([{ "cell_type": "markdown", "source": "" }, cell]));
        parse_error(&notebook.to_string()).2
    };
    assert!(
        cell_error(json!({ "source": "no cell_type" }))
            .starts_with("cell 1: missing field `cell_type`"),
    );
    assert!(cell_error(json!({ "cell_type": 3 })).starts_with("cell 1: invalid type: integer `3`"));
    assert!(
        cell_error(json!({ "cell_type": "code", "source": 3 })).starts_with("cell 1: invalid type")
    );
    assert!(cell_error(json!("not an object")).starts_with("cell 1: invalid type: string"));
}

#[test]
fn unknown_cell_type_is_kept_as_unknown_with_a_warning() {
    // nbformat 5.11.1 validates and reads this (an "unrecognized cell" from 4.6).
    let (notebook, warnings) = parse(json!({
        "cells": [
            { "cell_type": "markdown", "id": "a", "metadata": {}, "source": "# A" },
            { "cell_type": "chart", "id": "b", "metadata": {}, "source": ["x\n", "y"] },
            { "cell_type": "heading", "metadata": {}, "source": "", "level": 1 },
            { "cell_type": "code", "id": "c", "metadata": {}, "source": "1", "outputs": [],
              "execution_count": null }
        ],
        "metadata": {},
        "nbformat": 4,
        "nbformat_minor": 6
    }));
    let kinds: Vec<CellType> = notebook.cells.iter().map(|cell| cell.cell_type).collect();
    use CellType::{Code, Markdown, Unknown};
    assert_eq!(kinds, [Markdown, Unknown, Unknown, Code]);
    assert_eq!(notebook.cells[1].id.as_deref(), Some("b"));
    assert_eq!(notebook.cells[1].source.as_str(), "x\ny");
    let expected = [
        (1, "unknown cell_type \"chart\", cell ignored"),
        (2, "unknown cell_type \"heading\", cell ignored"),
    ]
    .map(|(cell, message)| (Some(cell), codes::UNKNOWN_CELL_TYPE, message.to_owned()));
    assert_eq!(summary(&notebook, &warnings), expected);
}

#[test]
fn newer_nbformat_is_read_with_a_warning() {
    let (notebook, warnings) = parse(json!({ "cells": [], "nbformat": 5 }));
    assert_eq!(notebook.nbformat, 5);
    assert_eq!(notebook.nbformat_minor, 0);
    assert_eq!(
        summary(&notebook, &warnings),
        [(
            None,
            codes::NBFORMAT_NEWER,
            "nbformat 5 is newer than this tool knows; reading it as nbformat 4".to_owned()
        )]
    );
    assert_eq!(
        warnings.iter().next().unwrap().to_string(),
        "test.ipynb: warning: nbformat 5 is newer than this tool knows; reading it as nbformat 4"
    );
}

#[test]
fn unknown_keys_anywhere_are_ignored() {
    let (notebook, warnings) = parse(json!({
        "cells": [
            {
                "cell_type": "code", "id": "x", "source": "1", "execution_count": 1,
                "future_cell_key": [1, 2],
                "metadata": { "custom": { "deep": true }, "collapsed": false, "scrolled": "auto" },
                "outputs": [
                    { "output_type": "display_data", "data": { "text/plain": "1" },
                      "metadata": {}, "transient": { "display_id": "d" }, "future_output_key": 1 },
                    { "output_type": "stream", "name": "stdout", "text": "s", "extra": null }
                ]
            },
            { "cell_type": "markdown", "source": "no metadata, no outputs" }
        ],
        "metadata": { "anything": { "at": ["all"] }, "widgets": {} },
        "nbformat": 4,
        "nbformat_minor": 5,
        "future_top_level_key": "x"
    }));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(notebook.cells.len(), 2);
    assert_eq!(notebook.cells[0].outputs.len(), 2);
    assert_eq!(notebook.cells[1].metadata, Value::Null);
    assert_eq!(notebook.cells[1].slide_type(), SlideType::Continue);
    assert!(notebook.cells[1].tags().is_empty());
}

#[test]
fn null_and_missing_optional_fields_use_defaults() {
    let (notebook, warnings) = parse(json!({
        "cells": [ {
            "cell_type": "code", "id": null, "metadata": null, "source": null,
            "outputs": null, "execution_count": null, "attachments": null
        } ],
        "metadata": null,
        "nbformat": 4
    }));
    assert!(warnings.is_empty(), "{warnings:?}");
    let cell = &notebook.cells[0];
    assert_eq!(cell.id, None);
    assert_eq!(cell.source, MultilineString::default());
    assert!(cell.outputs.is_empty() && cell.attachments.is_empty());
    assert_eq!(notebook.language(), "text");
    assert_eq!(notebook.title(), None);
}

#[test]
fn language_precedence() {
    let language = |metadata: Value| {
        let mut nb = notebook_with(json!([]));
        nb["metadata"] = metadata;
        parse(nb).0.language().to_owned()
    };
    let both = json!({
        "kernelspec": { "name": "ir", "display_name": "R", "language": "R" },
        "language_info": { "name": "julia" }
    });
    assert_eq!(language(both), "julia");
    assert_eq!(language(json!({ "kernelspec": { "language": "R" } })), "R");
    assert_eq!(
        language(json!({ "language_info": { "name": "rust" } })),
        "rust"
    );
    assert_eq!(
        language(json!({ "language_info": { "name": "" }, "kernelspec": { "language": "R" } })),
        "R"
    );
    assert_eq!(
        language(json!({ "kernelspec": { "name": "python3" } })),
        "text"
    );
    assert_eq!(language(json!({})), "text");
    assert_eq!(fixture("minimal.ipynb").language(), "python"); // kernelspec only
}

#[test]
fn unknown_slide_type_is_continue_with_a_warning() {
    let cell = |slide_type: Value| json!({ "cell_type": "markdown", "source": "", "metadata": { "slideshow": { "slide_type": slide_type } } });
    let (notebook, warnings) = parse(notebook_with(json!([
        cell(json!("slide")),
        cell(json!("Slide")),
        cell(json!(3)),
        cell(json!("-")),
        { "cell_type": "markdown", "source": "", "metadata": { "slideshow": {} } },
        cell(json!("")),   // JupyterLab 4 writes "" for "-"
        cell(json!(null)), // null means absent
        { "cell_type": "markdown", "source": "", "metadata": { "slideshow": null } }
    ])));
    let types: Vec<SlideType> = notebook.cells.iter().map(|c| c.slide_type()).collect();
    let mut expected = vec![SlideType::Continue; 8];
    expected[0] = SlideType::Slide;
    assert_eq!(types, expected);
    let expected = [
        (
            1,
            "unknown slideshow.slide_type \"Slide\", treated as \"-\"",
        ),
        (2, "unknown slideshow.slide_type 3, treated as \"-\""),
    ]
    .map(|(cell, message)| (Some(cell), codes::UNKNOWN_SLIDE_TYPE, message.to_owned()));
    assert_eq!(summary(&notebook, &warnings), expected);
    assert_eq!(
        format!("{:#}", warnings.iter().next().unwrap()),
        "test.ipynb: cell 1, line 1: warning: unknown slideshow.slide_type \"Slide\", treated as \"-\" [unknown-slide-type]"
    );
}

#[test]
fn jupyterlab_4_metadata_parses_without_warnings() {
    // The cell metadata JupyterLab 4's property inspector writes when "-" is selected.
    let metadata = json!({ "editable": true, "slideshow": { "slide_type": "" }, "tags": [] });
    let (notebook, warnings) = parse(notebook_with(json!([
        { "cell_type": "markdown", "id": "m", "metadata": metadata, "source": "# Title" },
        { "cell_type": "code", "id": "c", "metadata": metadata, "source": "1",
          "outputs": [], "execution_count": null }
    ])));
    assert!(warnings.is_empty(), "{warnings:?}");
    for cell in &notebook.cells {
        assert_eq!(cell.slide_type(), SlideType::Continue);
        assert!(cell.tags().is_empty());
    }
}

#[test]
fn tags_fixture_accessors() {
    let notebook = fixture("tags.ipynb");
    let cells = &notebook.cells;
    let tags: Vec<Vec<String>> = cells.iter().map(|cell| cell.tags()).collect();
    let expected: [&[&str]; 15] = [
        &[],
        &["remove-cell"],
        &["skip"],
        &["hide-cell"],
        &["remove-input"],
        &["hide-input", "unrelated"],
        &[],
        &["remove-output"],
        &["hide-output"],
        &[],
        &["fragment"],
        &["notes"],
        &["hide-input"],
        &[],
        &["custom-hide"],
    ];
    assert_eq!(tags, expected);
    let source_hidden: Vec<usize> = (0..cells.len())
        .filter(|&i| cells[i].is_hidden_source())
        .collect();
    let outputs_hidden: Vec<usize> = (0..cells.len())
        .filter(|&i| cells[i].is_hidden_outputs())
        .collect();
    assert_eq!(source_hidden, [6]);
    assert_eq!(outputs_hidden, [9, 12]);
    // Tags do not change the slide type at this level (the converter applies them).
    assert!(
        cells
            .iter()
            .all(|cell| matches!(cell.slide_type(), SlideType::Continue | SlideType::Slide))
    );
    assert_eq!(cells[0].slide_type(), SlideType::Slide);
    assert_eq!(
        cells[0].presenter_overrides(),
        Some(&json!({
            "layout": "two-column",
            "classes": ["dark", "wide"],
            "attrs": { "data-background-color": "#102030" }
        }))
    );
    assert_eq!(
        cells[13].presenter_overrides(),
        Some(&json!({ "layout": "ignored" }))
    );
    let with_overrides = cells
        .iter()
        .filter(|cell| cell.presenter_overrides().is_some())
        .count();
    assert_eq!(with_overrides, 2);
    for (index, cell) in cells.iter().enumerate() {
        if cell.cell_type == CellType::Code && index != 1 && index != 2 && index != 3 {
            assert!(
                matches!(&cell.outputs[..], [Output::Stream { name, .. }] if name == "stdout"),
                "cell {index}"
            );
        }
    }
}

#[test]
fn frontmatter_fixture_metadata() {
    let notebook = fixture("frontmatter.ipynb");
    assert_eq!(notebook.title(), Some("Title from metadata.title"));
    let presenter = notebook.presenter_meta().unwrap();
    assert_eq!(presenter["title"], json!("Title from metadata.presenter"));
    assert_eq!(presenter["author"], json!("Author from metadata.presenter"));
    assert_eq!(presenter["event"], json!("Fixture Conf"));
    let kinds: Vec<CellType> = notebook.cells.iter().map(|cell| cell.cell_type).collect();
    use CellType::{Code, Markdown};
    assert_eq!(kinds, [Code, Markdown, Code, Markdown]);
    // The first markdown cell is not cell 0; cell 0 is skipped.
    assert_eq!(notebook.cells[0].slide_type(), SlideType::Skip);
    // A later markdown cell also starts with a `---` block, which must not become metadata.
    let second = &notebook.cells[3];
    assert_eq!(second.slide_type(), SlideType::Slide);
    assert!(
        second
            .source
            .as_str()
            .starts_with("---\ntitle: must be ignored\n---\n## Second slide\n"),
        "{:?}",
        second.source
    );
    let first = &notebook.cells[1];
    assert_eq!(first.cell_type, CellType::Markdown);
    assert_eq!(first.slide_type(), SlideType::Slide);
    assert!(
        first
            .source
            .as_str()
            .starts_with("---\ntitle: Title from front matter\n"),
        "{:?}",
        first.source
    );
    assert!(first.source.as_str().contains("---\n# Welcome\n"));
    let titles = [
        notebook.title().unwrap(),
        presenter["title"].as_str().unwrap(),
        "Title from front matter",
    ];
    assert!(titles[0] != titles[1] && titles[1] != titles[2] && titles[0] != titles[2]);
}

#[test]
fn non_object_presenter_metadata_is_ignored_with_a_warning() {
    let (notebook, warnings) = parse(json!({
        "cells": [ { "cell_type": "markdown", "source": "", "metadata": { "presenter": "two-column" } } ],
        "metadata": { "presenter": ["x"], "title": 7 },
        "nbformat": 4
    }));
    assert_eq!(notebook.presenter_meta(), None);
    assert_eq!(notebook.title(), None);
    assert_eq!(notebook.cells[0].presenter_overrides(), None);
    let message = "metadata.presenter is not an object, ignored".to_owned();
    assert_eq!(
        summary(&notebook, &warnings),
        [
            (None, codes::PRESENTER_NOT_OBJECT, message.clone()),
            (Some(0), codes::PRESENTER_NOT_OBJECT, message),
        ]
    );
}

#[test]
fn null_presenter_metadata_is_absent_without_a_warning() {
    let (notebook, warnings) = parse(json!({
        "cells": [ { "cell_type": "markdown", "source": "", "metadata": { "presenter": null } } ],
        "metadata": { "presenter": null, "title": null },
        "nbformat": 4
    }));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(notebook.presenter_meta(), None);
    assert_eq!(notebook.title(), None);
    assert_eq!(notebook.cells[0].presenter_overrides(), None);
}

#[test]
fn raw_mimetype_falls_back_to_format() {
    let raw =
        |metadata: Value| json!({ "cell_type": "raw", "source": "<b>x</b>", "metadata": metadata });
    let (notebook, _) = parse(notebook_with(json!([
        raw(json!({ "raw_mimetype": "text/html", "format": "text/latex" })),
        raw(json!({ "format": "text/html" })),
        raw(json!({}))
    ])));
    let mimes: Vec<Option<&str>> = notebook.cells.iter().map(|c| c.raw_mimetype()).collect();
    assert_eq!(mimes, [Some("text/html"), Some("text/html"), None]);
}

/// The single display/execute-result output of cell `index`: (metadata, MIME types in
/// bundle order, payload of the first MIME type).
fn bundle(notebook: &Notebook, index: usize) -> (&Map<String, Value>, Vec<&str>, &Value) {
    let outputs = &notebook.cells[index].outputs;
    let [Output::DisplayData { data, metadata } | Output::ExecuteResult { data, metadata, .. }] =
        &outputs[..]
    else {
        panic!("cell {index}: {outputs:?}");
    };
    let first = data.values().next().unwrap();
    let mimes = data.keys().map(String::as_str).collect();
    (metadata.as_object().unwrap(), mimes, first)
}

/// The outputs fixture: one output of each kind of spec §4, in the documented order.
#[test]
fn outputs_fixture_cells_and_execution_counts() {
    let notebook = fixture("outputs.ipynb");
    let ids: Vec<&str> = notebook
        .cells
        .iter()
        .map(|c| c.id.as_deref().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "title",
            "html-table",
            "png",
            "png-size",
            "svg",
            "latex",
            "markdown",
            "json",
            "plain",
            "streams",
            "error",
            "widget-only",
            "widget-plain"
        ]
    );
    let execution_counts: Vec<Option<i64>> =
        notebook.cells.iter().map(|c| c.execution_count).collect();
    let expected: Vec<Option<i64>> = std::iter::once(None).chain((1..=12).map(Some)).collect();
    assert_eq!(execution_counts, expected);
}

#[test]
fn outputs_fixture_html_and_images() {
    let notebook = fixture("outputs.ipynb");
    let (_, mimes, html) = bundle(&notebook, 1);
    assert_eq!(mimes, ["text/html", "text/plain"]);
    assert!(
        mime_text(html)
            .unwrap()
            .contains("<table border=\"1\" class=\"dataframe\">\n")
    );

    for index in [2, 3] {
        let (metadata, mimes, png) = bundle(&notebook, index);
        assert_eq!(mimes, ["image/png", "text/plain"]);
        let base64 = mime_text(png).unwrap();
        let bytes = STANDARD.decode(base64.trim_end()).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&bytes[12..16], b"IHDR");
        assert_eq!(
            (&bytes[16..20], &bytes[20..24]),
            (&[0, 0, 0, 1][..], &[0, 0, 0, 1][..])
        );
        if index == 2 {
            assert!(base64.ends_with('\n') && metadata.is_empty());
        } else {
            assert_eq!(metadata["image/png"], json!({ "width": 64, "height": 64 }));
        }
    }

    let (metadata, mimes, svg) = bundle(&notebook, 4);
    assert_eq!(mimes, ["image/svg+xml", "text/plain"]);
    assert!(svg.is_array());
    assert!(mime_text(svg).unwrap().starts_with("<svg xmlns="));
    assert_eq!(
        metadata["image/svg+xml"],
        json!({ "width": 120, "height": 60 })
    );
}

#[test]
fn outputs_fixture_text_payloads() {
    let notebook = fixture("outputs.ipynb");
    let (_, mimes, latex) = bundle(&notebook, 5);
    assert_eq!(mimes, ["text/latex", "text/plain"]);
    let latex = mime_text(latex).unwrap();
    assert!(latex.starts_with("$$") && latex.ends_with("$$"), "{latex}");

    let (_, mimes, _) = bundle(&notebook, 6);
    assert_eq!(mimes, ["text/markdown", "text/plain"]);
    let (_, mimes, json_payload) = bundle(&notebook, 7);
    assert_eq!(mimes, ["application/json", "text/plain"]);
    assert!(json_payload.is_object());
    assert_eq!(mime_text(json_payload), None);
    let (_, mimes, plain) = bundle(&notebook, 8);
    assert_eq!(mimes, ["text/plain"]);
    assert!(mime_text(plain).unwrap().contains('<'));
}

#[test]
fn outputs_fixture_streams_error_and_widgets() {
    let notebook = fixture("outputs.ipynb");
    let streams = &notebook.cells[9].outputs;
    let expected_streams = [
        ("stdout", "a\n"),
        ("stdout", "b\n"),
        ("stderr", "\u{1b}[31mred\u{1b}[0m warning: x < y\n"),
    ];
    assert_eq!(streams.len(), expected_streams.len());
    for (output, (name, text)) in streams.iter().zip(expected_streams) {
        assert_eq!(
            output,
            &Output::Stream {
                name: name.into(),
                text: text.into()
            }
        );
    }

    let [
        Output::Error {
            ename,
            evalue,
            traceback,
        },
    ] = &notebook.cells[10].outputs[..]
    else {
        panic!("{:?}", notebook.cells[10].outputs);
    };
    assert_eq!(
        (ename.as_str(), evalue.as_str()),
        ("ZeroDivisionError", "division by zero")
    );
    assert_eq!(traceback.len(), 4);
    assert!(traceback.iter().all(|line| line.contains('\u{1b}')));

    let (_, mimes, widget) = bundle(&notebook, 11);
    assert_eq!(mimes, ["application/vnd.jupyter.widget-view+json"]);
    assert!(widget.is_object());
    let (_, mimes, _) = bundle(&notebook, 12);
    assert_eq!(
        mimes,
        ["application/vnd.jupyter.widget-view+json", "text/plain"]
    );
}

/// Audit F43: one leading UTF-8 byte order mark is ignored, as for Markdown and TOML files;
/// a second one, or one later in the text, is still invalid JSON.
#[test]
fn a_leading_byte_order_mark_is_ignored() {
    let path = fixture_dir().join("slideshow.ipynb");
    let json = std::fs::read_to_string(&path).unwrap();
    let (plain, _) = parse_notebook(&path, &json).unwrap();
    let (with_bom, warnings) = parse_notebook(&path, &format!("\u{feff}{json}")).unwrap();
    assert_eq!(with_bom, plain);
    assert!(warnings.is_empty(), "{warnings:?}");
    for bad in [
        format!("\u{feff}\u{feff}{json}"),
        format!(" \u{feff}{json}"),
    ] {
        match parse_notebook(&path, &bad) {
            Err(Error::Parse { message, .. }) => {
                assert!(message.starts_with("invalid notebook JSON: "), "{message}");
            }
            other => panic!("expected a parse error, got {other:?}"),
        }
    }
}
