---
# The notebook's first cell sets this deck metadata too. An included file's deck metadata
# is ignored, and `presenter check` names what differs; the same values here keep it quiet.
title: Smoothing a noisy signal
subtitle: An executed Jupyter notebook, rendered by presenter
author: presenter examples
theme: default
---

:::include ../notebook/analysis.ipynb
:::

---

## Why a centred window?

A trailing window averages the last $2k + 1$ samples, so the smooth curve lags $k$ samples
behind the data. A centred window takes $k$ samples on each side instead:

```python {lines="1|2"}
trailing = series.rolling(2 * k + 1).mean()
centred = series.rolling(2 * k + 1, center=True).mean()
```

:::notes
This slide is written in Markdown, between the notebook's slides and `part2.md`.
:::

--

## Choosing $k$

| $k$ | window | noise left |
|---:|---:|---|
| 3 | 7 samples | most of it |
| 7 | 15 samples | about half |
| 15 | 31 samples | little, but the peaks flatten |

---

:::include part2.md
:::
