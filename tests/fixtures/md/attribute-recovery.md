---
# attribute-recovery.md: dialect §3.1 (A3 contract note). comrak 0.56 rejects an attribute
# block on a heading, fenced code, image, link or inline code when a value is single-quoted
# or an unquoted value has characters other than letters, digits and `- _ . : %`; the
# frontend then reads it with the §3.1 grammar and removes it from the text. Behaviours:
#   - `# Dark {.dark background=#222}` → lifted: class dark, data-background-color #222;
#   - `## Image {background=images/ok.png}` → data-background-image, registered as an asset;
#   - `## Remote {background=https://example.org/bg.png}` → the URL is no autolink in the <h2>;
#   - `## Quoted {#q data-note='two words'}` → id q and data-note on the slide;
#   - fences `python {lines=1|3}` and `{.numbered lines=1,3}` (no language);
#   - `![…](images/ok.png){width='50%'}`, `[…](…){data-x='y z'}` and `` `code`{data-x=a/b} ``;
#   - `{title="…"}` on a link and an image without a Markdown title is written (F35);
#   - inline math in a slide title: `# The $E=mc^2$ law` → title "The E=mc^2 law" (F34);
#   - text in braces stays text (`{1, 2, 3}`); a block meant as attributes that is not valid
#     stays text and is an `unparsed-attr-block` warning (`{.big auto-animate}`: a key
#     needs a value outside `--- {…}`).
# Expected shape: 6 horizontal slides.
title: Attribute recovery
---

# Dark {.dark background=#222}

Heading attributes with a `#` in a value.

---

## Image {background=images/ok.png}

A relative image background on a heading.

---

## Remote {background=https://example.org/bg.png}

A URL in a heading's attributes.

---

## Quoted {#q data-note='two words'}

```python {lines=1|3}
a = 1
b = 2
c = 3
```

``` {.numbered lines=1,3}
x
y
z
```

---

## Inline elements

![Half](images/ok.png){width='50%'} and [a link](https://example.org){data-x='y z'}
and `code`{data-x=a/b}.

[Titled link](https://example.org){title="Link title"} ![Titled image](images/ok.png){title="Image title"}

---

# The $E=mc^2$ law

The set {1, 2, 3} stays text.

### Not attributes {.big auto-animate}
