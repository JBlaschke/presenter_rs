//! The notebook frontend (T31; `docs/spec/notebook-mapping.md` §2, §3, §5): golden IR
//! snapshots of every fixture in `tests/fixtures/ipynb/`, the spec §6 example cell for cell,
//! deck metadata precedence, tags, raw cells, notes, attachments, `metadata.presenter`
//! overrides and `split_markdown_cells`.
//!
//! Snapshots are the `Deck` as JSON with `source.path`/`source.base_dir` redacted to `[path]`
//! and asset ids replaced by stable labels (`000000000001`, … in order of first appearance;
//! ids of files hash absolute paths, so they depend on the checkout). A second snapshot per
//! fixture lists its diagnostics as `cell C, lines A-B [code] message`. Every deck, labelled
//! or not, satisfies `Deck::invariant_violations() == []`.
//!
//! Every fixture is snapshotted: the ones in [`DEDICATED`] by their own test (which also
//! checks the fixture's behaviours), any other `*.ipynb` by [`other_fixtures_are_snapshotted`]
//! with the default config.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use presenter::config::Config;
use presenter::error::{Error, Level};
use presenter::ir::{Block, CellOutput, Deck, NotebookCell, Parsed, Slide, SourceInfo, SourceKind};
use presenter::notebook::parse;
use regex::Regex;
use serde_json::{Value, json};

/// Fixtures with a dedicated test below.
const DEDICATED: [&str; 5] = ["frontmatter", "minimal", "outputs", "slideshow", "tags"];

/// The 1×1 PNG of `outputs.ipynb`, base64.
const PIXEL_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mPQqnr0HwAE3gKGkp+jugAAAABJRU5ErkJggg==";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_dir() -> PathBuf {
    root().join("tests/fixtures/ipynb")
}

fn source_in(dir: &Path, name: &str) -> SourceInfo {
    SourceInfo {
        path: dir.join(format!("{name}.ipynb")),
        kind: SourceKind::Notebook,
        base_dir: dir.to_path_buf(),
    }
}

fn parse_fixture(name: &str, config: &Config) -> Parsed {
    let source = source_in(&fixture_dir(), name);
    let json = std::fs::read_to_string(&source.path).unwrap();
    parse(&source, &json, config).unwrap()
}

/// The stems of `tests/fixtures/ipynb/*.ipynb`, sorted.
fn fixture_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fixture_dir())
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            let stem = path.file_stem()?.to_string_lossy().into_owned();
            (path.extension()? == "ipynb").then_some(stem)
        })
        .collect();
    names.sort();
    names
}

/// A notebook document with these cells and notebook metadata (nbformat 4.5).
fn notebook(cells: Value, metadata: Value) -> String {
    json!({"cells": cells, "metadata": metadata, "nbformat": 4, "nbformat_minor": 5}).to_string()
}

fn python() -> Value {
    json!({"kernelspec": {"name": "python3", "language": "python"}})
}

fn md(source: &str, metadata: Value) -> Value {
    json!({"cell_type": "markdown", "metadata": metadata, "source": source})
}

fn code(source: &str, metadata: Value, outputs: Value) -> Value {
    json!({"cell_type": "code", "execution_count": 1, "metadata": metadata, "outputs": outputs, "source": source})
}

fn raw(source: &str, metadata: Value) -> Value {
    json!({"cell_type": "raw", "metadata": metadata, "source": source})
}

fn slide_type(kind: &str) -> Value {
    json!({"slideshow": {"slide_type": kind}})
}

/// Parses an inline notebook whose relative URLs resolve against `tests/fixtures/md` (which
/// has `images/ok.png`), checking the invariants.
fn parse_inline(json: &str, config: &Config) -> presenter::Result<Parsed> {
    let source = source_in(&root().join("tests/fixtures/md"), "inline");
    let parsed = parse(&source, json, config)?;
    assert_eq!(parsed.deck.invariant_violations(), Vec::<String>::new());
    for diagnostic in parsed.diagnostics.iter() {
        assert_eq!(diagnostic.path.as_deref(), Some(source.path.as_path()));
        assert_eq!(diagnostic.level, Level::Warning, "{diagnostic}");
    }
    Ok(parsed)
}

fn inline(cells: Value) -> Parsed {
    parse_inline(&notebook(cells, python()), &Config::default()).unwrap()
}

/// `(code, cell, start line)` of every diagnostic.
fn codes(parsed: &Parsed) -> Vec<(&'static str, Option<usize>, usize)> {
    parsed
        .diagnostics
        .iter()
        .map(|d| {
            let span = d.span.as_ref();
            (
                d.code,
                span.and_then(|s| s.cell),
                span.map_or(0, |s| s.start_line),
            )
        })
        .collect()
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

/// Snapshots (deck and diagnostics) + invariants, shared by every fixture test.
fn check_fixture(name: &str, config: &Config, snapshot: &str) -> Parsed {
    let parsed = parse_fixture(name, config);
    let deck = &parsed.deck;
    assert_eq!(deck.invariant_violations(), Vec::<String>::new(), "{name}");
    assert!(deck.features.notebook, "{name}");
    let json = stable_json(deck);
    let relabelled: Deck = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(
        relabelled.invariant_violations(),
        Vec::<String>::new(),
        "{name}"
    );
    insta::assert_json_snapshot!(snapshot, json, {
        ".source.path" => "[path]",
        ".source.base_dir" => "[path]",
    });
    let diagnostics: Vec<String> = parsed
        .diagnostics
        .iter()
        .map(|d| {
            assert_eq!(d.path.as_deref(), Some(deck.source.path.as_path()), "{d}");
            let at = d.span.as_ref().map_or("notebook".to_string(), |s| {
                let cell = s.cell.map_or("-".to_string(), |c| c.to_string());
                format!("cell {cell}, lines {}-{}", s.start_line, s.end_line)
            });
            let message = d.message.replace(&*root().to_string_lossy(), "[root]");
            format!("{at} [{}] {}: {message}", d.code, d.level)
        })
        .collect();
    insta::assert_json_snapshot!(format!("{snapshot}_diagnostics"), diagnostics);
    parsed
}

fn html(block: &Block) -> &str {
    match block {
        Block::Html { html } => html,
        Block::Cell(cell) => panic!("expected Html, got cell {}", cell.index),
    }
}

fn cell(block: &Block) -> &NotebookCell {
    match block {
        Block::Cell(cell) => cell,
        Block::Html { html } => panic!("expected a cell, got {html}"),
    }
}

/// Indices of the code cells on a slide.
fn cell_indices(slide: &Slide) -> Vec<usize> {
    slide
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Cell(cell) => Some(cell.index),
            Block::Html { .. } => None,
        })
        .collect()
}

#[test]
fn every_fixture_has_a_snapshot_test() {
    let names = fixture_names();
    for name in DEDICATED {
        assert!(
            names.iter().any(|n| n == name),
            "fixture {name}.ipynb is missing"
        );
    }
}

#[test]
fn other_fixtures_are_snapshotted() {
    for name in fixture_names() {
        if !DEDICATED.contains(&name.as_str()) {
            check_fixture(&name, &Config::default(), &name);
        }
    }
}

#[test]
fn minimal_is_one_slide_with_markdown_and_a_cell() {
    let parsed = check_fixture("minimal", &Config::default(), "minimal");
    let deck = &parsed.deck;
    assert_eq!(deck.slides.len(), 1);
    let slide = &deck.slides[0];
    assert_eq!(slide.title.as_deref(), Some("Minimal notebook"));
    assert_eq!(slide.span.as_ref().and_then(|s| s.cell), Some(0));
    assert_eq!(
        html(&slide.blocks[0]),
        "<h1>Minimal notebook</h1>\n<p>One markdown cell and one code cell.</p>\n"
    );
    let hello = cell(&slide.blocks[1]);
    assert_eq!((hello.index, hello.language.as_str()), (1, "python"));
    assert_eq!(hello.source, "print(\"hello\")");
    assert!(hello.outputs.is_empty() && hello.execution_count.is_none());
    assert_eq!(hello.metadata, json!({}));
    assert!(deck.features.code && !deck.features.math && !deck.features.notes);
    assert!(parsed.diagnostics.is_empty());
}

/// notebook-mapping.md §6, exactly.
#[test]
fn slideshow_matches_the_spec_example() {
    let parsed = check_fixture("slideshow", &Config::default(), "slideshow");
    let deck = &parsed.deck;
    assert_eq!(deck.slides.len(), 2);
    let first = &deck.slides[0];
    assert_eq!(first.layout, "title");
    assert_eq!(first.title.as_deref(), Some("Analysis"));
    assert_eq!(first.blocks.len(), 2);
    assert_eq!(html(&first.blocks[0]), "<h1>Analysis</h1>\n");
    assert_eq!(html(&first.blocks[1]), "<p>Some intro</p>\n");
    assert!(first.children.is_empty() && first.notes.is_none());

    let second = &deck.slides[1];
    assert_eq!(cell_indices(second), [2, 3]);
    assert_eq!(second.blocks.len(), 2);
    let (load, describe) = (cell(&second.blocks[0]), cell(&second.blocks[1]));
    assert!(!load.fragment && !load.hide_input && !load.hide_output);
    assert!(describe.fragment && describe.hide_input && !describe.hide_output);
    assert_eq!(describe.tags, ["hide-input"]);
    assert_eq!(describe.execution_count, Some(2));
    assert!(matches!(
        &describe.outputs[..],
        [CellOutput::Display { mime, execution_count: Some(2), .. }] if mime == "text/plain"
    ));
    assert_eq!(second.title, None);
    assert_eq!(second.notes, None);

    assert_eq!(second.children.len(), 1);
    let details = &second.children[0];
    assert_eq!(details.title.as_deref(), Some("Details"));
    assert_eq!(details.blocks.len(), 2);
    assert_eq!(html(&details.blocks[0]), "<h2>Details</h2>\n");
    assert_eq!(
        html(&details.blocks[1]),
        "<video src=\"clip.mp4\" controls></video>"
    );
    assert_eq!(
        details.notes.as_deref(),
        Some("<p>Remember to mention caching</p>\n")
    );
    assert!(details.children.is_empty());
    // Cell 5 (skip) is nowhere.
    let all: Vec<usize> = deck
        .slides
        .iter()
        .flat_map(|s| std::iter::once(s).chain(&s.children))
        .flat_map(cell_indices)
        .collect();
    assert_eq!(all, [2, 3]);
    // Spans name the cell that opened each slide.
    let opened: Vec<Option<usize>> = [first, second, details]
        .iter()
        .map(|s| s.span.as_ref().and_then(|span| span.cell))
        .collect();
    assert_eq!(opened, [Some(0), Some(2), Some(4)]);
    assert!(deck.features.notebook && deck.features.code && deck.features.notes);
    // clip.mp4 does not exist: reported, URL left as written.
    assert_eq!(codes(&parsed), [("missing-asset", Some(7), 1)]);
}

/// notebook-mapping.md §5: front matter > metadata.presenter > metadata.title.
#[test]
fn frontmatter_precedence_and_removal() {
    let parsed = check_fixture("frontmatter", &Config::default(), "frontmatter");
    let deck = &parsed.deck;
    let meta = &deck.meta;
    assert_eq!(meta.title.as_deref(), Some("Title from front matter"));
    assert_eq!(meta.subtitle.as_deref(), Some("Subtitle from front matter"));
    assert_eq!(
        meta.author.as_deref(),
        Some("Author from metadata.presenter")
    );
    assert_eq!(meta.vars.get("event"), Some(&json!("Fixture Conf")));
    assert_eq!(deck.slides.len(), 2);
    let first = &deck.slides[0];
    assert_eq!(first.span.as_ref().and_then(|s| s.cell), Some(1));
    let first_html = html(&first.blocks[0]);
    assert!(first_html.starts_with("<h1>Welcome</h1>"), "{first_html}");
    assert!(!first_html.contains("---") && !first_html.contains("<hr"));
    assert!(!first_html.contains("Title from front matter"));
    assert_eq!(cell_indices(first), [2]);
    let second = &deck.slides[1];
    assert_eq!(second.span.as_ref().and_then(|s| s.cell), Some(3));
    // Only the first markdown cell's front matter is read: cell 3's `---` block is removed
    // without changing the metadata (the fixture's expected shape), reported as
    // front-matter-ignored.
    assert_eq!(second.title.as_deref(), Some("Second slide"));
    assert_eq!(
        html(&second.blocks[0]),
        "<h2>Second slide</h2>\n<p>Only the first markdown cell may carry front matter.</p>\n"
    );
    assert_eq!(codes(&parsed), [("front-matter-ignored", Some(3), 1)]);
    // Cell 0 (skip) is nowhere.
    let all: Vec<usize> = deck.slides.iter().flat_map(cell_indices).collect();
    assert_eq!(all, [2]);
}

/// notebook-mapping.md §5 without front matter: metadata.presenter > metadata.title.
#[test]
fn presenter_title_beats_the_notebook_title() {
    let title = |metadata: Value| {
        let parsed = parse_inline(
            &notebook(json!([md("text", json!({}))]), metadata),
            &Config::default(),
        )
        .unwrap();
        parsed.deck.meta.title
    };
    let both = title(json!({"title": "A", "presenter": {"title": "B"}}));
    assert_eq!(both.as_deref(), Some("B"));
    assert_eq!(title(json!({"title": "A"})).as_deref(), Some("A"));
    assert_eq!(
        title(json!({"presenter": {"title": "B"}})).as_deref(),
        Some("B")
    );
    // A presenter object without a title keeps metadata.title.
    let other = title(json!({"title": "A", "presenter": {"author": "X"}}));
    assert_eq!(other.as_deref(), Some("A"));
    // A front matter block in a later markdown cell changes nothing.
    let parsed = parse_inline(
        &notebook(
            json!([md("x", json!({})), md("---\ntitle: C\n---\n# Y", json!({}))]),
            json!({"title": "A", "presenter": {"title": "B"}}),
        ),
        &Config::default(),
    )
    .unwrap();
    assert_eq!(parsed.deck.meta.title.as_deref(), Some("B"));
    assert_eq!(html(&parsed.deck.slides[0].blocks[1]), "<h1>Y</h1>\n");
    assert_eq!(codes(&parsed), [("front-matter-ignored", Some(1), 1)]);
}

#[test]
fn front_matter_blocks_that_are_not_metadata_stay_markdown() {
    // In a later cell, `---` + a line that sets no deck metadata + `---` is a rule and a
    // setext heading, as Jupyter shows it: a scalar, a key that is no DeckMeta field nor a
    // config table (prose such as `Note: …`), or nothing at all.
    let parsed = inline(json!([
        md("x", json!({})),
        md("---\nJust a heading\n---\n\ntext", json!({})),
        md("---\n\n---\n", json!({})),
        md("---\nNote: this matters\n---\n\nbody", json!({})),
        md("---\n# commented: out\n---\n", json!({})),
    ]));
    let slide = &parsed.deck.slides[0];
    assert_eq!(
        html(&slide.blocks[1]),
        "<hr />\n<h2>Just a heading</h2>\n<p>text</p>\n"
    );
    assert_eq!(html(&slide.blocks[2]), "<hr />\n<hr />\n");
    assert_eq!(
        html(&slide.blocks[3]),
        "<hr />\n<h2>Note: this matters</h2>\n<p>body</p>\n"
    );
    assert_eq!(
        html(&slide.blocks[4]),
        "<hr />\n<h1>commented: out</h1>\n<hr />\n"
    );
    assert!(parsed.deck.meta.vars.is_empty());
    assert!(codes(&parsed).is_empty(), "{:?}", codes(&parsed));

    // Blocks that do set deck metadata (a field, reveal options, a config table) are removed
    // from a later cell, with a warning; they change nothing.
    let parsed = inline(json!([
        md("x", json!({})),
        md("---\nreveal: {controls: false}\n---\nA", json!({})),
        md(
            "---\nnotebook: {show_prompts: false}\nevent: X\n---\nB",
            json!({})
        ),
        md("---\nauthor: Me\n---\nC", json!({})),
    ]));
    let blocks: Vec<&str> = parsed.deck.slides[0].blocks[1..].iter().map(html).collect();
    assert_eq!(blocks, ["<p>A</p>\n", "<p>B</p>\n", "<p>C</p>\n"]);
    let meta = &parsed.deck.meta;
    assert!(meta.reveal.is_empty() && meta.vars.is_empty() && meta.author.is_none());
    assert_eq!(
        codes(&parsed),
        [
            ("front-matter-ignored", Some(1), 1),
            ("front-matter-ignored", Some(2), 1),
            ("front-matter-ignored", Some(3), 1),
        ]
    );
}

/// Spec §5 reads a first-cell `---` block exactly like Markdown front matter. One that holds
/// only YAML comments sets nothing; its text (a Markdown heading in Jupyter) is not shown,
/// with a warning. One that is no mapping is an error naming the cell, with a hint.
#[test]
fn first_cell_front_matter_that_sets_nothing_or_is_no_mapping() {
    let parsed = inline(json!([
        md("---\n# Intro to Pandas\n---\n", slide_type("slide")),
        md("# One", slide_type("slide")),
    ]));
    let deck = &parsed.deck;
    assert_eq!(deck.meta, Default::default());
    assert_eq!(deck.slides.len(), 1);
    assert_eq!(deck.slides[0].title.as_deref(), Some("One"));
    assert_eq!(codes(&parsed), [("front-matter-empty", Some(0), 1)]);
    let message = &parsed.diagnostics.iter().next().unwrap().message;
    assert!(message.contains("is not shown"), "{message}");
    // Real front matter with a commented-out key is not reported.
    let parsed = inline(json!([md(
        "---\n# author: X\ntitle: T\n---\n# One",
        json!({})
    )]));
    assert!(codes(&parsed).is_empty(), "{:?}", codes(&parsed));

    for yaml in ["Welcome to the talk!", "**Intro**"] {
        let source = format!("---\n{yaml}\n---\n\n# Agenda");
        let cells = json!([md(&source, json!({}))]);
        let err = parse_inline(&notebook(cells, python()), &Config::default()).unwrap_err();
        assert!(
            matches!(&err, Error::FrontMatter(message)
                if message.starts_with("cell 0: ") && message.ends_with("is read as YAML front matter)")),
            "{err}"
        );
    }
}

#[test]
fn tags_with_the_default_config() {
    let parsed = check_fixture("tags", &Config::default(), "tags");
    let deck = &parsed.deck;
    assert_eq!(deck.slides.len(), 1);
    let slide = &deck.slides[0];
    // metadata.presenter on cell 0 (which opens the slide).
    assert_eq!(slide.layout, "two-column");
    assert_eq!(slide.classes, ["dark", "wide"]);
    assert_eq!(
        slide.attrs,
        BTreeMap::from([("data-background-color".into(), "#102030".into())])
    );
    // 1-3 removed, 12 hidden twice → omitted; 14 shown.
    assert_eq!(cell_indices(slide), [4, 5, 6, 7, 8, 9, 14]);
    for block in &slide.blocks {
        if let Block::Cell(cell) = block {
            let expected = match cell.index {
                4..=6 => (true, false),
                7..=9 => (false, true),
                _ => (false, false),
            };
            assert_eq!(
                (cell.hide_input, cell.hide_output),
                expected,
                "cell {}",
                cell.index
            );
            assert!(!cell.fragment);
        }
    }
    let texts: Vec<&str> = slide
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Html { html } => Some(html.as_str()),
            Block::Cell(_) => None,
        })
        .collect();
    assert_eq!(texts.len(), 3);
    assert_eq!(texts[0], "<h1>Tags and hidden cells</h1>\n");
    assert!(texts[1].starts_with("<div class=\"fragment\">\n<p>This paragraph is a fragment"));
    assert!(texts[2].contains("is ignored (with a warning)"));
    assert_eq!(
        slide.notes.as_deref(),
        Some("<p>Speaker notes from the <code>notes</code> tag.</p>\n")
    );
    assert_eq!(
        codes(&parsed),
        [("presenter-override-ignored", Some(13), 1)]
    );
}

#[test]
fn custom_tag_names_from_the_config() {
    let remove = Config::from_toml_str(
        "[notebook]\nremove_cell_tags = [\"custom-hide\"]\nhide_input_tags = [\"unrelated\"]\n",
    )
    .unwrap();
    let parsed = check_fixture("tags", &remove, "tags_custom_config");
    let slide = &parsed.deck.slides[0];
    // remove-cell, skip and hide-cell are no longer removal tags; custom-hide is.
    assert_eq!(cell_indices(slide), [1, 2, 3, 4, 5, 6, 7, 8, 9, 12]);
    let hidden: Vec<(usize, bool)> = slide
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Cell(cell) => Some((cell.index, cell.hide_input)),
            Block::Html { .. } => None,
        })
        .collect();
    // `unrelated` (cell 5) and source_hidden (cell 6) hide inputs; hide-input (5, 12) and
    // remove-input (4) no longer do. Cell 12 keeps only its outputs_hidden flag.
    assert_eq!(
        hidden,
        [
            (1, false),
            (2, false),
            (3, false),
            (4, false),
            (5, true),
            (6, true),
            (7, false),
            (8, false),
            (9, false),
            (12, false)
        ]
    );

    let hide = Config::from_toml_str("[notebook]\nhide_output_tags = [\"custom-hide\"]\n").unwrap();
    let parsed = parse_fixture("tags", &hide);
    let custom = parsed.deck.slides[0]
        .blocks
        .iter()
        .find_map(|block| match block {
            Block::Cell(cell) if cell.index == 14 => Some(cell),
            _ => None,
        })
        .unwrap();
    assert!(custom.hide_output && !custom.hide_input);
}

#[test]
fn outputs_fixture() {
    let parsed = check_fixture("outputs", &Config::default(), "outputs");
    let deck = &parsed.deck;
    // Cell 0 (markdown) and the 12 code cells each open a slide.
    assert_eq!(deck.slides.len(), 13);
    assert!(deck.features.math && deck.features.code && !deck.features.notes);
    // Both PNG cells register the same bytes: one asset.
    assert_eq!(deck.assets.len(), 1);
    assert_eq!(deck.assets[0].mime, "image/png");
    let latex = cell(&deck.slides[5].blocks[0]);
    assert!(matches!(
        &latex.outputs[..],
        [CellOutput::Display { mime, html, .. }]
            if mime == "text/latex" && html.starts_with("<div data-math-style=\"display\">")
    ));
    let widget_only = cell(&deck.slides[11].blocks[0]);
    assert!(matches!(
        &widget_only.outputs[..],
        [CellOutput::Unsupported { .. }]
    ));
    let output_codes: Vec<_> = codes(&parsed);
    assert_eq!(
        output_codes,
        [
            ("output-unsupported", Some(11), 1),
            ("output-widget", Some(12), 1)
        ]
    );
}

#[test]
fn zero_cells_give_one_empty_slide() {
    let parsed = parse_inline(&notebook(json!([]), json!({})), &Config::default()).unwrap();
    let deck = &parsed.deck;
    assert_eq!(deck.slides, [Slide::new()]);
    assert!(deck.features.notebook && !deck.features.code);
    assert_eq!(codes(&parsed), [("empty-slide", None, 0)]);
    // Only skipped cells: the same.
    let parsed = inline(json!([md("x", slide_type("skip"))]));
    assert_eq!(parsed.deck.slides, [Slide::new()]);
}

#[test]
fn front_matter_only_cell_is_metadata_only() {
    // The recommended pattern (spec §5): a first markdown cell with only front matter, left
    // at "-", then a slide marker. It must not leave an empty slide 1 behind.
    let parsed = inline(json!([
        md(
            "---\ntitle: From the cell\nnotebook: {show_prompts: false}\n---\n",
            json!({})
        ),
        md("# One", slide_type("slide")),
        md("# Two", slide_type("slide")),
    ]));
    let deck = &parsed.deck;
    assert_eq!(deck.meta.title.as_deref(), Some("From the cell"));
    assert_eq!(deck.slides.len(), 2);
    assert_eq!(deck.slides[0].title.as_deref(), Some("One"));
    assert!(codes(&parsed).is_empty(), "{:?}", codes(&parsed));
    // Also when the front matter cell is skipped: its metadata still counts.
    let parsed = inline(json!([
        md(
            "---\ntitle: Skipped cell\n---\n# Not shown",
            slide_type("skip")
        ),
        md("# One", json!({})),
    ]));
    assert_eq!(parsed.deck.meta.title.as_deref(), Some("Skipped cell"));
    assert_eq!(parsed.deck.slides.len(), 1);
    assert_eq!(html(&parsed.deck.slides[0].blocks[0]), "<h1>One</h1>\n");
}

#[test]
fn front_matter_only_cell_keeps_its_slide_structure() {
    // After other content, a front-matter-only `slide` cell opens slide 2 (spec §2), which
    // the next cell fills; its metadata.presenter applies to that slide.
    let fm_slide =
        |kind: &str| json!({"slideshow": {"slide_type": kind}, "presenter": {"classes": ["x"]}});
    let parsed = inline(json!([
        code("setup()", slide_type("slide"), json!([])),
        md("---\ntitle: Deck\n---\n", fm_slide("slide")),
        md("# Next", json!({})),
    ]));
    let deck = &parsed.deck;
    assert_eq!(deck.meta.title.as_deref(), Some("Deck"));
    assert_eq!(deck.slides.len(), 2);
    assert_eq!(cell_indices(&deck.slides[0]), [0]);
    let next = &deck.slides[1];
    assert_eq!(next.span.as_ref().and_then(|s| s.cell), Some(1));
    assert_eq!(next.classes, ["x"]);
    assert_eq!(next.title.as_deref(), Some("Next"));
    assert_eq!(next.blocks, [Block::html("<h1>Next</h1>\n".into())]);
    assert!(codes(&parsed).is_empty(), "{:?}", codes(&parsed));

    // A `subslide` one opens a vertical child.
    let parsed = inline(json!([
        code("setup()", slide_type("slide"), json!([])),
        md("---\ntitle: Deck\n---\n", fm_slide("subslide")),
        md("# Next", json!({})),
    ]));
    let deck = &parsed.deck;
    assert_eq!(deck.slides.len(), 1);
    let child = &deck.slides[0].children[0];
    assert_eq!(child.span.as_ref().and_then(|s| s.cell), Some(1));
    assert_eq!(
        (child.title.as_deref(), child.classes.as_slice()),
        (Some("Next"), &["x".to_string()][..])
    );

    // A `-` one opens nothing: its override is ignored, with a warning.
    let parsed = inline(json!([
        code("setup()", slide_type("slide"), json!([])),
        md("---\ntitle: Deck\n---\n", fm_slide("-")),
        md("# Next", json!({})),
    ]));
    assert_eq!(parsed.deck.slides.len(), 1);
    assert!(parsed.deck.slides[0].classes.is_empty());
    assert_eq!(codes(&parsed), [("presenter-override-ignored", Some(1), 1)]);
}

#[test]
fn front_matter_only_first_cell_overrides_slide_one() {
    // Before any slide, the cell opens nothing (whatever its slide type); its override
    // goes to slide 1, which the next cell creates or opens.
    for (kind, next) in [
        ("slide", "-"),
        ("slide", "slide"),
        ("-", "slide"),
        ("subslide", "-"),
    ] {
        let parsed = inline(json!([
            md(
                "---\ntitle: T\n---\n",
                json!({"slideshow": {"slide_type": kind}, "presenter": {"layout": "title"}})
            ),
            md("# A", slide_type(next)),
            md("# B", slide_type("slide")),
        ]));
        let deck = &parsed.deck;
        assert_eq!(deck.slides.len(), 2, "{kind} {next}");
        let first = &deck.slides[0];
        assert_eq!(first.layout, "title", "{kind} {next}");
        assert_eq!(first.title.as_deref(), Some("A"));
        assert_eq!(first.span.as_ref().and_then(|s| s.cell), Some(1));
        assert_eq!(deck.slides[1].layout, "default");
        assert!(codes(&parsed).is_empty(), "{:?}", codes(&parsed));
    }
    // The only cell: the deck's one empty slide gets the override.
    let parsed = inline(json!([md(
        "---\ntitle: T\n---\n",
        json!({"presenter": {"layout": "title", "classes": [1]}})
    )]));
    assert_eq!(parsed.deck.slides.len(), 1);
    assert_eq!(parsed.deck.slides[0].layout, "title");
    assert_eq!(
        codes(&parsed),
        [
            ("empty-slide", None, 0),
            ("presenter-override-invalid", Some(0), 1)
        ]
    );
}

#[test]
fn notebook_metadata_overrides_the_config() {
    let metadata = json!({
        "kernelspec": {"language": "python"},
        "presenter": {"notebook": {"remove_cell_tags": ["drop"]}, "reveal": {"transition": "fade"}},
    });
    let cells = json!([
        md(
            "---\nnotebook: {hide_input_tags: [quiet]}\n---\n# T",
            json!({})
        ),
        code("a", json!({"tags": ["drop"]}), json!([])),
        code("b", json!({"tags": ["quiet"]}), json!([])),
    ]);
    let parsed = parse_inline(&notebook(cells, metadata), &Config::default()).unwrap();
    let slide = &parsed.deck.slides[0];
    assert_eq!(cell_indices(slide), [2]);
    assert!(cell(&slide.blocks[1]).hide_input);
    // Both layers' notebook tables stay in vars, merged, for the pipeline's own merge.
    assert_eq!(
        parsed.deck.meta.vars["notebook"],
        json!({"remove_cell_tags": ["drop"], "hide_input_tags": ["quiet"]})
    );
    assert_eq!(parsed.deck.meta.reveal["transition"], json!("fade"));
    let mut config = Config::default();
    config.merge_front_matter(&parsed.deck.meta).unwrap();
    assert_eq!(config.notebook.remove_cell_tags, ["drop"]);
    assert_eq!(config.notebook.hide_input_tags, ["quiet"]);
}

#[test]
fn invalid_deck_metadata_is_an_error() {
    let bad_presenter = notebook(json!([]), json!({"presenter": {"title": ["a"]}}));
    let err = parse_inline(&bad_presenter, &Config::default()).unwrap_err();
    assert!(
        matches!(&err, Error::FrontMatter(message) if message.starts_with("metadata.presenter: title")),
        "{err}"
    );
    let bad_yaml = notebook(
        json!([
            code("x", json!({}), json!([])),
            md("---\ntitle: a: b\n---\n", json!({}))
        ]),
        json!({}),
    );
    let err = parse_inline(&bad_yaml, &Config::default()).unwrap_err();
    assert!(
        matches!(&err, Error::FrontMatter(message) if message.starts_with("cell 1: ") && message.contains("line 2")),
        "{err}"
    );
    // Config tables and reveal.plugins name the layer they came from.
    let starts = |cells: Value, metadata: Value, prefix: &str| {
        let err = parse_inline(&notebook(cells, metadata), &Config::default()).unwrap_err();
        assert!(
            matches!(&err, Error::FrontMatter(message) if message.starts_with(prefix)),
            "{err} does not start with {prefix}"
        );
    };
    let fm_table = json!([
        code("x", json!({}), json!([])),
        md("---\nnotebook: {hide_inputs: []}\n---\n", json!({}))
    ]);
    starts(
        fm_table,
        json!({}),
        "cell 1: notebook: unknown field `hide_inputs`",
    );
    let presenter_table = json!({"presenter": {"notebook": {"hide_inputs": []}}});
    starts(
        json!([]),
        presenter_table,
        "metadata.presenter: notebook: unknown field `hide_inputs`",
    );
    let presenter_plugins = json!({"presenter": {"reveal": {"plugins": ["nope"]}}});
    starts(json!([]), presenter_plugins, "metadata.presenter: ");
    // The presenter layer is valid, the front matter layer is not.
    let both = json!({"presenter": {"notebook": {"show_prompts": false}}});
    starts(
        json!([md("---\nmath: {engine: 1}\n---\n", json!({}))]),
        both,
        "cell 0: math: ",
    );
}

#[test]
fn slide_types_and_tags_combine() {
    let parsed = inline(json!([
        md("intro", json!({})),
        code(
            "x",
            json!({"slideshow": {"slide_type": "slide"}, "tags": ["fragment"]}),
            json!([])
        ),
        md(
            "said aloud",
            json!({"slideshow": {"slide_type": "subslide"}, "tags": ["notes"]})
        ),
        code("print(1 < 2)", slide_type("notes"), json!([])),
        raw(
            "<b>raw note</b>",
            json!({"raw_mimetype": "text/html", "slideshow": {"slide_type": "notes"}})
        ),
        md("", slide_type("fragment")),
    ]));
    let deck = &parsed.deck;
    assert_eq!(deck.slides.len(), 2);
    // Content before the first marker is slide 1 (opened by cell 0).
    assert_eq!(deck.slides[0].span.as_ref().and_then(|s| s.cell), Some(0));
    let second = &deck.slides[1];
    assert!(cell(&second.blocks[0]).fragment);
    let child = &second.children[0];
    assert!(child.blocks.is_empty());
    assert_eq!(
        child.notes.as_deref(),
        Some("<p>said aloud</p>\n<pre><code>print(1 &lt; 2)</code></pre>\n<b>raw note</b>")
    );
    // A notes-only slide is not empty; the empty fragment cell adds nothing.
    assert!(codes(&parsed).is_empty(), "{:?}", codes(&parsed));
    assert!(deck.features.notes && deck.features.code);
}

#[test]
fn empty_slides_are_reported() {
    let parsed = inline(json!([
        md("# A", json!({})),
        // Hidden input and output: the cell is omitted, but its `slide` opens slide 2
        // (spec §2 structure), which the next cell does not fill.
        code(
            "setup()",
            json!({"slideshow": {"slide_type": "slide"}, "tags": ["hide-input", "hide-output"]}),
            json!([])
        ),
        md("", slide_type("slide")),
        md(
            "",
            json!({"slideshow": {"slide_type": "subslide"}, "presenter": {"classes": ["x"]}})
        ),
        md("# B", slide_type("slide")),
    ]));
    let deck = &parsed.deck;
    // An empty `slide` cell opens its slide (spec §2 structure), with no content.
    assert_eq!(deck.slides.len(), 4);
    assert_eq!(deck.slides[0].blocks.len(), 1);
    assert!(deck.slides[1].blocks.is_empty() && deck.slides[2].blocks.is_empty());
    assert_eq!(deck.slides[2].children[0].classes, ["x"]);
    let messages: Vec<&str> = parsed
        .diagnostics
        .iter()
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "slide 2 is empty",
            "slide 3 is empty",
            "slide 3.1 is empty (it only has attributes)"
        ]
    );
    assert_eq!(
        codes(&parsed),
        [
            ("empty-slide", Some(1), 1),
            ("empty-slide", Some(2), 1),
            ("empty-slide", Some(3), 1)
        ]
    );
}

/// Spec §2/§3: a code cell with hidden input and output is omitted entirely, but its slide
/// type still structures the deck (as in nbconvert): between slides a hidden `slide` cell
/// opens a slide the next cells fill. Before the first slide it opens nothing, so a hidden
/// setup cell at the top adds no empty slide 1; a raw cell that is not shown behaves the
/// same.
#[test]
fn hidden_and_skipped_cells_add_no_slides() {
    let hidden_both = [
        json!({"tags": ["hide-input", "hide-output"]}),
        json!({"jupyter": {"source_hidden": true, "outputs_hidden": true}}),
        json!({"tags": ["remove-input"], "jupyter": {"outputs_hidden": true}, "slideshow": {"slide_type": "slide"}}),
    ];
    for metadata in hidden_both {
        let parsed = inline(json!([
            code("import numpy", metadata.clone(), json!([])),
            md("# Title", slide_type("slide")),
        ]));
        let deck = &parsed.deck;
        assert_eq!(deck.slides.len(), 1, "{metadata}");
        assert_eq!(deck.slides[0].title.as_deref(), Some("Title"));
        assert_eq!(deck.slides[0].span.as_ref().and_then(|s| s.cell), Some(1));
        assert!(codes(&parsed).is_empty(), "{:?}", codes(&parsed));
    }
    // Before the first slide, its metadata.presenter goes to slide 1.
    let parsed = inline(json!([
        code(
            "import numpy",
            json!({"tags": ["hide-input", "hide-output"], "presenter": {"layout": "title"}}),
            json!([])
        ),
        md("# Title", json!({})),
    ]));
    assert_eq!(parsed.deck.slides.len(), 1);
    assert_eq!(parsed.deck.slides[0].layout, "title");
    assert!(codes(&parsed).is_empty(), "{:?}", codes(&parsed));

    // Between slides, a hidden `slide` cell opens its slide; its metadata.presenter applies
    // to it and the following content lands there (not on the previous slide).
    let hidden_slide = |presenter: Value| json!({"slideshow": {"slide_type": "slide"}, "tags": ["hide-input", "hide-output"], "presenter": presenter});
    let parsed = inline(json!([
        md("# A", json!({})),
        code("x", hidden_slide(json!({"layout": "title"})), json!([])),
        md("after", json!({})),
    ]));
    let deck = &parsed.deck;
    assert_eq!(deck.slides.len(), 2);
    assert_eq!(deck.slides[0].blocks.len(), 1);
    let second = &deck.slides[1];
    assert_eq!(second.span.as_ref().and_then(|s| s.cell), Some(1));
    assert_eq!(second.layout, "title");
    assert_eq!(second.blocks, [Block::html("<p>after</p>\n".into())]);
    assert!(!deck.features.code, "the hidden cell is not emitted");
    assert!(codes(&parsed).is_empty(), "{:?}", codes(&parsed));

    // Collapsed in JupyterLab (metadata.jupyter) is the same; a hidden `subslide` cell opens
    // a vertical slide whose title comes from the next cell.
    let collapsed = json!({"slideshow": {"slide_type": "slide"}, "jupyter": {"source_hidden": true, "outputs_hidden": true}});
    let mut collapsed_sub = collapsed.clone();
    collapsed_sub["slideshow"]["slide_type"] = json!("subslide");
    let parsed = inline(json!([
        md("# A", slide_type("slide")),
        code("plot()", collapsed, json!([])),
        md("## Results\n\nB content", json!({})),
        code("more()", collapsed_sub, json!([])),
        md("## Details", json!({})),
    ]));
    let deck = &parsed.deck;
    let titles: Vec<Option<&str>> = deck.slides.iter().map(|s| s.title.as_deref()).collect();
    assert_eq!(titles, [Some("A"), Some("Results")]);
    assert_eq!(
        html(&deck.slides[1].blocks[0]),
        "<h2>Results</h2>\n<p>B content</p>\n"
    );
    assert_eq!(deck.slides[1].children.len(), 1);
    assert_eq!(deck.slides[1].children[0].title.as_deref(), Some("Details"));
    assert!(codes(&parsed).is_empty(), "{:?}", codes(&parsed));

    // A hidden `-` cell opens nothing: its metadata.presenter is ignored, with a warning.
    let parsed = inline(json!([
        md("# A", json!({})),
        code(
            "x",
            json!({"tags": ["hide-input", "hide-output"], "presenter": {"layout": "title"}}),
            json!([])
        ),
        md("more", json!({})),
    ]));
    assert_eq!(parsed.deck.slides.len(), 1);
    assert_eq!(parsed.deck.slides[0].blocks.len(), 2);
    assert_eq!(parsed.deck.slides[0].layout, "default");
    assert_eq!(codes(&parsed), [("presenter-override-ignored", Some(1), 1)]);

    // A Quarto-style raw front matter cell (no raw_mimetype) at the top.
    let parsed = inline(json!([
        raw(
            "---\ntitle: Q\n---",
            json!({"presenter": {"classes": ["r"]}})
        ),
        md("# T", slide_type("slide")),
    ]));
    let deck = &parsed.deck;
    assert_eq!(deck.slides.len(), 1);
    assert_eq!(deck.slides[0].classes, ["r"]);
    assert_eq!(codes(&parsed), [("raw-cell-skipped", Some(0), 1)]);
    // Once a slide exists, a skipped raw `slide` cell still opens one (§2 structure).
    let parsed = inline(json!([
        md("# A", json!({})),
        raw("\\newpage", slide_type("slide")),
        md("more", json!({})),
    ]));
    let deck = &parsed.deck;
    assert_eq!(deck.slides.len(), 2);
    assert_eq!(deck.slides[1].span.as_ref().and_then(|s| s.cell), Some(1));
    assert_eq!(html(&deck.slides[1].blocks[0]), "<p>more</p>\n");
    assert_eq!(codes(&parsed), [("raw-cell-skipped", Some(1), 1)]);
}

#[test]
fn heading_attributes_lift_only_onto_an_empty_slide() {
    let parsed = inline(json!([
        md("# Title {.big layout=title}\n\ntext", slide_type("slide")),
        md("## Later {.small}", json!({})),
        code("x", slide_type("slide"), json!([])),
        md("## After a cell {.c}", json!({})),
    ]));
    let deck = &parsed.deck;
    let first = &deck.slides[0];
    assert_eq!(
        (first.layout.as_str(), first.classes.as_slice()),
        ("title", &["big".to_string()][..])
    );
    assert_eq!(html(&first.blocks[0]), "<h1>Title</h1>\n<p>text</p>\n");
    assert_eq!(html(&first.blocks[1]), "<h2 class=\"small\">Later</h2>\n");
    let second = &deck.slides[1];
    assert!(second.classes.is_empty());
    assert_eq!(
        html(&second.blocks[1]),
        "<h2 class=\"c\">After a cell</h2>\n"
    );
    // The title is the first heading of the slide's first HTML block, also after a cell.
    assert_eq!(second.title.as_deref(), Some("After a cell"));
}

/// Spec §2: "plain text of the first heading in the slide's first HTML block".
#[test]
fn slide_titles_come_from_the_first_html_block() {
    let html_raw = |source: &str, kind: &str| {
        raw(
            source,
            json!({"raw_mimetype": "text/html", "slideshow": {"slide_type": kind}}),
        )
    };
    let parsed = inline(json!([
        html_raw(
            "<h2 class=\"t\">Raw <em>x</em><br/>y &amp; z</h2><p>p</p>",
            "slide"
        ),
        md("## Md", json!({})),
        md("intro text", slide_type("slide")),
        md("## Later heading", json!({})),
        html_raw("<p>no heading</p>", "slide"),
        md("## After raw", json!({})),
        md("", slide_type("slide")),
        md(":::notes\nn\n:::", json!({})),
        md("## First block", json!({})),
        code("x", slide_type("slide"), json!([])),
        html_raw("<header><h3>\n  In   a header\n</h3></header>", "-"),
        // A commented-out heading is not shown, so it is no title.
        html_raw("<!-- <h1>old</h1> --><p>x</p><!-- <h2>", "slide"),
        md("## Later", json!({})),
        html_raw("<!-- <h1>old</h1> --><h2>New</h2>", "slide"),
    ]));
    let titles: Vec<Option<&str>> = parsed
        .deck
        .slides
        .iter()
        .map(|s| s.title.as_deref())
        .collect();
    assert_eq!(
        titles,
        [
            Some("Raw x y & z"),
            None,
            None,
            Some("First block"),
            Some("In a header"),
            None,
            Some("New")
        ]
    );
}

#[test]
fn presenter_overrides_use_slide_attribute_rules() {
    let parsed = inline(json!([
        md(
            "# A",
            json!({
                "slideshow": {"slide_type": "slide"},
                "presenter": {
                    "layout": "section",
                    "attrs": {"background": "images/ok.png", "transition": "zoom", "data-x": 1, "bad key": "v", "weird": "w"},
                    "classes": "one two",
                    "extra": true,
                }
            })
        ),
        md(
            "# B {layout=title .h}",
            json!({"slideshow": {"slide_type": "slide"}, "presenter": {"layout": "section", "id": "b"}})
        ),
    ]));
    let deck = &parsed.deck;
    let first = &deck.slides[0];
    assert_eq!(first.layout, "section");
    assert_eq!(first.classes, ["one", "two"]);
    let background = &first.attrs["data-background-image"];
    assert!(background.starts_with("asset:"), "{background}");
    assert_eq!(first.attrs["data-transition"], "zoom");
    assert_eq!(first.attrs["data-x"], "1");
    assert_eq!(first.attrs["weird"], "w");
    assert!(!first.attrs.contains_key("bad key"));
    assert_eq!(deck.assets.len(), 1);
    assert_eq!(deck.assets[0].origin, "file:images/ok.png");
    // A heading's attributes win over metadata.presenter, as over a separator.
    let second = &deck.slides[1];
    assert_eq!(
        (second.layout.as_str(), second.id.as_deref()),
        ("title", None)
    );
    assert_eq!(second.classes, ["h"]);
    assert_eq!(
        codes(&parsed),
        [
            ("presenter-override-invalid", Some(0), 1),
            ("unknown-slide-attr", Some(0), 1),
            ("invalid-slide-attr", Some(0), 1),
            ("presenter-override-invalid", Some(1), 1),
        ]
    );
    let messages: Vec<&str> = parsed
        .diagnostics
        .iter()
        .map(|d| d.message.as_str())
        .collect();
    assert!(messages[1].contains("`weird` on slide 1"), "{messages:?}");
    assert!(messages[3].contains("unknown key `id`"), "{messages:?}");
}

#[test]
fn raw_cells() {
    let parsed = inline(json!([
        raw(
            "<img src=\"images/ok.png\"> <img src=\"images/missing.png\">",
            json!({"raw_mimetype": "text/html"})
        ),
        raw("<p>legacy</p>", json!({"format": "text/html"})),
        raw(
            "<b>frag</b>",
            json!({"raw_mimetype": "text/html", "slideshow": {"slide_type": "fragment"}})
        ),
        raw("\\section{x}", json!({"raw_mimetype": "text/latex"})),
        raw("plain", json!({})),
        raw("  \n", json!({})),
        raw(
            "<span data-math-style=\"inline\">x</span>",
            json!({"raw_mimetype": "text/html"})
        ),
    ]));
    let slide = &parsed.deck.slides[0];
    assert_eq!(slide.blocks.len(), 4);
    let first = html(&slide.blocks[0]);
    assert!(first.starts_with("<img src=\"asset:"), "{first}");
    assert!(
        first.ends_with("<img src=\"images/missing.png\">"),
        "{first}"
    );
    assert_eq!(html(&slide.blocks[1]), "<p>legacy</p>");
    assert_eq!(
        html(&slide.blocks[2]),
        "<div class=\"fragment\">\n<b>frag</b>\n</div>\n"
    );
    assert!(parsed.deck.features.math);
    assert_eq!(slide.title, None);
    assert_eq!(
        codes(&parsed),
        [
            ("missing-asset", Some(0), 1),
            ("raw-cell-skipped", Some(3), 1),
            ("raw-cell-skipped", Some(4), 1),
        ]
    );
    let messages: Vec<&str> = parsed
        .diagnostics
        .iter()
        .map(|d| d.message.as_str())
        .collect();
    assert!(messages[1].contains("`text/latex`") && messages[2].contains("no raw_mimetype"));
}

#[test]
fn html_outputs_and_markdown_outputs_resolve_local_files() {
    let outputs = json!([
        {"output_type": "display_data", "metadata": {},
         "data": {"text/html": ["<video src=\"images/ok.png\" poster='images/ok.png'></video>"]}},
        {"output_type": "display_data", "metadata": {},
         "data": {"text/markdown": "![x](images/ok.png) and $x$"}},
        {"output_type": "display_data", "metadata": {},
         "data": {"text/html": "<img src=\"nope.png\">"}},
    ]);
    let parsed = inline(json!([code("show()", json!({}), outputs)]));
    let deck = &parsed.deck;
    let cell = cell(&deck.slides[0].blocks[0]);
    let htmls: Vec<&str> = cell
        .outputs
        .iter()
        .map(|output| match output {
            CellOutput::Display { html, .. } => html.as_str(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert!(!htmls[0].contains("images/ok.png") && htmls[0].matches("asset:").count() == 2);
    assert!(htmls[1].contains("src=\"asset:"), "{}", htmls[1]);
    assert_eq!(htmls[2], "<img src=\"nope.png\">");
    assert_eq!(deck.assets.len(), 1, "one file, registered once");
    assert!(deck.features.math);
    assert_eq!(codes(&parsed), [("missing-asset", Some(0), 1)]);
}

#[test]
fn attachments_become_inline_assets() {
    let attachments = json!({
        "pixel.png": {"image/png": format!("{PIXEL_PNG}\n")},
        "my plot.png": {"image/png": PIXEL_PNG},
        "broken.png": {"image/png": "!!!"},
        "pic.svg": {"image/svg+xml": "<svg xmlns=\"http://www.w3.org/2000/svg\"/>"},
    });
    let source = "![a](attachment:pixel.png) ![b](attachment:my%20plot.png) \
                  ![c](attachment:broken.png) ![d](attachment:pic.svg) ![e](attachment:gone.png)";
    let cell = json!({"cell_type": "markdown", "metadata": {}, "source": source, "attachments": attachments});
    let parsed = inline(json!([cell, md("![f](attachment:pixel.png)", json!({}))]));
    let deck = &parsed.deck;
    let first = html(&deck.slides[0].blocks[0]);
    assert_eq!(first.matches("src=\"asset:").count(), 3, "{first}");
    assert!(first.contains("src=\"attachment:broken.png\""), "{first}");
    assert!(first.contains("src=\"attachment:gone.png\""), "{first}");
    // Same bytes once; the SVG separately.
    assert_eq!(deck.assets.len(), 2);
    let mimes: Vec<&str> = deck.assets.iter().map(|a| a.mime.as_str()).collect();
    assert!(mimes.contains(&"image/png") && mimes.contains(&"image/svg+xml"));
    assert!(deck.assets.iter().all(|a| a.origin.starts_with("inline:")));
    // Attachments belong to their cell: cell 1 has none.
    assert_eq!(
        codes(&parsed),
        [
            ("notebook-attachment-skipped", Some(0), 1),
            ("missing-attachment", Some(0), 1),
            ("missing-attachment", Some(1), 1),
        ]
    );
}

#[test]
fn attachment_diagnostics_name_the_attachment_as_written() {
    let attachments = json!({
        "my plot.png": {"image/png": "!!!"},
        "empty.png": {},
    });
    let source = "![a](attachment:my%20plot.png) ![b](attachment:empty.png) \
                  ![c](attachment:ü.png) ![d](attachment:a&b.png)";
    let cell = json!({"cell_type": "markdown", "metadata": {}, "source": source, "attachments": attachments});
    let raw_cell = json!({
        "cell_type": "raw",
        "metadata": {"raw_mimetype": "text/html"},
        "source": "<img src='attachment:single.png'> <IMG SRC = \"attachment:upper.png\">",
    });
    let parsed = inline(json!([cell, raw_cell]));
    let first = html(&parsed.deck.slides[0].blocks[0]);
    assert!(first.contains("src=\"attachment:empty.png\""), "{first}");
    assert_eq!(
        codes(&parsed),
        [
            ("notebook-attachment-skipped", Some(0), 1),
            ("notebook-attachment-skipped", Some(0), 1),
            ("missing-attachment", Some(0), 1),
            ("missing-attachment", Some(0), 1),
            ("missing-attachment", Some(1), 1),
            ("missing-attachment", Some(1), 1),
        ]
    );
    let messages: Vec<&str> = parsed
        .diagnostics
        .iter()
        .map(|d| d.message.as_str())
        .collect();
    // Attachments are tried in name order: "empty.png" before "my plot.png".
    assert_eq!(messages[0], "attachment 'empty.png' skipped: no data");
    assert!(messages[1].starts_with("attachment 'my plot.png' skipped: invalid base64"));
    assert_eq!(
        messages[2..],
        [
            "`attachment:ü.png`: the cell has no attachment `ü.png`",
            "`attachment:a&b.png`: the cell has no attachment `a&b.png`",
            "`attachment:single.png`: the cell has no attachment `single.png`",
            "`attachment:upper.png`: the cell has no attachment `upper.png`",
        ]
    );
}

#[test]
fn attachment_urls_spelled_differently_from_the_name_resolve() {
    // Spellings the source rewrite does not know: `+` percent-encoded, lower-case hex, an
    // HTML entity in raw HTML. Once decoded they name an attachment and resolve to it.
    let svg = |width: u32| format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\"/>");
    let attachments = json!({
        "a+b.png": {"image/png": PIXEL_PNG},
        "ü.png": {"image/svg+xml": svg(1)},
        "a&b.png": {"image/svg+xml": svg(2)},
        "bad+.png": {"image/png": "!!!"},
    });
    let source = "![a](attachment:a%2Bb.png) ![u](attachment:%c3%bc.png) \
                  <img src=\"attachment:a&amp;b.png\">\n\n\
                  ![bad](attachment:bad%2B.png) ![bad](attachment:bad+.png) \
                  ![n](attachment:none%2B.png)\n\n\
                  :::notes\n![notes](attachment:a%2bb.png)\n:::\n";
    let cell = json!({"cell_type": "markdown", "metadata": {}, "source": source, "attachments": attachments});
    let raw_cell = json!({
        "cell_type": "raw",
        "metadata": {"raw_mimetype": "text/html"},
        "source": "<img src=\"attachment:a&amp;b.png\"> <img src='attachment:%c3%a4.png'> \
                   <img src=\"attachment:a%26b.png \">",
        "attachments": {"a&b.png": {"image/svg+xml": svg(2)}, "ä.png": {"image/png": PIXEL_PNG}},
    });
    let parsed = inline(json!([cell, raw_cell]));
    let slide = &parsed.deck.slides[0];
    let first = html(&slide.blocks[0]);
    assert_eq!(first.matches("src=\"asset:").count(), 3, "{first}");
    assert!(!first.contains("attachment:a"), "{first}");
    assert!(!first.contains("attachment:%"), "{first}");
    // A skipped attachment stays as written (reported once, under any spelling).
    assert!(first.contains("src=\"attachment:bad%2B.png\""), "{first}");
    assert!(first.contains("src=\"attachment:bad+.png\""), "{first}");
    assert!(first.contains("src=\"attachment:none%2B.png\""), "{first}");
    let notes = slide.notes.as_deref().unwrap_or_default();
    assert!(notes.contains("src=\"asset:"), "{notes}");
    assert!(!notes.contains("attachment:"), "{notes}");
    let raw_html = html(&slide.blocks[1]);
    assert_eq!(raw_html.matches("asset:").count(), 3, "{raw_html}");
    assert!(!raw_html.contains("attachment:"), "{raw_html}");
    // a+b.png and ä.png share their bytes; the two SVGs differ.
    assert_eq!(parsed.deck.assets.len(), 3);
    assert_eq!(
        codes(&parsed),
        [
            ("notebook-attachment-skipped", Some(0), 1),
            ("missing-attachment", Some(0), 1),
        ]
    );
    assert_eq!(
        parsed.diagnostics.iter().nth(1).map(|d| d.message.as_str()),
        Some("`attachment:none+.png`: the cell has no attachment `none+.png`")
    );

    // Split markdown cells resolve every part.
    let mut config = Config::default();
    config.notebook.split_markdown_cells = true;
    let split = json!({
        "cell_type": "markdown", "metadata": {},
        "source": "# One\n\n![a](attachment:a%2Bb.png)\n\n---\n\n# Two\n\n![b](attachment:a%2bb.png)\n",
        "attachments": {"a+b.png": {"image/png": PIXEL_PNG}},
    });
    let parsed = parse_inline(&notebook(json!([split]), python()), &config).unwrap();
    assert_eq!(parsed.deck.slides.len(), 2);
    for slide in &parsed.deck.slides {
        let body = html(&slide.blocks[0]);
        assert!(
            body.contains("src=\"asset:") && !body.contains("attachment:"),
            "{body}"
        );
    }
    assert!(parsed.diagnostics.is_empty(), "{:?}", codes(&parsed));
}

#[test]
fn notes_in_markdown_outputs_are_speaker_notes() {
    let outputs = json!([
        {"output_type": "display_data", "metadata": {},
         "data": {"text/markdown": "hi\n\n:::notes\nsecret\n:::\n"}},
    ]);
    let parsed = inline(json!([code("show()", json!({}), outputs.clone())]));
    let deck = &parsed.deck;
    let slide = &deck.slides[0];
    assert!(matches!(
        &cell(&slide.blocks[0]).outputs[..],
        [CellOutput::Display { html, .. }] if html == "<p>hi</p>\n"
    ));
    assert_eq!(slide.notes.as_deref(), Some("<p>secret</p>\n"));
    assert!(deck.features.notes);
    // Hidden outputs: their notes are not added.
    let parsed = inline(json!([code(
        "show()",
        json!({"tags": ["hide-output"]}),
        outputs
    )]));
    assert_eq!(parsed.deck.slides[0].notes, None);
    assert!(!parsed.deck.features.notes);
}

#[test]
fn unknown_cells_are_skipped_and_indices_stay_aligned() {
    let parsed = inline(json!([
        {"cell_type": "future", "metadata": {}, "source": "?"},
        code("x", json!({}), json!([])),
    ]));
    let slide = &parsed.deck.slides[0];
    assert_eq!(cell_indices(slide), [1]);
    assert_eq!(slide.span.as_ref().and_then(|s| s.cell), Some(1));
    assert_eq!(codes(&parsed), [("unknown-cell-type", Some(0), 1)]);
}

#[test]
fn markdown_cell_diagnostics_use_cell_lines() {
    let parsed = inline(json!([
        md(
            "---\ntitle: T\n---\n# A\n\n![x](images/missing.png)",
            json!({})
        ),
        md("text\n\n:::nope x=\"a:b\"\n:::", json!({})),
    ]));
    assert_eq!(
        codes(&parsed),
        [
            ("missing-asset", Some(0), 6),
            ("invalid-directive", Some(1), 3)
        ]
    );
    let first = &parsed.diagnostics.iter().next().unwrap();
    assert!(
        first
            .to_string()
            .contains("inline.ipynb: cell 0, line 6: warning"),
        "{first}"
    );
}

#[test]
fn split_markdown_cells_off_keeps_separators_as_content() {
    let parsed = inline(json!([md("# A\n\n---\n\n# B", json!({}))]));
    assert_eq!(parsed.deck.slides.len(), 1);
    assert!(html(&parsed.deck.slides[0].blocks[0]).contains("<hr />"));
}

#[test]
fn split_markdown_cells_open_slides() {
    let config = Config::from_toml_str("[notebook]\nsplit_markdown_cells = true\n").unwrap();
    let cells = json!([
        md(
            "# One {.lifted}\n\nintro\n\n---\n\n# Two\n\n--\n\n## Two.1\n\n:::notes\nsay\n:::\n\n--- {#three background=images/ok.png}\n\n# Three",
            json!({})
        ),
        md("more on three", json!({})),
        md("--\n\n## Three.1", json!({})),
        md("--- {.x}\n\n# Four", slide_type("slide")),
        code("c", slide_type("slide"), json!([])),
        md(
            "## Continues {.kept}\n\n--- {auto-animate}\n\n# Five",
            json!({})
        ),
        md("-- {.lead}\n\nvertical first", slide_type("fragment")),
    ]);
    let parsed = parse_inline(&notebook(cells, python()), &config).unwrap();
    let deck = &parsed.deck;
    let titles: Vec<(Option<&str>, Vec<Option<&str>>)> = deck
        .slides
        .iter()
        .map(|s| {
            (
                s.title.as_deref(),
                s.children.iter().map(|c| c.title.as_deref()).collect(),
            )
        })
        .collect();
    assert_eq!(
        titles,
        [
            (Some("One"), vec![]),
            (Some("Two"), vec![Some("Two.1")]),
            (Some("Three"), vec![Some("Three.1")]),
            (Some("Four"), vec![]),
            (Some("Continues"), vec![]),
            (Some("Five"), vec![None]),
        ]
    );
    let one = &deck.slides[0];
    assert_eq!(one.classes, ["lifted"]);
    assert_eq!(
        one.span.as_ref().map(|s| (s.start_line, s.cell)),
        Some((1, Some(0)))
    );
    let two = &deck.slides[1];
    assert_eq!(
        two.span.as_ref().map(|s| (s.start_line, s.end_line)),
        Some((5, 7))
    );
    assert_eq!(two.children[0].notes.as_deref(), Some("<p>say</p>\n"));
    let three = &deck.slides[2];
    assert_eq!(three.id.as_deref(), Some("three"));
    assert!(three.attrs["data-background-image"].starts_with("asset:"));
    assert_eq!(three.blocks.len(), 2, "cell 1 continues slide three");
    // A leading separator in the cell that opened the slide opens nothing new.
    let four = &deck.slides[3];
    assert_eq!(four.classes, ["x"]);
    assert_eq!(four.span.as_ref().and_then(|s| s.cell), Some(3));
    // Cell 5 continues the code slide: its heading keeps its attributes.
    let continues = &deck.slides[4];
    assert_eq!(cell_indices(continues), [4]);
    assert!(continues.classes.is_empty());
    assert_eq!(
        html(&continues.blocks[1]),
        "<h2 class=\"kept\">Continues</h2>\n"
    );
    let five = &deck.slides[5];
    assert_eq!(five.attrs["data-auto-animate"], "");
    // A leading `--` opens a vertical slide; the fragment flag applies to its content.
    let lead = &five.children[0];
    assert_eq!(lead.classes, ["lead"]);
    assert_eq!(
        html(&lead.blocks[0]),
        "<div class=\"fragment\">\n<p>vertical first</p>\n</div>\n"
    );
    assert!(codes(&parsed).is_empty(), "{:?}", codes(&parsed));
}

#[test]
fn split_markdown_cells_from_front_matter_and_errors() {
    let cells = json!([
        md(
            "---\nnotebook: {split_markdown_cells: true}\n---\n# A\n\n---\n\n# B\ntext\n--- {.x}\n",
            json!({})
        ),
        md("# C\n\n---\n", json!({})),
    ]);
    let parsed = parse_inline(&notebook(cells, python()), &Config::default()).unwrap();
    let deck = &parsed.deck;
    assert_eq!(deck.slides.len(), 3);
    // Spans are cell lines, counting the front matter.
    let b = deck.slides[1].span.as_ref().unwrap();
    assert_eq!((b.start_line, b.end_line, b.cell), (6, 10, Some(0)));
    // A trailing separator opens a slide the next cells fill; this one stays empty.
    assert_eq!(
        codes(&parsed),
        [
            ("separator-not-recognized", Some(0), 10),
            ("empty-slide", Some(1), 3)
        ]
    );
    let bad = notebook(
        json!([md("# A\n\n--- {title=\"x}\n\n# B", json!({}))]),
        json!({}),
    );
    let config = Config::from_toml_str("[notebook]\nsplit_markdown_cells = true\n").unwrap();
    let err = parse_inline(&bad, &config).unwrap_err();
    assert!(
        matches!(&err, Error::Parse { line: None, message, .. } if message.starts_with("cell 0, line 3: ")),
        "{err}"
    );
}

#[test]
fn split_cells_give_their_overrides_to_the_first_slide_they_open() {
    let config = Config::from_toml_str("[notebook]\nsplit_markdown_cells = true\n").unwrap();
    let overrides = |presenter: Value| json!({"presenter": presenter});
    let cells = json!([
        md("# A", json!({})),
        // A leading separator opens slide 2: the override goes there.
        md("---\n\n# B", overrides(json!({"classes": ["x"]}))),
        // The first part continues slide 2; the first separator opens slide 3, whose own
        // attributes win over the override.
        md(
            "more\n\n--- {layout=title}\n\n# C\n\n---\n\n# D",
            overrides(json!({"layout": "section", "classes": ["y"]}))
        ),
        // No separator: nothing opened, ignored with a warning.
        md("plain", overrides(json!({"classes": ["z"]}))),
    ]);
    let parsed = parse_inline(&notebook(cells, python()), &config).unwrap();
    let deck = &parsed.deck;
    let slides: Vec<(Option<&str>, &str, Vec<String>)> = deck
        .slides
        .iter()
        .map(|s| (s.title.as_deref(), s.layout.as_str(), s.classes.clone()))
        .collect();
    assert_eq!(
        slides,
        [
            (Some("A"), "default", vec![]),
            (Some("B"), "default", vec!["x".to_string()]),
            (Some("C"), "title", vec!["y".to_string()]),
            (Some("D"), "default", vec![]),
        ]
    );
    assert_eq!(cell_indices(&deck.slides[3]), Vec::<usize>::new());
    assert_eq!(deck.slides[3].blocks.len(), 2, "cell 3 continues slide D");
    assert_eq!(codes(&parsed), [("presenter-override-ignored", Some(3), 1)]);
    let message = &parsed.diagnostics.iter().next().unwrap().message;
    assert!(
        message.contains("or a separator in a split markdown cell"),
        "{message}"
    );
}

/// Renders the notebook fixtures (standalone, default theme, effective config as the
/// pipeline builds it) to `target/preview/notebook-<name>.html` for the manual browser check.
#[test]
#[ignore = "writes target/preview/notebook-*.html for the manual browser check; run with --ignored"]
fn writes_notebook_previews_for_manual_browser_checks() {
    use presenter::assets::emit::OutputMode;
    use presenter::render::{self, BuildInfo, RenderInput, context::effective_config};

    let preview = root().join("target/preview");
    std::fs::create_dir_all(&preview).unwrap();
    let theme = presenter::theme::resolve("default", &[]).unwrap();
    for name in fixture_names() {
        let parsed = parse_fixture(&name, &Config::default());
        let config = effective_config(&Config::default(), &parsed.deck.meta, &theme).unwrap();
        let document = render::render(RenderInput {
            deck: &parsed.deck,
            assets: &parsed.assets,
            theme: &theme,
            config: &config,
            output: OutputMode::Standalone,
            build: BuildInfo::build(true),
        })
        .unwrap();
        for diagnostic in document.diagnostics.iter() {
            eprintln!("{name}: {diagnostic:#}");
        }
        std::fs::write(
            preview.join(format!("notebook-{name}.html")),
            &document.html,
        )
        .unwrap();
    }
}
