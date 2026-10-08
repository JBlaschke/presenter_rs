---
# math.md: dialect §8 (math), plus the ```math fence from §5. Behaviours:
#   - `$…$` → <span data-math-style="inline">TeX</span>;
#   - `$$…$$` (also spanning lines) → <span data-math-style="display">TeX</span>;
#   - TeX is HTML-escaped inside the span: `$a < b$` → `a &lt; b`;
#   - a literal dollar sign is written `\$` and stays text;
#   - math inside a list item and a table cell works the same way;
#   - a ```math fence → <div data-math-style="display">TeX</div> (T12 rewrites comrak's
#     <pre><code class="language-math" data-math-style="display">);
#   - any math sets Features.math.
# Expected shape: 2 horizontal slides.
title: Math
---

## Inline and display

Inline $E = mc^2$, and escaped TeX $a < b$.

$$
\int_0^1 x^2 \, dx = \frac{1}{3}
$$

The cup costs \$5, not $5$ dollars.

---

## Math in other blocks

- a list item with $\alpha + \beta$
- display in a list: $$\sum_i x_i$$

| quantity | formula |
|---|---|
| area | $\pi r^2$ |

```math
\nabla \cdot \mathbf{E} = \frac{\rho}{\varepsilon_0}
```
