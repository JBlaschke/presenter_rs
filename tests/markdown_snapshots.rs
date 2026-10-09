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
const DEDICATED: [&str; 10] = [
    "attribute-recovery",
    "attributes",
    "code",
    "directives",
    "images",
    "math",
    "minimal",
    "notes",
    "pandoc-directives",
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
    let parsed = check_fixture("directives", &[0, 0, 0, 0]);
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

/// A3 (F04, F34, F35): attribute blocks comrak rejects are read with the dialect §3.1
/// grammar on headings, fences, images, links and inline code.
#[test]
fn attribute_recovery_md() {
    let parsed = check_fixture("attribute-recovery", &[0; 6]);
    let slides = &parsed.deck.slides;
    let id = AssetRegistry::new().register_file(&fixture_dir().join("images/ok.png"));
    // headings: lifted, clean, no literal `{…}` and no autolink
    assert_eq!(slides[0].classes, ["dark"]);
    assert_eq!(slides[0].attrs["data-background-color"], "#222");
    assert_eq!(slides[0].title.as_deref(), Some("Dark"));
    assert!(html(&slides[0]).starts_with("<h1>Dark</h1>\n"));
    assert_eq!(
        slides[1].attrs["data-background-image"],
        format!("asset:{id}")
    );
    assert!(html(&slides[1]).starts_with("<h2>Image</h2>\n"));
    assert_eq!(
        slides[2].attrs["data-background-image"],
        "https://example.org/bg.png"
    );
    assert!(
        html(&slides[2]).starts_with("<h2>Remote</h2>\n"),
        "{}",
        html(&slides[2])
    );
    assert_eq!(slides[3].id.as_deref(), Some("q"));
    assert_eq!(slides[3].attrs["data-note"], "two words");
    // fences
    let code = html(&slides[3]);
    assert!(code.contains(
        "<pre><code class=\"language-python\" data-trim data-line-numbers=\"1|3\">a = 1"
    ));
    assert!(
        code.contains("<pre><code data-trim data-line-numbers=\"1,3\">x"),
        "{code}"
    );
    // images, links, inline code; `title=` without a Markdown title
    let inline = html(&slides[4]);
    for expected in [
        format!(r#"<img src="asset:{id}" alt="Half" width="50%" />"#),
        r#"<a href="https://example.org" data-x="y z">a link</a>"#.to_string(),
        r#"<code data-x="a/b">code</code>."#.to_string(),
        r#"<a href="https://example.org" title="Link title">Titled link</a>"#.to_string(),
        format!(r#"<img src="asset:{id}" alt="Titled image" title="Image title" />"#),
    ] {
        assert!(inline.contains(&expected), "{expected}\n{inline}");
    }
    assert!(!inline.contains('{'), "{inline}");
    // math in a title; text in braces; a block that is not valid stays and is reported
    assert_eq!(slides[5].title.as_deref(), Some("The E=mc^2 law"));
    let last = html(&slides[5]);
    assert!(last.contains("<p>The set {1, 2, 3} stays text.</p>"));
    assert!(last.contains("<h3>Not attributes {.big auto-animate}</h3>"));
    assert_eq!(codes(&parsed.diagnostics), [("unparsed-attr-block", 68)]);
    assert_eq!(parsed.deck.assets.len(), 1);
}

/// A3 fix round: a recovered block gives what its double-quoted form gives comrak, also
/// where the text around it matters (adjacent code spans, reference links, a setext
/// heading's only line, two blocks on a heading), and items separated by commas are not
/// attributes.
#[test]
fn recovered_blocks_render_like_their_double_quoted_form() {
    for text in [
        "Keys `Ctrl`{data-k='a'}`C`{data-k='b'} copy\n",
        "Keys `Ctrl`{data-k='a/b'}`C` copy\n",
        "[ref]{data-x='y'}(note)\n\n[ref]: https://r.org\n",
        "Intro\n\n{k='v'}\n===\n",
        "# H {.a} {k='v'}\n",
        "![a](images/ok.png){width='5'}{.b}\n",
    ] {
        let single = parse_text(text).unwrap();
        let double = parse_text(&text.replace('\'', "\"")).unwrap();
        assert_eq!(
            stable_json(&single.deck)["slides"],
            stable_json(&double.deck)["slides"],
            "{text}"
        );
        assert_eq!(
            codes(&single.diagnostics),
            codes(&double.diagnostics),
            "{text}"
        );
    }
    let parsed = parse_text("Keys `Ctrl`{data-k='a'}`C`{data-k='b'} copy\n").unwrap();
    assert_eq!(
        html(&parsed.deck.slides[0]),
        "<p>Keys <code data-k=\"a\">Ctrl</code><code data-k=\"b\">C</code> copy</p>\n"
    );
    let parsed = parse_text("[ref]{data-x='y'}(note)\n\n[ref]: https://r.org\n").unwrap();
    assert_eq!(
        html(&parsed.deck.slides[0]),
        "<p><a href=\"https://r.org\" data-x=\"y\">ref</a>(note)</p>\n"
    );
    let parsed = parse_text("# H {.a} {k='v'}\n").unwrap();
    assert_eq!(parsed.deck.slides[0].title.as_deref(), Some("H {.a}"));
    // prose in braces after a heading that is not lifted is kept, with a warning
    let parsed =
        parse_text("# Intro\n\nSome text.\n\n## Config {debug=true, verbose=false}\n").unwrap();
    assert!(
        html(&parsed.deck.slides[0]).ends_with("<h2>Config {debug=true, verbose=false}</h2>\n")
    );
    assert_eq!(codes(&parsed.diagnostics), [("unparsed-attr-block", 5)]);
    // a block that would end a table cell early stays text, with a warning
    let parsed = parse_text("| a | b |\n|---|---|\n| `c`{lines=1|3} | d |\n").unwrap();
    assert_eq!(codes(&parsed.diagnostics), [("unparsed-attr-block", 3)]);
}

/// A3 (F26, F14): pandoc's `::: {.class}` directives and the fragment index.
#[test]
fn pandoc_directives_md() {
    let parsed = check_fixture("pandoc-directives", &[0, 0]);
    let slides = &parsed.deck.slides;
    assert_eq!(
        html(&slides[0]),
        "<h2>Pandoc fenced divs</h2>\n\
         <div class=\"fragment fade-up\">\n<p>Fades up.</p>\n</div>\n\
         <div class=\"columns\" style=\"grid-template-columns: 1fr 2fr\">\n\
         <div class=\"column\">\n<p>Left.</p>\n</div>\n\
         <div id=\"intro\" class=\"note\">\n<p>Right.</p>\n</div>\n\
         </div>\n"
    );
    assert_eq!(
        slides[0].notes.as_deref(),
        Some("<p>Speaker notes in pandoc syntax.</p>\n")
    );
    let fragments = html(&slides[1]);
    assert!(fragments.contains("<div class=\"fragment\">\n<p>Not a number"));
    assert!(fragments.contains("<div class=\"fragment\" data-fragment-index=\"2\">"));
    assert_eq!(codes(&parsed.diagnostics), [("invalid-directive-arg", 38)]);
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
                file: None,
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
    assert!(ctx.diagnostics.is_empty());
    // render_fragment never lifts; the layout left on the heading is reported (A3)
    let plain = render_fragment(cell, &opts, &mut ctx).unwrap();
    assert!(plain.html.starts_with("<h1 layout=\"title\">Analysis</h1>"));
    assert_eq!(codes(&diagnostics), [("unlifted-slide-attr", 1)]);
}

#[test]
fn front_matter_errors_and_bad_separators() {
    // A3 (F05): the root value's position and the front matter's extent, with the question
    match parse_text("---\n- a list\n---\n\n# x\n") {
        Err(err @ Error::FrontMatter(_)) => {
            assert_eq!(
                err.to_string(),
                "front matter: expected a mapping at line 2 column 1 (the front matter runs from \
                 line 1 to the `---` on line 3; is its closing `---` missing, or was line 1 \
                 meant as a slide separator?)"
            );
            assert_eq!(err.span().map(|span| span.start_line), Some(2));
            assert_eq!(err.column(), Some(1));
        }
        other => panic!("expected Error::FrontMatter, got {other:?}"),
    }
    match parse_text("# One\n\n--- {title=\"unclosed}\n\n# Two\n") {
        Err(Error::Parse { path, line, .. }) => {
            assert_eq!(path, source("inline").path);
            assert_eq!(line, Some(3));
        }
        other => panic!("expected Error::Parse, got {other:?}"),
    }
    // headings mode (T43) is implemented: front matter switches it on
    let headings = parse_text("---\nslides:\n  split: headings\n---\n\n# x\n\n## y\n").unwrap();
    assert_eq!(shape(&headings.deck), [1]);
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

/// The `Config` with `slides.split = "headings"`.
fn headings_config() -> Config {
    Config::from_toml_str("[slides]\nsplit = \"headings\"\n").unwrap()
}

/// A3 (F05): front matter that swallowed slide content is reported, not silent.
#[test]
fn front_matter_that_swallows_slides_is_reported() {
    // a deck that starts with a `---` rule: the heading is a YAML comment
    let parsed = parse_text("---\n\n# Real\n\n---\n\n# Two\n").unwrap();
    assert_eq!(parsed.deck.slides.len(), 1);
    assert_eq!(codes(&parsed.diagnostics), [("front-matter-empty", 1)]);
    let warning = parsed.diagnostics.iter().next().unwrap();
    assert_eq!(warning.span.as_ref().map(|s| s.end_line), Some(5));
    assert!(
        warning
            .message
            .contains("to the `---` on line 5 and sets nothing"),
        "{warning}"
    );
    // a missing closing `---`: the title slide is a YAML comment
    let unclosed =
        "---\ntitle: My talk\nauthor: Me\n\n# My talk {layout=title}\n\n---\n\n## Next\n\nMore\n";
    let parsed = parse_text(unclosed).unwrap();
    assert_eq!(parsed.deck.meta.title.as_deref(), Some("My talk"));
    assert_eq!(codes(&parsed.diagnostics), [("front-matter-unclosed", 5)]);
    assert_eq!(
        parsed.diagnostics.iter().next().unwrap().message,
        "`# My talk {layout=title}` on line 5 is a YAML comment, not a slide: the front \
         matter runs from line 1 to the `---` on line 7. Is its closing `---` missing (before \
         line 5)?"
    );
    // A3 fix round: valid front matter is no false alarm: options all commented out
    // (values or whole keys), or a closing remark right on the closing `---`
    for text in [
        "---\nreveal:\n  # transition: fade\n  # controls: false\n---\n\n# Hello\n",
        "---\n# title: My talk\n# author: Me\n---\n\n# Real title\n",
        "---\ntitle: X\nauthor: Y\n\n# More options live in presenter.toml\n---\n\n# Hello\n",
        "---\ntitle: Talk\nauthor: Me\n\n# Remember to update the abstract before the conference\n---\n\n# Hello\n",
    ] {
        let parsed = parse_text(text).unwrap();
        assert!(
            parsed.diagnostics.is_empty(),
            "{text:?}: {:?}",
            parsed.diagnostics
        );
    }
    // with body text it is a YAML error: located, with the same question
    let with_body = "---\ntitle: T\n\n# Title\n\nSome text\n\n---\n\n## Next\n";
    match parse_text(with_body) {
        Err(err @ Error::FrontMatter(_)) => {
            let message = err.to_string();
            assert!(
                message.ends_with(
                    "(the front matter runs from line 1 to the `---` on line 8; is its closing \
                     `---` missing?)"
                ),
                "{message}"
            );
            assert_eq!(err.span().map(|s| s.start_line), Some(7));
        }
        other => panic!("expected a front matter error, got {other:?}"),
    }
}

/// The scaffold of `presenter init` with its placeholders filled in.
fn scaffold() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/scaffold/slides.md");
    std::fs::read_to_string(path)
        .unwrap()
        .replace("{{child_theme}}", "")
        .replace("{{name_yaml}}", "\"Talk\"")
        .replace("{{name}}", "Talk")
}

/// A3 (F05): the scaffold's own front matter comments are no false alarm, and the scaffold
/// with its closing `---` deleted is reported.
#[test]
fn scaffold_front_matter_is_not_reported_unless_unclosed() {
    let text = scaffold();
    let parsed = parse_text(&text).unwrap();
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    // delete the closing `---` and the notes under the title (which would be a YAML error)
    let mut lines: Vec<&str> = text.lines().collect();
    let closing = lines.iter().skip(1).position(|l| *l == "---").unwrap() + 1;
    lines.remove(closing);
    let notes = lines.iter().position(|l| *l == ":::notes").unwrap();
    let end = notes + lines[notes..].iter().position(|l| *l == ":::").unwrap();
    lines.drain(notes..=end);
    let parsed = parse_text(&lines.join("\n")).unwrap();
    let found = codes(&parsed.diagnostics);
    assert_eq!(
        found,
        [("front-matter-unclosed", closing + 2)],
        "{lines:#?}"
    );
}

/// A3 (F36): `---` directly followed by `---` is empty front matter, as `---`, blank, `---`.
#[test]
fn dashes_on_lines_one_and_two_are_empty_front_matter() {
    for (text, line) in [("---\n---\n", 2), ("---\n\n---\n", 3)] {
        let parsed = parse_text(text).unwrap();
        assert_eq!(parsed.deck.slides.len(), 1, "{text:?}");
        // the one empty slide of an empty document, after the front matter
        assert_eq!(
            codes(&parsed.diagnostics),
            [("empty-slide", line)],
            "{text:?}"
        );
    }
    let parsed = parse_text("---\n---\n\n# Only\n").unwrap();
    assert_eq!(shape(&parsed.deck), [0]);
    assert_eq!(parsed.deck.slides[0].title.as_deref(), Some("Only"));
    assert!(parsed.diagnostics.is_empty());
    // a `***` rule or `---` later in the deck is still a separator
    let parsed = parse_text("***\n---\n\n# x\n").unwrap();
    assert_eq!(parsed.deck.slides.len(), 2);
    // A3 fix round: later separators are not taken for the closing `---` (comrak would
    // pair line 1 with the first of them and swallow slide 1 as YAML)
    let jekyll = "---\n---\n\n# A\n\ntext\n\n---\n\n# B\n";
    for text in [
        jekyll.to_string(),
        jekyll.replace('\n', "\r\n"),
        format!("\u{feff}{jekyll}"),
        "---\n---\n\n# A\n\n---\n\n# B\n".to_string(),
        "---\n---\n\n# A\n\nWelcome to the talk.\n\n---\n\n# B\n\nMore.\n".to_string(),
    ] {
        let parsed = parse_text(&text).unwrap();
        let titles: Vec<_> = parsed
            .deck
            .slides
            .iter()
            .map(|s| s.title.as_deref())
            .collect();
        assert_eq!(titles, [Some("A"), Some("B")], "{text:?}");
        assert_eq!(parsed.deck.meta, Default::default(), "{text:?}");
        assert!(
            parsed.diagnostics.is_empty(),
            "{text:?}: {:?}",
            parsed.diagnostics
        );
    }
    let parsed = parse(&source("inline"), jekyll, &headings_config()).unwrap();
    assert_eq!(parsed.deck.slides.len(), 2);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    // `---` with something on line 2 is still ordinary front matter
    let parsed = parse_text("---\ntitle: T\n---\n\n# A\n\n---\n\n# B\n").unwrap();
    assert_eq!(parsed.deck.meta.title.as_deref(), Some("T"));
    assert_eq!(parsed.deck.slides.len(), 2);
}

/// A3 (F12): `:::notes` before a slide's heading is not visible content.
#[test]
fn notes_before_the_heading_do_not_hide_it() {
    // rules mode: the heading's attributes are lifted
    let parsed =
        parse_text(":::notes\nSay hi.\n:::\n\n# Welcome {layout=section .x #w}\n").unwrap();
    let slide = &parsed.deck.slides[0];
    assert_eq!(
        (slide.layout.as_str(), slide.id.as_deref()),
        ("section", Some("w"))
    );
    assert_eq!(slide.classes, ["x"]);
    assert_eq!(html(slide), "<h1>Welcome</h1>\n");
    assert_eq!(slide.notes.as_deref(), Some("<p>Say hi.</p>\n"));
    assert!(parsed.diagnostics.is_empty());
    // headings mode: no notes-only blank slide, the notes stay with the title slide
    let text = ":::notes\nWelcome everyone.\n:::\n\n# Intro {layout=title}\n\n# Part two\n\ntext\n";
    let parsed = parse(&source("inline"), text, &headings_config()).unwrap();
    let slides = &parsed.deck.slides;
    assert_eq!(slides.len(), 2);
    assert_eq!(slides[0].layout, "title");
    assert_eq!(
        slides[0].notes.as_deref(),
        Some("<p>Welcome everyone.</p>\n")
    );
    assert_eq!(slides[1].title.as_deref(), Some("Part two"));
    // after a separator, too
    let text = "# A\n\n---\n\n:::notes\nn\n:::\n\n## B {.b}\n";
    let parsed = parse(&source("inline"), text, &headings_config()).unwrap();
    assert_eq!(parsed.deck.slides.len(), 2);
    assert_eq!(parsed.deck.slides[1].classes, ["b"]);
    // a heading that is not lifted keeps its slide attributes, with a warning
    let parsed = parse_text("# A\n\nText.\n\n## B {layout=section data-id=b}\n").unwrap();
    assert!(html(&parsed.deck.slides[0]).contains(r#"<h2 layout="section" data-id="b">B</h2>"#));
    assert_eq!(codes(&parsed.diagnostics), [("unlifted-slide-attr", 5)]);
}

/// A3 (F12): a notebook markdown cell that starts with notes lifts its heading.
#[test]
fn notebook_cells_lift_a_heading_after_notes() {
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
    let cell = "<!-- intro -->\n\n:::notes\nHello.\n:::\n\n# Title {layout=section}\n";
    let (rendered, lifted) = render_fragment_with_lift(cell, &opts, &mut ctx, true).unwrap();
    assert_eq!(
        lifted.unwrap().pairs,
        [("layout".to_string(), "section".to_string())]
    );
    assert!(
        rendered.html.ends_with("<h1>Title</h1>\n"),
        "{}",
        rendered.html
    );
    assert_eq!(rendered.notes.as_deref(), Some("<p>Hello.</p>\n"));
    // A3 (F04): attribute blocks comrak rejects are read in cells too
    let cell = "# T {.dark background=#222}\n\n![i](images/ok.png){width='50%'}\n";
    let (rendered, lifted) = render_fragment_with_lift(cell, &opts, &mut ctx, true).unwrap();
    let lifted = lifted.unwrap();
    assert_eq!(lifted.classes, ["dark"]);
    assert_eq!(
        lifted.pairs,
        [("background".to_string(), "#222".to_string())]
    );
    assert!(
        rendered
            .html
            .starts_with("<h1>T</h1>\n<p><img src=\"asset:")
    );
    assert!(rendered.html.ends_with(" alt=\"i\" width=\"50%\" /></p>\n"));
    assert!(diagnostics.is_empty());
}

/// A3 fix round: with `split_markdown_cells`, a continuing cell's heading after `:::notes`
/// or a comment keeps its attributes when the current slide is not empty
/// (notebook-mapping §2); the splitter now looks past those nodes for the heading to lift.
#[test]
fn split_notebook_cells_lift_a_heading_only_onto_an_empty_slide() {
    let cell = |source: &str, slide: Option<&str>| {
        let mut metadata = serde_json::json!({});
        if let Some(kind) = slide {
            metadata["slideshow"] = serde_json::json!({ "slide_type": kind });
        }
        serde_json::json!({ "cell_type": "markdown", "metadata": metadata, "source": source })
    };
    let notebook = serde_json::json!({
        "nbformat": 4,
        "nbformat_minor": 5,
        "metadata": {},
        "cells": [
            cell("# Results\n\nSome text", Some("slide")),
            cell(":::notes\nthe caveat\n:::\n\n## Details {layout=section .x data-k=v}\n\nmore", None),
            cell("<!-- c -->\n\n## Again {layout=section}\n\nx", None),
            cell(":::notes\nhello\n:::\n\n# New {layout=title .t}\n\nbody", Some("slide")),
        ],
    });
    let dir = fixture_dir();
    let source = SourceInfo {
        path: dir.join("split.ipynb"),
        kind: SourceKind::Notebook,
        base_dir: dir,
    };
    let config = Config::from_toml_str("[notebook]\nsplit_markdown_cells = true\n").unwrap();
    let parsed = presenter::notebook::parse(&source, &notebook.to_string(), &config).unwrap();
    let slides = &parsed.deck.slides;
    assert_eq!(slides.len(), 2);
    assert_eq!(slides[0].layout, "default");
    assert!(slides[0].classes.is_empty() && slides[0].attrs.is_empty());
    let body: String = slides[0]
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Html { html } => Some(html.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        body.contains("<h2 class=\"x\" layout=\"section\" data-k=\"v\">Details</h2>"),
        "{body}"
    );
    assert!(body.contains("<h2 layout=\"section\">Again</h2>"), "{body}");
    // a cell that opens a slide lifts its heading past the notes
    assert_eq!(slides[1].layout, "title");
    assert_eq!(slides[1].classes, ["t"]);
    let found: Vec<_> = parsed.diagnostics.iter().map(|d| d.code).collect();
    assert_eq!(found, ["unlifted-slide-attr", "unlifted-slide-attr"]);
}

/// A3 (F13): same-length nested directive fences are reported in both split modes.
#[test]
fn same_length_nested_directives_are_reported() {
    let text = "# Title\n\n---\n\n## Cols\n\n:::columns\n:::column\nLeft\n:::\n:::column\nRight\n:::\n:::\n\n---\n\n## Next slide\n";
    let parsed = parse_text(text).unwrap();
    assert_eq!(codes(&parsed.diagnostics), [("invalid-directive", 14)]);
    let message = &parsed.diagnostics.iter().next().unwrap().message;
    assert!(
        message.contains("`::::columns` … `:::column` … `:::` … `::::`"),
        "{message}"
    );
    let headings = "# Title\n\n## Cols\n\n:::columns\n:::column\nL\n:::\n:::column\nR\n:::\n:::\n\n## Next slide\n";
    let parsed = parse(&source("inline"), headings, &headings_config()).unwrap();
    assert_eq!(codes(&parsed.diagnostics), [("invalid-directive", 12)]);
    // the longer outer fence is fine
    let fixed = text.replacen(":::columns", "::::columns", 1).replacen(
        ":::\n\n---\n\n## Next",
        "::::\n\n---\n\n## Next",
        1,
    );
    let parsed = parse_text(&fixed).unwrap();
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.deck.slides.len(), 3);
    // the warning says how far the unnamed directive runs
    assert!(
        message.contains("to the end of the document (or notebook cell), and `---` lines inside it start no slides"),
        "{message}"
    );
}

/// A3 fix round: a `:::name a:b` line comrak did not open is reported once, with the fix,
/// also when paragraphs come between it and its closing `:::`, and in pandoc's form.
#[test]
fn unopened_directives_name_the_fix() {
    let text = "# T\n\n:::note title=\"Note: read\"\n\nFirst paragraph.\n\nSecond paragraph.\n\n:::\n\n---\n\n# Next\n";
    let parsed = parse_text(text).unwrap();
    assert_eq!(codes(&parsed.diagnostics), [("invalid-directive", 3)]);
    let message = &parsed.diagnostics.iter().next().unwrap().message;
    assert!(message.ends_with("write the colon as `&#58;`"), "{message}");
    // the fix works: the entity is the colon
    let fixed = text.replace("Note: read", "Note&#58; read");
    let parsed = parse_text(&fixed).unwrap();
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert!(
        html(&parsed.deck.slides[0]).contains("<div class=\"note\" data-title=\"Note: read\">")
    );
    // pandoc's form, with a colon in the block or closing colons after it
    let parsed = parse_text("# T\n\n::: {.callout title=\"Note: x\"}\nBody\n:::\n").unwrap();
    assert_eq!(codes(&parsed.diagnostics), [("invalid-directive", 3)]);
    let message = &parsed.diagnostics.iter().next().unwrap().message;
    assert!(
        message.starts_with("`::: {.callout …}` is not a directive"),
        "{message}"
    );
    assert!(message.ends_with("write the colon as `&#58;`"), "{message}");
    let parsed = parse_text("# T\n\n::: {.notes} :::\nSecret notes\n:::\n").unwrap();
    assert_eq!(codes(&parsed.diagnostics), [("invalid-directive", 3)]);
    let message = &parsed.diagnostics.iter().next().unwrap().message;
    assert!(
        message.contains("remove the `:::` after the attribute block")
            && message.ends_with("the speaker notes in it are shown on the slide"),
        "{message}"
    );
}

/// A3 (F22): a deck image found only by letter case is reported (on a case-insensitive
/// file system; elsewhere it is missing).
#[test]
fn images_found_only_by_letter_case_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("figures")).unwrap();
    std::fs::copy(
        fixture_dir().join("images/ok.png"),
        dir.path().join("figures/plain.png"),
    )
    .unwrap();
    let source = SourceInfo {
        path: dir.path().join("deck.md"),
        kind: SourceKind::Markdown,
        base_dir: dir.path().to_path_buf(),
    };
    let text = "![](figures/plain.png)\n\n![](Figures/plain.PNG)\n\n<img src=\"./figures/../figures/PLAIN.png\">\n";
    let parsed = parse(&source, text, &Config::default()).unwrap();
    let insensitive = dir.path().join("FIGURES/PLAIN.PNG").is_file();
    let found = codes(&parsed.diagnostics);
    if insensitive {
        assert_eq!(
            found,
            [("asset-case-mismatch", 3), ("asset-case-mismatch", 5)]
        );
        let first = &parsed.diagnostics.iter().next().unwrap().message;
        assert_eq!(
            first,
            "image `Figures/plain.PNG` matches `figures/plain.png` only by letter case: it is \
             found on this file system, but not on a case-sensitive one (Linux); write \
             `figures/plain.png`"
        );
        assert_eq!(parsed.deck.assets.len(), 1);
    } else {
        assert_eq!(found, [("missing-asset", 3), ("missing-asset", 5)]);
    }
}
