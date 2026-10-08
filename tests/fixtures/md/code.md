---
# code.md: dialect §5 (fenced code blocks), one slide per row of the §5 table.
# comrak parses `{…}` after the language into node attrs (info stays "python").
# Behaviours:
#   1. first word of the info string → class="language-<word>" on <code>; source is
#      HTML-escaped (`<`, `>`, `&`); the trailing newline is removed; data-trim present;
#   2. no info string → no class on <code> (highlight.js auto-detects);
#   3. `lines="1-2|4"` → data-line-numbers="1-2|4" (`|` separates steps);
#   4. `.numbered` → data-line-numbers="" ; `.numbered` plus `lines=` → lines wins;
#   5. `start=10` → data-ln-start-from="10";
#   6. `trim=false` → no data-trim;
#   7. `escape=false` → data-noescape and the source is emitted UNESCAPED (<mark> intact);
#   8. `.plain` → class="nohighlight" on <code>;
#   9. `#id` and other `.classes` → on <pre>;
#  10. other `key=value` → data-<key>="<value>" on <code>;
#  11. the combined example line from §5 (the table is normative: trim=false omits
#      data-trim, escape=false adds data-noescape).
# Every slide sets Features.code. Expected shape: 11 horizontal slides.
title: Code blocks
---

## Language and escaping

```python
def greet(name):
    return f"<b>{name}</b> & co"
```

---

## No language

```
plain text without an info string
```

---

## Line steps

```python {lines="1-2|4"}
import polars as pl
df = pl.read_csv("data.csv")

print(df.describe())
```

---

## Numbered

```rust {.numbered}
fn main() {
    println!("every line numbered");
}
```

```rust {.numbered lines="2"}
fn main() {
    println!("lines= wins over .numbered");
}
```

---

## Start

```js {lines="1|2" start=10}
const a = 1;
const b = 2;
```

---

## No trim

```text {trim=false}
    the first line keeps its indentation
```

---

## Unescaped

```html {escape=false}
<p>Some <mark>highlighted</mark> text &amp; an entity, kept as written.</p>
```

---

## Plain

```python {.plain}
print("not highlighted")
```

---

## Pre id and classes

```python {#pre-id .big .wide}
x = 1
```

---

## Extra keys

```python {title=Example foo=bar}
y = 2
```

---

## Everything at once

```python {lines="1-2|4" .numbered start=10 trim=false escape=false #snippet .big}
import polars as pl
df = pl.read_csv("data.csv")

print(df.<mark>describe</mark>())
```
