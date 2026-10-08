---
# Front matter: settings for this deck. The title slide shows the subtitle, author and
# date under the heading. `event` is not a built-in key: theme/templates/layouts/title.html
# adds it as a last line.
title: {{name_yaml}}
subtitle: What this talk is about
author: Your Name
# date: 2026-01-31
event: Event name, City
---

# {{name}} {layout=title}

:::notes
These are speaker notes. Press <kbd>S</kbd> to open the speaker view: it shows the notes,
the next slide and a timer. Notes never appear on the slides themselves.
:::

---

## Slides are Markdown

- A line with just `---` starts the next slide
- A line with just `--` starts a slide below the current one
- While `presenter serve` runs, every save updates the browser

Press <kbd>↓</kbd> for the slide below.

--

## A slide below

Slides below a slide hold the details of its topic.

Press <kbd>→</kbd> to move on, or <kbd>Esc</kbd> for an overview of all slides.

---

## One step at a time

:::fragment
A `:::fragment` block appears on the next click…
:::

:::fragment
…and the next one on the click after that.
:::

More effects: [fragments in reveal.js](https://revealjs.com/fragments/)

---

## Two columns

<!-- Nested blocks need a longer outer fence: ::::columns around :::column. -->
::::columns
:::column
**Content:** `slides.md`

Markdown with a few extras, like these columns
:::
:::column
**Look:** `theme/`

Colors in `styles/theme.css`, HTML in `templates/`
:::
::::

---

## Code

```python {lines="1-2|4-5"}
def greet(name):
    return f"Hello, {name}!"

for name in ["Ada", "Grace"]:
    print(greet(name))
```

Each click highlights the next group of lines.

:::notes
The fence is written as `python {lines="1-2|4-5"}`; `|` separates the steps.
:::
