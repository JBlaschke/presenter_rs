//! T02 harness for the built-in theme skeleton (`assets/themes/default/`).
//!
//! There is no renderer yet (T15), so this test builds the minijinja environment by hand:
//! `path_loader` over the theme's `templates/`, HTML auto-escaping for `.html`, and stub
//! versions of exactly the helpers of docs/spec/theme-format.md §4. It renders
//! `tests/fixtures/ir/sample-deck.json` with a context shaped like §4, writes the result to
//! `target/preview/skeleton.html` (stylesheets and scripts linked relatively to `assets/`, so
//! the file opens straight from disk for the manual browser check) and asserts the structure
//! the templates guarantee (§6, §7).

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::PathBuf;

use minijinja::{AutoEscape, Environment, UndefinedBehavior, Value, path_loader};
use presenter::config::{Config, PluginId};
use presenter::ir::{Block, Deck, Features, Slide};
use regex::Regex;
use serde_json::json;

/// The helpers of theme-format.md §4. The harness registers these and nothing else.
const SPEC_HELPERS: [&str; 6] = [
    "style",
    "script",
    "asset",
    "theme_style",
    "reveal_init",
    "live_reload",
];

/// Filters templates may use: §4's own (`markdown`, `attrs`) plus minijinja 2.24's
/// built-ins with this crate's features (defaults + `json`; checked against
/// `src/defaults.rs`), which include `tojson` (§4's "json (built-in)"). Whitespace-separated.
const ALLOWED_FILTERS: &str = "attrs markdown \
    abs attr batch bool capitalize chain count d default dictsort e escape first float \
    format groupby indent int items join last length lines list lower map max min pprint \
    reject rejectattr replace reverse round safe select selectattr slice sort split string \
    sum title tojson trim unique upper zip";

/// The only expressions allowed to bypass auto-escaping: IR HTML (ir.md invariant 5).
const SAFE_EXPRESSIONS: [&str; 3] = ["block.html", "slide.notes", "out.html"];

/// The only asset of the fixture: `asset:32debefb8003` is blake3 of these bytes.
const FIXTURE_ASSET: (&str, &str) = ("32debefb8003", "tests/fixtures/md/images/ok.png");

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn theme_dir() -> PathBuf {
    root().join("assets/themes/default")
}

fn templates_dir() -> PathBuf {
    theme_dir().join("templates")
}

fn sample_deck() -> Deck {
    let path = root().join("tests/fixtures/ir/sample-deck.json");
    let text = std::fs::read_to_string(path).expect("fixture exists");
    let deck: Deck = serde_json::from_str(&text).expect("fixture deserializes");
    assert!(deck.invariant_violations().is_empty());
    deck
}

fn manifest() -> toml::Table {
    let text = std::fs::read_to_string(theme_dir().join("theme.toml")).expect("theme.toml");
    toml::from_str(&text).expect("theme.toml parses with toml")
}

/// Every template file below `templates/`, as loader names (`layouts/title.html`).
fn template_names() -> Vec<String> {
    let dir = templates_dir();
    let mut names: Vec<String> = walkdir::WalkDir::new(&dir)
        .into_iter()
        .map(|entry| entry.expect("readable template dir"))
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            let rel = entry.path().strip_prefix(&dir).expect("below templates/");
            rel.to_string_lossy().replace('\\', "/")
        })
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------------------
// Environment with stub helpers
// ---------------------------------------------------------------------------------------

/// The `attrs` filter of §4: `{k: v}` → ` k="v"` pairs in map order, escaped.
fn attrs_filter(map: Value) -> Result<Value, minijinja::Error> {
    let mut out = String::new();
    if !(map.is_undefined() || map.is_none()) {
        for key in map.try_iter()? {
            let value = map.get_item(&key)?;
            let key = html_escape::encode_double_quoted_attribute(key.as_str().unwrap_or(""));
            let value = value.to_string();
            let value = html_escape::encode_double_quoted_attribute(&value);
            out.push_str(&format!(" {key}=\"{value}\""));
        }
    }
    Ok(Value::from_safe_string(out))
}

/// `Reveal.initialize` call in the §4 shape, built from the context's `reveal` object.
fn reveal_init_script(reveal: &serde_json::Value) -> String {
    let config_json = reveal["config_json"].as_str().expect("config_json string");
    let plugins: Vec<&str> = reveal["plugins"]
        .as_array()
        .expect("plugins array")
        .iter()
        .map(|p| p.as_str().expect("plugin identifier"))
        .collect();
    format!(
        "<script>Reveal.initialize(Object.assign({config_json}, {{ plugins: [{}] }}));</script>",
        plugins.join(", ")
    )
}

fn environment(context: &serde_json::Value, undefined: UndefinedBehavior) -> Environment<'static> {
    let mut env = Environment::new();
    env.set_loader(path_loader(templates_dir()));
    env.set_auto_escape_callback(|name| {
        if name.ends_with(".html") {
            AutoEscape::Html
        } else {
            AutoEscape::None
        }
    });
    env.set_undefined_behavior(undefined);

    let css = std::fs::read_to_string(theme_dir().join("styles/theme.css")).expect("theme.css");
    let init = reveal_init_script(&context["reveal"]);
    env.add_function("style", |path: String| {
        Value::from_safe_string(format!(
            r#"<link rel="stylesheet" href="../../assets/{path}">"#
        ))
    });
    env.add_function("script", |path: String| {
        Value::from_safe_string(format!(r#"<script src="../../assets/{path}"></script>"#))
    });
    env.add_function("asset", |path: String| format!("../../assets/{path}"));
    env.add_function("theme_style", move || {
        Value::from_safe_string(format!("<style id=\"presenter-theme\">\n{css}</style>"))
    });
    env.add_function("reveal_init", move || Value::from_safe_string(init.clone()));
    env.add_function("live_reload", || Value::from_safe_string(String::new()));
    env.add_filter("attrs", attrs_filter);
    env
}

// ---------------------------------------------------------------------------------------
// Context (theme-format.md §4)
// ---------------------------------------------------------------------------------------

/// A slide as JSON plus `layout_template` (and the same for its children), as the context
/// builder (T15) will do: `layouts/<layout>.html` when that file exists, else the default.
fn decorate(slide: &Slide) -> serde_json::Value {
    let mut value = serde_json::to_value(slide).expect("slide serializes");
    let candidate = format!("layouts/{}.html", slide.layout);
    let layout_template = if templates_dir().join(&candidate).is_file() {
        candidate
    } else {
        "layouts/default.html".to_string()
    };
    value["layout_template"] = layout_template.into();
    value["children"] = slide.children.iter().map(decorate).collect();
    value
}

/// Plugins the deck needs: enabled ones, with highlight only for code and notes only for
/// notes (T15's rule), as (JS identifier, script path).
fn plugins(config: &Config, features: Features) -> Vec<(&'static str, &'static str)> {
    config
        .enabled_plugins()
        .into_iter()
        .filter(|id| match id {
            PluginId::Highlight => features.code,
            PluginId::Notes => features.notes,
            PluginId::Zoom | PluginId::Search => true,
        })
        .map(|id| match id {
            PluginId::Highlight => ("RevealHighlight", "vendor/reveal/dist/plugin/highlight.js"),
            PluginId::Notes => ("RevealNotes", "vendor/reveal/dist/plugin/notes.js"),
            PluginId::Zoom => ("RevealZoom", "vendor/reveal/dist/plugin/zoom.js"),
            PluginId::Search => ("RevealSearch", "vendor/reveal/dist/plugin/search.js"),
        })
        .collect()
}

fn theme_context() -> serde_json::Value {
    let manifest = manifest();
    let styles = &manifest["styles"];
    let scripts = manifest
        .get("scripts")
        .and_then(|s| s.get("entries"))
        .cloned()
        .unwrap_or_else(|| toml::Value::Array(Vec::new()));
    json!({
        "name": manifest["name"].as_str(),
        "chain": [manifest["name"].as_str()],
        "reveal_theme": styles["reveal_theme"].as_str(),
        "highlight_style": styles["highlight_style"].as_str(),
        "vars": manifest.get("vars").cloned().unwrap_or_else(|| toml::Table::new().into()),
        "scripts": scripts,
    })
}

fn context(deck: &Deck) -> serde_json::Value {
    let mut config = Config::default();
    config
        .merge_front_matter(&deck.meta)
        .expect("fixture front matter merges");
    let plugins = plugins(&config, deck.features);
    json!({
        "deck": deck,
        "meta": deck.meta,
        "slides": deck.slides.iter().map(decorate).collect::<Vec<_>>(),
        "features": deck.features,
        "config": config,
        "reveal": {
            "config_json": config.reveal_options_json(),
            "plugins": plugins.iter().map(|p| p.0).collect::<Vec<_>>(),
            "plugin_scripts": plugins.iter().map(|p| p.1).collect::<Vec<_>>(),
        },
        "theme": theme_context(),
        "build": { "mode": "build", "standalone": true, "live_reload": false,
                   "version": env!("CARGO_PKG_VERSION"), "id": 0 },
    })
}

fn render_with(context: &serde_json::Value, undefined: UndefinedBehavior) -> String {
    let env = environment(context, undefined);
    let template = env.get_template("deck.html").expect("deck.html loads");
    template
        .render(Value::from_serialize(context))
        .unwrap_or_else(|e| panic!("deck.html renders: {e:#}"))
}

/// Renders the fixture and rewrites its asset placeholder to a path relative to
/// `target/preview/` (what the emitter, T14, does with real URLs).
fn render_sample() -> String {
    let (id, rel) = FIXTURE_ASSET;
    let bytes = std::fs::read(root().join(rel)).expect("fixture image");
    assert_eq!(&blake3::hash(&bytes).to_hex()[..12], id);
    let html = render_with(&context(&sample_deck()), UndefinedBehavior::Lenient);
    html.replace(&format!("asset:{id}"), &format!("../../{rel}"))
}

// ---------------------------------------------------------------------------------------
// A minimal <div>/<section> tree of the slides region
// ---------------------------------------------------------------------------------------

/// A `<section>` of the rendered document.
#[derive(Debug)]
struct Section {
    /// The opening tag verbatim, e.g. `<section class="layout-default">`.
    open_tag: String,
    /// Byte range of the inner HTML.
    inner: Range<usize>,
    /// `false` when a `<div>` sits between this section and its parent (or `.slides`).
    direct: bool,
    children: Vec<Section>,
}

enum Open {
    Div,
    Section(Section),
}

/// Parses the `<section>`s inside `<div class="slides">` by tracking `<div>`/`<section>`
/// nesting (the only container elements the templates emit; IR HTML closes its divs).
fn slides_tree(html: &str) -> Vec<Section> {
    let marker = r#"<div class="slides">"#;
    let start = html.find(marker).expect("slides container") + marker.len();
    assert!(
        !html[..start].contains("<section"),
        "section outside .slides"
    );
    let tag = Regex::new(r"<(/?)(div|section)(?:\s[^>]*)?>").expect("regex");
    let mut stack: Vec<Open> = Vec::new();
    let mut roots = Vec::new();
    for m in tag.captures_iter(&html[start..]) {
        let whole = m.get(0).expect("match");
        let (closing, name) = (&m[1] == "/", &m[2]);
        let (begin, end) = (start + whole.start(), start + whole.end());
        match (closing, name) {
            (false, "div") => stack.push(Open::Div),
            (false, _) => {
                let direct = matches!(stack.last(), None | Some(Open::Section(_)));
                stack.push(Open::Section(Section {
                    open_tag: whole.as_str().to_string(),
                    inner: end..end,
                    direct,
                    children: Vec::new(),
                }));
            }
            (true, "div") => match stack.pop() {
                None => return roots, // closes .slides
                Some(Open::Div) => {}
                Some(Open::Section(s)) => panic!("</div> closes {}", s.open_tag),
            },
            (true, _) => {
                let Some(Open::Section(mut section)) = stack.pop() else {
                    panic!("</section> does not close a section");
                };
                section.inner.end = begin;
                let parent = stack.iter_mut().rev().find_map(|open| match open {
                    Open::Section(parent) => Some(parent),
                    Open::Div => None,
                });
                match parent {
                    Some(parent) => parent.children.push(section),
                    None => roots.push(section),
                }
            }
        }
    }
    panic!(".slides is never closed");
}

/// The opening tag `layouts/default.html` must produce for `slide`.
fn expected_open_tag(slide: &Slide) -> String {
    let id = slide
        .id
        .as_ref()
        .map(|id| format!(r#" id="{id}""#))
        .unwrap_or_default();
    let classes: String = slide.classes.iter().map(|c| format!(" {c}")).collect();
    let attrs: String = slide
        .attrs
        .iter()
        .map(|(k, v)| {
            format!(
                r#" {k}="{}""#,
                html_escape::encode_double_quoted_attribute(v)
            )
        })
        .collect();
    format!(
        r#"<section{id} class="layout-{}{classes}"{attrs}>"#,
        slide.layout
    )
}

/// Number of elements whose `class` attribute contains the token `cell` (not `cell-input`).
fn count_cell_elements(html: &str) -> usize {
    let class = Regex::new(r#"class="([^"]*)""#).expect("regex");
    class
        .captures_iter(html)
        .filter(|c| c[1].split_whitespace().any(|token| token == "cell"))
        .count()
}

fn cell_blocks(slide: &Slide) -> usize {
    slide
        .blocks
        .iter()
        .filter(|b| matches!(b, Block::Cell(_)))
        .count()
}

/// Checks one rendered layout section against its slide.
fn check_leaf(html: &str, section: &Section, slide: &Slide, label: &str) {
    assert!(section.direct, "{label}: wrapped in a <div>");
    assert!(section.children.is_empty(), "{label}: nested sections");
    assert_eq!(section.open_tag, expected_open_tag(slide), "{label}");
    let inner = &html[section.inner.clone()];
    let notes = inner.matches(r#"<aside class="notes">"#).count();
    assert_eq!(notes, usize::from(slide.notes.is_some()), "{label}: notes");
    if let Some(text) = &slide.notes {
        assert!(
            inner.contains(&format!("<aside class=\"notes\">{text}</aside>")),
            "{label}"
        );
    }
    assert_eq!(
        count_cell_elements(inner),
        cell_blocks(slide),
        "{label}: cells"
    );
}

// ---------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------

#[test]
fn renders_sample_deck_into_reveal_structure() {
    let deck = sample_deck();
    let html = render_sample();
    let preview = root().join("target/preview/skeleton.html");
    std::fs::create_dir_all(preview.parent().expect("parent")).expect("mkdir target/preview");
    std::fs::write(&preview, &html).expect("write preview");

    let roots = slides_tree(&html);
    assert_eq!(roots.len(), deck.slides.len(), "top-level sections");
    for (i, (section, slide)) in roots.iter().zip(&deck.slides).enumerate() {
        assert!(
            section.direct,
            "slides[{i}] is not a direct child of .slides"
        );
        if slide.children.is_empty() {
            check_leaf(&html, section, slide, &format!("slides[{i}]"));
            continue;
        }
        assert_eq!(section.open_tag, "<section>", "slides[{i}]: stack wrapper");
        assert_eq!(section.children.len(), 1 + slide.children.len());
        let members = std::iter::once(slide).chain(&slide.children);
        for (j, (sub, member)) in section.children.iter().zip(members).enumerate() {
            check_leaf(&html, sub, member, &format!("slides[{i}] stack member {j}"));
        }
    }

    let with_notes = deck
        .slides
        .iter()
        .flat_map(|s| std::iter::once(s).chain(&s.children))
        .filter(|s| s.notes.is_some())
        .count();
    assert_eq!(html.matches(r#"<aside class="notes">"#).count(), with_notes);
    assert!(html.contains(r#"class="layout-title""#));
    let cells: usize = deck.slides.iter().map(cell_blocks).sum();
    assert_eq!(cells, 2);
    assert_eq!(count_cell_elements(&html), cells);
    assert_eq!(
        html.matches(r#"class="cell"#).count() - cell_parts(&html),
        cells
    );
}

/// `class="cell-…"` occurrences (cell-input, cell-outputs, cell-prompt), which the literal
/// substring `class="cell` also matches.
fn cell_parts(html: &str) -> usize {
    html.matches(r#"class="cell-"#).count()
}

#[test]
fn title_layout_shows_meta_under_the_heading() {
    let html = render_sample();
    let start = html
        .find(r#"<section id="intro" class="layout-title">"#)
        .expect("title");
    let section = &html[start..start + html[start..].find("</section>").expect("closed")];
    let heading = section
        .find("<h1>Rust for data people</h1>")
        .expect("heading");
    for (class, text) in [
        ("subtitle", "Fast tools without the footguns"),
        ("author", "J. Blaschke"),
        ("date", "2026-10-08"),
    ] {
        let element = format!(r#"class="{class}">{text}</"#);
        let at = section
            .find(&element)
            .unwrap_or_else(|| panic!("missing {element}"));
        assert!(at > heading, "{element} must follow the heading");
    }
    assert!(section.find("subtitle") < section.find(r#"<aside class="notes">"#));
}

#[test]
fn cells_render_input_outputs_and_flags() {
    let html = render_sample();
    // First cell: prompt, escaped source, one element per output kind.
    for part in [
        r#"<div class="cell" data-cell-index="4">"#,
        r#"<div class="cell-prompt">In [3]:</div>"#,
        r#"<pre><code class="language-python" data-trim>print(&quot;rows:&quot;, len(df))"#,
        r#"<pre class="output output-stream stdout">rows: 1000"#,
        r#"<pre class="output output-stream stderr"><span style='color:var(--yellow,#a50)'>warning</span>"#,
        r#"<div class="output output-display" data-mime="text&#x2f;html">"#,
        r#"<table class="dataframe">"#,
        r#"<div class="output output-unsupported">No renderable output (application&#x2f;vnd.jupyter.widget-view+json)</div>"#,
        r#"<pre class="output output-error"><span style='color:var(--red,#a00)'>"#,
    ] {
        assert!(html.contains(part), "missing {part}");
    }
    // Second cell: fragment + hide-input; Out [n] prompt only for execute results.
    assert!(html.contains(r#"<div class="cell fragment hide-input" data-cell-index="5">"#));
    assert_eq!(
        html.matches(r#"<div class="cell-prompt">Out [4]:</div>"#)
            .count(),
        1
    );
    assert_eq!(html.matches("Out [").count(), 1);
    assert!(
        html.contains(r#"<img class="output-image" src="../../tests/fixtures/md/images/ok.png""#)
    );
}

#[test]
fn head_and_scripts_follow_the_spec_order() {
    let html = render_sample();
    let head_end = html.find("</head>").expect("head");
    let theme = html
        .find(r#"<style id="presenter-theme">"#)
        .expect("theme style");
    let last_link = html[..head_end]
        .rfind(r#"<link rel="stylesheet""#)
        .expect("links");
    assert!(
        last_link < theme && theme < head_end,
        "theme stylesheet last in <head>"
    );
    for css in [
        "reveal/dist/reveal.css",
        "theme/white.css",
        "highlight/monokai.css",
        "katex.min.css",
    ] {
        assert!(html[..head_end].contains(css), "missing {css}");
    }
    assert!(html.starts_with("<!DOCTYPE html>\n<html lang=\"en\">"));
    assert!(html.contains("<title>Rust for data people</title>"));

    let order = [
        "vendor/reveal/dist/reveal.js\"",
        "plugin/highlight.js",
        "plugin/notes.js",
        "plugin/zoom.js",
        "plugin/search.js",
        "vendor/katex/katex.min.js",
        "presenter/math.js",
        "Reveal.initialize(Object.assign({",
    ];
    let positions: Vec<usize> = order
        .iter()
        .map(|s| html.find(s).unwrap_or_else(|| panic!("missing {s}")))
        .collect();
    assert!(
        positions.windows(2).all(|w| w[0] < w[1]),
        "script order {positions:?}"
    );
    assert!(html.contains("plugins: [RevealHighlight, RevealNotes, RevealZoom, RevealSearch]"));
    assert!(
        !Regex::new("asset:[0-9a-f]{12}")
            .expect("regex")
            .is_match(&html)
    );
    assert!(
        !html.contains("{{") && !html.contains("{%"),
        "template syntax leaked"
    );
}

#[test]
fn strict_undefined_renders_identically() {
    let context = context(&sample_deck());
    let lenient = render_with(&context, UndefinedBehavior::Lenient);
    let strict = render_with(&context, UndefinedBehavior::Strict);
    assert_eq!(lenient, strict);
}

#[test]
fn renders_without_optional_meta_and_layout_template() {
    let mut deck = sample_deck();
    deck.meta = Default::default();
    deck.features = Features::default();
    let mut slide = Slide::new();
    slide.layout = "title".into();
    slide.blocks.push(Block::html("<h1>Only</h1>\n".into()));
    deck.slides = vec![slide];
    let mut context = context(&deck);
    // Hand-built contexts may lack `layout_template`; slide.html falls back to the default.
    context["slides"][0]
        .as_object_mut()
        .expect("slide object")
        .remove("layout_template");
    let html = render_with(&context, UndefinedBehavior::Lenient);
    assert!(html.contains(r#"<section class="layout-title">"#));
    assert!(!html.contains("subtitle") && !html.contains(r#"class="author""#));
    assert!(html.contains("<title>Presentation</title>"));
    for absent in [
        "vendor/katex/",
        "presenter/math.js",
        "plugin/highlight",
        "RevealHighlight",
        "RevealNotes",
    ] {
        assert!(!html.contains(absent), "{absent} without the feature");
    }
}

#[test]
fn theme_toml_parses_and_declares_the_skeleton() {
    let manifest = manifest();
    assert_eq!(manifest["name"].as_str(), Some("default"));
    assert!(manifest.get("extends").is_none());
    let styles = &manifest["styles"];
    let entries = styles["entries"].as_array().expect("entries");
    // theme.css first; later tasks may append entries (T33: styles/notebook.css).
    assert_eq!(
        entries.first(),
        Some(&toml::Value::from("styles/theme.css"))
    );
    for entry in entries {
        assert!(theme_dir().join(entry.as_str().expect("path")).is_file());
    }
    assert_eq!(styles["reveal_theme"].as_str(), Some("white"));
    assert_eq!(styles["highlight_style"].as_str(), Some("monokai"));
    // The top-level keys of theme-format.md §2 (T13 parses with deny_unknown_fields).
    let known = BTreeSet::from([
        "name",
        "extends",
        "description",
        "styles",
        "scripts",
        "reveal",
        "layouts",
        "vars",
    ]);
    for key in manifest.keys() {
        assert!(known.contains(key.as_str()), "unknown manifest key {key}");
    }
    let layouts: BTreeSet<&str> = manifest["layouts"]
        .as_table()
        .expect("[layouts]")
        .keys()
        .map(String::as_str)
        .collect();
    for layout in ["blank", "default", "section", "title"] {
        assert!(layouts.contains(layout), "[layouts] lacks {layout}");
    }
}

/// Tag regions (`{{ … }}`, `{% … %}`) of a template, comments removed.
fn tag_regions(source: &str) -> Vec<String> {
    let comment = Regex::new(r"(?s)\{#.*?#\}").expect("regex");
    let tags = Regex::new(r"(?s)\{\{.*?\}\}|\{%.*?%\}").expect("regex");
    let source = comment.replace_all(source, "");
    tags.find_iter(&source)
        .map(|m| m.as_str().to_string())
        .collect()
}

fn read_template(name: &str) -> String {
    std::fs::read_to_string(templates_dir().join(name)).expect("template readable")
}

#[test]
fn every_referenced_template_exists() {
    let names: BTreeSet<String> = template_names().into_iter().collect();
    // Root-theme requirements (theme-format.md §3), the T02 layouts and partials.
    for required in [
        "deck.html",
        "slide.html",
        "block.html",
        "cell.html",
        "layouts/default.html",
        "layouts/title.html",
        "layouts/section.html",
        "layouts/blank.html",
        "partials/head.html",
        "partials/scripts.html",
    ] {
        assert!(names.contains(required), "missing {required}");
    }
    // Every template name written in a tag (include/extends targets, include fallbacks).
    let literal = Regex::new(r#""([^"]+\.html)""#).expect("regex");
    let mut referenced = 0;
    for name in &names {
        for region in tag_regions(&read_template(name)) {
            for target in literal.captures_iter(&region) {
                referenced += 1;
                assert!(
                    names.contains(&target[1]),
                    "{name} references missing {}",
                    &target[1]
                );
            }
        }
    }
    assert!(referenced >= 8, "scan found only {referenced} references");
    // Dynamic includes go through `layout_template`: each documented layout has its file
    // and each layout file is documented.
    let documented: BTreeSet<String> = manifest()["layouts"]
        .as_table()
        .expect("[layouts]")
        .keys()
        .map(|k| format!("layouts/{k}.html"))
        .collect();
    let files: BTreeSet<String> = names
        .iter()
        .filter(|n| n.starts_with("layouts/"))
        .cloned()
        .collect();
    assert_eq!(documented, files);
}

#[test]
fn templates_use_only_spec_helpers_and_filters() {
    let strings = Regex::new(r#""[^"]*"|'[^']*'"#).expect("regex");
    let filter = Regex::new(r"\|\s*([A-Za-z_]\w*)").expect("regex");
    let test = Regex::new(r"\bis\s+(?:not\s+)?[A-Za-z_]\w*").expect("regex");
    let call = Regex::new(r"(?:^|[^\w.])([A-Za-z_]\w*)\s*\(").expect("regex");
    let safe = Regex::new(r"([\w.]+)\s*\|\s*safe\b").expect("regex");
    let keywords = ["and", "or", "not", "in", "is", "if", "elif", "else"];
    let allowed_filters: BTreeSet<&str> = ALLOWED_FILTERS.split_whitespace().collect();
    let mut helpers_used = BTreeSet::new();
    for name in template_names() {
        for region in tag_regions(&read_template(&name)) {
            for m in safe.captures_iter(&region) {
                assert!(
                    SAFE_EXPRESSIONS.contains(&&m[1]),
                    "{name}: `{}|safe`",
                    &m[1]
                );
            }
            let region = strings.replace_all(&region, "\"\"");
            for m in filter.captures_iter(&region) {
                assert!(
                    allowed_filters.contains(&m[1]),
                    "{name}: filter `{}`",
                    &m[1]
                );
            }
            let region = filter.replace_all(&region, "");
            let region = test.replace_all(&region, "");
            for m in call.captures_iter(&region) {
                let ident = m[1].to_string();
                if keywords.contains(&ident.as_str()) || ident == "super" {
                    continue;
                }
                assert!(
                    SPEC_HELPERS.contains(&ident.as_str()),
                    "{name}: helper `{ident}`"
                );
                helpers_used.insert(ident);
            }
        }
    }
    // The document shell needs these; `asset` is optional (e.g. a logo in a layout).
    for needed in [
        "live_reload",
        "reveal_init",
        "script",
        "style",
        "theme_style",
    ] {
        assert!(helpers_used.contains(needed), "{needed} is never called");
    }
}

#[test]
fn attrs_filter_escapes_values() {
    let env = environment(&context(&sample_deck()), UndefinedBehavior::Lenient);
    let out = env
        .render_str(
            "{{ a|attrs }}|{{ none|attrs }}",
            minijinja::context! { a => json!({"data-x": "a\"b&c", "data-y": ""}) },
        )
        .expect("renders");
    assert_eq!(out, r#" data-x="a&quot;b&amp;c" data-y=""|"#);
}
