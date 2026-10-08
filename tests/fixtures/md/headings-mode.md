---
# headings-mode.md: dialect §2.3, `slides.split = "headings"` (T43). Behaviours:
#   - no separator lines needed: `#` headings (level < slides.slide_level, default 2) start
#     horizontal slides, `##` headings (level == slide_level) start vertical slides under
#     the `#` slide, `###` and deeper headings are ordinary content;
#   - the boundary heading is the slide's first visible node, so its `{…}` attributes are lifted
#     onto the slide as in rules mode;
#   - non-blank content before the first heading is an initial slide;
#   - separators are still honored (mixed mode): `---` starts a horizontal slide and `--` a
#     vertical one; a heading right after a separator joins that slide instead of starting
#     another one; after `---`, `##` headings have no `#` slide above them and start
#     horizontal slides until the next `#`;
#   - a `<!-- .slide: -->` comment right after a slide's heading gives the slide attributes;
#   - plain HTML comments are invisible: a heading after a separator and a comment still
#     joins the separator's slide, and its attributes are lifted.
# Expected shape with slide_level = 2: 6 horizontal slides; slide 2 has children 2.1 and
# 2.2, slide 6 has one child. With slide_level = 3 (`#` and `##` horizontal, `###`
# vertical): 8 horizontal slides; slide 3 and slide 8 have one child each.
title: Headings mode
slides:
  split: headings
---

Text before the first heading is an initial slide, because it is not blank.

# Introduction {#intro layout=section}

## Motivation

Why we migrate from pandoc.

### A level-3 heading is content

- point one
- point two

## Plan {.dark}
<!-- .slide: data-transition="fade" -->

1. Parse
2. Render

# Results

Some text.

---

<!-- a plain comment between the rule and the heading changes nothing -->

## After a rule {.wide}

A `---` line starts a horizontal slide; this heading joins it.

## Another

No `#` heading above it since the rule, so this slide is horizontal too.

# Summary

--

## Backup

A `--` line starts a vertical slide; this heading joins it.
