---
title: A theme in SCSS
subtitle: Derived from reveal.js's own theme template
author: Ada Lovelace
date: 2026-10-08
theme: theme
---

# A theme in SCSS {layout=title}

---

## What this deck shows

- The theme in `theme/` is written in **SCSS** and compiled by presenter
- It configures reveal's theme template with `@use … with (…)`
- Its headings use *Georgia*, its text reveal's **Source Sans Pro**
- Links are amber: [reveal.js](https://revealjs.com)

---

## The stylesheet

```scss
@use "palette" as p;
@use "reveal/theme/template/settings" with (
  $background-color: p.$night,
  $heading-font: "Georgia, serif",
  $link-color: p.$amber
);
@use "reveal/theme/template/theme";
```

---

## Everything else comes from `default`

| What | Where it comes from |
|---|---|
| Layouts, code blocks, tables | the `default` theme |
| Colours and fonts | `theme/styles/theme.scss` |
| `--p-accent` and friends | set from the palette |

> A child theme only overrides what it needs.

---

# Section dividers {layout=section}

---

## A light slide {background="#f4efe6"}

reveal's `light-bg-text-color` mixin keeps this text dark on a light `background=`.
