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

/// The binary, run from the crate root, without the developer's `RUST_LOG` / `NO_COLOR`
/// (stderr assertions must not depend on them).
fn presenter() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_presenter"));
    cmd.current_dir(root())
        .env_remove("RUST_LOG")
        .env_remove("NO_COLOR");
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
    assert!(caps[1].ends_with(path_suffix), "{stdout}");
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
        .stderr(contains("error: deck.md:3: asset not found: "))
        .stderr(contains("nope.png"));
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
fn failed_builds_print_the_warnings_and_other_missing_assets_first() {
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
    assert_eq!(lines.len(), 4, "{stderr}");
    assert!(lines[0].starts_with("two.md:1: warning: "), "{stderr}");
    assert!(lines[0].contains("bogus"), "{stderr}");
    assert!(lines[1].starts_with("two.md:5: warning: "), "{stderr}");
    assert!(
        lines[2].starts_with("two.md:9: error: ") && lines[2].contains("b2.png"),
        "{stderr}"
    );
    assert!(
        lines[3].starts_with("error: two.md:3: asset not found: ") && lines[3].ends_with("a1.png"),
        "{stderr}"
    );
    assert!(!tmp.path().join("two.html").exists());
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
        .stderr(contains("error: sub.md: asset not found: "))
        .stderr(contains("nologo.png"));
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
            .stderr(contains("error: fm.md: front matter: "));
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
