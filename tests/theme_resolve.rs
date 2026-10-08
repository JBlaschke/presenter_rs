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
    assert_eq!(origins(&styles), [("default", "styles/theme.css")]);
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
            ("child-theme", "styles/child.css")
        ]
    );
    assert_eq!(
        styles[1].bytes,
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
