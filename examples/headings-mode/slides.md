---
title: Migrating a pandoc deck
subtitle: Headings mode in presenter
author: Ada Lovelace
date: 2026-10-08
---

# Migrating a pandoc deck {layout=title}

:::notes
Welcome everyone. This deck has no `---` lines at all: the headings make the slides.
:::

# How headings become slides {layout=section}

## Sections and slides

With `split = "headings"` and `slide_level = 2`:

- a `#` heading starts a horizontal slide (a section)
- a `##` heading starts a vertical slide under it
- `###` and deeper headings are ordinary content

### Like this one

It stays on the slide above.

## Attributes still work {.dark}

`## Title {.dark}` lifts the class onto the slide, as in rules mode.

- this list appears item by item,
- because `incremental_lists = true`
  - nested items come with their parent

## Opting out {.no-incremental}

- the slide class `no-incremental`
- shows these items at once

# Mixing in separators

A `---` or `--` line still starts a slide, so a deck can move over gradually.

---

## After a rule

A heading right after a separator belongs to the separator's slide.

## No section above

Since the `---`, no `#` heading came, so this slide is horizontal as well.

# reveal.js habits

## Comments and notes
<!-- .slide: data-transition="fade" -->

- a `<!-- .slide: -->` comment under the heading sets slide attributes
- an `.element:` comment sets them on the element before it, or on the enclosing one

This sentence is set smaller by such a comment. <!-- .element: style="font-size: 0.6em" -->

Note: A paragraph starting with `Note:` ends the slide. It and everything after it
become speaker notes (press `s`).

# Questions? {layout=section}
