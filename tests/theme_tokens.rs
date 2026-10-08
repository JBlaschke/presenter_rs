//! T17: the built-in theme's documentation (`assets/themes/default/README.md`) matches its
//! stylesheet and templates, and the stylesheet stays offline.
//!
//! - every `--p-*` custom property the stylesheets (`theme.toml` `[styles] entries`)
//!   declare is in the README's token table with the same default, and the README names no
//!   token the CSS does not know;
//! - every `--r-*` variable the CSS overrides is listed in the README, and vice versa;
//! - every layout template is listed in the README and in `theme.toml` `[layouts]`;
//! - no `@import`, `@font-face` or remote URL in the theme CSS (offline output);
//! - rules that keep reveal's layout helpers, highlight styles, inline images, tables and
//!   the overview working (the browser checks themselves are manual, see the T17 PR);
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

/// The stylesheets `theme.toml` `[styles] entries` lists, in order.
fn stylesheets() -> Vec<String> {
    let manifest: toml::Table = read("theme.toml").parse().unwrap();
    let entries: Vec<String> = manifest["styles"]["entries"]
        .as_array()
        .expect("[styles] entries")
        .iter()
        .map(|entry| entry.as_str().expect("a path").to_string())
        .collect();
    assert!(
        entries.contains(&"styles/theme.css".to_string()),
        "{entries:?}"
    );
    entries
}

/// Every theme stylesheet, concatenated, without comments.
fn css() -> String {
    let comments = Regex::new(r"(?s)/\*.*?\*/").unwrap();
    stylesheets()
        .iter()
        .map(|entry| comments.replace_all(&read(entry), "").into_owned())
        .collect::<Vec<_>>()
        .join("\n")
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
            .unwrap_or_else(|| panic!("{name} is declared in the theme CSS but not documented"));
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
    let remote = Regex::new(r#"(?i)url\(\s*['"]?\s*(https?:|//)"#).unwrap();
    let address = Regex::new(r"(?i)https?://").unwrap();
    for entry in stylesheets() {
        let source = read(&entry);
        assert!(
            !source.contains("@import"),
            "{entry}: no @import (webfonts)"
        );
        assert!(!source.contains("@font-face"), "{entry}: system fonts only");
        assert!(!remote.is_match(&source), "{entry}: no remote url()");
        assert!(!address.is_match(&source), "{entry}");
    }
}

// ---------------------------------------------------------------------------------------
// Rules that keep reveal and the highlight styles working
// ---------------------------------------------------------------------------------------

/// One style rule of the theme CSS, in source order.
#[derive(Debug)]
struct Rule {
    /// The enclosing at-rule (`@media print`), or empty.
    context: String,
    /// The selector list, whitespace collapsed.
    selector: String,
    /// `property` → value, whitespace collapsed.
    declarations: BTreeMap<String, String>,
}

impl Rule {
    fn get(&self, property: &str) -> Option<&str> {
        self.declarations.get(property).map(String::as_str)
    }
}

/// The style rules of `css` (comments already removed), at-rule bodies flattened.
fn rules(css: &str) -> Vec<Rule> {
    let mut out = Vec::new();
    collect_rules(css, "", &mut out);
    out
}

fn collect_rules(css: &str, context: &str, out: &mut Vec<Rule>) {
    let mut rest = css;
    while let Some(open) = rest.find('{') {
        let prelude = normalize(&rest[..open]);
        let mut depth = 1;
        let mut close = rest.len();
        for (i, c) in rest[open + 1..].char_indices() {
            depth += match c {
                '{' => 1,
                '}' => -1,
                _ => 0,
            };
            if depth == 0 {
                close = open + 1 + i;
                break;
            }
        }
        let body = &rest[open + 1..close];
        if prelude.starts_with('@') {
            collect_rules(body, &prelude, out);
        } else {
            let declarations = body
                .split(';')
                .filter_map(|d| d.split_once(':'))
                .map(|(p, v)| (p.trim().to_string(), normalize(v)))
                .collect();
            out.push(Rule {
                context: context.to_string(),
                selector: prelude,
                declarations,
            });
        }
        rest = rest.get(close + 1..).unwrap_or("");
    }
}

/// The rule with exactly `selector`.
fn rule<'a>(rules: &'a [Rule], selector: &str) -> &'a Rule {
    rules
        .iter()
        .find(|r| r.selector == selector)
        .unwrap_or_else(|| panic!("no rule `{selector}`"))
}

#[test]
fn the_rule_parser_sees_every_rule() {
    let rules = rules(&css());
    assert!(rules.len() > 80, "{}", rules.len());
    assert_eq!(rule(&rules, ":root").get("--p-radius"), Some("0.25em"));
    let print = rule(&rules, ".reveal, .reveal *");
    assert_eq!(print.context, "@media print");
    assert_eq!(print.get("print-color-adjust"), Some("exact"));
}

/// reveal sizes a direct `.r-stretch` child to the slide's full width (1280px) and fits
/// `.r-fit-text` to its parent's width including the padding: both must stay inside the
/// padded slide.
#[test]
fn reveal_layout_helpers_stay_inside_the_slide_padding() {
    let rules = rules(&css());
    let stretch = rule(
        &rules,
        ".reveal .slides section > :is(.r-stretch, .stretch)",
    );
    assert_eq!(stretch.get("max-width"), Some("100%"));
    assert_eq!(stretch.get("box-sizing"), Some("border-box"));
    let media = rule(
        &rules,
        ".reveal .slides section > :is(img, video):is(.r-stretch, .stretch)",
    );
    assert_eq!(
        media.get("object-fit"),
        Some("contain"),
        "aspect ratio kept"
    );

    for (selector, important) in [
        (".reveal .slides section:not(.stack):has(> .r-fit-text)", ""),
        (
            "html.reveal-print .reveal .slides section:not(.stack):has(> .r-fit-text)",
            " !important",
        ),
    ] {
        let fit = rule(&rules, selector);
        for side in ["padding-left", "padding-right"] {
            assert_eq!(
                fit.get(side),
                Some(format!("0{important}").as_str()),
                "{selector}"
            );
        }
    }
    // The print rule must come after the one that restores the padding in reveal's print view.
    let position = |s: &str| rules.iter().position(|r| r.selector == s).unwrap();
    assert!(
        position("html.reveal-print .reveal .slides section:not(.stack):has(> .r-fit-text)")
            > position("html.reveal-print .reveal .slides section:not(.stack)")
    );
}

/// reveal stretches only direct children of a slide, but lifts the size caps of every
/// `.r-stretch` element; a Markdown image is wrapped in a paragraph, so it must keep them.
#[test]
fn only_images_reveal_stretches_lose_the_size_caps() {
    let rules = rules(&css());
    let caps: Vec<&Rule> = rules
        .iter()
        .filter(|r| r.get("max-height") == Some("var(--p-image-max-height)"))
        .collect();
    assert_eq!(caps.len(), 1, "{caps:?}");
    assert_eq!(
        caps[0].selector,
        ".reveal :where(.slides) :is(img, video):not(section > .r-stretch, section > .stretch)"
    );
    assert_eq!(
        caps[0].get("max-width"),
        Some("100%"),
        "beats reveal's max-width: none"
    );
    for r in &rules {
        assert!(
            !r.selector.contains(":not(.r-stretch") && !r.selector.contains(":not(.stretch"),
            "exempts every .r-stretch element: {}",
            r.selector
        );
    }
}

/// The selectors of a selector list (split at the commas outside parentheses).
fn complex_selectors(list: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0usize, 0);
    for (i, c) in list.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(list[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(list[start..].trim());
    parts
}

#[test]
fn complex_selectors_split_only_at_top_level_commas() {
    assert_eq!(
        complex_selectors(".a :is(img, video), .b:not(x, y) > p"),
        [".a :is(img, video)", ".b:not(x, y) > p"]
    );
}

/// reveal puts images, videos and iframes inside `.reveal` but outside `.slides` and sizes
/// them itself: slide backgrounds (`background-video` must cover the slide, a
/// `background-iframe` has no frame) and the lightbox of `data-preview-image`/`-video`.
/// Every theme rule that selects one of these elements must therefore be scoped to the
/// slides (`.slides` before the element in the selector), without gaining specificity over
/// the unscoped rules it replaced (`:where(.slides)`), so child themes still win as before.
#[test]
fn media_rules_style_slide_content_only() {
    let media = Regex::new(r"(^|[\s>+~(,])(img|video|iframe)\b").unwrap();
    let rules = rules(&css());
    let mut checked = 0;
    for r in &rules {
        for selector in complex_selectors(&r.selector) {
            let Some(m) = media.find(selector) else {
                continue;
            };
            checked += 1;
            assert!(
                selector[..m.start()].contains(".slides"),
                "`{selector}` ({}) also matches outside the slides",
                r.context
            );
        }
    }
    assert!(checked >= 6, "{checked}");
    for selector in [
        ".reveal :where(.slides) img, .reveal :where(.slides) video, .reveal :where(.slides) iframe",
        ".reveal :where(.slides) img",
        ".reveal :where(.slides) iframe",
    ] {
        rule(&rules, selector);
    }
    let frame = rule(&rules, ".reveal :where(.slides) iframe");
    assert_eq!(frame.get("border"), Some("1px solid var(--p-border)"));
}

/// CSS cannot see text nodes, so `p:has(> img:only-child)` also matches a sentence with an
/// inline icon: only the blank layout may use it, and only when the paragraph is all the
/// slide holds.
#[test]
fn a_paragraph_with_an_inline_image_keeps_its_alignment() {
    let rules = rules(&css());
    let lone = ":first-child:is(:last-child, :has(+ aside.notes:last-child))";
    let mut checked = 0;
    for r in rules.iter().filter(|r| r.selector.contains(":only-child")) {
        assert!(r.get("text-align").is_none(), "{}", r.selector);
        for selector in r.selector.split(", .reveal") {
            if !selector.contains(":only-child") {
                continue;
            }
            checked += 1;
            assert!(
                selector.contains(&format!("section.layout-blank > p{lone}")),
                "{selector}"
            );
            assert!(
                selector.contains(":is(img, video)"),
                "video too: {selector}"
            );
        }
    }
    assert_eq!(checked, 2, "the blank layout's paragraph and its image");
    let fill = rules
        .iter()
        .find(|r| {
            r.selector.starts_with(&format!(
                ".reveal .slides section.layout-blank > :is(img, video, iframe){lone}"
            ))
        })
        .expect("blank-layout fill rule");
    assert_eq!(fill.get("height"), Some("100%"));
    assert_eq!(fill.get("max-height"), Some("none"));
}

/// The highlight style's `.hljs` colours must win over the code tokens, so switching
/// `highlight_style` switches the code background (theme-format.md §2).
#[test]
fn the_highlight_style_colours_highlighted_code() {
    let rules = rules(&css());
    for token in ["var(--p-code-bg)", "var(--p-code-color)"] {
        let readers: Vec<&Rule> = rules
            .iter()
            .filter(|r| r.declarations.values().any(|v| v.contains(token)))
            .collect();
        assert!(!readers.is_empty(), "{token}");
        for r in readers {
            assert!(
                r.selector.starts_with(":where("),
                "{token} in {}",
                r.selector
            );
        }
    }
    let colours = rule(&rules, ":where(.reveal pre) code");
    assert_eq!(colours.get("background"), Some("var(--p-code-bg)"));
    // Line numbers follow whatever text colour the highlight style gives the block.
    let numbers = rule(&rules, ".reveal .hljs-ln-numbers");
    assert!(numbers.get("color").unwrap().contains("currentColor"));
}

/// Dimmed lines of a line step stay at least as legible as in reveal (opacity 0.4).
#[test]
fn dimmed_code_lines_stay_legible() {
    let rules = rules(&css());
    let dimmed = rule(
        &rules,
        ".reveal .hljs.has-highlights tr:not(.highlight-line)",
    );
    assert_eq!(dimmed.get("opacity"), Some("var(--p-code-dim-opacity)"));
    let value: f64 = rule(&rules, ":root")
        .get("--p-code-dim-opacity")
        .unwrap()
        .parse()
        .unwrap();
    assert!((0.4..1.0).contains(&value), "{value}");
}

/// With `reveal_theme = "none"` nothing else makes code blocks blocks or left-aligned.
#[test]
fn code_blocks_do_not_need_a_stock_reveal_theme() {
    let rules = rules(&css());
    assert_eq!(rule(&rules, ".reveal pre").get("text-align"), Some("left"));
    let code = rule(&rules, ".reveal pre code");
    assert_eq!(code.get("display"), Some("block"));
    assert_eq!(code.get("overflow"), Some("auto"));
    assert_eq!(code.get("white-space"), Some("pre"));
}

/// A table wider than the slide scrolls inside its own box, like code and display math.
#[test]
fn wide_tables_scroll_sideways() {
    let rules = rules(&css());
    let table = rule(&rules, ".reveal table:not(.hljs-ln)");
    assert_eq!(table.get("display"), Some("block"));
    assert_eq!(table.get("max-width"), Some("100%"));
    assert_eq!(table.get("width"), Some("max-content"));
    assert_eq!(table.get("overflow-x"), Some("auto"));
}

/// The `height: auto` of centred slides must not shrink their overview thumbnails: the
/// overview rule has the same specificity and has to come after every one of them.
#[test]
fn overview_thumbnails_are_one_slide_high() {
    let rules = rules(&css());
    let selector = ".reveal.overview .slides section:not(.stack)";
    let overview = rules.iter().position(|r| r.selector == selector).unwrap();
    assert_eq!(
        rules[overview].get("height"),
        Some("var(--slide-height, 100%)")
    );
    let auto: Vec<usize> = rules
        .iter()
        .enumerate()
        .filter(|(_, r)| r.get("height") == Some("auto") && r.selector.contains("section"))
        .map(|(i, _)| i)
        .collect();
    assert!(auto.len() >= 3, "{auto:?}");
    assert!(auto.iter().all(|&i| i < overview), "{auto:?} vs {overview}");
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

    // `logo: true` keeps the theme's logo (and is not looked up as the path "true").
    let section = title_slide(tmp.path(), "logo: true\n", Some(&theme)).unwrap();
    assert_eq!(logo(&section).as_deref(), Some("<svg id=\"theme-logo\"/>"));
    let section = title_slide(tmp.path(), "logo: true\n", None).unwrap();
    assert!(!section.contains("<img"), "{section}");
}

#[test]
fn a_logo_that_is_not_a_string_is_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let theme = brand_theme(tmp.path());
    for value in ["42", "[a.svg]", "{a: b}", "\"\"", "null"] {
        let section = title_slide(tmp.path(), &format!("logo: {value}\n"), Some(&theme))
            .unwrap_or_else(|e| panic!("logo: {value}: {e}"));
        assert!(!section.contains("<img"), "logo: {value}: {section}");
    }
}

#[test]
fn a_missing_logo_is_a_build_error_naming_it() {
    let tmp = tempfile::tempdir().unwrap();
    let error = title_slide(tmp.path(), "logo: nope.svg\n", None).unwrap_err();
    assert!(error.to_string().contains("nope.svg"), "{error}");
}

/// Deck-relative logos are not supported yet (the coordinator's decision, README): a file
/// next to the deck is not found, and the error points at the logo partial and says where
/// the path was looked up.
#[test]
fn a_logo_next_to_the_deck_is_not_found_and_the_error_says_logo() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("img")).unwrap();
    std::fs::write(tmp.path().join("img/logo.svg"), "<svg/>").unwrap();
    let error = title_slide(tmp.path(), "logo: img/logo.svg\n", None)
        .unwrap_err()
        .to_string();
    for part in [
        "partials/logo.html",
        "img/logo.svg",
        "assets/ directories of the theme chain",
    ] {
        assert!(error.contains(part), "{part} in {error}");
    }
}
