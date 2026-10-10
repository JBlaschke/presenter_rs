//! Theme resolution contract tests (docs/spec/theme-format.md §1–§3, §5): the built-in
//! `default` theme, the `child-theme` and `broken-cycle` fixtures, and chains built in
//! temporary directories (deep merges, entry order, depth cap, missing root templates,
//! fresh disk reads, `@<theme>/` names in the template environment).
//!
//! Only contract-level facts about the built-in theme are asserted (its name, the required
//! templates, `styles/theme.css`, `white`/`monokai`, the four documented layouts), so these
//! tests hold for any default theme that follows theme-format.md.

use std::fs;
use std::path::{Path, PathBuf};

use presenter::error::Error;
use presenter::theme::{REQUIRED_TEMPLATES, Theme, ThemeSource, builtin_names, resolve};
use serde_json::{Value, json};

fn crate_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn fixtures() -> PathBuf {
    crate_root().join("tests/fixtures/themes")
}

fn child_theme() -> Theme {
    resolve("child-theme", &[fixtures().as_path()]).unwrap()
}

fn chain_names(theme: &Theme) -> Vec<&str> {
    theme.chain.iter().map(|dir| dir.name.as_str()).collect()
}

/// The (theme, rel_path) pairs of theme files.
fn origins(files: &[presenter::theme::ThemeFile]) -> Vec<(&str, &str)> {
    files
        .iter()
        .map(|f| (f.theme.as_str(), f.rel_path.as_str()))
        .collect()
}

/// `Error::Theme`'s (name, message), or a panic.
fn theme_error(result: presenter::Result<Theme>) -> (String, String) {
    match result {
        Err(Error::Theme { name, message }) => (name, message),
        Err(other) => panic!("expected Error::Theme, got {other:?}"),
        Ok(theme) => panic!("expected an error, got chain {:?}", chain_names(&theme)),
    }
}

/// Writes `files` (path → contents) below `dir`.
fn write_files(dir: &Path, files: &[(&str, &str)]) {
    for (path, contents) in files {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
}

/// A complete root theme called `name` in `dir` (every required template) plus `extra`.
fn write_root_theme(dir: &Path, manifest: &str, extra: &[(&str, &str)]) {
    write_files(dir, &[("theme.toml", manifest)]);
    for template in REQUIRED_TEMPLATES {
        write_files(dir, &[(&format!("templates/{template}"), template)]);
    }
    write_files(dir, extra);
}

// ---------------------------------------------------------------------------------------
// The built-in theme

#[test]
fn builtin_default_resolves_without_search_roots() {
    assert!(builtin_names().contains(&"default".to_string()));
    let theme = resolve("default", &[]).unwrap();
    assert_eq!(chain_names(&theme), ["default"]);
    assert_eq!(theme.name(), "default");
    assert_eq!(
        theme.chain[0].source,
        ThemeSource::Embedded {
            prefix: "themes/default/".into()
        }
    );
    assert!(theme.watch_dirs().is_empty(), "nothing on disk to watch");
    for template in REQUIRED_TEMPLATES {
        assert!(theme.has_template(template), "{template}");
        let (from, source) = theme.read_template(template).unwrap().unwrap();
        assert_eq!(from, "default");
        assert!(!source.is_empty(), "{template}");
    }
    assert_eq!(theme.reveal_theme(), "white");
    assert_eq!(theme.highlight_style(), "monokai");
    let styles = theme.style_entries().unwrap();
    assert_eq!(
        origins(&styles),
        [
            ("default", "styles/theme.css"),
            ("default", "styles/notebook.css")
        ]
    );
    for layout in ["blank", "default", "section", "title"] {
        assert!(theme.layouts().contains(&layout.to_string()), "{layout}");
    }
}

#[test]
fn unknown_theme_lists_the_search_roots() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let (name, message) = theme_error(resolve("nope", &[first.path(), second.path()]));
    assert_eq!(name, "nope");
    assert!(message.contains("not found"), "{message}");
    for root in [first.path(), second.path()] {
        let tried = root.join("nope").display().to_string();
        assert!(message.contains(&tried), "{tried} missing in: {message}");
    }
    assert!(message.contains("built-in themes: default"), "{message}");

    let (_, message) = theme_error(resolve("nope", &[]));
    assert!(message.contains("no search directories"), "{message}");
}

#[test]
fn coinciding_search_roots_are_tried_and_reported_once() {
    // The pipeline passes config dir, source dir and CWD, which are often the same.
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join("nope")).unwrap();
    let roots = [tmp.path(), tmp.path(), tmp.path()];
    let (_, message) = theme_error(resolve("nope", &roots));
    let tried = tmp.path().join("nope").display().to_string();
    assert_eq!(
        message.matches(&tried).count(),
        2,
        "listed + note: {message}"
    );
    assert_eq!(
        message.matches("exists but has no theme.toml").count(),
        1,
        "{message}"
    );

    // Relative roots are reported as absolute directories, once.
    let spec = "no-such-theme-dir";
    let (_, message) = theme_error(resolve(spec, &[Path::new(""), Path::new(".")]));
    let absolute = std::env::current_dir().unwrap().join(spec);
    assert!(
        message.contains(&format!("in: {}; built-in", absolute.display())),
        "{message}"
    );
}

#[test]
fn directory_without_manifest_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join("mytheme")).unwrap();
    let (_, message) = theme_error(resolve("mytheme", &[tmp.path()]));
    assert!(
        message.contains("exists but has no theme.toml"),
        "{message}"
    );
}

#[test]
fn wrong_case_manifest_is_not_a_manifest_on_any_platform() {
    // A complete theme except that its manifest is `Theme.toml`: on macOS `is_file` finds
    // it case-insensitively, but theme files are matched exactly (as on Linux), so this is
    // the regular "no manifest" error with a hint, never a failed read.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("t");
    write_root_theme(&dir, "name = \"t\"", &[]);
    fs::rename(dir.join("theme.toml"), dir.join("Theme.toml")).unwrap();
    let (name, message) = theme_error(resolve("t", &[tmp.path()]));
    assert_eq!(name, "t");
    assert!(message.starts_with("not found;"), "{message}");
    assert!(
        message.contains(&format!(
            "{} exists but has no theme.toml; it has Theme.toml, but the name must be \
             theme.toml exactly",
            dir.display()
        )),
        "{message}"
    );
    assert!(!message.contains("disappeared"), "{message}");

    // A wrong-case directory named like a built-in theme falls back to the built-in one.
    let shadow = tmp.path().join("default");
    write_root_theme(&shadow, "name = \"default\"", &[]);
    fs::rename(shadow.join("theme.toml"), shadow.join("THEME.TOML")).unwrap();
    let theme = resolve("default", &[tmp.path()]).unwrap();
    assert_eq!(
        theme.chain[0].source,
        ThemeSource::Embedded {
            prefix: "themes/default/".into()
        }
    );
}

// ---------------------------------------------------------------------------------------
// The child-theme fixture

#[test]
fn child_theme_chain_and_sources() {
    let theme = child_theme();
    assert_eq!(chain_names(&theme), ["child-theme", "default"]);
    assert_eq!(theme.name(), "child-theme");
    let dir = fs::canonicalize(fixtures().join("child-theme")).unwrap();
    assert_eq!(
        theme.chain[0].source,
        ThemeSource::Disk { dir: dir.clone() }
    );
    assert!(matches!(
        theme.chain[1].source,
        ThemeSource::Embedded { .. }
    ));
    assert_eq!(theme.watch_dirs(), [dir]);

    // The same theme by relative and by absolute path.
    let by_path = resolve("tests/fixtures/themes/child-theme", &[crate_root()]).unwrap();
    assert_eq!(chain_names(&by_path), ["child-theme", "default"]);
    let absolute = fixtures().join("child-theme").display().to_string();
    let by_absolute = resolve(&absolute, &[]).unwrap();
    assert_eq!(chain_names(&by_absolute), ["child-theme", "default"]);
}

#[test]
fn child_theme_template_lookup_walks_the_chain() {
    let theme = child_theme();
    let (from, title) = theme.read_template("layouts/title.html").unwrap().unwrap();
    assert_eq!(from, "child-theme");
    assert!(title.contains("child-title"), "{title}");

    let (from, _) = theme
        .read_template("layouts/default.html")
        .unwrap()
        .unwrap();
    assert_eq!(from, "default");

    let (from, parent_title) = theme
        .read_template("@default/layouts/title.html")
        .unwrap()
        .unwrap();
    assert_eq!(from, "default");
    assert_ne!(parent_title, title);

    // `@<theme>/` reads that theme only.
    assert_eq!(
        theme
            .read_template("@child-theme/layouts/default.html")
            .unwrap(),
        None
    );
    assert!(!theme.has_template("@child-theme/layouts/default.html"));
    assert!(theme.has_template("@child-theme/layouts/title.html"));
    assert!(theme.has_template("layouts/default.html"));
    assert_eq!(theme.read_template("layouts/nope.html").unwrap(), None);
    assert_eq!(
        theme.read_template("../theme.toml").unwrap(),
        None,
        "stays in templates/"
    );
    assert!(!theme.has_template("../theme.toml"));
    // A template has exactly one name: no `.`/`..`/empty segments (like minijinja's
    // path_loader), so `layouts/../slide.html` is not another name for `slide.html`.
    for name in [
        "./layouts//title.html",
        "layouts/./title.html",
        "layouts//title.html",
        "/layouts/title.html",
        "layouts/../slide.html",
        "@default/layouts/../slide.html",
        "layouts\\title.html",
    ] {
        assert_eq!(theme.read_template(name).unwrap(), None, "{name}");
        assert!(!theme.has_template(name), "{name}");
    }
    assert!(theme.has_template("slide.html"));
    assert!(!theme.has_template("layouts/nope.html"));

    let (name, message) = match theme.read_template("@nope/deck.html") {
        Err(Error::Theme { name, message }) => (name, message),
        other => panic!("expected Error::Theme, got {other:?}"),
    };
    assert_eq!(name, "child-theme");
    assert!(message.contains("no theme named 'nope'"), "{message}");
    assert!(!theme.has_template("@nope/deck.html"));
}

#[test]
fn child_theme_styles_are_concatenated_parent_first() {
    let theme = child_theme();
    let styles = theme.style_entries().unwrap();
    assert_eq!(
        origins(&styles),
        [
            ("default", "styles/theme.css"),
            ("default", "styles/notebook.css"),
            ("child-theme", "styles/child.css")
        ]
    );
    assert_eq!(
        styles[2].bytes,
        fs::read(fixtures().join("child-theme/styles/child.css")).unwrap()
    );
}

#[test]
fn child_theme_overrides_and_merges_settings() {
    let theme = child_theme();
    let reveal = theme.reveal_defaults();
    assert_eq!(reveal["transition"], json!("fade"));
    assert_eq!(reveal["slideNumber"], json!("c/t"));
    assert_eq!(theme.vars()["accent"], json!("#2a76dd"));
    assert_eq!(theme.highlight_style(), "zenburn", "child override");
    assert_eq!(theme.reveal_theme(), "white", "inherited");
    let layouts = theme.layouts();
    for layout in ["blank", "default", "section", "title"] {
        assert!(layouts.contains(&layout.to_string()), "{layout}");
    }
    let mut sorted = layouts.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(layouts, sorted, "sorted, no duplicates");
}

#[test]
fn child_theme_environment_renders_the_title_layout() {
    let theme = child_theme();
    let mut env = theme.environment(true).unwrap();
    // `attrs` is a T15 helper; a stub stands in for it here (values are not escaped).
    env.add_filter(
        "attrs",
        |attrs: minijinja::Value| -> Result<minijinja::Value, minijinja::Error> {
            let mut out = String::new();
            for key in attrs.try_iter()? {
                out.push_str(&format!(" {key}=\"{}\"", attrs.get_item(&key)?));
            }
            Ok(minijinja::Value::from_safe_string(out))
        },
    );
    let context = json!({
        "meta": { "title": "Rust", "subtitle": "<Data> & more" },
        "theme": { "name": "child-theme", "vars": { "accent": "#2a76dd" } },
        "slide": {
            "id": "intro", "layout": "title", "classes": ["big"], "attrs": { "data-x": "1" },
            "blocks": [ { "type": "html", "html": "<h1>Hello</h1>" } ],
            "notes": "<p>Say hi</p>", "children": []
        }
    });
    let html = env
        .render_str(r#"{% include "layouts/title.html" %}"#, &context)
        .map_err(|e| presenter::error::template_error(e, true))
        .unwrap();
    assert!(
        html.contains(r#"<section id="intro" class="layout-title child-title big" data-x="1">"#),
        "{html}"
    );
    assert!(
        html.contains("<h1>Hello</h1>"),
        "block.html from default: {html}"
    );
    assert!(html.contains("background: #2a76dd"), "{html}");
    assert!(
        html.contains(r#"<p class="subtitle">&lt;Data&gt; &amp; more</p>"#),
        "auto-escaped: {html}"
    );
    assert!(
        html.contains(r#"<aside class="notes"><p>Say hi</p></aside>"#),
        "{html}"
    );
    assert_eq!(html.matches("<section").count(), 1, "{html}");
}

// ---------------------------------------------------------------------------------------
// Errors

#[test]
fn broken_cycle_is_an_error() {
    let (name, message) = theme_error(resolve(
        "tests/fixtures/themes/broken-cycle/a",
        &[crate_root()],
    ));
    assert_eq!(name, "tests/fixtures/themes/broken-cycle/a");
    assert!(message.contains("cycle"), "{message}");
    assert!(
        message.contains("'cycle-a'") && message.contains("'cycle-b'"),
        "{message}"
    );
}

#[test]
fn missing_root_templates_are_listed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    write_files(
        &root,
        &[
            ("theme.toml", r#"name = "incomplete""#),
            ("templates/deck.html", "d"),
            ("templates/slide.html", "s"),
            ("templates/block.html", "b"),
        ],
    );
    let (name, message) = theme_error(resolve("root", &[tmp.path()]));
    assert_eq!(name, "root");
    assert!(message.contains("'incomplete'"), "{message}");
    assert!(
        message.contains("templates/layouts/default.html"),
        "{message}"
    );
    assert!(message.contains("templates/cell.html"), "{message}");
    assert!(!message.contains("templates/deck.html"), "{message}");

    // A child cannot make up for an incomplete root: the root must provide them.
    write_files(
        &tmp.path().join("child"),
        &[
            ("theme.toml", "name = \"child\"\nextends = \"../root\""),
            ("templates/layouts/default.html", "l"),
            ("templates/cell.html", "c"),
        ],
    );
    let (name, message) = theme_error(resolve("child", &[tmp.path()]));
    assert_eq!(name, "child");
    assert!(
        message.contains("templates/layouts/default.html"),
        "{message}"
    );
}

#[test]
fn unknown_parent_and_bad_manifests_are_theme_errors() {
    let tmp = tempfile::tempdir().unwrap();
    write_files(
        &tmp.path().join("orphan"),
        &[(
            "theme.toml",
            "name = \"orphan\"\nextends = \"no-such-parent\"",
        )],
    );
    let (name, message) = theme_error(resolve("orphan", &[tmp.path()]));
    assert_eq!(name, "no-such-parent");
    assert!(message.contains("extended by theme 'orphan'"), "{message}");

    write_files(
        &tmp.path().join("typo"),
        &[("theme.toml", "name = \"typo\"\n\n[styles]\nentrys = []\n")],
    );
    let (name, message) = theme_error(resolve("typo", &[tmp.path()]));
    assert_eq!(name, "typo");
    assert!(message.contains("theme.toml"), "{message}");
    assert!(message.contains("line 4"), "{message}");
    assert!(message.contains("did you mean 'entries'"), "{message}");
}

#[test]
fn duplicate_names_in_a_chain_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    write_files(
        &tmp.path().join("mine"),
        &[("theme.toml", "name = \"default\"\nextends = \"default\"")],
    );
    let (_, message) = theme_error(resolve("mine", &[tmp.path()]));
    assert!(
        message.contains("two themes in the chain are named 'default'"),
        "{message}"
    );
}

#[test]
fn theme_names_must_work_in_at_template_names() {
    let tmp = tempfile::tempdir().unwrap();
    for (dir, name) in [("slash", "acme/dark"), ("at", "@acme"), ("space", " acme")] {
        write_root_theme(
            &tmp.path().join(dir),
            &format!("name = {}", json!(name)),
            &[],
        );
        let (_, message) = theme_error(resolve(dir, &[tmp.path()]));
        assert!(message.contains("invalid theme name"), "{name}: {message}");
        assert!(message.contains("theme.toml"), "{message}");
    }
}

#[test]
fn chains_longer_than_eight_themes_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    write_root_theme(&tmp.path().join("t0"), r#"name = "t0""#, &[]);
    for i in 1..=8 {
        let manifest = format!("name = \"t{i}\"\nextends = \"../t{}\"", i - 1);
        write_files(
            &tmp.path().join(format!("t{i}")),
            &[("theme.toml", &manifest)],
        );
    }
    // t7 → … → t0 is eight themes: allowed.
    let theme = resolve("t7", &[tmp.path()]).unwrap();
    assert_eq!(theme.chain.len(), 8);
    assert_eq!(theme.watch_dirs().len(), 8);
    // t8 → … → t0 is nine.
    let (_, message) = theme_error(resolve("t8", &[tmp.path()]));
    assert!(message.contains("longer than 8"), "{message}");
}

// ---------------------------------------------------------------------------------------
// Disk chains

/// `root` (complete) ← `child` (extends "../root") in a temp dir.
fn disk_chain(tmp: &Path) -> Theme {
    write_root_theme(
        &tmp.join("root"),
        r#"
        name = "root"
        [styles]
        entries = ["styles/a.css", "styles/b.css"]
        reveal_theme = "black"
        [scripts]
        entries = ["scripts/r.js"]
        [reveal]
        transition = "slide"
        controls = false
        nested = { a = 1, b = 2 }
        [vars]
        accent = "red"
        font = "serif"
        "#,
        &[
            ("styles/a.css", "a"),
            ("styles/b.css", "b"),
            ("scripts/r.js", "r"),
            ("templates/layouts/section.html", "root section"),
            ("assets/logo.svg", "root logo"),
            ("assets/fonts/x.woff2", "root font"),
        ],
    );
    write_files(
        &tmp.join("child"),
        &[
            (
                "theme.toml",
                r#"
                name = "child"
                extends = "../root"
                [styles]
                entries = ["styles/c.css"]
                [scripts]
                entries = ["scripts/c.js"]
                [reveal]
                transition = "fade"
                nested = { b = 3, c = 4 }
                [vars]
                accent = "blue"
                "#,
            ),
            ("styles/c.css", "c"),
            ("scripts/c.js", "cjs"),
            ("templates/layouts/two-up.html", "child two-up"),
            ("assets/logo.svg", "child logo"),
        ],
    );
    resolve("child", &[tmp]).unwrap()
}

#[test]
fn disk_chain_merges_and_concatenates_parent_first() {
    let tmp = tempfile::tempdir().unwrap();
    let theme = disk_chain(tmp.path());
    assert_eq!(chain_names(&theme), ["child", "root"]);
    assert_eq!(
        Value::Object(theme.reveal_defaults()),
        json!({"transition": "fade", "controls": false, "nested": {"a": 1, "b": 3, "c": 4}})
    );
    assert_eq!(
        Value::Object(theme.vars()),
        json!({"accent": "blue", "font": "serif"})
    );
    assert_eq!(theme.reveal_theme(), "black");
    assert_eq!(theme.highlight_style(), "monokai", "built-in fallback");

    let styles = theme.style_entries().unwrap();
    assert_eq!(
        origins(&styles),
        [
            ("root", "styles/a.css"),
            ("root", "styles/b.css"),
            ("child", "styles/c.css")
        ]
    );
    let contents: Vec<&[u8]> = styles.iter().map(|f| f.bytes.as_slice()).collect();
    assert_eq!(contents, [b"a".as_slice(), b"b", b"c"]);
    let scripts = theme.script_entries().unwrap();
    assert_eq!(
        origins(&scripts),
        [("root", "scripts/r.js"), ("child", "scripts/c.js")]
    );

    assert_eq!(theme.layouts(), ["default", "section", "two-up"]);
    let canonical = |name: &str| fs::canonicalize(tmp.path().join(name)).unwrap();
    assert_eq!(theme.watch_dirs(), [canonical("child"), canonical("root")]);
}

#[test]
fn assets_are_looked_up_child_first() {
    let tmp = tempfile::tempdir().unwrap();
    let theme = disk_chain(tmp.path());
    assert_eq!(
        theme.read_asset("logo.svg").unwrap(),
        Some(b"child logo".to_vec())
    );
    assert_eq!(
        theme.read_asset("./fonts/x.woff2").unwrap(),
        Some(b"root font".to_vec())
    );
    assert_eq!(theme.read_asset("nope.png").unwrap(), None);
    assert_eq!(
        theme.read_asset("../theme.toml").unwrap(),
        None,
        "stays in assets/"
    );
    assert_eq!(theme.read_asset("../../root/theme.toml").unwrap(), None);
}

#[test]
fn missing_style_entry_is_a_theme_error() {
    let tmp = tempfile::tempdir().unwrap();
    disk_chain(tmp.path());
    fs::remove_file(tmp.path().join("child/styles/c.css")).unwrap();
    let theme = resolve("child", &[tmp.path()]).unwrap();
    match theme.style_entries() {
        Err(Error::Theme { name, message }) => {
            assert_eq!(name, "child");
            assert!(message.contains("styles/c.css"), "{message}");
        }
        other => panic!("expected Error::Theme, got {other:?}"),
    }
}

#[test]
fn entries_outside_the_theme_directory_are_reported_as_such() {
    let tmp = tempfile::tempdir().unwrap();
    disk_chain(tmp.path());
    // The file exists, but entries must stay inside their theme's directory.
    write_files(tmp.path(), &[("shared/base.css", "shared")]);
    let child = tmp.path().join("child/theme.toml");
    let manifest = fs::read_to_string(&child).unwrap();
    let absolute = tmp.path().join("shared/base.css").display().to_string();
    for (entry, problem) in [
        ("../shared/base.css", "must stay inside the theme directory"),
        (absolute.as_str(), "must be a relative path"),
        ("styles/..", "does not name a file"),
    ] {
        let edited = manifest.replace("\"styles/c.css\"", &json!(entry).to_string());
        fs::write(&child, edited).unwrap();
        let theme = resolve("child", &[tmp.path()]).unwrap();
        match theme.style_entries() {
            Err(Error::Theme { name, message }) => {
                assert_eq!(name, "child");
                assert!(message.contains(entry), "{message}");
                assert!(message.contains(problem), "{entry}: {message}");
                assert!(!message.contains("not found"), "{message}");
            }
            other => panic!("expected Error::Theme for {entry}, got {other:?}"),
        }
    }
}

#[test]
fn disk_lookups_match_file_names_exactly() {
    // On a case-insensitive file system (macOS) `is_file` would accept these; the embedded
    // tree and Linux do not, so disk themes must not either.
    let tmp = tempfile::tempdir().unwrap();
    let theme = disk_chain(tmp.path());
    assert!(theme.has_template("layouts/two-up.html"));
    assert!(!theme.has_template("layouts/TWO-UP.html"));
    assert!(!theme.has_template("Layouts/two-up.html"));
    assert_eq!(theme.read_template("layouts/Two-Up.html").unwrap(), None);
    assert!(!theme.has_template("@child/layouts/TWO-UP.html"));
    assert_eq!(theme.read_asset("LOGO.svg").unwrap(), None);
    assert_eq!(
        theme.read_asset("Fonts/x.woff2").unwrap(),
        None,
        "directories too"
    );
    assert!(theme.chain[0].list("TEMPLATES").is_empty());
    assert!(!theme.chain[0].exists("STYLES/c.css"));

    let child = child_theme();
    assert!(!child.has_template("layouts/TITLE.html"));
    assert!(!child.has_template("@default/layouts/TITLE.html"));
    assert!(!child.has_layout("TITLE"));
}

#[test]
fn has_layout_agrees_with_layouts() {
    let tmp = tempfile::tempdir().unwrap();
    let theme = disk_chain(tmp.path());
    write_files(
        &tmp.path().join("child/templates/layouts"),
        &[("sub/x.html", "nested"), (".hidden.html", "hidden")],
    );
    let child = child_theme();
    let candidates = [
        "default",
        "section",
        "two-up",
        "title",
        "blank",
        "nope",
        "TWO-UP",
        "Title",
        "../slide",
        "../deck",
        "sub/x",
        "sub",
        ".hidden",
        "",
        "two-up.html",
    ];
    for theme in [&theme, &child] {
        let layouts = theme.layouts();
        for name in candidates {
            assert_eq!(
                theme.has_layout(name),
                layouts.contains(&name.to_string()),
                "{name} in {:?}: {layouts:?}",
                chain_names(theme)
            );
        }
    }
    assert_eq!(theme.layouts(), ["default", "section", "two-up"]);
    assert!(child.has_layout("title") && child.has_layout("blank"));
}

#[test]
fn disk_themes_are_read_fresh_on_every_call() {
    let tmp = tempfile::tempdir().unwrap();
    let theme = disk_chain(tmp.path());
    let section = tmp.path().join("root/templates/layouts/section.html");
    assert_eq!(
        theme.read_template("layouts/section.html").unwrap(),
        Some(("root".to_string(), "root section".to_string()))
    );
    let env = theme.environment(false).unwrap();
    assert_eq!(
        env.render_str(r#"{% include "layouts/section.html" %}"#, ())
            .unwrap(),
        "root section"
    );

    fs::write(&section, "edited section").unwrap();
    assert_eq!(
        theme
            .read_template("layouts/section.html")
            .unwrap()
            .unwrap()
            .1,
        "edited section"
    );
    let env = theme.environment(false).unwrap();
    assert_eq!(
        env.render_str(r#"{% include "layouts/section.html" %}"#, ())
            .unwrap(),
        "edited section"
    );

    // A file added to the child now shadows the parent's.
    write_files(
        &tmp.path().join("child"),
        &[("templates/layouts/section.html", "child section")],
    );
    assert_eq!(
        theme
            .read_template("layouts/section.html")
            .unwrap()
            .unwrap(),
        ("child".to_string(), "child section".to_string())
    );
    fs::write(tmp.path().join("root/styles/a.css"), "a2").unwrap();
    assert_eq!(theme.style_entries().unwrap()[0].bytes, b"a2");
}

#[test]
fn child_templates_can_extend_the_parent_version_by_theme_name() {
    let tmp = tempfile::tempdir().unwrap();
    disk_chain(tmp.path());
    write_files(
        &tmp.path().join("root"),
        &[(
            "templates/layouts/default.html",
            "<section>{% block content %}root content{% endblock %}</section>",
        )],
    );
    write_files(
        &tmp.path().join("child"),
        &[(
            "templates/layouts/default.html",
            r#"{% extends "@root/layouts/default.html" %}{% block content %}child: {{ super() }} {{ x }}{% endblock %}"#,
        )],
    );
    let theme = resolve("child", &[tmp.path()]).unwrap();
    let env = theme.environment(false).unwrap();
    let html = env
        .get_template("layouts/default.html")
        .unwrap()
        .render(json!({ "x": "<&>" }))
        .unwrap();
    assert_eq!(html, "<section>child: root content &lt;&amp;&gt;</section>");
}

// ---------------------------------------------------------------------------------------
// Audit fixes (A4): symbolic links, `Theme::problems`, "did you mean"

/// A child theme `t` of `default` in `dir/t` with `manifest_extra` appended to its manifest.
fn child_of_default(dir: &Path, manifest_extra: &str, files: &[(&str, &str)]) -> Theme {
    let manifest = format!("name = \"t\"\nextends = \"default\"\n{manifest_extra}");
    write_files(&dir.join("t"), &[("theme.toml", manifest.as_str())]);
    write_files(&dir.join("t"), files);
    resolve("t", &[dir]).unwrap()
}

/// F08: a symbolic link inside a disk theme whose target is outside it finds nothing (read,
/// exists, list, assets); links that stay inside the theme work.
#[cfg(unix)]
#[test]
fn symbolic_links_out_of_a_theme_are_not_followed() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    write_files(
        tmp.path(),
        &[("secret/hosts", "SECRET"), ("secret/b.css", "x{}")],
    );
    let theme = child_of_default(
        tmp.path(),
        "[styles]\nentries = [\"styles/theme.css\"]\n",
        &[
            (
                "styles/theme.css",
                "body { background: url(../assets/logo.png); }",
            ),
            ("assets/real.png", "REAL"),
        ],
    );
    let root = tmp.path().join("t");
    symlink(
        tmp.path().join("secret/hosts"),
        root.join("assets/logo.png"),
    )
    .unwrap();
    symlink(tmp.path().join("secret"), root.join("assets/shared")).unwrap();
    symlink(root.join("assets/real.png"), root.join("assets/alias.png")).unwrap();
    symlink(
        tmp.path().join("secret/b.css"),
        root.join("styles/linked.css"),
    )
    .unwrap();
    let dir = &theme.chain[0];
    for rel in [
        "assets/logo.png",
        "assets/shared/hosts",
        "styles/linked.css",
    ] {
        assert_eq!(dir.read(rel).unwrap(), None, "{rel}");
        assert!(!dir.exists(rel), "{rel}");
    }
    assert_eq!(theme.read_asset("logo.png").unwrap(), None);
    assert_eq!(theme.read_asset("shared/hosts").unwrap(), None);
    // A link to a file of the theme itself is fine.
    assert_eq!(
        theme.read_asset("alias.png").unwrap(),
        Some(b"REAL".to_vec())
    );
    assert_eq!(
        dir.list("assets"),
        ["assets/alias.png", "assets/real.png"],
        "list() lists what read() accepts"
    );
    // A theme reached through a linked directory is that directory, and works.
    symlink(&root, tmp.path().join("t-link")).unwrap();
    let linked = resolve("t-link", &[tmp.path()]).unwrap();
    assert_eq!(
        linked.read_asset("real.png").unwrap(),
        Some(b"REAL".to_vec())
    );
}

/// F08 end to end: a deck whose theme links a file outside it into `assets/` does not get
/// that file: the stylesheet's `url()` names nothing, so the build fails instead of
/// inlining it.
#[cfg(unix)]
#[test]
fn a_linked_file_outside_the_theme_is_never_inlined() {
    let tmp = tempfile::tempdir().unwrap();
    write_files(tmp.path(), &[("secret.txt", "SECRET-CONTENT-123")]);
    child_of_default(
        tmp.path(),
        "[styles]\nentries = [\"styles/theme.css\"]\n",
        &[(
            "styles/theme.css",
            "body { background-image: url(../assets/logo.png); }",
        )],
    );
    fs::create_dir_all(tmp.path().join("t/assets")).unwrap();
    std::os::unix::fs::symlink(
        tmp.path().join("secret.txt"),
        tmp.path().join("t/assets/logo.png"),
    )
    .unwrap();
    write_files(tmp.path(), &[("d.md", "---\ntheme: ./t\n---\n# A\n")]);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_presenter"))
        .current_dir(tmp.path())
        .args(["build", "d.md", "-o", "o.html"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("logo.png"), "{stderr}");
    if let Ok(html) = fs::read_to_string(tmp.path().join("o.html")) {
        // base64 of "SECRET-CONTENT-123"
        assert!(!html.contains("U0VDUkVULUNPTlRFTlQtMTIz"));
    }
}

/// F10: `Theme::problems` finds what makes a build fail without rendering.
#[test]
fn theme_problems_find_what_a_build_fails_on() {
    let tmp = tempfile::tempdir().unwrap();
    let theme = child_of_default(
        tmp.path(),
        "[styles]\nentries = [\"styles/nope.css\", \"styles/ok.css\"]\nhighlight_style = \
         \"monokia\"\n\n[scripts]\nentries = [\"scripts/extra.js\", \"../x.js\"]\n\n[reveal]\n\
         plugins = [\"bogus\", \"zoom\"]\n",
        &[
            ("styles/ok.css", "x{}"),
            ("templates/layouts/title.html", "{%- endiff %}"),
            ("templates/.gitkeep", ""),
        ],
    );
    let found = theme.problems();
    let summary: Vec<(&str, Option<String>, Option<usize>)> = found
        .iter()
        .map(|d| {
            let file = d.path.as_ref().map(|p| {
                // Joined with `/` (the path is native, `t\theme.toml` on Windows).
                p.strip_prefix(tmp.path().canonicalize().unwrap())
                    .unwrap()
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/")
            });
            (d.code, file, d.span.as_ref().map(|s| s.start_line))
        })
        .collect();
    let manifest = Some("t/theme.toml".to_string());
    assert_eq!(
        summary,
        [
            ("theme-style-entry", manifest.clone(), Some(4)),
            ("theme-script-entry", manifest.clone(), Some(8)),
            ("theme-script-entry", manifest.clone(), Some(8)),
            ("theme-highlight-style", manifest.clone(), Some(5)),
            (
                "theme-template",
                Some("t/templates/layouts/title.html".to_string()),
                Some(1)
            ),
            ("theme-plugins", manifest, Some(11)),
        ],
        "{found:#?}"
    );
    assert!(
        found
            .iter()
            .all(|d| d.level == presenter::error::Level::Error)
    );
    let messages: Vec<&str> = found.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(
        messages[0],
        "theme 't': [styles] entry 'styles/nope.css' is not found (paths are relative to the \
         theme directory)"
    );
    assert!(messages[2].contains("'../x.js' must stay inside the theme directory"));
    assert!(
        messages[3]
            == "theme 't': [styles] highlight_style 'monokia' (not monokai, zenburn or none, so \
                a CSS file) is not found (paths are relative to the theme directory)",
        "{}",
        messages[3]
    );
    assert!(messages[4].contains("syntax error"), "{}", messages[4]);
    assert!(
        messages[5].contains("unknown reveal plugin 'bogus'"),
        "{}",
        messages[5]
    );

    // A sound theme has none, the built-in one included.
    assert!(resolve("default", &[]).unwrap().problems().is_empty());
    assert!(child_theme().problems().is_empty());
}

/// F10 (b): a path-valued `highlight_style` must stay inside its theme, as `@<theme>/<path>`
/// allows no `..` (it used to load `assets/x.css` of the chain instead).
#[test]
fn highlight_style_files_stay_inside_their_theme() {
    let tmp = tempfile::tempdir().unwrap();
    write_files(tmp.path(), &[("x.css", "/* intended */")]);
    let theme = child_of_default(
        tmp.path(),
        "[styles]\nhighlight_style = \"../x.css\"\n",
        &[("assets/x.css", "/* WRONG FILE */")],
    );
    let (name, message) = match theme.highlight_style_file() {
        Err(Error::Theme { name, message }) => (name, message),
        other => panic!("expected Error::Theme, got {other:?}"),
    };
    assert_eq!(name, "t");
    assert!(
        message.contains("highlight_style '../x.css'")
            && message.contains("must stay inside the theme directory"),
        "{message}"
    );
    assert_eq!(theme.problems()[0].code, "theme-highlight-style");

    let theme = child_of_default(
        tmp.path(),
        "[styles]\nhighlight_style = \"styles/hl.css\"\n",
        &[("styles/hl.css", ".hljs{}")],
    );
    let file = theme.highlight_style_file().unwrap().unwrap();
    assert_eq!(
        (file.theme.as_str(), file.rel_path.as_str()),
        ("t", "styles/hl.css")
    );
    assert_eq!(file.bytes, b".hljs{}");
    for builtin in ["monokai", "zenburn", "none"] {
        let theme = child_of_default(
            tmp.path(),
            &format!("[styles]\nhighlight_style = \"{builtin}\"\n"),
            &[],
        );
        assert_eq!(theme.highlight_style_file().unwrap(), None, "{builtin}");
    }
    assert_eq!(
        resolve("default", &[])
            .unwrap()
            .highlight_style_file()
            .unwrap(),
        None
    );
}

/// F40: an unknown theme name suggests a built-in theme or a theme directory next to where
/// it looked, and a stock reveal.js theme name points at `reveal_theme`.
#[test]
fn unknown_themes_get_a_did_you_mean() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, message) = theme_error(resolve("defualt", &[tmp.path()]));
    assert!(message.ends_with(" (did you mean 'default'?)"), "{message}");
    let (_, message) = theme_error(resolve("Default", &[]));
    assert!(message.contains("did you mean 'default'?"), "{message}");

    write_root_theme(&tmp.path().join("themes/talk"), "name = \"talk\"", &[]);
    write_files(tmp.path(), &[("themes/notes/readme.txt", "no manifest")]);
    let (_, message) = theme_error(resolve("themes/tlak", &[tmp.path()]));
    assert!(
        message.ends_with(" (did you mean 'themes/talk'?)"),
        "{message}"
    );
    let (_, message) = theme_error(resolve("./themes/takl", &[tmp.path()]));
    assert!(
        message.contains("did you mean './themes/talk'?"),
        "{message}"
    );
    // A directory without a theme.toml is no suggestion; nothing close, no hint.
    let (_, message) = theme_error(resolve("themes/note", &[tmp.path()]));
    assert!(!message.contains("did you mean"), "{message}");
    let (_, message) = theme_error(resolve("zzzzzz", &[tmp.path()]));
    assert!(!message.contains("did you mean"), "{message}");

    let (_, message) = theme_error(resolve("black", &[tmp.path()]));
    assert!(
        message.contains("'black' is a reveal.js theme")
            && message.contains("reveal_theme = \"black\""),
        "{message}"
    );
}

/// F40: an unknown layout gets the near miss among the theme's layouts.
#[test]
fn unknown_layouts_get_a_did_you_mean() {
    let theme = resolve("default", &[]).unwrap();
    let message = theme.unknown_layout_message("slide 2", "titel");
    assert!(
        message.starts_with("slide 2: unknown layout 'titel' (theme 'default' has: "),
        "{message}"
    );
    assert!(
        message.ends_with("; did you mean 'title'?); using the default layout"),
        "{message}"
    );
    let message = theme.unknown_layout_message("slide 1", "nope");
    assert!(!message.contains("did you mean"), "{message}");
    assert!(
        message.ends_with("); using the default layout"),
        "{message}"
    );
}

/// `(code, path relative to `root`, level)` of each diagnostic.
fn problem_summary(
    found: &[presenter::error::Diagnostic],
    root: &Path,
) -> Vec<(String, String, presenter::error::Level)> {
    let root = root.canonicalize().unwrap();
    found
        .iter()
        .map(|d| {
            // Joined with `/` (the path is native, `t\theme.toml` on Windows).
            let path = d.path.as_ref().map_or(String::new(), |p| {
                p.strip_prefix(&root)
                    .unwrap()
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/")
            });
            (d.code.to_string(), path, d.level)
        })
        .collect()
}

/// F10 (review): a template that does not compile is an error for a deck whose build loads
/// it (the required templates, the deck's layouts and what these include, extend or import
/// by a literal name) and a warning otherwise: a layout no slide uses, a partial nothing
/// includes (a mention in a comment does not count), a parent's template the child
/// overrides. `Theme::problems`, which knows no deck, reports every one as an error.
#[test]
fn deck_problems_warn_about_templates_the_deck_does_not_use() {
    use presenter::error::Level::{Error as E, Warning as W};
    let tmp = tempfile::tempdir().unwrap();
    write_files(
        &tmp.path().join("p"),
        &[
            ("theme.toml", "name = \"p\"\nextends = \"default\"\n"),
            ("templates/layouts/title.html", "{%- endiff %}"),
        ],
    );
    let theme = {
        write_files(
            &tmp.path().join("t"),
            &[
                ("theme.toml", "name = \"t\"\nextends = \"../p\"\n"),
                (
                    "templates/layouts/unused.html",
                    "<section>{%- endiff %}</section>",
                ),
                ("templates/partials/draft.html", "{% if x %}"),
                ("templates/partials/broken.html", "{{ 1 + }}"),
                (
                    "templates/layouts/fancy.html",
                    "{#- {% include \"partials/draft.html\" %} -#}\n\
                     {%- extends \"layouts/default.html\" -%}\n\
                     {% block x %}{% include ['partials/missing.html', \"partials/broken.html\"] %}{% endblock %}",
                ),
                (
                    "templates/layouts/title.html",
                    "{%- extends \"@default/layouts/title.html\" %}",
                ),
            ],
        );
        resolve("t", &[tmp.path()]).unwrap()
    };
    let layouts = |names: &[&str]| -> std::collections::BTreeSet<String> {
        names.iter().map(|name| name.to_string()).collect()
    };
    let template = |path: &str| ("theme-template".to_string(), path.to_string());
    let summary = |found: &[presenter::error::Diagnostic]| {
        problem_summary(found, tmp.path())
            .into_iter()
            .map(|(code, path, level)| ((code, path), level))
            .collect::<Vec<_>>()
    };
    // The child's templates first, then the parent's (the chain's order).
    let unused = template("t/templates/layouts/unused.html");
    let broken = template("t/templates/partials/broken.html");
    let draft = template("t/templates/partials/draft.html");
    let parent = template("p/templates/layouts/title.html");
    assert_eq!(
        summary(&theme.deck_problems(&layouts(&["default", "title"]))),
        [
            (unused.clone(), W),
            (broken.clone(), W),
            (draft.clone(), W),
            (parent.clone(), W)
        ]
    );
    // `fancy` includes the broken partial; an unknown layout falls back to the default one.
    assert_eq!(
        summary(&theme.deck_problems(&layouts(&["fancy", "unused", "nope"]))),
        [
            (unused.clone(), E),
            (broken.clone(), E),
            (draft.clone(), W),
            (parent.clone(), W)
        ]
    );
    assert_eq!(
        summary(&theme.problems()),
        [(unused, E), (broken, E), (draft, E), (parent, E)]
    );
    let found = theme.deck_problems(&layouts(&["default"]));
    assert!(
        found[0].message.ends_with(
            "(this deck does not use the template, so its build does not fail on it; a deck \
             that uses it does)"
        ),
        "{}",
        found[0].message
    );
    // The parent's template is reached when the child's version extends it by name.
    write_files(
        &tmp.path().join("t"),
        &[(
            "templates/layouts/title.html",
            "{%- extends \"@p/layouts/title.html\" %}",
        )],
    );
    let found = theme.deck_problems(&layouts(&["title"]));
    assert_eq!(
        summary(&found)[3],
        (template("p/templates/layouts/title.html"), E)
    );
}

/// F10 (b, review): in a helper's argument, `@<theme>/<path>` stays inside that theme:
/// `..` cannot climb out of it into the chain's `assets/` (`@t/../x.css` named
/// `assets/x.css`) or into another theme. Inside the theme, `..` works.
#[test]
fn theme_helper_paths_stay_inside_their_theme() {
    use presenter::render::{normalize_helper_path, resolve_helper_path};
    for (path, expected) in [
        ("@t/../x.css", None),
        ("@t/../@default/styles/theme.css", None),
        ("./@t/a/../../x.css", None),
        ("@t\\..\\x.css", None),
        ("@t/a/../b.css", Some("@t/b.css")),
        ("@t/assets/../assets/x.css", Some("@t/assets/x.css")),
        ("./@t//x.css", Some("@t/x.css")),
        ("img/../x.css", Some("x.css")),
        ("vendor/../x.css", Some("x.css")),
    ] {
        assert_eq!(normalize_helper_path(path).as_deref(), expected, "{path}");
    }
    let tmp = tempfile::tempdir().unwrap();
    write_files(tmp.path(), &[("x.css", "/* intended */")]);
    let theme = child_of_default(tmp.path(), "", &[("assets/x.css", "/* assets */")]);
    assert_eq!(resolve_helper_path(&theme, "@t/../x.css").unwrap(), None);
    assert_eq!(
        resolve_helper_path(&theme, "@t/assets/../assets/x.css")
            .unwrap()
            .map(|(bytes, _)| bytes),
        Some(b"/* assets */".to_vec())
    );
    assert_eq!(
        resolve_helper_path(&theme, "x.css")
            .unwrap()
            .map(|(bytes, _)| bytes),
        Some(b"/* assets */".to_vec())
    );
}

/// F08 (review): a file that a symbolic link takes out of its theme is reported as such:
/// `theme-link-outside` warnings for each such link (not for links inside the theme), and
/// the messages about an entry or a manifest name the link instead of saying "not found".
#[cfg(unix)]
#[test]
fn links_out_of_a_theme_are_named() {
    use presenter::error::Level::Warning as W;
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    write_files(
        tmp.path(),
        &[
            ("secret/hosts", "SECRET"),
            ("secret/b.css", "x{}"),
            ("t2/x", ""),
        ],
    );
    let theme = child_of_default(
        tmp.path(),
        "[styles]\nentries = [\"styles/linked.css\"]\n",
        &[("assets/real.png", "REAL")],
    );
    let root = tmp.path().join("t");
    fs::create_dir_all(root.join("styles")).unwrap();
    symlink(
        tmp.path().join("secret/hosts"),
        root.join("assets/logo.png"),
    )
    .unwrap();
    symlink(tmp.path().join("secret"), root.join("assets/shared")).unwrap();
    symlink(root.join("assets/real.png"), root.join("assets/alias.png")).unwrap();
    symlink(
        tmp.path().join("secret/b.css"),
        root.join("styles/linked.css"),
    )
    .unwrap();
    symlink(tmp.path().join("nowhere"), root.join("assets/dangling.png")).unwrap();

    let dir = &theme.chain[0];
    assert!(dir.linked_outside("assets/logo.png"));
    assert!(dir.linked_outside("assets/shared/hosts"));
    assert!(!dir.linked_outside("assets/alias.png"));
    assert!(!dir.linked_outside("assets/nope.png"));
    assert!(!dir.linked_outside("assets/dangling.png"));

    let found = theme.problems();
    let style = ("theme-style-entry".to_string(), "t/theme.toml".to_string());
    let link = |path: &str| ("theme-link-outside".to_string(), path.to_string());
    assert_eq!(
        problem_summary(&found, tmp.path())
            .into_iter()
            .map(|(code, path, level)| ((code, path), level))
            .collect::<Vec<_>>(),
        [
            (style, presenter::error::Level::Error),
            (link("t/assets/logo.png"), W),
            (link("t/assets/shared"), W),
            (link("t/styles/linked.css"), W),
        ]
    );
    assert!(
        found[0].message.contains(
            "entry 'styles/linked.css' is a symbolic link (or below one) to a file outside \
             the theme directory"
        ),
        "{}",
        found[0].message
    );
    assert!(
        found[2]
            .message
            .starts_with("theme 't': assets/shared is a symbolic link to ")
            && found[2]
                .message
                .contains("presenter ignores it and everything below it, so it counts as missing"),
        "{}",
        found[2].message
    );
    let Err(Error::Theme { message, .. }) = theme.style_entries() else {
        panic!("expected Error::Theme");
    };
    assert!(message.contains("is a symbolic link"), "{message}");

    // A manifest that is a link out of its directory.
    symlink(root.join("theme.toml"), tmp.path().join("t2/theme.toml")).unwrap();
    let (_, message) = theme_error(resolve("t2", &[tmp.path()]));
    assert!(
        message.contains("t2 has a theme.toml that is a symbolic link (or below one) to a file outside the theme directory"),
        "{message}"
    );
}
