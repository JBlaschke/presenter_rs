//! T41 end to end: `presenter theme list / eject / new / check` (cli.md `theme`,
//! theme-format.md §1–§3) through the binary, in temporary directories.
//!
//! The ejected `default` theme is compared with the built-in one by building
//! `examples/basic` with both: the documents must be identical up to the theme's name.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::str::contains;
use presenter::theme::{ThemeManifest, builtin_names, resolve};
use regex::Regex;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The binary, run in `cwd`, without the developer's `RUST_LOG` / `NO_COLOR`.
fn presenter(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_presenter"));
    cmd.current_dir(cwd)
        .env_remove("RUST_LOG")
        .env_remove("NO_COLOR");
    cmd
}

/// Runs `presenter <args>` in `cwd` and returns (exit code, stdout, stderr).
fn run(cwd: &Path, args: &[&str]) -> (i32, String, String) {
    let output = presenter(cwd).args(args).output().unwrap();
    (
        output.status.code().expect("exit code"),
        String::from_utf8(output.stdout).expect("UTF-8 stdout"),
        String::from_utf8(output.stderr).expect("UTF-8 stderr"),
    )
}

/// Writes `files` (path, contents) below `dir`.
fn write_files(dir: &Path, files: &[(&str, &str)]) {
    for (path, contents) in files {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
}

/// Builds `examples/basic/slides.md` into `out` (extra arguments such as `--theme`) and
/// returns the document.
fn build_example(out: &Path, extra: &[&str]) -> String {
    let out_arg = out.display().to_string();
    let mut args = vec!["build", "examples/basic/slides.md", "-o", out_arg.as_str()];
    args.extend_from_slice(extra);
    presenter(&root()).args(&args).assert().success();
    fs::read_to_string(out).unwrap()
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

/// The compiled theme stylesheet (`<style id="presenter-theme">…</style>`).
fn theme_style(html: &str) -> &str {
    let start = html
        .find(r#"<style id="presenter-theme">"#)
        .expect("theme style");
    let end = start + html[start..].find("</style>").expect("closed style");
    &html[start..end]
}

/// The first line where `a` and `b` differ, for assertion messages.
fn first_difference(a: &str, b: &str) -> String {
    match a.lines().zip(b.lines()).position(|(x, y)| x != y) {
        Some(index) => format!(
            "line {}:\n  {:?}\n  {:?}",
            index + 1,
            a.lines().nth(index).unwrap_or(""),
            b.lines().nth(index).unwrap_or("")
        ),
        None => format!("{} vs {} lines", a.lines().count(), b.lines().count()),
    }
}

fn manifest(dir: &Path) -> ThemeManifest {
    let text = fs::read_to_string(dir.join("theme.toml")).unwrap();
    ThemeManifest::from_toml_str(&text, "test", "theme.toml").unwrap()
}

/// True if no directory from `dir` up has a presenter.toml (theme list then says so).
fn no_presenter_toml_above(dir: &Path) -> bool {
    dir.ancestors().all(|d| !d.join("presenter.toml").exists())
}

#[test]
fn list_shows_the_builtin_themes_with_their_descriptions() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = run(tmp.path(), &["theme", "list"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.starts_with("Built-in themes:\n"), "{stdout}");
    let names = builtin_names();
    assert!(names.contains(&"default".to_string()), "{names:?}");
    for name in names {
        let theme = resolve(&name, &[]).unwrap();
        let description = theme.chain[0].manifest.description.clone().unwrap();
        let line = Regex::new(&format!(
            r"(?m)^  {} +{}$",
            regex::escape(&name),
            regex::escape(&description)
        ))
        .unwrap();
        assert!(line.is_match(&stdout), "{name}: {stdout}");
    }
    if no_presenter_toml_above(tmp.path()) {
        assert!(
            stdout.contains("No presenter.toml in the current directory or its parents."),
            "{stdout}"
        );
    }
}

#[test]
fn list_shows_the_theme_chain_of_the_project() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("talk");
    write_files(
        &project,
        &[
            ("presenter.toml", "[deck]\ntheme = \"brand\"\n"),
            (
                "brand/theme.toml",
                "name = \"brand-theme\"\nextends = \"default\"\n",
            ),
            ("notes/.keep", ""),
        ],
    );
    let expected = [
        "Theme 'brand-theme' for this directory ([deck] theme = \"brand\" in ",
        "  chain: brand-theme → default\n",
        "    default      built-in",
    ];
    // Discovered from a subdirectory: the theme is found next to presenter.toml.
    let (code, stdout, stderr) = run(&project.join("notes"), &["theme", "list"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("  default  "), "{stdout}");
    for part in expected {
        assert!(stdout.contains(part), "{part:?} in {stdout}");
    }
    // --config bypasses discovery.
    let config = project.join("presenter.toml").display().to_string();
    let (code, stdout, stderr) = run(tmp.path(), &["--config", &config, "theme", "list"]);
    assert_eq!(code, 0, "{stderr}");
    for part in expected {
        assert!(stdout.contains(part), "{part:?} in {stdout}");
    }
    // A theme that does not resolve is a theme error, after the built-in list.
    write_files(
        &project,
        &[("presenter.toml", "[deck]\ntheme = \"nope\"\n")],
    );
    let (code, stdout, stderr) = run(&project, &["theme", "list"]);
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("  default  "), "{stdout}");
    assert!(
        stderr.contains("presenter.toml: [deck] theme: theme 'nope': not found"),
        "{stderr}"
    );
}

#[test]
fn eject_default_copies_every_file_and_renames_the_theme() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = run(tmp.path(), &["theme", "eject", "default", "my-copy"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("--theme my-copy"), "{stdout}");
    assert!(stdout.contains("theme = \"my-copy\""), "{stdout}");
    let copy = tmp.path().join("my-copy");
    assert!(copy.join("theme.toml").is_file());
    assert!(copy.join("templates/deck.html").is_file());

    let builtin = resolve("default", &[]).unwrap().chain.remove(0);
    let files = builtin.list("");
    assert!(files.len() >= 6, "{files:?}");
    // Byte for byte, except that templates name their own files `@my-copy/…`.
    for rel in &files {
        let copied = fs::read(copy.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        let original = builtin.read(rel).unwrap().unwrap();
        if rel.starts_with("templates/") {
            let original = String::from_utf8(original).unwrap();
            let expected = original.replace("@default/", "@my-copy/");
            assert_eq!(String::from_utf8(copied).unwrap(), expected, "{rel}");
        } else if rel != "theme.toml" {
            assert_eq!(copied, original, "{rel}");
        }
    }
    let deck = fs::read_to_string(copy.join("templates/deck.html")).unwrap();
    assert!(!deck.contains("@default/"), "{deck}");
    let copied_files = walk(&copy);
    assert_eq!(copied_files, files, "exactly the built-in theme's files");
    // Renamed, otherwise identical: no `extends`, so a root theme.
    assert_eq!(
        manifest(&copy),
        ThemeManifest {
            name: "my-copy".into(),
            ..builtin.manifest.clone()
        }
    );
    let (code, stdout, stderr) = run(tmp.path(), &["theme", "check", "my-copy"]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("  chain: my-copy\n"), "{stdout}");
    assert!(stdout.ends_with("No problems found.\n"), "{stdout}");
}

/// Files below `dir`, relative and `/`-separated, sorted.
fn walk(dir: &Path) -> Vec<String> {
    let mut files: Vec<String> = walkdir::WalkDir::new(dir)
        .into_iter()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            let rel = entry.path().strip_prefix(dir).unwrap();
            rel.components()
                .map(|c| c.as_os_str().to_str().unwrap())
                .collect::<Vec<_>>()
                .join("/")
        })
        .collect();
    files.sort();
    files
}

#[test]
fn ejected_default_builds_exactly_like_the_builtin_theme() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, _, stderr) = run(tmp.path(), &["theme", "eject", "default", "ejected-copy"]);
    assert_eq!(code, 0, "{stderr}");
    let ejected_dir = tmp.path().join("ejected-copy").display().to_string();

    // examples/basic/presenter.toml selects the built-in `default`.
    let builtin = build_example(&tmp.path().join("builtin.html"), &[]);
    let ejected = build_example(&tmp.path().join("ejected.html"), &["--theme", &ejected_dir]);
    assert_eq!(
        slides_region(&ejected),
        slides_region(&builtin),
        "slides region: {}",
        first_difference(slides_region(&ejected), slides_region(&builtin))
    );
    // The theme's name appears in the theme CSS (`/* @ejected-copy/styles/theme.css */`
    // source labels) and may appear wherever a template prints it; nothing else differs.
    let renamed = ejected.replace("ejected-copy", "default");
    assert_eq!(theme_style(&renamed), theme_style(&builtin), "theme CSS");
    assert!(
        renamed == builtin,
        "documents differ: {}",
        first_difference(&renamed, &builtin)
    );
}

#[test]
fn eject_refuses_existing_paths_and_themes_that_are_not_built_in() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join("existing")).unwrap();
    let (code, _, stderr) = run(tmp.path(), &["theme", "eject", "default", "existing"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("existing already exists"), "{stderr}");
    assert!(
        walk(&tmp.path().join("existing")).is_empty(),
        "nothing written"
    );

    fs::write(tmp.path().join("file"), "x").unwrap();
    let (code, _, stderr) = run(tmp.path(), &["theme", "eject", "default", "file"]);
    assert_eq!(code, 2, "{stderr}");

    let (code, _, stderr) = run(tmp.path(), &["theme", "eject", "nope", "x"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("'nope' is not a built-in theme (built-in themes: default"),
        "{stderr}"
    );
    assert!(!tmp.path().join("x").exists());

    // The directory name becomes the theme name, which cannot contain '@'.
    let (code, _, stderr) = run(tmp.path(), &["theme", "eject", "default", "@x"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("invalid theme name \"@x\""), "{stderr}");
    assert!(!tmp.path().join("@x").exists());
}

#[test]
fn new_then_check_passes_and_the_theme_builds() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = run(tmp.path(), &["theme", "new", "my-theme"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("Created the theme 'my-theme'"), "{stdout}");
    assert!(
        stdout.contains("presenter theme check my-theme"),
        "{stdout}"
    );
    let dir = tmp.path().join("my-theme");
    assert!(dir.join("templates/.gitkeep").is_file());
    let parsed = manifest(&dir);
    assert_eq!(parsed.name, "my-theme");
    assert_eq!(parsed.extends.as_deref(), Some("default"));
    assert_eq!(parsed.styles.entries, ["styles/theme.css"]);
    // The token block: the parent's custom properties, commented out.
    let css = fs::read_to_string(dir.join("styles/theme.css")).unwrap();
    assert!(css.contains(":root {"), "{css}");
    assert!(css.contains("/* --r-background-color: "), "{css}");
    assert!(css.contains("/* --p-"), "the default theme's tokens: {css}");
    let declarations = Regex::new(r"(?m)^\s*--[\w-]+\s*:").unwrap();
    assert!(
        !declarations.is_match(&css),
        "every token is commented out: {css}"
    );

    let (code, stdout, stderr) = run(tmp.path(), &["theme", "check", "my-theme"]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("  chain: my-theme → default\n"), "{stdout}");
    assert!(stdout.ends_with("No problems found.\n"), "{stdout}");

    // The new theme's stylesheet is part of the build.
    fs::write(
        dir.join("styles/theme.css"),
        format!("{css}\n.reveal h1 {{ letter-spacing: 0.0123em; }}\n"),
    )
    .unwrap();
    let theme_arg = dir.display().to_string();
    let html = build_example(&tmp.path().join("out.html"), &["--theme", &theme_arg]);
    assert!(
        theme_style(&html).contains("0.0123em"),
        "child CSS included"
    );
}

#[test]
fn new_extends_a_theme_directory_relative_to_the_new_theme() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join("nested")).unwrap();
    for args in [
        &["theme", "new", "base"][..],
        &["theme", "new", "child", "--extends", "base"][..],
        &["theme", "new", "nested/deep", "--extends", "./base"][..],
    ] {
        let (code, _, stderr) = run(tmp.path(), args);
        assert_eq!(code, 0, "{args:?}: {stderr}");
    }
    assert_eq!(
        manifest(&tmp.path().join("child")).extends.as_deref(),
        Some("../base")
    );
    assert_eq!(
        manifest(&tmp.path().join("nested/deep")).extends.as_deref(),
        Some("../../base")
    );
    let (code, stdout, stderr) = run(&tmp.path().join("nested"), &["theme", "check", "deep"]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(
        stdout.contains("  chain: deep → base → default\n"),
        "{stdout}"
    );
}

#[test]
fn new_refuses_non_empty_directories_name_clashes_and_unknown_parents() {
    let tmp = tempfile::tempdir().unwrap();
    write_files(tmp.path(), &[("full/x.txt", "x")]);
    let (code, _, stderr) = run(tmp.path(), &["theme", "new", "full"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("full exists and is not an empty directory"),
        "{stderr}"
    );

    fs::create_dir(tmp.path().join("empty")).unwrap();
    let (code, _, stderr) = run(tmp.path(), &["theme", "new", "empty"]);
    assert_eq!(code, 0, "an empty directory is fine: {stderr}");

    // A child named like its parent would make `@default/…` ambiguous.
    let (code, _, stderr) = run(tmp.path(), &["theme", "new", "default"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("would be named 'default'"), "{stderr}");
    assert!(!tmp.path().join("default").exists());

    let (code, _, stderr) = run(tmp.path(), &["theme", "new", "x", "--extends", "nope"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.contains("--extends nope: theme 'nope': not found"),
        "{stderr}"
    );
    assert!(!tmp.path().join("x").exists());
}

#[test]
fn check_without_theme_toml_fails_with_a_clear_message() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join("plain")).unwrap();
    let (code, stdout, stderr) = run(tmp.path(), &["theme", "check", "plain"]);
    assert_eq!(code, 1, "{stdout}");
    assert!(
        stderr.contains("error: theme check: plain has no theme.toml"),
        "{stderr}"
    );
    assert!(stdout.is_empty(), "{stdout}");
    let (code, _, stderr) = run(tmp.path(), &["theme", "check", "missing"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("missing does not exist"), "{stderr}");
}

#[test]
fn check_reports_every_problem_with_its_location() {
    let tmp = tempfile::tempdir().unwrap();
    write_files(
        tmp.path(),
        &[
            (
                "bad/theme.toml",
                "name = \"bad\"\nextends = \"default\"\n\n[styles]\n\
                 entries = [\"styles/ok.css\", \"styles/missing.css\"]\n\
                 highlight_style = \"styles/hl.css\"\n\n[scripts]\n\
                 entries = [\"../outside.js\"]\n\n[reveal]\nplugins = [\"notez\"]\n\n\
                 [layouts]\nfancy = \"no file\"\ntitle = \"provided by default\"\n",
            ),
            ("bad/styles/ok.css", ""),
            (
                "bad/templates/layouts/broken.html",
                "<section>\n{% if x %}\n",
            ),
            (
                "bad/templates/layouts/fine.html",
                "{% include \"block.html\" %}",
            ),
            ("bad/templates/.gitkeep", ""),
        ],
    );
    let (code, stdout, stderr) = run(tmp.path(), &["theme", "check", "bad"]);
    assert_eq!(code, 1, "{stdout}{stderr}");
    assert!(stdout.contains("  chain: bad → default\n"), "{stdout}");
    assert!(stdout.ends_with("6 problems found.\n"), "{stdout}");
    for expected in [
        "theme.toml:5: error: [styles] entry 'styles/missing.css' is not found",
        "theme.toml:6: error: [styles] highlight_style 'styles/hl.css' (not monokai, zenburn or none, so a CSS file) is not found",
        "theme.toml:9: error: [scripts] entry '../outside.js' is outside the theme directory",
        "theme.toml:15: error: [layouts] documents the layout 'fancy', but neither this theme nor a parent has templates/layouts/fancy.html",
        "broken.html:2: error: syntax error: unexpected end of input",
        "theme.toml:12: error: [reveal] unknown reveal plugin 'notez' (did you mean 'notes'?)",
    ] {
        assert!(stderr.contains(expected), "{expected:?} in\n{stderr}");
    }
    assert_eq!(stderr.lines().count(), 6, "{stderr}");
    assert!(!stderr.contains("ok.css"), "{stderr}");
    assert!(!stderr.contains("'title'"), "{stderr}");

    // -q: only the problems.
    let (code, stdout, quiet_stderr) = run(tmp.path(), &["-q", "theme", "check", "bad"]);
    assert_eq!(code, 1);
    assert!(stdout.is_empty(), "{stdout}");
    assert_eq!(quiet_stderr, stderr);
}

/// A `theme check` failure case: directory, its files, part of the expected message.
type FailureCase<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str);

#[test]
fn check_fails_on_manifest_and_chain_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let cases: &[FailureCase<'_>] = &[
        (
            "unknown-key",
            &[(
                "theme.toml",
                "name = \"t\"\nextends = \"default\"\ncolour = 1\n",
            )],
            "unknown field `colour`",
        ),
        (
            "typo",
            &[(
                "theme.toml",
                "name = \"t\"\nextends = \"default\"\n[styles]\nentrys = []\n",
            )],
            "did you mean 'entries'",
        ),
        (
            "bad-name",
            &[("theme.toml", "name = \"a/b\"\nextends = \"default\"\n")],
            "invalid theme name \"a/b\"",
        ),
        (
            "orphan",
            &[("theme.toml", "name = \"t\"\nextends = \"nope\"\n")],
            "theme 'nope': not found",
        ),
        (
            "incomplete-root",
            &[
                ("theme.toml", "name = \"t\"\n"),
                ("templates/deck.html", "<html></html>"),
            ],
            "is missing required files: templates/slide.html, templates/block.html, \
             templates/cell.html, templates/layouts/default.html",
        ),
    ];
    for (dir, files, message) in cases {
        write_files(&tmp.path().join(dir), files);
        let (code, stdout, stderr) = run(tmp.path(), &["theme", "check", dir]);
        assert_eq!(code, 1, "{dir}: {stdout}{stderr}");
        assert!(stderr.starts_with("error: "), "{dir}: {stderr}");
        assert!(stderr.contains(message), "{dir}: {message:?} in {stderr}");
        // Resolution errors of the checked theme name the directory as typed, not its
        // absolute path.
        if matches!(*dir, "unknown-key" | "typo" | "incomplete-root") {
            assert!(
                stderr.starts_with(&format!("error: theme '{dir}': ")),
                "{dir}: {stderr}"
            );
        }
    }
    presenter(tmp.path())
        .args(["theme", "check", "typo"])
        .assert()
        .code(1)
        .stderr(contains("line 4"));
}

// --- Fix round 1 -----------------------------------------------------------------------

#[test]
fn list_says_when_presenter_toml_leaves_the_theme_to_the_default() {
    let tmp = tempfile::tempdir().unwrap();
    write_files(tmp.path(), &[("presenter.toml", "[deck]\ntitle = \"x\"\n")]);
    let (code, stdout, stderr) = run(tmp.path(), &["theme", "list"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains(
            "Theme 'default' for this directory (the default; presenter.toml has no [deck] theme):\n  chain: default\n"
        ),
        "{stdout}"
    );
    assert!(!stdout.contains("[deck] theme = "), "{stdout}");
    // An invalid presenter.toml is a usage error (exit 2), after the built-in list.
    write_files(tmp.path(), &[("presenter.toml", "[deck]\ntheme = 1\n")]);
    let (code, stdout, stderr) = run(tmp.path(), &["theme", "list"]);
    assert_eq!(code, 2, "{stdout}{stderr}");
    assert!(stdout.starts_with("Built-in themes:"), "{stdout}");
}

#[test]
fn printed_commands_quote_directories_for_the_shell() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = run(tmp.path(), &["theme", "eject", "default", "full copy"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("  presenter build slides.md --theme 'full copy'\n"),
        "{stdout}"
    );
    assert!(stdout.contains("theme = \"full copy\""), "{stdout}");
    assert!(
        stdout.contains("presenter theme check 'full copy'"),
        "{stdout}"
    );
    let (code, stdout, stderr) = run(tmp.path(), &["theme", "new", "my theme"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains(
            "  presenter build slides.md --theme 'my theme'  # or [deck] theme = \"my theme\""
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains("presenter theme check 'my theme'"),
        "{stdout}"
    );
    // Plain names stay plain.
    let (_, stdout, _) = run(tmp.path(), &["theme", "new", "plain-name"]);
    assert!(
        stdout.contains("presenter theme check plain-name\n"),
        "{stdout}"
    );
}

#[test]
fn new_and_eject_take_dir_lexically_and_never_overwrite() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    let precious = [
        ("theme.toml", "name = \"mine\"\n"),
        ("styles/theme.css", "MY PRECIOUS CSS"),
    ];
    write_files(&work, &precious);
    fs::create_dir(work.join("empty")).unwrap();
    // `ghost/..` is `work`, which exists and is not empty.
    let (code, _, stderr) = run(&work, &["theme", "new", "ghost/.."]);
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("ghost/.. (that is ")
            && stderr.contains("exists and is not an empty directory"),
        "{stderr}"
    );
    let (code, _, stderr) = run(&work, &["theme", "eject", "default", "newdir/.."]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("already exists"), "{stderr}");
    assert!(!work.join("ghost").exists());
    assert!(!work.join("newdir").exists());
    for (path, contents) in precious {
        assert_eq!(fs::read_to_string(work.join(path)).unwrap(), contents);
    }
    // `empty/sub/..` is the empty directory `empty`: the theme goes there, no `sub`.
    let (code, _, stderr) = run(&work, &["theme", "new", "empty/sub/.."]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(manifest(&work.join("empty")).name, "empty");
    assert!(!work.join("empty/sub").exists());
}

#[test]
fn new_lists_what_it_can_when_a_parent_stylesheet_is_missing() {
    let tmp = tempfile::tempdir().unwrap();
    write_files(
        tmp.path(),
        &[(
            "base/theme.toml",
            "name = \"base\"\nextends = \"default\"\n[styles]\nentries = [\"styles/missing.css\"]\n",
        )],
    );
    let (code, _, stderr) = run(tmp.path(), &["theme", "new", "kid", "--extends", "base"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("theme 'base': the style entry 'styles/missing.css' cannot be read"),
        "{stderr}"
    );
    let css = fs::read_to_string(tmp.path().join("kid/styles/theme.css")).unwrap();
    assert!(css.contains("/* --r-background-color: "), "{css}");
    assert_eq!(
        manifest(&tmp.path().join("kid")).extends.as_deref(),
        Some("../base")
    );
    // `check` reports the parent's problem.
    let (code, stdout, stderr) = run(tmp.path(), &["theme", "check", "kid"]);
    assert_eq!(code, 1, "{stdout}{stderr}");
    assert!(
        stderr.contains("theme.toml:4: error: [styles] entry 'styles/missing.css' is not found"),
        "{stderr}"
    );
}

#[test]
fn new_refuses_a_parent_whose_chain_is_full_before_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, _, stderr) = run(tmp.path(), &["theme", "new", "t1"]);
    assert_eq!(code, 0, "{stderr}");
    for level in 2..=7 {
        let (dir, parent) = (format!("t{level}"), format!("t{}", level - 1));
        let (code, _, stderr) = run(tmp.path(), &["theme", "new", &dir, "--extends", &parent]);
        assert_eq!(code, 0, "{dir}: {stderr}");
    }
    // t7 → … → t1 → default is 8 themes, the most a chain may have.
    let (code, _, stderr) = run(tmp.path(), &["theme", "new", "t8", "--extends", "t7"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("--extends t7: its chain already has 8 themes (t7 → t6"),
        "{stderr}"
    );
    assert!(!tmp.path().join("t8").exists());
}

#[test]
fn check_points_at_the_entry_not_at_a_commented_example() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, _, stderr) = run(tmp.path(), &["-q", "theme", "new", "hint"]);
    assert_eq!(code, 0, "{stderr}");
    let path = tmp.path().join("hint/theme.toml");
    let mut text = fs::read_to_string(&path).unwrap();
    // The scaffold has the same entry as a commented example.
    assert!(
        text.contains("# entries = [\"scripts/extra.js\"]"),
        "{text}"
    );
    text.push_str("\n[scripts]\nentries = [\"scripts/extra.js\"]\n");
    fs::write(&path, &text).unwrap();
    let line = text
        .lines()
        .position(|l| l == "entries = [\"scripts/extra.js\"]")
        .unwrap()
        + 1;
    let (code, stdout, stderr) = run(tmp.path(), &["theme", "check", "hint"]);
    assert_eq!(code, 1, "{stdout}{stderr}");
    let expected =
        format!("theme.toml:{line}: error: [scripts] entry 'scripts/extra.js' is not found");
    assert!(stderr.contains(&expected), "{expected:?} in {stderr}");
}

#[test]
fn check_checks_the_directory_never_a_builtin_theme_of_that_name() {
    let tmp = tempfile::tempdir().unwrap();
    // A wrongly named manifest: on a case-insensitive file system `default/theme.toml`
    // "exists", but resolution must not fall back to the built-in `default`.
    write_files(
        tmp.path(),
        &[(
            "default/Theme.toml",
            "name = \"default\"\n[styles]\nentries = [\"styles/missing.css\"]\n",
        )],
    );
    let (code, stdout, stderr) = run(tmp.path(), &["theme", "check", "default"]);
    assert_eq!(code, 1, "{stdout}{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(
        stderr.contains(
            "default has no theme.toml (it has Theme.toml, but the name must be theme.toml exactly)"
        ),
        "{stderr}"
    );
    // A real `default` directory is checked as that directory.
    fs::remove_dir_all(tmp.path().join("default")).unwrap();
    let (code, _, stderr) = run(tmp.path(), &["-q", "theme", "eject", "default", "default"]);
    assert_eq!(code, 0, "{stderr}");
    let (code, stdout, stderr) = run(tmp.path(), &["theme", "check", "default"]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("  chain: default\n"), "{stdout}");
    assert!(!stdout.contains("built-in"), "{stdout}");
}

#[test]
fn new_without_extends_uses_the_builtin_default_even_next_to_a_default_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, _, stderr) = run(tmp.path(), &["-q", "theme", "eject", "default", "default"]);
    assert_eq!(code, 0, "{stderr}");
    let (code, stdout, stderr) = run(tmp.path(), &["theme", "new", "other"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        manifest(&tmp.path().join("other")).extends.as_deref(),
        Some("default")
    );
    assert!(stdout.contains("    default  built-in"), "{stdout}");
    // An explicit --extends resolves as theme-format.md §3 says: the directory first.
    let (code, _, stderr) = run(
        tmp.path(),
        &["theme", "new", "local", "--extends", "default"],
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        manifest(&tmp.path().join("local")).extends.as_deref(),
        Some("../default")
    );
}
