//! Golden IR snapshots of every Markdown fixture in `tests/fixtures/md/` (T12), plus the
//! fixture-specific acceptance checks of `docs/tasks/T12-markdown-renderer.md`.
//!
//! Snapshots are the `Deck` as JSON with `source.path`/`source.base_dir` redacted to `[path]`
//! and every asset id (blake3 of an absolute path, so machine-dependent) replaced by a stable
//! 12-hex label (`000000000001`, … in order of first appearance). The labelled deck still
//! satisfies `Deck::invariant_violations() == []`, which every test checks.
//!
//! Every fixture is snapshotted: the ones in [`DEDICATED`] by their own test below (which also
//! checks the slide shape and the fixture's behaviours), any other `*.md` (e.g. T43's
//! `headings-mode.md`) by [`other_fixtures_are_snapshotted`] with the default config. A
//! fixture that needs other settings sets them in its front matter (`slides: {split:
//! headings}`), which `parse` merges into the config; [`check_fixture_with`] takes a config
//! for a dedicated test that cannot.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use presenter::assets::registry::AssetRegistry;
use presenter::config::Config;
use presenter::error::{Diagnostics, Error, Level};
use presenter::ir::{Block, Deck, Parsed, Slide, SourceInfo, SourceKind, SourceSpan};
use presenter::markdown::render::{
    RenderCtx, RenderMode, missing_asset_path, render_fragment, render_fragment_with_lift,
};
use presenter::markdown::{comrak_options, parse};
use regex::Regex;
use serde_json::Value;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/md")
}

fn source(name: &str) -> SourceInfo {
    let dir = fixture_dir();
    SourceInfo {
        path: dir.join(format!("{name}.md")),
        kind: SourceKind::Markdown,
        base_dir: dir,
    }
}

fn parse_fixture(name: &str, config: &Config) -> Parsed {
    let source = source(name);
    let text = std::fs::read_to_string(&source.path).unwrap();
    parse(&source, &text, config).unwrap()
}

/// The stems of `tests/fixtures/md/*.md`, sorted.
fn fixture_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fixture_dir())
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            let stem = path.file_stem()?.to_string_lossy().into_owned();
            (path.extension()? == "md").then_some(stem)
        })
        .collect();
    names.sort();
    names
}

fn parse_text(text: &str) -> presenter::Result<Parsed> {
    parse(&source("inline"), text, &Config::default())
}

/// The deck as JSON with asset ids replaced by stable labels (module docs).
fn stable_json(deck: &Deck) -> Value {
    let ids: Vec<&str> = deck.assets.iter().map(|asset| asset.id.as_str()).collect();
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    let mut value = serde_json::to_value(deck).unwrap();
    relabel(&mut value, &ids, &mut labels);
    if let Some(assets) = value.get_mut("assets").and_then(Value::as_array_mut) {
        assets.sort_by_key(|asset| asset["id"].as_str().map(str::to_string));
    }
    value
}

fn relabel(value: &mut Value, ids: &[&str], labels: &mut BTreeMap<String, String>) {
    match value {
        Value::String(text) => {
            let hex = Regex::new("[0-9a-f]{12}").unwrap();
            let replaced = hex.replace_all(text, |caps: &regex::Captures<'_>| {
                let id = &caps[0];
                if !ids.contains(&id) {
                    return id.to_string();
                }
                let next = format!("{:012x}", labels.len() + 1);
                labels.entry(id.to_string()).or_insert(next).clone()
            });
            *text = replaced.into_owned();
        }
        Value::Array(items) => items.iter_mut().for_each(|v| relabel(v, ids, labels)),
        Value::Object(map) => map.values_mut().for_each(|v| relabel(v, ids, labels)),
        _ => {}
    }
}

/// Number of vertical children of each horizontal slide.
fn shape(deck: &Deck) -> Vec<usize> {
    deck.slides
        .iter()
        .map(|slide| slide.children.len())
        .collect()
}

fn html(slide: &Slide) -> &str {
    match slide.blocks.as_slice() {
        [] => "",
        [Block::Html { html }] => html,
        other => panic!("expected one Html block, got {other:?}"),
    }
}

/// Snapshot + invariants + shape, shared by every fixture test.
fn check_fixture(name: &str, expected_shape: &[usize]) -> Parsed {
    check_fixture_with(name, &Config::default(), Some(expected_shape))
}

/// [`check_fixture`] with a config; `expected_shape: None` skips the shape check.
fn check_fixture_with(name: &str, config: &Config, expected_shape: Option<&[usize]>) -> Parsed {
    let parsed = parse_fixture(name, config);
    let deck = &parsed.deck;
    assert_eq!(deck.invariant_violations(), Vec::<String>::new(), "{name}");
    if let Some(expected) = expected_shape {
        assert_eq!(shape(deck), expected, "{name}: shape");
    }
    let json = stable_json(deck);
    let relabelled: Deck = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(
        relabelled.invariant_violations(),
        Vec::<String>::new(),
        "{name}"
    );
    insta::assert_json_snapshot!(name, json, {
        ".source.path" => "[path]",
        ".source.base_dir" => "[path]",
    });
    for diagnostic in parsed.diagnostics.iter() {
        assert_eq!(diagnostic.path.as_deref(), Some(deck.source.path.as_path()));
        assert!(diagnostic.span.is_some(), "{diagnostic:#}");
    }
    // reveal.js would turn a slide containing a <section> into a vertical stack
    for slide in deck
        .slides
        .iter()
        .flat_map(|s| std::iter::once(s).chain(&s.children))
    {
        let notes = slide.notes.as_deref().unwrap_or_default();
        assert!(!html(slide).contains("<section") && !notes.contains("<section"));
    }
    parsed
}

fn codes(diagnostics: &Diagnostics) -> Vec<(&'static str, usize)> {
    diagnostics
        .iter()
        .map(|d| (d.code, d.span.as_ref().map_or(0, |s| s.start_line)))
        .collect()
}

/// Fixtures with a dedicated test below.
const DEDICATED: [&str; 8] = [
    "attributes",
    "code",
    "directives",
    "images",
    "math",
    "minimal",
    "notes",
    "separators",
];

#[test]
fn dedicated_fixtures_exist() {
    let names = fixture_names();
    for name in DEDICATED {
        assert!(names.iter().any(|n| n == name), "{name}.md is missing");
    }
}

/// Snapshot, invariants and diagnostics of every fixture without a dedicated test (module
/// docs), so new fixtures need no edit here.
#[test]
fn other_fixtures_are_snapshotted() {
    for name in fixture_names() {
        if !DEDICATED.contains(&name.as_str()) {
            check_fixture_with(&name, &Config::default(), None);
        }
    }
}

#[test]
fn minimal_md() {
    let parsed = check_fixture("minimal", &[0]);
    let slide = &parsed.deck.slides[0];
    assert_eq!(slide.title.as_deref(), Some("Hello, presenter"));
    assert!(
        html(slide).starts_with("<!--\nminimal.md:"),
        "comment passes through"
    );
    assert_eq!(parsed.deck.features, Default::default());
    assert!(parsed.diagnostics.is_empty());
}

#[test]
fn separators_md() {
    let parsed = check_fixture("separators", &[0, 2, 0, 0, 0, 0, 1]);
    let deck = &parsed.deck;
    assert_eq!(deck.meta.title.as_deref(), Some("Separators"));
    assert!(deck.slides[3].blocks.is_empty() && deck.slides[6].blocks.is_empty());
    assert!(deck.features.code, "the fenced block on slide 3");
    assert_eq!(
        codes(&parsed.diagnostics),
        [("empty-slide", 80), ("empty-slide", 93)]
    );
}

#[test]
fn attributes_md() {
    let parsed = check_fixture("attributes", &[0, 0, 1, 0, 0, 0, 0, 0]);
    let slides = &parsed.deck.slides;
    assert_eq!(slides[1].layout, "title");
    assert_eq!(slides[1].attrs["data-background-color"], "#111");
    // missing background file: warning, value kept as written
    assert_eq!(slides[2].attrs["data-background-image"], "bg.png");
    assert_eq!(slides[2].children[0].attrs["data-auto-animate"], "");
    assert_eq!(slides[3].id.as_deref(), Some("intro"));
    assert!(
        html(&slides[3]).starts_with("<h1>T</h1>"),
        "lifted heading is clean"
    );
    // §3.3: attributes on a heading that is not first, a link and inline code
    let last = html(&slides[7]);
    assert!(last.contains(r#"<h3 id="kept" class="muted">Not the first node</h3>"#));
    assert!(
        last.contains(r#"<a href="https://example.org" class="button" target="_blank">links</a>"#)
    );
    assert!(last.contains(r#"<code class="python">x = 1</code>"#));
    assert_eq!(
        codes(&parsed.diagnostics),
        [("missing-asset", 37), ("unknown-slide-attr", 49)]
    );
    assert!(parsed.diagnostics.iter().all(|d| d.level == Level::Warning));
}

#[test]
fn directives_md() {
    let parsed = check_fixture("directives", &[0, 0, 0, 0, 0]);
    let slides = &parsed.deck.slides;
    // notes removed from the body and present in Slide.notes
    let first = html(&slides[0]);
    assert!(!first.contains("Notes from a"), "{first}");
    assert!(first.contains("Body text before and after"));
    assert_eq!(
        slides[0].notes.as_deref(),
        Some(
            "<p>Notes from a <code>:::notes</code> block are removed from the body and become <em>Slide.notes</em>.</p>\n"
        )
    );
    assert!(slides[1..].iter().all(|slide| slide.notes.is_none()));
    assert!(parsed.deck.features.notes);
    // fragments
    let fragments = html(&slides[1]);
    assert!(fragments.contains("<div class=\"fragment\">\n<p>Appears on click.</p>\n</div>"));
    assert!(fragments.contains(r#"<div class="fragment fade-up" data-fragment-index="2">"#));
    assert!(fragments.contains(r#"<div class="fragment highlight-red" data-fragment-index="1">"#));
    // columns with nested column directives
    assert_eq!(
        html(&slides[2]),
        "<h2>Columns</h2>\n\
         <div class=\"columns\" style=\"grid-template-columns: 1fr 2fr\">\n\
         <div class=\"column\">\n<p>Left column.</p>\n</div>\n\
         <div class=\"column\">\n<p>Right column, twice as wide.</p>\n</div>\n\
         </div>\n"
    );
    // other names, unconsumed args
    let other = html(&slides[3]);
    assert!(other.contains(r#"<div class="center">"#));
    assert!(other.contains(r#"<div class="warning large">"#));
    assert!(other.contains(r#"<div class="callout tip" data-title="Hint">"#));
    assert!(other.contains(r#"<div class="r-stack">"#));
    assert!(html(&slides[4]).contains(r#"<div class="include other.md">"#));
    assert!(parsed.diagnostics.is_empty());
}

/// dialect §5, one entry per code block of code.md: the exact opening tags.
const CODE_ROWS: [&str; 12] = [
    r#"<pre><code class="language-python" data-trim>"#,
    r#"<pre><code data-trim>"#,
    r#"<pre><code class="language-python" data-trim data-line-numbers="1-2|4">"#,
    r#"<pre><code class="language-rust" data-trim data-line-numbers="">"#,
    r#"<pre><code class="language-rust" data-trim data-line-numbers="2">"#,
    r#"<pre><code class="language-js" data-trim data-line-numbers="1|2" data-ln-start-from="10">"#,
    r#"<pre><code class="language-text">"#,
    r#"<pre><code class="language-html" data-trim data-noescape>"#,
    r#"<pre><code class="nohighlight" data-trim>"#,
    r#"<pre id="pre-id" class="big wide"><code class="language-python" data-trim>"#,
    r#"<pre><code class="language-python" data-trim data-foo="bar" data-title="Example">"#,
    r#"<pre id="snippet" class="big"><code class="language-python" data-line-numbers="1-2|4" data-ln-start-from="10" data-noescape>"#,
];

#[test]
fn code_md() {
    let parsed = check_fixture("code", &[0; 11]);
    let deck = &parsed.deck;
    assert!(deck.features.code && !deck.features.math);
    let all: String = deck.slides.iter().map(html).collect();
    let open = Regex::new(r"<pre[^>]*><code[^>]*>").unwrap();
    let tags: Vec<&str> = open.find_iter(&all).map(|m| m.as_str()).collect();
    assert_eq!(tags, CODE_ROWS);
    // escaping (row 1), trailing newline removed
    assert!(
        html(&deck.slides[0])
            .contains("return f\"&lt;b&gt;{name}&lt;/b&gt; &amp; co\"</code></pre>")
    );
    assert!(!all.contains("\n</code>"), "trailing newlines are removed");
    // escape=false keeps <mark> and the entity as written
    assert!(html(&deck.slides[6]).contains(
        "<p>Some <mark>highlighted</mark> text &amp; an entity, kept as written.</p></code></pre>"
    ));
    assert!(html(&deck.slides[10]).contains("print(df.<mark>describe</mark>())</code></pre>"));
    // trim=false keeps the indentation
    assert!(html(&deck.slides[5]).contains(">    the first line keeps its indentation<"));
}

#[test]
fn math_md() {
    let parsed = check_fixture("math", &[0, 0]);
    let deck = &parsed.deck;
    assert!(deck.features.math && !deck.features.code);
    let first = html(&deck.slides[0]);
    assert!(first.contains(r#"<span data-math-style="inline">E = mc^2</span>"#));
    assert!(first.contains(r#"<span data-math-style="inline">a &lt; b</span>"#));
    assert!(first.contains(
        "<span data-math-style=\"display\">\n\\int_0^1 x^2 \\, dx = \\frac{1}{3}\n</span>"
    ));
    assert!(
        first.contains("The cup costs $5, not"),
        "\\$ is a literal dollar"
    );
    let second = html(&deck.slides[1]);
    assert!(second.contains(
        r#"<li>a list item with <span data-math-style="inline">\alpha + \beta</span></li>"#
    ));
    assert!(second.contains(r#"<span data-math-style="display">\sum_i x_i</span>"#));
    assert!(second.contains(r#"<td><span data-math-style="inline">\pi r^2</span></td>"#));
    assert!(second.contains(
        r#"<div data-math-style="display">\nabla \cdot \mathbf{E} = \frac{\rho}{\varepsilon_0}</div>"#
    ));
    assert!(!second.contains("language-math"));
    // every math element carries data-math-style
    let elements = Regex::new(r"<(span|div) data-math-style=").unwrap();
    assert_eq!(elements.find_iter(first).count(), 4);
    assert_eq!(elements.find_iter(second).count(), 4);
}

#[test]
fn images_md() {
    let parsed = check_fixture("images", &[0; 7]);
    let deck = &parsed.deck;
    // one file, three spellings, one id; id = blake3 of the resolved absolute path
    let id = AssetRegistry::new().register_file(&fixture_dir().join("images/ok.png"));
    assert_eq!(deck.assets.len(), 1);
    assert_eq!(deck.assets[0].id, id);
    assert_eq!(deck.assets[0].origin, "file:images/ok.png");
    assert_eq!(deck.assets[0].mime, "image/png");
    assert_eq!(parsed.assets.len(), 1);
    let placeholder = Regex::new(r#"src="asset:[0-9a-f]{12}""#).unwrap();
    let first = html(&deck.slides[0]);
    assert_eq!(placeholder.find_iter(first).count(), 3);
    assert!(first.contains(&format!(
        r#"<img src="asset:{id}" alt="A local image" title="Local file" />"#
    )));
    // missing: warning with path + span, URL left as written
    assert!(
        html(&deck.slides[1]).contains(r#"<img src="images/missing.png" alt="Missing image" />"#)
    );
    assert_eq!(codes(&parsed.diagnostics), [("missing-asset", 32)]);
    let missing = parsed.diagnostics.iter().next().unwrap();
    assert_eq!(missing.level, Level::Warning);
    assert!(missing.message.contains("images/missing.png"));
    // remote, protocol-relative, root-absolute and data: URLs untouched
    let remote = html(&deck.slides[2]);
    for url in [
        "https://example.org/logo.png",
        "//example.org/logo.png",
        "/abs.png",
        "data:image/gif;base64,R0lGODlhAQABAAAAACw=",
    ] {
        assert!(remote.contains(&format!("src=\"{url}\"")), "{url}");
    }
    // {width=50%}
    assert!(html(&deck.slides[3]).contains(&format!(
        r#"<img src="asset:{id}" alt="Half width" width="50%" />"#
    )));
    // raw HTML: block, inline and poster rewritten; remote untouched
    let raw = html(&deck.slides[4]);
    assert!(raw.contains(&format!(r#"<img src="asset:{id}" alt="raw html block">"#)));
    assert!(raw.contains(&format!(
        r#"<img src="asset:{id}" alt="raw inline" width="16">"#
    )));
    assert!(raw.contains(&format!(
        r#"<video src="https://example.org/clip.mp4" poster="asset:{id}"></video>"#
    )));
    assert!(raw.contains(r#"<img src="https://example.org/remote.png" alt="remote raw html">"#));
    // links are not assets; a relative background is
    assert!(html(&deck.slides[5]).contains(r#"<a href="handout.pdf">The handout</a>"#));
    assert_eq!(
        deck.slides[6].attrs["data-background-image"],
        format!("asset:{id}")
    );
}

#[test]
fn notes_md() {
    let parsed = check_fixture("notes", &[0, 1]);
    let slides = &parsed.deck.slides;
    assert!(parsed.deck.features.notes);
    assert_eq!(
        html(&slides[0]),
        "<h1>Notes</h1>\n<p>The slide body continues after a notes block.</p>\n<p>More body text.</p>\n"
    );
    assert_eq!(
        slides[0].notes.as_deref(),
        Some(
            "<p>First block, with <em>emphasis</em> and a list:</p>\n<ul>\n<li>point one</li>\n\
             <li>point two</li>\n</ul>\n<p>Second block, appended after the first, with \
             <code>inline code</code>.</p>\n<p>Third block, at the very end of the slide.</p>\n"
        )
    );
    assert_eq!(slides[1].notes, None);
    assert_eq!(
        slides[1].children[0].notes.as_deref(),
        Some("<p>Notes work on vertical children too.</p>\n")
    );
}

fn ctx_parts() -> (AssetRegistry, Diagnostics) {
    (AssetRegistry::new(), Diagnostics::new())
}

#[test]
fn render_fragment_without_front_matter_returns_the_title() {
    let (mut assets, mut diagnostics) = ctx_parts();
    let dir = fixture_dir();
    for mode in [RenderMode::Deck, RenderMode::Notebook] {
        let mut ctx = RenderCtx {
            assets: &mut assets,
            base_dir: &dir,
            mode,
            diagnostics: &mut diagnostics,
            missing_asset_is_error: false,
        };
        let opts = comrak_options(&Default::default());
        let rendered = render_fragment("# Hi\n\n$x$", &opts, &mut ctx).unwrap();
        assert_eq!(rendered.title.as_deref(), Some("Hi"));
        assert_eq!(
            rendered.html,
            "<h1>Hi</h1>\n<p><span data-math-style=\"inline\">x</span></p>\n"
        );
        assert!(rendered.features.math && !rendered.features.code);
        assert_eq!(rendered.notes, None);
    }
    assert!(diagnostics.is_empty() && assets.is_empty());
}

#[test]
fn missing_image_is_an_error_only_when_asked() {
    let dir = fixture_dir();
    let opts = comrak_options(&Default::default());
    let text = "# Slide\n\n![gone](images/missing.png)\n";
    let (mut assets, mut diagnostics) = ctx_parts();
    let mut ctx = RenderCtx {
        assets: &mut assets,
        base_dir: &dir,
        mode: RenderMode::Deck,
        diagnostics: &mut diagnostics,
        missing_asset_is_error: true,
    };
    match render_fragment(text, &opts, &mut ctx) {
        Err(Error::MissingAsset { path, span }) => {
            assert_eq!(path, dir.join("images/missing.png"));
            let line3 = SourceSpan {
                start_line: 3,
                end_line: 3,
                cell: None,
            };
            assert_eq!(span, Some(line3));
        }
        other => panic!("expected MissingAsset, got {other:?}"),
    }
    ctx.missing_asset_is_error = false;
    let rendered = render_fragment(text, &opts, &mut ctx).unwrap();
    assert!(
        rendered
            .html
            .contains(r#"<img src="images/missing.png" alt="gone" />"#)
    );
    assert_eq!(codes(&diagnostics), [("missing-asset", 3)]);
    // parse() never fails on a missing image
    let parsed = parse_text(text).unwrap();
    assert_eq!(codes(&parsed.diagnostics), [("missing-asset", 3)]);
    // the diagnostic's path is the deck; the missing file is recovered for Error::MissingAsset
    let warning = parsed.diagnostics.iter().next().unwrap();
    assert_eq!(warning.path, Some(source("inline").path));
    assert_eq!(
        missing_asset_path(warning),
        Some(dir.join("images/missing.png"))
    );
}

#[test]
fn notebook_cells_lift_a_leading_heading_on_request() {
    let (mut assets, mut diagnostics) = ctx_parts();
    let dir = fixture_dir();
    let mut ctx = RenderCtx {
        assets: &mut assets,
        base_dir: &dir,
        mode: RenderMode::Notebook,
        diagnostics: &mut diagnostics,
        missing_asset_is_error: false,
    };
    let opts = comrak_options(&Default::default());
    let cell = "# Analysis {layout=title}\n\n$x$\n\n:::notes\nSay hi.\n:::\n";
    let (rendered, lifted) = render_fragment_with_lift(cell, &opts, &mut ctx, true).unwrap();
    assert_eq!(
        rendered.html,
        "<h1>Analysis</h1>\n<p><span data-math-style=\"inline\">x</span></p>\n"
    );
    assert_eq!(rendered.notes.as_deref(), Some("<p>Say hi.</p>\n"));
    assert_eq!(rendered.title.as_deref(), Some("Analysis"));
    let lifted = lifted.unwrap();
    assert_eq!(lifted.pairs, [("layout".to_string(), "title".to_string())]);
    let mut slide = Slide::new();
    presenter::markdown::attrs::SlideAttrs::from_parsed(lifted).apply_to(&mut slide);
    assert_eq!(slide.layout, "title");
    // render_fragment never lifts
    let plain = render_fragment(cell, &opts, &mut ctx).unwrap();
    assert!(plain.html.starts_with("<h1 layout=\"title\">Analysis</h1>"));
    assert!(diagnostics.is_empty());
}

#[test]
fn front_matter_errors_and_bad_separators() {
    assert!(matches!(
        parse_text("---\n- a list\n---\n\n# x\n"),
        Err(Error::FrontMatter(message)) if message == "expected a mapping"
    ));
    match parse_text("# One\n\n--- {title=\"unclosed}\n\n# Two\n") {
        Err(Error::Parse { path, line, .. }) => {
            assert_eq!(path, source("inline").path);
            assert_eq!(line, Some(3));
        }
        other => panic!("expected Error::Parse, got {other:?}"),
    }
    assert!(matches!(
        parse_text("---\nslides:\n  split: headings\n---\n\n# x\n"),
        Err(Error::Other(_))
    ));
}

#[test]
fn front_matter_markdown_options_apply_to_the_deck() {
    let text = "---\nmarkdown:\n  smart_punctuation: true\n  sourcepos: true\n---\n\n# \"Quoted\" -- dash\n";
    let parsed = parse_text(text).unwrap();
    let slide = &parsed.deck.slides[0];
    assert_eq!(
        slide.title.as_deref(),
        Some("\u{201c}Quoted\u{201d} \u{2013} dash")
    );
    assert_eq!(
        html(slide),
        "<h1 data-sourcepos=\"7:1-7:18\">\u{201c}Quoted\u{201d} \u{2013} dash</h1>\n"
    );
    // without the front matter the defaults keep `--` and straight quotes
    let plain = parse_text("# \"Quoted\" -- dash\n").unwrap();
    assert_eq!(
        html(&plain.deck.slides[0]),
        "<h1>&quot;Quoted&quot; -- dash</h1>\n"
    );
}
