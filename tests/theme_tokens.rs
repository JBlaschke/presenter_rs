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
//! - the title layout's logo: front matter `logo:`, theme `[vars] logo`, `logo: false`;
//! - (T33) `styles/notebook.css`: its tokens are in the README's "Notebook cells" section,
//!   and the rules behind the notebook browser checks (outputs that scroll past a cap in
//!   slide pixels, hidden parts, DataFrame headers that stick flush with the frame and give
//!   way on slides with their own background, tinted stderr and errors, all 16 ANSI colours,
//!   readable text on ANSI backgrounds and in reverse video).
//! - (audit fixes) print rules that show scrolling code and cell outputs in full and leave
//!   room for the lines Chrome moves past page breaks (F15), task-list checkboxes (F45), and
//!   shipped theme files that name no task ids or repository-only documents (F47).

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

/// Custom properties with `prefix` (`--p-`, `--r-`) the CSS declares, each with its default:
/// the value of its last declaration in a top-level `:root` rule (a later stylesheet, such as
/// notebook.css, overrides an earlier one, as in the browser), or else of its first
/// declaration (a token only set on elements, like `--p-alert-color`).
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
    let roots = rules(css);
    for root in roots
        .iter()
        .filter(|r| r.context.is_empty() && r.selector == ":root")
    {
        for (name, value) in &root.declarations {
            if name.starts_with(prefix) {
                out.insert(name.clone(), value.clone());
            }
        }
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
        ".reveal :where(.slides section) :is(img, video):not(section > .r-stretch, section > .stretch)"
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

/// A selector prefix that keeps a rule inside slide content: a slide's `section`
/// (`.slides section`, `section.layout-…`), or a class the theme's slide markup guarantees
/// (theme-format.md §6: `.cell…`, `.output…`, `.column(s)`), which only occurs inside one.
fn slide_content_scope() -> Regex {
    Regex::new(r"(^|[\s>+~(,])section\b|\.(cell|output|column)(s|-[a-z-]+)?\b").unwrap()
}

/// reveal puts images, videos and iframes inside `.reveal` and sizes them itself: slide
/// backgrounds (`background-video` must cover the slide, a `background-iframe` has no
/// frame) and the lightbox of `data-preview-image`/`-video`. In the normal view they are
/// outside `.slides`, but in print view (`?print-pdf`, what `presenter export pdf` loads)
/// and scroll view (`?view=scroll`, and narrow screens) reveal moves each background into
/// `.slides`, next to its slide's `section` (in `.pdf-page` / `.scroll-page-content`).
/// Every theme rule that selects one of these elements must therefore be scoped to slide
/// content ([`slide_content_scope`] before the element in the selector; `.slides` alone is
/// not enough), and the default theme's own media rules must not gain specificity over the
/// unscoped rules they replaced (`:where(.slides section)`), so child themes still win as
/// before.
#[test]
fn media_rules_style_slide_content_only() {
    let media = Regex::new(r"(^|[\s>+~(,])(img|video|iframe)\b").unwrap();
    let scope = slide_content_scope();
    let rules = rules(&css());
    let mut checked = 0;
    for r in &rules {
        for selector in complex_selectors(&r.selector) {
            let Some(m) = media.find(selector) else {
                continue;
            };
            checked += 1;
            assert!(
                scope.is_match(&selector[..m.start()]),
                "`{selector}` ({}) also matches outside slide content (reveal's \
                 backgrounds in print and scroll view)",
                r.context
            );
        }
    }
    assert!(checked >= 6, "{checked}");
    for selector in [
        ".reveal :where(.slides section) img, .reveal :where(.slides section) video, \
         .reveal :where(.slides section) iframe",
        ".reveal :where(.slides section) img",
        ".reveal :where(.slides section) iframe",
    ] {
        rule(&rules, selector);
    }
    let frame = rule(&rules, ".reveal :where(.slides section) iframe");
    assert_eq!(frame.get("border"), Some("1px solid var(--p-border)"));
}

/// The rule test above must reject the scopes that leak onto reveal's backgrounds or its
/// lightbox, `.slides` among them.
#[test]
fn scopes_outside_slide_content_are_rejected() {
    let scope = slide_content_scope();
    for leaky in [
        ".reveal ",
        ".reveal .slides ",
        ".reveal :where(.slides) ",
        ".reveal .slides > .pdf-page ",
        ".reveal .slide-background ",
        ".reveal .r-overlay-content ",
        ".reveal .sections ",
        ".reveal .cellar ",
    ] {
        assert!(!scope.is_match(leaky), "{leaky}");
    }
    for scoped in [
        ".reveal :where(.slides section) ",
        ".reveal .slides section > ",
        ".reveal .slides section.layout-blank > p:has(> :is(",
        ".reveal .cell-outputs ",
        ".reveal .output-display ",
        ".reveal .columns > .column ",
    ] {
        assert!(scope.is_match(scoped), "{scoped}");
    }
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
// Notebook cells (T33): styles/notebook.css and the README's "Notebook cells" section
// ---------------------------------------------------------------------------------------

/// styles/notebook.css without comments.
fn notebook_css() -> String {
    let comments = Regex::new(r"(?s)/\*.*?\*/").unwrap();
    comments
        .replace_all(&read("styles/notebook.css"), "")
        .into_owned()
}

/// The README from `## Notebook cells` up to the next `## ` heading.
fn notebook_section() -> String {
    let readme = readme();
    let start = readme
        .find("\n## Notebook cells\n")
        .expect("README section Notebook cells");
    let rest = &readme[start + 1..];
    let end = rest[3..].find("\n## ").map_or(rest.len(), |i| i + 3);
    rest[..end].to_string()
}

/// notebook.css comes after theme.css, so its rules win over theme.css's structural cell
/// rules, and the README's notebook section documents every token it declares (the rows of
/// the other tables are checked by `every_declared_token_is_documented_with_its_default`).
#[test]
fn notebook_tokens_are_documented_in_the_notebook_section() {
    let entries = stylesheets();
    let position = |entry: &str| {
        entries
            .iter()
            .position(|e| e == entry)
            .unwrap_or_else(|| panic!("{entry} not in theme.toml [styles] entries"))
    };
    assert!(
        position("styles/theme.css") < position("styles/notebook.css"),
        "{entries:?}"
    );
    let css = notebook_css();
    let declared = declared(&css, "--p-");
    let section = documented_tokens(&notebook_section());
    for token in ["--p-prompt-color", "--p-cell-font-size"] {
        assert!(declared.contains_key(token), "{token} not declared");
    }
    for (name, value) in &declared {
        assert_eq!(section.get(name), Some(value), "{name} in Notebook cells");
    }
    // The cell tokens theme.css declares are read here and documented in the token table.
    let used = used(&css);
    let documented = documented_tokens(&readme());
    for token in ["--p-cell-gap", "--p-cell-border", "--p-output-max-height"] {
        assert!(used.contains(token), "notebook.css does not read {token}");
        assert!(documented.contains_key(token), "{token}");
    }
}

/// reveal scales the whole slide to the window with a transform, so a `vh` cap grows with the
/// window: 40vh is 576 slide pixels in a 2560×1440 window, and the 200-line output of the
/// example overflowed the slide. The cap that applies (notebook.css replaces theme.css's
/// 40vh) is in slide pixels and well inside the 720px slide; images under an `Out [n]:` prompt
/// leave room for it.
#[test]
fn the_output_cap_is_in_slide_pixels() {
    let effective = declared(&css(), "--p-");
    let cap = &effective["--p-output-max-height"];
    assert_eq!(
        cap,
        &declared(&notebook_css(), "--p-")["--p-output-max-height"]
    );
    let px: f64 = cap
        .strip_suffix("px")
        .unwrap_or_else(|| panic!("--p-output-max-height: {cap} is not in slide pixels"))
        .parse()
        .unwrap();
    assert!((150.0..=400.0).contains(&px), "{px}px");

    let rules = rules(&notebook_css());
    let image = ".reveal .output-display img.output-image, .reveal .output-display figure.output-image > svg";
    assert_eq!(
        rule(&rules, image).get("max-height"),
        Some("calc(var(--p-output-max-height) - 1em)")
    );
    let under_prompt = rule(
        &rules,
        ".reveal .output-display > .cell-prompt + img.output-image, .reveal .output-display > .cell-prompt + figure.output-image > svg",
    );
    assert_eq!(
        under_prompt.get("max-height"),
        Some("calc(var(--p-output-max-height) - 1.7em)")
    );
}

/// ansi-to-html writes `var(--<name>,<fallback>)` for all 16 four-bit colours, text and
/// background; `.cell-outputs` sets every one, so none falls back to the converter's colours
/// for a black terminal (`#ff5` bright yellow is invisible on a white slide). These values
/// are for text on the slide; text on an ANSI background is the next test's.
#[test]
fn every_ansi_colour_is_themed() {
    let mut sgr = String::new();
    for code in (30..=37).chain(40..=47).chain(90..=97).chain(100..=107) {
        sgr.push_str(&format!("\u{1b}[{code}mx\u{1b}[0m "));
    }
    let html = presenter::notebook::outputs::ansi_to_html(&sgr);
    let reference = Regex::new(r"var\(--([a-z-]+),").unwrap();
    let names: BTreeSet<String> = reference
        .captures_iter(&html)
        .map(|caps| format!("--{}", &caps[1]))
        .collect();
    assert_eq!(names.len(), 16, "{names:?} from {html}");
    assert!(
        names.contains("--bright-yellow") && names.contains("--black"),
        "{names:?}"
    );

    let rules = rules(&notebook_css());
    let outputs = rule(&rules, ".reveal .cell-outputs");
    let section = notebook_section();
    for name in &names {
        let value = outputs
            .get(name)
            .unwrap_or_else(|| panic!("{name} is not set on .cell-outputs"));
        assert!(
            value.contains("currentColor") || value.starts_with("var(--"),
            "{name}: {value} does not follow the text colour"
        );
        assert!(
            section.contains(&format!("`{name}`")) || name.starts_with("--bright-"),
            "{name}"
        );
    }
    for name in ["--bright-black", "--bright-white"] {
        assert!(
            section.contains(&format!("`{name}`")),
            "{name} in the README"
        );
    }
}

/// `#rgb` or `#rrggbb` as 8-bit channels.
fn hex_rgb(hex: &str) -> [u8; 3] {
    let digits = hex
        .strip_prefix('#')
        .unwrap_or_else(|| panic!("{hex} is not a hex colour"));
    let wide: String = match digits.len() {
        3 => digits.chars().flat_map(|c| [c, c]).collect(),
        6 => digits.to_string(),
        _ => panic!("{hex} is not a hex colour"),
    };
    let channel = |i: usize| u8::from_str_radix(&wide[i..i + 2], 16).unwrap();
    [channel(0), channel(2), channel(4)]
}

/// The WCAG 2 contrast ratio of two colours (1 to 21).
fn contrast(a: [u8; 3], b: [u8; 3]) -> f64 {
    let luminance = |rgb: [u8; 3]| {
        let linear = |c: u8| {
            let c = f64::from(c) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(rgb[0]) + 0.7152 * linear(rgb[1]) + 0.0722 * linear(rgb[2])
    };
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The test above mixes the 16 colours with the text colour, which is right for text on the
/// slide but not on an ANSI background or in reverse video (SGR 7 became dark grey on darker
/// grey). Inside a background span, notebook.css switches to a fixed terminal palette for the
/// background and the text: an ANSI text colour comes from its own span or an enclosing one
/// (`--ansi-fg`, set by every colour span), other text and reverse video's default text are
/// black or white by background (`--ansi-default-fg`). On an 8-bit or RGB background the
/// converter writes that black or white itself. The selectors match the converter's spans in
/// its outputs (streams, errors, text/plain), and the palette keeps text readable (WCAG AA,
/// 4.5:1): default text on every background, black on every colour (and every colour on
/// black), white and `--white` on the colours white text reads on.
#[test]
fn ansi_backgrounds_and_reverse_video_keep_their_text_readable() {
    let html = |sgr: &str| presenter::notebook::outputs::ansi_to_html(&format!("\u{1b}[{sgr}mx"));
    // Reverse video: one span, the default background as black text.
    assert_eq!(
        html("7"),
        "<span style='color:var(--black,#000);background:var(--bright-white,#fff)'>x</span>"
    );
    assert_eq!(
        html("31;7"),
        "<span style='color:var(--red,#a00)'><span style='color:var(--black,#000);\
         background:var(--red,#a00)'>x</span></span>"
    );
    // A text colour encloses the background's span, also an 8-bit one (IPython 9 marks the
    // failing expression of a traceback this way).
    assert_eq!(
        html("97;41"),
        "<span style='color:var(--bright-white,#fff)'><span style='background:var(--red,#a00)'>\
         x</span></span>"
    );
    assert_eq!(
        html("38;5;241;43"),
        "<span style='color:#626262'><span style='background:var(--yellow,#a60)'>x</span></span>"
    );
    // 8-bit and RGB backgrounds are hex values; the converter colours the text without an
    // ANSI colour on them, also in reverse video of an 8-bit text colour.
    assert_eq!(
        html("38;5;100;7"),
        "<span style='color:#878700'><span style='color:#000;background:#878700'>x</span></span>"
    );
    assert_eq!(
        html("38;5;241;7"),
        "<span style='color:#626262'><span style='color:#fff;background:#626262'>x</span></span>"
    );
    assert_eq!(
        html("48;2;39;40;34"),
        "<span style='background:#272822;color:#fff'>x</span>"
    );
    assert_eq!(
        html("31;48;5;250"),
        "<span style='color:var(--red,#a00)'><span style='background:#bcbcbc'>x</span></span>"
    );
    let background = Regex::new(r"background:var\(--([a-z-]+),").unwrap();
    let names: Vec<String> = (40..=47)
        .chain(100..=107)
        .map(|code| {
            let html = html(&code.to_string());
            let caps = background
                .captures(&html)
                .unwrap_or_else(|| panic!("SGR {code}: {html}"));
            format!("--{}", &caps[1])
        })
        .collect();
    assert_eq!(names.len(), 16);

    // The converter's outputs are these `pre`s.
    let template = read("templates/cell.html");
    for class in ["output output-stream", "output output-error"] {
        assert!(
            template.contains(&format!("<pre class=\"{class}")),
            "{class}"
        );
    }
    assert!(html("41").starts_with("<span style='background:var(--"));
    let scope = ".reveal .cell-outputs pre:is(.output, .output-text)";
    let rules = rules(&notebook_css());
    let palette = rule(
        &rules,
        &format!(r#"{scope} span:is([style*="background:var(--"], [style*="background:#"])"#),
    );
    let colour = |name: &str| {
        hex_rgb(
            palette
                .get(name)
                .unwrap_or_else(|| panic!("{name} is not in the terminal palette")),
        )
    };
    assert_eq!(palette.get("--ansi-default-fg"), Some("#000"));
    assert_eq!(
        palette.get("color"),
        Some("var(--ansi-fg, var(--ansi-default-fg))")
    );
    let white_text = rules
        .iter()
        .find(|r| r.get("--ansi-default-fg") == Some("#fff"))
        .expect("a rule for the backgrounds white text reads on");
    assert!(
        white_text.selector.starts_with(scope),
        "{}",
        white_text.selector
    );
    let on_white: BTreeSet<String> = background
        .captures_iter(&white_text.selector)
        .map(|caps| format!("--{}", &caps[1]))
        .collect();
    let expected = [
        "--black",
        "--red",
        "--green",
        "--blue",
        "--magenta",
        "--cyan",
        "--bright-black",
    ];
    assert_eq!(on_white, BTreeSet::from(expected.map(String::from)));
    let reverse = rule(
        &rules,
        &format!(r#"{scope} span[style^="color:var(--black,"][style*="background:var(--"]"#),
    );
    assert_eq!(reverse.get("--ansi-fg"), Some("initial"));
    assert_eq!(
        reverse.get("color"),
        Some("var(--ansi-default-fg) !important")
    );
    for name in &names {
        let selector = format!(r#"{scope} span[style*="color:var({name},"]"#);
        assert_eq!(rule(&rules, &selector).get("--ansi-fg"), palette.get(name));
    }
    // An 8-bit or RGB text colour starts its span; the colour the converter appends to a
    // background is not an ANSI colour.
    let raw = rule(&rules, &format!(r#"{scope} span[style^="color:#"]"#));
    assert_eq!(raw.get("--ansi-fg"), Some("currentColor"));

    let (black, white) = ([0, 0, 0], [255, 255, 255]);
    assert_eq!(colour("--black"), black);
    assert_eq!(colour("--bright-white"), white);
    for name in &names {
        let bg = colour(name);
        let default = if on_white.contains(name) {
            white
        } else {
            black
        };
        let ratio = contrast(default, bg);
        assert!(ratio >= 4.5, "default text on {name}: {ratio:.2}");
        if name != "--black" {
            let ratio = contrast(black, bg);
            assert!(
                ratio >= 4.5,
                "--black on {name} and {name} on --black: {ratio:.2}"
            );
        }
        if on_white.contains(name) {
            for fg in ["--white", "--bright-white"] {
                let ratio = contrast(colour(fg), bg);
                assert!(
                    ratio >= 4.5,
                    "{fg} on {name} and {name} on {fg}: {ratio:.2}"
                );
            }
        }
    }
    let ratio = contrast(hex_rgb("#626262"), colour("--yellow"));
    assert!(ratio >= 4.5, "IPython's highlight: {ratio:.2}");
}

/// The DataFrame header: the whole `thead` sticks (pandas writes two header rows for a named
/// index, and two `th` rows stuck at the same `top` overlap), flush with the frame's top edge
/// (`top` is minus the frame's padding-top, both in the frame's em, so the text size sits on
/// the rows), with separate borders so its underline moves with it. On a slide with its own
/// background it scrolls with the table and paints no background: the deck's background
/// colour would be a white band on a dark slide.
#[test]
fn dataframe_headers_stick_flush_and_follow_the_slide_background() {
    let rules = rules(&notebook_css());
    let outputs = rule(&rules, ".reveal .cell-outputs");
    let padding_top = outputs.get("padding").unwrap().split(' ').next().unwrap();
    assert_eq!(padding_top, "0.3em");

    let table = rule(&rules, ".reveal .output-display table");
    assert_eq!(table.get("border-collapse"), Some("separate"));
    assert_eq!(
        table.get("font-size"),
        Some("inherit"),
        "thead's em is the frame's"
    );
    assert_eq!(
        rule(&rules, ".reveal .output-display table :is(tr, caption)").get("font-size"),
        Some("var(--p-cell-font-size)")
    );
    let thead = rule(&rules, ".reveal .output-display table thead");
    assert_eq!(thead.get("position"), Some("sticky"));
    assert_eq!(thead.get("top"), Some(format!("-{padding_top}").as_str()));
    assert_eq!(
        rule(
            &rules,
            ".reveal .cell.hide-input .output-display table thead"
        )
        .get("top"),
        Some("0"),
        "a hidden-input cell's frame has no padding"
    );
    assert_eq!(
        rule(&rules, ".reveal .cell.hide-input .cell-outputs").get("padding"),
        Some("0")
    );
    let th = rule(&rules, ".reveal .output-display table thead th");
    assert_eq!(th.get("position"), None, "rows of a sticky th overlap");
    assert_eq!(
        th.get("background"),
        Some("var(--r-background-color, #fff)")
    );

    let own = ".reveal .slides section:is(.has-dark-background, .has-light-background, [data-background], [data-background-color], [data-background-image], [data-background-video], [data-background-iframe], [data-background-gradient])";
    assert_eq!(
        rule(&rules, &format!("{own} .output-display table thead")).get("position"),
        Some("static")
    );
    assert_eq!(
        rule(&rules, &format!("{own} .output-display table thead th")).get("background"),
        Some("none")
    );
}

/// The rules behind the browser checks of T33: outputs scroll past --p-output-max-height,
/// hidden parts are `display: none`, DataFrame headers stick, images fit, stderr and errors
/// are tinted with their tokens, prompts use theirs.
#[test]
fn notebook_outputs_scroll_and_hidden_parts_stay_hidden() {
    let rules = rules(&notebook_css());
    let outputs = rule(&rules, ".reveal .cell-outputs");
    assert_eq!(
        outputs.get("max-height"),
        Some("var(--p-output-max-height)")
    );
    assert_eq!(outputs.get("overflow"), Some("auto"));
    assert_eq!(outputs.get("border"), Some("var(--p-cell-border)"));
    assert_eq!(
        rule(&rules, ".reveal .cell").get("margin"),
        Some("var(--p-cell-gap) 0")
    );
    assert_eq!(
        rule(&rules, ".reveal .cell-prompt").get("color"),
        Some("var(--p-prompt-color)")
    );
    let hidden = rule(
        &rules,
        ".reveal .cell.hide-input .cell-input, .reveal .cell.hide-output .cell-outputs, .reveal .cell.hide-input .cell-prompt",
    );
    assert_eq!(hidden.get("display"), Some("none"));
    let header = rule(&rules, ".reveal .output-display table thead");
    assert_eq!(header.get("position"), Some("sticky"));
    let images = rule(
        &rules,
        ".reveal .output-display img.output-image, .reveal .output-display figure.output-image > svg",
    );
    assert_eq!(images.get("max-width"), Some("100%"));
    assert!(
        images.get("width").is_none() && images.get("height").is_none(),
        "keeps width/height attributes"
    );
    let stderr = rule(&rules, ".reveal .cell-outputs .output-stream.stderr");
    assert!(
        stderr
            .get("background")
            .unwrap()
            .contains("var(--p-stderr-color)")
    );
    let error = rule(&rules, ".reveal .cell-outputs .output-error");
    assert!(
        error
            .get("border-left")
            .unwrap()
            .contains("var(--p-error-color)")
    );
    assert!(
        error
            .get("background")
            .unwrap()
            .contains("var(--p-error-color)")
    );
}

/// The theme stylesheet is inlined into every document, and tests elsewhere scan whole
/// documents: no prompt text (tests/theme_skeleton.rs asserts its absence when prompts are
/// off), no lowercase `katex` (tests/render.rs, math off) and no `<section`, comments
/// included.
#[test]
fn theme_css_contains_no_text_that_document_tests_look_for() {
    for entry in stylesheets() {
        let source = read(&entry);
        for needle in ["In [", "Out [", "katex", "<section"] {
            assert!(!source.contains(needle), "{entry} contains {needle:?}");
        }
    }
}

// ---------------------------------------------------------------------------------------
// Print and PDF: boxes that scroll on screen print in full (audit F15)
// ---------------------------------------------------------------------------------------

/// The rule with exactly `selector` inside `context` (`""` for top level, `"@media print"`).
fn rule_in<'a>(rules: &'a [Rule], context: &str, selector: &str) -> &'a Rule {
    rules
        .iter()
        .find(|r| r.context == context && r.selector == selector)
        .unwrap_or_else(|| panic!("no rule `{selector}` in `{context}`"))
}

/// A unitless `line-height` or a length in px/em, as a number.
fn number(value: &str, unit: &str) -> f64 {
    value
        .strip_suffix(unit)
        .unwrap_or_else(|| panic!("{value} is not in {unit}"))
        .parse()
        .unwrap_or_else(|_| panic!("{value} is not a number"))
}

/// reveal's print view (`html.reveal-print`, which `presenter export pdf` loads) counts each
/// slide's pages from its height on screen, then Chrome prints. So the scroll caps of code
/// blocks (`--p-code-max-height`) and cell outputs (`--p-output-max-height`) must be lifted
/// in the print view itself, not only under `@media print`: there reveal would still measure
/// one page and clip the rest (the audit's variant B). Nothing scrolls sideways either: long
/// lines are clipped at the box (documented in the README's "Print and PDF").
#[test]
fn scroll_boxes_print_in_full() {
    let rules = rules(&css());
    for selector in [
        "html.reveal-print .reveal pre code",
        "html.reveal-print .reveal .cell-outputs",
    ] {
        let lifted = rule_in(&rules, "", selector);
        assert_eq!(lifted.get("max-height"), Some("none"), "{selector}");
        // A page break may fall between any two lines: it moves at most one.
        assert_eq!(lifted.get("orphans"), Some("1"), "{selector}");
        assert_eq!(lifted.get("widows"), Some("1"), "{selector}");
    }
    assert!(
        rules
            .iter()
            .all(|r| r.context != "@media print" || r.get("max-height").is_none()),
        "the caps are lifted where reveal measures, not only in print"
    );
    let outputs = rule_in(&rules, "", "html.reveal-print .reveal .cell-outputs");
    assert_eq!(outputs.get("overflow"), Some("hidden"));
    // A DataFrame header prints once: not sticky, not a repeated table header group.
    let thead = rule_in(
        &rules,
        "",
        "html.reveal-print .reveal .output-display table thead",
    );
    assert_eq!(thead.get("display"), Some("table-row-group"));
    assert_eq!(thead.get("position"), Some("static"));
}

/// Chrome moves a line (or table row) that straddles a page break to the next page, so a
/// box that spans pages prints taller than reveal measured, and its last lines fell off the
/// slide's last page (the example notebook lost rows 198 and 199 of 200). In print the
/// lifted boxes set their lines closer than on screen; the space that saves on a full page
/// must exceed the one line a page break moves, for the default sizes (36px text, code at
/// 0.6em, outputs and tables at 0.5em) on reveal's default 720px slide with its 4 % margin,
/// and not so much that a long slide often ends a page early (a blank last page).
#[test]
fn print_leaves_room_for_the_line_each_page_break_moves() {
    let rules = rules(&css());
    let root = rule(&rules, ":root");
    let text = number(root.get("--r-main-font-size").unwrap(), "px");
    let page = (720.0_f64 * 1.04).floor();
    let tokens = declared(&css(), "--p-");
    let em = |token: &str| number(&tokens[token], "em") * text;
    let code_font = em("--p-code-font-size");
    let cell_font = em("--p-cell-font-size");
    // Code sets its print line height relative to its `pre`'s (`lh` in `line-height` is the
    // parent's line height), so a child theme's own code line height is tightened too.
    let screen_code = number(rule(&rules, ".reveal pre").get("line-height").unwrap(), "");
    let print_code = screen_code
        * number(
            rule_in(&rules, "@media print", "html.reveal-print .reveal pre code")
                .get("line-height")
                .unwrap(),
            "lh",
        );
    let screen_output = number(
        rule(
            &rules,
            ".reveal .cell-outputs :where(pre.output, pre.output-text, pre.output-json)",
        )
        .get("line-height")
        .unwrap(),
        "",
    );
    let print_output = number(
        rule_in(
            &rules,
            "@media print",
            "html.reveal-print .reveal .cell-outputs pre",
        )
        .get("line-height")
        .unwrap(),
        "",
    );
    // A table row: its line, the vertical padding and the 1px rule under it.
    let cell = rule(&rules, ".reveal .output-display table :is(th, td)");
    let screen_padding = number(
        cell.get("padding").unwrap().split(' ').next().unwrap(),
        "em",
    );
    let table_line = number(cell.get("line-height").unwrap(), "");
    let print_cell = rule_in(
        &rules,
        "@media print",
        "html.reveal-print .reveal .output-display table :is(th, td)",
    );
    let print_padding = number(print_cell.get("padding-top").unwrap(), "em");
    assert_eq!(
        print_cell.get("padding-bottom"),
        print_cell.get("padding-top")
    );
    // (box, the height of one of its lines on screen and in print, in px)
    let cases = [
        ("code", code_font * screen_code, code_font * print_code),
        (
            "outputs",
            cell_font * screen_output,
            cell_font * print_output,
        ),
        (
            "table rows",
            cell_font * (table_line + 2.0 * screen_padding) + 1.0,
            cell_font * (table_line + 2.0 * print_padding) + 1.0,
        ),
    ];
    for (name, screen, print) in cases {
        assert!(print < screen, "{name}: {print} vs {screen}");
        // Not so much closer that a slide ends a whole page early (a blank last page).
        assert!(print / screen > 0.9, "{name}: {print} vs {screen}");
        let lines_per_page = (page / print).floor();
        let saved = lines_per_page * (screen - print);
        assert!(
            saved > print,
            "{name}: a page saves {saved:.1}px, a moved line needs {print:.1}px"
        );
    }
}

/// The print rules sit in the theme's print sections and say why (the README documents
/// them for theme authors).
#[test]
fn the_readme_documents_the_print_rules() {
    let readme = readme();
    let start = readme
        .find("\n## Print and PDF\n")
        .expect("README section Print and PDF");
    let section = &readme[start..];
    let section = &section[..section[3..].find("\n## ").map_or(section.len(), |i| i + 3)];
    for part in [
        "--p-code-max-height",
        "--p-output-max-height",
        "pdfMaxPagesPerSlide",
        "0.95lh",
        "text outputs at 1.3",
        "cut off",
        "header rows print once",
    ] {
        assert!(section.contains(part), "README Print and PDF: {part}");
    }
}

// ---------------------------------------------------------------------------------------
// The SCSS example keeps the default theme's code blocks (audit F44)
// ---------------------------------------------------------------------------------------

/// `examples/custom-theme-scss` includes reveal's theme template, whose rules come after the
/// default theme's: its `.reveal pre code { padding: 5px }` put the code 5px from the box
/// edge. The value that wins for each property (the last top-level rule that sets it for
/// exactly that selector; they all weigh the same) is the default theme's again.
#[test]
fn the_scss_example_keeps_the_default_code_blocks() {
    let deck =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/custom-theme-scss/slides.md");
    let built = build_deck(&BuildOptions::new(&deck)).unwrap();
    let comments = Regex::new(r"(?s)/\*.*?\*/").unwrap();
    let default = rules(&css());
    let rules = rules(&comments.replace_all(&built.document.theme_css, ""));
    let winner = |selector: &str, property: &str| -> String {
        rules
            .iter()
            .filter(|r| r.context.is_empty() && complex_selectors(&r.selector).contains(&selector))
            .filter_map(|r| r.get(property))
            .next_back()
            .unwrap_or_else(|| panic!("nothing sets {property} on {selector}"))
            .to_string()
    };
    // reveal's template does set these, so the example overrides something.
    assert!(
        rules
            .iter()
            .any(|r| r.selector == ".reveal pre code" && r.get("padding") == Some("5px")),
        "reveal's template rules are in the theme stylesheet"
    );
    assert_eq!(winner(".reveal pre code", "padding"), "0.75em 1em");
    assert_eq!(
        winner(".reveal pre code", "max-height"),
        "var(--p-code-max-height)"
    );
    assert_eq!(
        winner(".reveal pre", "font-size"),
        "var(--p-code-font-size)"
    );
    assert_eq!(winner(".reveal pre", "line-height"), "1.4");
    assert_eq!(winner(".reveal pre", "box-shadow"), "none");
    assert_eq!(winner(".reveal pre", "width"), "auto");
    assert_eq!(
        rule(&default, ".reveal pre code").get("padding"),
        Some("0.75em 1em")
    );
    assert_eq!(
        rule(&default, ".reveal pre").get("line-height"),
        Some("1.4")
    );
}

// ---------------------------------------------------------------------------------------
// Task lists (audit F45)
// ---------------------------------------------------------------------------------------

/// comrak renders `- [ ] open` as `<li><input type="checkbox" disabled="" /> open</li>` in a
/// plain list: without rules that is a bullet plus the browser's 12px greyed-out box next to
/// 36px text. The theme drops the bullet, draws a box sized in em in the accent colour, and
/// moves it into the bullet's place so the item's text lines up with the other items'.
#[test]
fn task_list_items_show_a_text_sized_box_instead_of_a_bullet() {
    let tmp = tempfile::tempdir().unwrap();
    let deck = tmp.path().join("tasks.md");
    std::fs::write(&deck, "# Tasks\n\n- [ ] open\n- [x] done\n").unwrap();
    let html = build_deck(&BuildOptions::new(&deck)).unwrap().document.html;
    assert!(
        html.contains("<li><input type=\"checkbox\" disabled=\"\" /> open</li>"),
        "the markup the rules select"
    );
    assert!(html.contains("<li><input type=\"checkbox\" checked=\"\" disabled=\"\" /> done</li>"));

    let rules = rules(&css());
    let item = rule(
        &rules,
        r#".reveal li:has(> input[type="checkbox"]:first-child)"#,
    );
    assert_eq!(item.get("list-style"), Some("none"));
    let box_ = rule(&rules, r#".reveal li > input[type="checkbox"]:first-child"#);
    assert_eq!(box_.get("appearance"), Some("none"), "not greyed out");
    assert_eq!(box_.get("font"), Some("inherit"), "em of the item's text");
    for side in ["width", "height"] {
        let size = number(box_.get(side).unwrap(), "em");
        assert!((0.6..=1.0).contains(&size), "{side}: {size}em");
    }
    assert!(box_.get("border").unwrap().contains("var(--p-accent)"));
    // The box starts where the bullet's indentation starts, and box, gap and the space after
    // the box (about 0.25em) end where the item's text would start: its text lines up with
    // the other items'.
    let list = rule(&rules, ".reveal ul, .reveal ol, .reveal dl");
    let indent = number(list.get("margin").unwrap().split(' ').nth(3).unwrap(), "em");
    let margin: Vec<&str> = box_.get("margin").unwrap().split(' ').collect();
    assert_eq!(number(margin[3], "em"), -indent);
    let width = number(box_.get("width").unwrap(), "em");
    let end = width + number(margin[1], "em") + 0.25;
    assert!((end - indent).abs() < 0.05, "{margin:?}");
    // Form controls do not inherit the text colour, which --p-accent mixes in.
    assert_eq!(box_.get("color"), Some("inherit"));
    let tick = rule(
        &rules,
        r#".reveal li > input[type="checkbox"]:first-child:checked::after"#,
    );
    assert!(tick.get("border").unwrap().contains("var(--p-accent)"));
    assert!(
        readme().contains("| `li > input[type=checkbox]` | task lists"),
        "README: what the stylesheet styles"
    );
}

// ---------------------------------------------------------------------------------------
// Shipped files (audit F47)
// ---------------------------------------------------------------------------------------

/// The theme's files ship in the binary: the CSS is inlined into every standalone deck, and
/// `theme eject` and `init --full-theme` copy all of them. They must not name the tasks that
/// built them or point at the repository's `docs/` (a binary user has neither); the theme's
/// own README is the documentation that travels with it. (README.md links the repository's
/// theme-format spec for readers of the repository; it is not checked here.)
#[test]
fn shipped_theme_files_name_no_tasks_or_repository_documents() {
    let task = Regex::new(r"\bT[0-9]{2}\b").unwrap();
    let mut checked = 0;
    for entry in walkdir::WalkDir::new(theme_dir()) {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() || entry.file_name() == "README.md" {
            continue;
        }
        let path = entry.path();
        let text = std::fs::read_to_string(path).unwrap();
        let name = path
            .strip_prefix(theme_dir())
            .unwrap()
            .display()
            .to_string();
        assert!(!task.is_match(&text), "{name} names a task id");
        for reference in ["docs/spec", "docs/tasks", ".md §", "config.md", "ir.md"] {
            assert!(!text.contains(reference), "{name} points at {reference}");
        }
        checked += 1;
    }
    assert!(checked >= 14, "{checked} files");
    let readme = readme();
    assert!(!task.is_match(&readme), "README names a task id");
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
