//! HTML post-processing (incremental lists, compat attributes) via `lol_html`. Implemented by T43.
//!
//! Contract: `docs/spec/markdown-dialect.md` §3.3 (the `<!-- .element: -->` comment form) and
//! `docs/spec/config.md` (`slides.incremental_lists`). Both passes work on a slide's rendered
//! HTML, so they also see raw HTML written by the author. Each runs only when there is
//! something to do (an `.element` comment, incremental lists switched on); otherwise the HTML
//! is returned unchanged, byte for byte. `lol_html` rewrites only the start tags and comments
//! it is asked to change; everything else is copied as it was.
//!
//! - [`element_attributes`]: every `<!-- .element: … -->` comment ([`compat::element_comment`])
//!   gives attributes to an element, chosen the way reveal.js's Markdown plugin does: the
//!   nearest preceding sibling element (`<br>` skipped), or else the enclosing element. The
//!   attribute text is read by [`parse_attr_block`] (`k="v"`, `k=v`, `k='v'`, bare keys,
//!   `.class`, `#id`). Values are taken literally, as reveal sets them from the comment text.
//!   Unlike reveal, which replaces the attribute, `class` values are *added* to the element's
//!   classes (so `class="fragment"` keeps `language-x` or `task-list-item`). Other keys
//!   replace the attribute. The comment is removed once applied. A comment with an unbalanced
//!   quote, or with no element before it or around it (the first thing in a slide), is kept
//!   and reported as `element-comment-ignored`. A key that is not a valid attribute name
//!   ([`is_valid_attr_name`]) is dropped and reported as `invalid-element-attr`.
//! - [`incremental_lists`]: the `<li>` children of the lists at the top level of the slide get
//!   the class `fragment` (reveal steps through them), except in a list that has the class
//!   `no-incremental`. Nested lists and lists inside directives, block quotes or raw HTML
//!   containers are left alone. Whether a slide gets this is [`incremental_enabled`]:
//!   `slides.incremental_lists`, overridden by the slide classes `.incremental` and
//!   `.no-incremental`.
//!
//! Both find "the element before" and "the top level" on the element tree as a browser
//! builds it from the HTML (comrak's output is always balanced; raw HTML may not be): tag
//! names are case-insensitive, a stray end tag closes nothing, and the end tags a browser
//! implies are honored where it matters here (a `<ul>`, `<div>`, `<h2>`, … start tag closes
//! an open `<p>`; `<li>` closes the previous `<li>`; see [`close_implied`]). Other implied
//! end tags (such as `</td>`, `</option>` or `</dd>`) and the adoption agency's re-nesting of
//! misnested inline tags are not modelled: in such raw HTML an `.element` comment may pick
//! another element than reveal would, and a list may be taken as nested.
//!
//! [`slide_html`] runs both, in that order, so a list can be given `no-incremental` with an
//! `.element:` comment on the line after it.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use lol_html::html_content::{Comment, Element};
use lol_html::{HandlerResult, RewriteStrSettings, doc_comments, element, rewrite_str};

use crate::error::{Diagnostics, Error, Result};
use crate::ir::SourceSpan;
use crate::markdown::attrs::{ParsedAttrs, is_valid_attr_name, parse_attr_block};
use crate::markdown::compat;

/// The class that turns an element into a reveal.js fragment.
const FRAGMENT: &str = "fragment";

/// Whether a slide's top-level lists become incremental: the slide class `no-incremental`
/// says no, else `incremental` says yes, else `slides.incremental_lists` (`config`) decides.
pub fn incremental_enabled(config: bool, classes: &[String]) -> bool {
    if classes.iter().any(|class| class == "no-incremental") {
        false
    } else {
        config || classes.iter().any(|class| class == "incremental")
    }
}

/// Post-processes a slide's body HTML: [`element_attributes`], then (when `incremental`)
/// [`incremental_lists`]. Warnings go to `diagnostics` with `span` (the slide's lines).
pub fn slide_html(
    html: &str,
    incremental: bool,
    span: Option<&SourceSpan>,
    diagnostics: &mut Diagnostics,
) -> Result<String> {
    let html = element_attributes(html, span, diagnostics)?;
    if incremental {
        incremental_lists(&html)
    } else {
        Ok(html)
    }
}

/// Applies the `<!-- .element: … -->` comments in `html` (module docs). Returns `html`
/// unchanged when it has none. Errors: `Error::Other` if `lol_html` fails (it does not for
/// any input in non-strict mode, but its API is fallible).
pub fn element_attributes(
    html: &str,
    span: Option<&SourceSpan>,
    diagnostics: &mut Diagnostics,
) -> Result<String> {
    if !html.contains(".element") {
        return Ok(html.to_string());
    }
    let found = scan_element_comments(html)?;
    if found.is_empty() {
        return Ok(html.to_string());
    }
    let mut warn = |code: &'static str, message: String| {
        diagnostics.warn(code, message).span = span.cloned();
    };
    let mut edits: BTreeMap<usize, Vec<ParsedAttrs>> = BTreeMap::new();
    let mut applied: BTreeSet<usize> = BTreeSet::new();
    for comment in found {
        let text = comment.text.trim();
        let parsed = match parse_attr_block(text) {
            Ok(parsed) => parsed,
            Err(err) => {
                let message = format!("`<!-- .element: {text} -->` is ignored: {err}");
                warn("element-comment-ignored", message);
                continue;
            }
        };
        let Some(target) = comment.target else {
            let message = format!(
                "`<!-- .element: {text} -->` is ignored: there is no element before it or \
                 around it (it is the first thing on the slide)"
            );
            warn("element-comment-ignored", message);
            continue;
        };
        let mut parsed = parsed;
        parsed.pairs.retain(|(key, _)| {
            let valid = is_valid_attr_name(key);
            if !valid {
                let message = format!(
                    "`{key}` in `<!-- .element: {text} -->` is not a valid attribute name \
                     (dropped)"
                );
                warn("invalid-element-attr", message);
            }
            valid
        });
        edits.entry(target).or_default().push(parsed);
        applied.insert(comment.index);
    }
    apply_element_edits(html, &edits, &applied)
}

/// An `.element` comment found by [`scan_element_comments`].
#[derive(Debug, PartialEq, Eq)]
struct ElementComment {
    /// The comment's position among all comments of the HTML (document order).
    index: usize,
    /// The start-tag index (document order, all elements) of the element it applies to.
    target: Option<usize>,
    /// The attribute text after `.element:`.
    text: String,
}

/// One open element while scanning: its start-tag index (`None` for the top level), its tag
/// name (lowercase; empty for the top level), the index of its latest child element that is
/// not a `<br>` ([`element_attributes`]), and whether it is a top-level list whose items
/// become fragments ([`incremental_lists`]).
#[derive(Debug, Default)]
struct Frame {
    element: Option<usize>,
    tag: String,
    last_child: Option<usize>,
    fragments: bool,
}

/// Closes the elements a browser closes implicitly before the start tag `tag`, which
/// `lol_html` keeps open (it only pairs end tags with start tags): a block-level start tag
/// closes an open `<p>` ([`closes_p`]), and `<li>` closes the open `<li>` of its list (HTML
/// tree construction, "in body" insertion mode, simplified to the scopes below). So a list
/// after a raw `<p>` without `</p>` is at the slide's top level, as the browser shows it.
/// `stack[0]` is the top level and is never closed.
fn close_implied(stack: &mut Vec<Frame>, tag: &str) {
    if closes_p(tag)
        && let Some(at) = open_in_scope(stack, |t| t == "p", is_button_scope_boundary)
    {
        stack.truncate(at);
    }
    if tag == "li" {
        let li_boundary = |t: &str| is_special(t) && !matches!(t, "address" | "div" | "p");
        if let Some(at) = open_in_scope(stack, |t| t == "li", li_boundary) {
            stack.truncate(at);
        }
    }
}

/// The position of the innermost open element matching `target`, unless an element for which
/// `boundary` holds is open inside it.
fn open_in_scope(
    stack: &[Frame],
    target: impl Fn(&str) -> bool,
    boundary: impl Fn(&str) -> bool,
) -> Option<usize> {
    for (at, frame) in stack.iter().enumerate().skip(1).rev() {
        if target(&frame.tag) {
            return Some(at);
        }
        if boundary(&frame.tag) {
            return None;
        }
    }
    None
}

/// Start tags that close an open `<p>` (in button scope), space-separated.
const CLOSES_P: &str = "address article aside blockquote center details dialog dir div dl dd \
                        dt fieldset figcaption figure footer form h1 h2 h3 h4 h5 h6 header \
                        hgroup hr li listing main menu nav ol p plaintext pre search section \
                        summary table ul xmp";

/// Elements that end the button scope a `<p>` is looked for in, space-separated.
const BUTTON_SCOPE: &str = "applet button caption html marquee object table td template th";

/// HTML's other "special" elements that can have content, space-separated.
const OTHER_SPECIAL: &str = "body colgroup iframe noembed noframes noscript script select \
                             style tbody textarea tfoot thead title tr";

/// Whether `tag` is one of the space-separated `names`.
fn is_one_of(tag: &str, names: &str) -> bool {
    names.split_ascii_whitespace().any(|name| name == tag)
}

fn closes_p(tag: &str) -> bool {
    is_one_of(tag, CLOSES_P)
}

fn is_button_scope_boundary(tag: &str) -> bool {
    is_one_of(tag, BUTTON_SCOPE)
}

/// HTML's "special" elements that can have content.
fn is_special(tag: &str) -> bool {
    closes_p(tag) || is_button_scope_boundary(tag) || is_one_of(tag, OTHER_SPECIAL)
}

/// What the scan has seen so far.
#[derive(Debug)]
struct Scan {
    /// Start tags seen.
    elements: usize,
    /// Comments seen.
    comments: usize,
    /// The open elements, outermost first; `stack[0]` is the top level.
    stack: Vec<Frame>,
    found: Vec<ElementComment>,
}

/// First pass: finds the `.element` comments and the element each applies to (reveal's rule:
/// the latest preceding sibling element other than `<br>`, else the parent). Elements are
/// numbered by start tag in document order, as [`apply_element_edits`] numbers them. An
/// element without an end tag stays open until a browser would close it ([`close_implied`])
/// or an ancestor's end tag.
fn scan_element_comments(html: &str) -> Result<Vec<ElementComment>> {
    let scan = Rc::new(RefCell::new(Scan {
        elements: 0,
        comments: 0,
        stack: vec![Frame::default()],
        found: Vec::new(),
    }));
    let on_element = {
        let scan = Rc::clone(&scan);
        move |el: &mut Element<'_, '_>| -> HandlerResult {
            let tag = el.tag_name();
            let index = {
                let mut state = scan.borrow_mut();
                let index = state.elements;
                state.elements += 1;
                close_implied(&mut state.stack, &tag);
                if tag != "br"
                    && let Some(parent) = state.stack.last_mut()
                {
                    parent.last_child = Some(index);
                }
                index
            };
            if let Some(handlers) = el.end_tag_handlers() {
                scan.borrow_mut().stack.push(Frame {
                    element: Some(index),
                    tag,
                    ..Frame::default()
                });
                let scan = Rc::clone(&scan);
                handlers.push(Box::new(move |_end| {
                    close_element(&mut scan.borrow_mut().stack, index);
                    Ok(())
                }));
            }
            Ok(())
        }
    };
    let on_comment = {
        let scan = Rc::clone(&scan);
        move |comment: &mut Comment<'_>| -> HandlerResult {
            let mut state = scan.borrow_mut();
            let index = state.comments;
            state.comments += 1;
            if let Some(text) = compat::element_comment(&comment.text()) {
                let target = state
                    .stack
                    .last()
                    .and_then(|frame| frame.last_child.or(frame.element));
                state.found.push(ElementComment {
                    index,
                    target,
                    text: text.to_string(),
                });
            }
            Ok(())
        }
    };
    let settings = RewriteStrSettings::new()
        .with_strict(false)
        .with_enable_esi_tags(false)
        .append_element_content_handler(element!("*", on_element))
        .append_document_content_handler(doc_comments!(on_comment));
    rewrite_str(html, settings).map_err(rewrite_error)?;
    let found = std::mem::take(&mut scan.borrow_mut().found);
    Ok(found)
}

/// Pops the frames down to and including the one of element `index` (frames above it belong
/// to elements closed implicitly). Does nothing if it is not open.
fn close_element(stack: &mut Vec<Frame>, index: usize) {
    if let Some(at) = stack.iter().rposition(|frame| frame.element == Some(index)) {
        stack.truncate(at);
    }
}

/// Second pass: applies `edits` (start-tag index → attribute blocks, in order) and removes the
/// comments in `remove` (comment indexes).
fn apply_element_edits(
    html: &str,
    edits: &BTreeMap<usize, Vec<ParsedAttrs>>,
    remove: &BTreeSet<usize>,
) -> Result<String> {
    let mut element_index = 0;
    let mut comment_index = 0;
    let settings = RewriteStrSettings::new()
        .with_strict(false)
        .with_enable_esi_tags(false)
        .append_element_content_handler(element!("*", |el| {
            let index = element_index;
            element_index += 1;
            for attrs in edits.get(&index).into_iter().flatten() {
                apply_attrs(el, attrs)?;
            }
            Ok(())
        }))
        .append_document_content_handler(doc_comments!(|comment| {
            if remove.contains(&comment_index) {
                comment.remove();
            }
            comment_index += 1;
            Ok(())
        }));
    rewrite_str(html, settings).map_err(rewrite_error)
}

/// Sets one attribute block on an element: `#id` and pairs replace the attribute (values are
/// written literally: `&` is escaped, `lol_html` escapes `"`), `.class` entries and `class=`
/// values are added to its classes.
fn apply_attrs(el: &mut Element<'_, '_>, attrs: &ParsedAttrs) -> HandlerResult {
    if let Some(id) = &attrs.id {
        el.set_attribute("id", &id.replace('&', "&amp;"))?;
    }
    for class in &attrs.classes {
        add_classes(el, class)?;
    }
    for (key, value) in &attrs.pairs {
        if key.eq_ignore_ascii_case("class") {
            add_classes(el, value)?;
        } else {
            el.set_attribute(key, &value.replace('&', "&amp;"))?;
        }
    }
    Ok(())
}

/// Adds the whitespace-separated `classes` (plain text) the element does not have yet to its
/// `class`. The attribute's current value is kept byte for byte (`lol_html` gives it with its
/// entities); only the added names are escaped (`&`; `lol_html` escapes `"`).
fn add_classes(el: &mut Element<'_, '_>, classes: &str) -> HandlerResult {
    let mut value = el.get_attribute("class").unwrap_or_default();
    let mut added: Vec<String> = Vec::new();
    for class in classes.split_whitespace() {
        let class = class.replace('&', "&amp;");
        if !value.split_whitespace().any(|have| have == class) && !added.contains(&class) {
            added.push(class);
        }
    }
    if added.is_empty() {
        return Ok(());
    }
    if !value.is_empty() && !value.ends_with(|c: char| c.is_ascii_whitespace()) {
        value.push(' ');
    }
    value.push_str(&added.join(" "));
    el.set_attribute("class", &value)?;
    Ok(())
}

/// Whether `html` may contain a `<ul` or `<ol` start tag (ASCII case-insensitive).
fn mentions_list(html: &str) -> bool {
    html.as_bytes().windows(3).any(|w| {
        w[0] == b'<'
            && matches!(w[1].to_ascii_lowercase(), b'u' | b'o')
            && w[2].eq_ignore_ascii_case(&b'l')
    })
}

/// Adds the class `fragment` to the `<li>` children of the lists at the top level of `html`
/// (a slide body), except lists with the class `no-incremental` (module docs). The top level
/// is tracked like the browser builds it: elements are opened by start tags and closed by
/// their end tags or implicitly ([`close_implied`]); a stray end tag closes nothing. Returns
/// `html` unchanged when it has no list. Errors as [`element_attributes`].
pub fn incremental_lists(html: &str) -> Result<String> {
    if !mentions_list(html) {
        return Ok(html.to_string());
    }
    let stack = Rc::new(RefCell::new(vec![Frame::default()]));
    let mut elements = 0;
    let on_element = move |el: &mut Element<'_, '_>| -> HandlerResult {
        let index = elements;
        elements += 1;
        let tag = el.tag_name();
        let (item, top_level) = {
            let mut open = stack.borrow_mut();
            close_implied(&mut open, &tag);
            // `open[1]` is a top-level element; an item of a top-level list has it as parent
            (open.len() == 2 && open[1].fragments, open.len() == 1)
        };
        if tag == "li" && item {
            add_classes(el, FRAGMENT)?;
        }
        let no_incremental = el
            .get_attribute("class")
            .is_some_and(|class| class.split_whitespace().any(|c| c == "no-incremental"));
        let fragments = top_level && matches!(tag.as_str(), "ul" | "ol") && !no_incremental;
        if let Some(handlers) = el.end_tag_handlers() {
            stack.borrow_mut().push(Frame {
                element: Some(index),
                tag,
                fragments,
                ..Frame::default()
            });
            let stack = Rc::clone(&stack);
            handlers.push(Box::new(move |_end| {
                close_element(&mut stack.borrow_mut(), index);
                Ok(())
            }));
        }
        Ok(())
    };
    let settings = RewriteStrSettings::new()
        .with_strict(false)
        .with_enable_esi_tags(false)
        .append_element_content_handler(element!("*", on_element));
    rewrite_str(html, settings).map_err(rewrite_error)
}

fn rewrite_error(err: lol_html::errors::RewritingError) -> Error {
    Error::Other(format!("HTML post-processing failed: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elements(html: &str) -> (String, Vec<(&'static str, String)>) {
        let mut diagnostics = Diagnostics::new();
        let out = element_attributes(html, None, &mut diagnostics).unwrap();
        let found = diagnostics
            .iter()
            .map(|d| (d.code, d.message.clone()))
            .collect();
        (out, found)
    }

    fn applied(html: &str) -> String {
        let (out, found) = elements(html);
        assert!(found.is_empty(), "{found:?}");
        out
    }

    #[test]
    fn element_comments_apply_to_lists_paragraphs_and_images() {
        let cases: &[(&str, &str)] = &[
            // the acceptance examples (as comrak renders them)
            (
                "<ul>\n<li>item <!-- .element: class=\"fragment\" --></li>\n<li>b</li>\n</ul>\n",
                "<ul>\n<li class=\"fragment\">item </li>\n<li>b</li>\n</ul>\n",
            ),
            (
                "<p><img src=\"x.png\" alt=\"\" /> <!-- .element: width=\"300\" --></p>\n",
                "<p><img src=\"x.png\" alt=\"\" width=\"300\" /> </p>\n",
            ),
            // a paragraph's text: the paragraph
            (
                "<p>Text <!-- .element: class=\"x\" data-y=\"1\" --></p>\n",
                "<p class=\"x\" data-y=\"1\">Text </p>\n",
            ),
            // the preceding sibling element, not the parent
            (
                "<p>A <em>b</em> c <!-- .element: class=\"x\" --></p>",
                "<p>A <em class=\"x\">b</em> c </p>",
            ),
            // `<br>` is skipped
            (
                "<p><a href=\"u\">l</a><br />\nx<!-- .element: class=\"x\" --></p>",
                "<p><a href=\"u\" class=\"x\">l</a><br />\nx</p>",
            ),
            // a comment block after a list or a paragraph: that element
            (
                "<ul>\n<li>a</li>\n</ul>\n<!-- .element: class=\"no-incremental\" -->\n",
                "<ul class=\"no-incremental\">\n<li>a</li>\n</ul>\n\n",
            ),
            (
                "<p>a</p>\n<!-- .element: #ident .c k=v -->\n<p>b</p>\n",
                "<p id=\"ident\" class=\"c\" k=\"v\">a</p>\n\n<p>b</p>\n",
            ),
            // nested lists: the nearest element at the comment's own level
            (
                "<ul>\n<li>a\n<ul>\n<li>b</li>\n</ul>\n<!-- .element: class=\"x\" --></li>\n</ul>",
                "<ul>\n<li>a\n<ul class=\"x\">\n<li>b</li>\n</ul>\n</li>\n</ul>",
            ),
            // classes are added, other attributes replaced, `&` and `"` kept literally
            (
                "<pre class=\"big\"><code>x</code></pre><!-- .element: class=\"fragment big\" \
                 title='a \"q\" & b' -->",
                "<pre class=\"big fragment\" title=\"a &quot;q&quot; &amp; b\"><code>x</code>\
                 </pre>",
            ),
            // reveal's colon-less form; bare keys
            (
                "<p>a<!-- .element data-auto-animate --></p>",
                "<p data-auto-animate=\"\">a</p>",
            ),
            // several comments on one element apply in order
            (
                "<p>a <!-- .element: data-x=1 --><!-- .element: data-x=2 class=y --></p>",
                "<p data-x=\"2\" class=\"y\">a </p>",
            ),
            // other comments are untouched
            (
                "<p>a <!-- note .element: x --></p><!-- plain -->",
                "<p>a <!-- note .element: x --></p><!-- plain -->",
            ),
            // not inside raw text
            (
                "<script>var s = '<p>x</p><!-- .element: class=y -->';</script>",
                "<script>var s = '<p>x</p><!-- .element: class=y -->';</script>",
            ),
            // an element closed implicitly stays open until an ancestor closes
            (
                "<div><p>a<p>b</div><!-- .element: class=z -->",
                "<div class=\"z\"><p>a<p>b</div>",
            ),
            // ... or until the browser closes it: `<li>` closes `<li>`, `<ul>` closes `<p>`
            (
                "<ul><li>a<li>b<!-- .element: class=y --></ul>",
                "<ul><li>a<li class=\"y\">b</ul>",
            ),
            (
                "<p>a<ul><li>b</li></ul><!-- .element: class=x -->",
                "<p>a<ul class=\"x\"><li>b</li></ul>",
            ),
        ];
        for (html, expected) in cases {
            assert_eq!(applied(html), *expected, "{html:?}");
        }
    }

    #[test]
    fn element_comment_problems_are_reported() {
        let (out, found) = elements("<!-- .element: class=x --><p>a</p>");
        assert_eq!(out, "<!-- .element: class=x --><p>a</p>");
        assert_eq!(found[0].0, "element-comment-ignored");
        let (out, found) = elements("<p>a <!-- .element: title=\"open --></p>");
        assert_eq!(out, "<p>a <!-- .element: title=\"open --></p>");
        assert_eq!(found[0].0, "element-comment-ignored");
        assert!(found[0].1.contains("unbalanced"), "{}", found[0].1);
        let (out, found) = elements("<p>a <!-- .element: \"a b\"=1 ok=2 --></p>");
        assert_eq!(out, "<p ok=\"2\">a </p>");
        assert_eq!(
            found,
            [(
                "invalid-element-attr",
                "`a b` in `<!-- .element: \"a b\"=1 ok=2 -->` is not a valid attribute name \
                 (dropped)"
                    .to_string()
            )]
        );
    }

    #[test]
    fn html_without_element_comments_is_unchanged() {
        for html in [
            "",
            "<p>x</p>\n<!-- c -->\n",
            "<p>broken <b>markup</i> & stuff <!-- .elements -->",
        ] {
            assert_eq!(applied(html), html);
        }
    }

    #[test]
    fn incremental_lists_only_touch_top_level_items() {
        let html = "<h2>T</h2>\n<ul>\n<li>a\n<ul>\n<li>nested</li>\n</ul>\n</li>\n\
                    <li class=\"x\">b</li>\n<li class=\"fragment\">c</li>\n</ul>\n\
                    <ol start=\"3\">\n<li>d</li>\n</ol>\n\
                    <ul class=\"no-incremental\">\n<li>e</li>\n</ul>\n\
                    <div class=\"columns\">\n<ul>\n<li>f</li>\n</ul>\n</div>\n\
                    <blockquote>\n<ol>\n<li>g</li>\n</ol>\n</blockquote>\n";
        let expected = "<h2>T</h2>\n<ul>\n<li class=\"fragment\">a\n<ul>\n<li>nested</li>\n\
                        </ul>\n</li>\n<li class=\"x fragment\">b</li>\n\
                        <li class=\"fragment\">c</li>\n</ul>\n\
                        <ol start=\"3\">\n<li class=\"fragment\">d</li>\n</ol>\n\
                        <ul class=\"no-incremental\">\n<li>e</li>\n</ul>\n\
                        <div class=\"columns\">\n<ul>\n<li>f</li>\n</ul>\n</div>\n\
                        <blockquote>\n<ol>\n<li>g</li>\n</ol>\n</blockquote>\n";
        assert_eq!(incremental_lists(html).unwrap(), expected);
        // no list; a list in an unclosed raw `<div>` is not at the top level
        for unchanged in ["", "<p>no lists</p>\n", "<div>\n<ul>\n<li>x</li>\n</ul>\n"] {
            assert_eq!(incremental_lists(unchanged).unwrap(), unchanged);
        }
    }

    /// Raw author HTML: the top level is the one the browser builds.
    #[test]
    fn incremental_lists_follow_the_browser_tree_in_raw_html() {
        let cases: &[(&str, &str)] = &[
            // tag names in any case
            (
                "<UL>\n<LI>x</LI>\n</UL>\n",
                "<UL>\n<LI class=\"fragment\">x</LI>\n</UL>\n",
            ),
            // a stray end tag closes nothing
            (
                "<p>stray</div> <ul><li>x</li></ul>",
                "<p>stray</div> <ul><li class=\"fragment\">x</li></ul>",
            ),
            // a list closes an open `<p>`, as in the browser
            (
                "<p>open\n<ul>\n<li>x</li>\n</ul>\n",
                "<p>open\n<ul>\n<li class=\"fragment\">x</li>\n</ul>\n",
            ),
            // items without end tags close each other; their nested lists stay nested
            (
                "<ol><li>a<li>b<ul><li>c</ul><li>d</ol>",
                "<ol><li class=\"fragment\">a<li class=\"fragment\">b<ul><li>c</ul>\
                 <li class=\"fragment\">d</ol>",
            ),
            // closing a `<p>` does not leave the container it is in
            (
                "<div><p>x<ul><li>a</li></ul></div>",
                "<div><p>x<ul><li>a</li></ul></div>",
            ),
            // an `<li>` in a table cell does not close the item the table is in
            (
                "<ul><li><table><tr><td><li>in a cell</td></tr></table></li></ul>",
                "<ul><li class=\"fragment\"><table><tr><td><li>in a cell</td></tr></table>\
                 </li></ul>",
            ),
        ];
        for (html, expected) in cases {
            assert_eq!(incremental_lists(html).unwrap(), *expected, "{html:?}");
        }
    }

    #[test]
    fn added_classes_keep_the_existing_value_as_written() {
        assert_eq!(
            incremental_lists("<ul>\n<li class=\"a&amp;b\">x</li>\n</ul>\n").unwrap(),
            "<ul>\n<li class=\"a&amp;b fragment\">x</li>\n</ul>\n"
        );
        assert_eq!(
            applied("<p class=\"a&amp;b  \">t <!-- .element: class=\"f a&b\" --></p>"),
            "<p class=\"a&amp;b  f\">t </p>"
        );
        assert_eq!(
            applied("<p class=\"x\">t <!-- .element: class=\"c&d x c&d\" --></p>"),
            "<p class=\"x c&amp;d\">t </p>"
        );
    }

    #[test]
    fn slide_classes_override_the_config() {
        let classes =
            |list: &[&str]| -> Vec<String> { list.iter().map(|c| c.to_string()).collect() };
        assert!(!incremental_enabled(false, &[]));
        assert!(incremental_enabled(true, &[]));
        assert!(incremental_enabled(false, &classes(&["incremental"])));
        assert!(!incremental_enabled(true, &classes(&["no-incremental"])));
        assert!(!incremental_enabled(
            false,
            &classes(&["incremental", "no-incremental"])
        ));
    }

    /// End to end through `markdown::parse`: `slides.incremental_lists` and the slide classes.
    #[test]
    fn incremental_lists_in_a_deck() {
        use crate::config::Config;
        use crate::ir::{Block, Slide, SourceInfo, SourceKind};
        let text = "\
# Plain

- a
  - nested
- b

---

# Opt out {.no-incremental}

- c

---

# Opt in {.incremental}

1. d

--

## Child

- e
";
        let source = SourceInfo {
            path: "deck.md".into(),
            kind: SourceKind::Markdown,
            base_dir: ".".into(),
        };
        let fragments = |slide: &Slide| -> usize {
            match slide.blocks.as_slice() {
                [Block::Html { html }] => html.matches("<li class=\"fragment\">").count(),
                other => panic!("{other:?}"),
            }
        };
        for (config_on, expected) in [(false, [0, 0, 1, 0]), (true, [2, 0, 1, 1])] {
            let mut config = Config::default();
            config.slides.incremental_lists = config_on;
            let parsed = crate::markdown::parse(&source, text, &config).unwrap();
            let slides = &parsed.deck.slides;
            let found = [
                fragments(&slides[0]),
                fragments(&slides[1]),
                fragments(&slides[2]),
                fragments(&slides[2].children[0]),
            ];
            assert_eq!(found, expected, "incremental_lists = {config_on}");
            assert!(parsed.diagnostics.is_empty());
        }
        // the nested item never becomes a fragment
        let mut config = Config::default();
        config.slides.incremental_lists = true;
        let parsed = crate::markdown::parse(&source, text, &config).unwrap();
        let [Block::Html { html }] = parsed.deck.slides[0].blocks.as_slice() else {
            panic!()
        };
        assert!(html.contains("<ul>\n<li>nested</li>\n</ul>"), "{html}");
    }

    #[test]
    fn slide_html_runs_element_comments_before_incremental_lists() {
        let html = "<ul>\n<li>a</li>\n</ul>\n<!-- .element: class=\"no-incremental\" -->\n\
                    <ol>\n<li>b</li>\n</ol>\n";
        let mut diagnostics = Diagnostics::new();
        let out = slide_html(html, true, None, &mut diagnostics).unwrap();
        assert_eq!(
            out,
            "<ul class=\"no-incremental\">\n<li>a</li>\n</ul>\n\n<ol>\n\
             <li class=\"fragment\">b</li>\n</ol>\n"
        );
        assert_eq!(
            slide_html(html, false, None, &mut diagnostics).unwrap(),
            "<ul class=\"no-incremental\">\n<li>a</li>\n</ul>\n\n<ol>\n<li>b</li>\n</ol>\n"
        );
        assert!(diagnostics.is_empty());
    }
}
