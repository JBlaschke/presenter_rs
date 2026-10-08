//! Markdown frontend: front matter, slide splitting, attributes and block rendering.
//!
//! Contract: `docs/spec/markdown-dialect.md`. [`parse`] (T12) turns one `.md` file into a
//! [`Parsed`] deck:
//!
//! 1. comrak parses the text with [`comrak_options`] (dialect §1);
//! 2. the `FrontMatter` node goes through [`frontmatter::parse_front_matter`] into
//!    `Deck.meta`, and is merged into a clone of the config (`Config::merge_front_matter`), so
//!    front matter can switch `slides.split` or `markdown.*`; when it changes
//!    `markdown.smart_punctuation` the text is parsed again with the new options;
//! 3. [`split::split_with_source`] splits the document into slides;
//! 4. each slide is rendered by [`render::render_nodes`] (through [`compat::render_slide`],
//!    which splits off a `Note:` paragraph and what follows as notes); its attributes
//!    (separator, `<!-- .slide: -->` comments and lifted heading attributes) are applied with
//!    `SlideAttrs::apply_to`, an image-like `data-background` becomes
//!    `data-background-image` ([`compat::image_background`]), and relative background
//!    images/videos/iframes are registered ([`render::rewrite_slide_assets`]); the body and notes HTML then go through
//!    [`postprocess`] (`<!-- .element: -->` comments; incremental lists in the body when
//!    [`postprocess::incremental_enabled`] says so for the slide);
//! 5. `Deck.features` is the union of the slides' features and `Deck.assets` summarizes the
//!    registry. The splitter's and the renderer's diagnostics get the source path and are
//!    ordered by line.
//!
//! Missing local files never fail [`parse`]: they are `missing-asset` warnings (dialect §6;
//! ir.md §4 contract change). The pipeline decides whether they are fatal; the warning's
//! `path` is the deck, and [`render::missing_asset_path`] gives the missing file for
//! `Error::MissingAsset`.

pub mod attrs;
pub mod code;
pub mod compat;
pub mod frontmatter;
pub mod postprocess;
pub mod render;
pub mod split;

use std::path::Path;

use comrak::nodes::{AstNode, NodeValue};
use comrak::{Arena, Options, parse_document};

use crate::assets::registry::AssetRegistry;
use crate::config::{Config, MarkdownConfig, SlideSplit, SlidesConfig};
use crate::error::{Diagnostics, Error, Result};
use crate::ir::{Block, Deck, DeckMeta, Features, Parsed, Slide, SourceInfo};
use crate::markdown::render::{RenderCtx, RenderMode, rewrite_slide_assets};
use crate::markdown::split::{RawSlide, SplitMode, split_with_source};

/// Parses a Markdown deck into the IR (see the module docs for the steps).
///
/// Errors: `Error::FrontMatter` for invalid front matter (or front matter the config
/// rejects), `Error::Parse { path: source.path, line, .. }` for a separator attribute block
/// or `<!-- .slide: -->` comment with an unbalanced quote.
pub fn parse(source: &SourceInfo, text: &str, config: &Config) -> Result<Parsed> {
    let arena = Arena::new();
    let first_opts = comrak_options(&config.markdown);
    let mut root = parse_document(&arena, text, &first_opts);
    let meta = deck_meta(root)?;
    let mut deck_config = config.clone();
    deck_config.merge_front_matter(&meta)?;
    let opts = comrak_options(&deck_config.markdown);
    if opts.parse.smart != first_opts.parse.smart {
        root = parse_document(&arena, text, &opts);
    }
    let raw = split_with_source(root, text, split_mode(&deck_config.slides))
        .map_err(|err| with_source_path(err, &source.path))?;

    let mut assets = AssetRegistry::new();
    let mut diagnostics = raw.diagnostics;
    let mut features = Features::default();
    let mut slides = Vec::with_capacity(raw.slides.len());
    let mut ctx = RenderCtx {
        assets: &mut assets,
        base_dir: &source.base_dir,
        mode: RenderMode::Deck,
        diagnostics: &mut diagnostics,
        missing_asset_is_error: false,
    };
    let incremental = deck_config.slides.incremental_lists;
    let lines = split::source_lines(text);
    for (raw_slide, raw_children) in raw.slides {
        let mut slide = build_slide(&arena, raw_slide, &lines, &opts, &mut ctx, &mut features)?;
        postprocess_slide(&mut slide, incremental, ctx.diagnostics)?;
        for raw_child in raw_children {
            let mut child = build_slide(&arena, raw_child, &lines, &opts, &mut ctx, &mut features)?;
            postprocess_slide(&mut child, incremental, ctx.diagnostics)?;
            slide.children.push(child);
        }
        slides.push(slide);
    }

    let deck = Deck {
        meta,
        slides,
        features,
        source: source.clone(),
        assets: assets.infos(&source.base_dir),
    };
    Ok(Parsed {
        deck,
        assets,
        diagnostics: finish_diagnostics(diagnostics, &source.path),
    })
}

/// comrak options exactly as `docs/spec/markdown-dialect.md` §1, with
/// `parse.smart = config.smart_punctuation` and `render.sourcepos = config.sourcepos`.
pub fn comrak_options(config: &MarkdownConfig) -> Options<'static> {
    let mut o = Options::default();
    // extensions
    o.extension.front_matter_delimiter = Some("---".into());
    o.extension.table = true;
    o.extension.strikethrough = true;
    o.extension.tasklist = true;
    o.extension.autolink = true;
    o.extension.footnotes = true;
    o.extension.description_lists = true;
    o.extension.alerts = true;
    o.extension.math_dollars = true;
    o.extension.math_code = true;
    o.extension.block_directive = true;
    o.extension.header_attributes = true;
    o.extension.fenced_code_attributes = true;
    o.extension.link_attributes = true;
    o.extension.inline_code_attributes = true;
    // parse
    o.parse.smart = config.smart_punctuation;
    o.parse.relaxed_autolinks = false;
    // render
    o.render.r#unsafe = true;
    o.render.escape = false;
    o.render.hardbreaks = false;
    o.render.github_pre_lang = false;
    o.render.full_info_string = false;
    o.render.sourcepos = config.sourcepos;
    o
}

/// `Deck.meta` from the document's `FrontMatter` node (always its first child), or the
/// default when there is none.
fn deck_meta<'a>(root: &'a AstNode<'a>) -> Result<DeckMeta> {
    let Some(first) = root.first_child() else {
        return Ok(DeckMeta::default());
    };
    match &first.data().value {
        NodeValue::FrontMatter(literal) => {
            frontmatter::parse_front_matter(frontmatter::strip_delimiters(literal))
        }
        _ => Ok(DeckMeta::default()),
    }
}

fn split_mode(slides: &SlidesConfig) -> SplitMode {
    match slides.split {
        SlideSplit::Rules => SplitMode::Rules,
        SlideSplit::Headings => SplitMode::Headings {
            slide_level: slides.slide_level,
        },
    }
}

/// Renders one raw slide into an IR [`Slide`] (without children); `lines` is the source
/// split by [`split::source_lines`] (for `Note:` paragraphs, T43).
fn build_slide<'a>(
    arena: &'a Arena<'a>,
    raw: RawSlide<'a>,
    lines: &[&str],
    opts: &Options,
    ctx: &mut RenderCtx,
    features: &mut Features,
) -> Result<Slide> {
    let span = raw.span();
    let rendered = compat::render_slide(arena, &raw.nodes, Some(lines), opts, ctx)?;
    let mut slide = Slide::new();
    raw.attrs.apply_to(&mut slide);
    compat::image_background(&mut slide.attrs);
    rewrite_slide_assets(&mut slide.attrs, Some(&span), ctx)?;
    slide.title = rendered.title;
    slide.notes = rendered.notes;
    slide.span = Some(span);
    if !rendered.html.is_empty() {
        slide.blocks.push(Block::html(rendered.html));
    }
    features.merge(rendered.features);
    Ok(slide)
}

/// [`postprocess`] for a built slide (T43): `.element:` comments in its body and notes, and
/// incremental lists in its body when [`postprocess::incremental_enabled`] (`config` is
/// `slides.incremental_lists`).
fn postprocess_slide(slide: &mut Slide, config: bool, diagnostics: &mut Diagnostics) -> Result<()> {
    let incremental = postprocess::incremental_enabled(config, &slide.classes);
    let span = slide.span.clone();
    for block in &mut slide.blocks {
        if let Block::Html { html } = block {
            *html = postprocess::slide_html(html, incremental, span.as_ref(), diagnostics)?;
        }
    }
    if let Some(notes) = &mut slide.notes {
        *notes = postprocess::element_attributes(notes, span.as_ref(), diagnostics)?;
    }
    Ok(())
}

/// `split` reports separator errors without a path; fill in the source file.
fn with_source_path(err: Error, path: &Path) -> Error {
    match err {
        Error::Parse {
            path: empty,
            line,
            message,
        } if empty.as_os_str().is_empty() => Error::Parse {
            path: path.to_path_buf(),
            line,
            message,
        },
        other => other,
    }
}

/// Sets `path` on every diagnostic that has none and orders them by line (stable, so
/// diagnostics of one line keep their order; those without a span come first).
fn finish_diagnostics(diagnostics: Diagnostics, path: &Path) -> Diagnostics {
    let mut diagnostics = diagnostics.into_vec();
    for diagnostic in &mut diagnostics {
        diagnostic.path.get_or_insert_with(|| path.to_path_buf());
    }
    diagnostics.sort_by_key(|d| d.span.as_ref().map(|span| span.start_line));
    diagnostics.into()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::ir::SourceKind;

    fn fixture_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/md")
    }

    fn parse_text(text: &str) -> Parsed {
        let dir = fixture_dir();
        let source = SourceInfo {
            path: dir.join("unit.md"),
            kind: SourceKind::Markdown,
            base_dir: dir,
        };
        let parsed = parse(&source, text, &Config::default()).unwrap();
        assert_eq!(parsed.deck.invariant_violations(), Vec::<String>::new());
        parsed
    }

    fn html(slide: &Slide) -> &str {
        match slide.blocks.as_slice() {
            [Block::Html { html }] => html,
            other => panic!("expected one Html block, got {other:?}"),
        }
    }

    #[test]
    fn comrak_options_follow_the_dialect_exactly() {
        let o = comrak_options(&MarkdownConfig::default());
        let e = &o.extension;
        assert_eq!(e.front_matter_delimiter.as_deref(), Some("---"));
        assert!(e.table && e.strikethrough && e.tasklist && e.autolink && e.footnotes);
        assert!(e.description_lists && e.alerts && e.math_dollars && e.math_code);
        assert!(e.block_directive && e.header_attributes && e.fenced_code_attributes);
        assert!(e.link_attributes && e.inline_code_attributes);
        assert!(!o.parse.smart && !o.parse.relaxed_autolinks);
        assert!(o.render.r#unsafe && !o.render.escape && !o.render.hardbreaks);
        assert!(!o.render.github_pre_lang && !o.render.full_info_string && !o.render.sourcepos);
        let config = MarkdownConfig {
            smart_punctuation: true,
            sourcepos: true,
        };
        let o = comrak_options(&config);
        assert!(o.parse.smart && o.render.sourcepos);
    }

    #[test]
    fn footnotes_are_numbered_and_closed_per_slide() {
        let text = "\
# One

Text[^a] and[^b], again[^a].

[^a]: A note.
[^b]: B note.

---

# Two

More[^c].

[^c]: C note.
";
        let parsed = parse_text(text);
        let slides = &parsed.deck.slides;
        assert_eq!(slides.len(), 2);
        let first = html(&slides[0]);
        assert!(
            first.contains(r##"<a href="#fn-a" id="fnref-a" data-footnote-ref>1</a>"##),
            "{first}"
        );
        assert!(first.contains(r##"<a href="#fn-b" id="fnref-b" data-footnote-ref>2</a>"##));
        assert!(first.contains(r##"<a href="#fn-a" id="fnref-a-2" data-footnote-ref>1</a>"##));
        assert!(first.ends_with("</ol>\n</div>\n"), "{first}");
        let second = html(&slides[1]);
        assert!(second.starts_with("<h1>Two</h1>\n"));
        assert!(
            second.contains(r##"<a href="#fn-c" id="fnref-c" data-footnote-ref>1</a>"##),
            "{second}"
        );
        assert!(
            second.contains("<div class=\"footnotes\" data-footnotes>\n<ol>\n<li id=\"fn-c\">")
        );
        assert!(second.contains("data-footnote-backref-idx=\"1\""));
        assert!(second.ends_with("</ol>\n</div>\n"));
        assert!(parsed.diagnostics.is_empty());
    }

    /// reveal.js turns a slide that contains a `<section>` into a vertical stack, so nothing
    /// the renderer writes may contain one (comrak's footnote list is a `<section>`).
    #[test]
    fn slides_and_notes_never_contain_a_section() {
        let text = "\
# One

A claim[^src].

::::fragment
Step.
::::

:::notes
Speaker text[^n].

[^n]: A note's footnote.
:::

[^src]: Smith 2020.
";
        let parsed = parse_text(text);
        let slide = &parsed.deck.slides[0];
        let body = html(slide);
        let notes = slide.notes.as_deref().unwrap();
        for html in [body, notes] {
            assert!(!html.contains("<section"), "{html}");
        }
        assert!(body.ends_with(
            "<div class=\"footnotes\" data-footnotes>\n<ol>\n<li id=\"fn-src\">\n<p>Smith 2020. \
             <a href=\"#fnref-src\" class=\"footnote-backref\" data-footnote-backref \
             data-footnote-backref-idx=\"1\" aria-label=\"Back to reference 1\">↩</a></p>\n\
             </li>\n</ol>\n</div>\n"
        ));
        // the definition written inside the notes block stays in the notes
        assert!(!body.contains("A note's footnote"), "{body}");
        assert!(notes.starts_with(
            "<p>Speaker text<sup class=\"footnote-ref\"><a href=\"#fn-n\" id=\"fnref-n\" \
             data-footnote-ref>1</a></sup>.</p>\n<div class=\"footnotes\" data-footnotes>\n"
        ));
        assert!(notes.contains("<li id=\"fn-n\">\n<p>A note's footnote. <a href=\"#fnref-n\""));
        assert!(parsed.diagnostics.is_empty());
    }

    #[test]
    fn footnote_labels_can_be_reused_on_every_slide() {
        let text = "# One\n\nA[^1]\n\n[^1]: one\n\n---\n\n# Two\n\nB[^1]\n\n[^1]: two\n";
        let parsed = parse_text(text);
        let (first, second) = (html(&parsed.deck.slides[0]), html(&parsed.deck.slides[1]));
        // comrak resolves `1` to the last definition (`fn-1`); the other one is `fn<line>-1`
        assert!(first.contains(r##"<a href="#fn5-1" id="fnref-1" data-footnote-ref>1</a>"##));
        assert!(
            first.contains(
                "<li id=\"fn5-1\">\n<p>one <a href=\"#fnref-1\" class=\"footnote-backref\""
            )
        );
        assert!(second.contains(r##"<a href="#fn-1" id="fnref-1-2" data-footnote-ref>1</a>"##));
        assert!(second.contains(
            "<li id=\"fn-1\">\n<p>two <a href=\"#fnref-1-2\" class=\"footnote-backref\" \
             data-footnote-backref data-footnote-backref-idx=\"1\" \
             aria-label=\"Back to reference 1\">↩</a></p>"
        ));
        // ids are unique across the deck and every back-link stays on its slide
        let ids = regex::Regex::new(r#" id="([^"]+)""#).unwrap();
        let mut all: Vec<&str> = [first, second]
            .iter()
            .flat_map(|html| ids.captures_iter(html).map(|c| c.get(1).unwrap().as_str()))
            .collect();
        let count = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), count, "duplicate ids: {first}{second}");
        assert_eq!(first.matches("footnote-backref\"").count(), 1);
        assert_eq!(second.matches("footnote-backref\"").count(), 1);
        assert!(parsed.diagnostics.is_empty());
    }

    #[test]
    fn footnotes_defined_elsewhere_or_twice_are_reported() {
        let text = "\
# One

A[^1] and[^1].

[^1]: one

---

B[^1], C[^2].

[^2]: first
[^2]: second
";
        let parsed = parse_text(text);
        let second = html(&parsed.deck.slides[1]);
        // the reference with no definition here links to comrak's definition on slide 1
        assert!(second.contains(r##"<a href="#fn-1" id="fnref-1-3" data-footnote-ref>1</a>"##));
        assert!(second.contains(r##"<a href="#fn11-2" id="fnref-2" data-footnote-ref>2</a>"##));
        // slide 1 lists only its own two references
        let first = html(&parsed.deck.slides[0]);
        assert!(first.contains("href=\"#fnref-1-2\""));
        assert!(!first.contains("href=\"#fnref-1-3\""));
        let found: Vec<_> = parsed
            .diagnostics
            .iter()
            .map(|d| (d.code, d.span.as_ref().map(|s| s.start_line)))
            .collect();
        assert_eq!(
            found,
            [
                ("footnote-elsewhere", Some(9)),
                ("duplicate-footnote", Some(12))
            ]
        );
    }

    #[test]
    fn footnote_definitions_follow_their_first_reference_on_the_slide() {
        // `b` is referenced first on slide 2 but written on slide 1's lines after `a`
        let text = "[^a] text\n\n[^a]: A.\n\n---\n\n[^b] then [^a]\n\n[^b]: B.\n";
        let parsed = parse_text(text);
        let second = html(&parsed.deck.slides[1]);
        assert!(second.contains(">1</a></sup> then <sup"), "{second}");
        assert!(
            second.contains(r##"id="fnref-a-2" data-footnote-ref>2</a>"##),
            "{second}"
        );
    }

    #[test]
    fn diagnostics_have_the_path_and_are_ordered_by_line() {
        let text = "\
# A

![x](gone.png)

--- {background=missing-bg.png unknown=1}

:::box 9=x
y
:::

---
";
        let parsed = parse_text(text);
        let found: Vec<_> = parsed
            .diagnostics
            .iter()
            .map(|d| (d.code, d.span.as_ref().map(|s| s.start_line)))
            .collect();
        assert_eq!(
            found,
            [
                ("missing-asset", Some(3)),
                ("unknown-slide-attr", Some(5)),
                ("missing-asset", Some(5)),
                ("invalid-directive-arg", Some(7)),
                ("empty-slide", Some(11)),
            ]
        );
        let path = fixture_dir().join("unit.md");
        assert!(
            parsed
                .diagnostics
                .iter()
                .all(|d| d.path.as_ref() == Some(&path))
        );
    }

    #[test]
    fn slide_backgrounds_are_registered() {
        let text = "# A\n\n--- {background-video=\"images/ok.png,https://e.org/v.mp4\" background-iframe=images/ok.png}\n\n# B\n";
        let parsed = parse_text(text);
        let id = AssetRegistry::new().register_file(&fixture_dir().join("images/ok.png"));
        let attrs = &parsed.deck.slides[1].attrs;
        assert_eq!(
            attrs["data-background-video"],
            format!("asset:{id},https://e.org/v.mp4")
        );
        assert_eq!(attrs["data-background-iframe"], format!("asset:{id}"));
        assert_eq!(parsed.deck.assets.len(), 1);
        assert!(parsed.diagnostics.is_empty());
    }

    #[test]
    fn empty_documents_have_one_empty_slide() {
        for text in ["", "---\ntitle: Only front matter\n---\n"] {
            let parsed = parse_text(text);
            assert_eq!(parsed.deck.slides.len(), 1);
            let slide = &parsed.deck.slides[0];
            assert!(slide.blocks.is_empty() && slide.title.is_none() && slide.notes.is_none());
            assert_eq!(parsed.deck.features, Features::default());
        }
        let parsed = parse_text("---\ntitle: Only front matter\n---\n");
        assert_eq!(parsed.deck.meta.title.as_deref(), Some("Only front matter"));
    }

    #[test]
    fn notes_only_slides_have_no_blocks() {
        let parsed = parse_text("# A\n\n---\n\n:::notes\nJust notes.\n:::\n");
        let slide = &parsed.deck.slides[1];
        assert!(slide.blocks.is_empty());
        assert_eq!(slide.notes.as_deref(), Some("<p>Just notes.</p>\n"));
        assert!(parsed.deck.features.notes);
    }
}
