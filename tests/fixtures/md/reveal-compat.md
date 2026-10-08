---
# reveal-compat.md: reveal.js Markdown conventions (dialect §2.2 row 4, §3.2, §3.3, §7; T43).
# Rules mode (the default). Behaviours:
#   - `<!-- .slide: … -->` as a slide's first node gives the slide attributes and is removed:
#     keys are normalized like `--- {…}` keys (`data-background` stays verbatim, the
#     `background=` shorthand expands), `class=` and `.class` add classes, `#id` sets the id;
#     a `data-background` value that reveal takes for an image becomes `data-background-image`
#     (registered as an asset);
#   - plain comments are invisible: before the first heading they change nothing (the
#     heading's attributes are lifted, a `.slide:` comment under it is read);
#   - precedence: separator `{…}` < `.slide:` comment < heading `{…}` (later wins; classes
#     accumulate);
#   - the comment may also come right after the slide's first heading; anywhere else (also
#     inline in a paragraph) it stays a plain comment and is reported
#     (`misplaced-slide-comment`);
#   - a top-level paragraph starting with `Note:` or `Notes:` ends the slide body: it
#     (without the marker) and everything after it become notes, before the notes of the
#     `:::notes` blocks; `**Note:**` and `Note\:` stay content;
#   - `<!-- .element: … -->` gives attributes to the element before it at the same level,
#     or else to the enclosing one: the `<li>` of a list item, the `<img>` before it, the
#     `<p>` of a paragraph's text, the `<ul>` before a comment on its own line; the
#     comments are removed.
# Expected shape: 6 horizontal slides; slide 3 has one child.
title: Reveal compat
---

<!-- .slide: data-background="#f00" class="x" -->

# Slide attributes

The comment above gives this slide `data-background="#f00"` and the class `x`.

---

<!-- .slide: background=navy #compat .y -->
<!-- .slide: data-state="second-comment" -->

## Shorthand, id and two comments

`background=navy` becomes `data-background-color`.

--- {.from-separator data-state="separator" data-x="separator"}

<!-- .slide: data-state="comment" class="from-comment" -->

## Precedence {data-x="heading"}

The separator, then the comment, then the heading: later wins.

--

## After the heading
<!-- .slide: data-auto-animate="" -->

A `.slide:` comment directly under the heading also counts.

<!-- .slide: class="ignored" -->

That one is not at the start of the slide: it stays a comment.

---

## Notes

Body text.

:::notes
Directive notes come after the `Note:` part.
:::

**Note:** emphasised, so this paragraph is content.

Note: These are *speaker notes*.

- they continue
- to the end of the slide

---

## Element attributes

- item <!-- .element: class="fragment" -->
- second item <!-- .element: class="fragment" data-fragment-index="1" -->

![](images/ok.png) <!-- .element: width="300" -->

A paragraph with a class. <!-- .element: class="small" -->

- a list whose items stay put
- even with incremental lists on

<!-- .element: class="no-incremental" -->

---

<!-- TODO: a plain comment first changes nothing on this slide -->

## Comment first {#comment-first .wide}
<!-- .slide: data-background="images/ok.png" -->

The heading's attributes are lifted and the `.slide:` comment under it is read.

Note\: an escaped colon keeps this paragraph on the slide.

Inline comments are not read: <!-- .slide: class="inline" -->
