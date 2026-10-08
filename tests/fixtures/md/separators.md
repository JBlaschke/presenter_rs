---
# separators.md: dialect §2.2, rules mode (the default `slides.split = "rules"`).
# Separators are decided on the comrak AST, top-level nodes only. Behaviours:
#   - this front matter is neither a slide nor a separator;
#   - content before the first separator is slide 1;
#   - `---` (ThematicBreak) starts a new horizontal slide; `***` and `___` too;
#   - `--` (a paragraph that is exactly `--`, blank line before) starts a vertical
#     child of the current horizontal slide; a second `--` adds another child;
#   - `---` while a vertical stack is open closes the stack;
#   - `---` and `--` inside a fenced code block, a list item, a block quote and a
#     directive are NOT separators (they are not top-level nodes);
#   - setext pitfall: `Some text` directly followed by a `--` line is an <h2>, not a
#     separator (`check` warns about the raw `--` line);
#   - two consecutive `---` yield an empty slide (not collapsed; `check` warns);
#   - `--` directly after a horizontal separator gives the new slide an empty first
#     vertical slide (`check` warns).
# Expected shape: 7 horizontal slides. Slide 2 has children 2.1 and 2.2; slide 4 is
# empty; slide 7 is empty and has one child, 7.1. Slide 3 holds every non-separator.
title: Separators
---

# Slide 1

Content before the first separator is slide 1.

---

## Slide 2

A `---` line (a thematic break) starts a new horizontal slide.

--

## Slide 2.1

A paragraph that is exactly `--` starts a vertical slide: the first child of slide 2.

--

## Slide 2.2

A second `--` adds another child to the same vertical stack.

---

## Slide 3

The `---` above closed the vertical stack. None of the following are separators:

```text
---
--
```

- a list item

  --

- another list item

> A block quote
>
> ---
>
> still quoted

:::note
Inside a directive

---

still inside
:::

Some text
--

The `--` line directly under "Some text" turned it into a setext heading.

---
---

## Slide 5

The two consecutive `---` above produced an empty slide 4.

***

## Slide 6

`***` is a thematic break, so it starts a horizontal slide too.

___

--

## Slide 7.1

`--` directly after a horizontal separator (`___` here): slide 7 is empty and this is
its first vertical child.
