---
# pandoc-directives.md: dialect §4 (A3 contract note): pandoc's fenced-div attribute syntax
# on directives, and the fragment index. Behaviours:
#   - `::: {.fragment .fade-up}` is `:::fragment fade-up`;
#   - `:::: {.columns widths="1fr 2fr"}` and `::: {.column width="40%"}` are the columns
#     directives (grid style; the column width is consumed);
#   - `::: {#intro .note}` → <div id="intro" class="note">;
#   - `::: {.notes}` is `:::notes`: speaker notes, never on the slide;
#   - `:::fragment index=first` → no data-fragment-index (reveal.js would never show the
#     fragment) and an `invalid-directive-arg` warning; `index=2` stays.
# Expected shape: 2 horizontal slides.
title: Pandoc directives
---

## Pandoc fenced divs

::: {.fragment .fade-up}
Fades up.
:::

:::: {.columns widths="1fr 2fr"}
::: {.column width="40%"}
Left.
:::
::: {#intro .note}
Right.
:::
::::

::: {.notes}
Speaker notes in pandoc syntax.
:::

---

## Fragment indices

:::fragment index=first
Not a number: the index is dropped.
:::

:::fragment index=2
Second.
:::
