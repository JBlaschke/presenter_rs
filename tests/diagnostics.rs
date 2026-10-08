//! Diagnostics quality (T44): how errors are printed on stderr, with their location and the
//! offending source line (`presenter::cli::report`), colors (`NO_COLOR`), and the panic
//! hook.
//!
//! The snapshots are `format_error` output without colors (what `print_error` and a failed
//! command print). The errors come from the real frontends, minijinja and the theme
//! manifest parser where possible; paths are relative so the snapshots do not depend on
//! where the tests run.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
use presenter::cli::report::format_error;
use presenter::config::Config;
use presenter::error::{Error, template_error};
use presenter::ir::{SourceInfo, SourceKind, SourceSpan};
use presenter::theme::ThemeManifest;

/// A source cache with one file.
fn one_file<'a>(name: &'a str, text: &'a str) -> impl Fn(&Path) -> Option<String> + 'a {
    move |path: &Path| (path == Path::new(name)).then(|| text.to_string())
}

/// The error of parsing `text` as `name` (relative) with the frontend for `kind`.
fn frontend_error(name: &str, kind: SourceKind, text: &str) -> Error {
    let source = SourceInfo {
        path: PathBuf::from(name),
        kind,
        base_dir: PathBuf::new(),
    };
    let parsed = match kind {
        SourceKind::Markdown => presenter::markdown::parse(&source, text, &Config::default()),
        SourceKind::Notebook => presenter::notebook::parse(&source, text, &Config::default()),
    };
    match parsed {
        Ok(_) => panic!("{name} must not parse"),
        Err(err) => err,
    }
}

/// `format_error` without colors, reading only `name` with `text`.
fn printed(err: &Error, input: Option<&str>, name: &str, text: &str) -> String {
    format_error(err, input.map(Path::new), &one_file(name, text), false)
}

const DECK: &str = "---\ntitle: Diagnostics\n---\n\n# One\n\n--- {class=\"dark}\n\n# Two\n";

#[test]
fn parse_error_in_markdown() {
    let err = frontend_error("slides.md", SourceKind::Markdown, DECK);
    insta::assert_snapshot!(printed(&err, None, "slides.md", DECK));
}

/// A notebook whose third cell (index 2) has a separator with an unbalanced quote; the
/// first cell's front matter turns on `split_markdown_cells`.
const NOTEBOOK: &str = r###"{"nbformat": 4, "nbformat_minor": 5, "metadata": {}, "cells": [
 {"cell_type": "markdown", "metadata": {},
  "source": ["---\n", "notebook:\n", "  split_markdown_cells: true\n", "---\n", "# Title\n"]},
 {"cell_type": "code", "metadata": {}, "execution_count": 1, "outputs": [], "source": ["x = 1"]},
 {"cell_type": "markdown", "metadata": {}, "source": ["## Two\n", "\n", "--- {title=\"ab}\n", "\n", "more"]}
]}"###;

#[test]
fn parse_error_in_a_notebook_cell() {
    let err = frontend_error("talk.ipynb", SourceKind::Notebook, NOTEBOOK);
    insta::assert_snapshot!(printed(&err, None, "talk.ipynb", NOTEBOOK));
}

#[test]
fn parse_error_in_notebook_json() {
    let json = "{\"cells\": [\n  {\"cell_type\": \"markdown\",, }\n]}\n";
    let err = frontend_error("talk.ipynb", SourceKind::Notebook, json);
    insta::assert_snapshot!(printed(&err, None, "talk.ipynb", json));
}

#[test]
fn missing_asset() {
    // As the pipeline reports it: the resolved path, the span in the deck source, which
    // the CLI knows (`cli::locate`).
    let deck = "# Results\n\n![The plot](figures/plot.png){width=80%}\n";
    let err = Error::MissingAsset {
        path: PathBuf::from("figures/plot.png"),
        span: Some(SourceSpan {
            start_line: 3,
            end_line: 3,
            cell: None,
        }),
    };
    insta::assert_snapshot!(printed(&err, Some("slides.md"), "slides.md", deck));
}

#[test]
fn front_matter_error() {
    let deck = "---\ntitle: Diagnostics: a talk\n---\n\n# One\n";
    let err = frontend_error("slides.md", SourceKind::Markdown, deck);
    insta::assert_snapshot!(printed(&err, Some("slides.md"), "slides.md", deck));
}

/// A child theme's layout with a syntax error on line 3.
const LAYOUT: &str = "{% extends \"layouts/default.html\" %}\n{% block content %}\n\
                      <h1>{{ slide.title </h1>\n{% endblock %}\n";

/// The minijinja error of compiling [`LAYOUT`] in an environment with the debug flag.
fn layout_error(debug: bool) -> minijinja::Error {
    let mut env = minijinja::Environment::new();
    env.set_debug(debug);
    match env.add_template("layouts/title.html", LAYOUT) {
        Ok(()) => panic!("the layout must not compile"),
        Err(err) => err,
    }
}

#[test]
fn template_error_in_serve_mode() {
    // Serve mode renders with minijinja's debug info: its excerpt is in the message.
    let err = template_error(layout_error(true), true);
    insta::assert_snapshot!(printed(&err, None, "", ""));
}

#[test]
fn template_error_in_build_mode() {
    // `build` renders without debug info: the template's name and line.
    let err = template_error(layout_error(false), false);
    insta::assert_snapshot!(printed(&err, None, "", ""));
}

#[test]
fn theme_error() {
    let manifest = "name = \"talk\"\nextends = \"default\"\nnme = 3\n";
    let err = match ThemeManifest::from_toml_str(manifest, "./theme", "theme/theme.toml") {
        Ok(_) => panic!("an unknown key must be an error"),
        Err(err) => err,
    };
    insta::assert_snapshot!(printed(&err, None, "", ""));
}

/// The binary, without the developer's color and logging variables.
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

/// Runs `presenter build <deck>` in `dir` with `env`; returns stderr (the build must fail).
fn failed_build(dir: &Path, deck: &str, env: &[(&str, &str)]) -> String {
    let mut cmd = presenter(dir);
    cmd.args(["build", deck, "-o", "out.html"]);
    for (key, value) in env {
        cmd.env(key, value);
    }
    let output = cmd.assert().code(1).stdout("").get_output().clone();
    String::from_utf8(output.stderr).unwrap()
}

#[test]
fn a_failed_build_prints_the_location_and_the_line() {
    let dir = project(&[
        ("slides.md", DECK),
        ("missing.md", "# One\n\n![x](img/nope.png)\n"),
    ]);
    assert_eq!(
        failed_build(dir.path(), "slides.md", &[]),
        "error: unbalanced \" quote in attribute block {class=\"dark}\n --> slides.md:7\n  |\n\
         7 | --- {class=\"dark}\n  | ^^^^^^^^^^^^^^^^^\n"
    );
    // The location `cli::locate` adds to a missing asset.
    assert_eq!(
        failed_build(dir.path(), "missing.md", &[]),
        "error: asset not found: img/nope.png\n --> missing.md:3\n  |\n\
         3 | ![x](img/nope.png)\n  |      ^^^^^^^^^^^^\n"
    );
}

#[test]
fn no_color_turns_colors_off() {
    let dir = project(&[("slides.md", DECK)]);
    // Not a terminal: no colors.
    let piped = failed_build(dir.path(), "slides.md", &[]);
    assert!(!piped.contains("\x1b["), "{piped:?}");
    // CLICOLOR_FORCE colors a pipe too ...
    let forced = failed_build(dir.path(), "slides.md", &[("CLICOLOR_FORCE", "1")]);
    assert!(forced.starts_with("\x1b[1;31merror\x1b[0m: "), "{forced:?}");
    assert!(
        forced.contains("\x1b[1;34m-->\x1b[0m slides.md:7"),
        "{forced:?}"
    );
    // ... unless NO_COLOR is set: same text, no escape sequences.
    let no_color = failed_build(
        dir.path(),
        "slides.md",
        &[("CLICOLOR_FORCE", "1"), ("NO_COLOR", "1")],
    );
    assert!(!no_color.contains("\x1b["), "{no_color:?}");
    assert_eq!(no_color, piped);
}

/// Panics after installing presenter's panic hook when `PRESENTER_TEST_PANIC` is set (run
/// as a child process by [`panics_print_a_bug_report_line`]); passes otherwise.
#[test]
fn panic_child() {
    if std::env::var_os("PRESENTER_TEST_PANIC").is_none() {
        return;
    }
    presenter::cli::report::install_panic_hook();
    panic!("boom");
}

/// The stderr of [`panic_child`] run in a child process of this test binary.
fn child_panic(backtrace: Option<&str>) -> String {
    let mut cmd = StdCommand::new(std::env::current_exe().unwrap());
    cmd.args([
        "panic_child",
        "--exact",
        "--nocapture",
        "--test-threads",
        "1",
    ])
    .env("PRESENTER_TEST_PANIC", "1")
    .env_remove("NO_COLOR")
    .env_remove("CLICOLOR_FORCE")
    .env_remove("RUST_BACKTRACE");
    if let Some(value) = backtrace {
        cmd.env("RUST_BACKTRACE", value);
    }
    let output = cmd.output().unwrap();
    assert!(!output.status.success(), "the child test panics");
    String::from_utf8(output.stderr).unwrap()
}

#[test]
fn panics_print_a_bug_report_line() {
    let stderr = child_panic(None);
    let error = stderr
        .lines()
        .find(|line| line.starts_with("error: "))
        .unwrap_or_else(|| panic!("no error line: {stderr}"));
    // The location of the `panic!` in `panic_child`.
    assert!(
        error.starts_with("error: internal error at tests/diagnostics.rs:")
            && error.ends_with(": boom"),
        "{stderr}"
    );
    // `presenter --version`'s text names the build.
    let note = format!(
        "\nnote: this is a bug in presenter {}; please report it with the command line you \
         ran (RUST_BACKTRACE=1 shows a backtrace)\n",
        presenter::cli::version()
    );
    assert!(stderr.contains(&note), "{stderr}");
    // The standard message is replaced.
    assert!(!stderr.contains("panicked at"), "{stderr}");

    // RUST_BACKTRACE keeps the standard message and its backtrace after the report.
    let stderr = child_panic(Some("1"));
    let report = stderr.find("error: internal error at ").unwrap();
    let standard = stderr.find("panicked at").unwrap();
    assert!(report < standard, "{stderr}");
    assert!(stderr.contains("stack backtrace:"), "{stderr}");
    assert!(!stderr.contains("RUST_BACKTRACE=1 shows"), "{stderr}");
}
