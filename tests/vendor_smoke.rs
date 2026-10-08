//! Smoke tests for the embedded assets (docs/spec/vendored-assets.md §2–§4).

use std::path::Path;

use presenter::vendor;

/// The 14 stock reveal.js themes (`dist/theme/<name>.css` and `css/theme/<name>.scss`).
const REVEAL_THEMES: [&str; 14] = [
    "beige",
    "black",
    "black-contrast",
    "blood",
    "dracula",
    "league",
    "moon",
    "night",
    "serif",
    "simple",
    "sky",
    "solarized",
    "white",
    "white-contrast",
];

/// The 20 KaTeX font families shipped as woff2.
const KATEX_FONTS: [&str; 20] = [
    "AMS-Regular",
    "Caligraphic-Bold",
    "Caligraphic-Regular",
    "Fraktur-Bold",
    "Fraktur-Regular",
    "Main-Bold",
    "Main-BoldItalic",
    "Main-Italic",
    "Main-Regular",
    "Math-BoldItalic",
    "Math-Italic",
    "SansSerif-Bold",
    "SansSerif-Italic",
    "SansSerif-Regular",
    "Script-Regular",
    "Size1-Regular",
    "Size2-Regular",
    "Size3-Regular",
    "Size4-Regular",
    "Typewriter-Regular",
];

/// Every file under `assets/vendor/`, per vendored-assets.md §2 (plus `VERSIONS.toml`).
fn expected_vendor_files() -> Vec<String> {
    let mut files: Vec<String> = [
        "vendor/VERSIONS.toml",
        "vendor/katex/LICENSE",
        "vendor/katex/katex.min.css",
        "vendor/katex/katex.min.js",
        "vendor/reveal/LICENSE",
        "vendor/reveal/dist/reveal.js",
        "vendor/reveal/dist/reveal.css",
        "vendor/reveal/dist/reset.css",
        "vendor/reveal/dist/plugin/highlight.js",
        "vendor/reveal/dist/plugin/highlight/monokai.css",
        "vendor/reveal/dist/plugin/highlight/zenburn.css",
        "vendor/reveal/dist/plugin/notes.js",
        "vendor/reveal/dist/plugin/zoom.js",
        "vendor/reveal/dist/plugin/search.js",
        "vendor/reveal/css/theme/template/settings.scss",
        "vendor/reveal/css/theme/template/theme.scss",
        "vendor/reveal/css/theme/template/mixins.scss",
        // reveal 6.0.2 ships the theme fonts as .woff only (no .woff2 exists upstream).
        "vendor/reveal/css/theme/fonts/league-gothic/LICENSE",
        "vendor/reveal/css/theme/fonts/league-gothic/league-gothic.css",
        "vendor/reveal/css/theme/fonts/league-gothic/league-gothic.woff",
        "vendor/reveal/css/theme/fonts/source-sans-pro/LICENSE",
        "vendor/reveal/css/theme/fonts/source-sans-pro/source-sans-pro.css",
        "vendor/reveal/css/theme/fonts/source-sans-pro/source-sans-pro-italic.woff",
        "vendor/reveal/css/theme/fonts/source-sans-pro/source-sans-pro-regular.woff",
        "vendor/reveal/css/theme/fonts/source-sans-pro/source-sans-pro-semibold.woff",
        "vendor/reveal/css/theme/fonts/source-sans-pro/source-sans-pro-semibolditalic.woff",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for theme in REVEAL_THEMES {
        files.push(format!("vendor/reveal/dist/theme/{theme}.css"));
        files.push(format!("vendor/reveal/css/theme/{theme}.scss"));
    }
    for font in KATEX_FONTS {
        files.push(format!("vendor/katex/fonts/KaTeX_{font}.woff2"));
    }
    files.sort();
    files
}

#[test]
fn reveal_js_is_embedded() {
    let reveal = vendor::get("vendor/reveal/dist/reveal.js").expect("reveal.js is vendored");
    assert!(
        reveal.len() > 100_000,
        "reveal.js is {} bytes",
        reveal.len()
    );
}

#[test]
fn katex_is_embedded() {
    assert!(vendor::get("vendor/katex/katex.min.js").is_some());
    assert!(vendor::get("vendor/katex/katex.min.css").is_some());
}

#[test]
fn fourteen_reveal_themes() {
    let themes = vendor::list("vendor/reveal/dist/theme/");
    assert_eq!(themes.len(), 14, "{themes:?}");
    assert!(themes.iter().all(|p| p.ends_with(".css")), "{themes:?}");
}

#[test]
fn excluded_reveal_files_are_absent() {
    for path in [
        "vendor/reveal/dist/plugin/markdown.js",
        "vendor/reveal/dist/plugin/math.js",
        "vendor/reveal/dist/reveal.mjs",
        "vendor/reveal/dist/reveal.d.ts",
        "vendor/reveal/index.html",
        "vendor/reveal/demo.html",
    ] {
        assert!(vendor::get(path).is_none(), "{path} must not be vendored");
    }
}

#[test]
fn vendored_file_list_matches_spec() {
    assert_eq!(vendor::list("vendor/"), expected_vendor_files());
}

#[test]
fn list_is_sorted_and_prefix_filtered() {
    let all = vendor::list("");
    let mut sorted = all.clone();
    sorted.sort();
    assert_eq!(all, sorted);
    assert!(
        vendor::list("vendor/katex/")
            .iter()
            .all(|p| p.starts_with("vendor/katex/"))
    );
    assert!(vendor::list("no-such-prefix/").is_empty());
}

/// Debug builds resolve paths on disk, release builds by exact key; `get` must agree in both
/// (CI runs this file under `cargo test` and `cargo test --release`).
#[test]
fn non_normalized_paths_resolve_like_clean_ones() {
    for (messy, clean) in [
        (
            "vendor/reveal/css/theme/./fonts/league-gothic/league-gothic.css",
            "vendor/reveal/css/theme/fonts/league-gothic/league-gothic.css",
        ),
        (
            "vendor/reveal/css/theme/fonts/league-gothic/./league-gothic.woff",
            "vendor/reveal/css/theme/fonts/league-gothic/league-gothic.woff",
        ),
        (
            "vendor/katex/./fonts/KaTeX_Main-Regular.woff2",
            "vendor/katex/fonts/KaTeX_Main-Regular.woff2",
        ),
        (
            "vendor/katex/fonts/../katex.min.css",
            "vendor/katex/katex.min.css",
        ),
        ("vendor//VERSIONS.toml", "vendor/VERSIONS.toml"),
        (
            "vendor//reveal/dist/reveal.js",
            "vendor/reveal/dist/reveal.js",
        ),
        (
            "vendor/../vendor/reveal/dist/reveal.js",
            "vendor/reveal/dist/reveal.js",
        ),
        ("presenter/../vendor/VERSIONS.toml", "vendor/VERSIONS.toml"),
        ("./presenter/math.js", "presenter/math.js"),
        ("/presenter/math.js", "presenter/math.js"),
        (
            "vendor/reveal/dist/reveal.js/",
            "vendor/reveal/dist/reveal.js",
        ),
        (
            "vendor\\reveal\\dist\\reveal.js",
            "vendor/reveal/dist/reveal.js",
        ),
    ] {
        let expected = vendor::get(clean).unwrap_or_else(|| panic!("{clean} is vendored"));
        let actual = vendor::get(messy);
        // Compare without printing the (possibly binary) contents on failure.
        assert!(
            actual.as_deref() == Some(&*expected),
            "{messy:?}: got {:?} bytes, want the {} bytes of {clean}",
            actual.map(|data| data.len()),
            expected.len()
        );
    }
    assert_eq!(
        vendor::get_str("vendor/./VERSIONS.toml").unwrap(),
        vendor::get_str("vendor/VERSIONS.toml").unwrap()
    );
}

#[test]
fn wrong_case_directories_and_escaping_paths_are_none() {
    for path in [
        // On macOS the debug-mode disk lookup is case-insensitive; release lookup is not.
        "vendor/reveal/dist/Reveal.js",
        "vendor/reveal/dist/REVEAL.JS",
        "Vendor/reveal/dist/reveal.js",
        // Directories are not files.
        "vendor/reveal",
        "vendor/reveal/dist/",
        // Nothing outside assets/.
        "../assets/vendor/VERSIONS.toml",
        "vendor/../../Cargo.toml",
        "../Cargo.toml",
        "",
        "/",
    ] {
        assert!(vendor::get(path).is_none(), "{path:?} must be None");
    }
}

#[test]
fn every_listed_path_is_gettable() {
    let all = vendor::list("");
    // vendor/ plus the two presenter scripts, at least.
    assert!(all.len() >= expected_vendor_files().len() + 2, "{all:?}");
    for path in &all {
        assert!(
            vendor::get(path).is_some(),
            "{path} is listed but not gettable"
        );
    }
}

/// CI plants junk files (`.DS_Store`, `._*`, `Thumbs.db`, `*.swp`, `*~`) under `assets/`
/// before testing, so there this checks the rust-embed excludes in both build modes.
#[test]
fn os_junk_is_never_listed_or_served() {
    let is_junk = |path: &str| {
        let name = path.rsplit('/').next().unwrap_or(path);
        name == ".DS_Store"
            || name.starts_with("._")
            || name == "Thumbs.db"
            || name.ends_with(".swp")
            || name.ends_with('~')
    };
    let junk: Vec<String> = vendor::list("")
        .into_iter()
        .filter(|p| is_junk(p))
        .collect();
    assert!(junk.is_empty(), "OS/editor junk embedded: {junk:?}");
    for path in [
        "vendor/.DS_Store",
        "vendor/reveal/dist/theme/.DS_Store",
        "vendor/katex/._katex.min.js",
        "themes/default/.DS_Store",
        "themes/default/Thumbs.db",
        "presenter/.math.js.swp",
        "presenter/math.js~",
    ] {
        assert!(vendor::get(path).is_none(), "{path} must be None");
    }
}

#[test]
fn presenter_scripts_are_embedded() {
    assert!(vendor::get("presenter/math.js").is_some());
    assert!(vendor::get("presenter/live-reload.js").is_some());
}

#[test]
fn get_str_reads_utf8_and_reports_missing() {
    let versions = vendor::get_str("vendor/VERSIONS.toml").unwrap();
    assert!(versions.contains("version = \"6.0.2\""), "{versions}");
    assert!(versions.contains("version = \"0.19.0\""), "{versions}");
    let license = vendor::get_str("vendor/reveal/LICENSE").unwrap();
    assert!(license.contains("MIT") || license.contains("Permission is hereby granted"));
    let err = vendor::get_str("vendor/does-not-exist.js").unwrap_err();
    assert!(
        err.to_string().contains("vendor/does-not-exist.js"),
        "{err}"
    );
    // A binary font is not valid UTF-8.
    assert!(vendor::get_str("vendor/katex/fonts/KaTeX_Main-Regular.woff2").is_err());
}

#[test]
fn versions_toml_records_pins() {
    let versions: toml::Table = toml::from_str(&vendor::get_str("vendor/VERSIONS.toml").unwrap())
        .expect("VERSIONS.toml is valid TOML");
    let packages = versions["package"].as_array().expect("[[package]] array");
    assert_eq!(packages.len(), 2);
    for package in packages {
        for key in ["name", "version", "license", "url", "sha256", "date"] {
            assert!(package.get(key).is_some(), "missing {key} in {package:?}");
        }
        let sha = package["sha256"].as_str().unwrap();
        assert_eq!(sha.len(), 64);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(package["license"].as_str(), Some("MIT"));
    }
}

#[test]
fn vendor_dir_is_below_8_mb() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/vendor");
    let total: u64 = walkdir::WalkDir::new(&dir)
        .into_iter()
        .map(|entry| entry.expect("readable entry"))
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.metadata().expect("metadata").len())
        .sum();
    assert!(total > 1_000_000, "suspiciously small: {total} bytes");
    assert!(total < 8_000_000, "assets/vendor is {total} bytes");
}
