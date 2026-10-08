//! T02 harness for the built-in theme skeleton (`assets/themes/default/`).
//!
//! There is no renderer yet (T15), so this test builds the minijinja environment by hand:
//! `path_loader` over the theme's `templates/`, HTML auto-escaping for `.html`, and stub
//! versions of exactly the helpers of docs/spec/theme-format.md §4. It renders
//! `tests/fixtures/ir/sample-deck.json` with a context shaped like §4, writes the result to
//! `target/preview/skeleton.html` (stylesheets and scripts linked relatively to `assets/`, so
//! the file opens straight from disk for the manual browser check) and asserts the structure
//! the templates guarantee (§6, §7).
//!
//! Markup assertions work on parsed opening tags (element name, class tokens, decoded
//! attributes) rather than on literal tag strings, so that T17 (design) and T33 (cell.html)
//! can reorder classes, add classes or attributes and change whitespace without touching
//! this file. What stays pinned is what §6/§7 guarantee.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::LazyLock;

use minijinja::{AutoEscape, Environment, UndefinedBehavior, Value, path_loader};
use presenter::config::{Config, PluginId};
use presenter::ir::{Block, Deck, Features, Slide};
use regex::Regex;
use serde_json::json;

/// `&'static Regex` for a literal pattern, compiled once: the scans run every pattern over
/// every tag of every template.
macro_rules! regex {
    ($pattern:expr) => {{
        static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new($pattern).expect("regex"));
        &*RE
    }};
}

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

/// Callables minijinja itself provides, which are not helpers in the §4 sense: the
/// built-in globals of minijinja 2.24 `src/defaults.rs` (`range`, `dict`, `debug`,
/// `namespace`) and the special names of blocks, macros and loops.
const MINIJINJA_CALLABLES: &str =
    "range dict debug namespace super self caller loop varargs kwargs";

/// Statement keywords that may directly precede `(` as the first word of a `{% … %}` tag:
/// before a tuple target (`{% for (a, b) in … %}`, `set`, `{% with (a, b) = … %}`), a
/// `{% call(x) row() %}` signature, or a parenthesized expression (`if`, `elif`, `include`,
/// `import`, `extends`, `from`, `do`). minijinja 2.24 reserves none of these names (its
/// `RESERVED_NAMES`, which cannot be assigned, are `true`/`True`, `false`/`False`,
/// `none`/`None`, `loop` and `self`), so anywhere else `with(…)` calls a value or global
/// named `with`, and the scan treats it as a call ([`is_keyword_use`]).
const STATEMENT_KEYWORDS: &str = "for set with call if elif include import extends from do";

/// Operator keywords that may directly precede `(` after an operand: `a and (b)`,
/// `a or (b)`, `x in (1, 2)`, `x not in (…)`, `a if (b) else (c)`, `{% for x in y if (z) %}`.
/// In operand position (`{{ and(1) }}`) minijinja reads them as names, so there they are
/// calls. The unary `not` is exempt everywhere.
const OPERATOR_KEYWORDS: &str = "and or in if else";

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

/// `styles.entries` of the manifest, in order.
fn style_entries() -> Vec<String> {
    manifest()["styles"]["entries"]
        .as_array()
        .expect("styles.entries")
        .iter()
        .map(|entry| entry.as_str().expect("entry path").to_string())
        .collect()
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

fn read_template(name: &str) -> String {
    std::fs::read_to_string(templates_dir().join(name)).expect("template readable")
}

/// Every template as `(name, source)`.
fn templates() -> Vec<(String, String)> {
    template_names()
        .into_iter()
        .map(|name| {
            let source = read_template(&name);
            (name, source)
        })
        .collect()
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

/// The theme stylesheet of §5: every `styles.entries` file, in order, concatenated.
fn theme_css() -> String {
    style_entries()
        .iter()
        .map(|entry| std::fs::read_to_string(theme_dir().join(entry)).expect("style entry"))
        .collect()
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

    let css = theme_css();
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

/// Removes `layout_template` from every slide and child of a context's `slides`.
fn undecorate(slides: &mut serde_json::Value) {
    for slide in slides.as_array_mut().expect("slides array") {
        slide
            .as_object_mut()
            .expect("slide object")
            .remove("layout_template");
        undecorate(&mut slide["children"]);
    }
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

fn try_render(
    context: &serde_json::Value,
    undefined: UndefinedBehavior,
) -> Result<String, minijinja::Error> {
    let env = environment(context, undefined);
    let template = env.get_template("deck.html").expect("deck.html loads");
    template.render(Value::from_serialize(context))
}

fn render_with(context: &serde_json::Value, undefined: UndefinedBehavior) -> String {
    try_render(context, undefined).unwrap_or_else(|e| panic!("deck.html renders: {e:#}"))
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
// Opening tags with class tokens and decoded attributes
// ---------------------------------------------------------------------------------------

/// An opening tag of the rendered document.
#[derive(Debug, Clone)]
struct Tag {
    /// Lower-case element name.
    name: String,
    /// Attributes, values entity-decoded; boolean attributes map to `""`.
    attrs: BTreeMap<String, String>,
    /// Byte range of the tag itself (`<` … `>`) in the string it was parsed from.
    range: Range<usize>,
}

impl Tag {
    fn parse(text: &str, start: usize) -> Tag {
        let name = regex!(r"^<([A-Za-z][\w-]*)");
        let attr = regex!(r#"([^\s=/>"']+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>"']+)))?"#);
        let head = name.captures(text).expect("tag name");
        let rest = &text[head[0].len()..text.len() - 1];
        let attrs = attr
            .captures_iter(rest)
            .map(|c| {
                let value = c
                    .get(2)
                    .or(c.get(3))
                    .or(c.get(4))
                    .map_or("", |m| m.as_str());
                let value = html_escape::decode_html_entities(value).into_owned();
                (c[1].to_ascii_lowercase(), value)
            })
            .collect();
        Tag {
            name: head[1].to_ascii_lowercase(),
            attrs,
            range: start..start + text.len(),
        }
    }

    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.get(name).map(String::as_str)
    }

    fn classes(&self) -> Vec<&str> {
        self.attr("class")
            .map(|c| c.split_whitespace().collect())
            .unwrap_or_default()
    }

    fn has_class(&self, class: &str) -> bool {
        self.classes().contains(&class)
    }

    fn has_classes(&self, classes: &[&str]) -> bool {
        classes.iter().all(|class| self.has_class(class))
    }
}

/// Every opening tag of `html`, in document order.
fn open_tags(html: &str) -> Vec<Tag> {
    regex!(r"<[A-Za-z][^>]*>")
        .find_iter(html)
        .map(|m| Tag::parse(m.as_str(), m.start()))
        .collect()
}

/// The first tag satisfying `pred`, or a panic naming `what`.
fn find_tag<'a>(tags: &'a [Tag], what: &str, pred: impl Fn(&Tag) -> bool) -> &'a Tag {
    tags.iter()
        .find(|t| pred(t))
        .unwrap_or_else(|| panic!("no element {what}"))
}

/// Inner HTML of `tag` (an element of `html`) up to its matching closing tag.
fn inner_html<'a>(html: &'a str, tag: &Tag) -> &'a str {
    let pattern = format!(r"<(/?){}\b[^>]*>", regex::escape(&tag.name));
    let same = Regex::new(&pattern).expect("regex");
    let start = tag.range.end;
    let mut depth = 1;
    for m in same.captures_iter(&html[start..]) {
        depth = if &m[1] == "/" { depth - 1 } else { depth + 1 };
        if depth == 0 {
            return &html[start..start + m.get(0).expect("match").start()];
        }
    }
    panic!("<{}> is never closed", tag.name)
}

/// The text right after `tag` up to the next tag, decoded and trimmed.
fn text_after(html: &str, tag: &Tag) -> String {
    let rest = &html[tag.range.end..];
    let text = &rest[..rest.find('<').unwrap_or(rest.len())];
    html_escape::decode_html_entities(text).trim().to_string()
}

/// The opening tag of `<div class="slides">`.
fn slides_div(html: &str) -> Tag {
    let tags = open_tags(html);
    find_tag(&tags, ".slides", |t| {
        t.name == "div" && t.has_class("slides")
    })
    .clone()
}

/// The inner HTML of `<div class="slides">`.
fn slides_region(html: &str) -> &str {
    inner_html(html, &slides_div(html))
}

// ---------------------------------------------------------------------------------------
// A minimal <div>/<section> tree of the slides region
// ---------------------------------------------------------------------------------------

/// A `<section>` of the rendered document.
#[derive(Debug)]
struct Section {
    /// The opening tag.
    tag: Tag,
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
    let start = slides_div(html).range.end;
    assert!(
        !html[..start].contains("<section"),
        "section outside .slides"
    );
    let tag = regex!(r"<(/?)(div|section)(?:\s[^>]*)?>");
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
                    tag: Tag::parse(whole.as_str(), begin),
                    inner: end..end,
                    direct,
                    children: Vec::new(),
                }));
            }
            (true, "div") => match stack.pop() {
                None => return roots, // closes .slides
                Some(Open::Div) => {}
                Some(Open::Section(s)) => panic!("</div> closes {:?}", s.tag),
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

/// The `layout-*` class tokens of a tag.
fn layout_classes(tag: &Tag) -> Vec<&str> {
    tag.classes()
        .into_iter()
        .filter(|c| c.starts_with("layout-"))
        .collect()
}

/// Checks the layout section's opening tag against its slide (§6, §7): the slide's id,
/// exactly one `layout-<name>` class, every slide class and every slide attribute. Extra
/// classes and attributes are allowed (a theme may add them).
fn check_section_tag(tag: &Tag, slide: &Slide, label: &str) {
    assert_eq!(tag.name, "section", "{label}");
    assert_eq!(tag.attr("id"), slide.id.as_deref(), "{label}: id");
    let layout = format!("layout-{}", slide.layout);
    assert_eq!(
        layout_classes(tag),
        [layout.as_str()],
        "{label}: layout class"
    );
    for class in &slide.classes {
        assert!(tag.has_class(class), "{label}: class {class}");
    }
    for (key, value) in &slide.attrs {
        assert_eq!(tag.attr(key), Some(value.as_str()), "{label}: {key}");
    }
}

/// Number of elements whose class list contains the token `cell` (not `cell-input`).
fn count_cell_elements(html: &str) -> usize {
    open_tags(html)
        .iter()
        .filter(|t| t.has_class("cell"))
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
    check_section_tag(&section.tag, slide, label);
    let inner = &html[section.inner.clone()];
    let tags = open_tags(inner);
    let notes: Vec<&Tag> = tags
        .iter()
        .filter(|t| t.name == "aside" && t.has_class("notes"))
        .collect();
    assert_eq!(notes.len(), usize::from(slide.notes.is_some()), "{label}");
    if let (Some(text), Some(aside)) = (&slide.notes, notes.first()) {
        assert!(inner_html(inner, aside).contains(text.as_str()), "{label}");
    }
    assert_eq!(
        count_cell_elements(inner),
        cell_blocks(slide),
        "{label}: cells"
    );
}

// ---------------------------------------------------------------------------------------
// Tests: rendering the fixture
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
        // The vertical-stack wrapper is not a slide itself: no layout class.
        assert!(
            layout_classes(&section.tag).is_empty(),
            "slides[{i}] wrapper"
        );
        assert_eq!(section.children.len(), 1 + slide.children.len());
        let members = std::iter::once(slide).chain(&slide.children);
        for (j, (sub, member)) in section.children.iter().zip(members).enumerate() {
            check_leaf(&html, sub, member, &format!("slides[{i}] stack member {j}"));
        }
    }

    let tags = open_tags(&html);
    let with_notes = deck
        .slides
        .iter()
        .flat_map(|s| std::iter::once(s).chain(&s.children))
        .filter(|s| s.notes.is_some())
        .count();
    let asides = tags
        .iter()
        .filter(|t| t.name == "aside" && t.has_class("notes"));
    assert_eq!(asides.count(), with_notes);
    assert!(tags.iter().any(|t| t.has_class("layout-title")));
    let cells: usize = deck.slides.iter().map(cell_blocks).sum();
    assert_eq!(cells, 2);
    assert_eq!(count_cell_elements(&html), cells);
}

/// Position, class and text of the title-layout meta lines inside `inner`.
fn title_meta(inner: &str) -> Vec<(usize, &'static str, String)> {
    let tags = open_tags(inner);
    ["subtitle", "author", "date"]
        .into_iter()
        .filter_map(|class| {
            let tag = tags.iter().find(|t| t.has_class(class))?;
            Some((tag.range.start, class, text_after(inner, tag)))
        })
        .collect()
}

#[test]
fn title_layout_shows_meta_under_the_heading() {
    let html = render_sample();
    let tags = open_tags(&html);
    let intro = find_tag(&tags, "#intro", |t| t.attr("id") == Some("intro"));
    assert!(intro.has_class("layout-title"));
    let section = inner_html(&html, intro);
    let heading = section
        .find("<h1>Rust for data people</h1>")
        .expect("heading");
    let meta = title_meta(section);
    let found: Vec<(&str, &str)> = meta.iter().map(|(_, c, t)| (*c, t.as_str())).collect();
    assert_eq!(
        found,
        [
            ("subtitle", "Fast tools without the footguns"),
            ("author", "J. Blaschke"),
            ("date", "2026-10-08"),
        ]
    );
    assert!(meta.windows(2).all(|w| w[0].0 < w[1].0), "meta order");
    assert!(meta[0].0 > heading, "meta must follow the heading");
    assert!(meta[2].0 < section.find("<aside").expect("notes"));
}

#[test]
fn title_meta_follows_the_first_block_and_falls_back_to_config() {
    let mut deck = sample_deck();
    (deck.meta.title, deck.meta.author, deck.meta.date) = (None, None, None);
    // notebook-mapping.md §6: a title slide with one block per markdown cell.
    let mut first = Slide::new();
    first.layout = "title".into();
    first.blocks = vec![
        Block::html("<h1>Analysis</h1>\n".into()),
        Block::html("<p>Some intro</p>\n".into()),
    ];
    let mut empty = Slide::new();
    empty.layout = "title".into();
    deck.slides = vec![first, empty];
    let mut context = context(&deck);
    context["config"]["deck"]["title"] = json!("Config title");
    context["config"]["deck"]["author"] = json!("Config Author");
    context["config"]["deck"]["date"] = json!("2026-01-02");
    let html = render_with(&context, UndefinedBehavior::Strict);

    let roots = slides_tree(&html);
    let expected = [
        ("subtitle", "Fast tools without the footguns"),
        ("author", "Config Author"),
        ("date", "2026-01-02"),
    ];
    let first = &html[roots[0].inner.clone()];
    let meta = title_meta(first);
    let found: Vec<(&str, &str)> = meta.iter().map(|(_, c, t)| (*c, t.as_str())).collect();
    assert_eq!(found, expected);
    let heading = first.find("<h1>Analysis</h1>").expect("heading");
    let intro = first.find("<p>Some intro</p>").expect("second block");
    assert!(
        heading < meta[0].0 && meta[2].0 < intro,
        "meta between the blocks"
    );
    // A title slide without blocks still shows the meta.
    let empty = &html[roots[1].inner.clone()];
    let found: Vec<String> = title_meta(empty).into_iter().map(|m| m.2).collect();
    assert_eq!(found, expected.map(|e| e.1));
    // <head> falls back to presenter.toml [deck] as well.
    let tags = open_tags(&html);
    let author = find_tag(&tags, "meta[name=author]", |t| {
        t.name == "meta" && t.attr("name") == Some("author")
    });
    assert_eq!(author.attr("content"), Some("Config Author"));
    let title = find_tag(&tags, "title", |t| t.name == "title");
    assert_eq!(text_after(&html, title), "Config title");
}

/// The `.cell` element with `data-cell-index="<index>"`.
fn cell_tag<'a>(tags: &'a [Tag], index: &str) -> &'a Tag {
    find_tag(tags, &format!(".cell[{index}]"), |t| {
        t.has_class("cell") && t.attr("data-cell-index") == Some(index)
    })
}

#[test]
fn cell_renders_prompt_escaped_input_and_every_output_kind() {
    let html = render_sample();
    let tags = open_tags(&html);
    let first = cell_tag(&tags, "4");
    for absent in ["fragment", "hide-input", "hide-output"] {
        assert!(!first.has_class(absent), "cell 4 has {absent}");
    }
    let body = inner_html(&html, first);
    let inner = open_tags(body);
    let input = inner_html(
        body,
        find_tag(&inner, ".cell-input", |t| t.has_class("cell-input")),
    );
    let prompt = find_tag(&inner, ".cell-prompt", |t| t.has_class("cell-prompt"));
    assert!(text_after(body, prompt).starts_with("In [3]"));
    let code = find_tag(&inner, "code.language-python", |t| {
        t.name == "code" && t.has_class("language-python") && t.attr("data-trim").is_some()
    });
    let source = inner_html(body, code);
    assert!(input.contains(source), "code inside .cell-input");
    assert!(
        !source.contains(r#"print("rows"#),
        "cell source is not escaped"
    );
    assert!(html_escape::decode_html_entities(source).contains(r#"print("rows:", len(df))"#));

    let outputs = inner_html(
        body,
        find_tag(&inner, ".cell-outputs", |t| t.has_class("cell-outputs")),
    );
    // One element per output kind, all inside .cell-outputs.
    let output = |classes: &[&str]| {
        let tag = find_tag(&inner, &classes.join("."), |t| {
            t.has_classes(classes) && t.has_class("output")
        });
        assert!(
            outputs.contains(&body[tag.range.clone()]),
            "{classes:?} inside .cell-outputs"
        );
        tag
    };
    let stdout = output(&["output-stream", "stdout"]);
    assert!(inner_html(body, stdout).contains("rows: 1000"));
    let stderr = output(&["output-stream", "stderr"]);
    let warning = "<span style='color:var(--yellow,#a50)'>warning</span>";
    assert!(
        inner_html(body, stderr).contains(warning),
        "stream html unescaped"
    );
    let display = output(&["output-display"]);
    assert_eq!(display.attr("data-mime"), Some("text/html"));
    assert!(inner_html(body, display).contains(r#"<table class="dataframe">"#));
    let unsupported = output(&["output-unsupported"]);
    assert!(text_after(body, unsupported).contains("application/vnd.jupyter.widget-view+json"));
    let error = output(&["output-error"]);
    assert!(inner_html(body, error).contains("<span style='color:var(--red,#a00)'>"));
}

#[test]
fn fragment_cell_flags_and_out_prompt() {
    let html = render_sample();
    let tags = open_tags(&html);
    // Fragment + hide-input; an Out [n] prompt only for execute results.
    let second = cell_tag(&tags, "5");
    assert!(second.has_classes(&["fragment", "hide-input"]) && !second.has_class("hide-output"));
    let out_prompts: Vec<String> = tags
        .iter()
        .filter(|t| t.has_class("cell-prompt"))
        .map(|t| text_after(&html, t))
        .filter(|text| text.starts_with("Out"))
        .collect();
    assert_eq!(out_prompts.len(), 1, "{out_prompts:?}");
    assert!(out_prompts[0].starts_with("Out [4]"));
    let image = find_tag(&tags, "img.output-image", |t| t.has_class("output-image"));
    assert_eq!(
        image.attr("src"),
        Some("../../tests/fixtures/md/images/ok.png")
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
    assert!(html.starts_with("<!DOCTYPE html>"));
    let tags = open_tags(&html);
    let root = find_tag(&tags, "html", |t| t.name == "html");
    assert_eq!(root.attr("lang"), Some("en"));
    let title = find_tag(&tags, "title", |t| t.name == "title");
    assert_eq!(text_after(&html, title), "Rust for data people");

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
fn theme_stylesheet_is_every_style_entry_in_order() {
    let html = render_sample();
    let start = html
        .find(r#"<style id="presenter-theme">"#)
        .expect("theme style");
    let css = theme_css();
    assert!(html[start..].contains(&css));
    for entry in style_entries() {
        let text = std::fs::read_to_string(theme_dir().join(&entry)).expect("entry");
        assert!(
            css.contains(&text),
            "{entry} missing from the theme stylesheet"
        );
    }
}

#[test]
fn strict_undefined_renders_identically() {
    let context = context(&sample_deck());
    let lenient = render_with(&context, UndefinedBehavior::Lenient);
    let strict = render_with(&context, UndefinedBehavior::Strict);
    assert_eq!(lenient, strict);
}

#[test]
fn serve_mode_renders_the_same_slides() {
    let build = context(&sample_deck());
    let mut serve = build.clone();
    serve["build"] = json!({ "mode": "serve", "standalone": false, "live_reload": true,
                             "version": env!("CARGO_PKG_VERSION"), "id": 3 });
    let build = render_with(&build, UndefinedBehavior::Strict);
    let serve = render_with(&serve, UndefinedBehavior::Strict);
    assert_eq!(slides_region(&serve), slides_region(&build));
}

#[test]
fn undecorated_slides_resolve_their_layout_by_name() {
    let mut deck = sample_deck();
    deck.slides[1].children[0].layout = "title".into();
    deck.slides[1].children[1].layout = "no-such-layout".into();
    let decorated = context(&deck);
    let mut bare = decorated.clone();
    undecorate(&mut bare["slides"]);
    let expected = render_with(&decorated, UndefinedBehavior::Strict);
    assert_eq!(render_with(&bare, UndefinedBehavior::Strict), expected);

    // The child really went through the title layout, the unknown one through the default.
    let roots = slides_tree(&expected);
    let child = &expected[roots[1].children[1].inner.clone()];
    assert_eq!(title_meta(child).len(), 3, "title meta on the child");
    let tags = open_tags(&expected);
    let unknown = find_tag(&tags, "#tooling", |t| t.attr("id") == Some("tooling"));
    assert_eq!(layout_classes(unknown), ["layout-no-such-layout"]);
    assert_eq!(tags.iter().filter(|t| t.has_class("subtitle")).count(), 2);

    // A decoration wins over the name: section markup for a slide named "title".
    let mut forced = decorated;
    forced["slides"][0]["layout_template"] = json!("layouts/section.html");
    let html = render_with(&forced, UndefinedBehavior::Strict);
    let intro = &html[slides_tree(&html)[0].inner.clone()];
    assert!(title_meta(intro).is_empty());
}

/// Every message in an error's source chain, outermost first.
fn error_chain(error: &minijinja::Error) -> Vec<String> {
    let mut messages = vec![error.to_string()];
    let mut source = std::error::Error::source(error);
    while let Some(inner) = source {
        messages.push(inner.to_string());
        source = inner.source();
    }
    messages
}

/// A decoration is used as it is: one naming a missing template fails the render (it is never
/// silently replaced by the by-name or the default layout), in each of slide.html's three
/// includes. A null decoration counts as none.
#[test]
fn a_decorated_missing_layout_is_an_error_in_every_include() {
    let decorated = context(&sample_deck());
    let expected = render_with(&decorated, UndefinedBehavior::Strict);
    let not_found =
        Regex::new(r#"^template not found: .*"layouts/missing\.html" \(in (slide\.html:\d+)\)$"#)
            .expect("regex");
    // slides[0] has no children; slides[1] is a stack: its own include, then its children's.
    let mut sites = BTreeMap::new();
    for pointer in [
        "/slides/0",
        "/slides/1",
        "/slides/1/children/0",
        "/slides/1/children/1",
    ] {
        let mut context = decorated.clone();
        let slide = context.pointer_mut(pointer).expect("fixture slide");
        slide["layout_template"] = json!("layouts/missing.html");
        for undefined in [UndefinedBehavior::Strict, UndefinedBehavior::Lenient] {
            let Err(error) = try_render(&context, undefined) else {
                panic!("{pointer}: rendered although its decorated layout is missing");
            };
            let chain = error_chain(&error);
            let site = chain
                .iter()
                .find_map(|message| Some(not_found.captures(message)?[1].to_string()))
                .unwrap_or_else(|| panic!("{pointer}: {chain:#?}"));
            sites.insert(pointer, site);
        }

        // null is "not decorated": resolved by name, byte-identical to the decorated render.
        let mut context = decorated.clone();
        let slide = context.pointer_mut(pointer).expect("fixture slide");
        slide["layout_template"] = serde_json::Value::Null;
        assert_eq!(
            render_with(&context, UndefinedBehavior::Strict),
            expected,
            "{pointer}"
        );
    }
    // Three different include sites failed: leaf slide, stack member, child.
    let distinct: BTreeSet<&String> = sites.values().collect();
    assert_eq!(distinct.len(), 3, "{sites:#?}");
    assert_eq!(sites["/slides/1/children/0"], sites["/slides/1/children/1"]);
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
    // Hand-built contexts may lack `layout_template`; slide.html resolves the layout by name.
    undecorate(&mut context["slides"]);
    let html = render_with(&context, UndefinedBehavior::Lenient);
    let tags = open_tags(&html);
    let section = find_tag(&tags, "section", |t| t.name == "section");
    assert_eq!(layout_classes(section), ["layout-title"]);
    assert!(!html.contains("subtitle") && !html.contains(r#"class="author""#));
    let title = find_tag(&tags, "title", |t| t.name == "title");
    assert_eq!(text_after(&html, title), "Presentation");
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
fn hidden_output_and_disabled_prompts() {
    let mut deck = sample_deck();
    let Block::Cell(cell) = &mut deck.slides[5].blocks[1] else {
        panic!("fixture: slides[5].blocks[1] is a cell");
    };
    cell.hide_output = true;
    let mut context = context(&deck);
    context["config"]["notebook"]["show_prompts"] = json!(false);
    let html = render_with(&context, UndefinedBehavior::Strict);
    let tags = open_tags(&html);
    let cell = cell_tag(&tags, "4");
    assert!(cell.has_class("hide-output") && !cell.has_class("hide-input"));
    // Hidden output is still emitted (CSS hides it, §6).
    assert!(inner_html(&html, cell).contains("rows: 1000"));
    assert!(
        !tags.iter().any(|t| t.has_class("cell-prompt")),
        "prompts shown"
    );
    assert!(!html.contains("In [") && !html.contains("Out ["));
}

#[test]
fn math_mode_controls_katex() {
    let markers = [
        "vendor/katex/katex.min.css",
        "vendor/katex/katex.min.js",
        "presenter/math.js",
    ];
    for (feature, mode, included) in [
        (true, "auto", true),
        (false, "auto", false),
        (false, "always", true),
        (true, "never", false),
    ] {
        let mut deck = sample_deck();
        deck.features.math = feature;
        let mut context = context(&deck);
        context["config"]["math"]["enabled"] = json!(mode);
        let html = render_with(&context, UndefinedBehavior::Strict);
        for marker in markers {
            assert_eq!(
                html.contains(marker),
                included,
                "{marker} with features.math = {feature}, math.enabled = {mode}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// Tests: the theme files themselves
// ---------------------------------------------------------------------------------------

#[test]
fn theme_toml_parses_and_declares_the_skeleton() {
    let manifest = manifest();
    assert_eq!(manifest["name"].as_str(), Some("default"));
    assert!(manifest.get("extends").is_none());
    let styles = &manifest["styles"];
    // theme.css first; later tasks may append entries (T33: styles/notebook.css).
    let entries = style_entries();
    assert_eq!(
        entries.first().map(String::as_str),
        Some("styles/theme.css")
    );
    for entry in &entries {
        assert!(theme_dir().join(entry).is_file(), "missing {entry}");
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
    let source = regex!(r"(?s)\{#.*?#\}").replace_all(source, "");
    regex!(r"(?s)\{\{.*?\}\}|\{%.*?%\}")
        .find_iter(&source)
        .map(|m| m.as_str().to_string())
        .collect()
}

/// String literals in either quote style.
fn string_literal() -> &'static Regex {
    regex!(r#""([^"]*)"|'([^']*)'"#)
}

/// Every complete template name written as a string literal in a tag (include, extends,
/// import targets and include fallbacks), in either quote style; `@default/` is dropped.
/// A literal joined with `~` is a fragment of a computed name and is skipped.
fn template_references(source: &str) -> Vec<String> {
    let literal = string_literal();
    let mut names = Vec::new();
    for region in tag_regions(source) {
        for m in literal.captures_iter(&region) {
            let text = m.get(1).or(m.get(2)).expect("literal body").as_str();
            let whole = m.get(0).expect("match");
            let joined = region[..whole.start()].trim_end().ends_with('~')
                || region[whole.end()..].trim_start().starts_with('~');
            if text.ends_with(".html") && !joined {
                names.push(text.strip_prefix("@default/").unwrap_or(text).to_string());
            }
        }
    }
    names
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
    let mut referenced = 0;
    for (name, source) in templates() {
        for target in template_references(&source) {
            referenced += 1;
            assert!(
                names.contains(&target),
                "{name} references missing {target}"
            );
        }
    }
    assert!(referenced >= 8, "scan found only {referenced} references");
    // Dynamic includes go through `layout_template` / `layouts/<layout>.html`: each
    // documented layout has its file and each layout file is documented.
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

/// Result of the static template scan.
#[derive(Debug, Default)]
struct Scan {
    /// One line per violation: `<template>: <what>`.
    problems: Vec<String>,
    /// §4 helpers that are called somewhere.
    helpers: BTreeSet<String>,
}

/// The receivers whose methods every template may call: minijinja's `loop`
/// (`loop.cycle(…)`, `loop.changed(…)`) and `self` (`self.title()` renders a block again).
/// A method of any other value may be a helper stored in it (`{% set d = {"h": helper} %}`
/// then `d.h(1)`, `namespace(h=helper)`, a macro's `kwargs`), so only the macros of an
/// imported module are allowed besides these ([`Callables::modules`]).
const METHOD_RECEIVERS: &str = "loop self";

/// The macros a template defines (`{% macro NAME(`).
fn macro_names(source: &str) -> BTreeSet<String> {
    let macro_def = regex!(r"^\{%[-+]?\s*macro\s+([A-Za-z_]\w*)\s*\(");
    tag_regions(source)
        .iter()
        .filter_map(|region| Some(macro_def.captures(region.trim())?[1].to_string()))
        .collect()
}

/// What one template may call besides minijinja's callables and the §4 helpers
/// ([`callables`]).
#[derive(Debug, Default)]
struct Callables {
    /// Names it may call: its macros, the names `{% from … import … %}` binds and the
    /// modules `{% import … as NAME %}` binds.
    names: BTreeSet<String>,
    /// Module name → the macros of the imported template, the methods it may call.
    modules: BTreeMap<String, BTreeSet<String>>,
}

impl Callables {
    /// Whether the method `method` of the receiver `path` (an attribute path such as `m` or
    /// `loop`) may be called.
    fn method_allowed(&self, path: &str, method: &str) -> bool {
        let root = path.split('.').next().unwrap_or(path);
        listed(METHOD_RECEIVERS, root) || self.modules.get(path).is_some_and(|m| m.contains(method))
    }
}

/// The callables one template defines or imports itself: its macros ([`macro_names`]), the
/// module bound by `{% import "<template>" as NAME %}`, and the names bound by
/// `{% from "<template>" import a, b as c %}` for which `<template>` (looked up by its literal
/// name among `templates`) defines that macro; a module's methods are the macros of its
/// template. Calling one of these is not calling a helper, but only in that template: a macro
/// of another template needs an import. Names bound by `set`, `with` or `for` are values,
/// never exempt; so is a name imported from a template that does not define it (minijinja
/// binds nothing then, and the call reaches a global helper) or from a computed template name.
fn callables(source: &str, templates: &[(String, String)]) -> Callables {
    let target =
        regex!(r#"^\{%[-+]?\s*(?:from|import)\s+(?:"([^"]*)"|'([^']*)')\s+(?:import|as)\b"#);
    let from_import = regex!(r"(?s)^\{%[-+]?\s*from\s.*?\bimport\s+(.*?)\s*[-+]?%\}$");
    let import_as = regex!(r"(?s)^\{%[-+]?\s*import\s.*?\bas\s+([A-Za-z_]\w*)\b");
    let context_marker = regex!(r"\s*\b(?:with|without)\s+context$");
    let mut found = Callables {
        names: macro_names(source),
        ..Callables::default()
    };
    for region in tag_regions(source) {
        let defined = target
            .captures(region.trim())
            .and_then(|c| {
                let name = c.get(1).or(c.get(2)).expect("literal body").as_str();
                let name = name.strip_prefix("@default/").unwrap_or(name);
                templates.iter().find(|(known, _)| known == name)
            })
            .map(|(_, imported)| macro_names(imported))
            .unwrap_or_default();
        // Template names are string literals; they must not look like syntax.
        let region = string_literal().replace_all(region.trim(), "\"\"");
        if let Some(c) = import_as.captures(&region) {
            found.names.insert(c[1].to_string());
            found.modules.insert(c[1].to_string(), defined);
            continue;
        }
        let Some(c) = from_import.captures(&region) else {
            continue;
        };
        for item in context_marker.replace(&c[1], "").split(',') {
            // `name` or `name as alias`: the alias, if any, is what the template can call.
            let mut words = item.split_whitespace();
            let (Some(imported), alias) = (words.next(), words.last()) else {
                continue;
            };
            if defined.contains(imported) {
                found.names.insert(alias.unwrap_or(imported).to_string());
            }
        }
    }
    found
}

/// Checks one tag region: `|safe` only on IR HTML, filters from the allowlist, every call
/// either a §4 helper, one of minijinja's callables or one of `own.names`, and every method
/// call allowed by [`Callables::method_allowed`].
fn scan_region(region: &str, own: &Callables, scan: &mut Scan, name: &str) {
    let statement = region.trim_start().starts_with("{%");
    if regex!(r"^\{%[-+]?\s*(?:autoescape\b|filter\b.*\bsafe\b)").is_match(region.trim()) {
        scan.problems
            .push(format!("{name}: `{region}` turns escaping off"));
    }
    let region = string_literal().replace_all(region, "\"\"");
    let region = regex!(r"\{\{[-+]?|[-+]?\}\}|\{%[-+]?|[-+]?%\}").replace_all(&region, " ");
    for m in regex!(r"(\S+)\s*\|\s*safe\b").captures_iter(&region) {
        if !SAFE_EXPRESSIONS.contains(&&m[1]) {
            scan.problems.push(format!("{name}: `{}|safe`", &m[1]));
        }
    }
    for m in regex!(r"\|\s*([A-Za-z_]\w*)").captures_iter(&region) {
        if !listed(ALLOWED_FILTERS, &m[1]) {
            scan.problems.push(format!("{name}: filter `{}`", &m[1]));
        }
    }
    let region = strip_filters_and_tests(&region);
    // Every name directly followed by `(` (also nested: `f(g(1))`); `.name(` is a method.
    for m in regex!(r"\b([A-Za-z_]\w*)\s*\(").captures_iter(&region) {
        let found = m.get(1).expect("group 1");
        let (ident, before) = (found.as_str(), &region[..found.start()]);
        if let Some(receiver) = before.trim_end().strip_suffix('.') {
            match receiver_path(receiver) {
                Some(path) if own.method_allowed(&path, ident) => {}
                Some(path) => scan
                    .problems
                    .push(format!("{name}: method `{ident}` of `{path}`")),
                None => scan
                    .problems
                    .push(format!("{name}: method `{ident}` of a computed value")),
            }
        } else if is_keyword_use(ident, before, statement) {
            continue;
        } else if SPEC_HELPERS.contains(&ident) {
            scan.helpers.insert(ident.to_string());
        } else if !(own.names.contains(ident) || listed(MINIJINJA_CALLABLES, ident)) {
            scan.problems.push(format!("{name}: helper `{ident}`"));
        }
    }
    for callee in computed_callees(&region, statement, own) {
        scan.problems
            .push(format!("{name}: call of the computed callee `{callee}`"));
    }
}

/// Whether `word` is one of the space-separated words of `list`.
fn listed(list: &str, word: &str) -> bool {
    list.split_whitespace().any(|k| k == word)
}

/// The trailing name or number of `text` (empty when `text` ends in another character).
fn last_word(text: &str) -> &str {
    text.rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .next()
        .unwrap_or_default()
}

/// True when `word`, directly followed by `(` in a tag region whose text up to `word` is
/// `before` (delimiters and string literals blanked), is minijinja syntax rather than a call:
/// a [`STATEMENT_KEYWORDS`] word opening a `{% … %}` tag (`statement`), `not`, or an
/// [`OPERATOR_KEYWORDS`] word after an operand ([`after_operand`]). `in` is also an operator
/// after a binary `not` (`x not in (…)`), but not after a unary one: `{{ not in(1) }}` calls
/// a global named `in`.
fn is_keyword_use(word: &str, before: &str, statement: bool) -> bool {
    let before = before.trim_end();
    if before.is_empty() {
        return (statement && listed(STATEMENT_KEYWORDS, word)) || word == "not";
    }
    if word == "not" {
        return true;
    }
    if !listed(OPERATOR_KEYWORDS, word) {
        return false;
    }
    let binary_not = word == "in"
        && last_word(before) == "not"
        && after_operand(&before[..before.len() - "not".len()], statement);
    binary_not || after_operand(before, statement)
}

/// True when the tag-region text `before` ends with an operand: a closing bracket, a string
/// literal, or a name or number that is not a keyword (an [`OPERATOR_KEYWORDS`] word, `not`,
/// `is`, or the first word of a `{% … %}` tag, which is its keyword).
fn after_operand(before: &str, statement: bool) -> bool {
    let before = before.trim_end();
    let word = last_word(before);
    match before.chars().last() {
        Some(')' | ']' | '}' | '"' | '\'') => true,
        Some(_) if !word.is_empty() => {
            let keyword = listed(OPERATOR_KEYWORDS, word)
                || matches!(word, "not" | "is")
                || (statement && before.trim_start() == word);
            !keyword
        }
        _ => false,
    }
}

/// `region` (delimiters and string literals blanked) with the name of every filter (`|name`)
/// and test (`is [not] name`) blanked and the parentheses around its arguments, if any,
/// turned into brackets: `x|default(range(3))` becomes `x        [range(3)]`. So the
/// arguments are still scanned (`x|default(helper(1))` calls `helper`), but they never read
/// as a call of `x`. Byte offsets are kept.
fn strip_filters_and_tests(region: &str) -> String {
    let mut bytes = region.as_bytes().to_vec();
    for m in regex!(r"\|\s*[A-Za-z_]\w*|\bis\s+(?:not\s+)?[A-Za-z_]\w*").find_iter(region) {
        bytes[m.range()].fill(b' ');
        let rest = &region[m.end()..];
        let open = m.end() + (rest.len() - rest.trim_start().len());
        if region[open..].starts_with('(')
            && let Some(close) = matching_close(region, open)
        {
            bytes[open] = b'[';
            bytes[close] = b']';
        }
    }
    String::from_utf8(bytes).expect("only ASCII bytes were replaced")
}

/// The receiver of a method call, given the tag-region text before its `.`: its attribute
/// path (`m`, `loop`, `ns.a`; spaces removed) when the receiver is a plain one, `None` when it
/// is computed (a subscript, a call's result, a group or a literal).
fn receiver_path(before: &str) -> Option<String> {
    let path = regex!(r"[A-Za-z_]\w*(?:\s*\.\s*[A-Za-z_]\w*)*\s*$").find(before)?;
    let prefix = before[..path.start()].trim_end();
    if matches!(
        prefix.chars().last(),
        Some('.' | ')' | ']' | '}' | '"' | '\'')
    ) {
        return None;
    }
    Some(path.as_str().split_whitespace().collect())
}

/// The callees of calls on a computed value (`(…)(…)`, `[…](…)`, `f(…)(…)`) in a tag region
/// with delimiters and string literals blanked. The scan cannot tell what such a call
/// reaches (`(helper)(1)` and `[helper][0](1)` call the global `helper`), so each one is a
/// problem, except a parenthesized attribute path such as `{% do (m.fmt)(1) %}`, a method
/// call like `m.fmt(1)`, where `own` allows that method.
fn computed_callees(region: &str, statement: bool, own: &Callables) -> Vec<String> {
    let path = regex!(r"^\(\s*([A-Za-z_]\w*(?:\s*\.\s*[A-Za-z_]\w*)+)\s*\)$");
    let mut callees = Vec::new();
    for m in regex!(r"[)\]]\s*\(").find_iter(region) {
        let close = m.start();
        let open = matching_open(region, close).unwrap_or(close);
        let callee = &region[open..=close];
        // A `(` after an operand opens a call's arguments (`f(ns.a)(1)`), not a group.
        let before = region[..open].trim_end();
        let word = last_word(before);
        let group = if word.is_empty() {
            !matches!(before.chars().last(), Some(')' | ']' | '}' | '"' | '\''))
        } else {
            is_keyword_use(word, &before[..before.len() - word.len()], statement)
        };
        let method = path.captures(callee).map(|c| {
            let path: String = c[1].split_whitespace().collect();
            let (receiver, method) = path.rsplit_once('.').expect("a path with a dot");
            own.method_allowed(receiver, method)
        });
        if !(group && method == Some(true)) {
            callees.push(callee.to_string());
        }
    }
    callees
}

/// The index of the opening bracket matched by the closing one at byte index `close`.
fn matching_open(text: &str, close: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in text[..=close].char_indices().rev() {
        match c {
            ')' | ']' | '}' => depth += 1,
            '(' | '[' | '{' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The index of the closing bracket that matches the opening one at byte index `open`.
fn matching_close(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in text[open..].char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The static scan behind "no helper other than those of §4 is used": every tag of every
/// template, including branches the fixture never executes. Each template may call minijinja's
/// own callables and the macros it defines or imports ([`callables`]).
///
/// Known gap: the scan follows calls, not values. A helper passed around without being
/// called where it is named is only caught where it is called: by name (`f(1)` after
/// `{% set f = helper %}`), as a computed callee (`x[0](1)`), or as a method of anything but
/// `loop`, `self` or an imported module ([`METHOD_RECEIVERS`]). A module's top-level `set`
/// variables are not methods it may call, but a module's macro could itself hold one.
fn scan_templates(templates: &[(String, String)]) -> Scan {
    let mut scan = Scan::default();
    for (name, source) in templates {
        let own = callables(source, templates);
        for region in tag_regions(source) {
            scan_region(&region, &own, &mut scan, name);
        }
    }
    scan
}

#[test]
fn templates_use_only_spec_helpers_and_filters() {
    let scan = scan_templates(&templates());
    assert!(scan.problems.is_empty(), "{:#?}", scan.problems);
    // The document shell needs these; `asset` is optional (e.g. a logo in a layout).
    for needed in [
        "live_reload",
        "reveal_init",
        "script",
        "style",
        "theme_style",
    ] {
        assert!(scan.helpers.contains(needed), "{needed} is never called");
    }
}

// The static scans on synthetic templates: ordinary minijinja passes, bypasses do not.

/// [`scan_templates`] of `t.html` = `source` next to `m.html`, which defines the macros `fmt`
/// and `other`.
fn scan_t(source: &str) -> Scan {
    let macros = "{% macro fmt(x) %}{{ x }}{% endmacro %}{% macro other() %}{% endmacro %}";
    scan_templates(&[
        ("m.html".to_string(), macros.to_string()),
        ("t.html".to_string(), source.to_string()),
    ])
}

/// Each source is valid minijinja and passes the scan.
fn assert_accepted(sources: &[&str]) {
    for source in sources {
        let result = scan_t(source);
        assert!(
            result.problems.is_empty(),
            "{source}: {:?}",
            result.problems
        );
        assert_parses(source);
    }
}

/// Each source is valid minijinja and fails the scan.
fn assert_rejected(sources: &[&str]) {
    for source in sources {
        assert!(
            !scan_t(source).problems.is_empty(),
            "{source} passed the scan"
        );
        assert_parses(source);
    }
}

#[test]
fn scan_accepts_ordinary_minijinja() {
    assert_accepted(&[
        r#"{% macro prompt(label, n) %}{{ label }} [{{ n }}]{% endmacro %}{{ prompt("In", 1) }}"#,
        r#"{% from "m.html" import fmt as f, other %}{{ f(1) }}{{ other() }}"#,
        r#"{%- from "m.html" import fmt, other as o with context -%}{{ fmt(1) }}{{ o() }}"#,
        r#"{% from '@default/m.html' import fmt %}{{ fmt(1) }}"#,
        r#"{% import "m.html" as m %}{{ m(1) }}{{ m.fmt(1) }}{{ m . other() }}"#,
        r#"{% macro row(x) %}{{ x }}{% endmacro %}{% call row(1) %}{{ caller() }}{% endcall %}"#,
        r#"{%- set mimes = out.mimes %}{{ mimes|join(", ") }}{{ out.mimes|join(', ') }}"#,
        "{{ range(3)|join }}{% set ns = namespace(n=1) %}{{ dict(a=1)|tojson }}",
        "{% for a, b in x|items %}{{ loop.cycle(1, 2) }}{{ loop . changed(a) }}{% endfor %}",
        "{{ x is divisibleby(3) }}{{ caller() }}{{ super() }}{{ style(\"a.css\") }}",
        "{{ block.html|safe }}{{ slide.notes | safe }}{{-out.html|safe-}}",
        // Calls inside filter and test arguments.
        "{{ x|default(range(3)) }}{{ x|default(dict(a=1)) }}{{ x|default((y)) }}",
        "{{ x|default (range(2)|first) in (1, 2) }}{{ x is divisibleby((3)) }}",
        "{{ self.title() }}{% if x is sameas(range(1)|first) and (y) %}{% endif %}",
        // Keywords directly before `(` where minijinja reads them as keywords.
        "{% with (a, b) = (1, 2) %}{{ a }}-{{ b }}{% endwith %}",
        "{%- with x = range(3), (y, z) = (1, 2) -%}{{ x|join }}{% endwith %}",
        r#"{% include "m.html" with context %}{% include ["m.html"] ignore missing without context %}"#,
        "{% for (a, b) in x|items if (a) %}{{ a }}{% endfor %}{% set (c, d) = (1, 2) %}",
        "{% if (a) and (b) or (c) %}{% elif not (d) %}{% endif %}{{ x if (y) else (z) }}",
        r#"{{ x in (1, 2) }}{{ x not in ("a", 'b') }}{{ 1 in (1,) }}{{ x|length in [1] }}"#,
        "{{ not(x) }}{{ (a) and not (b) }}{{ x is defined and (y) }}{{ [x][0] in (y) }}",
        "{% macro row(x) %}{{ caller(x) }}{% endmacro %}{% call(v) row(1) %}{{ v }}{% endcall %}",
        r#"{% include ("m" ~ ".html") %}{{ loop . cycle(1) }}"#,
        r#"{% import "m.html" as m %}{{ (m.fmt)(1) }}{{ not ( m . fmt )(2) }}"#,
        "{% if x and (loop.cycle)(3) %}{% endif %}{% do (self.title)() %}",
        "{{ x not in(1) }}{{ (a) not in(b) }}{% for x in(y) %}{% endfor %}",
        // minijinja's `+` whitespace marker, like `-`.
        "{%+ with (a, b) = (1, 2) %}{{+ a +}}{%- endwith +%}",
        "{%+ if (a) %}x{% endif %}{%+ for (a, b) in x +%}{% endfor %}{%+ set (c, d) = (1, 2) %}",
        r#"{%+ include ("m.html") %}{%+ macro g(x) %}{% endmacro %}{{ g(1) }}"#,
        r#"{%+ from "m.html" import fmt +%}{{ fmt(1) }}{%+ import "m.html" as m %}{{ m(1) }}"#,
    ]);
    // minijinja binds the tuple target of `with (…)`.
    let mut env = Environment::new();
    env.add_template(
        "t.html",
        "{% with (a, b) = (1, 2) %}{{ a }}-{{ b }}{% endwith %}",
    )
    .expect("parses");
    assert_eq!(
        env.get_template("t.html").unwrap().render(()).unwrap(),
        "1-2"
    );
}

#[test]
fn scan_rejects_calls_of_template_values() {
    assert_rejected(&[
        "{{ helper(1) }}",
        // Values bound by set/with/for are not callables (a dead `date()` call is a helper).
        "{% set date = 1 %}{{ date(2) }}",
        "{% set title_meta %}x{% endset %}{{ title_meta() }}",
        "{% with name = slide.id %}{{ name() }}{% endwith %}",
        "{% for path in theme.scripts %}{{ path(\"x\") }}{% endfor %}",
        "{% for a, b in x|items %}{{ b() }}{% endfor %}",
        "{% if false %}{{ date(meta.date, \"%Y\") }}{% endif %}",
        // ... also when the value's name is a keyword that never precedes `(`.
        "{% for block in slide.blocks %}{{ block(1) }}{% endfor %}",
        "{% set filter = 1 %}{{ filter(2) }}",
        "{% set missing = 1 %}{{ missing() }}",
        "{% with recursive = 1 %}{{ recursive() }}{% endwith %}",
        "{% for ignore in x %}{{ ignore() }}{% endfor %}",
        "{% set macro = 1 %}{{ macro() }}",
        // ... and when it is a keyword that minijinja does not read as one there.
        "{% set with = 1 %}{{ with() }}",
        "{% with with = 1 %}{{ with(2) }}{% endwith %}",
        "{% for call in x %}{{ call() }}{% endfor %}",
        "{% set include = 1 %}{{ include(2) }}",
        "{% for (a, b) in x %}{{ for(1) }}{% endfor %}",
        "{% set and = 1 %}{{ and(2) }}{{ x if (and(3)) }}",
        "{% if x and and(1) %}{% endif %}",
        "{% if and(1) %}{% endif %}",
        "{% include in(1) %}",
        "{{ x if y else (or(1)) }}",
        // `in` after a unary `not` is an operand.
        "{{ not in(1) }}",
        "{% if x and not in(1) %}{% endif %}",
        "{% if not in(1) %}{% endif %}",
        // Nested calls are found too, also in filter and test arguments.
        r#"{{ style(helper("x")) }}"#,
        "{{ x|default(helper(1)) }}",
        "{{ x|default((helper)(1)) }}",
        "{{ x is sameas(helper(1)) }}",
        "{{+ helper(1) +}}",
    ]);
}

#[test]
fn scan_rejects_computed_callees_and_methods() {
    assert_rejected(&[
        // A computed callee may be anything.
        "{{ (helper)(1) }}",
        "{% do (helper)(1) %}",
        "{{ [helper][0](1) }}",
        "{% set x = [helper] %}{{ x[0](1) }}",
        "{{ (style)(1) }}",
        r#"{% from "m.html" import fmt %}{{ fmt(1)(2) }}{{ fmt (ns.a)(3) }}"#,
        "{{ (ns.a if x else helper)(1) }}",
        // So may a method of anything but loop, self or an imported module's macro.
        r#"{% set d = {"h": helper} %}{{ d.h(1) }}"#,
        "{% set ns = namespace(h=helper) %}{{ ns.h(1) }}",
        "{% set ns = namespace() %}{% set ns.h = helper %}{{ ns . h(1) }}",
        "{% set ns = namespace(a=[]) %}{% do (ns.a.append)(1) %}",
        "{% macro m() %}{{ kwargs.h(1) }}{% endmacro %}{{ m(h=helper) }}",
        r#"{{ ({"h": helper}).h(1) }}{{ x[0].h(1) }}"#,
        "{{ (x.y)(1) }}",
        "{{ not ( ns . a )(2) }}",
        "{% if x and (ns.a.f)(3) %}{% endif %}",
        r#"{% import "m.html" as m %}{{ m.date(1) }}"#,
        r#"{% import ("m" ~ ".html") as m %}{{ m.fmt(1) }}"#,
        r#"{% import "other.html" as m %}{{ m.fmt(1) }}"#,
    ]);
}

#[test]
fn scan_rejects_imports_that_bind_nothing_and_escaping_bypasses() {
    assert_rejected(&[
        // An import binds its alias, not the imported name.
        r#"{% from "m.html" import fmt as f %}{{ fmt(1) }}"#,
        r#"{% import "m.html" as m %}{{ fmt(1) }}"#,
        // An import binds only macros its template defines; nothing from an unknown or
        // computed template name.
        r#"{% from "m.html" import date %}{{ date(1) }}"#,
        r#"{% from "m.html" import date as fmt %}{{ fmt(1) }}"#,
        r#"{% from "other.html" import fmt %}{{ fmt(1) }}"#,
        r#"{% from "m" ~ ".html" import fmt %}{{ fmt(1) }}"#,
        "{{ x|unknown_filter }}",
        "{{ (out.name ~ out.html)|safe }}",
        "{{ cell.source|safe }}",
        "{{ block.html|e|safe }}",
        "{% autoescape false %}{{ cell.source }}{% endautoescape %}",
        "{% filter safe %}{{ cell.source }}{% endfilter %}",
        // A `+` whitespace marker hides nothing.
        "{%+ autoescape false %}{{ cell.source }}{% endautoescape %}",
    ]);
}

#[test]
fn scan_reports_each_kind_of_problem() {
    for (bad, problem) in [
        ("{{ not in(1) }}", "t.html: helper `in`"),
        ("{%+ if in(1) %}{% endif %}", "t.html: helper `in`"),
        ("{{ x|default(helper(1)) }}", "t.html: helper `helper`"),
        (
            "{% do (helper)(1) %}",
            "t.html: call of the computed callee `(helper)`",
        ),
        (
            "{{ [helper][0](1) }}",
            "t.html: call of the computed callee `[0]`",
        ),
        (
            "{% set ns = namespace(a=[]) %}{% do (ns.a.append)(1) %}",
            "t.html: call of the computed callee `(ns.a.append)`",
        ),
        (
            r#"{% set d = {"h": helper} %}{{ d . h(1) }}"#,
            "t.html: method `h` of `d`",
        ),
        (
            r#"{% import "m.html" as m %}{{ m.date(1) }}"#,
            "t.html: method `date` of `m`",
        ),
        (
            r#"{{ ({"h": helper}).h(1) }}"#,
            "t.html: method `h` of a computed value",
        ),
        (
            "{%+ autoescape false %}{% endautoescape %}",
            "t.html: `{%+ autoescape false %}` turns escaping off",
        ),
    ] {
        assert_eq!(scan_t(bad).problems, [problem], "{bad}");
    }
}

#[test]
fn scan_scopes_callables_to_their_template() {
    let two = |caller: &str| {
        scan_templates(&[
            (
                "m.html".to_string(),
                "{% macro fmt(x) %}{{ x }}{% endmacro %}{{ fmt(0) }}".to_string(),
            ),
            ("t.html".to_string(), caller.to_string()),
        ])
        .problems
    };
    assert_eq!(two("{{ fmt(1) }}"), ["t.html: helper `fmt`"]);
    assert_eq!(
        two(r#"{% from "m.html" import fmt %}{{ fmt(1) }}"#),
        Vec::<String>::new()
    );
    // A dead branch in a real template calling names that other templates bind with
    // set/for (title.html's `date`, scripts.html's `path`, default.html's `name`, title.html's
    // and default.html's `block`), and a name imported from a template without that macro.
    let mut real = templates();
    let (_, cell) = real
        .iter_mut()
        .find(|(name, _)| name == "cell.html")
        .expect("cell.html");
    cell.push_str(concat!(
        r#"{% from "block.html" import title_meta %}{% if false %}{{ date(meta.date, "%Y") }}"#,
        r#"{{ path("x") }}{{ name() }}{{ block() }}{{ title_meta() }}{% endif %}"#,
    ));
    assert_eq!(
        scan_templates(&real).problems,
        [
            "cell.html: helper `date`",
            "cell.html: helper `path`",
            "cell.html: helper `name`",
            "cell.html: helper `block`",
            "cell.html: helper `title_meta`",
        ]
    );
}

#[test]
fn template_references_are_complete_literal_names() {
    let refs = template_references(
        r#"{% include 'partials/a.html' %}{% include ["b.html", "@default/c.html"] %}
           {% include "layouts/" ~ slide.layout ~ ".html" %}{# {% include "d.html" %} #}"#,
    );
    assert_eq!(refs, ["partials/a.html", "b.html", "c.html"]);
}

/// `source` is valid minijinja 2.24 syntax (with this crate's features), so the scan alone
/// decides whether it passes.
fn assert_parses(source: &str) {
    let mut env = Environment::new();
    if let Err(err) = env.add_template("t.html", source) {
        panic!("{source} does not parse: {err}");
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
