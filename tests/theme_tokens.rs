//! T17: the built-in theme's documentation (`assets/themes/default/README.md`) matches its
//! stylesheet and templates, and the stylesheet stays offline.
//!
//! - every `--p-*` custom property `styles/theme.css` declares is in the README's token
//!   table with the same default, and the README names no token the CSS does not know;
//! - every `--r-*` variable the CSS overrides is listed in the README, and vice versa;
//! - every layout template is listed in the README and in `theme.toml` `[layouts]`;
//! - no `@import`, `@font-face` or remote URL in the theme CSS (offline output);
//! - the title layout's logo: front matter `logo:`, theme `[vars] logo`, `logo: false`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use presenter::pipeline::{BuildOptions, build_deck};
use regex::Regex;

fn theme_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/themes/default")
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(theme_dir().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The theme stylesheet without its comments.
fn css() -> String {
    let comments = Regex::new(r"(?s)/\*.*?\*/").unwrap();
    comments
        .replace_all(&read("styles/theme.css"), "")
        .into_owned()
}

fn readme() -> String {
    read("README.md")
}

/// Collapses runs of whitespace, so multi-line values compare equal to one-line ones.
fn normalize(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Custom properties with `prefix` (`--p-`, `--r-`) the CSS declares, each with the value
/// of its first declaration.
fn declared(css: &str, prefix: &str) -> BTreeMap<String, String> {
    let declaration = Regex::new(&format!(
        r"({}[a-z0-9-]+)\s*:\s*([^;]+);",
        regex::escape(prefix)
    ))
    .unwrap();
    let mut out = BTreeMap::new();
    for caps in declaration.captures_iter(css) {
        out.entry(caps[1].to_string())
            .or_insert_with(|| normalize(&caps[2]));
    }
    out
}

/// Custom properties the CSS reads with `var(--name`.
fn used(css: &str) -> BTreeSet<String> {
    let usage = Regex::new(r"var\(\s*(--[a-z0-9-]+)").unwrap();
    usage
        .captures_iter(css)
        .map(|caps| caps[1].to_string())
        .collect()
}

/// The README's token table: `| `--p-name` | default | purpose |` → name → default cell
/// (backticks removed; `unset` for a token the CSS only reads).
fn documented_tokens(readme: &str) -> BTreeMap<String, String> {
    let row = Regex::new(r"(?m)^\| `(--p-[a-z0-9-]+)` \| (.+?) \| .+\|$").unwrap();
    let mut out = BTreeMap::new();
    for caps in row.captures_iter(readme) {
        let default = caps[2].trim().trim_matches('`').to_string();
        let previous = out.insert(caps[1].to_string(), default);
        assert!(previous.is_none(), "{} documented twice", &caps[1]);
    }
    out
}

#[test]
fn every_declared_token_is_documented_with_its_default() {
    let css = css();
    let declared = declared(&css, "--p-");
    let documented = documented_tokens(&readme());
    assert!(declared.len() >= 20, "{declared:?}");
    for (name, value) in &declared {
        let doc = documented
            .get(name)
            .unwrap_or_else(|| panic!("{name} is declared in theme.css but not documented"));
        assert_eq!(doc, value, "README default of {name}");
    }
    // A documented token the CSS does not declare is one it reads with a fallback.
    let used = used(&css);
    for (name, default) in &documented {
        if !declared.contains_key(name) {
            assert_eq!(default, "unset", "{name}");
            assert!(used.contains(name), "{name} is documented but never used");
        }
    }
}

#[test]
fn the_readme_names_only_tokens_the_css_knows() {
    let css = css();
    let known: BTreeSet<String> = declared(&css, "--p-")
        .into_keys()
        .chain(used(&css))
        .collect();
    let mention = Regex::new(r"--p-[a-z0-9-]*[a-z0-9]").unwrap();
    for m in mention.find_iter(&readme()) {
        assert!(
            known.contains(m.as_str()),
            "README mentions unknown {}",
            m.as_str()
        );
    }
    // Every token the CSS reads is declared or documented as optional.
    let documented = documented_tokens(&readme());
    for name in used(&css).iter().filter(|n| n.starts_with("--p-")) {
        assert!(
            documented.contains_key(name),
            "{name} is read but not documented"
        );
    }
}

#[test]
fn overridden_reveal_variables_are_listed() {
    let css = css();
    let overridden: BTreeSet<String> = declared(&css, "--r-").into_keys().collect();
    let readme = readme();
    let row = Regex::new(r"(?m)^\| ([^|]*`--r-[^|]*)\|").unwrap();
    let name = Regex::new(r"`(--r-[a-z0-9-]+)`").unwrap();
    let listed: BTreeSet<String> = row
        .captures_iter(&readme)
        .flat_map(|caps| {
            name.captures_iter(&caps[1])
                .map(|n| n[1].to_string())
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(listed, overridden);
    for required in [
        "--r-main-font",
        "--r-heading-font",
        "--r-main-font-size",
        "--r-link-color",
    ] {
        assert!(overridden.contains(required), "{required} not overridden");
    }
}

#[test]
fn every_layout_is_documented_and_declared() {
    let files: BTreeSet<String> = std::fs::read_dir(theme_dir().join("templates/layouts"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "html"))
        .map(|path| path.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        files,
        BTreeSet::from(["blank", "default", "section", "title"].map(String::from))
    );
    let manifest: toml::Table = read("theme.toml").parse().unwrap();
    let declared: BTreeSet<String> = manifest["layouts"]
        .as_table()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(declared, files, "theme.toml [layouts]");
    let readme = readme();
    for name in &files {
        let row = format!("| `{name}` | `templates/layouts/{name}.html` |");
        assert!(readme.contains(&row), "README layout row for {name}");
    }
}

#[test]
fn stylesheet_loads_nothing_from_the_network() {
    let source = read("styles/theme.css");
    assert!(!source.contains("@import"), "no @import (webfonts)");
    assert!(!source.contains("@font-face"), "system fonts only");
    let remote = Regex::new(r#"(?i)url\(\s*['"]?\s*(https?:|//)"#).unwrap();
    assert!(!remote.is_match(&source), "no remote url()");
    assert!(!Regex::new(r"(?i)https?://").unwrap().is_match(&source));
}

// ---------------------------------------------------------------------------------------
// The title layout's logo
// ---------------------------------------------------------------------------------------

/// A child theme of `default` with `[vars] logo = "logo.svg"`, its `assets/logo.svg` and a
/// second image at the theme root, in `dir/brand`.
fn brand_theme(dir: &Path) -> PathBuf {
    let theme = dir.join("brand");
    std::fs::create_dir_all(theme.join("assets")).unwrap();
    std::fs::write(
        theme.join("theme.toml"),
        "name = \"brand\"\nextends = \"default\"\n[vars]\nlogo = \"logo.svg\"\n",
    )
    .unwrap();
    std::fs::write(theme.join("assets/logo.svg"), "<svg id=\"theme-logo\"/>").unwrap();
    std::fs::write(theme.join("other.svg"), "<svg id=\"other-logo\"/>").unwrap();
    theme
}

/// Builds a title-slide deck with `front_matter` lines and `theme`; returns the HTML of the
/// first slide's section.
fn title_slide(dir: &Path, front_matter: &str, theme: Option<&Path>) -> presenter::Result<String> {
    let deck = dir.join("slides.md");
    std::fs::write(
        &deck,
        format!("---\ntitle: Talk\n{front_matter}---\n\n# Talk {{layout=title}}\n"),
    )
    .unwrap();
    let mut opts = BuildOptions::new(&deck);
    opts.cli.theme = theme.map(|t| t.to_string_lossy().into_owned());
    let html = build_deck(&opts)?.document.html;
    let start = html.find("<section").expect("a slide");
    let end = html[start..].find("</section>").expect("closed") + start;
    Ok(html[start..end].to_string())
}

/// The decoded `src` of the title slide's logo, if any.
fn logo(section: &str) -> Option<String> {
    let img = Regex::new(r#"<img class="logo" src="data:image/svg\+xml;base64,([^"]+)" alt="">"#)
        .unwrap();
    let caps = img.captures(section)?;
    let bytes = STANDARD.decode(&caps[1]).expect("base64");
    Some(String::from_utf8(bytes).unwrap())
}

#[test]
fn title_logo_comes_from_the_theme_or_the_front_matter() {
    let tmp = tempfile::tempdir().unwrap();
    let theme = brand_theme(tmp.path());

    let section = title_slide(tmp.path(), "", Some(&theme)).unwrap();
    assert!(
        section.contains("class=\"layout-title center\""),
        "{section}"
    );
    assert_eq!(logo(&section).as_deref(), Some("<svg id=\"theme-logo\"/>"));
    // The logo comes before the heading.
    assert!(section.find("class=\"logo\"").unwrap() < section.find("<h1>").unwrap());

    // The front matter wins, in any form asset() resolves.
    let section = title_slide(tmp.path(), "logo: \"@brand/other.svg\"\n", Some(&theme)).unwrap();
    assert_eq!(logo(&section).as_deref(), Some("<svg id=\"other-logo\"/>"));

    // `logo: false` hides the theme's logo; the default theme has none.
    let section = title_slide(tmp.path(), "logo: false\n", Some(&theme)).unwrap();
    assert!(!section.contains("class=\"logo\""), "{section}");
    let section = title_slide(tmp.path(), "", None).unwrap();
    assert!(!section.contains("<img"), "{section}");
}

#[test]
fn a_missing_logo_is_a_build_error_naming_it() {
    let tmp = tempfile::tempdir().unwrap();
    let error = title_slide(tmp.path(), "logo: nope.svg\n", None).unwrap_err();
    assert!(error.to_string().contains("nope.svg"), "{error}");
}
