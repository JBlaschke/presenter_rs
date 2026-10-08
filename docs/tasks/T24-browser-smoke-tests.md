# T24 — Headless-browser smoke tests (opt-in)

| | |
|---|---|
| Milestone | M2 |
| Size | S |
| Depends on | T16 (and T23 for the serve test) |
| Parallel-safe with | everything |
| Files you own | `tests/browser.rs`, `Cargo.toml` (feature `browser-tests` + optional dev-dep, additive), `.github/workflows/ci.yml` (additive job) |

## Goal
Catch the failures snapshot tests cannot: JavaScript errors, reveal not initializing, KaTeX
or highlight not running, live reload not swapping CSS.

## Steps
1. Add `headless_chrome = { version = "1", optional = true }` under `[dev-dependencies]` is not possible
   (optional dev-deps are not supported) — instead add it as a normal optional dependency gated by
   the `browser-tests` feature and only use it from `tests/browser.rs` behind `#[cfg(feature = "browser-tests")]`.
2. Tests (each skips with a clear message when no Chrome binary is found via the same discovery as T45;
   duplicate the 20-line discovery here if T45 is not merged):
   - build `examples/basic/slides.md` standalone → open `file://` → wait for `Reveal.isReady()` → assert
     `Reveal.getTotalSlides() >= 10`, `document.querySelectorAll('.katex').length > 0`,
     `document.querySelectorAll('pre code.hljs').length > 0`, zero console errors (collect via `Tab::enable_log` / `add_event_listener`).
   - serve: start `presenter serve --port 0`, open the URL, read `#presenter-theme` href, touch the theme CSS,
     wait until href changes (`?v=` increments) without `performance.navigation`/`PerformanceNavigationTiming` showing a reload.
3. CI: a separate job `browser` on `ubuntu-latest` (Chrome preinstalled) running
   `cargo test --features browser-tests --test browser`; allowed to be `continue-on-error: true` for the first week.

## Acceptance criteria
- Both tests pass locally with Google Chrome installed.
- Without Chrome the tests print `skipping: no chrome found` and pass.
- Default `cargo test` does not compile `headless_chrome`.

## Hints
- `headless_chrome::Browser::new(LaunchOptions { headless: true, path: Some(chrome), ..Default::default() })`,
  `tab.navigate_to(url)?.wait_until_navigated()?`, `tab.evaluate("Reveal.isReady()", false)?.value`.
