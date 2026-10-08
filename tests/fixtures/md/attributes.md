---
# attributes.md: dialect §3 (slide attributes and element attributes). Behaviours:
#   - separator form `--- {…}` / `-- {…}` (parsed by markdown::attrs::parse_attr_block):
#       `--- {.dark background=#111 layout=title}` → classes ["dark"],
#         attrs {"data-background-color": "#111"}, layout "title" (consumed, never an attr);
#       `--- {background=bg.png}` → data-background-image (bg.png does not exist, so the
#         renderer also records a missing-asset warning; the value stays as written);
#       `-- {auto-animate}` → bare key, ("data-auto-animate", "") on a vertical slide;
#   - heading lifting: attributes of a slide's FIRST node, when it is a heading, move to
#     the slide and the heading is left clean:
#       `# T {#intro .big k=v}` → id "intro", classes ["big"], attrs {"k": "v"},
#         unknown_keys ["k"] (kept verbatim, `check` warns);
#       `## Lifted layout {layout=section .x}` → layout "section", classes ["x"];
#   - merging: separator attrs first, heading attrs win for the same key, classes
#     accumulate deduplicated in order (a, shared, b); heading `auto-animate=""` form;
#   - `background` colour detection: `#…`, `rgb(…)`/`rgba(…)`/`hsl(…)`/`hsla(…)` and CSS
#     colour keywords → data-background-color; anything else → data-background-image;
#   - every other shorthand gets a `data-` prefix (background-opacity, transition-speed,
#     visibility, state, …); keys starting with `data-` or `aria-` are verbatim;
#     single- and double-quoted values;
#   - a heading that is not the first node keeps its attributes on the element (§3.3),
#     as do links and inline code.
# Expected shape: 8 horizontal slides; slide 3 has one child.
title: Attributes
---

# Attributes

A plain first slide: no separator attributes, and its heading has none to lift.

--- {.dark background=#111 layout=title}

# Separator attributes

`.dark` → class, `background=#111` → `data-background-color`, `layout=title` → layout.

--- {background=bg.png}

## Image background

A `background` value that is not a colour becomes `data-background-image`.

-- {auto-animate}

## Bare key

`{auto-animate}` in the separator form → `data-auto-animate=""`.

---

# T {#intro .big k=v}

Heading lifting: id `intro`, class `big`, unknown key `k` kept verbatim.

--- {.a .shared #from-separator transition=fade data-state-note="from separator"}

## Merged {.b .shared #merged transition=zoom auto-animate=""}

Classes accumulate (`a shared b`); the heading wins for `id` and `transition`.

--- {background="rgba(0, 0, 0, 0.5)" background-opacity=0.4 aria-label='Single quoted'}

## Colours and verbatim keys

`rgba(…)` is a colour; `background-opacity` gets a `data-` prefix; `aria-label` is verbatim.

--- {background=navy background-size=contain transition-speed=fast visibility=uncounted state=calm}

## Keyword colour and more shorthands

`navy` is a CSS colour keyword.

---

## Lifted layout {layout=section .x}

The first heading's `layout=section` is lifted to the slide.

### Not the first node {#kept .muted}

This heading keeps its attributes, as do [links](https://example.org){.button target=_blank}
and inline code `x = 1`{.python}.
