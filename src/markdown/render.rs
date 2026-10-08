//! Markdown block renderer: directives, notes, math, images → assets. Implemented by T12.
//!
//! Contract: `docs/spec/markdown-dialect.md` §3.3–§9. [`render_nodes`] renders one slide (a
//! group of top-level comrak nodes) and [`render_fragment`] parses and renders a standalone
//! Markdown string (notebook cells, `text/markdown` outputs, the `markdown` template filter);
//! [`render_fragment_with_lift`] also lifts a leading heading's attributes (notebook cells).
//!
//! The nodes are moved under a synthetic `Document` node, so lists and footnotes render as a
//! unit (the footnote list of a slide is closed at the end of that slide). Then:
//!
//! 1. **title**: [`first_heading_text`] of the nodes, before anything is changed;
//! 2. **notes** (§4, §7): every outermost `:::notes` directive, wherever it is nested, is
//!    detached; its children (and the footnote definitions written inside it, which comrak
//!    moved to the end of the document) are rendered through a second synthetic document with
//!    the same passes and appended to [`Rendered::notes`] in document order;
//! 3. **checks**: a paragraph that starts like a directive (`:::name …`) but is not one is an
//!    `invalid-directive` warning: comrak does not open a directive whose line has a `:`
//!    after the fence (`title="a:b"`, a URL); `&#58;` is the way to write that colon;
//! 4. **assets** (§6): relative image URLs and relative `src` / `poster` attributes of raw
//!    HTML start tags ([`rewrite_html_assets`]) are resolved against [`RenderCtx::base_dir`]:
//!    an existing file is registered ([`AssetRegistry::register_file`]) and the URL becomes
//!    `asset:<id>` (a `?query` or `#fragment` is kept after it); a missing file is a
//!    `missing-asset` warning (the URL stays as written; [`missing_asset_path`] reads the
//!    file back from it) or [`Error::MissingAsset`] when [`RenderCtx::missing_asset_is_error`].
//!    Raw HTML is tokenized like a browser does: comments, the content of raw-text elements
//!    (`<script>`, `<style>`, …) and text inside other attributes' quoted values never count,
//!    also when a `<script>` opens in one raw HTML node and closes in a later one;
//! 5. **footnotes**: every slide (and every notes block) has its own footnote list.
//!    References are numbered per slide in order of first use (comrak numbers them across the
//!    whole document) and link to the definition *on the same slide*, so a label such as
//!    `[^1]` can be reused on every slide; the definitions move to the end, ordered to match,
//!    with back-links to this slide's references only. Ids stay unique in the deck: the
//!    definition comrak resolved a label to keeps `fn-<label>`, any other definition of the
//!    label (on another slide) gets `fn<line>-<label>`; reference ids are comrak's
//!    (`fnref-<label>[-<n>]`). A label defined twice on one slide is a `duplicate-footnote`
//!    warning, a reference with no definition on its slide a `footnote-elsewhere` warning
//!    (it links to comrak's definition). The list is a `<div class="footnotes"
//!    data-footnotes><ol>…</ol></div>`, never comrak's `<section>`: reveal.js would turn a
//!    slide containing a `<section>` into a vertical stack;
//! 6. **HTML**: comrak's formatter with a small override (`format_node`) for the nodes the
//!    dialect renders differently from comrak:
//!    - `BlockDirective` (§4): `<div class="name classes…" …>` from a quote-aware split of the
//!      info string: `:::fragment … index=N` adds `data-fragment-index="N"`,
//!      `::::columns widths="1fr 2fr"` adds `style="grid-template-columns: 1fr 2fr"`,
//!      `:::column width=…` renders `<div class="column">` (the width is consumed), and every
//!      other `key=value` arg becomes `data-<key>="value"`;
//!    - `CodeBlock` (§5): [`render_code_block`](crate::markdown::code::render_code_block);
//!      a ```` ```math ```` fence becomes `<div data-math-style="display">TeX</div>`;
//!    - `Math` (§8): `<span data-math-style="inline|display">TeX</span>` (also for code math);
//!    - headings, links, images and inline code with `{…}` attributes (§3.3): comrak's own
//!      tag with `id`, `class` and `key="value"` appended (comrak's formatter never writes
//!      node attributes). Doing this in the formatter rather than by rewriting nodes to raw
//!      HTML keeps the inline children (emphasis, alt text, …) rendered by comrak;
//!    - footnote references and definitions (pass 5).
//!
//! Every attribute value written here is escaped with `encode_double_quoted_attribute`.
//! [`Rendered::features`] reports `code` (any code block other than a math fence), `math`
//! (an element with a `data-math-style` attribute in the HTML or notes, see
//! [`has_math_markup`]; the text `data-math-style` in inline code does not count) and `notes`.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::convert::Infallible;
use std::fmt::{self, Write as _};
use std::ops::Range;
use std::path::{Path, PathBuf};

use comrak::html::{
    ChildRendering, Context, dangerous_url, format_document_with_formatter, format_node_default,
    render_sourcepos,
};
use comrak::nodes::{
    AstNode, Attributes, NodeCodeBlock, NodeFootnoteDefinition, NodeFootnoteReference, NodeLink,
    NodeMath, NodeValue,
};
use comrak::options::Plugins;
use comrak::{Arena, Options, parse_document};
use html_escape::{decode_html_entities, encode_quoted_attribute};

use crate::assets::registry::{AssetRegistry, normalize_lexically};
use crate::error::{Diagnostic, Diagnostics, Error, Result};
use crate::ir::{Features, SourceSpan};
use crate::markdown::attrs::{ParsedAttrs, is_valid_attr_name};
use crate::markdown::code::{
    code_attrs, push_attr, render_code_block_with, strip_trailing_newline,
};
use crate::markdown::split::{first_heading_text, lift_heading_attrs};

/// The attribute every math element carries (`Features.math` is derived from it).
const MATH_ATTR: &str = "data-math-style";

/// Diagnostic code of a missing local file.
const MISSING_ASSET: &str = "missing-asset";

/// Separates the URL as written from the resolved path in a `missing-asset` message.
const LOOKED_FOR: &str = " (looked for ";

/// What is being rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// A Markdown deck (slides of a `.md` file) or the `markdown` template filter.
    Deck,
    /// A notebook markdown cell or `text/markdown` output: [`render_fragment`] parses without
    /// front matter (the notebook frontend strips the first cell's front matter itself), so a
    /// leading `---` block is ordinary Markdown.
    Notebook,
}

/// Everything the renderer needs besides the nodes: where assets go and resolve from, and
/// where diagnostics go.
#[derive(Debug)]
pub struct RenderCtx<'r> {
    /// Registry that local images are registered in.
    pub assets: &'r mut AssetRegistry,
    /// Directory relative URLs resolve against (`SourceInfo.base_dir`).
    pub base_dir: &'r Path,
    /// Deck or notebook rendering.
    pub mode: RenderMode,
    /// Receives `missing-asset`, `invalid-directive`, `invalid-directive-arg`,
    /// `duplicate-footnote` and `footnote-elsewhere` warnings. Spans are lines of the rendered
    /// text (`cell: None`) and the path is not set: the caller adds both. The missing file of
    /// a `missing-asset` warning is in its message only: see [`missing_asset_path`].
    pub diagnostics: &'r mut Diagnostics,
    /// Return [`Error::MissingAsset`] for a missing local file instead of a warning.
    pub missing_asset_is_error: bool,
}

/// One rendered slide or fragment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rendered {
    /// The body HTML (notes removed). Empty when there is no content.
    pub html: String,
    /// The `:::notes` blocks rendered to HTML and concatenated; `None` when there are none
    /// (or they are all empty).
    pub notes: Option<String>,
    /// `code`, `math` and `notes` as found in this content (`notebook` is never set here).
    pub features: Features,
    /// Plain text of the first heading ([`first_heading_text`]).
    pub title: Option<String>,
}

/// Render a group of top-level nodes (one slide) to HTML. Detaches `:::notes`, rewrites
/// directives/code/images, formats with comrak.
///
/// The nodes are moved (detached) under a new `Document` node allocated in `arena`, so
/// afterwards they no longer belong to their original document. Heading attributes are
/// *not* lifted here: callers that lift them onto a slide do so before calling this (the
/// splitter already did for Markdown decks), and attributes still on a heading are written
/// on the element.
///
/// Errors: [`Error::MissingAsset`] for a missing local image when
/// `ctx.missing_asset_is_error`.
pub fn render_nodes<'a>(
    arena: &'a Arena<'a>,
    nodes: &[&'a AstNode<'a>],
    opts: &Options,
    ctx: &mut RenderCtx,
) -> Result<Rendered> {
    let title = first_heading_text(nodes);
    let doc = new_document(arena);
    for node in nodes {
        doc.append(node);
    }
    let mut rendered = render_document(arena, doc, opts, ctx)?;
    rendered.title = title;
    Ok(rendered)
}

/// Parse + render a standalone Markdown string (notebook cells, text/markdown outputs, filter).
///
/// Never splits into slides: separators render as ordinary Markdown (`<hr />`). In
/// [`RenderMode::Notebook`] the text is parsed without the front matter extension; in
/// [`RenderMode::Deck`] `opts` are used as given and a front matter block, if `opts` enable
/// it, is dropped. `:::notes` blocks are extracted into [`Rendered::notes`] in both modes.
/// Heading attributes are written on the heading; [`render_fragment_with_lift`] can lift them.
/// Errors as [`render_nodes`].
pub fn render_fragment(text: &str, opts: &Options, ctx: &mut RenderCtx) -> Result<Rendered> {
    render_fragment_with_lift(text, opts, ctx, false).map(|(rendered, _)| rendered)
}

/// [`render_fragment`], and with `lift_heading` the attributes of a leading heading are
/// lifted first (notebook-mapping §2: a markdown cell that starts with
/// `# Title {layout=title}` while the current slide is still empty gives them to the slide).
///
/// With `lift_heading`, [`lift_heading_attrs`] runs on the fragment's top-level nodes (front
/// matter excluded) before rendering: when the first node is a heading with `{…}` attributes,
/// they are removed from it (the heading renders clean) and returned for the caller to apply
/// (`SlideAttrs::from_parsed(attrs).apply_to(slide)`). Otherwise, and always without
/// `lift_heading`, the second value is `None` and the attributes stay on the `<hN>` element.
/// Errors as [`render_nodes`].
pub fn render_fragment_with_lift(
    text: &str,
    opts: &Options,
    ctx: &mut RenderCtx,
    lift_heading: bool,
) -> Result<(Rendered, Option<ParsedAttrs>)> {
    let arena = Arena::new();
    let root = match ctx.mode {
        RenderMode::Deck => parse_document(&arena, text, opts),
        RenderMode::Notebook => {
            let mut notebook_opts = opts.clone();
            notebook_opts.extension.front_matter_delimiter = None;
            parse_document(&arena, text, &notebook_opts)
        }
    };
    let nodes: Vec<_> = root
        .children()
        .filter(|node| !matches!(node.data().value, NodeValue::FrontMatter(_)))
        .collect();
    let lifted = if lift_heading {
        lift_heading_attrs(&nodes)
    } else {
        None
    };
    let rendered = render_nodes(&arena, &nodes, opts, ctx)?;
    Ok((rendered, lifted))
}

/// The missing file a `missing-asset` diagnostic of this module is about (resolved, normally
/// absolute), or `None` for any other diagnostic.
///
/// Such a diagnostic's `path` is the *source* file (the deck or notebook, set by the
/// frontend) and its span the referencing lines; the missing file is only in the message,
/// whose format is frozen for this function: ``<what> `<url as written>` not found (looked
/// for <resolved path>)``. The pipeline escalates with
/// `Error::MissingAsset { path: missing_asset_path(d)?, span: d.span.clone() }`.
pub fn missing_asset_path(diagnostic: &Diagnostic) -> Option<PathBuf> {
    if diagnostic.code != MISSING_ASSET {
        return None;
    }
    let (_, path) = diagnostic
        .message
        .strip_suffix(')')?
        .rsplit_once(LOOKED_FOR)?;
    Some(PathBuf::from(path))
}

/// Whether `html` contains an element with a `data-math-style` attribute (dialect §8), i.e.
/// whether KaTeX is needed. Only start tags count, outside comments and raw-text elements, so
/// the text `data-math-style` in inline code, in an escaped code sample or in another
/// attribute's value does not. [`Rendered::features`]`.math` is derived from this.
pub fn has_math_markup(html: &str) -> bool {
    let mut found = false;
    let Ok(()) = HtmlScan::default().start_tags(html, |tag| {
        found |= tag.attrs.iter().any(|attr| attr.name == MATH_ATTR);
        Ok::<(), Infallible>(())
    });
    found
}

/// Registers the local files named by a slide's `data-background-image`,
/// `data-background-video` (a comma-separated list; each entry is resolved) and
/// `data-background-iframe` attributes (dialect §6) and replaces them with `asset:<id>`.
/// Missing files: a `missing-asset` warning with `span` (the value stays as written), or
/// [`Error::MissingAsset`] when `ctx.missing_asset_is_error`. Remote and root-absolute
/// values are left alone.
pub fn rewrite_slide_assets(
    attrs: &mut BTreeMap<String, String>,
    span: Option<&SourceSpan>,
    ctx: &mut RenderCtx,
) -> Result<()> {
    for key in [
        "data-background-image",
        "data-background-video",
        "data-background-iframe",
    ] {
        let Some(value) = attrs.get(key) else {
            continue;
        };
        let parts: Vec<String> = if key == "data-background-video" {
            value.split(',').map(str::to_string).collect()
        } else {
            vec![value.clone()]
        };
        let mut changed = false;
        let mut rewritten = Vec::with_capacity(parts.len());
        for part in parts {
            match local_asset(part.trim(), "slide background", span, ctx)? {
                Some(url) => {
                    changed = true;
                    rewritten.push(url);
                }
                None => rewritten.push(part),
            }
        }
        if changed {
            attrs.insert(key.to_string(), rewritten.join(","));
        }
    }
    Ok(())
}

/// Rewrites relative `src` and `poster` attribute values (double-, single- or unquoted) in a
/// raw HTML snippet like images (dialect §6): existing files become `asset:<id>`, missing
/// ones are reported. Only attributes of start tags count, found by an HTML-style tokenizer:
/// text, `<!-- … -->` comments, the content of raw-text elements (`<script>`, `<style>`,
/// `<textarea>`, `<title>`, `<iframe>`, …) and text inside another attribute's quoted value
/// (`alt="x src=y.png"`) are left alone. Values are HTML-entity-decoded before they are
/// resolved. Errors as [`rewrite_slide_assets`].
pub fn rewrite_html_assets(
    html: &mut String,
    span: Option<&SourceSpan>,
    ctx: &mut RenderCtx,
) -> Result<()> {
    rewrite_html_snippet(html, span, ctx, &mut HtmlScan::default())
}

/// [`rewrite_html_assets`] for one of several raw HTML nodes scanned in document order:
/// `scan` carries an open comment or raw-text element from one node to the next.
fn rewrite_html_snippet(
    html: &mut String,
    span: Option<&SourceSpan>,
    ctx: &mut RenderCtx,
    scan: &mut HtmlScan,
) -> Result<()> {
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let source: &str = html;
    scan.start_tags(source, |tag| {
        for attr in &tag.attrs {
            let Some(range) = attr.value.clone() else {
                continue;
            };
            if !matches!(attr.name.as_str(), "src" | "poster") {
                continue;
            }
            let url = decode_html_entities(&source[range.clone()]);
            if let Some(asset) = local_asset(&url, "file", span, ctx)? {
                edits.push((range, encode_quoted_attribute(&asset).into_owned()));
            }
        }
        Ok::<(), Error>(())
    })?;
    for (range, text) in edits.into_iter().rev() {
        html.replace_range(range, &text);
    }
    Ok(())
}

/// Elements whose content is raw text (or escapable raw text) for the HTML tokenizer: tags
/// inside them are not tags.
const RAW_TEXT_ELEMENTS: [&str; 8] = [
    "script", "style", "textarea", "title", "xmp", "iframe", "noembed", "noframes",
];

/// Where an HTML scan stands between snippets. The raw HTML nodes of a document are scanned
/// one after the other, and a comment or `<script>` opened in one can close in a later one
/// (`x <script>…'<img src=a.png>'…</script>` is three inline HTML nodes).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum HtmlScan {
    /// Ordinary content: tags are recognized.
    #[default]
    Data,
    /// Inside `<!-- … -->`.
    Comment,
    /// Inside a raw-text element (one of [`RAW_TEXT_ELEMENTS`]) until its end tag.
    RawText(&'static str),
}

/// A start tag found by [`HtmlScan::start_tags`].
#[derive(Debug)]
struct StartTag {
    /// The tag name, ASCII-lowercased.
    name: String,
    /// Its attributes in order.
    attrs: Vec<TagAttr>,
    /// Byte offset just after the tag's `>` (or the end of the snippet).
    end: usize,
}

/// One attribute of a [`StartTag`].
#[derive(Debug)]
struct TagAttr {
    /// The name, ASCII-lowercased.
    name: String,
    /// Byte range of the value in the snippet, quotes excluded; `None` for a bare attribute.
    value: Option<Range<usize>>,
}

impl HtmlScan {
    /// Calls `on_tag` for every start tag of `html` that is outside comments and raw-text
    /// elements, then leaves `self` in the state the snippet ends in. End tags, `<!…>`
    /// declarations and `<?…>` are skipped; a `<` that starts none of these is text.
    fn start_tags<E>(
        &mut self,
        html: &str,
        mut on_tag: impl FnMut(&StartTag) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        let bytes = html.as_bytes();
        let mut at = 0;
        while at < html.len() {
            match *self {
                HtmlScan::Comment => match html[at..].find("-->") {
                    Some(end) => {
                        at += end + "-->".len();
                        *self = HtmlScan::Data;
                    }
                    None => break,
                },
                HtmlScan::RawText(name) => match raw_text_end(html, at, name) {
                    Some(end) => {
                        at = end;
                        *self = HtmlScan::Data;
                    }
                    None => break,
                },
                HtmlScan::Data => {
                    let Some(lt) = html[at..].find('<') else {
                        break;
                    };
                    at += lt;
                    let rest = &html[at..];
                    if let Some(empty) = ["<!-->", "<!--->"].iter().find(|c| rest.starts_with(**c))
                    {
                        at += empty.len();
                    } else if rest.starts_with("<!--") {
                        at += "<!--".len();
                        *self = HtmlScan::Comment;
                    } else if bytes.get(at + 1).is_some_and(u8::is_ascii_alphabetic) {
                        let tag = start_tag(html, at);
                        on_tag(&tag)?;
                        if let Some(raw) = RAW_TEXT_ELEMENTS.iter().find(|raw| **raw == tag.name) {
                            *self = HtmlScan::RawText(raw);
                        }
                        at = tag.end;
                    } else if matches!(bytes.get(at + 1), Some(b'/' | b'!' | b'?')) {
                        at = html[at..].find('>').map_or(html.len(), |gt| at + gt + 1);
                    } else {
                        at += 1;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Byte offset of the `</name` that ends a raw-text element (searching from `from`).
fn raw_text_end(html: &str, from: usize, name: &str) -> Option<usize> {
    let bytes = html.as_bytes();
    html[from..]
        .match_indices("</")
        .map(|(offset, _)| from + offset)
        .find(|&lt| {
            let after = &bytes[lt + 2..];
            after.len() >= name.len()
                && after[..name.len()].eq_ignore_ascii_case(name.as_bytes())
                && after
                    .get(name.len())
                    .is_none_or(|b| b.is_ascii_whitespace() || matches!(b, b'/' | b'>'))
        })
}

/// Tokenizes the start tag whose `<` is at byte `lt` (HTML tokenizer rules: attribute names
/// end at whitespace, `/`, `>` or `=`; a value is quoted or runs to whitespace or `>`).
fn start_tag(html: &str, lt: usize) -> StartTag {
    let bytes = html.as_bytes();
    let ends_name = |b: u8| b.is_ascii_whitespace() || matches!(b, b'/' | b'>');
    let mut at = lt + 1;
    while at < bytes.len() && !ends_name(bytes[at]) {
        at += 1;
    }
    let name = html[lt + 1..at].to_ascii_lowercase();
    let mut attrs = Vec::new();
    loop {
        while at < bytes.len() && (bytes[at].is_ascii_whitespace() || bytes[at] == b'/') {
            at += 1;
        }
        match bytes.get(at) {
            None => {
                return StartTag {
                    name,
                    attrs,
                    end: at,
                };
            }
            Some(b'>') => {
                return StartTag {
                    name,
                    attrs,
                    end: at + 1,
                };
            }
            Some(_) => {}
        }
        let name_start = at;
        at += 1; // the first character of a name may be `=`
        while at < bytes.len() && !ends_name(bytes[at]) && bytes[at] != b'=' {
            at += 1;
        }
        let attr_name = html[name_start..at].to_ascii_lowercase();
        let (value, next) = attr_value(bytes, at);
        at = next;
        attrs.push(TagAttr {
            name: attr_name,
            value,
        });
    }
}

/// The value after an attribute name that ends at byte `at`: its byte range (quotes
/// excluded) when `=` follows, and where tokenizing continues.
fn attr_value(bytes: &[u8], at: usize) -> (Option<Range<usize>>, usize) {
    let skip_space = |mut i: usize| {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        i
    };
    let eq = skip_space(at);
    if bytes.get(eq) != Some(&b'=') {
        return (None, at);
    }
    let start = skip_space(eq + 1);
    match bytes.get(start) {
        Some(&quote @ (b'"' | b'\'')) => {
            let end = bytes[start + 1..]
                .iter()
                .position(|&b| b == quote)
                .map_or(bytes.len(), |p| start + 1 + p);
            (Some(start + 1..end), (end + 1).min(bytes.len()))
        }
        _ => {
            let end = bytes[start..]
                .iter()
                .position(|&b| b.is_ascii_whitespace() || b == b'>')
                .map_or(bytes.len(), |p| start + p);
            (Some(start..end), end)
        }
    }
}

/// Notes, assets, footnotes and formatting for one synthetic document (see the module docs).
fn render_document<'a>(
    arena: &'a Arena<'a>,
    doc: &'a AstNode<'a>,
    opts: &Options,
    ctx: &mut RenderCtx,
) -> Result<Rendered> {
    let (notes, mut features) = extract_notes(arena, doc, opts, ctx)?;
    check_unopened_directives(doc, ctx);
    rewrite_assets(doc, ctx)?;
    let footnotes = prepare_footnotes(doc, ctx);
    features.code |= doc.descendants().any(|node| {
        matches!(&node.data().value, NodeValue::CodeBlock(block) if !is_math_fence(&block.info))
    });
    let html = format_html(doc, opts, ctx, footnotes)?;
    features.math |= has_math_markup(&html);
    features.notes |= notes.is_some();
    Ok(Rendered {
        html,
        notes,
        features,
        title: None,
    })
}

/// Detaches the outermost `:::notes` directives under `doc` and renders their contents
/// (nested notes are rendered through the same passes and appended after their parent).
/// Footnote definitions written inside a notes block go with it: comrak moved them to the
/// end of the document, so they are found among `doc`'s children by their source line.
fn extract_notes<'a>(
    arena: &'a Arena<'a>,
    doc: &'a AstNode<'a>,
    opts: &Options,
    ctx: &mut RenderCtx,
) -> Result<(Option<String>, Features)> {
    let blocks: Vec<_> = doc
        .descendants()
        .filter(|node| {
            is_notes(node) && !node.parent().is_some_and(|p| p.ancestors().any(is_notes))
        })
        .collect();
    let mut html = String::new();
    let mut features = Features::default();
    for block in blocks {
        block.detach();
        let notes_doc = new_document(arena);
        while let Some(child) = block.first_child() {
            notes_doc.append(child);
        }
        let pos = block.data().sourcepos;
        let lines = pos.start.line..=pos.end.line;
        let definitions: Vec<_> = doc
            .children()
            .filter(|node| {
                let data = node.data();
                matches!(data.value, NodeValue::FootnoteDefinition(_))
                    && lines.contains(&data.sourcepos.start.line)
            })
            .collect();
        for definition in definitions {
            notes_doc.append(definition);
        }
        let rendered = render_document(arena, notes_doc, opts, ctx)?;
        html.push_str(&rendered.html);
        html.push_str(rendered.notes.as_deref().unwrap_or_default());
        features.merge(rendered.features);
    }
    let notes = (!html.trim().is_empty()).then_some(html);
    Ok((notes, features))
}

fn is_notes(node: &AstNode<'_>) -> bool {
    matches!(&node.data().value,
        NodeValue::BlockDirective(directive) if directive.info.split_whitespace().next() == Some("notes"))
}

/// Pass 3: a paragraph whose text starts with `:::name` is a directive comrak did not open
/// (module docs): an `invalid-directive` warning on its first line.
fn check_unopened_directives<'a>(doc: &'a AstNode<'a>, ctx: &mut RenderCtx) {
    for node in doc.descendants() {
        if !matches!(node.data().value, NodeValue::Paragraph) {
            continue;
        }
        let Some(first) = node.first_child() else {
            continue;
        };
        let name = match &first.data().value {
            NodeValue::Text(text) => {
                let line = text.lines().next().unwrap_or_default();
                let rest = line.trim_start_matches(':');
                if line.len() - rest.len() < 3 {
                    continue;
                }
                let name: String = rest
                    .trim_start()
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
                    .collect();
                name
            }
            _ => continue,
        };
        if !name.starts_with(char::is_alphabetic) {
            continue;
        }
        let message = format!(
            "`:::{name} …` is not a directive: comrak does not open one when the line has a \
             `:` after the fence (a URL, `title=\"a:b\"`); write the colon as `&#58;`"
        );
        let span = node_span(node).map(|span| SourceSpan {
            end_line: span.start_line,
            ..span
        });
        ctx.diagnostics.warn("invalid-directive", message).span = span;
    }
}

/// Pass 4: images and raw HTML `src`/`poster` (module docs).
fn rewrite_assets<'a>(doc: &'a AstNode<'a>, ctx: &mut RenderCtx) -> Result<()> {
    let mut scan = HtmlScan::default();
    for node in doc.descendants() {
        let span = node_span(node);
        let mut data = node.data_mut();
        match &mut data.value {
            NodeValue::Image(link) => {
                if let Some(url) = local_asset(&link.url, "image", span.as_ref(), ctx)? {
                    link.url = url;
                }
            }
            NodeValue::HtmlBlock(block) => {
                rewrite_html_snippet(&mut block.literal, span.as_ref(), ctx, &mut scan)?;
            }
            NodeValue::HtmlInline(literal) => {
                rewrite_html_snippet(literal, span.as_ref(), ctx, &mut scan)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Resolves one URL (dialect §6). `Some("asset:<id>…")` when it names an existing local
/// file; `None` when it is not relative or the file is missing (then reported).
fn local_asset(
    url: &str,
    what: &str,
    span: Option<&SourceSpan>,
    ctx: &mut RenderCtx,
) -> Result<Option<String>> {
    if !is_relative_url(url) {
        return Ok(None);
    }
    let (path, suffix) = url.split_at(url.find(['?', '#']).unwrap_or(url.len()));
    let file = ctx.base_dir.join(percent_decode(path));
    if file.is_file() {
        let id = ctx.assets.register_file(&file);
        return Ok(Some(format!("{}{suffix}", AssetRegistry::placeholder(&id))));
    }
    let resolved = normalize_lexically(&file);
    if ctx.missing_asset_is_error {
        return Err(Error::MissingAsset {
            path: resolved,
            span: span.cloned(),
        });
    }
    // The format is frozen: `missing_asset_path` parses it.
    let message = format!(
        "{what} `{url}` not found{LOOKED_FOR}{})",
        resolved.display()
    );
    ctx.diagnostics.warn(MISSING_ASSET, message).span = span.cloned();
    Ok(None)
}

/// A local reference: not empty, no URL scheme (`https:`, `data:`, `asset:`, …), not
/// root-absolute or protocol-relative (`/…`, `//…`) and not just a `#fragment` or `?query`.
fn is_relative_url(url: &str) -> bool {
    let scheme = url.find(':').is_some_and(|at| {
        let mut chars = url[..at].chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
    });
    !(url.is_empty() || scheme || url.starts_with(['/', '\\', '#', '?']))
}

/// Decodes `%XX` escapes; the input unchanged when the result is not UTF-8.
fn percent_decode(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok());
        match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
            Some(byte) if bytes[i] == b'%' => {
                out.push(byte);
                i += 3;
            }
            _ => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| path.to_string())
}

/// How one document's footnotes link up (pass 5 of the module docs).
#[derive(Debug, Default)]
struct FootnotePlan {
    /// Label key ([`label_key`]) → id of the definition this document's references link to:
    /// the first definition of the label in this document.
    targets: BTreeMap<String, String>,
    /// Label key → ids of this document's references to the label, in document order.
    references: BTreeMap<String, Vec<String>>,
}

/// Pass 5 (module docs): numbers the references per document in order of first use, moves
/// every footnote definition to the end in that order and plans the links. Warns about
/// labels defined twice here (`duplicate-footnote`) and references with no definition here
/// (`footnote-elsewhere`, once per label).
fn prepare_footnotes<'a>(doc: &'a AstNode<'a>, ctx: &mut RenderCtx) -> FootnotePlan {
    let mut plan = FootnotePlan::default();
    // label keys in order of first use, with the label and the line of that use
    let mut order: Vec<(String, String, Option<SourceSpan>)> = Vec::new();
    for node in doc.descendants() {
        let span = node_span(node);
        if let NodeValue::FootnoteReference(reference) = &mut node.data_mut().value {
            let key = label_key(&reference.name);
            let ix = match order.iter().position(|(k, ..)| *k == key) {
                Some(ix) => ix,
                None => {
                    order.push((key.clone(), reference.name.clone(), span));
                    order.len() - 1
                }
            };
            reference.ix = u32::try_from(ix + 1).unwrap_or(u32::MAX);
            let id = reference_id(reference);
            plan.references.entry(key).or_default().push(id);
        }
    }
    let mut definitions: Vec<_> = doc
        .descendants()
        .filter(|node| matches!(node.data().value, NodeValue::FootnoteDefinition(_)))
        .collect();
    let first_use = |node: &AstNode<'_>| match &node.data().value {
        NodeValue::FootnoteDefinition(def) => {
            let key = label_key(&def.name);
            order.iter().position(|(k, ..)| *k == key)
        }
        _ => None,
    };
    // stable: definitions without references here keep their order, after the others
    definitions.sort_by_key(|node| first_use(node).unwrap_or(usize::MAX));
    for node in definitions {
        doc.append(node);
        let data = node.data();
        let NodeValue::FootnoteDefinition(def) = &data.value else {
            continue;
        };
        match plan.targets.entry(label_key(&def.name)) {
            Entry::Vacant(entry) => {
                entry.insert(definition_id(node, def));
            }
            Entry::Occupied(_) => {
                let message = format!(
                    "footnote `[^{}]` is defined more than once on this slide; its references \
                     link to the first definition",
                    def.name
                );
                ctx.diagnostics.warn("duplicate-footnote", message).span = node_span(node);
            }
        }
    }
    for (key, name, span) in order {
        if !plan.targets.contains_key(&key) {
            let message = format!(
                "footnote `[^{name}]` is not defined on this slide: every slide (and every \
                 notes block) lists its own footnotes, so define it where it is used"
            );
            ctx.diagnostics.warn("footnote-elsewhere", message).span = span;
        }
    }
    plan
}

/// A footnote label as comrak matches them: trimmed, whitespace runs collapsed, case folded
/// (lowercased; comrak's full Unicode case folding differs only for a few characters).
fn label_key(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// comrak's id of a reference: `fnref-<label>`, `fnref-<label>-<n>` for the n-th (n > 1) use
/// of the label in the parsed document, so ids are unique in a deck.
fn reference_id(reference: &NodeFootnoteReference) -> String {
    if reference.ref_num > 1 {
        format!("fnref-{}-{}", reference.name, reference.ref_num)
    } else {
        format!("fnref-{}", reference.name)
    }
}

/// `fn-<label>` for the definition comrak resolved the label to (it counted the references);
/// `fn<line>-<label>` for another definition of the same label, which comrak left in place
/// with no references (a label reused on another slide). Unique in a deck either way.
fn definition_id(node: &AstNode<'_>, def: &NodeFootnoteDefinition) -> String {
    if def.total_references > 0 {
        format!("fn-{}", def.name)
    } else {
        format!("fn{}-{}", node.data().sourcepos.start.line, def.name)
    }
}

/// comrak's `format_html` with [`format_node`]; formatter diagnostics go to `ctx`. Closes the
/// footnote list that the first footnote definition opened.
fn format_html<'a>(
    doc: &'a AstNode<'a>,
    opts: &Options,
    ctx: &mut RenderCtx,
    footnotes: FootnotePlan,
) -> Result<String> {
    let mut html = String::new();
    let plugins = Plugins::default();
    let state = format_document_with_formatter(
        doc,
        opts,
        &mut html,
        &plugins,
        format_node,
        FormatState {
            footnotes,
            ..FormatState::default()
        },
    )
    .map_err(|err| Error::Other(format!("formatting HTML failed: {err}")))?;
    if state.definitions_written > 0 {
        // the definitions are the last nodes of the document (`prepare_footnotes`)
        html.push_str("</ol>\n</div>\n");
    }
    ctx.diagnostics.extend(state.diagnostics);
    Ok(html)
}

/// State threaded through the formatter.
#[derive(Default)]
struct FormatState {
    diagnostics: Vec<Diagnostic>,
    footnotes: FootnotePlan,
    /// Footnote definitions written so far; the first one opens the list.
    definitions_written: u32,
    /// Back-links of the definition being written (its number and the ids of the references
    /// on this slide), until they are placed: in its last paragraph or before `</li>`.
    pending_backrefs: Option<(u32, Vec<String>)>,
}

type FormatResult = std::result::Result<ChildRendering, fmt::Error>;

/// The formatter override (module docs, pass 5); everything else is comrak's default.
fn format_node<'a>(
    context: &mut Context<'_, '_, FormatState>,
    node: &'a AstNode<'a>,
    entering: bool,
) -> FormatResult {
    let data = node.data();
    let attrs = data.attrs.as_deref();
    match (&data.value, attrs) {
        (NodeValue::BlockDirective(directive), _) => {
            write_directive(context, node, &directive.info, entering)
        }
        (NodeValue::CodeBlock(block), _) => write_code_block(context, node, block, attrs, entering),
        (NodeValue::Math(math), _) => write_math(context, node, math, entering),
        (NodeValue::Heading(heading), Some(attrs)) => {
            write_heading(context, node, heading.level, attrs, entering)
        }
        (NodeValue::Link(link), Some(attrs)) => write_link(context, node, link, attrs, entering),
        (NodeValue::Image(link), Some(attrs)) => write_image(context, node, link, attrs, entering),
        (NodeValue::Code(code), Some(attrs)) => {
            if entering {
                context.write_str("<code")?;
                render_sourcepos(context, node)?;
                context.write_str(&element_attrs(attrs, &[]))?;
                context.write_str(">")?;
                context.escape(&code.literal)?;
                context.write_str("</code>")?;
            }
            Ok(ChildRendering::HTML)
        }
        (NodeValue::FootnoteReference(reference), _) => {
            write_footnote_reference(context, node, reference, entering)
        }
        (NodeValue::FootnoteDefinition(def), _) => {
            write_footnote_definition(context, node, def, entering)
        }
        // the last paragraph of a footnote definition carries its back-links (as in comrak)
        (NodeValue::Paragraph, _)
            if !entering
                && node.next_sibling().is_none()
                && node.parent().is_some_and(|parent| {
                    matches!(parent.data().value, NodeValue::FootnoteDefinition(_))
                }) =>
        {
            write_backrefs(context, true)?;
            context.write_str("</p>")?;
            context.lf()?;
            Ok(ChildRendering::HTML)
        }
        _ => format_node_default(context, node, entering),
    }
}

/// `<sup class="footnote-ref"><a href="#<definition on this slide>" id="fnref-…">N</a></sup>`
/// (module docs, pass 5); comrak's markup with a per-slide target and number.
fn write_footnote_reference<'a>(
    context: &mut Context<'_, '_, FormatState>,
    node: &'a AstNode<'a>,
    reference: &NodeFootnoteReference,
    entering: bool,
) -> FormatResult {
    if entering {
        let target = context
            .user
            .footnotes
            .targets
            .get(&label_key(&reference.name))
            .cloned()
            .unwrap_or_else(|| format!("fn-{}", reference.name));
        context.write_str("<sup")?;
        render_sourcepos(context, node)?;
        context.write_str(" class=\"footnote-ref\"><a href=\"#")?;
        context.escape_href(&target)?;
        context.write_str("\" id=\"")?;
        context.escape_href(&reference_id(reference))?;
        write!(context, "\" data-footnote-ref>{}</a></sup>", reference.ix)?;
    }
    Ok(ChildRendering::HTML)
}

/// `<li id="…">` of a footnote definition; the first one opens
/// `<div class="footnotes" data-footnotes><ol>` (closed by [`format_html`]). Only the
/// definition a label's references link to gets back-links.
fn write_footnote_definition<'a>(
    context: &mut Context<'_, '_, FormatState>,
    node: &'a AstNode<'a>,
    def: &NodeFootnoteDefinition,
    entering: bool,
) -> FormatResult {
    if !entering {
        if write_backrefs(context, false)? {
            context.lf()?;
        }
        context.write_str("</li>")?;
        context.lf()?;
        return Ok(ChildRendering::HTML);
    }
    if context.user.definitions_written == 0 {
        context.cr()?;
        context.write_str("<div")?;
        render_sourcepos(context, node)?;
        context.write_str(" class=\"footnotes\" data-footnotes>")?;
        context.lf()?;
        context.write_str("<ol>")?;
        context.lf()?;
    }
    context.user.definitions_written += 1;
    let id = definition_id(node, def);
    context.write_str("<li")?;
    render_sourcepos(context, node)?;
    context.write_str(" id=\"")?;
    context.escape_href(&id)?;
    context.write_str("\">")?;
    let state = &mut context.user;
    let key = label_key(&def.name);
    state.pending_backrefs = match state.footnotes.references.get(&key) {
        Some(refs) if state.footnotes.targets.get(&key) == Some(&id) => {
            Some((state.definitions_written, refs.clone()))
        }
        _ => None,
    };
    Ok(ChildRendering::HTML)
}

/// Writes the pending back-links of the current footnote definition (comrak's markup: `↩`,
/// then `↩²`, … for further references on this slide); `false` when there were none.
fn write_backrefs(
    context: &mut Context<'_, '_, FormatState>,
    leading_space: bool,
) -> std::result::Result<bool, fmt::Error> {
    let Some((ix, refs)) = context.user.pending_backrefs.take() else {
        return Ok(false);
    };
    for (n, id) in (1..).zip(&refs) {
        let (suffix, superscript) = if n == 1 {
            (String::new(), String::new())
        } else {
            (
                format!("-{n}"),
                format!("<sup class=\"footnote-ref\">{n}</sup>"),
            )
        };
        if n > 1 || leading_space {
            context.write_str(" ")?;
        }
        context.write_str("<a href=\"#")?;
        context.escape_href(id)?;
        write!(
            context,
            "\" class=\"footnote-backref\" data-footnote-backref \
             data-footnote-backref-idx=\"{ix}{suffix}\" \
             aria-label=\"Back to reference {ix}{suffix}\">↩{superscript}</a>"
        )?;
    }
    Ok(true)
}

/// `<div class="name classes…" data-…>` … `</div>` (dialect §4).
fn write_directive<'a>(
    context: &mut Context<'_, '_, FormatState>,
    node: &'a AstNode<'a>,
    info: &str,
    entering: bool,
) -> FormatResult {
    if !entering {
        context.cr()?;
        context.write_str("</div>")?;
        context.lf()?;
        return Ok(ChildRendering::HTML);
    }
    let (attrs, problems) = directive_attrs(info);
    let span = node_span(node);
    for problem in problems {
        let mut diagnostic = Diagnostic::warning("invalid-directive-arg", problem);
        diagnostic.span = span.clone();
        context.user.diagnostics.push(diagnostic);
    }
    context.cr()?;
    context.write_str("<div")?;
    context.write_str(&attrs)?;
    render_sourcepos(context, node)?;
    context.write_str(">")?;
    Ok(ChildRendering::HTML)
}

/// The attributes of a directive's `<div>` (leading space included) and the problems found
/// in its info string (dialect §4).
fn directive_attrs(info: &str) -> (String, Vec<String>) {
    let (tokens, unbalanced) = split_info(info);
    let mut problems = Vec::new();
    if let Some(quote) = unbalanced {
        problems.push(format!(
            "unbalanced {quote} quote in directive `:::{info}` (read to the end of the line)"
        ));
    }
    let mut tokens = tokens.into_iter();
    let name = tokens.next().map(|token| token.text).unwrap_or_default();
    let mut classes = vec![name.clone()];
    let mut attrs: BTreeMap<String, String> = BTreeMap::new();
    for token in tokens {
        let Some(eq) = token.eq else {
            classes.push(token.text);
            continue;
        };
        let (key, value) = (&token.text[..eq], &token.text[eq + 1..]);
        let lower = key.to_ascii_lowercase();
        match (name.as_str(), lower.as_str()) {
            ("fragment", "index") => {
                attrs.insert("data-fragment-index".into(), value.into());
            }
            ("columns", "widths") => {
                attrs.insert("style".into(), format!("grid-template-columns: {value}"));
            }
            // consumed: the dialect §4 table renders `:::column width=…` as `<div class="column">`
            ("column", "width") => {}
            _ if !is_valid_attr_name(&lower) => problems.push(format!(
                "`{key}` in directive `:::{info}` is not a valid attribute name (dropped)"
            )),
            _ if lower.starts_with("data-") => {
                attrs.insert(lower, value.into());
            }
            _ => {
                attrs.insert(format!("data-{lower}"), value.into());
            }
        }
    }
    let mut unique: Vec<String> = Vec::new();
    for class in classes {
        if !class.is_empty() && !unique.contains(&class) {
            unique.push(class);
        }
    }
    let classes = unique;
    let mut html = String::new();
    if !classes.is_empty() {
        push_attr(&mut html, "class", &classes.join(" "));
    }
    for (key, value) in &attrs {
        push_attr(&mut html, key, value);
    }
    (html, problems)
}

/// One whitespace-separated item of a directive info string, quotes removed.
#[derive(Default)]
struct InfoToken {
    text: String,
    /// Byte offset in `text` of the first `=` outside quotes.
    eq: Option<usize>,
}

/// Splits on whitespace outside `"…"` / `'…'` (backslash escapes the next character inside
/// quotes). An unclosed quote runs to the end; its character is returned.
fn split_info(info: &str) -> (Vec<InfoToken>, Option<char>) {
    let mut tokens = Vec::new();
    let mut current: Option<InfoToken> = None;
    let mut unbalanced = None;
    let mut chars = info.chars();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            tokens.extend(current.take());
            continue;
        }
        let token = current.get_or_insert_with(InfoToken::default);
        match c {
            '"' | '\'' => loop {
                match chars.next() {
                    None => {
                        unbalanced = Some(c);
                        break;
                    }
                    Some('\\') => token.text.extend(chars.next()),
                    Some(q) if q == c => break,
                    Some(other) => token.text.push(other),
                }
            },
            '=' if token.eq.is_none() => {
                token.eq = Some(token.text.len());
                token.text.push(c);
            }
            _ => token.text.push(c),
        }
    }
    tokens.extend(current);
    (tokens, unbalanced)
}

/// Fenced code (dialect §5) and ```` ```math ```` fences (§5 notes, §8).
fn write_code_block<'a>(
    context: &mut Context<'_, '_, FormatState>,
    node: &'a AstNode<'a>,
    block: &NodeCodeBlock,
    attrs: Option<&Attributes>,
    entering: bool,
) -> FormatResult {
    if !entering {
        return Ok(ChildRendering::HTML);
    }
    let sourcepos = context
        .options
        .render
        .sourcepos
        .then(|| node.data().sourcepos.to_string());
    context.cr()?;
    if is_math_fence(&block.info) {
        context.write_str("<div")?;
        render_sourcepos(context, node)?;
        context.write_str(" data-math-style=\"display\"")?;
        if let Some(attrs) = attrs {
            context.write_str(&element_attrs(attrs, &[MATH_ATTR]))?;
        }
        context.write_str(">")?;
        context.escape(strip_trailing_newline(&block.literal))?;
        context.write_str("</div>")?;
    } else {
        let parsed = attrs.map(ParsedAttrs::from);
        let code = code_attrs(&block.info, parsed.as_ref());
        context.write_str(&render_code_block_with(
            &block.literal,
            &code,
            sourcepos.as_deref(),
        ))?;
    }
    context.lf()?;
    Ok(ChildRendering::HTML)
}

/// Whether a fence's info string names the `math` language (comrak's `math_code` rule).
fn is_math_fence(info: &str) -> bool {
    info.split_whitespace().next() == Some("math")
}

/// `<span data-math-style="inline|display">TeX</span>` (dialect §8).
fn write_math<'a>(
    context: &mut Context<'_, '_, FormatState>,
    node: &'a AstNode<'a>,
    math: &NodeMath,
    entering: bool,
) -> FormatResult {
    if entering {
        let style = if math.display_math {
            "display"
        } else {
            "inline"
        };
        write!(context, "<span {MATH_ATTR}=\"{style}\"")?;
        render_sourcepos(context, node)?;
        context.write_str(">")?;
        context.escape(&math.literal)?;
        context.write_str("</span>")?;
    }
    Ok(ChildRendering::HTML)
}

/// A heading with `{…}` attributes (§3.3): comrak's `<hN>` plus the attributes.
fn write_heading<'a>(
    context: &mut Context<'_, '_, FormatState>,
    node: &'a AstNode<'a>,
    level: u8,
    attrs: &Attributes,
    entering: bool,
) -> FormatResult {
    if entering {
        context.cr()?;
        write!(context, "<h{level}")?;
        render_sourcepos(context, node)?;
        context.write_str(&element_attrs(attrs, &[]))?;
        context.write_str(">")?;
    } else {
        write!(context, "</h{level}>")?;
        context.lf()?;
    }
    Ok(ChildRendering::HTML)
}

/// A link with `{…}` attributes (§3.3): comrak's `<a href title>` plus the attributes.
fn write_link<'a>(
    context: &mut Context<'_, '_, FormatState>,
    node: &'a AstNode<'a>,
    link: &NodeLink,
    attrs: &Attributes,
    entering: bool,
) -> FormatResult {
    if !entering {
        context.write_str("</a>")?;
        return Ok(ChildRendering::HTML);
    }
    context.write_str("<a")?;
    render_sourcepos(context, node)?;
    context.write_str(" href=\"")?;
    if context.options.render.r#unsafe || !dangerous_url(&link.url) {
        context.escape_href(&link.url)?;
    }
    context.write_str("\"")?;
    if !link.title.is_empty() {
        context.write_str(" title=\"")?;
        context.escape(&link.title)?;
        context.write_str("\"")?;
    }
    context.write_str(&element_attrs(attrs, &["href", "title"]))?;
    context.write_str(">")?;
    Ok(ChildRendering::HTML)
}

/// An image with `{…}` attributes (§3.3): comrak's `<img src alt title />` plus the
/// attributes. The alt text is the image's children rendered as plain text.
fn write_image<'a>(
    context: &mut Context<'_, '_, FormatState>,
    node: &'a AstNode<'a>,
    link: &NodeLink,
    attrs: &Attributes,
    entering: bool,
) -> FormatResult {
    if entering {
        context.write_str("<img")?;
        render_sourcepos(context, node)?;
        context.write_str(" src=\"")?;
        if context.options.render.r#unsafe || !dangerous_url(&link.url) {
            context.escape_href(&link.url)?;
        }
        context.write_str("\" alt=\"")?;
        return Ok(ChildRendering::Plain);
    }
    if !link.title.is_empty() {
        context.write_str("\" title=\"")?;
        context.escape(&link.title)?;
    }
    context.write_str("\"")?;
    context.write_str(&element_attrs(attrs, &["src", "alt", "title"]))?;
    context.write_str(" />")?;
    Ok(ChildRendering::HTML)
}

/// ` id="…" class="…" key="value"…` for comrak node attributes (§3.3). `id=` and `class=`
/// pairs act like `#id` and `.class`; a repeated key keeps its last value; keys in
/// `reserved` (written by the caller) and invalid names are skipped.
fn element_attrs(attrs: &Attributes, reserved: &[&str]) -> String {
    let mut id = attrs.id.clone();
    let mut classes: Vec<String> = Vec::new();
    let mut pairs: Vec<(String, String)> = Vec::new();
    let add_classes = |classes: &mut Vec<String>, text: &str| {
        for class in text.split_whitespace() {
            if !classes.iter().any(|c| c == class) {
                classes.push(class.to_string());
            }
        }
    };
    for class in &attrs.classes {
        add_classes(&mut classes, class);
    }
    for (key, value) in &attrs.pairs {
        let lower = key.to_ascii_lowercase();
        match lower.as_str() {
            "id" => id = Some(value.clone()),
            "class" => add_classes(&mut classes, value),
            _ if reserved.contains(&lower.as_str()) || !is_valid_attr_name(key) => {}
            _ => match pairs.iter_mut().find(|(k, _)| *k == lower) {
                Some(pair) => pair.1 = value.clone(),
                None => pairs.push((lower, value.clone())),
            },
        }
    }
    let mut html = String::new();
    if let Some(id) = id.filter(|id| !id.is_empty()) {
        push_attr(&mut html, "id", &id);
    }
    if !classes.is_empty() {
        push_attr(&mut html, "class", &classes.join(" "));
    }
    for (key, value) in &pairs {
        push_attr(&mut html, key, value);
    }
    html
}

/// A new, empty `Document` node in `arena`.
fn new_document<'a>(arena: &'a Arena<'a>) -> &'a AstNode<'a> {
    arena.alloc(NodeValue::Document.into())
}

/// The node's source lines as a Markdown span (`None` for synthetic nodes).
fn node_span(node: &AstNode<'_>) -> Option<SourceSpan> {
    let pos = node.data().sourcepos;
    (pos.start.line > 0).then(|| SourceSpan {
        start_line: pos.start.line,
        end_line: pos.end.line.max(pos.start.line),
        cell: None,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::MarkdownConfig;
    use crate::markdown::comrak_options;

    fn fixture_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/md")
    }

    /// Renders `text` with the dialect options; returns the result, the registry and the
    /// diagnostics as `(code, line)`.
    fn render_with(
        text: &str,
        mode: RenderMode,
        sourcepos: bool,
    ) -> (Rendered, AssetRegistry, Vec<(&'static str, usize)>) {
        let mut assets = AssetRegistry::new();
        let mut diagnostics = Diagnostics::new();
        let dir = fixture_dir();
        let mut ctx = RenderCtx {
            assets: &mut assets,
            base_dir: &dir,
            mode,
            diagnostics: &mut diagnostics,
            missing_asset_is_error: false,
        };
        let opts = comrak_options(&MarkdownConfig {
            sourcepos,
            ..MarkdownConfig::default()
        });
        let rendered = render_fragment(text, &opts, &mut ctx).unwrap();
        let codes = diagnostics
            .iter()
            .map(|d| (d.code, d.span.as_ref().map_or(0, |s| s.start_line)))
            .collect();
        (rendered, assets, codes)
    }

    fn render(text: &str) -> Rendered {
        let (rendered, _, codes) = render_with(text, RenderMode::Deck, false);
        assert_eq!(codes, [], "unexpected diagnostics for {text:?}");
        rendered
    }

    fn ok_png_id() -> String {
        AssetRegistry::new().register_file(&fixture_dir().join("images/ok.png"))
    }

    #[test]
    fn notes_are_extracted_wherever_they_are_nested() {
        let text = "\
# Title

::::fragment
Body in a fragment.
:::notes
Note in a fragment.
:::
::::

- item

  :::notes
  Note in a list item.
  :::

::::notes
Outer note.
:::notes
Inner note.
:::
After inner.
::::
";
        let rendered = render(text);
        assert_eq!(
            rendered.html,
            "<h1>Title</h1>\n<div class=\"fragment\">\n<p>Body in a fragment.</p>\n</div>\n\
             <ul>\n<li>\n<p>item</p>\n</li>\n</ul>\n"
        );
        assert_eq!(
            rendered.notes.as_deref(),
            Some(
                "<p>Note in a fragment.</p>\n<p>Note in a list item.</p>\n\
                 <p>Outer note.</p>\n<p>After inner.</p>\n<p>Inner note.</p>\n"
            )
        );
        assert!(rendered.features.notes);
        assert_eq!(rendered.title.as_deref(), Some("Title"));
    }

    #[test]
    fn empty_notes_are_none_and_note_content_counts_for_features() {
        let empty = render("Body\n\n:::notes\n:::\n");
        assert_eq!(empty.notes, None);
        assert!(!empty.features.notes);
        let (rendered, assets, codes) = render_with(
            "Body\n\n:::notes\n```py\nx\n```\n\n$y$ ![i](images/ok.png)\n:::\n",
            RenderMode::Deck,
            false,
        );
        assert_eq!(codes, []);
        assert_eq!(rendered.html, "<p>Body</p>\n");
        let notes = rendered.notes.unwrap();
        assert!(
            notes.contains(&format!("src=\"asset:{}\"", ok_png_id())),
            "{notes}"
        );
        assert_eq!(assets.len(), 1);
        assert_eq!(
            rendered.features,
            Features {
                math: true,
                code: true,
                notes: true,
                notebook: false
            }
        );
    }

    #[test]
    fn directive_arguments() {
        let html = render(
            ":::callout data-x=1 Index=2 widths=a title='say \"hi\" <b>'\nx\n:::\n\n\
             :::fragment index=3 fade-in\ny\n:::\n\n:::column width=40% grow\nz\n:::\n",
        )
        .html;
        assert_eq!(
            html,
            "<div class=\"callout\" data-index=\"2\" data-title=\"say &quot;hi&quot; &lt;b&gt;\" \
             data-widths=\"a\" data-x=\"1\">\n<p>x</p>\n</div>\n\
             <div class=\"fragment fade-in\" data-fragment-index=\"3\">\n<p>y</p>\n</div>\n\
             <div class=\"column grow\">\n<p>z</p>\n</div>\n"
        );
    }

    #[test]
    fn bad_directive_arguments_are_reported() {
        let (rendered, _, codes) = render_with(
            "text\n\n:::box 1a=x ok=y title=\"open\nbody\n:::\n",
            RenderMode::Deck,
            false,
        );
        assert_eq!(
            rendered.html,
            "<p>text</p>\n<div class=\"box\" data-ok=\"y\" data-title=\"open\">\n<p>body</p>\n</div>\n"
        );
        assert_eq!(
            codes,
            [("invalid-directive-arg", 3), ("invalid-directive-arg", 3)]
        );
    }

    #[test]
    fn split_info_is_quote_aware() {
        let (tokens, unbalanced) = split_info(r#"columns widths="1fr 2fr" 'a b' k='v "w"' x=y=z"#);
        let texts: Vec<(&str, Option<usize>)> =
            tokens.iter().map(|t| (t.text.as_str(), t.eq)).collect();
        assert_eq!(
            texts,
            [
                ("columns", None),
                ("widths=1fr 2fr", Some(6)),
                ("a b", None),
                ("k=v \"w\"", Some(1)),
                ("x=y=z", Some(1)),
            ]
        );
        assert_eq!(unbalanced, None);
        assert_eq!(split_info("a 'b").1, Some('\''));
    }

    #[test]
    fn element_attributes_on_headings_links_images_and_code() {
        let html = render(
            "Intro\n\n## H {#h .a class=\"b c\" data-x=1 id=over}\n\n\
             [l](u \"T\"){.btn href=bad target=_blank} ![alt *x*](https://x/p.png \"t\"){width=10 src=bad .r}\n\
             `c`{.lang}\n",
        )
        .html;
        assert_eq!(
            html,
            "<p>Intro</p>\n<h2 id=\"over\" class=\"a b c\" data-x=\"1\">H</h2>\n\
             <p><a href=\"u\" title=\"T\" class=\"btn\" target=\"_blank\">l</a> \
             <img src=\"https://x/p.png\" alt=\"alt x\" title=\"t\" class=\"r\" width=\"10\" />\n\
             <code class=\"lang\">c</code></p>\n"
        );
    }

    #[test]
    fn nodes_without_attributes_render_like_comrak() {
        let text = "# H\n\n[l](u) ![a](https://x/p.png) `c` $m$ $$d$$ $`k`$\n";
        assert_eq!(
            render(text).html,
            "<h1>H</h1>\n<p><a href=\"u\">l</a> <img src=\"https://x/p.png\" alt=\"a\" /> \
             <code>c</code> <span data-math-style=\"inline\">m</span> \
             <span data-math-style=\"display\">d</span> <span data-math-style=\"inline\">k</span></p>\n"
        );
    }

    #[test]
    fn sourcepos_is_kept_on_rewritten_nodes() {
        let text = ":::box k=v\n# T {.x}\n:::\n\n```py {.big}\nx\n```\n\n```math\ny\n```\n\n$z$\n";
        let (rendered, _, _) = render_with(text, RenderMode::Deck, true);
        assert_eq!(
            rendered.html,
            "<div class=\"box\" data-k=\"v\" data-sourcepos=\"1:1-3:3\">\n\
             <h1 data-sourcepos=\"2:1-2:8\" class=\"x\">T</h1>\n</div>\n\
             <pre data-sourcepos=\"5:1-7:3\" class=\"big\"><code class=\"language-py\" data-trim>x</code></pre>\n\
             <div data-sourcepos=\"9:1-11:3\" data-math-style=\"display\">y</div>\n\
             <p data-sourcepos=\"13:1-13:3\"><span data-math-style=\"inline\" data-sourcepos=\"13:1-13:3\">z</span></p>\n"
        );
    }

    #[test]
    fn raw_html_urls() {
        let id = ok_png_id();
        let text = "\
<video poster='images/ok.png'>
<source src=\"images/ok.png?v=1#t\">
</video>

<img data-src=\"images/ok.png\" src=images/o%6B.png alt=a>

<!-- <img src=\"images/missing.png\"> -->

x <img src=\"images/missing.png\"> <img src=\"https://e.org/a.png\"> <img src=\"images/ok.png#a&amp;b\">

<div>
<!-- <img src=\"images/missing.png\"> -->
<p title=\"a > b\" src=\"images/ok.png\">text with src=\"images/missing.png\" in it</p>
</div>
";
        let (rendered, assets, codes) = render_with(text, RenderMode::Deck, false);
        assert_eq!(
            rendered.html,
            format!(
                "<video poster='asset:{id}'>\n<source src=\"asset:{id}?v=1#t\">\n</video>\n\
                 <img data-src=\"images/ok.png\" src=asset:{id} alt=a>\n\
                 <!-- <img src=\"images/missing.png\"> -->\n\
                 <p>x <img src=\"images/missing.png\"> <img src=\"https://e.org/a.png\"> \
                 <img src=\"asset:{id}#a&amp;b\"></p>\n\
                 <div>\n<!-- <img src=\"images/missing.png\"> -->\n\
                 <p title=\"a > b\" src=\"asset:{id}\">text with src=\"images/missing.png\" in it</p>\n\
                 </div>\n"
            )
        );
        assert_eq!(assets.len(), 1);
        assert_eq!(codes, [("missing-asset", 9)]);
    }

    #[test]
    fn raw_html_text_in_values_scripts_and_styles_is_not_an_attribute() {
        let id = ok_png_id();
        let text = "\
<img alt=\"x src=images/missing.png\" title=' poster=images/ok.png' src=\"https://e.org/a.png\">

<script>var s = '<img src=\"images/missing.png\">';</script>

<STYLE>x::after { content: '<video poster=images/missing.png>' }</Style>

x <script>var s = '<img src=\"images/missing.png\">';</script> <img src=images/ok.png>

<textarea><img src=\"images/missing.png\"></textarea><img data-x=a src=images/ok.png>
";
        let (rendered, assets, codes) = render_with(text, RenderMode::Deck, false);
        assert_eq!(codes, [], "{}", rendered.html);
        assert_eq!(assets.len(), 1);
        let html = &rendered.html;
        assert!(html.starts_with(
            "<img alt=\"x src=images/missing.png\" title=' poster=images/ok.png' \
             src=\"https://e.org/a.png\">\n"
        ));
        assert!(html.contains("<script>var s = '<img src=\"images/missing.png\">';</script>\n"));
        assert!(html.contains(&format!(
            "</script> <img src=asset:{id}></p>\n<textarea><img src=\"images/missing.png\">\
             </textarea><img data-x=a src=asset:{id}>"
        )));
    }

    #[test]
    fn start_tags_follow_the_html_tokenizer() {
        let tags = |snippets: &[&str]| {
            let mut scan = HtmlScan::default();
            let mut found: Vec<String> = Vec::new();
            for html in snippets {
                let Ok(()) = scan.start_tags(html, |tag| {
                    let attrs: Vec<String> = tag
                        .attrs
                        .iter()
                        .map(|a| match &a.value {
                            Some(range) => format!("{}={}", a.name, &html[range.clone()]),
                            None => a.name.clone(),
                        })
                        .collect();
                    found.push(format!("{}[{}]", tag.name, attrs.join(" ")));
                    Ok::<(), Infallible>(())
                });
            }
            (found, scan)
        };
        let (found, scan) = tags(&[
            "<A HREF = 'x y' Data-K=v/ bare =odd>",
            "<!--> <b x=1> <!---> <i y=2> <!-- <c> -->",
            "</p x=1> <!DOCTYPE html> <?pi <d>?> a < b <e f=\"1>2\" g=é>",
            "<script type=module>if (a<b) { '<f src=1>' }</SCRIPT ><g>",
            "<p>unclosed <!-- <h>",
            "--> <i src=\"unterminated",
        ]);
        assert_eq!(
            found,
            [
                "a[href=x y data-k=v/ bare=odd]",
                "b[x=1]",
                "i[y=2]",
                "e[f=1>2 g=é]",
                "script[type=module]",
                "g[]",
                "p[]",
                "i[src=unterminated]",
            ]
        );
        assert_eq!(scan, HtmlScan::Data);
        assert_eq!(tags(&["<style>a<b>"]).1, HtmlScan::RawText("style"));
        assert_eq!(tags(&["x <!-- y"]).1, HtmlScan::Comment);
    }

    #[test]
    fn math_markup_is_a_start_tag_attribute() {
        for html in [
            "<span data-math-style=\"inline\">x</span>",
            "<div class=\"m\" DATA-MATH-STYLE='display'>x</div>",
            "<p data-math-style>x</p>",
        ] {
            assert!(has_math_markup(html), "{html}");
        }
        for html in [
            "<p>Use <code>data-math-style</code> attribute.</p>",
            "<pre><code>&lt;span data-math-style=\"inline\"&gt;</code></pre>",
            "<p title=\" data-math-style=x\">t</p>",
            "<!-- <span data-math-style=\"inline\"> -->",
            "<script>'<span data-math-style=\"inline\">'</script>",
        ] {
            assert!(!has_math_markup(html), "{html}");
        }
        let rendered = render(
            "Use `data-math-style` attribute.\n\n```html\n<span data-math-style=\"inline\">x</span>\n```\n",
        );
        assert!(!rendered.features.math && rendered.features.code);
        assert!(
            render("<span data-math-style=\"inline\">x</span>\n")
                .features
                .math
        );
    }

    #[test]
    fn missing_asset_path_reads_the_frozen_message() {
        let (_, _, codes) = render_with("![a](images/missing.png)\n", RenderMode::Deck, false);
        assert_eq!(codes, [("missing-asset", 1)]);
        let mut assets = AssetRegistry::new();
        let mut diagnostics = Diagnostics::new();
        let dir = fixture_dir();
        let mut ctx = RenderCtx {
            assets: &mut assets,
            base_dir: &dir,
            mode: RenderMode::Deck,
            diagnostics: &mut diagnostics,
            missing_asset_is_error: false,
        };
        let opts = comrak_options(&MarkdownConfig::default());
        let text = "<img src=\"a b (1).png\"> ![x](sub/../gone%29.png)\n";
        render_fragment(text, &opts, &mut ctx).unwrap();
        let paths: Vec<_> = diagnostics.iter().map(missing_asset_path).collect();
        assert_eq!(
            paths,
            [Some(dir.join("a b (1).png")), Some(dir.join("gone).png"))]
        );
        let other = Diagnostic::warning("invalid-directive", "x (looked for y)");
        assert_eq!(missing_asset_path(&other), None);
    }

    #[test]
    fn colons_in_a_directive_line_are_reported() {
        let codes_of = |text: &str| render_with(text, RenderMode::Deck, false).2;
        let (rendered, _, codes) = render_with(
            "Intro\n\n:::box title=\"a:b\"\nx\n:::\n",
            RenderMode::Deck,
            false,
        );
        assert_eq!(codes, [("invalid-directive", 3)]);
        assert_eq!(
            rendered.html,
            "<p>Intro</p>\n<p>:::box title=&quot;a:b&quot;\nx</p>\n<div>\n</div>\n"
        );
        assert_eq!(
            codes_of("- item\n\n  ::::callout https://x.org\n  y\n  ::::\n"),
            [("invalid-directive", 3)]
        );
        // `&#58;` is the colon comrak accepts
        let rendered = render("::: tip title=\"a&#58;b\"\nok\n:::\n");
        assert_eq!(
            rendered.html,
            "<div class=\"tip\" data-title=\"a:b\">\n<p>ok</p>\n</div>\n"
        );
        // not reported: text that does not start with a fence, two colons, no name
        assert_eq!(codes_of("A :::b line\n\n::x\n\n::: 1:2\n"), []);
    }

    #[test]
    fn duplicate_footnotes_on_one_fragment() {
        let (rendered, _, codes) = render_with(
            "A[^x]\n\n[^x]: first\n\n[^x]: second\n",
            RenderMode::Deck,
            false,
        );
        assert_eq!(codes, [("duplicate-footnote", 5)]);
        assert!(
            rendered
                .html
                .contains(r##"<a href="#fn3-x" id="fnref-x" data-footnote-ref>1</a>"##)
        );
        assert!(
            rendered
                .html
                .contains("<li id=\"fn-x\">\n<p>second</p>\n</li>")
        );
    }

    #[test]
    fn heading_attributes_are_lifted_on_request() {
        let mut assets = AssetRegistry::new();
        let mut diagnostics = Diagnostics::new();
        let dir = fixture_dir();
        let mut ctx = RenderCtx {
            assets: &mut assets,
            base_dir: &dir,
            mode: RenderMode::Notebook,
            diagnostics: &mut diagnostics,
            missing_asset_is_error: false,
        };
        let opts = comrak_options(&MarkdownConfig::default());
        let text = "# Analysis {#a .wide layout=title}\n\n## Sub {.x}\n";
        let (rendered, lifted) = render_fragment_with_lift(text, &opts, &mut ctx, true).unwrap();
        assert_eq!(
            rendered.html,
            "<h1>Analysis</h1>\n<h2 class=\"x\">Sub</h2>\n"
        );
        assert_eq!(rendered.title.as_deref(), Some("Analysis"));
        let lifted = lifted.unwrap();
        assert_eq!(lifted.id.as_deref(), Some("a"));
        assert_eq!(lifted.classes, ["wide"]);
        let (rendered, lifted) = render_fragment_with_lift(text, &opts, &mut ctx, false).unwrap();
        assert!(lifted.is_none());
        assert!(
            rendered
                .html
                .starts_with("<h1 id=\"a\" class=\"wide\" layout=\"title\">")
        );
        // only a leading heading is lifted
        let (_, lifted) =
            render_fragment_with_lift("x\n\n# H {.c}\n", &opts, &mut ctx, true).unwrap();
        assert!(lifted.is_none());
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn image_urls_keep_query_and_fragment_and_are_percent_decoded() {
        let id = ok_png_id();
        let (rendered, _, codes) = render_with(
            "![a](images/ok.png?x=1#f) ![b](<images/o%6B.png>) ![c](images) ![d](#top)\n",
            RenderMode::Deck,
            false,
        );
        assert_eq!(
            rendered.html,
            format!(
                "<p><img src=\"asset:{id}?x=1#f\" alt=\"a\" /> <img src=\"asset:{id}\" alt=\"b\" /> \
                 <img src=\"images\" alt=\"c\" /> <img src=\"#top\" alt=\"d\" /></p>\n"
            )
        );
        // a directory is not a file
        assert_eq!(codes, [("missing-asset", 1)]);
    }

    #[test]
    fn relative_url_classification() {
        for url in [
            "a.png",
            "./a.png",
            "../a.png",
            "dir/a:b.png",
            "a.png?x",
            "%20.png",
        ] {
            assert!(is_relative_url(url), "{url}");
        }
        for url in [
            "",
            "/a.png",
            "//host/a.png",
            "#frag",
            "?q",
            "https://h/a.png",
            "data:image/png;base64,AA",
            "asset:0123456789ab",
            "mailto:a@b",
            "attachment:img.png",
            "\\\\server\\a.png",
        ] {
            assert!(!is_relative_url(url), "{url}");
        }
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b%2Fc"), "a b/c");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
        assert_eq!(percent_decode("%C3%A9"), "é");
        assert_eq!(percent_decode("%FF"), "%FF", "not UTF-8: unchanged");
    }

    #[test]
    fn slide_background_assets() {
        let id = ok_png_id();
        let mut assets = AssetRegistry::new();
        let mut diagnostics = Diagnostics::new();
        let dir = fixture_dir();
        let mut ctx = RenderCtx {
            assets: &mut assets,
            base_dir: &dir,
            mode: RenderMode::Deck,
            diagnostics: &mut diagnostics,
            missing_asset_is_error: false,
        };
        let mut attrs: BTreeMap<String, String> = [
            ("data-background-image", "https://e.org/a.png"),
            (
                "data-background-video",
                "images/ok.png, gone.mp4,https://e.org/v.webm",
            ),
            ("data-background-iframe", "images/ok.png"),
            ("data-background-color", "images/ok.png"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let span = SourceSpan {
            start_line: 4,
            end_line: 9,
            cell: None,
        };
        rewrite_slide_assets(&mut attrs, Some(&span), &mut ctx).unwrap();
        assert_eq!(attrs["data-background-image"], "https://e.org/a.png");
        assert_eq!(
            attrs["data-background-video"],
            format!("asset:{id}, gone.mp4,https://e.org/v.webm")
        );
        assert_eq!(attrs["data-background-iframe"], format!("asset:{id}"));
        assert_eq!(attrs["data-background-color"], "images/ok.png");
        let found: Vec<_> = ctx
            .diagnostics
            .iter()
            .map(|d| (d.code, d.span.clone()))
            .collect();
        assert_eq!(found, [("missing-asset", Some(span.clone()))]);

        ctx.missing_asset_is_error = true;
        let mut missing =
            BTreeMap::from([("data-background-image".to_string(), "x.png".to_string())]);
        assert!(matches!(
            rewrite_slide_assets(&mut missing, Some(&span), &mut ctx),
            Err(Error::MissingAsset { path, span: Some(_) }) if path == dir.join("x.png")
        ));
    }

    #[test]
    fn notebook_fragments_have_no_front_matter() {
        let text = "---\ntitle: x\n---\n\nBody\n";
        let (deck_mode, _, _) = render_with(text, RenderMode::Deck, false);
        assert_eq!(deck_mode.html, "<p>Body</p>\n");
        let (notebook_mode, _, _) = render_with(text, RenderMode::Notebook, false);
        assert_eq!(
            notebook_mode.html,
            "<hr />\n<h2>title: x</h2>\n<p>Body</p>\n"
        );
        // never split: separators stay content
        let rendered = render("# A\n\n---\n\n# B\n\n--\n");
        assert_eq!(rendered.html, "<h1>A</h1>\n<hr />\n<h1>B</h1>\n<p>--</p>\n");
        assert_eq!(rendered.title.as_deref(), Some("A"));
    }

    #[test]
    fn heading_attributes_are_not_lifted_by_the_renderer() {
        let rendered = render("# Hi {#top .x}\n\ntext\n");
        assert_eq!(
            rendered.html,
            "<h1 id=\"top\" class=\"x\">Hi</h1>\n<p>text</p>\n"
        );
        assert_eq!(rendered.title.as_deref(), Some("Hi"));
    }

    #[test]
    fn render_nodes_moves_the_nodes_under_a_new_document() {
        let arena = Arena::new();
        let opts = comrak_options(&MarkdownConfig::default());
        let root = parse_document(&arena, "# A\n\n:::notes\nn\n:::\n\npara\n\n# B\n", &opts);
        let nodes: Vec<_> = root.children().take(3).collect();
        let mut assets = AssetRegistry::new();
        let mut diagnostics = Diagnostics::new();
        let dir = fixture_dir();
        let mut ctx = RenderCtx {
            assets: &mut assets,
            base_dir: &dir,
            mode: RenderMode::Deck,
            diagnostics: &mut diagnostics,
            missing_asset_is_error: true,
        };
        let rendered = render_nodes(&arena, &nodes, &opts, &mut ctx).unwrap();
        assert_eq!(rendered.html, "<h1>A</h1>\n<p>para</p>\n");
        assert_eq!(rendered.notes.as_deref(), Some("<p>n</p>\n"));
        assert_eq!(
            root.children().count(),
            1,
            "only `# B` is left in the original"
        );
    }
}
