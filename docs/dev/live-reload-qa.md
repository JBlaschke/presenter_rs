# Live-reload client: manual QA checklist

Covers `assets/presenter/live-reload.js`, the browser side of `presenter serve`
(`docs/spec/live-reload.md` §3). Run it after changing the client, the dev server (T21),
the change classification (T20) or the serve-mode templates. T24 automates only the CSS swap
and the reload.

## Setup

### A. Mock server (no Rust needed)

`tests/fixtures/live-reload/mock-server.py` (Python 3 standard library only) serves a reveal
deck built from the vendored assets, with the routes of live-reload.md §1, and has control
routes that fake rebuilds. Its docstring lists them.

```sh
python3 tests/fixtures/live-reload/mock-server.py --port 8765
S=http://127.0.0.1:8765
```

Open `$S/#/1/1` in Chrome. Set the DevTools console level to "Verbose", because the client
logs with `console.debug`. Then run `window.__qaMarker = 1` in the console: the variable
survives only if no full reload happens. Trigger builds with:

| what | command |
|---|---|
| edit the theme CSS | `curl --data-binary '.reveal h2 { color: red }' $S/__mock/css && curl "$S/__mock/emit?kind=css"` |
| edit slides | `curl -s $S/__mock/slides > s.html`, edit `s.html`, then `curl --data-binary @s.html $S/__mock/slides && curl "$S/__mock/emit?kind=content"` |
| change the title | `curl "$S/__mock/title?t=New"` and then a content build |
| change head stylesheets | `curl --data-binary '<link rel="stylesheet" href="/__presenter/asset/vendor/reveal/dist/theme/black.css">' $S/__mock/head` and then a content build |
| parse error | `curl "$S/__mock/emit?kind=error"` (by default `slides.md:41: unknown directive 'include'`; override with `&message=…&file=…&line=…`) |
| "no good build yet" page | `curl "$S/__mock/emit?kind=error&nodoc=1"` and then reload the page |
| server unreachable | `curl "$S/__mock/down?ms=3000"`, or stop the process with Ctrl-C |
| server restart | `curl $S/__mock/restart`, or start the process again after stopping it |
| start over | `curl $S/__mock/reset` |

`curl $S/__mock/log` returns and clears the list of requests the page made, including
`Sec-Fetch-Dest`, so you can see exactly what the client fetched.

Useful console expressions: `Reveal.getIndices()`, `__presenterLiveReload.state`
(`buildId`, `connected`, `retryMs`, `lastStatus`, `swaps`),
`document.getElementById('presenter-theme').getAttribute('href')` and `__qaMarker`.

### B. `presenter serve` (T23)

Run `cargo run -- serve examples/basic/slides.md` and repeat the checklist by editing the
real files: the theme's CSS for item 1, `slides.md` for items 2–6 and 9–17, `theme.toml` for
item 16. For items 7 and 8, stop and restart the server process.

## Checklist

| # | step | expected |
|---|---|---|
| 1 | Edit the theme CSS while on a slide | no reload (the marker survives); same slide; `#presenter-theme` href is `/__presenter/theme.css?v=<build_id>`; theme-related `/__presenter/asset/…theme…` stylesheets get `?v=<build_id>`; each old `<link>` is removed only after its replacement has loaded (no flash of unstyled content) |
| 2 | Edit the text of the current slide | in-place swap with no reload; same slide and fragment; the client requests only `GET /` and `theme.css?v=<id>` |
| 3 | Add a slide | the new slide exists and can be reached; the current slide is unchanged |
| 4 | Remove the current slide (last horizontal; bottom of a vertical stack) | no reload; indices are clamped to the new counts |
| 5 | Introduce a parse error | a fixed overlay at the top right (≤ 50 % width, monospace, z-index 10000) shows `build error (<kind>)`, the message, `file:line` and `(click to dismiss)`; the deck stays visible; clicking hides the overlay and the next error shows it again |
| 6 | Fix the error | the overlay disappears on the next good build (css or content); the first good build after an error also refreshes the slides |
| 7 | Stop the server | the deck stays usable; the client retries after 0.5, 1, 2, 4 and then every 5 s (`connection lost, retrying in … ms`); no reload |
| 8 | Restart the server | on reconnect, `hello` carries a different `build_id`: exactly one full reload, on the same slide (URL hash) |
| 9 | Edit a slide with a stepped code block (`data-line-numbers="1\|2"`) while on a step | after the swap, highlighting, line numbers, `data-trim` de-indentation and the current step are the same as after a page load |
| 10 | Edit a math slide | KaTeX is rendered in the swapped slides |
| 11 | Change the deck title | `document.title` is updated without a reload |
| 12 | Swap while in overview mode and paused | both states are kept |
| 13 | Add a `data-visibility="hidden"` slide | it stays hidden after the swap, as on load |
| 14 | Burst of builds (content/css alternating) | applied in order; the final state matches the last build |
| 15 | Builds that happen while disconnected | one full reload on reconnect |
| 16 | A build whose document has other stylesheets or scripts (e.g. `reveal_theme` changed) | one full reload |
| 17 | Slides that contain a `<script>` (notebook HTML outputs) | every content build reloads in full, so the scripts run |
| 18 | Open the page while the latest build failed, or before any build succeeded | the overlay is shown right away (from `hello`); on the fallback page, the first good build reloads into the deck |
| 19 | Console during the whole run | no uncaught exceptions; every client message starts with `[presenter]` |

## Results

### 2026-10-08: mock server (T22)

Environment: macOS, Google Chrome 154.0.8037.98 (headless, driven over the DevTools
protocol), Node 26.10.0, Python 3.14.8. `live-reload.js` was 7988 bytes, sha256
`21b6ce360242ff80…`.

Method: each item ran as a scripted browser session. The page was opened at the slide given
below, builds were triggered through the mock's control routes, and the assertions ran in
the page. `window.__qaMarker` detected full reloads, and `/__mock/log` recorded the requests.
Every item passed.

| # | result | evidence |
|---|---|---|
| 1 | pass | Opened at `#/1/1`. After the CSS build, the indices were still `{h:1,v:1}` and the marker was kept. The links were `/__presenter/theme.css?v=2` and `…/dist/theme/white.css?v=2`, and exactly those two stylesheets were requested. A MutationObserver showed that both replacement links already had a loaded sheet when the old links were removed, and exactly one `#presenter-theme` remained. |
| 2 | pass | After the content build, the indices were still `{h:1,v:1}` and the same section had `present`. The requests were `GET /` (`fetch`) and `GET /__presenter/theme.css?v=3`. |
| 3 | pass | 6 → 7 slides; the indices were unchanged; `Reveal.slide(5)` reached the new slide. |
| 4 | pass | From the last slide (h=5), removing it gave h=4 (the new last slide), and the hash became `#/last` once reveal's 1 s `replaceState` debounce had passed. From the bottom of a vertical stack (1/1), removing it gave `{h:1,v:0}`. |
| 5 | pass | The overlay was `position: fixed`, `z-index: 10000`, monospace, 349 px wide at a 1280 px viewport, at the top right. Its text was `build error (parse)` / `slides.md:41: unknown directive 'include'` / `slides.md:41` / `(click to dismiss)`. The deck stayed visible and the error triggered no requests. A click hid the overlay, and the next error (`theme.scss:3`) showed it again with the new text. |
| 6 | pass | A css build after the error hid the overlay and refreshed the slides and all theme stylesheets (`GET /`, `theme.css`, `white.css`). An error followed by a content build also hid the overlay and showed the new text. |
| 7 | pass | With the mock answering 503 for 2.5 s, the client retried after 500, 1000 and 2000 ms, reconnected at 3.5 s, reset the delay to 500 ms, did not reload, and applied the next edit. With the server process killed for 16 s, it retried after 500, 1000, 2000, 4000, 5000 and 5000 ms, and the deck (marker) stayed until the restart. |
| 8 | pass | A mock restart (build id 2 → 1) and a real process restart (4 → 1) each caused exactly one reload. The slide came back from the hash (`#/math`, `#/stack-bottom`), and no second reload followed. |
| 9 | pass | On step 0 of the code slide (`{h:2,v:0,f:0}`), the code block had the same structure before and after the swap: two `code` elements, `.code-wrapper`, `hljs`, the line-number table, the first line `fn main() {` with its indentation trimmed, and the step clone still `visible current-fragment`. The indices were kept. |
| 10 | pass | The swapped math slide had one `.katex`. |
| 11 | pass | `document.title` became `New title` with no reload. |
| 12 | pass | `Reveal.isOverview()` and `Reveal.isPaused()` were both still true after the swap. |
| 13 | pass | The hidden slide was not in the DOM after the swap. |
| 14 | pass | After 5 builds, the last edit was shown, the stored build id was the last one, and the only `#presenter-theme` pointed at `?v=<last>`. |
| 15 | pass | The page reloaded once and showed the edit made while it was disconnected. |
| 16 | pass | An extra reveal theme stylesheet in the new document caused one reload, and the new theme was applied. |
| 17 | pass | Adding a slide script caused a reload, and the script ran. A later text edit, with the script unchanged, reloaded again. |
| 18 | pass | Opened while the latest build had failed: the overlay showed `boom` over a working deck. On the fallback page, the overlay came from `hello`, and the first good build reloaded into the deck (`cannot update in place`). |
| 19 | pass | No exceptions occurred in any session, and every client message was `[presenter] …`. The only other console lines were Chrome's own `Failed to load resource` errors for `/__presenter/events` during the deliberate outages (503 and `ERR_CONNECTION_REFUSED`). |

Still to do: repeat this checklist against `presenter serve` once T23 has landed.

## Behaviour beyond the §3 pseudo-code (why each item above exists)

- The children of `.reveal .slides` are swapped, not the element itself. reveal caches
  that element at init, attaches its `click`/`transitionend` listeners to it and sets its
  layout styles on it.
- Hidden slides are removed (`Reveal.removeHiddenSlides()`), math is rendered and code is
  re-highlighted *before* `Reveal.sync()`/`setState()`, so that reveal sees the step
  fragments that `highlightBlock` creates and can restore the fragment index.
- A content build also re-requests `#presenter-theme`, because `theme.toml` and
  `presenter.toml` edits are content builds that can change the theme CSS. After an error
  build, the next good build is applied as both content and css, since it may contain any
  change made in between.
- When the stylesheets and scripts of the new document differ from those the page was loaded
  with (ignoring `?v=`), or when the new slides contain a `<script>`, the client reloads in
  full instead of swapping.
- The build id is stored in `sessionStorage` once an update has been applied, and before any
  reload. A `hello` after an update that failed therefore reloads, and a reloaded page does
  not reload a second time.
- A `hello` with `status: "error"` shows the overlay, which covers T21's fallback page.

## Known limitations

- When in doubt, the client reloads in full. reveal's `hash: true` (the presenter default)
  brings the reloaded page back to the same slide.
- For up to 1 s after a swap that moved the slide, the URL hash can still name the previous
  slide, because reveal debounces `replaceState`. This matters only if a full reload happens
  within that second.
- The protocol has no server instance id, so a restarted server whose first `build_id`
  happens to equal the stored one does not cause a reload. Changes then show up with the next
  build.
- A theme script that inserts `<link>` or `<script>` elements before the client runs makes
  every content build reload in full.
