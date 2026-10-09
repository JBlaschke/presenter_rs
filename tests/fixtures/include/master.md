---
# master.md (T48, markdown-dialect §4.1): a Markdown deck that includes a notebook
# (notebooks/analysis.ipynb) and a Markdown part (parts/part.md, which includes
# parts/nested.md in turn). Expected slides, in order:
#   1 "Composed deck" (this file; an image next to it)
#   2 "Notebook slide" with the child "Notebook sub-slide", 3 "Second notebook slide"
#   4 "Between" (this file; math) with the child "Below between"
#   5 "Part one" (part.md; an image next to part.md) with the child "Part one, below"
#   6 "Nested" (nested.md; a code fence)
# part.md sets another title and theme: one include-meta-ignored warning at line 36.
title: Composed deck
theme: default
---

# Composed deck {layout=title}

![master image](images/master.png)

---

:::include notebooks/analysis.ipynb
:::

---

## Between {#between}

Some math: $E = mc^2$

--

## Below between

---

:::include parts/part.md
:::
