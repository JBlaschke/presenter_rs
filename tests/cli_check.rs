//! T34 end to end: `presenter check` on small decks written into temporary directories and on
//! the examples: the lints of markdown-dialect §10 / notebook-mapping §4 / theme-format §2,
//! the text and JSON formats and the exit codes of `docs/spec/cli.md` (`check`).

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The binary, run in `dir`, without the developer's `RUST_LOG` / `NO_COLOR` /
/// `CLICOLOR_FORCE`.
fn presenter(dir: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_presenter"));
    cmd.current_dir(dir)
        .env_remove("RUST_LOG")
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE");
    cmd
}

/// A temporary directory with `files` (path, contents) written into it.
fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, contents) in files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    dir
}

/// Runs `presenter <args>` in `dir`: (exit code, stdout, stderr).
fn run(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let output = presenter(dir).args(args).output().unwrap();
    (
        output.status.code().expect("exit code"),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

/// `presenter check --format json <inputs>` in `dir`: (exit code, the parsed array).
fn check_json(dir: &Path, inputs: &[&str]) -> (i32, Vec<Value>) {
    let mut args = vec!["check", "--format", "json"];
    args.extend_from_slice(inputs);
    let (code, stdout, stderr) = run(dir, &args);
    assert_eq!(stderr, "", "JSON mode prints nothing on stderr");
    let Value::Array(entries) = serde_json::from_str(&stdout).expect("stdout is JSON") else {
        panic!("not a JSON array: {stdout}");
    };
    (code, entries)
}

/// `(code, line)` of each JSON entry.
fn codes_and_lines(entries: &[Value]) -> Vec<(String, Option<u64>)> {
    entries
        .iter()
        .map(|e| (e["code"].as_str().unwrap().to_string(), e["line"].as_u64()))
        .collect()
}

/// A deck with the setext pitfall (line 8), `layout=nope` (slide 3, line 10) and the id
/// `same` on slides 1 and 4 (lines 1 and 14).
const PITFALLS: &str = "\
# First {#same}

A real separator follows.

---

Some text
--

--- {layout=nope}

## Second

---

## Third {#same}
";

#[test]
fn setext_pitfall_unknown_layout_and_duplicate_ids_are_reported() {
    let dir = project(&[("slides.md", PITFALLS)]);
    let (code, stdout, stderr) = run(dir.path(), &["check", "slides.md"]);
    assert_eq!(code, 0, "warnings alone exit 0: {stderr}");
    assert_eq!(stdout, "", "text mode prints on stderr only");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 3, "{stderr}");
    assert!(
        lines[0].starts_with("slides.md:8: warning: `--` under a line of text"),
        "{stderr}"
    );
    assert!(
        lines[1].starts_with("slides.md:10: warning: slide 3: unknown layout 'nope'"),
        "{stderr}"
    );
    assert!(
        lines[2].starts_with(
            "slides.md:14: warning: slide 4: id `same` is already the id of slide 1 (line 1)"
        ),
        "{stderr}"
    );
    assert!(
        !stderr.contains('\x1b'),
        "no colors when stderr is not a terminal"
    );

    let (code, entries) = check_json(dir.path(), &["slides.md"]);
    assert_eq!(code, 0);
    assert_eq!(
        codes_and_lines(&entries),
        [
            ("separator-not-recognized".to_string(), Some(8)),
            ("unknown-layout".to_string(), Some(10)),
            ("duplicate-slide-id".to_string(), Some(14)),
        ]
    );
}

#[test]
fn json_entries_have_the_documented_fields() {
    let dir = project(&[("deck.md", PITFALLS)]);
    let (_, entries) = check_json(dir.path(), &["deck.md"]);
    let first = entries[0].as_object().unwrap();
    let keys: Vec<&str> = first.keys().map(String::as_str).collect();
    let mut expected = vec!["level", "code", "message", "path", "line", "cell"];
    expected.sort_unstable();
    let mut keys = keys;
    keys.sort_unstable();
    assert_eq!(keys, expected);
    assert_eq!(first["level"], "warning");
    assert_eq!(first["code"], "separator-not-recognized");
    assert_eq!(first["path"], "deck.md");
    assert_eq!(first["line"], 8);
    assert_eq!(first["cell"], Value::Null);
    assert!(first["message"].as_str().unwrap().starts_with("`--` under"));
}

#[test]
fn deny_warnings_flips_the_exit_code() {
    let dir = project(&[
        ("slides.md", PITFALLS),
        ("clean.md", "# Clean\n\n---\n\n## Two\n"),
    ]);
    assert_eq!(run(dir.path(), &["check", "slides.md"]).0, 0);
    assert_eq!(
        run(dir.path(), &["check", "slides.md", "--deny-warnings"]).0,
        1
    );
    let (code, entries) = check_json(dir.path(), &["--deny-warnings", "slides.md"]);
    assert_eq!((code, entries.len()), (1, 3));
    // Without warnings --deny-warnings changes nothing.
    assert_eq!(
        run(dir.path(), &["check", "clean.md", "--deny-warnings"]),
        (0, String::new(), String::new())
    );
    // -q hides warnings in text mode, but the exit code still counts them.
    assert_eq!(
        run(dir.path(), &["-q", "check", "slides.md", "--deny-warnings"]),
        (1, String::new(), String::new())
    );
    // JSON is the full result, -q or not.
    let (code, stdout, _) = run(
        dir.path(),
        &["-q", "check", "--format", "json", "slides.md"],
    );
    assert_eq!(code, 0);
    assert_eq!(
        serde_json::from_str::<Vec<Value>>(&stdout).unwrap().len(),
        3
    );
}

#[test]
fn a_missing_file_exits_1_with_its_path() {
    let dir = project(&[]);
    let (code, stdout, stderr) = run(dir.path(), &["check", "talk/missing.md"]);
    assert_eq!(code, 1);
    assert_eq!(stdout, "");
    assert!(
        stderr.starts_with("talk/missing.md: error: cannot read talk/missing.md"),
        "{stderr}"
    );
    let (code, entries) = check_json(dir.path(), &["talk/missing.md"]);
    assert_eq!(code, 1);
    assert_eq!(entries[0]["level"], "error");
    assert_eq!(entries[0]["code"], "io-error");
    assert_eq!(entries[0]["path"], "talk/missing.md");
}

#[test]
fn every_input_is_checked_and_errors_win() {
    let dir = project(&[
        ("a.md", PITFALLS),
        ("b.md", "# Clean\n"),
        ("bad.md", "---\n- not a mapping\n---\n\n# x\n"),
    ]);
    let (code, entries) = check_json(dir.path(), &["a.md", "missing.md", "b.md", "bad.md"]);
    assert_eq!(code, 1);
    let found: Vec<(&str, &str, &str)> = entries
        .iter()
        .map(|e| {
            (
                e["path"].as_str().unwrap(),
                e["level"].as_str().unwrap(),
                e["code"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            ("a.md", "warning", "separator-not-recognized"),
            ("a.md", "warning", "unknown-layout"),
            ("a.md", "warning", "duplicate-slide-id"),
            ("missing.md", "error", "io-error"),
            ("bad.md", "error", "front-matter-error"),
        ]
    );
}

#[test]
fn unknown_extensions_and_config_errors_exit_1() {
    // cli.md `check`: "1 on errors", whatever the error (other commands exit 2 for these).
    let dir = project(&[
        ("notes.txt", "hello"),
        ("ok.md", "# One\n"),
        ("broken/presenter.toml", "[slides]\nsplit = \"sideways\"\n"),
        ("broken/slides.md", "# One\n"),
    ]);
    let (code, _, stderr) = run(dir.path(), &["check", "ok.md", "notes.txt"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.starts_with("notes.txt: error: cannot tell the input format"),
        "{stderr}"
    );
    let (code, _, stderr) = run(dir.path(), &["check", "broken/slides.md"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("presenter.toml"), "{stderr}");
    let (code, entries) = check_json(dir.path(), &["broken/slides.md"]);
    assert_eq!(code, 1);
    assert_eq!(entries[0]["code"], "config-error");
    let (code, _, stderr) = run(dir.path(), &["--config", "nope.toml", "check", "ok.md"]);
    assert_eq!(code, 1, "{stderr}");
    // clap's own usage errors keep exit 2.
    assert_eq!(run(dir.path(), &["check", "--format", "xml", "ok.md"]).0, 2);
}

#[test]
fn missing_assets_are_warnings_and_a_missing_theme_is_an_error() {
    let dir = project(&[
        ("img.md", "# One\n\n![x](nope.png)\n"),
        ("themed.md", "---\ntheme: ./no-such-theme\n---\n\n# One\n"),
    ]);
    let (code, entries) = check_json(dir.path(), &["img.md"]);
    assert_eq!(code, 0);
    assert_eq!(
        codes_and_lines(&entries),
        [("missing-asset".to_string(), Some(3))]
    );
    assert_eq!(entries[0]["level"], "warning");
    let (code, entries) = check_json(dir.path(), &["themed.md"]);
    assert_eq!(code, 1);
    assert_eq!(
        codes_and_lines(&entries),
        [("theme-error".to_string(), None)]
    );
    // --theme overrides the front matter, as in build.
    let (code, entries) = check_json(dir.path(), &["themed.md", "--theme", "default"]);
    assert_eq!((code, entries.len()), (0, 0));
}

#[test]
fn examples_report_no_warnings() {
    let mut decks = vec![
        "examples/basic/slides.md",
        "examples/headings-mode/slides.md",
    ];
    // Created by T33; it must be clean once it exists.
    if root().join("examples/notebook/analysis.ipynb").exists() {
        decks.push("examples/notebook/analysis.ipynb");
    }
    for deck in decks {
        let (code, stdout, stderr) = run(&root(), &["check", "--deny-warnings", deck]);
        assert_eq!(
            (code, stdout.as_str(), stderr.as_str()),
            (0, "", ""),
            "{deck}"
        );
    }
}

#[test]
fn notebook_lints_are_reported_by_cell() {
    let (code, entries) = check_json(&root(), &["tests/fixtures/ipynb/outputs.ipynb"]);
    assert_eq!(code, 0);
    let found: Vec<(&str, Option<u64>)> = entries
        .iter()
        .map(|e| (e["code"].as_str().unwrap(), e["cell"].as_u64()))
        .collect();
    assert_eq!(
        found,
        [
            ("output-unsupported", Some(11)),
            ("output-widget", Some(12))
        ]
    );
    let (_, entries) = check_json(&root(), &["tests/fixtures/ipynb/minimal.ipynb"]);
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["code"], "notebook-no-outputs");
    assert_eq!(entries[0]["line"], Value::Null);
    let (code, _, stderr) = run(&root(), &["check", "tests/fixtures/ipynb/minimal.ipynb"]);
    assert_eq!(code, 0);
    assert!(
        stderr.starts_with(
            "tests/fixtures/ipynb/minimal.ipynb: warning: the notebook's code cell has no outputs"
        ),
        "{stderr}"
    );
}

/// A notebook with `cells` (JSON objects).
fn notebook(cells: &[Value]) -> String {
    serde_json::json!({"nbformat": 4, "nbformat_minor": 5, "metadata": {}, "cells": cells})
        .to_string()
}

#[test]
fn script_outputs_and_unknown_slide_types_are_reported_by_cell() {
    use serde_json::json;
    let cells = [
        json!({"cell_type": "markdown", "source": "# One",
               "metadata": {"slideshow": {"slide_type": "slidee"}}}),
        json!({"cell_type": "code", "source": "show()", "metadata": {}, "execution_count": 1,
               "outputs": [{"output_type": "display_data", "metadata": {},
                            "data": {"text/html": "<div id=\"x\"></div><script>draw()</script>"}}]}),
    ];
    let dir = project(&[("nb.ipynb", &notebook(&cells))]);
    let (code, entries) = check_json(dir.path(), &["nb.ipynb"]);
    assert_eq!(code, 0);
    let found: Vec<(&str, &str, Option<u64>)> = entries
        .iter()
        .map(|e| {
            (
                e["level"].as_str().unwrap(),
                e["code"].as_str().unwrap(),
                e["cell"].as_u64(),
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            ("warning", "unknown-slide-type", Some(0)),
            ("warning", "output-script", Some(1)),
        ]
    );
    assert_eq!(
        run(dir.path(), &["check", "--deny-warnings", "nb.ipynb"]).0,
        1
    );
}

#[test]
fn split_markdown_cells_are_linted_for_separators() {
    let cells = [serde_json::json!({
        "cell_type": "markdown",
        "metadata": {},
        "source": "# One\n\n---\n\n## Two\n\nSome text\n--\n\nmore\n\n:::include x.md\n:::\n",
    })];
    let nb = notebook(&cells);
    let dir = project(&[
        ("plain/n.ipynb", &nb),
        ("split/n.ipynb", &nb),
        (
            "split/presenter.toml",
            "[notebook]\nsplit_markdown_cells = true\n",
        ),
    ]);
    let found = |path: &str| -> Vec<(String, Option<u64>, Option<u64>)> {
        let (code, entries) = check_json(dir.path(), &[path]);
        assert_eq!(code, 0);
        entries
            .iter()
            .map(|e| {
                (
                    e["code"].as_str().unwrap().to_string(),
                    e["cell"].as_u64(),
                    e["line"].as_u64(),
                )
            })
            .collect()
    };
    let include = ("unknown-directive".to_string(), Some(0), Some(12));
    assert_eq!(found("plain/n.ipynb"), std::slice::from_ref(&include));
    assert_eq!(
        found("split/n.ipynb"),
        [
            ("separator-not-recognized".to_string(), Some(0), Some(8)),
            include
        ]
    );
}

#[test]
fn headings_mode_decks_report_lost_separators() {
    let deck = "---\nslides: {split: headings}\n---\n\n# Section\n\nIntro text.\n\n---\n\n\
                More intro on a new slide.\n\n    ---\n\nThird part\n\n--\nfourth\n\n\
                A title\n---\n\ntext\n";
    let dir = project(&[("h.md", deck)]);
    let (code, entries) = check_json(dir.path(), &["h.md"]);
    assert_eq!(code, 0);
    assert_eq!(
        codes_and_lines(&entries),
        [
            ("separator-not-recognized".to_string(), Some(13)),
            ("separator-not-recognized".to_string(), Some(17)),
        ]
    );
}

#[test]
fn indented_dash_lines_rules_after_link_definitions_and_content_setext_headings_are_reported() {
    // Each of these loses a slide (`dump` shows one slide per deck).
    let indented = "# One\n\nSome text\n    --\n\nmore\n\nOther text\n    ---\n\nend\n\n\
                    tabbed\n\t--\n";
    let definition = "# One\n\ntext\n\n[x]: /u\n---\n\nAfter [x]\n";
    let level_1 = "---\nslides: {split: headings, slide_level: 1}\n---\n\n# A\n\ntext\n\n\
                   Title\n---\n\nmore\n";
    let dir = project(&[
        ("indented.md", indented),
        ("definition.md", definition),
        ("level-1.md", level_1),
    ]);
    let lost = |line: u64| ("separator-not-recognized".to_string(), Some(line));
    let (code, entries) = check_json(dir.path(), &["indented.md"]);
    assert_eq!(code, 0);
    assert_eq!(codes_and_lines(&entries), [lost(4), lost(9), lost(14)]);
    let message = entries[0]["message"].as_str().unwrap();
    assert!(message.contains("remove its indentation"), "{message}");
    let (_, entries) = check_json(dir.path(), &["definition.md"]);
    assert_eq!(codes_and_lines(&entries), [lost(6)]);
    let (_, entries) = check_json(dir.path(), &["level-1.md"]);
    assert_eq!(codes_and_lines(&entries), [lost(10)]);
    // With `--deny-warnings` they fail the check.
    let (code, _, stderr) = run(
        dir.path(),
        &[
            "check",
            "--deny-warnings",
            "indented.md",
            "definition.md",
            "level-1.md",
        ],
    );
    assert_eq!(code, 1, "{stderr}");
    assert_eq!(stderr.matches(": warning: `-").count(), 5, "{stderr}");
}

// ---------------------------------------------------------------------------------------
// Audit fixes (A4)

/// `(code, path, line)` of each JSON entry.
fn codes_paths_and_lines(entries: &[Value]) -> Vec<(String, String, Option<u64>)> {
    entries
        .iter()
        .map(|e| {
            (
                e["code"].as_str().unwrap().to_string(),
                e["path"].as_str().unwrap().to_string(),
                e["line"].as_u64(),
            )
        })
        .collect()
}

/// F10: a deck whose build fails on its theme does not pass `check`: the problems
/// `presenter theme check` reports that a build fails on are errors, located in the theme.
#[test]
fn theme_problems_that_fail_a_build_fail_the_check() {
    let manifest = "name = \"th\"\nextends = \"default\"\n[styles]\nentries = [\"styles/nope.css\"]\n\
                    highlight_style = \"monokia\"\n[scripts]\nentries = [\"scripts/extra.js\"]\n\
                    [reveal]\nplugins = [\"bogus\", \"zoom\"]\n";
    let dir = project(&[
        ("th/theme.toml", manifest),
        (
            "th/templates/layouts/title.html",
            "<section>\n{%- endiff %}\n",
        ),
        ("deck.md", "---\ntheme: ./th\n---\n# One\n"),
        ("ok.md", "# One\n"),
    ]);
    let (code, entries) = check_json(dir.path(), &["deck.md"]);
    assert_eq!(code, 1);
    let theme = "th/theme.toml".to_string();
    assert_eq!(
        codes_paths_and_lines(&entries),
        [
            ("theme-style-entry".into(), theme.clone(), Some(4)),
            ("theme-script-entry".into(), theme.clone(), Some(7)),
            ("theme-highlight-style".into(), theme.clone(), Some(5)),
            (
                "theme-template".into(),
                "th/templates/layouts/title.html".into(),
                Some(2)
            ),
            ("theme-plugins".into(), theme, Some(9)),
        ]
    );
    assert!(entries.iter().all(|e| e["level"] == "error"));
    let (_, _, stderr) = run(dir.path(), &["check", "deck.md"]);
    assert!(
        stderr.starts_with(
            "th/theme.toml:4: error: theme 'th': [styles] entry 'styles/nope.css' is not found"
        ),
        "{stderr}"
    );
    // The build fails on the same theme; a deck with a sound theme passes.
    assert_eq!(run(dir.path(), &["build", "deck.md", "-o", "o.html"]).0, 1);
    assert_eq!(check_json(dir.path(), &["ok.md"]), (0, Vec::new()));

    // A highlight_style typo alone: build says what is wrong (not a deck.html line).
    let dir = project(&[
        (
            "th/theme.toml",
            "name = \"th\"\nextends = \"default\"\n[styles]\nhighlight_style = \"monokia\"\n",
        ),
        ("deck.md", "---\ntheme: ./th\n---\n# One\n"),
    ]);
    let (code, entries) = check_json(dir.path(), &["deck.md"]);
    assert_eq!((code, entries.len()), (1, 1), "{entries:?}");
    let (code, _, stderr) = run(dir.path(), &["build", "deck.md", "-o", "o.html"]);
    assert_eq!(code, 1);
    assert!(
        stderr.starts_with("error: theme 'th': [styles] highlight_style 'monokia'"),
        "{stderr}"
    );
}

/// F40: a near-miss layout name gets a suggestion.
#[test]
fn unknown_layouts_suggest_the_near_miss() {
    let dir = project(&[("deck.md", "# One\n\n---\n\n## Two {layout=titel}\n")]);
    let (code, _, stderr) = run(dir.path(), &["check", "deck.md"]);
    assert_eq!(code, 0);
    assert!(
        stderr.contains("unknown layout 'titel' (theme 'default' has: ")
            && stderr.contains("; did you mean 'title'?); using the default layout"),
        "{stderr}"
    );
}

/// A notebook cell of `kind` with `source` and `metadata`.
fn nb_cell(kind: &str, source: &str, metadata: Value) -> Value {
    let mut cell = serde_json::json!({"cell_type": kind, "metadata": metadata, "source": source});
    if kind == "code" {
        cell["outputs"] = serde_json::json!([]);
        cell["execution_count"] = serde_json::json!(1);
    }
    cell
}

/// F28: a notebook whose cells have no slide types is one slide; `check` (and `build`) say
/// why and how to split it.
#[test]
fn notebooks_without_slide_types_get_a_hint() {
    use serde_json::json;
    let plain = [
        nb_cell("markdown", "# Analysis", json!({})),
        nb_cell("markdown", "Some text", json!({})),
        nb_cell("code", "x = 1", json!({})),
        nb_cell(
            "markdown",
            "Notes",
            json!({"slideshow": {"slide_type": "notes"}}),
        ),
    ];
    let mut typed = plain.clone();
    typed[1]["metadata"] = json!({"slideshow": {"slide_type": "slide"}});
    let dir = project(&[
        ("plain.ipynb", &notebook(&plain)),
        ("typed.ipynb", &notebook(&typed)),
        ("short.ipynb", &notebook(&plain[..2])),
    ]);
    let (code, entries) = check_json(dir.path(), &["plain.ipynb"]);
    assert_eq!(code, 0);
    assert_eq!(
        codes_and_lines(&entries),
        [("notebook-single-slide".to_string(), None)]
    );
    let message = entries[0]["message"].as_str().unwrap();
    assert!(
        message.starts_with(
            "all 3 cells are on one slide because no cell has a slide type (as in `jupyter \
             nbconvert --to slides`)"
        ) && message.contains("Slide Type")
            && message.contains("split_markdown_cells"),
        "{message}"
    );
    assert_eq!(check_json(dir.path(), &["typed.ipynb"]), (0, Vec::new()));
    assert_eq!(check_json(dir.path(), &["short.ipynb"]), (0, Vec::new()));
    let (code, _, stderr) = run(dir.path(), &["build", "plain.ipynb", "-o", "o.html"]);
    assert_eq!(code, 0);
    assert!(
        stderr.contains("warning: all 3 cells are on one slide"),
        "{stderr}"
    );
}

/// F38: a missing file named by an output is reported for that output, not at a line of
/// the cell's code.
#[test]
fn missing_files_of_outputs_name_the_output() {
    use serde_json::json;
    let mut cell = nb_cell("code", "x\ny = 2", json!({}));
    cell["outputs"] = json!([
        {"output_type": "display_data", "metadata": {}, "data": {"text/markdown": "![m](missing.png)"}},
        {"output_type": "display_data", "metadata": {}, "data": {"text/html": "<img src=\"missing2.png\">"}}
    ]);
    let title = nb_cell(
        "markdown",
        "# T",
        json!({"slideshow": {"slide_type": "slide"}}),
    );
    let dir = project(&[("a.ipynb", &notebook(&[title, cell]))]);
    let (code, _, stderr) = run(dir.path(), &["check", "a.ipynb"]);
    assert_eq!(code, 0);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 2, "{stderr}");
    assert!(
        lines[0].starts_with("a.ipynb: warning: cell 1, output 0: image `missing.png` not found"),
        "{stderr}"
    );
    assert!(
        lines[1].starts_with("a.ipynb: warning: cell 1, output 1: file `missing2.png` not found"),
        "{stderr}"
    );
    let (code, _, stderr) = run(dir.path(), &["build", "a.ipynb", "-o", "o.html"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("asset not found: missing.png"), "{stderr}");
    assert!(
        !stderr.contains("| x") && !stderr.contains("line 1"),
        "no line of the code is quoted for an output: {stderr}"
    );
    assert!(
        stderr.contains("cell 1, output 1: file `missing2.png`"),
        "{stderr}"
    );
}

/// F08: files named by outputs (written by executed code) are only embedded from inside the
/// notebook's directory; the author's cells resolve `..` like a Markdown deck.
#[test]
fn outputs_embed_no_files_from_outside_the_notebook_directory() {
    use serde_json::json;
    let mut cell = nb_cell("code", "show()", json!({}));
    cell["outputs"] = json!([
        {"output_type": "display_data", "metadata": {}, "data": {"text/html": "<img src=\"../secret.txt\"><img src=\"fig.png\">"}},
        {"output_type": "display_data", "metadata": {}, "data": {"text/markdown": "![x](../secret.txt) ![y](fig.png)"}}
    ]);
    let title = nb_cell(
        "markdown",
        "# T\n\n![logo](../logo.png)",
        json!({"slideshow": {"slide_type": "slide"}}),
    );
    let dir = project(&[
        ("secret.txt", "SECRET-CONTENT-123\n"),
        ("logo.png", "LOGO-BYTES"),
        ("sub/fig.png", "FIG-BYTES"),
        ("sub/nb.ipynb", &notebook(&[title, cell])),
    ]);
    let (code, entries) = check_json(dir.path(), &["sub/nb.ipynb"]);
    assert_eq!(code, 0);
    let found: Vec<(&str, &str)> = entries
        .iter()
        .map(|e| (e["code"].as_str().unwrap(), e["message"].as_str().unwrap()))
        .collect();
    assert_eq!(found.len(), 2, "{found:?}");
    for (output, (code, message)) in found.iter().enumerate() {
        assert_eq!(*code, "output-file-outside");
        assert!(
            message.starts_with(&format!("cell 1, output {output}: `../secret.txt` names "))
                && message.contains("secret.txt, which is outside the notebook's directory"),
            "{message}"
        );
    }
    let (code, _, stderr) = run(dir.path(), &["build", "sub/nb.ipynb", "-o", "o.html"]);
    assert_eq!(code, 0, "{stderr}");
    let html = std::fs::read_to_string(dir.path().join("o.html")).unwrap();
    // base64 of the secret, the figure and the logo
    assert!(
        !html.contains("U0VDUkVULUNPTlRFTlQtMTIz"),
        "the secret is not embedded"
    );
    assert!(
        html.contains("src=\"../secret.txt\""),
        "the URL stays as written"
    );
    assert_eq!(html.matches("RklHLUJZVEVT").count(), 2, "fig.png, twice");
    assert!(
        html.contains("TE9HTy1CWVRFUw=="),
        "the author's ../logo.png is embedded"
    );
}

/// F43: a notebook saved with a UTF-8 byte order mark is read like any other.
#[test]
fn notebooks_with_a_byte_order_mark_check_and_build() {
    let text = std::fs::read_to_string(root().join("examples/notebook/analysis.ipynb")).unwrap();
    let dir = project(&[("bom.ipynb", &format!("\u{feff}{text}"))]);
    let (code, stdout, stderr) = run(dir.path(), &["check", "--deny-warnings", "bom.ipynb"]);
    assert_eq!((code, stdout.as_str(), stderr.as_str()), (0, "", ""));
    let (code, _, stderr) = run(dir.path(), &["build", "bom.ipynb", "-o", "o.html"]);
    assert_eq!(code, 0, "{stderr}");
}
