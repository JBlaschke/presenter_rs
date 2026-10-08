// KaTeX initialisation for presenter (docs/spec/vendored-assets.md §2, theme-format.md §6).
// Renders every [data-math-style] element ("inline" or "display") with katex.render, using
// window.__presenterKatexOptions (from [math] katex_options, set by reveal_init()) on top of
// { displayMode, throwOnError: false }. Rendered elements are marked data-rendered="1", so
// window.__presenterRenderMath(root) can be called again on new content (live reload).
(function () {
  function render(root) {
    if (!window.katex) return;
    root.querySelectorAll('[data-math-style]').forEach(function (el) {
      if (el.dataset.rendered) return;
      var display = el.dataset.mathStyle === 'display';
      try {
        katex.render(el.textContent, el, Object.assign(
          { displayMode: display, throwOnError: false },
          window.__presenterKatexOptions || {}));
        el.dataset.rendered = '1';
      } catch (e) {
        console.warn('[presenter] katex', e);
      }
    });
  }
  window.__presenterRenderMath = render;
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', function () { render(document); });
  } else {
    render(document);
  }
})();
