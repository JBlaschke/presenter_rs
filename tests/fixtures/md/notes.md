---
# notes.md: dialect §7 (speaker notes). Behaviours:
#   - `:::notes … :::` anywhere in a slide (before, between and after body content) is
#     removed from the body; its content is rendered as HTML (emphasis, lists, code) into
#     Slide.notes;
#   - several notes blocks on one slide are appended in order;
#   - a slide without notes has Slide.notes = None;
#   - notes work on vertical child slides too;
#   - any notes set Features.notes.
#   (The `Note:` paragraph convention is reveal compat, tested in reveal-compat.md by T43.)
# Expected shape: 2 horizontal slides; slide 2 has one child.
title: Notes
---

# Notes

:::notes
First block, with *emphasis* and a list:

- point one
- point two
:::

The slide body continues after a notes block.

:::notes
Second block, appended after the first, with `inline code`.
:::

More body text.

:::notes
Third block, at the very end of the slide.
:::

---

## No notes

This slide has no notes.

--

## Notes on a vertical slide

Vertical body.

:::notes
Notes work on vertical children too.
:::
