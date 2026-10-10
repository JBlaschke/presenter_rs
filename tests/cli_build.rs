//! T16 end to end: the `presenter` binary's `build` and `dump` commands on the example deck
//! (`examples/basic`), exit codes and messages of `docs/spec/cli.md`, site mode, and an
//! insta snapshot of the built example's `<div class="slides">` region.
//!
//! Every build writes into a temporary directory (`-o`, or a copy of the example), never
//! into the source tree.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::str::contains;
use presenter::ir::Deck;
use regex::Regex;
use serde_json::Value;

/// Top-level `<section>`s of the example: 15 horizontal slides (one is a vertical stack).
const EXAMPLE_TOP_LEVEL_SECTIONS: usize = 15;
/// All slides of the example, vertical children included.
const EXAMPLE_SLIDES: usize = 17;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The binary, run from the crate root, without the developer's `RUST_LOG` / `NO_COLOR` /
/// `CLICOLOR_FORCE` (stderr assertions must not depend on them).
fn presenter() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_presenter"));
    cmd.current_dir(root())
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

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("UTF-8 stderr")
}

fn stdout_of(cmd: &mut Command) -> String {
    let output = cmd.assert().success().get_output().clone();
    String::from_utf8(output.stdout).expect("UTF-8 stdout")
}

/// A copy of `examples/basic` (deck, config, figures) in `dir/<name>`.
fn copy_example(dir: &Path, name: &str) -> PathBuf {
    let src = root().join("examples/basic");
    let dst = dir.join(name);
    std::fs::create_dir_all(dst.join("figures")).unwrap();
    for file in [
        "slides.md",
        "presenter.toml",
        "figures/plot.png",
        "figures/embed.html",
    ] {
        std::fs::copy(src.join(file), dst.join(file)).unwrap();
    }
    dst
}

/// The `<div class="slides">…</div>` element of a document.
fn slides_region(html: &str) -> &str {
    let start = html.find(r#"<div class="slides">"#).expect("slides div");
    let tags = Regex::new(r"<div\b|</div>").unwrap();
    let mut depth = 0usize;
    for m in tags.find_iter(&html[start..]) {
        if m.as_str() == "</div>" {
            depth -= 1;
            if depth == 0 {
                return &html[start..start + m.end()];
            }
        } else {
            depth += 1;
        }
    }
    panic!("unclosed slides div");
}

/// Number of `<section>` elements directly inside the slides region.
fn top_level_sections(region: &str) -> usize {
    let tags = Regex::new(r"<section\b|</section>").unwrap();
    let mut depth = 0usize;
    let mut count = 0;
    for m in tags.find_iter(region) {
        if m.as_str() == "</section>" {
            depth -= 1;
        } else {
            if depth == 0 {
                count += 1;
            }
            depth += 1;
        }
    }
    assert_eq!(depth, 0, "unbalanced sections");
    count
}

/// Asserts the `Wrote <path> (<n> slides, <size>)` line.
fn assert_wrote(stdout: &str, path_suffix: &str) {
    let line = Regex::new(&format!(
        r"^Wrote (.+) \({EXAMPLE_SLIDES} slides, \d+(\.\d)? (B|kB|MB)\)\n$"
    ))
    .unwrap();
    let caps = line
        .captures(stdout)
        .unwrap_or_else(|| panic!("unexpected stdout: {stdout:?}"));
    // (A relative path is `/`-separated everywhere; an absolute one, shown when the output
    // is on another drive than the current directory on Windows, is native.)
    assert!(
        caps[1].replace('\\', "/").ends_with(path_suffix),
        "{stdout}"
    );
}

#[test]
fn builds_the_example_into_one_self_contained_file() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("slides.html");
    let output = presenter()
        .args(["build", "examples/basic/slides.md", "-o"])
        .arg(&out)
        .assert()
        .success()
        .stderr("")
        .get_output()
        .clone();
    assert_wrote(&String::from_utf8(output.stdout).unwrap(), "slides.html");

    let html = std::fs::read_to_string(&out).expect("output written");
    let region = slides_region(&html);
    assert_eq!(top_level_sections(region), EXAMPLE_TOP_LEVEL_SECTIONS);
    assert!(html.contains("Reveal.initialize("));
    assert!(!html.contains("asset:"), "unresolved asset placeholder");
    assert!(!html.contains("/__presenter/"));
    // Remote references only come from the deck's own content (the footnote links).
    let remote = Regex::new(r#"(?:href|src)="(?:https?:)?//"#).unwrap();
    let start = html.find(region).unwrap();
    for m in remote.find_iter(&html) {
        assert!(
            (start..start + region.len()).contains(&m.start()),
            "remote reference outside the slides: {}",
            &html[m.start()..(m.end() + 40).min(html.len())]
        );
    }
    assert_eq!(
        remote.find_iter(region).count(),
        2,
        "the two footnote links"
    );
    assert!(
        html.contains(r#"<img src="data:image/png;base64,"#),
        "image inlined"
    );
    assert!(html.contains(r#"<iframe src="data:text/html;base64,"#));
    assert!(html.contains("katex"), "KaTeX included for the math slide");

    let mut settings = insta::Settings::clone_current();
    settings.add_filter(r#"data:[^"]+"#, "[data-uri]");
    settings.bind(|| insta::assert_snapshot!("example_slides_region", region));
}

#[test]
fn builds_next_to_the_input_without_out() {
    let tmp = tempfile::tempdir().unwrap();
    copy_example(tmp.path(), "talk");
    let stdout = stdout_of(
        presenter()
            .current_dir(tmp.path())
            .args(["build", "talk/slides.md"]),
    );
    assert_wrote(&stdout, "talk/dist/slides.html");
    assert!(tmp.path().join("talk/dist/slides.html").is_file());

    // --site without -o: <out_dir>/<stem>/index.html
    let stdout =
        stdout_of(
            presenter()
                .current_dir(tmp.path())
                .args(["build", "talk/slides.md", "--site"]),
        );
    assert_wrote(&stdout, "talk/dist/slides/index.html");
    assert!(tmp.path().join("talk/dist/slides/assets").is_dir());
}

#[test]
fn site_mode_writes_index_and_relative_assets() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("site");
    let stdout = stdout_of(
        presenter()
            .args(["build", "examples/basic/slides.md", "--site", "-o"])
            .arg(&out),
    );
    assert_wrote(&stdout, "site/index.html");

    let index = std::fs::read_to_string(out.join("index.html")).expect("index.html");
    let assets = out.join("assets");
    let names: Vec<String> = std::fs::read_dir(&assets)
        .expect("assets/")
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    let has = |prefix: &str, ext: &str| {
        names
            .iter()
            .any(|n| n.starts_with(prefix) && n.ends_with(ext))
    };
    assert!(has("theme-", ".css"), "theme stylesheet in {names:?}");
    assert!(has("plot-", ".png"), "deck image in {names:?}");
    assert!(has("embed-", ".html"), "iframe page in {names:?}");
    assert!(has("reveal-", ".js"), "reveal.js in {names:?}");

    // Every reference to an emitted file is relative and resolves inside the site.
    let refs = Regex::new(r#"(?:href|src|data-background-image)="(assets/[^"]+)""#).unwrap();
    let mut referenced = 0;
    for caps in refs.captures_iter(&index) {
        assert!(out.join(&caps[1]).is_file(), "{} is missing", &caps[1]);
        referenced += 1;
    }
    assert!(referenced >= 6, "only {referenced} relative references");
    assert!(
        Regex::new(
            r#"<link id="presenter-theme" rel="stylesheet" href="assets/theme-[0-9a-f]{12}\.css">"#
        )
        .unwrap()
        .is_match(&index)
    );
    assert!(index.contains(r#"<img src="assets/plot-"#));
    assert!(
        !index.contains("data:image/png"),
        "site mode does not inline"
    );
    assert!(!index.contains("/__presenter/"));
    assert!(
        !index.contains(&*tmp.path().to_string_lossy()),
        "no absolute paths"
    );
    assert!(!index.contains("asset:"), "unresolved asset placeholder");
    assert_eq!(
        top_level_sections(slides_region(&index)),
        EXAMPLE_TOP_LEVEL_SECTIONS
    );
}

#[test]
fn missing_input_is_a_build_error_naming_the_path() {
    presenter()
        .args(["build", "missing.md"])
        .assert()
        .code(1)
        .stdout("")
        .stderr(contains("missing.md"));
}

#[test]
fn unknown_theme_is_a_build_error() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("x.html");
    presenter()
        .args(["build", "examples/basic/slides.md", "--theme", "nope", "-o"])
        .arg(&out)
        .assert()
        .code(1)
        .stdout("")
        .stderr(contains("error: theme 'nope': not found"));
    assert!(!out.exists());
}

#[test]
fn missing_asset_is_a_build_error_with_its_location() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("deck.md"), "# One\n\n![x](nope.png)\n").unwrap();
    presenter()
        .current_dir(tmp.path())
        .args(["build", "deck.md"])
        .assert()
        .code(1)
        .stderr(contains(
            "error: asset not found: nope.png\n --> deck.md:3\n  |\n3 | ![x](nope.png)\n",
        ));
    assert!(!tmp.path().join("dist").exists());
}

#[test]
fn warnings_go_to_stderr_as_path_line_level_message() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("deck.md"),
        "# One\n\n---\n\n---\n\n# Three\n",
    )
    .unwrap();
    let output = presenter()
        .current_dir(tmp.path())
        .args(["build", "deck.md", "-o", "out.html"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        Regex::new(r"^deck\.md:\d+: warning: .+\n$")
            .unwrap()
            .is_match(&stderr),
        "{stderr:?}"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("Wrote out.html (3 slides, "), "{stdout}");

    // -q: no warnings, no summary.
    presenter()
        .current_dir(tmp.path())
        .args(["-q", "build", "deck.md", "-o", "out.html"])
        .assert()
        .success()
        .stdout("")
        .stderr("");
}

#[test]
fn dump_prints_the_deck_ir_with_sorted_keys() {
    let stdout = stdout_of(presenter().args(["dump", "examples/basic/slides.md"]));
    let deck: Deck = serde_json::from_str(&stdout).expect("JSON that deserializes to Deck");
    assert_eq!(deck.slides.len(), EXAMPLE_TOP_LEVEL_SECTIONS);
    assert_eq!(deck.slides[2].children.len(), 2);
    assert!(deck.features.math && deck.features.code && deck.features.notes);
    assert_eq!(deck.meta.title.as_deref(), Some("Presenter basics"));
    assert_eq!(deck.assets.len(), 2);
    assert!(deck.invariant_violations().is_empty());

    let value: Value = serde_json::from_str(&stdout).unwrap();
    let keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["assets", "features", "meta", "slides", "source"]);
    assert!(stdout.lines().count() > 1, "pretty-printed");

    let compact = stdout_of(presenter().args(["dump", "examples/basic/slides.md", "--compact"]));
    assert_eq!(compact.lines().count(), 1);
    assert_eq!(serde_json::from_str::<Value>(&compact).unwrap(), value);
}

#[test]
fn dump_context_prints_the_template_context() {
    let stdout = stdout_of(presenter().args(["dump", "examples/basic/slides.md", "--context"]));
    let value: Value = serde_json::from_str(&stdout).expect("valid JSON");
    let keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "deck", "meta", "slides", "features", "config", "reveal", "theme", "build"
        ]
    );
    assert_eq!(value["theme"]["name"], "default");
    assert_eq!(value["config"]["reveal"]["slideNumber"], "c/t");
    assert_eq!(value["build"]["mode"], "build");
    assert_eq!(value["slides"][0]["layout_template"], "layouts/title.html");
    serde_json::from_value::<Deck>(value["deck"].clone()).expect("context.deck is a Deck");

    // --theme applies to the context too.
    let child = root().join("tests/fixtures/themes/child-theme");
    let stdout = stdout_of(
        presenter()
            .args(["dump", "examples/basic/slides.md", "--context", "--theme"])
            .arg(&child),
    );
    let value: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        value["theme"]["chain"],
        serde_json::json!(["child-theme", "default"])
    );
}

#[test]
fn format_flag_overrides_the_extension() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("deck.txt"), "# One\n").unwrap();
    presenter()
        .current_dir(tmp.path())
        .args(["build", "deck.txt", "-o", "out.html"])
        .assert()
        .code(2)
        .stderr(contains("--format"));
    presenter()
        .current_dir(tmp.path())
        .args(["build", "deck.txt", "--format", "md", "-o", "out.html"])
        .assert()
        .success()
        .stdout(contains("Wrote out.html (1 slide, "));
}

#[test]
fn usage_and_config_errors_exit_2() {
    presenter().arg("build").assert().code(2).stdout("");
    presenter()
        .args(["build", "a.md", "--site", "--standalone"])
        .assert()
        .code(2);
    presenter()
        .args([
            "--config",
            "no/such/presenter.toml",
            "build",
            "examples/basic/slides.md",
        ])
        .assert()
        .code(2)
        .stderr(contains("no/such/presenter.toml"));
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("presenter.toml"),
        "[build]\nout_dri = \"x\"\n",
    )
    .unwrap();
    std::fs::write(tmp.path().join("deck.md"), "# One\n").unwrap();
    presenter()
        .current_dir(tmp.path())
        .args(["build", "deck.md"])
        .assert()
        .code(2)
        .stderr(contains("did you mean 'out_dir'"));
}

#[test]
fn failed_builds_print_the_warnings_then_every_missing_asset_in_source_order() {
    let tmp = project(&[(
        "two.md",
        "# A {bogus=1}\n\n![a](a1.png)\n\n---\n\n---\n\n![b](b2.png)\n",
    )]);
    let output = presenter()
        .current_dir(tmp.path())
        .args(["build", "two.md", "-o", "two.html"])
        .assert()
        .code(1)
        .stdout("")
        .get_output()
        .clone();
    let stderr = stderr_of(&output);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 12, "{stderr}");
    assert!(lines[0].starts_with("two.md:1: warning: "), "{stderr}");
    assert!(lines[0].contains("bogus"), "{stderr}");
    assert!(lines[1].starts_with("two.md:5: warning: "), "{stderr}");
    // Audit F37: every missing asset in the same form, in source order.
    assert_eq!(
        lines[2..],
        [
            "error: asset not found: a1.png",
            " --> two.md:3",
            "  |",
            "3 | ![a](a1.png)",
            "  |      ^^^^^^",
            "error: asset not found: b2.png",
            " --> two.md:9",
            "  |",
            "9 | ![b](b2.png)",
            "  |      ^^^^^^"
        ],
        "{stderr}"
    );
    assert!(!tmp.path().join("two.html").exists());
}

#[test]
fn missing_assets_of_every_kind_are_reported_in_source_order() {
    // The finding's repro: an image, an <img> and a slide background, none of which
    // exist; the frontend reports them in different passes.
    let tmp = project(&[(
        "img.md",
        "# Title\n\n---\n\n![](figures/plot.png)\n\n<img src=\"figures/raw.png\">\n\n\
         --- {background=figures/bg.jpg}\n\n## Background\n",
    )]);
    let output = presenter()
        .current_dir(tmp.path())
        .args(["build", "img.md"])
        .assert()
        .code(1)
        .stdout("")
        .get_output()
        .clone();
    let stderr = stderr_of(&output);
    let located: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with(" --> "))
        .collect();
    assert_eq!(
        located,
        [" --> img.md:5", " --> img.md:7", " --> img.md:9"],
        "{stderr}"
    );
    let errors: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("error: "))
        .collect();
    assert_eq!(errors.len(), 3, "{stderr}");
    assert!(
        !stderr.contains("img.md:7: error:"),
        "no one-line form: {stderr}"
    );
}

#[test]
fn rejected_front_matter_prints_the_warnings_first() {
    let notebook = r#"{"nbformat": 4, "nbformat_minor": 5, "metadata": METADATA, "cells": [
        {"cell_type": "markdown", "metadata": {}, "source": FIRST},
        {"cell_type": "raw", "metadata": {"raw_mimetype": "text/latex"}, "source": "x"}]}"#;
    let notebook = |metadata: &str, first: &str| {
        notebook
            .replace("METADATA", metadata)
            .replace("FIRST", first)
    };
    let plugins_nb = notebook(
        r#"{"presenter": {"reveal": {"plugins": ["bogus"]}}}"#,
        r##""# One""##,
    );
    let front_nb = notebook("{}", r##""---\nmath:\n  enable: never\n---\n# One""##);
    let tmp = project(&[
        (
            "fp.md",
            "---\nreveal:\n  plugins: [bogus]\n---\n# One\n\n---\n\n---\n\n# Three\n",
        ),
        (
            "mt.md",
            "---\nmath:\n  enable: never\n---\n# One\n\n---\n\n---\n\n# Three\n",
        ),
        (
            "nbt.md",
            "---\nnotebook: {hide_inputs: []}\n---\n# One\n\n---\n\n---\n\n# Three\n",
        ),
        ("fp.ipynb", &plugins_nb),
        ("fm.ipynb", &front_nb),
    ]);
    let raw_skipped = "cell 1, line 1: warning: raw cell skipped: its raw_mimetype is `text/latex`; \
                       only `text/html` raw cells are shown";
    let cases = [
        (
            "fp.md",
            "fp.md:7: warning: slide 2 is empty".to_owned(),
            "front matter: unknown reveal plugin 'bogus'",
        ),
        (
            "mt.md",
            "mt.md:7: warning: slide 2 is empty".to_owned(),
            "front matter: math: unknown field `enable`",
        ),
        (
            "nbt.md",
            "nbt.md:6: warning: slide 2 is empty".to_owned(),
            "front matter: notebook: unknown field `hide_inputs`",
        ),
        (
            "fp.ipynb",
            format!("fp.ipynb: {raw_skipped}"),
            "front matter: metadata.presenter: unknown reveal plugin 'bogus'",
        ),
        (
            "fm.ipynb",
            format!("fm.ipynb: {raw_skipped}"),
            "front matter: cell 0: math: unknown field `enable`",
        ),
    ];
    for (deck, warning, error) in cases {
        let out = format!("{deck}.html");
        let commands: [&[&str]; 3] = [
            &["build", deck, "-o", &out],
            &["dump", deck],
            &["dump", deck, "--context"],
        ];
        for args in commands {
            let output = presenter()
                .current_dir(tmp.path())
                .args(args)
                .assert()
                .code(1)
                .stdout("")
                .get_output()
                .clone();
            let stderr = stderr_of(&output);
            let lines: Vec<&str> = stderr.lines().collect();
            assert_eq!(lines.len(), 3, "{args:?}: {stderr}");
            assert_eq!(lines[0], warning, "{args:?}");
            assert!(
                lines[1].starts_with(&format!("error: {error}")),
                "{args:?}: {stderr}"
            );
            assert_eq!(lines[2], format!(" --> {deck}"), "{args:?}");
        }
        assert!(!tmp.path().join(&out).exists());
    }
}

#[test]
fn missing_image_in_markdown_filter_input_fails_the_build() {
    let layout = "{%- extends \"layouts/default.html\" %}{%- block content %}\
                  <p class=\"subtitle\">{{ meta.subtitle|markdown }}</p>{%- endblock %}";
    let tmp = project(&[
        ("t/theme.toml", "name = \"t\"\nextends = \"default\"\n"),
        ("t/templates/layouts/title.html", layout),
        (
            "sub.md",
            "---\ntheme: t\nsubtitle: \"![logo](nologo.png) hi\"\n---\n# Title {layout=title}\n",
        ),
    ]);
    presenter()
        .current_dir(tmp.path())
        .args(["build", "sub.md", "-o", "sub.html"])
        .assert()
        .code(1)
        .stdout("")
        .stderr(contains(
            "error: asset not found: nologo.png\n --> sub.md\n",
        ));
    assert!(!tmp.path().join("sub.html").exists());

    // dump --context does not render the templates, so the filter never runs: no error.
    let output = presenter()
        .current_dir(tmp.path())
        .args(["dump", "sub.md", "--context"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = stderr_of(&output);
    assert!(stderr.is_empty(), "the context is not rendered: {stderr}");
}

#[test]
fn bad_plugins_in_a_theme_are_theme_errors() {
    let tmp = project(&[
        (
            "bt/theme.toml",
            "name = \"bt\"\nextends = \"default\"\n[reveal]\nplugins = [\"highlight\", \"bogus\"]\n",
        ),
        ("p.md", "# One\n"),
    ]);
    presenter()
        .current_dir(tmp.path())
        .args(["build", "p.md", "--theme", "./bt", "-o", "p.html"])
        .assert()
        .code(1)
        .stderr(contains(
            "error: theme 'bt': theme.toml [reveal]: unknown reveal plugin 'bogus'",
        ));
}

#[test]
fn out_dir_is_relative_to_the_presenter_toml() {
    let tmp = project(&[
        ("proj/presenter.toml", "[build]\nout_dir = \"public\"\n"),
        ("proj/talks/one/a.md", "# One\n"),
    ]);
    let proj = tmp.path().join("proj");
    let stdout = stdout_of(
        presenter()
            .current_dir(&proj)
            .args(["build", "talks/one/a.md"]),
    );
    assert!(
        stdout.starts_with("Wrote public/a.html (1 slide, "),
        "{stdout}"
    );
    assert!(proj.join("public/a.html").is_file());
    assert!(!proj.join("talks/one/public").exists());

    // --config from elsewhere: still relative to that file.
    let stdout = stdout_of(
        presenter()
            .current_dir(tmp.path())
            .args([
                "--config",
                "proj/presenter.toml",
                "build",
                "proj/talks/one/a.md",
            ])
            .arg("--site"),
    );
    assert!(
        stdout.starts_with("Wrote proj/public/a/index.html (1 slide, "),
        "{stdout}"
    );
}

#[test]
fn dump_errors_name_the_input_and_suggest_no_missing_flag() {
    let tmp = project(&[
        ("deck.txt", "# One\n"),
        ("fm.md", "---\ntitle: [1, 2]\n---\n# One\n"),
    ]);
    let output = presenter()
        .current_dir(tmp.path())
        .args(["dump", "deck.txt"])
        .assert()
        .code(2)
        .stdout("")
        .get_output()
        .clone();
    let stderr = stderr_of(&output);
    assert!(stderr.contains("deck.txt"), "{stderr}");
    assert!(!stderr.contains("--format"), "{stderr}");

    for args in [&["dump", "fm.md"][..], &["dump", "fm.md", "--context"][..]] {
        presenter()
            .current_dir(tmp.path())
            .args(args)
            .assert()
            .code(1)
            .stderr(contains("error: front matter: "))
            .stderr(contains("\n --> fm.md:2:8\n"));
    }
}

#[test]
fn rust_log_adds_to_the_verbosity_flags() {
    let tmp = project(&[("a.md", "# One\n")]);
    let output = presenter()
        .current_dir(tmp.path())
        .env("RUST_LOG", "presenter=debug")
        .args(["-v", "build", "a.md", "-o", "a.html"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("DEBUG"),
        "RUST_LOG applies with -v: {stderr}"
    );
    assert!(stderr.contains("INFO"), "{stderr}");

    // A bare RUST_LOG level: -v and -vv never hide presenter's own debug lines.
    for flags in [&[][..], &["-v"][..], &["-vv"][..]] {
        let output = presenter()
            .current_dir(tmp.path())
            .env("RUST_LOG", "debug")
            .args(flags)
            .args(["build", "a.md", "-o", "a.html"])
            .assert()
            .success()
            .get_output()
            .clone();
        let stderr = stderr_of(&output);
        for line in ["DEBUG parsed ", "DEBUG rendered "] {
            assert!(stderr.contains(line), "{flags:?}: no `{line}`: {stderr}");
        }
    }

    // -q ignores RUST_LOG.
    presenter()
        .current_dir(tmp.path())
        .env("RUST_LOG", "presenter=debug")
        .args(["-q", "build", "a.md", "-o", "a.html"])
        .assert()
        .success()
        .stdout("")
        .stderr("");
}

/// T47: `presenter --version` names the exact build, `presenter <version> (<build id>)`,
/// where the build id is what `build.rs` embeds: `git describe --always --dirty` output
/// (`33a4607`, `v0.1.0`, `v0.1.0-3-g33a4607`, each optionally `-dirty`), `unknown` without
/// git, or the `PRESENTER_GIT_SHA` override, in the charset `tests/cli_version.rs` accepts.
#[test]
fn version_names_the_build() {
    let line = Regex::new(r"^presenter (\d+\.\d+\.\d+\S*) \(([0-9A-Za-z.+-]+)\)\n$").unwrap();
    let build_id = env!("PRESENTER_GIT_SHA");
    for flag in ["--version", "-V"] {
        let stdout = stdout_of(presenter().arg(flag));
        let caps = line
            .captures(&stdout)
            .unwrap_or_else(|| panic!("{flag}: {stdout:?}"));
        assert_eq!(&caps[1], env!("CARGO_PKG_VERSION"));
        assert_eq!(&caps[2], build_id);
    }

    // build.rs is wired to git: when git describes this checkout, the binary names HEAD
    // (`cargo test` rebuilt it if HEAD moved). Cargo also exports the build script's
    // `rustc-env` variables to test processes, so the runtime environment cannot tell an
    // override from a described build; the script's PRESENTER_BUILD_ID_SOURCE can.
    let source = env!("PRESENTER_BUILD_ID_SOURCE");
    match git_in_root(&["describe", "--always"]) {
        _ if source == "override" => {}
        Some(described) => {
            assert_eq!(
                source, "git",
                "git describes the checkout as {described}, but the build id is {build_id}"
            );
            // Edits outside src/ do not rerun build.rs, so `-dirty` may lag behind; and the
            // abbreviation may have grown since the build. Compare commits, unless the id
            // does not name one (a tag name with characters build.rs replaced).
            let clean = build_id.strip_suffix("-dirty").unwrap_or(build_id);
            let commit = |rev: &str| git_in_root(&["rev-parse", "--verify", "-q", rev]);
            match (commit(&format!("{clean}^{{commit}}")), commit("HEAD")) {
                (Some(built), Some(head)) => assert_eq!(built, head, "{build_id} is not HEAD"),
                _ => {
                    let charset = |c: char| c.is_ascii_alphanumeric() || "+.-".contains(c);
                    assert_eq!(clean, described.replace(|c| !charset(c), "-"));
                }
            }
        }
        None => eprintln!("git does not describe {}; skipped", root().display()),
    }
}

/// Runs git in the crate directory the way `build.rs` does (no search in parent
/// directories, no caller `GIT_DIR`); its trimmed stdout, or `None` when git is missing,
/// fails or prints nothing.
fn git_in_root(args: &[&str]) -> Option<String> {
    let root = root();
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(&root)
        .env("GIT_CEILING_DIRECTORIES", root.parent()?)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .ok()?;
    let stdout = String::from_utf8(output.stdout).ok()?;
    let stdout = stdout.trim();
    (output.status.success() && !stdout.is_empty()).then(|| stdout.to_string())
}

// --- Audit fixes (A1): output safety, out_dir, messages --------------------------------

/// The bytes of each of `files` (relative to `dir`).
fn snapshot(dir: &Path, files: &[&str]) -> Vec<(String, Vec<u8>)> {
    files
        .iter()
        .map(|file| (file.to_string(), std::fs::read(dir.join(file)).unwrap()))
        .collect()
}

/// Runs `presenter <args>` in `dir` and asserts the audit F01 refusal: exit 2, nothing on
/// stdout, one `error: refusing to write …` line on stderr, and every file of `protected`
/// unchanged. Returns stderr.
fn assert_refused(dir: &Path, args: &[&str], protected: &[&str]) -> String {
    let before = snapshot(dir, protected);
    let output = presenter()
        .current_dir(dir)
        .args(args)
        .timeout(std::time::Duration::from_secs(60))
        .assert()
        .code(2)
        .stdout("")
        .get_output()
        .clone();
    let stderr = stderr_of(&output);
    assert!(stderr.starts_with("error: refusing to write "), "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert_eq!(
        snapshot(dir, protected),
        before,
        "{args:?} changed an input"
    );
    stderr
}

#[test]
fn build_refuses_to_overwrite_its_input() {
    let tmp = project(&[("slides.md", "# My talk\n\nprecious content\n")]);
    let stderr = assert_refused(
        tmp.path(),
        &["build", "slides.md", "-o", "slides.md"],
        &["slides.md"],
    );
    assert_eq!(
        stderr,
        "error: refusing to write slides.md: it is the input file slides.md; choose another \
         output path with -o\n"
    );
    // Other spellings of the same path, and -q, which hides no error.
    assert_refused(
        tmp.path(),
        &[
            "-q",
            "build",
            "./slides.md",
            "-o",
            "sub/../slides.md",
            "--standalone",
        ],
        &["slides.md"],
    );
    let absolute = tmp.path().join("slides.md");
    assert_refused(
        tmp.path(),
        &["build", "slides.md", "-o", absolute.to_str().unwrap()],
        &["slides.md"],
    );
    assert_eq!(
        std::fs::read_dir(tmp.path()).unwrap().count(),
        1,
        "nothing else was written"
    );
}

#[test]
fn build_refuses_to_overwrite_a_notebook() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::copy(
        root().join("tests/fixtures/ipynb/minimal.ipynb"),
        tmp.path().join("nb.ipynb"),
    )
    .unwrap();
    let stderr = assert_refused(
        tmp.path(),
        &["build", "nb.ipynb", "-o", "nb.ipynb"],
        &["nb.ipynb"],
    );
    assert!(stderr.contains("it is the input file nb.ipynb"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn build_refuses_links_to_its_input() {
    let tmp = project(&[("deck.md", "# Deck\n")]);
    std::os::unix::fs::symlink("deck.md", tmp.path().join("alias.html")).unwrap();
    std::fs::hard_link(tmp.path().join("deck.md"), tmp.path().join("hard.html")).unwrap();
    let stderr = assert_refused(
        tmp.path(),
        &["build", "deck.md", "-o", "alias.html"],
        &["deck.md"],
    );
    assert_eq!(
        stderr,
        "error: refusing to write alias.html: it is the input file deck.md; choose another \
         output path with -o\n"
    );
    assert!(
        std::fs::symlink_metadata(tmp.path().join("alias.html"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let stderr = assert_refused(
        tmp.path(),
        &["build", "deck.md", "-o", "hard.html"],
        &["deck.md", "hard.html"],
    );
    assert!(
        stderr.contains("hard.html: it is the input file deck.md"),
        "{stderr}"
    );
    // The input given through a link, the output by its real name.
    let stderr = assert_refused(
        tmp.path(),
        &["build", "alias.html", "--format", "md", "-o", "deck.md"],
        &["deck.md"],
    );
    assert!(
        stderr.contains("deck.md: it is the input file alias.html"),
        "{stderr}"
    );
}

#[test]
fn build_refuses_another_spelling_of_its_input_on_a_case_insensitive_file_system() {
    let tmp = project(&[("s.md", "# Small\n")]);
    if !tmp.path().join("S.MD").exists() {
        eprintln!("the file system is case-sensitive; skipped");
        return;
    }
    let stderr = assert_refused(tmp.path(), &["build", "s.md", "-o", "S.MD"], &["s.md"]);
    assert!(
        stderr.contains("refusing to write S.MD: it is the input file s.md"),
        "{stderr}"
    );
}

#[test]
fn build_refuses_a_default_output_path_that_is_its_input() {
    let tmp = project(&[
        ("presenter.toml", "[build]\nout_dir = \".\"\n"),
        ("notes.html", "# Notes\n"),
        ("index/index.html", "# Site\n"),
    ]);
    let stderr = assert_refused(
        tmp.path(),
        &["build", "notes.html", "--format", "md"],
        &["notes.html"],
    );
    assert_eq!(
        stderr,
        "error: refusing to write notes.html: it is the input file notes.html; it is the \
         default output path ([build] out_dir); choose another one with -o\n"
    );
    // Site mode: <out_dir>/<stem>/index.html.
    let stderr = assert_refused(
        tmp.path(),
        &["build", "index/index.html", "--format", "md", "--site"],
        &["index/index.html"],
    );
    assert!(
        stderr.contains(&format!(
            "refusing to write {}: it is the input file",
            Path::new("index/index.html").display()
        )),
        "{stderr}"
    );
}

#[test]
fn build_refuses_a_site_index_that_is_its_input() {
    let tmp = project(&[("s/index.html", "# Site source\n")]);
    let stderr = assert_refused(
        tmp.path(),
        &[
            "build",
            "s/index.html",
            "--format",
            "md",
            "--site",
            "-o",
            "s",
        ],
        &["s/index.html"],
    );
    assert!(stderr.contains("it is the input file"), "{stderr}");
    assert!(!tmp.path().join("s/assets").exists());
}

#[test]
fn build_watch_refuses_its_input_before_watching() {
    let tmp = project(&[("w.md", "# Watched\n")]);
    let stderr = assert_refused(
        tmp.path(),
        &["build", "w.md", "-o", "w.md", "--watch"],
        &["w.md"],
    );
    assert!(!stderr.contains("Watching"), "{stderr}");
}

#[test]
fn build_refuses_to_overwrite_the_config_or_an_asset() {
    let tmp = project(&[
        ("presenter.toml", "[deck]\nlang = \"en\"\n"),
        ("a.md", "# A\n\n![plot](figures/plot.png)\n"),
    ]);
    std::fs::create_dir(tmp.path().join("figures")).unwrap();
    std::fs::copy(
        root().join("tests/fixtures/md/images/ok.png"),
        tmp.path().join("figures/plot.png"),
    )
    .unwrap();
    let protected = ["presenter.toml", "a.md", "figures/plot.png"];
    let stderr = assert_refused(
        tmp.path(),
        &["build", "a.md", "-o", "presenter.toml"],
        &protected,
    );
    assert!(
        stderr.contains("presenter.toml: it is the config file presenter.toml"),
        "{stderr}"
    );
    let stderr = assert_refused(
        tmp.path(),
        &["build", "a.md", "-o", "figures/plot.png"],
        &protected,
    );
    let plot = Path::new("figures/plot.png").display().to_string();
    assert!(
        stderr.contains(&format!("{plot}: it is the deck's asset {plot}")),
        "{stderr}"
    );
    // The same deck builds next to its inputs.
    presenter()
        .current_dir(tmp.path())
        .args(["build", "a.md", "-o", "a.html"])
        .assert()
        .success();
}

#[test]
fn build_watch_refuses_an_asset_as_its_output_before_watching() {
    // The first build finds the clash: the command ends like a single build (exit 2,
    // display paths and the hint), instead of watching with an error it cannot recover
    // from (-o cannot change while watching).
    let tmp = project(&[("a.md", "# A\n\n![plot](figures/plot.png)\n")]);
    std::fs::create_dir(tmp.path().join("figures")).unwrap();
    std::fs::copy(
        root().join("tests/fixtures/md/images/ok.png"),
        tmp.path().join("figures/plot.png"),
    )
    .unwrap();
    let stderr = assert_refused(
        tmp.path(),
        &["build", "a.md", "-o", "figures/plot.png", "--watch"],
        &["a.md", "figures/plot.png"],
    );
    let plot = Path::new("figures/plot.png").display().to_string();
    assert_eq!(
        stderr,
        format!(
            "error: refusing to write {plot}: it is the deck's asset {plot}; choose another \
             output path with -o\n"
        )
    );
}

/// A `presenter build --watch` process in `dir` whose stdout and stderr go to files in
/// `logs` (outside the watched directory); killed when dropped.
struct Watching {
    child: std::process::Child,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl Watching {
    fn spawn(dir: &Path, logs: &Path, args: &[&str]) -> Watching {
        let stdout = logs.join("stdout");
        let stderr = logs.join("stderr");
        let child = std::process::Command::new(env!("CARGO_BIN_EXE_presenter"))
            .current_dir(dir)
            .args(args)
            .env_remove("RUST_LOG")
            .env_remove("NO_COLOR")
            .env_remove("CLICOLOR_FORCE")
            .stdout(std::fs::File::create(&stdout).unwrap())
            .stderr(std::fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        Watching {
            child,
            stdout,
            stderr,
        }
    }

    fn stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    /// Waits (at most 30 s) until `done` holds for stderr; the process must keep running.
    fn wait_for(&mut self, what: &str, done: impl Fn(&str) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            if done(&self.stderr()) {
                return;
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!(
                    "presenter exited ({status}) before {what}:\n{}",
                    self.stderr()
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("timed out waiting for {what}:\n{}", self.stderr());
    }
}

impl Drop for Watching {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn build_watch_reports_a_rebuild_that_would_overwrite_an_input_like_a_build() {
    // The deck starts to use its own output as an image: that rebuild is refused with the
    // message of a single build (display paths, hint), and watching goes on.
    let tmp = project(&[("w.md", "# Watched\n")]);
    let logs = tempfile::tempdir().unwrap();
    let mut watching = Watching::spawn(
        tmp.path(),
        logs.path(),
        &["build", "w.md", "-o", "out/w.png", "--watch"],
    );
    watching.wait_for("the first build", |stderr| {
        stderr.contains("Watching for changes")
    });
    let first = std::fs::read(tmp.path().join("out/w.png")).unwrap();
    // Replaced in one step, so no rebuild sees a half-written deck.
    let edited = logs.path().join("w.md");
    std::fs::write(&edited, "# Watched\n\n![me](out/w.png)\n").unwrap();
    std::fs::rename(&edited, tmp.path().join("w.md")).unwrap();
    let out = Path::new("out/w.png").display().to_string();
    let refusal = format!(
        "\nerror: refusing to write {out}: it is the deck's asset {out}; choose another output \
         path with -o\n"
    );
    // The message may be written in several pieces: wait for all of it.
    watching.wait_for("the refused rebuild", |stderr| stderr.contains(&refusal));
    assert!(
        watching.child.try_wait().unwrap().is_none(),
        "still watching"
    );
    assert_eq!(std::fs::read(tmp.path().join("out/w.png")).unwrap(), first);
    let stdout = std::fs::read_to_string(&watching.stdout).unwrap();
    assert!(
        stdout.starts_with(&format!("Wrote {out} (1 slide, ")),
        "{stdout}"
    );
}

#[cfg(unix)]
#[test]
fn build_refuses_its_input_reached_through_a_link_after_dot_dot() {
    // work/link -> ../other/sub: from work/, `nonexist/../link/../deck.md` is
    // other/deck.md once `nonexist` is created, the input itself.
    let tmp = project(&[("other/deck.md", "# Precious\n"), ("work/keep", "")]);
    std::fs::create_dir(tmp.path().join("other/sub")).unwrap();
    let work = tmp.path().join("work");
    std::os::unix::fs::symlink("../other/sub", work.join("link")).unwrap();
    let stderr = assert_refused(
        &work,
        &[
            "build",
            "../other/deck.md",
            "-o",
            "nonexist/../link/../deck.md",
        ],
        &["../other/deck.md"],
    );
    assert_eq!(
        stderr,
        "error: refusing to write ../other/deck.md: it is the input file ../other/deck.md; \
         choose another output path with -o\n"
    );
    assert!(!work.join("nonexist").exists());
    // `Wrote` names the file a `..` after the link reaches.
    std::fs::write(work.join("x.md"), "# X\n").unwrap();
    let stdout =
        stdout_of(
            presenter()
                .current_dir(&work)
                .args(["build", "x.md", "-o", "link/../out.html"]),
        );
    assert!(
        stdout.starts_with("Wrote ../other/out.html (1 slide, "),
        "{stdout}"
    );
    assert!(tmp.path().join("other/out.html").is_file());
    assert!(!work.join("out.html").exists());
}

#[test]
fn build_refuses_to_overwrite_the_files_of_its_theme() {
    // In a `presenter init` project: the theme's manifest, stylesheet and templates, and a
    // file its stylesheet uses with url(), are inputs like presenter.toml.
    let tmp = tempfile::tempdir().unwrap();
    presenter()
        .current_dir(tmp.path())
        .args(["-q", "init", "."])
        .assert()
        .success();
    std::fs::copy(
        root().join("tests/fixtures/md/images/ok.png"),
        tmp.path().join("theme/bg.png"),
    )
    .unwrap();
    // Wave-8 review: a file the stylesheet reaches outside the theme's standard
    // directories (`url("../img/x.png")` from styles/) is an input too, as are the files a
    // template names with asset() and a Sass partial loaded from another directory: every
    // file of the theme that the build read.
    std::fs::create_dir_all(tmp.path().join("theme/img")).unwrap();
    std::fs::copy(
        root().join("tests/fixtures/md/images/ok.png"),
        tmp.path().join("theme/img/x.png"),
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join("theme/media")).unwrap();
    std::fs::copy(
        root().join("tests/fixtures/md/images/ok.png"),
        tmp.path().join("theme/media/logo.png"),
    )
    .unwrap();
    let title = tmp.path().join("theme/templates/layouts/title.html");
    let text = std::fs::read_to_string(&title).unwrap().replace(
        "{{- super() }}",
        "{{- super() }}\n<img src=\"{{ asset('@' ~ theme.name ~ '/media/logo.png') }}\">",
    );
    std::fs::write(&title, text).unwrap();
    std::fs::create_dir_all(tmp.path().join("theme/lib")).unwrap();
    std::fs::write(
        tmp.path().join("theme/lib/_accent.scss"),
        "$accent: #123456;\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("theme/styles/extra.scss"),
        "@use \"../lib/accent\";\n.reveal h1 { color: accent.$accent; }\n",
    )
    .unwrap();
    let manifest = tmp.path().join("theme/theme.toml");
    let text = std::fs::read_to_string(&manifest).unwrap().replace(
        "entries = [\"styles/theme.css\"]",
        "entries = [\"styles/theme.css\", \"styles/extra.scss\"]",
    );
    std::fs::write(&manifest, text).unwrap();
    let css = tmp.path().join("theme/styles/theme.css");
    let mut text = std::fs::read_to_string(&css).unwrap();
    text.push_str("\n.reveal .slides { background-image: url(\"../bg.png\"); }\n");
    text.push_str(".reveal .backgrounds { background-image: url(\"../img/x.png\"); }\n");
    std::fs::write(&css, text).unwrap();
    let protected = [
        "slides.md",
        "presenter.toml",
        "theme/theme.toml",
        "theme/styles/theme.css",
        "theme/templates/layouts/title.html",
        "theme/bg.png",
        "theme/img/x.png",
        "theme/media/logo.png",
        "theme/lib/_accent.scss",
    ];
    let html = tmp.path().join("check.html");
    presenter()
        .current_dir(tmp.path())
        .args(["-q", "build", "slides.md", "-o", "check.html"])
        .assert()
        .success();
    let html = std::fs::read_to_string(html).unwrap();
    assert!(html.contains("#123456"), "the partial is compiled in");
    assert!(
        html.matches("data:image/png;base64,").count() >= 3,
        "bg.png, x.png and logo.png are inlined"
    );
    for out in &protected[2..] {
        let stderr = assert_refused(tmp.path(), &["build", "slides.md", "-o", out], &protected);
        let shown = Path::new(out).display().to_string();
        assert_eq!(
            stderr,
            format!(
                "error: refusing to write {shown}: it is the theme file {shown}; choose \
                 another output path with -o\n"
            )
        );
    }
    // The first build of --watch finds it too.
    assert_refused(
        tmp.path(),
        &["build", "slides.md", "-o", "theme/theme.toml", "--watch"],
        &protected,
    );
    // An output kept in the theme directory (HTML at its top level, or anywhere below a
    // directory that is not one of the theme's) can be built again.
    for out in ["theme/preview.html", "theme/dist/slides.html"] {
        for _ in 0..2 {
            presenter()
                .current_dir(tmp.path())
                .args(["-q", "build", "slides.md", "-o", out])
                .assert()
                .success();
        }
    }
    presenter()
        .current_dir(tmp.path())
        .args(["-q", "build", "slides.md"])
        .assert()
        .success();
}

#[test]
fn a_standalone_output_that_names_a_directory_is_a_usage_error() {
    let tmp = project(&[("slides.md", "# One\n")]);
    std::fs::create_dir(tmp.path().join("existing")).unwrap();
    // A last component `.` or `..` names a directory too, whether it exists or not.
    for out in [
        "../outdir/",
        "existing",
        "out/.",
        "out/..",
        "existing/.",
        ".",
    ] {
        presenter()
            .current_dir(tmp.path())
            .args(["build", "slides.md", "-o", out])
            .assert()
            .code(2)
            .stdout("")
            .stderr(format!(
                "error: -o {out} names a directory; give the HTML file to write, or use \
                 --site to write a directory\n"
            ));
    }
    assert!(!tmp.path().join("../outdir").exists());
    assert!(!tmp.path().join("out").exists(), "no directory is created");
    // A directory is what --site writes.
    presenter()
        .current_dir(tmp.path())
        .args(["build", "slides.md", "--site", "-o", "site/"])
        .assert()
        .success();
    assert!(tmp.path().join("site/index.html").is_file());
}

#[test]
fn decks_in_sibling_directories_keep_their_own_default_output() {
    // Audit F09: a presenter.toml that does not set out_dir leaves dist/ next to each deck.
    let tmp = project(&[
        ("course/presenter.toml", "[deck]\nlang = \"en\"\n"),
        ("course/week1/slides.md", "# Week 1\n"),
        ("course/week2/slides.md", "# Week 2\n"),
    ]);
    let course = tmp.path().join("course");
    for week in ["week1", "week2"] {
        let stdout = stdout_of(
            presenter()
                .current_dir(&course)
                .args(["build", &format!("{week}/slides.md")]),
        );
        // (`/`-separated on every platform: cli::display_path.)
        assert!(
            stdout.starts_with(&format!("Wrote {week}/dist/slides.html (1 slide, ")),
            "{stdout}"
        );
    }
    let week1 = std::fs::read_to_string(course.join("week1/dist/slides.html")).unwrap();
    let week2 = std::fs::read_to_string(course.join("week2/dist/slides.html")).unwrap();
    assert!(week1.contains("Week 1") && !week1.contains("Week 2"));
    assert!(week2.contains("Week 2"));
    assert!(!course.join("dist").exists());
}

#[test]
fn check_and_build_name_an_invalid_config_alike() {
    // Audit F39 (a): check's error diagnostics show the presenter.toml as build does.
    let tmp = project(&[
        ("c/presenter.toml", "[deck]\ntheme = 3\n"),
        ("c/s.md", "# A\n"),
    ]);
    let config = Path::new("c/presenter.toml").display().to_string();
    let output = presenter()
        .current_dir(tmp.path())
        .args(["build", "c/s.md"])
        .assert()
        .code(2)
        .get_output()
        .clone();
    assert!(
        stderr_of(&output).starts_with(&format!("error: config: {config}: ")),
        "{}",
        stderr_of(&output)
    );
    let output = presenter()
        .current_dir(tmp.path())
        .args(["check", "c/s.md"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let deck = Path::new("c/s.md").display().to_string();
    assert!(
        stderr_of(&output).starts_with(&format!("{deck}: error: config: {config}: ")),
        "{}",
        stderr_of(&output)
    );
    let output = presenter()
        .current_dir(tmp.path())
        .args(["check", "c/s.md", "--format", "json"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let entries: Value = serde_json::from_slice(&output.stdout).unwrap();
    let message = entries[0]["message"].as_str().unwrap();
    assert!(
        message.starts_with(&format!("config: {config}: ")),
        "{message}"
    );
}

#[test]
fn paths_outside_the_current_directory_are_shown_with_dot_dot() {
    // Audit F39 (b): `path:line:` lines and framed errors agree.
    let tmp = project(&[
        ("talk/x.md", "# X\n"),
        ("m2/w.md", "# W {bogus=1}\n\n![a](nope.png)\n"),
    ]);
    let talk = tmp.path().join("talk");
    let deck = Path::new("../m2/w.md").display().to_string();
    let output = presenter()
        .current_dir(&talk)
        .args(["check", "../m2/w.md"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let stderr = stderr_of(&output);
    assert!(!stderr.is_empty());
    for line in stderr.lines() {
        assert!(line.starts_with(&format!("{deck}:")), "{stderr}");
    }
    let output = presenter()
        .current_dir(&talk)
        .args(["build", "../m2/w.md"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stderr = stderr_of(&output);
    assert!(
        stderr.starts_with(&format!("{deck}:1: warning: ")),
        "{stderr}"
    );
    assert!(stderr.contains(&format!("\n --> {deck}:3\n")), "{stderr}");
    // An absolute spelling of the same file is shown the same way.
    let absolute = tmp.path().join("m2/w.md");
    let output = presenter()
        .current_dir(&talk)
        .arg("check")
        .arg(&absolute)
        .assert()
        .code(0)
        .get_output()
        .clone();
    assert!(
        stderr_of(&output).starts_with(&format!("{deck}:1: warning: ")),
        "{}",
        stderr_of(&output)
    );
}

#[cfg(unix)]
#[test]
fn paths_through_a_symlinked_prefix_are_shown_relative() {
    // Audit F39 (c): the current directory is canonical, the typed path goes through a link.
    let tmp = tempfile::tempdir().unwrap();
    let real = std::fs::canonicalize(tmp.path()).unwrap().join("real");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(real.join("ok.md"), "# OK\n").unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let output = presenter()
        .current_dir(&real)
        .arg("build")
        .arg(link.join("ok.md"))
        .arg("-o")
        .arg(link.join("out.html"))
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("Wrote out.html (1 slide, "), "{stdout}");
}

#[cfg(unix)]
#[test]
fn a_path_with_dot_dot_keeps_its_last_name_as_typed() {
    // Wave-8 review of the A1 fixes: a path with `..` was shown with its last name replaced
    // by the target of a final symbolic link (or its on-disk letter case), so build and
    // check named the same input differently.
    let tmp = project(&[
        ("w/a.md", "# A\n"),
        ("decks/talk-v2.md", "# T\n\n![x](missing.png)\n"),
        ("site/v/v3.html", "old\n"),
        ("sib/s.md", "# S\n"),
    ]);
    std::os::unix::fs::symlink("a.md", tmp.path().join("w/alias.html")).unwrap();
    std::os::unix::fs::symlink("talk-v2.md", tmp.path().join("decks/current.md")).unwrap();
    std::os::unix::fs::symlink("v/v3.html", tmp.path().join("site/current.html")).unwrap();
    let w = tmp.path().join("w");
    let stderr = assert_refused(&w, &["build", "a.md", "-o", "../w/alias.html"], &["a.md"]);
    assert_eq!(
        stderr,
        "error: refusing to write alias.html: it is the input file a.md; choose another \
         output path with -o\n"
    );
    if w.join("A.MD").exists() {
        let stderr = assert_refused(&w, &["build", "a.md", "-o", "../w/A.MD"], &["a.md"]);
        assert!(
            stderr.starts_with("error: refusing to write A.MD: it is the input file a.md;"),
            "{stderr}"
        );
    }
    let sib = tmp.path().join("sib");
    let output = presenter()
        .current_dir(&sib)
        .args(["build", "../decks/current.md"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("\n --> ../decks/current.md:3\n"),
        "{stderr}"
    );
    let output = presenter()
        .current_dir(&sib)
        .args(["check", "../decks/current.md"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let stderr = stderr_of(&output);
    assert!(
        stderr.starts_with("../decks/current.md:3: warning: "),
        "{stderr}"
    );
    let stdout = stdout_of(presenter().current_dir(&sib).args([
        "build",
        "s.md",
        "-o",
        "../site/current.html",
    ]));
    assert!(
        stdout.starts_with("Wrote ../site/current.html (1 slide, "),
        "{stdout}"
    );
    // Wave-8 review of W8FIX: so are the directories after the last `..`, a symbolic link
    // to a directory included; build and check show the same path, and `Wrote` the one
    // given.
    std::os::unix::fs::symlink("decks", tmp.path().join("decks-link")).unwrap();
    let output = presenter()
        .current_dir(&sib)
        .args(["build", "../decks-link/talk-v2.md"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stderr = stderr_of(&output);
    assert!(
        stderr.starts_with("error: asset not found: ../decks-link/missing.png\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("\n --> ../decks-link/talk-v2.md:3\n"),
        "{stderr}"
    );
    let output = presenter()
        .current_dir(&sib)
        .args(["check", "../decks-link/talk-v2.md"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let stderr = stderr_of(&output);
    assert!(
        stderr.starts_with("../decks-link/talk-v2.md:3: warning: "),
        "{stderr}"
    );
    let stdout = stdout_of(presenter().current_dir(&sib).args([
        "build",
        "s.md",
        "-o",
        "../decks-link/out.html",
    ]));
    assert!(
        stdout.starts_with("Wrote ../decks-link/out.html (1 slide, "),
        "{stdout}"
    );
    assert!(tmp.path().join("decks/out.html").is_file());
}
