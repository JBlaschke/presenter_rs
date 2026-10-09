//! T40 end to end: `presenter init` scaffolds a project (cli.md `init`) that builds with the
//! CLI, uses its child theme, has no diagnostics, and refuses a non-empty directory.
//!
//! Every project is created in a temporary directory. The scaffold is embedded
//! (`assets/scaffold/`, see `src/vendor.rs`); `cargo test --release --test cli_init` runs the
//! same tests against the embedded copy instead of the files on disk.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use presenter::config::Config;
use presenter::pipeline::{self, BuildOptions};
use regex::Regex;
use serde_json::Value;

/// A name with every character the scaffold has to escape somewhere: Markdown inline
/// syntax, YAML/TOML/JSON quoting and backslashes.
const TRICKY_NAME: &str =
    r#"Rust: "fast" & <safe> {x} $5 #1 *a* _b_ `c` [d](e) ~~f~~ C:\path it's |p| !g {{deck}}"#;

/// A name with URLs and an e-mail address: Markdown escapes do not work inside autolinks,
/// so the title heading must not become one.
const URL_NAME: &str = "Talk at https://example.com/x_y, www.example.org/a_b and a_b@c.com";

/// The placeholders and the line marker of `src/cli/init.rs`; none may survive into a
/// project.
const PLACEHOLDERS: [&str; 5] = [
    "{{name}}",
    "{{name_yaml}}",
    "{{theme_name}}",
    "{{deck}}",
    "{{child_theme}}",
];

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

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("UTF-8 output")
}

fn read(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Runs `presenter init <args>` in `cwd`, asserts exit 0 and an empty stderr, and returns
/// stdout.
fn init(cwd: &Path, args: &[&str]) -> String {
    let output = presenter(cwd).arg("init").args(args).output().unwrap();
    assert!(
        output.status.success(),
        "init {args:?}: {}",
        text(&output.stderr)
    );
    assert_eq!(text(&output.stderr), "", "init {args:?}");
    text(&output.stdout)
}

/// Runs `presenter init <args>` in `cwd` and asserts the usage-error exit code 2.
fn init_fails(cwd: &Path, args: &[&str]) -> Output {
    let output = presenter(cwd).arg("init").args(args).output().unwrap();
    assert_eq!(output.status.code(), Some(2), "init {args:?}: {output:?}");
    output
}

/// Builds `deck` in `project` with the CLI: exit 0, nothing on stderr (so no diagnostics),
/// stdout exactly `Wrote dist/slides.html (<slides> slides, …)`. Returns the HTML.
fn build(project: &Path, deck: &str, slides: usize) -> String {
    let output = presenter(project).args(["build", deck]).output().unwrap();
    let stderr = text(&output.stderr);
    assert!(output.status.success(), "build {deck}: {stderr}");
    assert_eq!(stderr, "", "build {deck} printed diagnostics");
    let wrote = Regex::new(&format!(
        r"^Wrote dist/slides\.html \({slides} slides, \d+(\.\d)? (B|kB|MB)\)\n$"
    ))
    .unwrap();
    let stdout = text(&output.stdout);
    assert!(wrote.is_match(&stdout), "{stdout:?}");
    read(project.join("dist/slides.html"))
}

/// `presenter dump <deck>` in `project`, parsed.
fn dump(project: &Path, deck: &str, context: bool) -> Value {
    let mut cmd = presenter(project);
    cmd.args(["dump", deck]);
    if context {
        cmd.arg("--context");
    }
    let output = cmd.output().unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Every file below `dir`, as `/`-separated relative paths.
fn files(dir: &Path) -> BTreeSet<String> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            let rel = entry.path().strip_prefix(dir).unwrap();
            rel.to_string_lossy().replace('\\', "/")
        })
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// The `<style id="presenter-theme">` element's contents.
fn theme_stylesheet(html: &str) -> &str {
    let start = html
        .find(r#"<style id="presenter-theme">"#)
        .expect("theme stylesheet");
    let end = start + html[start..].find("</style>").expect("</style>");
    &html[start..end]
}

/// The custom-property declarations (`--name: value`) of a stylesheet.
fn custom_properties(css: &str) -> Vec<String> {
    let declaration = Regex::new(r"(--[\w-]+)\s*:\s*([^;]+);").unwrap();
    declaration
        .captures_iter(css)
        .map(|caps| format!("{}: {}", &caps[1], caps[2].trim()))
        .collect()
}

#[test]
fn init_then_build_uses_the_child_theme() {
    let tmp = tempfile::tempdir().unwrap();
    let stdout = init(tmp.path(), &["talk"]);
    assert!(
        stdout.contains("Next steps:\n  cd talk\n  presenter serve slides.md "),
        "{stdout}"
    );
    assert!(
        stdout.contains("\n  presenter build slides.md "),
        "{stdout}"
    );
    let project = tmp.path().join("talk");
    assert_eq!(
        files(&project),
        set(&[
            ".gitignore",
            "README.md",
            "presenter.toml",
            "slides.md",
            "theme/styles/theme.css",
            "theme/templates/layouts/title.html",
            "theme/theme.toml",
        ])
    );
    assert!(
        read(project.join(".gitignore"))
            .lines()
            .any(|l| l == "dist/")
    );

    let html = build(&project, "slides.md", 6);
    // The child theme's tokens are in the theme stylesheet, after the default theme's.
    let theme_css = read(project.join("theme/styles/theme.css"));
    let tokens = custom_properties(&theme_css);
    assert!(tokens.len() >= 3, "{tokens:?}");
    let stylesheet = theme_stylesheet(&html);
    let child_at = stylesheet
        .find(theme_css.trim())
        .expect("child theme CSS in the theme stylesheet");
    let default_at = stylesheet.find("@default/").expect("default theme CSS");
    assert!(default_at < child_at);
    for token in &tokens {
        assert!(stylesheet.contains(token.as_str()), "{token}");
    }
    // The overridden title layout adds the front matter's `event` to the title slide.
    assert!(
        html.contains(r#"<p class="event">Event name, City</p>"#),
        "event line"
    );
    let context = dump(&project, "slides.md", true);
    assert_eq!(
        context["theme"]["chain"],
        serde_json::json!(["talk", "default"])
    );
    assert_eq!(context["meta"]["title"], "talk");
    let layouts: Vec<&str> = context["slides"]
        .as_array()
        .unwrap()
        .iter()
        .map(|slide| slide["layout"].as_str().unwrap())
        .collect();
    assert_eq!(layouts[0], "title");
}

#[test]
fn starter_deck_shows_the_promised_features() {
    let tmp = tempfile::tempdir().unwrap();
    init(tmp.path(), &["talk", "-q"]);
    let project = tmp.path().join("talk");
    let deck = dump(&project, "slides.md", false);
    let slides = deck["slides"].as_array().unwrap();
    assert_eq!(slides.len(), 5, "horizontal slides");
    assert_eq!(
        slides[1]["children"].as_array().unwrap().len(),
        1,
        "a `--` slide"
    );
    assert_eq!(deck["features"]["notes"], true);
    assert_eq!(deck["features"]["code"], true);
    let html: String = slides
        .iter()
        .flat_map(|slide| {
            let own = slide["blocks"].as_array().unwrap().iter();
            let children = slide["children"].as_array().unwrap().iter();
            own.chain(children.flat_map(|child| child["blocks"].as_array().unwrap().iter()))
        })
        .map(|block| block["html"].as_str().unwrap_or_default())
        .collect();
    for needle in [
        r#"<div class="fragment">"#,
        r#"<div class="columns">"#,
        r#"<div class="column">"#,
        r#"data-line-numbers="1-2|4-5""#,
    ] {
        assert!(html.contains(needle), "{needle}");
    }
    assert!(slides[0]["notes"].is_string() && slides[4]["notes"].is_string());
}

#[test]
fn scaffold_has_no_diagnostics_and_passes_check() {
    let tmp = tempfile::tempdir().unwrap();
    for (dir, deck, flag) in [
        ("md", "slides.md", None),
        ("nb", "slides.ipynb", Some("--notebook")),
    ] {
        let mut args = vec![dir, "-q"];
        args.extend(flag);
        init(tmp.path(), &args);
        let project = tmp.path().join(dir);
        let built = pipeline::build_deck(&BuildOptions::new(project.join(deck))).unwrap();
        let diagnostics: Vec<String> = built.diagnostics.iter().map(|d| d.to_string()).collect();
        assert!(diagnostics.is_empty(), "{deck}: {diagnostics:?}");

        // `presenter check`: no warnings.
        let output = presenter(&project)
            .args(["check", deck, "--deny-warnings"])
            .output()
            .unwrap();
        let stderr = text(&output.stderr);
        let stdout = text(&output.stdout);
        assert!(output.status.success(), "check {deck}: {stdout}{stderr}");
        assert!(
            !stdout.contains("warning") && !stderr.contains("warning"),
            "check {deck}: {stdout}{stderr}"
        );
    }
}

#[test]
fn init_refuses_a_non_empty_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("talk");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), "keep me").unwrap();
    let output = init_fails(tmp.path(), &["talk"]);
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with("error: talk is not empty (it contains notes.txt)"),
        "{stderr}"
    );
    assert_eq!(text(&output.stdout), "");
    assert_eq!(files(&dir), set(&["notes.txt"]));
    assert_eq!(read(dir.join("notes.txt")), "keep me");

    // A hidden file counts too, and so does the current directory.
    let hidden = tmp.path().join("hidden");
    std::fs::create_dir_all(hidden.join(".git")).unwrap();
    init_fails(tmp.path(), &["hidden"]);
    for args in [&[][..], &["."]] {
        let stderr = text(&init_fails(&dir, args).stderr);
        assert!(
            stderr.contains("the current directory is not empty"),
            "{args:?}: {stderr}"
        );
    }
    // clap rejects an empty DIR.
    init_fails(&dir, &[""]);
    assert_eq!(files(&dir), set(&["notes.txt"]));

    // A file is not a directory.
    let output = init_fails(tmp.path(), &["talk/notes.txt"]);
    assert!(
        text(&output.stderr).contains("exists and is not a directory"),
        "{output:?}"
    );
}

/// Every entry below `dir`, directories included.
fn entries(dir: &Path) -> BTreeSet<String> {
    walkdir::WalkDir::new(dir)
        .min_depth(1)
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            let rel = entry.path().strip_prefix(dir).unwrap();
            rel.to_string_lossy().replace('\\', "/")
        })
        .collect()
}

#[test]
fn init_never_writes_where_it_cannot_scaffold() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    std::fs::write(cwd.join("notes.txt"), "keep me").unwrap();
    std::fs::create_dir(cwd.join("src")).unwrap();
    std::fs::write(cwd.join("README.md"), "mine").unwrap();
    let before = entries(cwd);

    for (dir, message) in [
        // `new/..` is the (non-empty) current directory once `new` exists.
        ("newdir/..", "`..` follows newdir, which does not exist"),
        ("a/b/../../src", "`..` follows a/b, which does not exist"),
        (
            "src/..",
            "src/.. is not empty (it contains README.md, notes.txt, src)",
        ),
        ("notes.txt/sub", "notes.txt is not a directory"),
        ("src/../notes.txt", "exists and is not a directory"),
    ] {
        let output = init_fails(cwd, &[dir]);
        let stderr = text(&output.stderr);
        assert!(stderr.starts_with("error: "), "{dir}: {stderr}");
        assert!(stderr.contains(message), "{dir}: {stderr}");
        assert_eq!(text(&output.stdout), "");
        assert_eq!(entries(cwd), before, "{dir}");
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("nowhere", cwd.join("dangling")).unwrap();
        std::os::unix::fs::symlink("notes.txt", cwd.join("to-file")).unwrap();
        std::os::unix::fs::symlink("src", cwd.join("to-src")).unwrap();
        // A loop, and a link that runs through a file.
        std::os::unix::fs::symlink("loop", cwd.join("loop")).unwrap();
        std::os::unix::fs::symlink("notes.txt/x", cwd.join("via-file")).unwrap();
        std::fs::write(cwd.join("src/main.rs"), "fn main() {}").unwrap();
        let before = entries(cwd);
        for (dir, message) in [
            (
                "dangling",
                "dangling exists and is not a directory (a broken symbolic link)",
            ),
            ("dangling/sub", "(a broken symbolic link)"),
            ("to-file", "to-file exists and is not a directory"),
            ("to-src", "to-src is not empty (it contains main.rs)"),
            (
                "loop",
                "loop exists and is not a directory (a broken symbolic link: ",
            ),
            ("loop/sub", "loop exists and is not a directory (a broken"),
            (
                "via-file",
                "via-file exists and is not a directory (a broken symbolic link: ",
            ),
            (
                "via-file/sub",
                "via-file exists and is not a directory (a broken",
            ),
        ] {
            let stderr = text(&init_fails(cwd, &[dir]).stderr);
            assert!(stderr.contains(message), "{dir}: {stderr}");
            assert_eq!(entries(cwd), before, "{dir}");
        }
        // A link to an empty directory is fine: the files go into its target.
        std::fs::create_dir(cwd.join("empty")).unwrap();
        std::os::unix::fs::symlink("empty", cwd.join("to-empty")).unwrap();
        init(cwd, &["to-empty", "-q"]);
        assert!(cwd.join("empty/slides.md").is_file());
    }

    // `..` through existing directories is fine.
    init(cwd, &["src/../talk", "-q"]);
    assert!(cwd.join("talk/slides.md").is_file());
}

#[test]
fn init_creates_missing_directories_or_fills_an_empty_one() {
    let tmp = tempfile::tempdir().unwrap();
    init(tmp.path(), &["a/b/c", "-q"]);
    assert!(tmp.path().join("a/b/c/presenter.toml").is_file());
    // `.` components name the directory before them, also one that does not exist yet.
    init(tmp.path(), &["new/.", "-q"]);
    assert!(tmp.path().join("new/slides.md").is_file());
    init(tmp.path(), &["./other/./sub/.", "-q"]);
    assert!(tmp.path().join("other/sub/slides.md").is_file());

    // `presenter init` without DIR scaffolds the (empty) current directory; the name is
    // the directory's.
    let here = tmp.path().join("Data Talk");
    std::fs::create_dir(&here).unwrap();
    let stdout = init(&here, &[]);
    assert!(
        stdout.starts_with("Created a presentation in the current directory:"),
        "{stdout}"
    );
    assert!(!stdout.contains("cd "), "{stdout}");
    assert!(stdout.contains("\n  presenter serve slides.md"), "{stdout}");
    let context = dump(&here, "slides.md", true);
    assert_eq!(context["meta"]["title"], "Data Talk");
    assert_eq!(context["theme"]["name"], "data-talk");

    // A directory with a space is quoted in the `cd` hint.
    let stdout = init(tmp.path(), &["my talk"]);
    assert!(stdout.contains("\n  cd 'my talk'\n"), "{stdout}");
}

/// The `cd` line of `init`'s next steps.
fn cd_line(stdout: &str) -> &str {
    stdout
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("cd "))
        .unwrap_or_else(|| panic!("no cd line: {stdout}"))
}

#[test]
fn the_cd_hint_works_in_a_shell() {
    let tmp = tempfile::tempdir().unwrap();
    for (args, expected) in [
        (&["--", "-dash"][..], "cd ./-dash"),
        (&["--", "-my talk"][..], "cd './-my talk'"),
        (&["it's", "--notebook"][..], r"cd 'it'\''s'"),
        (&["a/b"][..], "cd a/b"),
    ] {
        let stdout = init(tmp.path(), args);
        let cd = cd_line(&stdout);
        assert_eq!(cd, expected);
        if cfg!(unix) {
            let status = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("{cd} && test -f presenter.toml"))
                .current_dir(tmp.path())
                .status()
                .unwrap();
            assert!(status.success(), "{cd}");
        }
    }
}

#[test]
fn quiet_init_prints_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(init(tmp.path(), &["-q", "talk"]), "");
    assert!(tmp.path().join("talk/slides.md").is_file());
}

#[test]
fn notebook_scaffold_is_nbformat_4_5_and_builds() {
    let tmp = tempfile::tempdir().unwrap();
    let stdout = init(tmp.path(), &["nb", "--notebook"]);
    assert!(
        stdout.contains("\n  cd nb\n  presenter serve slides.ipynb "),
        "{stdout}"
    );
    let project = tmp.path().join("nb");
    let names = files(&project);
    assert!(names.contains("slides.ipynb") && !names.contains("slides.md"));
    assert!(names.contains("theme/theme.toml"));

    let notebook: Value = serde_json::from_str(&read(project.join("slides.ipynb"))).unwrap();
    assert_eq!(notebook["nbformat"], 4);
    assert_eq!(notebook["nbformat_minor"], 5);
    let cells = notebook["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 3);
    let id = Regex::new(r"^[a-zA-Z0-9_-]{1,64}$").unwrap();
    let mut ids = BTreeSet::new();
    for cell in cells {
        let cell_id = cell["id"].as_str().expect("nbformat 4.5 cell id");
        assert!(id.is_match(cell_id) && ids.insert(cell_id), "{cell_id}");
        let slide_type = cell["metadata"]["slideshow"]["slide_type"]
            .as_str()
            .unwrap();
        assert!(["slide", "subslide", "fragment", "skip", "notes", "-"].contains(&slide_type));
        assert!(cell["source"].is_array());
        match cell["cell_type"].as_str().unwrap() {
            "code" => assert!(cell["outputs"].is_array() && cell.get("execution_count").is_some()),
            "markdown" => assert!(cell.get("outputs").is_none()),
            other => panic!("unexpected cell type {other}"),
        }
    }

    let html = build(&project, "slides.ipynb", 2);
    assert!(html.contains(r#"<p class="event">Event name, City</p>"#));
    let deck = dump(&project, "slides.ipynb", false);
    assert_eq!(deck["meta"]["title"], "nb");
    assert_eq!(deck["slides"][0]["layout"], "title");
    assert_eq!(deck["features"]["notebook"], true);
    let blocks = deck["slides"][1]["blocks"].as_array().unwrap();
    let cell = blocks
        .iter()
        .find(|b| b["type"] == "cell")
        .expect("code cell");
    assert_eq!(cell["fragment"], true);
    assert_eq!(cell["outputs"].as_array().unwrap().len(), 2);
}

#[test]
fn full_theme_copies_the_default_theme() {
    let tmp = tempfile::tempdir().unwrap();
    let stdout = init(tmp.path(), &["full", "--full-theme"]);
    assert!(
        stdout.contains("a copy of the built-in \"default\" theme"),
        "{stdout}"
    );
    let project = tmp.path().join("full");

    let builtin = presenter::vendor::list("themes/default/");
    assert!(!builtin.is_empty());
    // Every file but the built-in theme's README, which documents it in the repository
    // and whose links break in a copy (audit F48).
    let builtin: Vec<String> = builtin
        .into_iter()
        .filter(|key| key != "themes/default/README.md")
        .collect();
    let expected: BTreeSet<String> = builtin
        .iter()
        .map(|key| format!("theme/{}", &key["themes/default/".len()..]))
        .collect();
    assert!(!project.join("theme/README.md").exists());
    let theme_files: BTreeSet<String> = files(&project)
        .into_iter()
        .filter(|f| f.starts_with("theme/"))
        .collect();
    assert_eq!(theme_files, expected);
    for key in &builtin {
        let rel = &key["themes/default/".len()..];
        if rel == "theme.toml" {
            continue;
        }
        let copied = std::fs::read(project.join("theme").join(rel)).unwrap();
        assert_eq!(
            copied,
            presenter::vendor::get(key).unwrap().as_ref(),
            "{rel}"
        );
    }
    // The manifest is the default theme's, renamed after the project, with a header and a
    // description that say where it came from.
    let manifest_source = read(project.join("theme/theme.toml"));
    assert!(
        manifest_source.starts_with(
            "# This presentation's theme, copied from the built-in \"default\" theme by\n"
        ),
        "{manifest_source}"
    );
    assert!(!manifest_source.contains("The built-in theme"));
    let manifest: toml::Table = manifest_source.parse().unwrap();
    let mut original: toml::Table = presenter::vendor::get_str("themes/default/theme.toml")
        .unwrap()
        .parse()
        .unwrap();
    assert!(!manifest.contains_key("extends"));
    assert_eq!(manifest["name"].as_str(), Some("full"));
    let description = manifest["description"].as_str().unwrap();
    assert!(description.contains("copied from the built-in default theme"));
    original.insert("name".into(), "full".into());
    original.insert("description".into(), description.into());
    assert_eq!(manifest, original);

    let html = build(&project, "slides.md", 6);
    let context = dump(&project, "slides.md", true);
    assert_eq!(context["theme"]["chain"], serde_json::json!(["full"]));
    assert_no_child_theme_text(&project, "slides.md", &html);
}

/// A `--full-theme` project says nothing about the child theme it does not have: no
/// `event` front matter (the copied title layout does not show it), no inheritance.
fn assert_no_child_theme_text(project: &Path, deck: &str, html: &str) {
    for file in [deck, "presenter.toml", "README.md"] {
        let text = read(project.join(file));
        for needle in [
            "event",
            "extends the built-in",
            "layouts/title.html adds",
            "eject",
        ] {
            assert!(!text.contains(needle), "{file}: {needle}");
        }
    }
    assert!(!html.contains(r#"class="event""#));
    assert!(!html.contains("Event name"));
}

#[test]
fn full_theme_notebook_builds() {
    let tmp = tempfile::tempdir().unwrap();
    init(tmp.path(), &["nb", "-q", "--notebook", "--full-theme"]);
    let project = tmp.path().join("nb");
    let notebook: Value = serde_json::from_str(&read(project.join("slides.ipynb"))).unwrap();
    assert_eq!(notebook["cells"].as_array().unwrap().len(), 3);
    let html = build(&project, "slides.ipynb", 2);
    assert_no_child_theme_text(&project, "slides.ipynb", &html);
    let deck = dump(&project, "slides.ipynb", false);
    assert_eq!(deck["meta"]["title"], "nb");
    assert_eq!(deck["meta"]["author"], "Your Name");
}

#[test]
fn presenter_toml_lists_every_setting_with_its_default() {
    let tmp = tempfile::tempdir().unwrap();
    init(tmp.path(), &["talk", "-q"]);
    let source = read(tmp.path().join("talk/presenter.toml"));

    // As written, only the theme is set.
    let mut expected = Config::default();
    expected.deck.theme = "theme".into();
    assert_eq!(Config::from_toml_str(&source).unwrap(), expected);

    // Switching every `#key = value` line on (array continuation lines included) gives
    // the built-in defaults, plus the title/author/date examples (unset by default).
    let setting = Regex::new(r#"^#(\w+ = |\s+"|\])"#).unwrap();
    let enabled: Vec<&str> = source
        .lines()
        .map(|line| {
            if setting.is_match(line) {
                &line[1..]
            } else {
                line
            }
        })
        .collect();
    let enabled = enabled.join("\n");
    let mut all = Config::from_toml_str(&enabled).unwrap();
    for field in [
        &mut all.deck.title,
        &mut all.deck.author,
        &mut all.deck.date,
    ] {
        assert!(
            field.take().is_some(),
            "example values for title/author/date"
        );
    }
    expected.reveal = Config::builtin_reveal_defaults();
    assert_eq!(all, expected);

    // ... and every key of config.md §2 is there.
    let keys = |table: &serde_json::Map<String, Value>| -> BTreeSet<String> {
        table
            .iter()
            .flat_map(|(name, inner)| {
                let inner = inner.as_object().unwrap();
                inner.keys().map(move |key| format!("{name}.{key}"))
            })
            .collect()
    };
    let written: toml::Table = enabled.parse().unwrap();
    let written = serde_json::to_value(written).unwrap();
    let mut schema = serde_json::to_value(Config::default()).unwrap();
    schema["reveal"] = Value::Object(Config::builtin_reveal_defaults());
    assert_eq!(
        keys(written.as_object().unwrap()),
        keys(schema.as_object().unwrap())
    );
}

#[test]
fn names_survive_every_file_format() {
    let tmp = tempfile::tempdir().unwrap();
    for (dir, deck, flag) in [
        ("md", "slides.md", None),
        ("nb", "slides.ipynb", Some("--notebook")),
    ] {
        let mut args = vec![dir, "-q", "--name", TRICKY_NAME];
        args.extend(flag);
        init(tmp.path(), &args);
        let project = tmp.path().join(dir);
        build(&project, deck, if flag.is_some() { 2 } else { 6 });
        let context = dump(&project, deck, true);
        assert_eq!(context["meta"]["title"], TRICKY_NAME, "{deck}");
        assert_eq!(context["slides"][0]["title"], TRICKY_NAME, "{deck}");
        let theme_name = context["theme"]["name"].as_str().unwrap();
        assert_eq!(
            theme_name,
            "rust-fast-safe-x-5-1-a-b-c-d-e-f-c-path-it-s-p-g-deck"
        );
    }
}

#[test]
fn names_with_urls_stay_plain_text() {
    let tmp = tempfile::tempdir().unwrap();
    for (dir, deck, flag) in [
        ("md", "slides.md", None),
        ("nb", "slides.ipynb", Some("--notebook")),
    ] {
        let mut args = vec![dir, "-q", "--name", URL_NAME];
        args.extend(flag);
        init(tmp.path(), &args);
        let project = tmp.path().join(dir);
        let html = build(&project, deck, if flag.is_some() { 2 } else { 6 });
        let context = dump(&project, deck, true);
        assert_eq!(context["meta"]["title"], URL_NAME, "{deck}");
        assert_eq!(context["slides"][0]["title"], URL_NAME, "{deck}");
        let heading = context["slides"][0]["blocks"][0]["html"].as_str().unwrap();
        assert!(heading.starts_with("<h1"), "{heading}");
        assert!(
            !heading.contains("<a ") && !heading.contains('\\'),
            "{heading}"
        );
        assert!(heading.contains("https://example.com/x_y"), "{heading}");
        assert!(!html.contains("href=\"https://example.com"), "{deck}");
        assert!(html.contains(&format!(">{URL_NAME}</h1>")), "{deck}");
    }
}

#[test]
fn bad_names_are_usage_errors() {
    let tmp = tempfile::tempdir().unwrap();
    for name in [
        "",
        "  ",
        "two\nlines",
        "x\u{fffe}y",
        "x\u{ffff}y",
        "a\u{2028}b",
    ] {
        let output = init_fails(tmp.path(), &["talk", "--name", name]);
        assert!(text(&output.stderr).starts_with("error: "), "{output:?}");
        assert!(!tmp.path().join("talk").exists(), "{name:?}");
    }
}

#[test]
fn every_scaffold_file_is_embedded_and_used() {
    let embedded: BTreeSet<String> = presenter::vendor::list("scaffold/")
        .iter()
        .map(|key| key["scaffold/".len()..].to_string())
        .collect();
    // rust-embed keeps dotfiles: the scaffold's .gitignore is embedded (in release builds
    // too, see the module docs).
    assert!(embedded.contains(".gitignore"), "{embedded:?}");
    let gitignore = presenter::vendor::get_str("scaffold/.gitignore").unwrap();
    assert!(gitignore.lines().any(|line| line == "dist/"));

    let tmp = tempfile::tempdir().unwrap();
    init(tmp.path(), &["md", "-q"]);
    init(tmp.path(), &["nb", "-q", "--notebook"]);
    init(tmp.path(), &["full", "-q", "--full-theme"]);
    let mut created = files(&tmp.path().join("md"));
    created.extend(files(&tmp.path().join("nb")));
    assert_eq!(created, embedded);

    // No placeholder is left in any variant.
    for dir in ["md", "nb", "full"] {
        let project = tmp.path().join(dir);
        for file in files(&project) {
            let contents = read(project.join(&file));
            for placeholder in PLACEHOLDERS {
                assert!(
                    !contents.contains(placeholder),
                    "{dir}/{file}: {placeholder}"
                );
            }
        }
    }

    // The embedded scaffold is exactly the directory on disk (minus OS junk, which
    // src/vendor.rs excludes).
    let junk = |path: &String| {
        let name = path.rsplit('/').next().unwrap_or(path);
        name == ".DS_Store"
            || name.starts_with("._")
            || name == "Thumbs.db"
            || name.ends_with(".swp")
            || name.ends_with('~')
    };
    let on_disk: BTreeSet<String> =
        files(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/scaffold"))
            .into_iter()
            .filter(|path| !junk(path))
            .collect();
    assert_eq!(embedded, on_disk);
}
