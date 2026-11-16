//! Markdown block renderer: directives, notes, math, images → assets. Implemented by T12.
//!
//! Contract: `docs/spec/markdown-dialect.md` §3.3–§9. [`render_nodes`] renders one slide (a
//! group of top-level comrak nodes) and [`render_fragment`] parses and renders a standalone
//! Markdown string (notebook cells, `text/markdown` outputs, the `markdown` template filter);
//! [`render_fragment_with_lift`] also lifts a leading heading's attributes (notebook cells).
//! A fragment is parsed like a deck: directives nested with fences of one length are read as
//! the dialect nests them ([`renest_directives`]) and attribute blocks comrak rejects are read
//! again ([`recover_attr_blocks`], A3).
//!
//! The nodes are moved under a synthetic `Document` node, so lists and footnotes render as a
//! unit (the footnote list of a slide is closed at the end of that slide). Then:
//!
//! 0. **includes** (§4.1, T48), in [`RenderMode::Notebook`] only: an `:::include` directive
//!    is removed with an `include-in-notebook` warning (only a Markdown deck includes files;
//!    the Markdown frontend handles every `:::include` of a deck before rendering); what
//!    comrak put inside it (the rest of the cell, when it is not closed) stays in its place;
//! 1. **title**: [`first_heading_text`] of the nodes, before anything is changed;
//! 2. **notes** (§4, §7): every outermost `:::notes` directive, wherever it is nested, is
//!    detached; its children (and the footnote definitions written inside it, which comrak
//!    moved to the end of the document) are rendered through a second synthetic document with
//!    the same passes and appended to [`Rendered::notes`] in document order;
//! 3. **checks**: a paragraph that starts like a directive (`:::name …`, `::: {.name …}`) but
//!    is not one is an `invalid-directive` warning: comrak does not open a directive whose
//!    line has a `:` after the fence (`title="a:b"`, a URL, pandoc's closing `:::`); `&#58;`
//!    is the way to write that colon. Its colons count as typed in the source the nodes were
//!    parsed from ([`SourceLines::typed_colons`]): a line written `\:::name` or `&#58;::name`
//!    shows the fence as text;
//! 4. **assets** (§6): relative image URLs and relative `src` / `poster` attributes of raw
//!    HTML start tags ([`rewrite_html_assets`]) are resolved against [`RenderCtx::base_dir`]:
//!    an existing file is registered ([`AssetRegistry::register_file`]) and the URL becomes
//!    `asset:<id>` (a `?query` or `#fragment` is kept after it); a missing file is a
//!    `missing-asset` warning (the URL stays as written; [`missing_asset_path`] reads the
//!    file back from it) or [`Error::MissingAsset`] when [`RenderCtx::missing_asset_is_error`].
//!    Raw HTML is tokenized like a browser does: comments, the content of raw-text elements
//!    (`<script>`, `<style>`, …) and text inside other attributes' quoted values never count,
//!    also when a `<script>` opens in one raw HTML node and closes in a later one. A file
//!    found only by letter case (`plain.PNG` for `plain.png`, which a case-insensitive file
//!    system accepts) is registered and reported as `asset-case-mismatch` (A3);
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
//!      info string, or from pandoc's attribute block (`::: {.fragment .fade-up}`,
//!      `::: {#id .note}`, `::: {.notes}`; A3): `:::fragment … index=N` adds
//!      `data-fragment-index="N"` (N a non-negative integer, else an `invalid-directive-arg`
//!      warning), `::::columns widths="1fr 2fr"` adds `style="grid-template-columns: 1fr 2fr"`,
//!      `:::column width=…` renders `<div class="column">` (the width is consumed), and every
//!      other `key=value` arg becomes `data-<key>="value"`. A `:::` line that closes no
//!      directive opens none either: [`nesting`](crate::markdown::nesting) puts a marker in
//!      its place (a bare `:::`, or a `:::` followed only by white space other than spaces
//!      and tabs, written as such or as character references, such as a no-break space or
//!      `&#32;`; a zero-width character is no white space and names the directive). The
//!      marker (an `HtmlBlock`, [`IgnoredFence`]) renders nothing but the end of its line
//!      (as the fence line ended one in a tight list), and the slide splitter skips it.
//!      [`report_ignored_fences`], which `nesting` calls once the document is parsed, makes
//!      it an `invalid-directive` warning that says why the line closes nothing: the
//!      directive around it has a longer fence, the line is indented too far, something
//!      follows the colons, the line is in a list item, block quote, footnote or definition
//!      and cannot close a directive around it (a `:::` there closes only directives opened in
//!      it), or there is a `:::` too many. A directive without a name that comrak still opens
//!      (the text left as it was) is reported the same way, and its contents are written
//!      without a `<div>`;
//!    - `CodeBlock` (§5): [`render_code_block`](crate::markdown::code::render_code_block);
//!      a ```` ```math ```` fence becomes `<div data-math-style="display">TeX</div>`. A `{…}`
//!      block comrak left in the info string is read with
//!      [`fence_attrs`] (A3);
//!    - `Math` (§8): `<span data-math-style="inline|display">TeX</span>` (also for code math);
//!    - headings, links, images and inline code with `{…}` attributes (§3.3): comrak's own
//!      tag with `id`, `class` and `key="value"` appended (comrak's formatter never writes
//!      node attributes; a `title=` attribute on a link or image only when it has no
//!      Markdown title). Doing this in the formatter rather than by rewriting nodes to raw
//!      HTML keeps the inline children (emphasis, alt text, …) rendered by comrak. Slide
//!      attributes (`layout=`, `background=`, …) still on a heading were not lifted: an
//!      `unlifted-slide-attr` warning (A3);
//!    - footnote references and definitions (pass 5).
//!
//! Every attribute value written here is escaped with `encode_double_quoted_attribute`.
//! [`Rendered::features`] reports `code` (any code block other than a math fence), `math`
//! (an element with a `data-math-style` attribute in the HTML or notes, see
//! [`has_math_markup`]; the text `data-math-style` in inline code does not count) and `notes`.

use std::borrow::Cow;
use std::cell::{OnceCell, RefCell};
use std::collections::btree_map::Entry;
use std::collections::hash_map::Entry as HashEntry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::Infallible;
use std::ffi::{OsStr, OsString};
use std::fmt::{self, Write as _};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use comrak::html::{
    ChildRendering, Context, dangerous_url, format_document_with_formatter, format_node_default,
    render_sourcepos,
};
use comrak::nodes::{
    AstNode, Attributes, NodeBlockDirective, NodeCodeBlock, NodeFootnoteDefinition,
    NodeFootnoteReference, NodeLink, NodeMath, NodeValue,
};
use comrak::options::Plugins;
use comrak::{Arena, Options, parse_document};
use html_escape::{decode_html_entities, encode_quoted_attribute};

use crate::assets::registry::{AssetRegistry, normalize_lexically};
use crate::error::{Diagnostic, Diagnostics, Error, Result};
use crate::ir::{Features, SourceSpan};
use crate::markdown::attrs::{
    ElementAttrs, ParsedAttrs, is_valid_attr_name, normalize_key, parse_attr_block,
};
use crate::markdown::code::{
    code_attrs, push_attr, render_code_block_with, strip_trailing_newline,
};
use crate::markdown::compat::is_invisible;
use crate::markdown::nesting::{IgnoredFence, Kind, as_written, is_marker, renest_directives};
use crate::markdown::recover::{fence_attrs, recover_attr_blocks, unparsed};
use crate::markdown::split::{SourceLines, first_heading_text, lift_heading_attrs};

/// The attribute every math element carries (`Features.math` is derived from it).
const MATH_ATTR: &str = "data-math-style";

/// Diagnostic code of a missing local file.
const MISSING_ASSET: &str = "missing-asset";

/// Diagnostic code of a local file found only by letter case (A3).
const ASSET_CASE_MISMATCH: &str = "asset-case-mismatch";

/// Diagnostic code of an `:::include` in a notebook (T48).
const INCLUDE_IN_NOTEBOOK: &str = crate::markdown::include::codes::INCLUDE_IN_NOTEBOOK;

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
    /// Receives `missing-asset`, `asset-case-mismatch`, `invalid-directive`,
    /// `invalid-directive-arg`, `duplicate-footnote`, `footnote-elsewhere`,
    /// `unparsed-attr-block`, `unlifted-slide-attr` and (notebook mode)
    /// `include-in-notebook` warnings. Spans are lines of the
    /// rendered text (`cell: None`) and the path is not set: the caller adds both. The missing
    /// file of a `missing-asset` warning is in its message only: see [`missing_asset_path`].
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
/// on the element. `source` is the text the nodes were parsed from, for pass 3 to read the
/// colons as typed ([`SourceLines::typed_colons`]); with `None` it reads them as comrak
/// decoded them.
///
/// Errors: [`Error::MissingAsset`] for a missing local image when
/// `ctx.missing_asset_is_error`.
pub fn render_nodes<'a>(
    arena: &'a Arena<'a>,
    nodes: &[&'a AstNode<'a>],
    source: Option<&SourceLines>,
    opts: &Options,
    ctx: &mut RenderCtx,
) -> Result<Rendered> {
    let title = first_heading_text(nodes);
    let doc = new_document(arena);
    for node in nodes {
        doc.append(node);
    }
    let mut rendered = render_document(arena, doc, source, opts, ctx)?;
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
/// matter excluded) before rendering: when the first visible node (HTML comments and
/// `:::notes` blocks before it do not count, [`is_invisible`]) is a heading with `{…}`
/// attributes, they are removed from it (the heading renders clean) and returned for the
/// caller to apply (`SlideAttrs::from_parsed(attrs).apply_to(slide)`). Otherwise, and always
/// without `lift_heading`, the second value is `None` and the attributes stay on the `<hN>`
/// element. Attribute blocks comrak rejects are read as in a deck
/// ([`recover_attr_blocks`]). Errors as [`render_nodes`].
pub fn render_fragment_with_lift(
    text: &str,
    opts: &Options,
    ctx: &mut RenderCtx,
    lift_heading: bool,
) -> Result<(Rendered, Option<ParsedAttrs>)> {
    let arena = Arena::new();
    let mut parse_opts = opts.clone();
    if ctx.mode == RenderMode::Notebook {
        parse_opts.extension.front_matter_delimiter = None;
    }
    let root = parse_document(&arena, text, &parse_opts);
    let (root, nested) = renest_directives(&arena, root, text, &parse_opts, ctx.diagnostics);
    let (root, recovered) =
        recover_attr_blocks(&arena, root, &nested, &parse_opts, ctx.diagnostics);
    if let Cow::Owned(_) = recovered {
        // a new tree, whose directives the warnings need as written
        as_written(root, text);
    }
    // without the markers of ignored `:::` lines, as the splitter leaves them out
    let nodes: Vec<_> = root
        .children()
        .filter(|node| !matches!(node.data().value, NodeValue::FrontMatter(_)) && !is_marker(node))
        .collect();
    let lifted = if lift_heading {
        let visible = nodes.iter().take_while(|node| is_invisible(node)).count();
        lift_heading_attrs(&nodes[visible..])
    } else {
        None
    };
    let source = SourceLines::new(&recovered);
    let rendered = render_nodes(&arena, &nodes, Some(&source), opts, ctx)?;
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

/// Every start tag of `html` outside comments and raw-text elements, in order: its
/// ASCII-lowercased name and the byte offset just after it (the tokenizer of
/// [`rewrite_html_assets`]). The notebook frontend reads raw HTML slide titles with it.
pub(crate) fn html_start_tags(html: &str) -> Vec<(String, usize)> {
    let mut tags = Vec::new();
    let Ok(()) = HtmlScan::default().start_tags(html, |tag| {
        tags.push((tag.name.clone(), tag.end));
        Ok::<(), Infallible>(())
    });
    tags
}

/// Every attribute value of the start tags [`html_start_tags`] finds, in order: the
/// attribute's ASCII-lowercased name and the byte range of its value (quotes excluded,
/// entities not decoded; bare attributes are left out). The notebook frontend resolves
/// attachment URLs with it.
pub(crate) fn html_attr_values(html: &str) -> Vec<(String, Range<usize>)> {
    let mut values = Vec::new();
    let Ok(()) = HtmlScan::default().start_tags(html, |tag| {
        let valued = tag
            .attrs
            .iter()
            .filter_map(|attr| Some((attr.name.clone(), attr.value.clone()?)));
        values.extend(valued);
        Ok::<(), Infallible>(())
    });
    values
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
    source: Option<&SourceLines>,
    opts: &Options,
    ctx: &mut RenderCtx,
) -> Result<Rendered> {
    drop_notebook_includes(doc, ctx);
    let (notes, mut features) = extract_notes(arena, doc, source, opts, ctx)?;
    check_unopened_directives(doc, source, ctx);
    let _listings = DirListings::enter();
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
    source: Option<&SourceLines>,
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
        let rendered = render_document(arena, notes_doc, source, opts, ctx)?;
        html.push_str(&rendered.html);
        html.push_str(rendered.notes.as_deref().unwrap_or_default());
        features.merge(rendered.features);
    }
    let notes = (!html.trim().is_empty()).then_some(html);
    Ok((notes, features))
}

fn is_notes(node: &AstNode<'_>) -> bool {
    matches!(&node.data().value,
        NodeValue::BlockDirective(directive) if is_notes_info(&directive.info))
}

/// Pass 3: a paragraph whose text starts with `:::name` (or pandoc's `::: {.name …}`) as
/// typed in `source` is a directive comrak did not open (module docs): an
/// `invalid-directive` warning on its first line, which says why and names the fix (a line
/// of its own for the text of a task item, `&#58;` for a colon; no closing `:::` after a
/// pandoc block) and, for `:::notes`, says that the speaker notes show on the slide.
fn check_unopened_directives<'a>(
    doc: &'a AstNode<'a>,
    source: Option<&SourceLines>,
    ctx: &mut RenderCtx,
) {
    for node in doc.descendants() {
        let Some(unopened) = unopened_directive(node, source) else {
            continue;
        };
        let name = &unopened.name;
        let shown = if unopened.pandoc {
            format!("::: {{.{name} …}}")
        } else {
            format!(":::{name} …")
        };
        let mut message = if starts_task_item(node) {
            format!(
                "`{shown}` is not a directive: a directive cannot start the text of a task \
                 item (comrak reads the rest of the line after the checkbox as text); start \
                 it on the next line, indented as the item's text"
            )
        } else {
            let fix = if unopened.closing_colons {
                "remove the `:::` after the attribute block (pandoc allows it, comrak does not)"
            } else {
                "write the colon as `&#58;`"
            };
            format!(
                "`{shown}` is not a directive: comrak does not open one when the line has a `:` \
                 after the fence (a URL, `title=\"a:b\"`); {fix}"
            )
        };
        if name == "notes" {
            message.push_str(". Until then the speaker notes in it are shown on the slide");
        }
        let span = node_span(node).map(|span| SourceSpan {
            end_line: span.start_line,
            ..span
        });
        ctx.diagnostics.warn("invalid-directive", message).span = span;
    }
}

/// Whether `node` is the paragraph that a task item starts with, on the item's first line:
/// comrak's tasklist extension takes the `[ ]` (or `[x]`) from the start of the item's text
/// only after it has read the line, so a `:::` after it opens no directive, and is the text.
pub(crate) fn starts_task_item<'a>(node: &'a AstNode<'a>) -> bool {
    let Some(item) = node.parent() else {
        return false;
    };
    matches!(node.data().value, NodeValue::Paragraph)
        && matches!(item.data().value, NodeValue::TaskItem(_))
        && item
            .first_child()
            .is_some_and(|first| std::ptr::eq(first, node))
        && node.data().sourcepos.start.line == item.data().sourcepos.start.line
}

/// A paragraph that starts like a directive comrak did not open (see [`unopened_directive`]).
struct Unopened {
    /// The directive's name: the word after the fence, or the first class of a pandoc block.
    name: String,
    /// Written as pandoc's `::: {.name …}`.
    pandoc: bool,
    /// A pandoc block followed by closing colons (`::: {.notes} :::`).
    closing_colons: bool,
}

/// The directive a paragraph starts like (`:::name …` or `::: {.name …}` on its first line,
/// the name starting with a letter) but is not, for pass 3; `None` for any other node, and
/// for a paragraph whose colons are not typed as such in `source` (`&#58;::name`,
/// `\:::name`: the way to show the line as text, [`SourceLines::typed_colons`]).
fn unopened_directive<'a>(node: &'a AstNode<'a>, source: Option<&SourceLines>) -> Option<Unopened> {
    if !matches!(node.data().value, NodeValue::Paragraph) {
        return None;
    }
    let first = node.first_child()?;
    let data = first.data();
    let NodeValue::Text(text) = &data.value else {
        return None;
    };
    let line = text.lines().next().unwrap_or_default();
    let rest = line.trim_start_matches(':');
    if line.len() - rest.len() < 3 {
        return None;
    }
    let rest = rest.trim_start();
    let word = |text: &str| -> String {
        text.chars()
            .take_while(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
            .collect()
    };
    let unopened = match rest.strip_prefix('{') {
        // the first `.class`; the text may stop early (an autolink or HTML in the block)
        Some(block) => Unopened {
            name: block
                .split_whitespace()
                .find_map(|token| token.strip_prefix('.'))
                .map(word)
                .unwrap_or_default(),
            pandoc: true,
            closing_colons: rest.trim_end().ends_with(':'),
        },
        None => Unopened {
            name: word(rest),
            pandoc: false,
            closing_colons: false,
        },
    };
    let typed = || source.is_none_or(|source| source.typed_colons(node));
    (unopened.name.starts_with(char::is_alphabetic) && typed()).then_some(unopened)
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
    let decoded = percent_decode(path);
    let file = ctx.base_dir.join(&decoded);
    if file.is_file() {
        if let Some(on_disk) = case_mismatch(ctx.base_dir, &decoded) {
            let message = format!(
                "{what} `{url}` matches `{on_disk}` only by letter case: it is found on this \
                 file system, but not on a case-sensitive one (Linux); write `{on_disk}`"
            );
            ctx.diagnostics.warn(ASSET_CASE_MISMATCH, message).span = span.cloned();
        }
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

/// The spelling on disk of `relative` (a path under `base` that exists) when some segment
/// of it matches the file system's entry only by letter case, which a case-insensitive file
/// system (macOS, Windows) accepts and a case-sensitive one (Linux) does not: each segment
/// is compared exactly with the names `read_dir` lists, as theme files are (A3). `.` is
/// skipped and `..` goes up unchecked. `None` when every segment matches exactly (or a
/// directory cannot be listed). A directory that finds a name by its exact spelling only
/// (case-sensitive) is not listed ([`exact_lookup`]). Listings are cached while a
/// [`DirListings`] guard is open.
fn case_mismatch(base: &Path, relative: &str) -> Option<String> {
    let mut dir = base.to_path_buf();
    let mut on_disk: Vec<String> = Vec::new();
    let mut mismatch = false;
    for component in Path::new(relative).components() {
        let name = match component {
            std::path::Component::CurDir => continue,
            std::path::Component::ParentDir => OsString::from(".."),
            std::path::Component::Normal(name) => {
                let spelled = DirListings::spelling(&dir, name)?;
                mismatch |= spelled != name;
                spelled
            }
            _ => return None,
        };
        on_disk.push(name.to_string_lossy().into_owned());
        dir.push(name);
    }
    mismatch.then(|| on_disk.join("/"))
}

/// `name` in lowercase, for comparisons that ignore letter case.
fn folded(name: &OsStr) -> String {
    name.to_string_lossy().to_lowercase()
}

/// What a lookup of `name` in `dir` tells without listing `dir` ([`case_mismatch`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lookup {
    /// `name` has no letter case (`2024`): a name it matches only by letter case would be
    /// itself, so it is spelled as on disk.
    Uncased,
    /// `dir` has `name`, and not `name` with its ASCII letters in the other case: `dir`
    /// finds names by their exact spelling (a case-sensitive file system, Linux), so every
    /// name it has is spelled as on disk.
    ExactOnly,
    /// Only a listing tells (a case-insensitive file system, macOS; or `dir` has both
    /// spellings, or `name` is not there).
    Unknown,
}

/// [`Lookup`] of `name` in `dir`, with at most two `lstat` calls: on a case-sensitive file
/// system the check of a reference needs no listing (wave-8 review of the A3 fixes: a
/// `figures/` folder of thousands of files was listed for each build).
fn exact_lookup(dir: &Path, name: &OsStr) -> Lookup {
    let Some(text) = name.to_str() else {
        return Lookup::Unknown;
    };
    if text.to_lowercase() == text && text.to_uppercase() == text {
        return Lookup::Uncased;
    }
    if !text.bytes().any(|byte| byte.is_ascii_alphabetic()) {
        return Lookup::Unknown;
    }
    // Swapping ASCII letters gives a name every case-insensitive file system takes for
    // `name`: there a lookup of it always succeeds.
    let swapped: String = text
        .chars()
        .map(|c| match c.is_ascii_uppercase() {
            true => c.to_ascii_lowercase(),
            false => c.to_ascii_uppercase(),
        })
        .collect();
    let has = |name: &str| std::fs::symlink_metadata(dir.join(name)).is_ok();
    match has(text) && !has(&swapped) {
        true => Lookup::ExactOnly,
        false => Lookup::Unknown,
    }
}

/// The entry names of a directory, for [`case_mismatch`].
struct Listing {
    /// Every name as listed.
    exact: HashSet<OsString>,
    /// Lowercased name → the smallest listed name with that lowercase form: built when the
    /// first name misses `exact`, so a deck whose names all match exactly never lowercases
    /// a listing.
    folded: OnceCell<HashMap<String, OsString>>,
}

impl Listing {
    fn read(dir: &Path) -> Option<Listing> {
        count_directory_read();
        let names = std::fs::read_dir(dir).ok()?.flatten();
        Some(Listing {
            exact: names.map(|entry| entry.file_name()).collect(),
            folded: OnceCell::new(),
        })
    }

    /// The listed name that `name` names: itself when listed, else the smallest listed name
    /// that differs only by letter case; `None` when there is none.
    fn spelling(&self, name: &OsStr) -> Option<OsString> {
        if self.exact.contains(name) {
            return Some(name.to_os_string());
        }
        let folded_names = self.folded.get_or_init(|| {
            let mut map: HashMap<String, OsString> = HashMap::new();
            for listed in &self.exact {
                match map.entry(folded(listed)) {
                    HashEntry::Occupied(mut kept) if listed < kept.get() => {
                        kept.insert(listed.clone());
                    }
                    HashEntry::Occupied(_) => {}
                    HashEntry::Vacant(slot) => {
                        slot.insert(listed.clone());
                    }
                }
            }
            map
        });
        folded_names.get(&folded(name)).cloned()
    }

    /// [`Listing::spelling`] of `name` in `dir` without keeping a listing (no guard is
    /// open): the scan stops at an exact match, and only a name that misses compares the
    /// entries in lowercase.
    fn scan(dir: &Path, name: &OsStr) -> Option<OsString> {
        count_directory_read();
        let mut listed: Vec<OsString> = Vec::new();
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let entry = entry.file_name();
            if entry == name {
                return Some(entry);
            }
            listed.push(entry);
        }
        let wanted = folded(name);
        listed.into_iter().filter(|e| folded(e) == wanted).min()
    }
}

#[cfg(test)]
thread_local! {
    /// How many directories [`Listing`] read on this thread (tests count them).
    static DIRECTORY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Counts one directory listing read (`directory_reads`, in tests); nothing outside tests.
fn count_directory_read() {
    #[cfg(test)]
    DIRECTORY_READS.set(DIRECTORY_READS.get() + 1);
}

/// How many directories the `asset-case-mismatch` check listed on this thread so far.
#[cfg(test)]
pub(crate) fn directory_reads() -> usize {
    DIRECTORY_READS.get()
}

/// What the open [`DirListings`] guards keep of a directory.
#[derive(Clone)]
enum Known {
    /// It finds names by their exact spelling ([`Lookup::ExactOnly`]): not listed.
    ExactOnly,
    /// Its listing (`None`: it cannot be listed).
    Listed(Option<Rc<Listing>>),
}

/// The open [`DirListings`] guards on a thread and what they know of each directory.
type ListingCache = (usize, HashMap<PathBuf, Known>);

thread_local! {
    /// This thread's [`ListingCache`].
    static LISTINGS: RefCell<ListingCache> = RefCell::new((0, HashMap::new()));
}

/// While a guard is alive, [`case_mismatch`] lists each directory once on this thread
/// instead of once per reference and path segment (a deck next to a large `figures/`
/// folder would otherwise list it for every image), and a directory that finds names by
/// their exact spelling ([`exact_lookup`]) is probed once and never listed. The outermost
/// guard forgets what it learnt when dropped, so a rebuild sees the files as they are then.
/// A deck parse and a notebook parse hold one for the whole deck, and each rendered slide
/// or fragment one of its own. Without a guard, each lookup probes the directory and, when
/// that does not tell, scans it up to an exact match.
pub(crate) struct DirListings(());

impl DirListings {
    /// Opens a guard (nested guards share the outermost one's listings).
    pub(crate) fn enter() -> DirListings {
        LISTINGS.with_borrow_mut(|(open, _)| *open += 1);
        DirListings(())
    }

    /// The name in `dir` that `name` names ([`Listing::spelling`]): `name` itself when a
    /// lookup tells ([`exact_lookup`]), else from the listing cached while a guard is open
    /// (read now if it is not yet), else from a scan of `dir`. `None` when there is none or
    /// the directory cannot be listed.
    fn spelling(dir: &Path, name: &OsStr) -> Option<OsString> {
        let (open, cached) =
            LISTINGS.with_borrow(|(open, listings)| (*open > 0, listings.get(dir).cloned()));
        let listing = match cached {
            Some(Known::ExactOnly) => return Some(name.to_os_string()),
            Some(Known::Listed(listing)) => listing,
            None => {
                let lookup = exact_lookup(dir, name);
                if lookup != Lookup::Unknown {
                    if open && lookup == Lookup::ExactOnly {
                        Self::remember(dir, Known::ExactOnly);
                    }
                    return Some(name.to_os_string());
                }
                if !open {
                    return Listing::scan(dir, name);
                }
                let listing = Listing::read(dir).map(Rc::new);
                Self::remember(dir, Known::Listed(listing.clone()));
                listing
            }
        };
        listing?.spelling(name)
    }

    /// Keeps `known` for `dir` until the outermost guard is dropped.
    fn remember(dir: &Path, known: Known) {
        LISTINGS.with_borrow_mut(|(_, listings)| {
            listings.insert(dir.to_path_buf(), known);
        });
    }
}

impl Drop for DirListings {
    fn drop(&mut self) {
        LISTINGS.with_borrow_mut(|(open, listings)| {
            *open = open.saturating_sub(1);
            if *open == 0 {
                listings.clear();
            }
        });
    }
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

/// Decodes `%XX` escapes (exactly two hex digits: `%+F` stays as written); the input
/// unchanged when the result is not UTF-8. Also used for notebook attachment names.
pub(crate) fn percent_decode(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
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
    if is_marker(node) {
        // a `:::` line the dialect ignores (reported by `nesting`): it renders nothing, and
        // ends the line, as the fence line ended a paragraph that comrak writes without `<p>`
        // in a tight list
        if entering {
            context.cr()?;
        }
        return Ok(ChildRendering::Skip);
    }
    let attrs = data.attrs.as_deref();
    match (&data.value, attrs) {
        (NodeValue::BlockDirective(directive), _) => {
            write_directive(context, node, directive, entering)
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

/// `<div class="name classes…" data-…>` … `</div>` (dialect §4). A directive without a
/// name is a `:::` line that comrak still reads as opening one where the dialect ignores the
/// line (when [`nesting`](crate::markdown::nesting) left the text as it was, which reports
/// it as it does a marker, [`report_ignored_fences`]): its contents are written without a
/// `<div>`, on lines of their own.
fn write_directive<'a>(
    context: &mut Context<'_, '_, FormatState>,
    node: &'a AstNode<'a>,
    directive: &NodeBlockDirective,
    entering: bool,
) -> FormatResult {
    let info = directive.info.as_str();
    if info.trim().is_empty() {
        context.cr()?;
        return Ok(ChildRendering::HTML);
    }
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

/// The `invalid-directive` warnings (A3) for the `:::` lines that the dialect ignores (§4)
/// in `root`, the tree [`renest_directives`] returns, in line order: their markers, and the
/// directives without a name that comrak still opens where the text was left as it was.
/// Reported for the whole text at once, so that each warning sees the directives and
/// containers around its line, also in speaker notes and on slides that are never rendered
/// (an `:::include` slide). `source` is the text of `root`. `around_footnote` gives the
/// label of the innermost directive around a footnote definition of `root` (which comrak may
/// have moved to the end of the document), if any.
pub(crate) fn report_ignored_fences<'a>(
    root: &'a AstNode<'a>,
    source: &SourceLines,
    diagnostics: &mut Diagnostics,
    around_footnote: &mut dyn FnMut(&'a AstNode<'a>) -> Option<String>,
) {
    for node in root.descendants() {
        let fence = match &node.data().value {
            NodeValue::HtmlBlock(block) => IgnoredFence::from_html(&block.literal),
            // comrak trims ASCII white space after decoding references: what is left is
            // another kind of space, and `as_written` gives a line whose references decoded
            // to nothing a one-space info
            NodeValue::BlockDirective(directive) if directive.info.trim().is_empty() => {
                Some(IgnoredFence {
                    colons: directive.fence_length,
                    offset: directive.fence_offset,
                    odd: !directive.info.is_empty(),
                })
            }
            _ => None,
        };
        if let Some(fence) = fence {
            report_ignored_fence(diagnostics, node, fence, source, around_footnote);
        }
    }
}

/// The `invalid-directive` warning for `fence`, a `:::` line that the dialect ignores, at
/// `node`: its marker, or a directive comrak still opens there. It says why the line closes
/// no directive ([`ignored_fence_message`]), or, for a `:::` followed only by white space
/// other than spaces and tabs (written as such or as character references), that it is no
/// closing fence at all ([`odd_fence_message`]); unless a `:::name` paragraph that pass 3
/// reports comes before it among its siblings, after any directive or other ignored line
/// (then it is that one's closing fence, and that line's warning says it all). A paragraph
/// whose colons are escaped or written as character references is no such line: the author
/// showed it as text, and a `:::` after it is reported.
fn report_ignored_fence<'a>(
    diagnostics: &mut Diagnostics,
    node: &'a AstNode<'a>,
    fence: IgnoredFence,
    source: &SourceLines,
    around_footnote: &mut dyn FnMut(&'a AstNode<'a>) -> Option<String>,
) {
    let after_unopened = node
        .preceding_siblings()
        .skip(1)
        .take_while(|&sibling| {
            !matches!(sibling.data().value, NodeValue::BlockDirective(_)) && !is_marker(sibling)
        })
        .any(|sibling| unopened_directive(sibling, Some(source)).is_some());
    if after_unopened {
        return;
    }
    let message = if fence.odd {
        odd_fence_message()
    } else {
        ignored_fence_message(node, fence, around_footnote)
    };
    diagnostics.warn("invalid-directive", message).span = node_span(node).map(|span| SourceSpan {
        end_line: span.start_line,
        ..span
    });
}

/// The warning for `fence`, a bare `:::` line at `node` that the dialect ignores (see
/// [`report_ignored_fence`]): that it is ignored, then why it closes nothing (dialect §4).
/// The tree's fence lengths are those written ([`as_written`]), and a line followed by
/// something that reads as nothing has its own warning ([`odd_fence_message`]). So the line
/// cannot close the directive right around it in its list item, block quote, footnote or
/// definition (or the document) only when that one's fence is longer, or when the line is
/// indented 4 columns or more where that directive's closing fence is looked for (comrak
/// looks before it strips the directive's own indentation): the dialect would have closed it
/// otherwise. A directive around the container it is in it never closes (a footnote
/// definition, which comrak moves to the end of the document, is in the directives its lines
/// are in, which `around_footnote` names). Otherwise there is a `:::` too many. The message
/// names no lines: in a notebook cell they would be the cell's own.
fn ignored_fence_message<'a>(
    node: &'a AstNode<'a>,
    fence: IgnoredFence,
    around_footnote: &mut dyn FnMut(&'a AstNode<'a>) -> Option<String>,
) -> String {
    let (own_length, own_offset) = (fence.colons, fence.offset);
    // the innermost container the line is in; the innermost directive around the line in
    // it (label, fence length and indentation), else the innermost one around the container
    let mut container = None;
    let mut around = None;
    let mut outer = None;
    for ancestor in node.ancestors().skip(1) {
        match &ancestor.data().value {
            NodeValue::Document => break,
            // comrak may have moved the footnote definition out of the blocks it is in, to the
            // end of the document
            NodeValue::FootnoteDefinition(_) => {
                container.get_or_insert(Kind::Footnote.name());
                if around.is_none() && outer.is_none() {
                    outer = around_footnote(ancestor);
                }
                break;
            }
            NodeValue::BlockDirective(directive) => {
                let label = directive_label(&directive.info);
                if container.is_none() {
                    around.get_or_insert((label, directive.fence_length, directive.fence_offset));
                } else if around.is_none() {
                    outer.get_or_insert(label);
                }
            }
            value => {
                if container.is_none() {
                    container = Kind::of(value).map(Kind::name);
                }
            }
        }
    }
    let mut message = "this `:::` line closes no directive, so it is ignored.".to_string();
    let reason = match &around {
        Some((label, _, offset)) if offset + own_offset >= 4 => Some(format!(
            " It cannot close the {label} around it: this line is indented by 4 columns or \
             more where that directive's closing fence is looked for, and a closing fence may \
             be indented by 3 at most."
        )),
        Some((label, length, _)) if *length > own_length => Some(format!(
            " It cannot close the {label} around it, whose fence is longer: a closing fence \
             needs at least as many colons as its opening fence."
        )),
        Some(_) => None,
        None => container.zip(outer).map(|(kind, label)| {
            format!(
                " It cannot close the {label} around the {kind} it is in: a `:::` in a {kind} \
                 closes only directives opened in it."
            )
        }),
    };
    let hedged = reason.is_some();
    message.push_str(&reason.unwrap_or_default());
    message.push_str(if hedged {
        " Otherwise, there is"
    } else {
        " There is"
    });
    message.push_str(
        " one `:::` too many: this line, or one above it that closed the directive this line \
         was meant for.",
    );
    message
}

/// How a warning names the directive with `info`: by its name, by its attributes for a
/// pandoc block without a class (`::: {#intro}`), or as the unnamed directive.
pub(crate) fn directive_label(info: &str) -> String {
    let name = directive_info(info).name;
    if !name.is_empty() {
        format!("`{name}` directive")
    } else if !info.trim().is_empty() {
        format!("`{}` directive", info.trim())
    } else {
        "unnamed directive".to_string()
    }
}

/// The warning for a `:::` line followed only by white space other than spaces and tabs
/// (written as such or as character references), which closes no directive and which the
/// dialect therefore ignores (see [`report_ignored_fence`]).
fn odd_fence_message() -> String {
    "this `:::` line is followed by something that is not a plain space or tab, though it \
     reads as one or as nothing (a no-break or full-width space, or a character reference \
     such as `&#32;`), so it closes no directive, and it is ignored. Delete what follows the \
     colons"
        .to_string()
}

/// A directive's info string, read as dialect §4 writes it (`name class… key=value…`) or as
/// pandoc does (`{.name .class #id key=value}`, A3 contract note).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct DirectiveInfo {
    /// The name: the first token, or the first class of a pandoc block; empty for a bare
    /// `:::` (or a pandoc block without a class).
    pub(crate) name: String,
    /// The other classes, in order.
    pub(crate) classes: Vec<String>,
    /// The `#id` of a pandoc block.
    pub(crate) id: Option<String>,
    /// `key=value` args in order (a pandoc block's bare keys have an empty value).
    pub(crate) args: Vec<(String, String)>,
    /// Problems found while reading it (an unbalanced quote), for warnings.
    pub(crate) problems: Vec<String>,
}

/// Reads a directive's info string (see [`DirectiveInfo`]). It is a pandoc attribute block
/// when it starts with `{` and ends with `}` (after trimming): `::: {.fragment .fade-up}` is
/// `:::fragment fade-up`, `:::: {.columns}` is `::::columns`, `::: {.column width="40%"}` is
/// `:::column width=40%`, `::: {#intro .note}` is `:::note` with `id="intro"` and
/// `::: {.notes}` is `:::notes`. Otherwise it is split on whitespace outside quotes: the
/// first token is the name, tokens with `=` are args, the others classes.
pub(crate) fn directive_info(info: &str) -> DirectiveInfo {
    let trimmed = info.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return match parse_attr_block(trimmed) {
            Ok(parsed) => {
                let mut classes = parsed.classes;
                let mut id = parsed.id;
                let mut args = Vec::new();
                for (key, value) in parsed.pairs {
                    match key.to_ascii_lowercase().as_str() {
                        "class" => classes.extend(value.split_whitespace().map(str::to_string)),
                        "id" => id = Some(value),
                        _ => args.push((key, value)),
                    }
                }
                let mut classes = classes.into_iter().filter(|c| !c.is_empty());
                DirectiveInfo {
                    name: classes.next().unwrap_or_default(),
                    classes: classes.collect(),
                    id: id.filter(|id| !id.is_empty()),
                    args,
                    problems: Vec::new(),
                }
            }
            Err(err) => DirectiveInfo {
                problems: vec![format!("{err} in directive `:::{info}`")],
                ..DirectiveInfo::default()
            },
        };
    }
    let (tokens, unbalanced) = split_info(info);
    let mut out = DirectiveInfo::default();
    if let Some(quote) = unbalanced {
        out.problems.push(format!(
            "unbalanced {quote} quote in directive `:::{info}` (read to the end of the line)"
        ));
    }
    let mut tokens = tokens.into_iter();
    out.name = tokens.next().map(|token| token.text).unwrap_or_default();
    for token in tokens {
        match token.eq {
            Some(eq) => {
                let (key, value) = (&token.text[..eq], &token.text[eq + 1..]);
                out.args.push((key.to_string(), value.to_string()));
            }
            None => out.classes.push(token.text),
        }
    }
    out
}

/// Whether a directive's info string names `:::notes` (also pandoc's `::: {.notes}`).
pub(crate) fn is_notes_info(info: &str) -> bool {
    directive_info(info).name == "notes"
}

/// Whether a directive's info string names `:::include` (dialect §4.1, T48; also
/// `::: {.include}`, which the Markdown frontend rejects with a hint).
pub(crate) fn is_include_info(info: &str) -> bool {
    directive_info(info).name == "include"
}

/// Pass 0, in [`RenderMode::Notebook`] only (notebook markdown cells and outputs, the
/// `markdown` template filter): every `:::include` directive under `doc` is replaced by its
/// children (none for a closed one; the rest of the cell, which comrak puts inside one that
/// is not closed) with an `include-in-notebook` warning on its line, so the directive itself
/// renders nothing and no content is lost. Only a Markdown deck includes files (dialect
/// §4.1); the Markdown frontend consumes or rejects every `:::include` before rendering, so
/// in [`RenderMode::Deck`] there is none.
fn drop_notebook_includes<'a>(doc: &'a AstNode<'a>, ctx: &mut RenderCtx) {
    if ctx.mode != RenderMode::Notebook {
        return;
    }
    let found: Vec<_> = doc
        .descendants()
        .filter(|node| {
            matches!(&node.data().value,
                NodeValue::BlockDirective(directive) if is_include_info(&directive.info))
        })
        .collect();
    // Document order: an include inside another one is moved out with the other's children
    // first, then replaced in turn.
    for node in found {
        let message = "`:::include` works only in a Markdown deck, not in a notebook: nothing is \
                       included and the directive is not shown (include the notebook from a \
                       Markdown deck instead)";
        let span = node_span(node).map(|span| SourceSpan {
            end_line: span.start_line,
            ..span
        });
        ctx.diagnostics.warn(INCLUDE_IN_NOTEBOOK, message).span = span;
        while let Some(child) = node.first_child() {
            node.insert_before(child);
        }
        node.detach();
    }
}

/// The attributes of a directive's `<div>` (leading space included) and the problems found
/// in its info string (dialect §4): `id` (pandoc form), `class` (the name, then the other
/// classes, deduplicated), then the other attributes by name. `:::fragment index=N` needs a
/// non-negative integer N (anything else would hide the fragment for good in reveal.js): it
/// is written normalized, and any other value is dropped with a problem (A3).
fn directive_attrs(info: &str) -> (String, Vec<String>) {
    let directive = directive_info(info);
    let mut problems = directive.problems;
    let name = directive.name;
    let mut classes = vec![name.clone()];
    classes.extend(directive.classes);
    let mut attrs: BTreeMap<String, String> = BTreeMap::new();
    for (key, value) in directive.args {
        let lower = key.to_ascii_lowercase();
        match (name.as_str(), lower.as_str()) {
            ("fragment", "index") => match value.trim().parse::<u32>() {
                Ok(index) => {
                    attrs.insert("data-fragment-index".into(), index.to_string());
                }
                Err(_) => problems.push(format!(
                    "`index={value}` in directive `:::{info}` is not a non-negative integer \
                     (dropped; reveal.js would never show the fragment)"
                )),
            },
            ("columns", "widths") => {
                attrs.insert("style".into(), format!("grid-template-columns: {value}"));
            }
            // consumed: the dialect §4 table renders `:::column width=…` as `<div class="column">`
            ("column", "width") => {}
            _ if !is_valid_attr_name(&lower) => problems.push(format!(
                "`{key}` in directive `:::{info}` is not a valid attribute name (dropped)"
            )),
            _ if lower.starts_with("data-") => {
                attrs.insert(lower, value);
            }
            _ => {
                attrs.insert(format!("data-{lower}"), value);
            }
        }
    }
    let mut unique: Vec<String> = Vec::new();
    for class in classes {
        if !class.is_empty() && !unique.contains(&class) {
            unique.push(class);
        }
    }
    let mut html = String::new();
    if let Some(id) = &directive.id {
        push_attr(&mut html, "id", id);
    }
    if !unique.is_empty() {
        push_attr(&mut html, "class", &unique.join(" "));
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
    let (info, parsed) = fence_info(context, node, &block.info, attrs);
    context.cr()?;
    if is_math_fence(info) {
        context.write_str("<div")?;
        render_sourcepos(context, node)?;
        context.write_str(" data-math-style=\"display\"")?;
        if let Some(parsed) = &parsed {
            let attrs = Attributes {
                id: parsed.id.clone(),
                classes: parsed.classes.clone(),
                pairs: parsed.pairs.clone(),
            };
            context.write_str(&element_attrs(&attrs, &[MATH_ATTR]))?;
        }
        context.write_str(">")?;
        context.escape(strip_trailing_newline(&block.literal))?;
        context.write_str("</div>")?;
    } else {
        let code = code_attrs(info, parsed.as_ref());
        context.write_str(&render_code_block_with(
            &block.literal,
            &code,
            sourcepos.as_deref(),
        ))?;
    }
    context.lf()?;
    Ok(ChildRendering::HTML)
}

/// A fence's info string and attributes: comrak's, or, when comrak left a `{…}` block it
/// rejected at the end of the info string, the info without it and the block read by
/// [`fence_attrs`] (dialect §3.1, A3). A block meant as attributes that is not valid stays in
/// the info string and is an `unparsed-attr-block` warning.
fn fence_info<'i, 'a>(
    context: &mut Context<'_, '_, FormatState>,
    node: &'a AstNode<'a>,
    info: &'i str,
    attrs: Option<&Attributes>,
) -> (&'i str, Option<ParsedAttrs>) {
    if let Some(attrs) = attrs {
        return (info, Some(ParsedAttrs::from(attrs)));
    }
    match fence_attrs(info) {
        Some((rest, _, ElementAttrs::Attrs(parsed))) => (rest, Some(parsed)),
        Some((_, block, ElementAttrs::Invalid(reason))) => {
            let mut diagnostic = unparsed("code fence's language", block, &reason);
            diagnostic.span = node_span(node).map(|span| SourceSpan {
                end_line: span.start_line,
                ..span
            });
            context.user.diagnostics.push(diagnostic);
            (info, None)
        }
        _ => (info, None),
    }
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

/// A heading with `{…}` attributes (§3.3): comrak's `<hN>` plus the attributes. Slide
/// attributes still on it ([`slide_only_keys`]) were not lifted onto the slide: an
/// `unlifted-slide-attr` warning (A3).
fn write_heading<'a>(
    context: &mut Context<'_, '_, FormatState>,
    node: &'a AstNode<'a>,
    level: u8,
    attrs: &Attributes,
    entering: bool,
) -> FormatResult {
    if entering {
        let keys = slide_only_keys(attrs);
        if !keys.is_empty() {
            let message = format!(
                "{keys} on this heading {does} not apply to the slide and {is} written on the \
                 `<h{level}>`: slide attributes on a heading are lifted only when the heading \
                 is the slide's first visible element (only HTML comments and `:::notes` \
                 blocks may come before it)",
                keys = keys.join(", "),
                does = if keys.len() == 1 { "does" } else { "do" },
                is = if keys.len() == 1 { "is" } else { "are" },
            );
            let mut diagnostic = Diagnostic::warning("unlifted-slide-attr", message);
            diagnostic.span = node_span(node);
            context.user.diagnostics.push(diagnostic);
        }
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

/// The `key=value` pairs of a heading's attributes that only mean something on a slide
/// (`layout`, `background` and the other dialect §3.2 shorthands; not `data-`/`aria-`
/// keys, which elements use too), as `` `key=value` ``, in order.
fn slide_only_keys(attrs: &Attributes) -> Vec<String> {
    attrs
        .pairs
        .iter()
        .filter(|(key, _)| {
            let lower = key.to_ascii_lowercase();
            let verbatim = lower.starts_with("data-") || lower.starts_with("aria-");
            lower == "layout" || (!verbatim && normalize_key(&lower, "").1)
        })
        .map(|(key, value)| format!("`{key}={value}`"))
        .collect()
}

/// A link with `{…}` attributes (§3.3): comrak's `<a href title>` plus the attributes; a
/// `title=` attribute is written when the link has no Markdown title (A3).
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
    let reserved: &[&str] = if link.title.is_empty() {
        &["href"]
    } else {
        &["href", "title"]
    };
    context.write_str(&element_attrs(attrs, reserved))?;
    context.write_str(">")?;
    Ok(ChildRendering::HTML)
}

/// An image with `{…}` attributes (§3.3): comrak's `<img src alt title />` plus the
/// attributes; a `title=` attribute is written when the image has no Markdown title (A3).
/// The alt text is the image's children rendered as plain text.
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
    let reserved: &[&str] = if link.title.is_empty() {
        &["src", "alt"]
    } else {
        &["src", "alt", "title"]
    };
    context.write_str(&element_attrs(attrs, reserved))?;
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
        file: None,
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

    /// A3 (F26): pandoc's attribute block as a directive's info string.
    #[test]
    fn pandoc_directive_info() {
        let info =
            |name: &str, classes: &[&str], id: Option<&str>, args: &[(&str, &str)]| DirectiveInfo {
                name: name.to_string(),
                classes: classes.iter().map(|c| c.to_string()).collect(),
                id: id.map(str::to_string),
                args: args
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                problems: Vec::new(),
            };
        let cases = [
            ("{.fragment}", info("fragment", &[], None, &[])),
            (
                " {.fragment .fade-up} ",
                info("fragment", &["fade-up"], None, &[]),
            ),
            (
                "{.column width=\"40%\"}",
                info("column", &[], None, &[("width", "40%")]),
            ),
            ("{#intro .note}", info("note", &[], Some("intro"), &[])),
            (
                "{.a class=\"b c\" id=x k='v w'}",
                info("a", &["b", "c"], Some("x"), &[("k", "v w")]),
            ),
            ("{#only}", info("", &[], Some("only"), &[])),
            (
                "columns widths=\"1fr 2fr\" big",
                info("columns", &["big"], None, &[("widths", "1fr 2fr")]),
            ),
            ("", info("", &[], None, &[])),
        ];
        for (text, expected) in cases {
            assert_eq!(directive_info(text), expected, "{text:?}");
        }
        assert!(is_notes_info("{.notes}") && is_notes_info("notes") && is_notes_info(" notes x"));
        assert!(!is_notes_info("{.fragment .notes}") && !is_notes_info("note"));
        let problems = directive_info("{.a k='v}").problems;
        assert_eq!(problems.len(), 1, "{problems:?}");
        let html = render(
            "::: {.fragment index=1}\nx\n:::\n\n:::: {.columns widths=\"1fr 2fr\"}\n::: {#c .column width=1}\ny\n:::\n::::\n\n::: {.notes}\nn\n:::\n",
        );
        assert_eq!(
            html.html,
            "<div class=\"fragment\" data-fragment-index=\"1\">\n<p>x</p>\n</div>\n\
             <div class=\"columns\" style=\"grid-template-columns: 1fr 2fr\">\n\
             <div id=\"c\" class=\"column\">\n<p>y</p>\n</div>\n</div>\n"
        );
        assert_eq!(html.notes.as_deref(), Some("<p>n</p>\n"));
    }

    /// A3 fix round: directory listings for `asset-case-mismatch` are read once while a
    /// guard is open, and read again after the outermost guard is dropped (a rebuild). A
    /// case-sensitive file system lists nothing ([`exact_lookups_need_no_listing`]).
    #[test]
    fn case_listings_are_cached_while_a_guard_is_open() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Figs")).unwrap();
        std::fs::write(dir.path().join("Figs/a.png"), b"x").unwrap();
        if !dir.path().join("FIGS/A.PNG").exists() {
            // Case-sensitive: names are found by their exact spelling (`figs/a.png` is not
            // there), so the check of a file that exists needs no listing.
            let _deck = DirListings::enter();
            let before = directory_reads();
            assert_eq!(case_mismatch(dir.path(), "Figs/./a.png"), None);
            assert_eq!(case_mismatch(dir.path(), "Figs/a.png"), None);
            assert_eq!(directory_reads() - before, 0);
            return;
        }
        assert_eq!(
            case_mismatch(dir.path(), "figs/./a.png").as_deref(),
            Some("Figs/a.png")
        );
        {
            let _deck = DirListings::enter();
            let before = directory_reads();
            assert_eq!(case_mismatch(dir.path(), "Figs/a.png"), None);
            assert_eq!(
                case_mismatch(dir.path(), "figs/a.png").as_deref(),
                Some("Figs/a.png")
            );
            assert_eq!(
                case_mismatch(dir.path(), "figs/./a.png").as_deref(),
                Some("Figs/a.png")
            );
            assert_eq!(
                directory_reads() - before,
                2,
                "each directory is listed once"
            );
            std::fs::write(dir.path().join("Figs/B.png"), b"x").unwrap();
            // the listing read above is reused, and it has no `B.png`
            assert_eq!(case_mismatch(dir.path(), "Figs/b.png"), None);
            drop(DirListings::enter());
            assert_eq!(case_mismatch(dir.path(), "Figs/b.png"), None);
        }
        assert_eq!(
            case_mismatch(dir.path(), "Figs/b.png").as_deref(),
            Some("Figs/B.png")
        );
        assert_eq!(case_mismatch(dir.path(), "Figs/../Figs/B.png"), None);
    }

    /// Wave-8 review of the A3 fixes: a name without letter case, and every name of a
    /// directory that finds names by their exact spelling (a case-sensitive file system),
    /// is checked without listing the directory; on a case-insensitive file system each
    /// directory is listed once (per guard).
    #[test]
    fn exact_lookups_need_no_listing() {
        let dir = tempfile::tempdir().unwrap();
        let year = dir.path().join("2024");
        std::fs::create_dir_all(year.join("Figs")).unwrap();
        std::fs::write(year.join("01"), b"x").unwrap();
        std::fs::write(year.join("Figs/a.png"), b"x").unwrap();
        let insensitive = year.join("FIGS/A.PNG").exists();
        assert_eq!(
            exact_lookup(dir.path(), OsStr::new("2024")),
            Lookup::Uncased
        );
        assert_eq!(exact_lookup(&year, OsStr::new("01")), Lookup::Uncased);
        let expected = match insensitive {
            true => Lookup::Unknown,
            false => Lookup::ExactOnly,
        };
        assert_eq!(exact_lookup(&year, OsStr::new("Figs")), expected);
        // A name that is not there is never taken for exact; nor one without ASCII letters.
        assert_eq!(exact_lookup(&year, OsStr::new("gone")), Lookup::Unknown);
        assert_eq!(exact_lookup(&year, OsStr::new("é")), Lookup::Unknown);
        {
            let _deck = DirListings::enter();
            let before = directory_reads();
            assert_eq!(case_mismatch(dir.path(), "2024/01"), None);
            assert_eq!(directory_reads() - before, 0, "names without letter case");
            for _ in 0..3 {
                assert_eq!(case_mismatch(dir.path(), "2024/./Figs/a.png"), None);
            }
            let listed = directory_reads() - before;
            assert_eq!(
                listed,
                if insensitive { 2 } else { 0 },
                "2024/ and Figs/, once"
            );
        }
        if !insensitive {
            // Both spellings in one directory: only its listing tells them apart.
            std::fs::write(year.join("Figs/A.PNG"), b"x").unwrap();
            let figs = year.join("Figs");
            assert_eq!(exact_lookup(&figs, OsStr::new("a.png")), Lookup::Unknown);
            let before = directory_reads();
            assert_eq!(case_mismatch(dir.path(), "2024/Figs/A.PNG"), None);
            assert_eq!(directory_reads() - before, 1);
        }
    }

    /// A3 (F14): only a non-negative integer is a fragment index.
    #[test]
    fn fragment_indices_must_be_integers() {
        for (index, expected) in [
            ("3", Some("3")),
            ("\" 2 \"", Some("2")),
            ("+4", Some("4")),
            ("007", Some("7")),
        ] {
            let (rendered, _, codes) = render_with(
                &format!(":::fragment index={index}\nx\n:::\n"),
                RenderMode::Deck,
                false,
            );
            assert_eq!(codes, [], "{index}");
            let attr = expected.map(|n| format!(" data-fragment-index=\"{n}\""));
            assert!(
                rendered.html.starts_with(&format!(
                    "<div class=\"fragment\"{}>",
                    attr.unwrap_or_default()
                )),
                "{index}: {}",
                rendered.html
            );
        }
        for index in ["first", "", "-1", "1.5", "99999999999"] {
            let (rendered, _, codes) = render_with(
                &format!("Text\n\n:::fragment index={index}\nx\n:::\n"),
                RenderMode::Deck,
                false,
            );
            assert_eq!(codes, [("invalid-directive-arg", 3)], "{index}");
            assert!(
                rendered.html.contains("<div class=\"fragment\">\n"),
                "{index}: {}",
                rendered.html
            );
        }
        // other directives keep `index=` as data
        assert!(
            render(":::box index=a\nx\n:::\n")
                .html
                .contains("data-index=\"a\"")
        );
    }

    /// A3 (F13): a bare `:::` line that closes nothing is ignored and reported, once per
    /// mistake.
    #[test]
    fn ignored_fences_are_reported() {
        // a `:::` too many: it renders nothing, and opens nothing
        let (rendered, _, codes) =
            render_with(":::fragment\nA\n:::\n:::\n\nC\n", RenderMode::Deck, false);
        assert_eq!(codes, [("invalid-directive", 4)]);
        assert_eq!(
            rendered.html,
            "<div class=\"fragment\">\n<p>A</p>\n</div>\n<p>C</p>\n"
        );
        // nested with fences of one length (contract change 2026-11-14) or with a longer
        // outer fence: nothing to report
        for text in [
            ":::fragment\nA\n:::notes\nN\n:::\nB\n:::\n\nC\n",
            "::::fragment\nA\n:::notes\nN\n:::\nB\n::::\n\nC\n",
        ] {
            let (rendered, _, codes) = render_with(text, RenderMode::Deck, false);
            assert_eq!(codes, []);
            assert_eq!(
                rendered.html,
                "<div class=\"fragment\">\n<p>A</p>\n<p>B</p>\n</div>\n<p>C</p>\n"
            );
            assert_eq!(rendered.notes.as_deref(), Some("<p>N</p>\n"));
        }
        // a `{#id}`-only pandoc block has no name but is no mistake
        let (rendered, _, codes) = render_with("::: {#x}\ny\n:::\n", RenderMode::Deck, false);
        assert_eq!(codes, []);
        assert_eq!(rendered.html, "<div id=\"x\">\n<p>y</p>\n</div>\n");
    }

    /// A3 (F35): a `title=` attribute is written when there is no Markdown title.
    #[test]
    fn title_attributes_on_links_and_images() {
        let html = render(
            "[a](https://x.y){title=\"T1\"} [b](https://x.y \"Md\"){title=\"T2\"} \
             ![c](https://x.y/c.png){title=\"T3\"} ![d](https://x.y/d.png \"Md\"){title=\"T4\"}\n",
        )
        .html;
        assert_eq!(
            html,
            "<p><a href=\"https://x.y\" title=\"T1\">a</a> <a href=\"https://x.y\" title=\"Md\">b</a> \
             <img src=\"https://x.y/c.png\" alt=\"c\" title=\"T3\" /> \
             <img src=\"https://x.y/d.png\" alt=\"d\" title=\"Md\" /></p>\n"
        );
    }

    /// A3 (F04): a fence's `{…}` block that comrak rejects; one meant as attributes but not
    /// valid is reported on the fence's line.
    #[test]
    fn fence_attribute_blocks_comrak_rejects() {
        let html =
            render("```python {lines=1|3 .numbered}\na\n```\n\n```math {data-x='a b'}\nx\n```\n")
                .html;
        assert_eq!(
            html,
            "<pre><code class=\"language-python\" data-trim data-line-numbers=\"1|3\">a</code></pre>\n\
             <div data-math-style=\"display\" data-x=\"a b\">x</div>\n"
        );
        let (rendered, _, codes) = render_with(
            "Text\n\n```python {numbered lines=1|3}\na\n```\n\n```{r}\nb\n```\n",
            RenderMode::Deck,
            false,
        );
        assert_eq!(codes, [("unparsed-attr-block", 3)]);
        assert!(
            rendered
                .html
                .contains("<pre><code class=\"language-python\" data-trim>a")
        );
        assert!(
            rendered
                .html
                .contains("<code class=\"language-{r}\" data-trim>b")
        );
    }

    /// A3 (F12): slide attributes left on a heading are reported; element attributes not.
    #[test]
    fn slide_attributes_left_on_a_heading_are_reported() {
        let (rendered, _, codes) = render_with(
            "Intro\n\n## A {layout=section}\n\n## B {.x data-id=b aria-label=l title=t}\n\n\
             ## C {background=red transition=zoom}\n",
            RenderMode::Deck,
            false,
        );
        assert_eq!(
            codes,
            [("unlifted-slide-attr", 3), ("unlifted-slide-attr", 7)]
        );
        assert!(rendered.html.contains("<h2 layout=\"section\">A</h2>"));
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
        // the `:::` meant to close it is ignored, reported only once (A3)
        assert_eq!(codes, [("invalid-directive", 3)]);
        assert_eq!(
            rendered.html,
            "<p>Intro</p>\n<p>:::box title=&quot;a:b&quot;\nx</p>\n"
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
    fn escaped_colons_are_no_directive_line_in_a_fragment() {
        let text = "&#58;::small\n\n\\:::box title=\"a:b\"\n\n- [ ] \\:::small\n";
        for mode in [RenderMode::Deck, RenderMode::Notebook] {
            let (rendered, _, codes) = render_with(text, mode, false);
            assert_eq!(codes, [], "{mode:?}");
            assert!(rendered.html.starts_with("<p>:::small</p>\n"), "{mode:?}");
        }
        // after an escaped line, a `:::` that closes nothing is reported on its own
        let (_, _, codes) = render_with("\\:::small\n\nx\n\n:::\n", RenderMode::Notebook, false);
        assert_eq!(codes, [("invalid-directive", 5)]);
        // the source is what tells: `render_nodes` without it reads the decoded text
        let arena = Arena::new();
        let opts = comrak_options(&MarkdownConfig::default());
        let root = parse_document(&arena, "\\:::small\n", &opts);
        let nodes: Vec<_> = root.children().collect();
        let (mut assets, mut diagnostics) = (AssetRegistry::new(), Diagnostics::new());
        let dir = fixture_dir();
        let mut ctx = RenderCtx {
            assets: &mut assets,
            base_dir: &dir,
            mode: RenderMode::Deck,
            diagnostics: &mut diagnostics,
            missing_asset_is_error: false,
        };
        render_nodes(&arena, &nodes, None, &opts, &mut ctx).unwrap();
        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn a_fence_followed_by_another_kind_of_space_is_reported_as_such() {
        let message_of = |text: &str| {
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
            render_fragment(text, &opts, &mut ctx).unwrap();
            // the ignored lines' messages; the parse reports the directives left open first
            let messages: Vec<String> = diagnostics
                .iter()
                .map(|d| d.message.clone())
                .filter(|message| !message.contains("has no closing fence"))
                .collect();
            messages
        };
        for space in ["\u{a0}", "\u{3000}"] {
            let messages = message_of(&format!(":::fragment\nx\n:::{space}\n"));
            assert_eq!(messages.len(), 1, "{space:?}: {messages:?}");
            assert!(
                messages[0].contains("not a plain space or tab"),
                "{space:?}: {}",
                messages[0]
            );
        }
        let messages = message_of(":::fragment\nx\n:::\n:::\n");
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(
            messages[0].contains("one `:::` too many"),
            "{}",
            messages[0]
        );
        assert!(!messages[0].contains("cannot close"), "{}", messages[0]);
        assert!(!messages[0].contains("list item"), "{}", messages[0]);
        // a directive around it with a longer fence
        let messages = message_of("::::fragment\nA\n:::\n::::\n");
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(
            messages[0].contains("cannot close the `fragment` directive around it"),
            "{}",
            messages[0]
        );
        // fences of one length in a list item nest as at the top
        let messages = message_of(
            "- item\n\n  :::columns\n  :::column\n  A\n  :::\n  :::column\n  B\n  :::\n  :::\n",
        );
        assert_eq!(messages, Vec::<String>::new());
        // a `:::` in a block quote or a list item closes no directive around them, whatever
        // its length
        for (text, kind) in [
            (":::note\n> :::\n:::\n", "block quote"),
            (":::note\n1.  item\n\n    :::\n:::\n", "list item"),
            (":::note\n- item\n\n  :::\n:::\n", "list item"),
            (":::note\n- item\n  :::\n:::\n", "list item"),
            (":::note\nTerm\n: Text\n\n  :::\n:::\n", "definition"),
        ] {
            let messages = message_of(text);
            assert_eq!(messages.len(), 1, "{text:?}: {messages:?}");
            let expected = format!(
                "It cannot close the `note` directive around the {kind} it is in: a `:::` in a \
                 {kind} closes only directives opened in it. Otherwise, there is one `:::` too \
                 many"
            );
            assert!(messages[0].contains(&expected), "{text:?}: {}", messages[0]);
            assert!(
                !messages[0].contains("whose fence is longer"),
                "{}",
                messages[0]
            );
        }
        // a directive inside the list item with a longer fence
        let messages = message_of("- item\n\n  ::::note\n  :::\n  ::::\n");
        assert_eq!(messages.len(), 1, "{messages:?}");
        let expected = "cannot close the `note` directive around it, whose fence is longer";
        assert!(messages[0].contains(expected), "{}", messages[0]);
        // indented 4 columns where the closing fence is looked for: at the top, in a list
        // item, and inside a directive whose fence the rewrite made longer
        for (text, name) in [
            ("  :::note\n    Text\n    :::\n", "note"),
            ("- item\n\n    :::note\n      Text\n      :::\n", "note"),
            (
                ":::columns\n  :::column\n  :::small\n  Small\n  :::\n    :::\n  :::\n:::\n",
                "column",
            ),
        ] {
            let messages = message_of(text);
            assert_eq!(messages.len(), 1, "{text:?}: {messages:?}");
            let expected = format!(
                "cannot close the `{name}` directive around it: this line is indented by 4 \
                 columns or more"
            );
            assert!(messages[0].contains(&expected), "{text:?}: {}", messages[0]);
            assert!(
                !messages[0].contains("whose fence is longer"),
                "{text:?}: {}",
                messages[0]
            );
        }
        // a character reference after the colons: at the top, inside a directive whose
        // fence the rewrite made longer (the tree keeps the lengths as written), in a list
        // item, after a pandoc block without a class
        for text in [
            ":::fragment\nA\n:::&#32;\n\nB\n",
            ":::note\n:::small\nx\n:::\n:::&#32;\ny\n:::\n",
            ":::note\n- item\n  :::&#32;\n",
            "::: {#intro}\n:::&#9;\nText\n",
        ] {
            let messages = message_of(text);
            assert_eq!(messages.len(), 1, "{text:?}: {messages:?}");
            for expected in ["not a plain space or tab", "`&#32;`", "it is ignored"] {
                assert!(messages[0].contains(expected), "{text:?}: {}", messages[0]);
            }
            for unexpected in ["cannot close", "whose fence is longer", "end of"] {
                assert!(
                    !messages[0].contains(unexpected),
                    "{text:?}: {}",
                    messages[0]
                );
            }
        }
        // in a container with no directive around, the `:::` is one too many, as at the top
        for text in [
            "- item\n\n  :::\n\nAfter the list.\n",
            "> :::\n",
            "Text[^1]\n\n[^1]: Note\n    :::\n",
        ] {
            let messages = message_of(text);
            assert_eq!(messages.len(), 1, "{text:?}: {messages:?}");
            let expected = "closes no directive, so it is ignored. There is one `:::` too many";
            assert!(messages[0].contains(expected), "{text:?}: {}", messages[0]);
        }
        // a footnote is in the directive its lines are in, though comrak moves it out
        for text in [
            ":::note\nText[^1]\n\n[^1]: Note\n    :::\n:::\n",
            ":::note\n- Item[^1]\n\n  [^1]: Note\n      :::\n:::\n",
        ] {
            let messages = message_of(text);
            assert_eq!(messages.len(), 1, "{text:?}: {messages:?}");
            let expected = "It cannot close the `note` directive around the footnote it is in: \
                            a `:::` in a footnote closes only directives opened in it.";
            assert!(messages[0].contains(expected), "{text:?}: {}", messages[0]);
        }
        let messages = message_of(":::note\nText[^1]\n:::\n\n[^1]: Note\n    :::\n");
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(!messages[0].contains("cannot close"), "{}", messages[0]);
        // nor in a directive of a list item before it, whose lines comrak may count up to the
        // footnote's first line; a directive around both is named
        let messages = message_of("Text[^1]\n\n- :::warn\n  x\n\n[^1]: Note\n    :::\n");
        assert!(
            messages
                .iter()
                .all(|message| !message.contains("cannot close")),
            "{messages:?}"
        );
        let messages =
            message_of(":::note\nText[^1]\n\n- :::warn\n  x\n\n[^1]: Note\n    :::\n:::\n");
        let expected = "It cannot close the `note` directive around the footnote it is in";
        assert!(
            messages.iter().any(|message| message.contains(expected)),
            "{messages:?}"
        );
        assert!(
            messages.iter().all(|message| !message.contains("`warn`")),
            "{messages:?}"
        );
        // and in the directive of the list item the footnote is in
        let messages = message_of("Text[^1]\n\n- :::note\n  [^1]: Note\n      :::\n  :::\n");
        assert!(
            messages.iter().any(|message| message
                .contains("It cannot close the `note` directive around the footnote it is in")),
            "{messages:?}"
        );
        // not where the fence written is longer
        let messages = message_of("::::note\n:::small\nx\n:::\n- item\n  :::\n::::\n");
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(
            messages[0].contains("cannot close the `note` directive around the list item"),
            "{}",
            messages[0]
        );
        // a pandoc block without a class is named by its attributes
        let messages = message_of(":::: {#intro}\nA\n:::\n::::\n");
        assert_eq!(messages.len(), 1, "{messages:?}");
        let expected = "cannot close the `{#intro}` directive around it, whose fence is longer";
        assert!(messages[0].contains(expected), "{}", messages[0]);
        // a `:::` in a list item or definition closes the directive opened in it, never one
        // around the list: these read as written, with no warning
        for text in [
            ":::note\n- item\n\n  :::small\n  text\n  :::\n- item 2\n:::\n",
            "::::note\n- item\n\n  ::::small\n  text\n  ::::\n::::\n",
            ":::columns\n:::column\n- item\n\n  :::note\n  text\n  :::\n:::\n:::column\nRight\n\
             :::\n:::\n",
            ":::a\n::::note\n:::e\nx\n:::\n- item\n  :::tip\n  t\n  :::\n::::\n:::\n",
            ":::columns\n:::column\n- Point\n\n  :::fragment\n  Detail\n  :::\n\nCaption\n:::\n\
             :::column\nRight\n:::\n:::\n",
            ":::a\n:::b\n:::c\n- x\n\n  :::tip\n  t\n  :::\n:::\nMore b.\n:::\nMore a.\n:::\n",
            ":::columns\n:::column\n- one\n\n  :::fragment\n  t\n  :::\n- two\n:::\n:::column\n\
             Right\n:::\n:::\n",
            ":::a\n- one\n  :::tip\n  t\n  :::\n:::b\n:::c\n- plain\n:::\npara\n:::\n:::\n",
            ":::note\n- a\n\n  :::small\n  - b\n  :::\n:::\n",
            ":::note\nTerm\n: Text\n\n  :::small\n  N\n  :::\n:::\n",
            "> :::columns\n> :::column\n> A\n> :::\n> :::\n",
            // longer outer fences, as before
            "::::note\n- item\n\n  :::small\n  text\n  :::\n- item 2\n::::\n",
            "::::columns\n::::column\n- item\n\n  :::note\n  text\n  :::\n::::\n:::column\n\
             Right\n:::\n::::\n",
        ] {
            assert_eq!(message_of(text), Vec::<String>::new(), "{text:?}");
        }
        // a `:::` too many after the list is one too many
        let messages = message_of(
            ":::note\n- item\n\n  :::small\n  text\n  :::\n- item 2\n:::\n:::\n\nx\n\n:::\n",
        );
        assert_eq!(messages.len(), 2, "{messages:?}");
        for message in &messages {
            assert!(
                message.ends_with(
                    "ignored. There is one `:::` too many: this line, or one above it that \
                     closed the directive this line was meant for."
                ),
                "{message}"
            );
        }
        // the `:::` in the `::::note` is ignored, the `::::` in the list cannot close the
        // note around it, and the last `:::` is too short
        let messages = message_of("::::note\n:::\n- a\n  ::::\n:::\n");
        assert_eq!(messages.len(), 3, "{messages:?}");
        assert!(
            messages[0].contains("whose fence is longer"),
            "{}",
            messages[0]
        );
        assert!(
            messages[1].contains("around the list item it is in"),
            "{}",
            messages[1]
        );
        assert!(
            messages[2].contains("whose fence is longer"),
            "{}",
            messages[2]
        );
        // a character reference after the colons, after a container's marker on the line
        for text in [
            "> :::&#32;\n> x\n",
            "- :::&#32;\n  x\n",
            "> :::note\n> x\n> :::&#32;\n",
        ] {
            let messages = message_of(text);
            assert_eq!(messages.len(), 1, "{text:?}: {messages:?}");
            for expected in ["not a plain space or tab", "it is ignored"] {
                assert!(messages[0].contains(expected), "{text:?}: {}", messages[0]);
            }
            assert!(!messages[0].contains("bare"), "{text:?}: {}", messages[0]);
        }
        // with an attribute block comrak rejects, which has the text parsed again
        for (text, expected) in [
            (
                "# T {background='#222'}\n\n:::note\n:::small\nx\n:::\n:::&#32;\ny\n:::\n:::\n",
                "not a plain space or tab",
            ),
            (
                "# T {background='#222'}\n\n:::note\n:::small\nx\n:::\n- item\n  :::\n:::\n",
                "cannot close the `note` directive around the list item it is in",
            ),
        ] {
            let messages = message_of(text);
            assert!(
                messages.iter().any(|message| message.contains(expected)),
                "{text:?}: {messages:?}"
            );
            for message in &messages {
                assert!(!message.contains("whose fence is longer"), "{message}");
            }
        }
    }

    #[test]
    fn warnings_for_ignored_fences_take_linear_time() {
        // each warning looked at every directive above it on the slide: 4 s for the first
        // deck in a release build, and tens of seconds in a debug one
        for text in [
            ":::\nA\n:::\n\n".repeat(40_000),
            format!(
                ":::big\n{}:::\n\n{}",
                "A\n\n".repeat(100_000),
                ":::\n:::\n\n".repeat(5_000)
            ),
        ] {
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
            let started = std::time::Instant::now();
            render_fragment(&text, &opts, &mut ctx).unwrap();
            let elapsed = started.elapsed();
            assert!(diagnostics.len() >= 5_000);
            assert!(elapsed.as_secs() < 20, "{elapsed:?}");
        }
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
        assert!(ctx.diagnostics.is_empty());
        let (rendered, lifted) = render_fragment_with_lift(text, &opts, &mut ctx, false).unwrap();
        assert!(lifted.is_none());
        assert!(
            rendered
                .html
                .starts_with("<h1 id=\"a\" class=\"wide\" layout=\"title\">")
        );
        // a slide attribute left on the element is reported (A3)
        let codes: Vec<_> = ctx.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(codes, ["unlifted-slide-attr"]);
        *ctx.diagnostics = Diagnostics::new();
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
            file: None,
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
        let rendered = render_nodes(&arena, &nodes, None, &opts, &mut ctx).unwrap();
        assert_eq!(rendered.html, "<h1>A</h1>\n<p>para</p>\n");
        assert_eq!(rendered.notes.as_deref(), Some("<p>n</p>\n"));
        assert_eq!(
            root.children().count(),
            1,
            "only `# B` is left in the original"
        );
    }
}
