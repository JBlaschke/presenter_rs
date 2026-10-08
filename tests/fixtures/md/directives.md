---
# directives.md: dialect §4 (`:::name args` … `:::` block directives). Behaviours:
#   - `:::notes` is removed from the body; its content is rendered to HTML into
#     Slide.notes (the body text around it stays);
#   - `:::fragment` → <div class="fragment">; effects become classes; `index=N` →
#     data-fragment-index="N": `:::fragment fade-up index=2` →
#     <div class="fragment fade-up" data-fragment-index="2">;
#   - `:::columns widths="1fr 2fr"` (a quoted arg with a space) →
#     <div class="columns" style="grid-template-columns: 1fr 2fr">, with nested
#     `:::column` / `:::column width=40%` → <div class="column">;
#   - NESTING with comrak 0.56: the OUTER fence must be longer than the inner ones
#     (`::::columns` … `::::` around `:::column` … `:::`). With equal-length fences the
#     first `:::` closes the outer directive.
#   - any other name → comrak's default <div class="name args">: `:::center`,
#     `:::warning large` (extra tokens are classes), `:::r-stack`;
#   - unconsumed `key=value` args → data-<key>="<value>": `:::callout tip title=Hint`;
#   - `:::include path` is reserved, not implemented (`check` warns "unknown directive
#     include").
# Expected shape: 5 horizontal slides, no vertical slides. Slide 1 has notes.
title: Directives
---

# Directives

:::notes
Notes from a `:::notes` block are removed from the body and become *Slide.notes*.
:::

Body text before and after the notes block stays on the slide.

---

## Fragments

:::fragment
Appears on click.
:::

:::fragment fade-up index=2
Second, fading up, with an explicit index.
:::

:::fragment highlight-red index=1
Shown first because of `index=1`.
:::

---

## Columns

::::columns widths="1fr 2fr"
:::column
Left column.
:::
:::column width=40%
Right column, twice as wide.
:::
::::

---

## Other names

:::center
`:::center` → `<div class="center">`.
:::

:::warning large
Extra tokens become classes: `<div class="warning large">`.
:::

:::callout tip title=Hint
The unconsumed `title=Hint` becomes `data-title="Hint"`.
:::

:::r-stack
reveal's own layout helper class `r-stack`.
:::

---

## Reserved

:::include other.md
`:::include` is reserved and not implemented.
:::
