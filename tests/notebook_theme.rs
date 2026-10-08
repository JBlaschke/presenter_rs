//! T33: notebook theming end to end. The executed example notebook
//! (`examples/notebook/analysis.ipynb`) follows the task's recipe and builds cleanly with the
//! default theme's `cell.html` and `styles/notebook.css`; `[notebook] show_prompts` switches
//! the prompts; a child theme that replaces only `cell.html`
//! (`tests/fixtures/themes/cell-override/`) renders with the default stylesheet unchanged.
//!
//! What only a browser shows (the DataFrame fits 1280×720, a 200-line output scrolls, errors
//! and stderr are tinted) was checked by hand over the DevTools protocol; see the T33 commit
//! message and `docs/screenshots/notebook-*.png`. The CSS rules behind it are pinned in
//! `tests/theme_tokens.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use presenter::pipeline::{Build, BuildOptions, build_deck};
use regex::Regex;
use serde_json::Value;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn example() -> PathBuf {
    root().join("examples/notebook/analysis.ipynb")
}

fn cell_override() -> PathBuf {
    root().join("tests/fixtures/themes/cell-override")
}

/// The example notebook as JSON.
fn notebook() -> Value {
    let text = std::fs::read_to_string(example()).expect("examples/notebook/analysis.ipynb");
    serde_json::from_str(&text).expect("valid JSON")
}

fn cells(nb: &Value) -> &[Value] {
    nb["cells"].as_array().expect("cells")
}

/// An nbformat multiline string (a string or a list of lines) as one string.
fn joined(text: &Value) -> String {
    match text {
        Value::String(text) => text.clone(),
        Value::Array(lines) => lines.iter().filter_map(Value::as_str).collect(),
        other => panic!("not a multiline string: {other}"),
    }
}

/// A cell's source as one string.
fn source(cell: &Value) -> String {
    joined(&cell["source"])
}

fn tags(cell: &Value) -> Vec<&str> {
    cell["metadata"]["tags"]
        .as_array()
        .map(|tags| tags.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

fn code_cells(nb: &Value) -> Vec<&Value> {
    cells(nb)
        .iter()
        .filter(|cell| cell["cell_type"] == "code")
        .collect()
}

/// Builds `input` like `presenter build` (standalone), optionally with `--theme` and
/// `--config`.
fn build(input: &Path, theme: Option<&Path>, config: Option<&Path>) -> Build {
    let mut opts = BuildOptions::new(input);
    opts.cli.theme = theme.map(|t| t.to_string_lossy().into_owned());
    opts.config_path = config.map(Path::to_path_buf);
    build_deck(&opts).unwrap_or_else(|e| panic!("{}: {e}", input.display()))
}

/// The `<div class="slides">` part of a document (the theme stylesheet in `<head>` names the
/// same classes).
fn slides(html: &str) -> &str {
    let start = html.find(r#"<div class="slides">"#).expect("slides");
    &html[start..]
}

/// The class lists of every element whose class list contains `token`.
fn elements_with_class<'a>(html: &'a str, token: &str) -> Vec<&'a str> {
    let class = Regex::new(r#"class="([^"]*)""#).unwrap();
    class
        .captures_iter(html)
        .map(|caps| caps.get(1).unwrap().as_str())
        .filter(|classes| classes.split_whitespace().any(|c| c == token))
        .collect()
}

// ---------------------------------------------------------------------------------------
// The example notebook
// ---------------------------------------------------------------------------------------

/// Task step 3: slideshow metadata on every cell, one `hide-input` tag, front matter with
/// `title` and `theme: default` in the first markdown cell, a deliberate `ZeroDivisionError`
/// cell marked `fragment`, a `$$` LaTeX cell; numpy, pandas and matplotlib.
#[test]
fn the_example_notebook_follows_the_recipe() {
    let nb = notebook();
    assert_eq!(nb["nbformat"], 4);
    assert!(nb["nbformat_minor"].as_u64().unwrap() >= 5, "cell ids");
    let cells = cells(&nb);
    assert!((8..=16).contains(&cells.len()), "{} cells", cells.len());
    for (i, cell) in cells.iter().enumerate() {
        assert!(
            cell["metadata"]["slideshow"]["slide_type"].is_string(),
            "cell {i} has no slideshow metadata"
        );
        assert!(cell["id"].is_string(), "cell {i} has no id");
    }
    let hide_input: Vec<usize> = (0..cells.len())
        .filter(|&i| tags(&cells[i]).contains(&"hide-input"))
        .collect();
    assert_eq!(hide_input.len(), 1, "{hide_input:?}");

    let first_markdown = cells
        .iter()
        .find(|cell| cell["cell_type"] == "markdown")
        .unwrap();
    let front = source(first_markdown);
    assert!(front.starts_with("---\n"), "{front}");
    // Up to and including the line break before the closing `---`.
    let end = front[3..].find("\n---\n").expect("closed front matter") + 4;
    assert!(front[..end].contains("\ntitle: "), "{front}");
    assert!(front[..end].contains("\ntheme: default\n"), "{front}");

    let error = cells
        .iter()
        .find(|cell| {
            cell["outputs"]
                .as_array()
                .is_some_and(|outs| outs.iter().any(|o| o["ename"] == "ZeroDivisionError"))
        })
        .expect("a ZeroDivisionError cell");
    assert_eq!(error["metadata"]["slideshow"]["slide_type"], "fragment");
    assert!(cells.iter().any(|cell| {
        cell["cell_type"] == "markdown" && source(cell).matches("$$").count() >= 2
    }));
    let code: String = code_cells(&nb).iter().map(|cell| source(cell)).collect();
    for import in ["import numpy", "import pandas", "import matplotlib.pyplot"] {
        assert!(code.contains(import), "{import}");
    }
}

/// The outputs come from one top-to-bottom run (`jupyter nbconvert --execute`, README) and
/// hold what the slides demonstrate: a PNG plot, a DataFrame, stdout and stderr, an output
/// long enough to scroll. No per-cell timing metadata, so re-running changes nothing.
#[test]
fn the_example_notebook_is_executed() {
    let nb = notebook();
    let code = code_cells(&nb);
    for (n, cell) in code.iter().enumerate() {
        assert_eq!(cell["execution_count"], n + 1, "cell {}", cell["id"]);
        assert!(
            !cell["outputs"].as_array().unwrap().is_empty(),
            "cell {} has no outputs",
            cell["id"]
        );
    }
    for cell in cells(&nb) {
        assert!(
            cell["metadata"].get("execution").is_none(),
            "timing metadata in cell {}",
            cell["id"]
        );
    }
    let outputs: Vec<&Value> = code
        .iter()
        .flat_map(|cell| cell["outputs"].as_array().unwrap())
        .collect();
    assert!(
        outputs
            .iter()
            .any(|o| o["output_type"] == "display_data" && o["data"]["image/png"].is_string())
    );
    assert!(outputs.iter().any(|o| {
        o["output_type"] == "execute_result"
            && o["data"]["text/html"].to_string().contains("dataframe")
    }));
    assert!(outputs.iter().any(|o| o["name"] == "stderr"));
    let longest = outputs
        .iter()
        .filter(|o| o["name"] == "stdout")
        .map(|o| joined(&o["text"]).lines().count())
        .max()
        .unwrap();
    assert!(longest >= 200, "{longest} lines");
}

/// Acceptance criterion 1 (the parts a test can see): the build succeeds without
/// diagnostics, every code cell is a `.cell`, the error output is there inside a fragment
/// cell, the plot is an inline PNG, the DataFrame is an HTML table, and the theme stylesheet
/// includes notebook.css.
#[test]
fn the_example_notebook_builds_with_styled_cells() {
    let built = build(&example(), None, None);
    assert!(
        built.diagnostics.is_empty(),
        "{:?}",
        built.diagnostics.iter().collect::<Vec<_>>()
    );
    let html = built.document.html.as_str();
    let slides = slides(html);
    let cells = elements_with_class(slides, "cell");
    assert_eq!(cells.len(), code_cells(&notebook()).len(), "{cells:?}");
    assert_eq!(
        cells.iter().filter(|c| c.contains("hide-input")).count(),
        1,
        "{cells:?}"
    );

    let error = Regex::new(
        r#"(?s)<div class="cell fragment"[^>]*>.*?<pre class="output output-error">(.*?)</pre>"#,
    )
    .unwrap();
    let traceback = &error.captures(slides).expect("an error in a fragment cell")[1];
    assert!(traceback.contains("ZeroDivisionError"), "{traceback}");
    assert!(slides.contains(r#"<pre class="output output-stream stderr">"#));
    assert!(slides.contains(r#"<img class="output-image" src="data:image/png;base64,"#));
    assert!(slides.contains(r#"<table border="1" class="dataframe">"#));
    assert!(
        slides.contains(r#"data-math-style="display""#),
        "the $$ cell"
    );
    assert!(slides.contains(r#"class="layout-title center""#));

    let css = &built.document.theme_css;
    let theme = css.find("/* @default/styles/theme.css */").unwrap();
    let notebook = css.find("/* @default/styles/notebook.css */").unwrap();
    assert!(theme < notebook, "notebook.css comes after theme.css");
    assert!(
        html.contains(css.as_str()),
        "standalone: the stylesheet is inlined"
    );
}

/// `[notebook] show_prompts` (config.md §2) switches every In/Out prompt.
#[test]
fn show_prompts_switches_the_prompts() {
    let built = build(&example(), None, None);
    let prompts = Regex::new(r#"<div class="cell-prompt">((?:In|Out) \[\d+\]):</div>"#).unwrap();
    let shown: Vec<&str> = prompts
        .captures_iter(slides(&built.document.html))
        .map(|caps| caps.get(1).unwrap().as_str())
        .collect();
    let code = code_cells(&notebook()).len();
    assert_eq!(shown.iter().filter(|p| p.starts_with("In")).count(), code);
    assert_eq!(shown.iter().filter(|p| p.starts_with("Out")).count(), 1);

    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("presenter.toml");
    std::fs::write(&config, "[notebook]\nshow_prompts = false\n").unwrap();
    let built = build(&example(), None, Some(&config));
    let slides = slides(&built.document.html);
    assert!(elements_with_class(slides, "cell-prompt").is_empty());
    assert_eq!(elements_with_class(slides, "cell").len(), code);
}

// ---------------------------------------------------------------------------------------
// A child theme that overrides only cell.html
// ---------------------------------------------------------------------------------------

/// Acceptance criterion 2: the fixture child theme holds nothing but `theme.toml` and
/// `templates/cell.html`; its cells replace the default markup, and the theme stylesheet is
/// byte for byte the default theme's.
#[test]
fn a_child_theme_can_replace_cell_html_alone() {
    let files: BTreeSet<String> = walkdir::WalkDir::new(cell_override())
        .into_iter()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            let rel = entry.path().strip_prefix(cell_override()).unwrap();
            rel.to_string_lossy().replace('\\', "/")
        })
        .collect();
    assert_eq!(
        files,
        BTreeSet::from(["theme.toml".to_string(), "templates/cell.html".to_string()])
    );

    let default = build(&example(), None, None);
    let child = build(&example(), Some(&cell_override()), None);
    let names: Vec<&str> = child.theme.chain.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["cell-override", "default"]);
    assert_eq!(
        child.document.theme_css, default.document.theme_css,
        "no CSS of its own"
    );

    let slides = slides(&child.document.html);
    let cells = elements_with_class(slides, "cell");
    assert_eq!(cells.len(), code_cells(&notebook()).len());
    assert!(
        cells.iter().all(|c| c.contains("compact-cell")),
        "{cells:?}"
    );
    assert!(elements_with_class(slides, "cell-prompt").is_empty());
    // The default theme's classes are kept, so its stylesheet applies.
    for class in [
        "cell-input",
        "cell-outputs",
        "output-error",
        "output-display",
        "stderr",
        "hide-input",
        "fragment",
    ] {
        assert!(
            !elements_with_class(slides, class).is_empty(),
            "{class} missing"
        );
    }
    assert!(slides.contains(r#"data-tags="hide-input""#));
}
