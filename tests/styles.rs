//! T42: SCSS theme stylesheets (grass over the virtual stylesheet tree), reveal.js's theme
//! template as a Sass library, and the inlining of local `@import`s
//! (`docs/spec/theme-format.md` §5, `docs/spec/vendored-assets.md` §2).
//!
//! The example theme is `examples/custom-theme-scss/theme` ("midnight"); its deck is built
//! standalone, in site mode and in serve mode here.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use assert_cmd::Command;
use predicates::prelude::*;
use presenter::assets::emit::OutputMode;
use presenter::assets::registry::{AssetData, AssetRegistry};
use presenter::error::Error;
use presenter::pipeline::{BuildOptions, build_deck};
use presenter::render::{self, BuildInfo};
use presenter::theme::styles::{
    MAX_IMPORT_DEPTH, VirtualFs, compile_entry, compile_scss, inline_imports, reveal_fallback,
};
use presenter::theme::{self, Theme};
use regex::Regex;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn default_theme() -> Theme {
    theme::resolve("default", &[]).expect("built-in default theme")
}

fn example_dir() -> PathBuf {
    root().join("examples/custom-theme-scss")
}

fn example_theme() -> Theme {
    theme::resolve(example_dir().join("theme").to_str().unwrap(), &[]).expect("example theme")
}

/// A disk theme at `dir` with this manifest and files (paths relative to `dir`).
fn disk_theme(dir: &Path, manifest: &str, files: &[(&str, &str)]) -> Theme {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("theme.toml"), manifest).unwrap();
    for (rel, text) in files {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    theme::resolve(dir.to_str().unwrap(), &[]).unwrap()
}

/// A theme `t` extending `default` whose only entry is `styles/<entry>`.
fn scss_theme(dir: &Path, entry: &str, files: &[(&str, &str)]) -> Theme {
    disk_theme(
        &dir.join("t"),
        &format!("name = \"t\"\nextends = \"default\"\n[styles]\nentries = [\"styles/{entry}\"]\n"),
        files,
    )
}

/// The theme stylesheet of `theme` with placeholders, and the registry it filled.
fn compile(theme: &Theme) -> presenter::Result<(String, AssetRegistry)> {
    let mut registry = AssetRegistry::new();
    let css = render::compile_theme_css(theme, &mut registry)?;
    Ok((css, registry))
}

/// The part of a compiled theme stylesheet that comes from the entry `key`.
fn entry_part<'c>(css: &'c str, key: &str) -> &'c str {
    let marker = format!("/* {key} */");
    let start = css
        .find(&marker)
        .unwrap_or_else(|| panic!("{marker} in the output"));
    &css[start..]
}

/// `css` without whitespace, so `--r-background-color: #fff` and `…:#fff` compare equal.
fn squeeze(css: &str) -> String {
    css.chars().filter(|c| !c.is_whitespace()).collect()
}

/// True if `css` still refers to reveal's font directory relatively (`url(fonts/…)`,
/// `url('./fonts/…')`, `@import url(./fonts/…)`, …).
fn has_relative_font_reference(css: &str) -> bool {
    Regex::new(r#"(?i)url\(\s*["']?(?:\./)?fonts/"#)
        .unwrap()
        .is_match(css)
}

fn style_error(err: Error) -> (String, String) {
    match err {
        Error::Style { file, message } => (file, message),
        other => panic!("expected Error::Style, got {other:?}"),
    }
}

/// `files` as the `(&str, &str)` pairs [`disk_theme`] takes.
fn as_refs(files: &[(String, String)]) -> Vec<(&str, &str)> {
    files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect()
}

fn identity(_: &str, css: String) -> presenter::Result<String> {
    Ok(css)
}

// ---------------------------------------------------------------------------------------
// reveal.js's own theme sources
// ---------------------------------------------------------------------------------------

#[test]
fn reveal_white_scss_compiles_through_the_virtual_fs() {
    let theme = default_theme();
    // The vendored file itself, by its alias and by its embedded path.
    for path in [
        "reveal/theme/white.scss",
        "vendor/reveal/css/theme/white.scss",
    ] {
        let css = compile_scss(&theme, path).unwrap();
        let flat = squeeze(&css);
        assert!(flat.contains("--r-background-color:#fff;"), "{path}");
        assert!(flat.contains("--r-main-color:#222;"), "{path}");
        // `color.scale($active-color, $lightness: 15%)` from sass:color.
        assert!(flat.contains("--r-link-color-hover:#"), "{path}");
        assert!(
            css.contains("section.has-dark-background"),
            "mixins: {path}"
        );
        // Sass passes the font import through; it names reveal's font directory.
        assert!(
            flat.contains("@importurl(\"./fonts/source-sans-pro/source-sans-pro.css\")")
                || flat.contains("@importurl('./fonts/source-sans-pro/source-sans-pro.css')"),
            "{css}"
        );
    }

    // Inlining the import (next to white.scss) leaves no relative font reference.
    let key = "vendor/reveal/css/theme/white.scss";
    let css = compile_scss(&theme, key).unwrap();
    let mut finished = Vec::new();
    let inlined = inline_imports(&theme, key, &css, &mut |file, css| {
        if !finished.iter().any(|seen| seen == file) {
            finished.push(file.to_string());
        }
        Ok(css)
    })
    .unwrap();
    // `finish` sees the font CSS's text first (it is spliced in before white's rules).
    assert_eq!(
        finished,
        [
            "vendor/reveal/css/theme/fonts/source-sans-pro/source-sans-pro.css",
            key
        ]
    );
    assert!(inlined.imports.is_empty());
    let inlined = inlined.css;
    assert!(!inlined.contains("@import"), "{inlined}");
    assert_eq!(inlined.matches("@font-face").count(), 4);
    assert!(inlined.contains("url('./source-sans-pro-regular.woff')"));
    assert!(squeeze(&inlined).contains("--r-background-color:#fff;"));
}

#[test]
fn every_stock_reveal_theme_compiles() {
    let theme = default_theme();
    let names = presenter::vendor::list("vendor/reveal/css/theme/");
    let scss: Vec<&String> = names
        .iter()
        .filter(|path| path.ends_with(".scss") && !path.contains("/template/"))
        .collect();
    assert_eq!(scss.len(), 14, "{scss:?}");
    for path in scss {
        let css = compile_scss(&theme, path).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert!(css.contains("--r-background-color"), "{path}");
        let inlined = inline_imports(&theme, path, &css, &mut identity).unwrap();
        assert!(!has_relative_font_reference(&inlined.css), "{path}");
        assert!(!inlined.css.contains("@import"), "{path}");
    }
}

#[test]
fn a_theme_entry_using_reveal_white_inlines_its_fonts() {
    let tmp = tempfile::tempdir().unwrap();
    let theme = scss_theme(
        tmp.path(),
        "theme.scss",
        &[("styles/theme.scss", "@use \"reveal/theme/white\";\n")],
    );
    let (css, registry) = compile(&theme).unwrap();
    let part = entry_part(&css, "@t/styles/theme.scss");
    assert!(squeeze(part).contains("--r-background-color:#fff;"));
    assert!(!part.contains("@import"), "{part}");
    assert!(!has_relative_font_reference(part));
    let fonts: Vec<_> = Regex::new(r"url\('asset:([0-9a-f]{12})'\)")
        .unwrap()
        .captures_iter(part)
        .map(|caps| caps[1].to_string())
        .collect();
    assert_eq!(fonts.len(), 4, "{part}");
    for id in fonts {
        let asset = registry.get(&id).unwrap();
        assert_eq!(asset.mime, "font/woff");
        assert!(asset.file_name_hint.starts_with("source-sans-pro-"));
        assert!(matches!(&asset.data, AssetData::Bytes(b) if b.starts_with(b"wOFF")));
    }
}

// ---------------------------------------------------------------------------------------
// The example theme
// ---------------------------------------------------------------------------------------

#[test]
fn the_example_theme_compiles_quickly() {
    let theme = example_theme();
    assert_eq!(theme.name(), "midnight");
    assert_eq!(theme.reveal_theme(), "none");
    let entries = theme.style_entries().unwrap();
    let entry = entries.last().unwrap();
    assert_eq!(entry.rel_path, "styles/theme.scss");

    // The SCSS compilation alone (the task budget is 300 ms in a release build; debug
    // builds are several times slower, hence the loose cap).
    let started = Instant::now();
    let css = compile_entry(entry, &theme).unwrap();
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");

    let flat = squeeze(&css);
    for declaration in [
        "--r-background-color:#0b1021;",
        "--r-main-color:#e6e6e6;",
        "--r-heading-font:Georgia,serif;",
        "--r-link-color:#ffb454;",
        "--p-accent:#ffb454;",
    ] {
        assert!(flat.contains(declaration), "{declaration}");
    }
    // `@use "palette"` found the partial next to the entry; nothing Sass-only is left.
    assert!(!css.contains('$'), "{css}");
    assert!(!css.contains("@use"));

    // The whole theme stylesheet: default's CSS first, then the SCSS with its fonts inlined.
    let (css, registry) = compile(&theme).unwrap();
    assert!(
        css.find("/* @default/styles/theme.css */").unwrap()
            < css.find("/* @midnight/styles/theme.scss */").unwrap()
    );
    let part = entry_part(&css, "@midnight/styles/theme.scss");
    assert!(!part.contains("@import"));
    assert_eq!(part.matches("@font-face").count(), 4);
    assert_eq!(registry.iter().filter(|a| a.mime == "font/woff").count(), 4);
}

#[test]
fn the_example_builds_standalone_without_network_references() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("deck.html");
    Command::cargo_bin("presenter")
        .unwrap()
        .current_dir(root())
        .args(["build", "examples/custom-theme-scss/slides.md", "-o"])
        .arg(&out)
        .assert()
        .success();
    let html = std::fs::read_to_string(&out).unwrap();
    assert!(
        html.contains("data:font/woff;base64,d09GR"),
        "reveal's woff fonts inlined"
    );
    assert!(!has_relative_font_reference(&html));
    assert!(!html.contains("url(fonts/"));
    assert!(!html.contains("url(./fonts/"));
    let theme_css = &html[html.find("<style id=\"presenter-theme\">").unwrap()..];
    let theme_css = &theme_css[..theme_css.find("</style>").unwrap()];
    assert!(!theme_css.contains("@import"));
    assert!(squeeze(theme_css).contains("--r-background-color:#0b1021;"));
}

#[test]
fn the_example_works_in_site_and_serve_mode() {
    let input = example_dir().join("slides.md");
    let site = build_deck(&BuildOptions {
        output: OutputMode::Site {
            assets_dir: PathBuf::from("out/assets"),
            url_prefix: "assets/".into(),
        },
        build: BuildInfo::build(false),
        ..BuildOptions::new(&input)
    })
    .unwrap();
    // The theme stylesheet file names the font files next to it.
    let fonts = Regex::new(r"url\('(source-sans-pro-[a-z]+-[0-9a-f]{12}\.woff)'\)").unwrap();
    let names: Vec<&str> = fonts
        .captures_iter(&site.document.theme_css)
        .map(|caps| caps.get(1).unwrap().as_str())
        .collect();
    assert_eq!(names.len(), 4, "{}", site.document.theme_css);
    for name in names {
        assert!(
            site.document
                .files
                .iter()
                .any(|f| f.path == Path::new("out/assets").join(name)),
            "{name}"
        );
    }

    // Serve: /__presenter/theme.css is the compiled stylesheet; its fonts are helper paths
    // that the asset route resolves.
    let serve = build_deck(&BuildOptions {
        output: OutputMode::Serve,
        build: BuildInfo::serve(1),
        missing_asset_is_error: false,
        ..BuildOptions::new(&input)
    })
    .unwrap();
    let css = &serve.document.theme_css;
    let prefix = "/__presenter/asset/";
    let font = "vendor/reveal/css/theme/fonts/source-sans-pro/source-sans-pro-regular.woff";
    assert!(css.contains(&format!("url('{prefix}{font}')")), "{css}");
    assert!(squeeze(css).contains("--r-background-color:#0b1021;"));
    let (bytes, _) = render::resolve_helper_path(&serve.theme, font)
        .unwrap()
        .unwrap();
    assert!(bytes.starts_with(b"wOFF"));
}

// ---------------------------------------------------------------------------------------
// The virtual file system
// ---------------------------------------------------------------------------------------

#[test]
fn sass_finds_partials_next_to_the_entry_then_in_the_chain() {
    let tmp = tempfile::tempdir().unwrap();
    disk_theme(
        &tmp.path().join("p"),
        "name = \"p\"\nextends = \"default\"\n",
        &[
            ("styles/_tokens.scss", "$accent: #123456;"),
            ("styles/colors/index.scss", "$ink: #222;"),
        ],
    );
    let child = disk_theme(
        &tmp.path().join("c"),
        "name = \"c\"\nextends = \"../p\"\n[styles]\nentries = [\"styles/main.scss\"]\n",
        &[
            (
                "styles/main.scss",
                "\u{feff}@use \"tokens\";\n@use \"colors\";\n@use \"local/box\";\n\
                 @use \"../../@p/styles/tokens\" as parent;\n\
                 a { color: tokens.$accent; border-color: colors.$ink; }\n\
                 b { color: parent.$accent; }",
            ),
            ("styles/local/_box.scss", ".box { padding: 1px; }"),
        ],
    );
    let (css, _) = compile(&child).unwrap();
    let part = entry_part(&css, "@c/styles/main.scss");
    let flat = squeeze(part);
    assert!(
        flat.contains("a{color:#123456;border-color:#222;}"),
        "{part}"
    );
    assert!(flat.contains(".box{padding:1px;}"));
    // `..` steps into another theme of the chain.
    assert!(flat.contains("b{color:#123456;}"), "{part}");
}

#[test]
fn sass_cannot_reach_files_outside_the_tree() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("_secret.scss"), "$secret: 1;").unwrap();
    let secret = tmp.path().join("_secret.scss");
    for url in [
        "../../secret".to_string(),
        secret.to_string_lossy().into_owned(),
        "/etc/hosts".to_string(),
    ] {
        let theme = scss_theme(
            &tmp.path().join("themes"),
            "x.scss",
            &[("styles/x.scss", &format!("@use \"{url}\";"))],
        );
        let (file, message) = style_error(compile(&theme).unwrap_err());
        assert_eq!(file, "@t/styles/x.scss");
        assert!(
            message.contains("Can't find stylesheet to import"),
            "{message}"
        );
    }

    let fs_theme = default_theme();
    let fs = VirtualFs::new(&fs_theme);
    assert!(grass::Fs::is_file(
        &fs,
        Path::new("reveal/theme/white.scss")
    ));
    assert!(grass::Fs::is_file(
        &fs,
        Path::new("@default/styles/theme.css")
    ));
    assert!(grass::Fs::is_dir(&fs, Path::new("reveal/theme/template")));
    assert!(grass::Fs::is_dir(&fs, Path::new("@default/styles")));
    assert!(!grass::Fs::is_file(
        &fs,
        Path::new("@nope/styles/theme.css")
    ));
    assert!(!grass::Fs::is_file(&fs, Path::new("/etc/hosts")));
    assert!(!grass::Fs::is_dir(&fs, Path::new("/etc")));
    // Reading never fails (grass would panic): a file outside the tree reads as empty and
    // the compilation reports it.
    assert!(!grass::Fs::is_file(&fs, Path::new("Cargo.toml")));
    assert!(
        grass::Fs::read(&fs, Path::new("Cargo.toml"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn indented_sass_entries_compile() {
    let tmp = tempfile::tempdir().unwrap();
    let theme = scss_theme(
        tmp.path(),
        "theme.sass",
        &[(
            "styles/theme.sass",
            "@use \"reveal/theme/template/settings\" with ($main-color: #333)\n.x\n  color: red\n",
        )],
    );
    let (css, _) = compile(&theme).unwrap();
    let flat = squeeze(entry_part(&css, "@t/styles/theme.sass"));
    assert!(flat.contains("--r-main-color:#333;"));
    assert!(flat.contains(".x{color:red;}"));
}

#[test]
fn sass_urls_with_an_extension_use_the_load_paths_too() {
    // grass 0.13.4 looks such URLs up next to the loading file only; dart-sass also
    // searches the load paths (the chain's styles/, then reveal/…, vendor/…, @<theme>/…).
    let tmp = tempfile::tempdir().unwrap();
    disk_theme(
        &tmp.path().join("p"),
        "name = \"p\"\nextends = \"default\"\n",
        &[
            ("styles/_tokens.scss", "$accent: #123456;"),
            ("styles/_shadowed.scss", "$who: parent;"),
            ("styles/_plain-next-to.scss", "$who: parent;"),
            ("styles/plain.css", ".plain { x: y; }"),
        ],
    );
    let child = disk_theme(
        &tmp.path().join("c"),
        "name = \"c\"\nextends = \"../p\"\n[styles]\nentries = [\"styles/main.scss\"]\n",
        &[
            (
                "styles/main.scss",
                "@use \"reveal/theme/template/settings.scss\" with ($main-color: #333);\n\
                 @use \"tokens.scss\";\n@use \"shadowed.scss\";\n@use \"plain.css\";\n\
                 @use \"parts/box.scss\";\n@use \"plain-next-to.scss\";\n\
                 a { color: tokens.$accent; content: \"#{shadowed.$who}\"; }\n\
                 b { content: \"#{plain-next-to.$who}\"; }",
            ),
            // Next to the entry wins over the parent theme's file of the same name, also
            // over a partial there when the file next to the entry is not one.
            ("styles/_shadowed.scss", "$who: child;"),
            ("styles/plain-next-to.scss", "$who: child;"),
            // From a partial in a subdirectory, the load paths are the same.
            (
                "styles/parts/_box.scss",
                "@import \"reveal/theme/template/mixins.scss\";\n.box { @include dark-bg-text-color(#fff); }",
            ),
        ],
    );
    let (css, _) = compile(&child).unwrap();
    let flat = squeeze(entry_part(&css, "@c/styles/main.scss"));
    assert!(flat.contains("--r-main-color:#333;"), "{flat}");
    assert!(
        flat.contains("a{color:#123456;content:\"child\";}"),
        "{flat}"
    );
    assert!(flat.contains("b{content:\"child\";}"), "{flat}");
    assert!(flat.contains(".plain{x:y;}"), "{flat}");
    assert!(
        flat.contains(".boxsection.has-dark-background,.boxsection.has-dark-backgroundh1"),
        "{flat}"
    );

    // A URL with an extension that is nowhere is still Sass's error.
    let theme = scss_theme(
        &tmp.path().join("missing"),
        "x.scss",
        &[(
            "styles/x.scss",
            "\n@use \"reveal/theme/template/nope.scss\";",
        )],
    );
    let (file, message) = style_error(compile(&theme).unwrap_err());
    assert_eq!(file, "@t/styles/x.scss");
    assert!(
        message.starts_with("line 2, column 1: Can't find stylesheet"),
        "{message}"
    );
}

#[test]
fn partials_that_cannot_be_loaded_are_style_errors() {
    // grass 0.13.4 panics on these (an I/O or UTF-8 error while loading a @use'd file).
    let tmp = tempfile::tempdir().unwrap();
    let theme = scss_theme(
        &tmp.path().join("latin1"),
        "t.scss",
        &[
            ("styles/t.scss", "@use \"fonts\";\na { color: red; }\n"),
            ("styles/_fonts.scss", ""),
        ],
    );
    std::fs::write(
        tmp.path().join("latin1/t/styles/_fonts.scss"),
        b".x { content: \"caf\xe9\"; }",
    )
    .unwrap();
    let (file, message) = style_error(compile(&theme).unwrap_err());
    assert_eq!(file, "@t/styles/_fonts.scss");
    assert!(message.starts_with("not valid UTF-8: "), "{message}");
    assert!(
        message.ends_with("(compiling '@t/styles/t.scss')"),
        "{message}"
    );

    // The same for a .scss file inlined by a CSS @import, and through the CLI.
    let theme = scss_theme(
        &tmp.path().join("via-css"),
        "t.css",
        &[
            ("styles/t.css", "@import \"t.scss\";"),
            ("styles/t.scss", "@use \"fonts\";"),
            ("styles/_fonts.scss", ""),
        ],
    );
    std::fs::write(tmp.path().join("via-css/t/styles/_fonts.scss"), b"\xff").unwrap();
    let (file, message) = style_error(compile(&theme).unwrap_err());
    assert_eq!(file, "@t/styles/_fonts.scss");
    assert!(
        message.ends_with("(compiling '@t/styles/t.scss')"),
        "{message}"
    );

    std::fs::write(tmp.path().join("s.md"), "---\ntheme: latin1/t\n---\n# A\n").unwrap();
    Command::cargo_bin("presenter")
        .unwrap()
        .current_dir(tmp.path())
        .args(["build", "s.md", "-o", "s.html"])
        .assert()
        .failure()
        .code(predicates::ord::ne(101))
        .stderr(predicates::str::contains("@t/styles/_fonts.scss"))
        .stderr(predicates::str::contains("panicked").not());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let theme = scss_theme(
            &tmp.path().join("locked"),
            "t.scss",
            &[
                ("styles/t.scss", "@use \"fonts\";\n"),
                ("styles/_fonts.scss", ".x { y: z; }"),
            ],
        );
        let locked = tmp.path().join("locked/t/styles/_fonts.scss");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Skipped where permissions do not apply (e.g. running as root).
        if std::fs::read(&locked).is_err() {
            let (file, message) = style_error(compile(&theme).unwrap_err());
            assert_eq!(file, "@t/styles/_fonts.scss");
            assert!(message.starts_with("cannot read "), "{message}");
            assert!(
                message.ends_with("(compiling '@t/styles/t.scss')"),
                "{message}"
            );
        }
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
}

#[test]
fn a_panic_inside_grass_is_a_style_error() {
    // `selector.simple-selectors('>')` reaches a `todo!()` in grass 0.13.4.
    let tmp = tempfile::tempdir().unwrap();
    let theme = scss_theme(
        tmp.path(),
        "t.scss",
        &[(
            "styles/t.scss",
            "@use \"sass:selector\";\na { b: selector.simple-selectors(\">\"); }\n",
        )],
    );
    let (file, message) = style_error(compile(&theme).unwrap_err());
    assert_eq!(file, "@t/styles/t.scss");
    assert!(
        message.starts_with("the Sass compiler (grass) failed unexpectedly: "),
        "{message}"
    );
}

#[test]
fn compile_scss_names_what_is_missing() {
    let theme = default_theme();
    let (file, message) = style_error(compile_scss(&theme, "reveal/theme/nope.scss").unwrap_err());
    assert_eq!(file, "vendor/reveal/css/theme/nope.scss");
    assert_eq!(message, "no such file");
    let (file, message) = style_error(compile_scss(&theme, "styles/x.scss").unwrap_err());
    assert_eq!(file, "styles/x.scss");
    assert!(
        message.starts_with("not a file of the stylesheet tree"),
        "{message}"
    );
}

// ---------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------

#[test]
fn sass_errors_name_the_file_and_the_line() {
    let tmp = tempfile::tempdir().unwrap();
    let theme = scss_theme(
        tmp.path(),
        "theme.scss",
        &[(
            "styles/theme.scss",
            ".a {\n  color: red;\n\n  .b { color: ; }\n}\n",
        )],
    );
    let err = compile(&theme).unwrap_err();
    let shown = err.to_string();
    let (file, message) = style_error(err);
    assert_eq!(file, "@t/styles/theme.scss");
    assert!(message.starts_with("line 4, column "), "{message}");
    assert!(
        message.contains("\n 4 |   .b { color: ; }\n   |"),
        "{message}"
    );
    assert!(shown.starts_with("style '@t/styles/theme.scss': line 4, column "));

    // An error in a partial names the partial and the entry being compiled.
    let theme = scss_theme(
        &tmp.path().join("2"),
        "theme.scss",
        &[
            ("styles/theme.scss", "@use \"parts/vars\";\n"),
            ("styles/parts/_vars.scss", "\n\n$x: 1px + 1s;\n"),
        ],
    );
    let (file, message) = style_error(compile(&theme).unwrap_err());
    assert_eq!(file, "@t/styles/parts/_vars.scss");
    assert!(message.starts_with("line 3, column "), "{message}");
    assert!(
        message.contains("(compiling '@t/styles/theme.scss')"),
        "{message}"
    );

    // A misconfigured reveal template: the error is in the entry's `@use … with`.
    let theme = scss_theme(
        &tmp.path().join("3"),
        "theme.scss",
        &[(
            "styles/theme.scss",
            "@use \"reveal/theme/template/settings\" with ($no-such-setting: 1);\n",
        )],
    );
    let (file, message) = style_error(compile(&theme).unwrap_err());
    assert_eq!(file, "@t/styles/theme.scss");
    assert!(message.starts_with("line 1, column "), "{message}");
    assert!(message.contains("!default"), "{message}");

    // Not UTF-8; another extension.
    let theme = scss_theme(&tmp.path().join("4"), "x.scss", &[("styles/x.scss", "")]);
    std::fs::write(tmp.path().join("4/t/styles/x.scss"), b"a { b: \xff }").unwrap();
    let (file, message) = style_error(compile(&theme).unwrap_err());
    assert_eq!(file, "@t/styles/x.scss");
    assert!(message.contains("UTF-8"), "{message}");
    let theme = scss_theme(&tmp.path().join("5"), "x.less", &[("styles/x.less", "")]);
    let (file, message) = style_error(compile(&theme).unwrap_err());
    assert_eq!(file, "@t/styles/x.less");
    assert!(message.contains(".css, .scss or .sass"), "{message}");
}

// ---------------------------------------------------------------------------------------
// Local @import inlining
// ---------------------------------------------------------------------------------------

#[test]
fn local_css_imports_are_inlined_with_their_own_urls() {
    let tmp = tempfile::tempdir().unwrap();
    let theme = scss_theme(
        tmp.path(),
        "theme.css",
        &[
            (
                "styles/theme.css",
                "/* @import url(commented.css); */\n\
                 @import url(\"parts/a.css?v=2\");\n\
                 @import 'parts/print.css' print;\n\
                 @import url(https://fonts.example/f.css);\n\
                 .main { background: url(bg.png); }\n",
            ),
            (
                "styles/parts/a.css",
                "@import \"b.scss\";\n.a { background: url(img/a.png); }\n",
            ),
            ("styles/parts/b.scss", "$c: #abc;\n.b { color: $c; }\n"),
            ("styles/parts/print.css", ".p { display: none; }"),
            ("styles/parts/img/a.png", "A"),
            ("styles/bg.png", "BG"),
        ],
    );
    let (css, registry) = compile(&theme).unwrap();
    let part = entry_part(&css, "@t/styles/theme.css");
    // Imported files are spliced in place, nested ones first; comments are left alone.
    assert!(part.contains("/* @import url(commented.css); */"));
    let order = [
        "/* inlined @t/styles/parts/a.css */",
        "/* inlined @t/styles/parts/b.scss */",
        ".b {\n  color: #abc;\n}",
        ".a { background: url(asset:",
        "@media print {\n/* inlined @t/styles/parts/print.css */\n.p { display: none; }\n}",
        ".main { background: url(asset:",
    ];
    let positions: Vec<usize> = order
        .iter()
        .map(|s| part.find(s).unwrap_or_else(|| panic!("{s} in {part}")))
        .collect();
    assert!(positions.is_sorted(), "{part}");
    // The remote import is left as written, but first in the stylesheet: CSS ignores an
    // @import after other rules (here, the default theme's).
    assert!(!part.contains("@import url(https"));
    assert!(css.starts_with(
        "/* hoisted from @t/styles/theme.css */\n@import url(https://fonts.example/f.css);\n\
         /* @default/styles/theme.css */\n"
    ));
    // `url(img/a.png)` resolved next to parts/a.css, `url(bg.png)` next to theme.css.
    let bytes: Vec<&[u8]> = registry
        .iter()
        .filter_map(|asset| match &asset.data {
            AssetData::Bytes(bytes) => Some(bytes.as_slice()),
            AssetData::File(_) => None,
        })
        .collect();
    assert!(bytes.contains(&b"A".as_slice()) && bytes.contains(&b"BG".as_slice()));
}

#[test]
fn imports_that_cannot_be_inlined_are_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let fail = |name: &str, files: &[(&str, &str)]| {
        let theme = scss_theme(&tmp.path().join(name), "theme.css", files);
        style_error(compile(&theme).unwrap_err())
    };

    // In a CSS file the message has the line of the import; reveal's sources are only
    // mentioned for `fonts/…` and `reveal/…` URLs.
    let (file, message) = fail(
        "missing",
        &[("styles/theme.css", "a {}\n@import url(nope.css);")],
    );
    assert_eq!(file, "@t/styles/theme.css");
    assert_eq!(
        message,
        "line 2: @import url(nope.css) not found next to the stylesheet"
    );
    let (_, message) = fail(
        "missing-font",
        &[("styles/theme.css", "@import 'fonts/x.css';")],
    );
    assert_eq!(
        message,
        "line 1: @import url(fonts/x.css) not found next to the stylesheet or among \
         reveal.js's theme sources"
    );
    // A CSS import that Sass passes through is relative to the file compiled, also when
    // it is written in a partial (Sass does not say where it came from).
    let (file, message) = fail(
        "missing-sass",
        &[
            ("styles/theme.css", "@import \"theme.scss\";"),
            ("styles/theme.scss", "@use \"parts/a\";"),
            ("styles/parts/_a.scss", "@import url(\"b.css\");"),
            ("styles/parts/b.css", ".b {}"),
        ],
    );
    assert_eq!(file, "@t/styles/theme.scss");
    assert!(
        message.starts_with("@import url(b.css) not found next to the stylesheet (Sass passes"),
        "{message}"
    );

    let (file, message) = fail(
        "cycle",
        &[
            ("styles/theme.css", "@import \"a.css\";"),
            ("styles/a.css", "@import \"b.css\";"),
            ("styles/b.css", "@import \"a.css\";"),
        ],
    );
    assert_eq!(file, "@t/styles/b.css");
    assert!(
        message.contains("@import cycle: @t/styles/a.css → @t/styles/b.css → @t/styles/a.css"),
        "{message}"
    );

    // theme.css → 1.css → … → <last>.css, each with a rule `.l<k> {}`.
    let chain = |last: usize| -> Vec<(String, String)> {
        let mut files = vec![("styles/theme.css".into(), "@import \"1.css\";".into())];
        for level in 1..=last {
            let import = match level < last {
                true => format!("@import \"{}.css\";\n", level + 1),
                false => String::new(),
            };
            files.push((
                format!("styles/{level}.css"),
                format!("{import}.l{level} {{}}"),
            ));
        }
        files
    };
    // Five levels below the entry are inlined …
    let five = chain(MAX_IMPORT_DEPTH);
    let theme = scss_theme(&tmp.path().join("five"), "theme.css", &as_refs(&five));
    let css = compile(&theme).unwrap().0;
    assert!(css.contains(".l1 {}") && css.contains(&format!(".l{MAX_IMPORT_DEPTH} {{}}")));
    // … a sixth is refused, naming the stylesheet that imports it.
    let six = chain(MAX_IMPORT_DEPTH + 1);
    let (file, message) = fail("six", &as_refs(&six));
    assert_eq!(file, format!("@t/styles/{MAX_IMPORT_DEPTH}.css"));
    assert!(
        message.contains("nested more than 5 levels deep"),
        "{message}"
    );

    for (name, condition) in [
        ("layer", "layer(base)"),
        ("supports", "supports(display: grid) screen"),
        ("bare-layer", "layer"),
    ] {
        let (_, message) = fail(
            name,
            &[
                (
                    "styles/theme.css",
                    &format!("@import url(a.css) {condition};"),
                ),
                ("styles/a.css", ""),
            ],
        );
        assert_eq!(
            message,
            format!(
                "line 1: @import url(a.css) {condition}: imports with layer() or supports() \
                 cannot be inlined"
            )
        );
    }
}

#[test]
fn inlined_vendor_css_keeps_its_unresolvable_fallbacks() {
    // KaTeX's CSS lists .woff/.ttf fallbacks that are not vendored (vendored-assets.md §2):
    // inlined, they stay as written, even with a same-named file in the theme's assets/.
    let tmp = tempfile::tempdir().unwrap();
    let theme = scss_theme(
        tmp.path(),
        "theme.css",
        &[
            (
                "styles/theme.css",
                "@import url(\"../../vendor/katex/katex.min.css\");\n.own { background: url(fonts/own.png); }\n",
            ),
            ("assets/fonts/KaTeX_AMS-Regular.woff", "WRONG"),
            ("assets/fonts/own.png", "OWN"),
        ],
    );
    let (css, registry) = compile(&theme).unwrap();
    let part = entry_part(&css, "@t/styles/theme.css");
    assert!(part.contains("/* inlined vendor/katex/katex.min.css */"));
    assert!(part.contains("url(fonts/KaTeX_AMS-Regular.woff)"), "{part}");
    assert!(part.contains("url(fonts/KaTeX_AMS-Regular.ttf)"), "{part}");
    assert!(
        !part.contains("url(fonts/KaTeX_AMS-Regular.woff2)"),
        "woff2 inlined"
    );
    // The entry's own url() still finds the theme's assets/.
    assert!(!part.contains("url(fonts/own.png)"));
    let bytes: Vec<&[u8]> = registry
        .iter()
        .filter_map(|asset| match &asset.data {
            AssetData::Bytes(bytes) => Some(bytes.as_slice()),
            AssetData::File(_) => None,
        })
        .collect();
    assert!(bytes.contains(&b"OWN".as_slice()));
    assert!(!bytes.contains(&b"WRONG".as_slice()));
}

#[test]
fn remote_imports_keep_the_media_queries_of_their_import_chain() {
    let tmp = tempfile::tempdir().unwrap();
    let theme = scss_theme(
        &tmp.path().join("ok"),
        "theme.css",
        &[
            (
                "styles/theme.css",
                "@import url(print.css) print;\n.a {}\n\
                 @media screen { @import url(https://x.example/nested.css); }\n",
            ),
            (
                "styles/print.css",
                "@import url(https://x.example/y.css);\n\
                 @import url(https://x.example/z.css) layer(fonts);\n.p {}\n",
            ),
        ],
    );
    let (css, _) = compile(&theme).unwrap();
    assert!(
        css.starts_with(
            "/* hoisted from @t/styles/print.css */\n@import url(https://x.example/y.css) print;\n\
             /* hoisted from @t/styles/print.css */\n\
             @import url(https://x.example/z.css) layer(fonts) print;\n\
             /* @default/styles/theme.css */\n"
        ),
        "{}",
        &css[..300]
    );
    let part = entry_part(&css, "@t/styles/theme.css");
    assert!(part.contains("@media print {\n/* inlined @t/styles/print.css */\n\n\n.p {}\n}"));
    // An @import inside a block is invalid CSS (browsers ignore it): left as written.
    assert!(part.contains("@media screen { @import url(https://x.example/nested.css); }"));

    // Two media query lists are not combined.
    let theme = scss_theme(
        &tmp.path().join("both"),
        "theme.css",
        &[
            ("styles/theme.css", "@import url(print.css) print;"),
            (
                "styles/print.css",
                "@import url(https://x.example/y.css) screen;",
            ),
        ],
    );
    let (file, message) = style_error(compile(&theme).unwrap_err());
    assert_eq!(file, "@t/styles/print.css");
    assert!(
        message.contains(
            "cannot be combined with those of the import that inlines this stylesheet (print)"
        ),
        "{message}"
    );
}

#[test]
fn reveal_fallback_only_maps_reveal_and_font_paths() {
    assert_eq!(
        reveal_fallback("./fonts/league-gothic/league-gothic.css").as_deref(),
        Some("vendor/reveal/css/theme/fonts/league-gothic/league-gothic.css")
    );
    assert_eq!(
        reveal_fallback("reveal/theme/fonts/league-gothic/league-gothic.woff").as_deref(),
        Some("vendor/reveal/css/theme/fonts/league-gothic/league-gothic.woff")
    );
    assert_eq!(reveal_fallback("img/fonts/x.css"), None);
}
