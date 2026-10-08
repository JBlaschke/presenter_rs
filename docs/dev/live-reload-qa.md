# Live-reload client: manual QA checklist

Covers `assets/presenter/live-reload.js`, the browser side of `presenter serve`
(`docs/spec/live-reload.md` §3). Run it after changing the client, the dev server (T21),
the change classification (T20) or the serve-mode templates. T24 automates only the CSS swap
and the reload.

## Setup

### A. Mock server (no Rust needed)

`tests/fixtures/live-reload/mock-server.py` (Python 3 standard library only) serves a reveal
deck built from the vendored assets, with the routes of live-reload.md §1 and the events of
§2. Its `hello` is exactly `{"build_id", "status"}`, as in §2, and its "no good build yet"
page pre-renders the error overlay, as in §1. Control routes fake rebuilds; the docstring
lists them all.

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
| edit a theme-chain stylesheet | `curl --data-binary '.reveal p { color: green }' "$S/__mock/asset?path=extra.css"` and then a css build; the page must link `/__presenter/asset/extra.css` (add the `<link>` with "edit the head" and a content build first) |
| edit slides | `curl -s $S/__mock/slides > s.html`, edit `s.html`, then `curl --data-binary @s.html $S/__mock/slides && curl "$S/__mock/emit?kind=content"` |
| change the title or `<html lang>` | `curl "$S/__mock/title?t=New"` or `curl "$S/__mock/lang?l=de"`, and then a content build |
| edit the head (stylesheets, inline `<style>`) | `curl --data-binary '<style>.reveal h2 { letter-spacing: 7px }</style>' $S/__mock/head` and then a content build |
| edit `body_start` markup | `curl --data-binary '<div class="footer">footer v2</div>' $S/__mock/body` and then a content build |
| edit `body_end` markup (after the client's `<script>`) | `curl --data-binary '<div class="end">footer</div>' $S/__mock/bodyend` and then a content build |
| emit the client's `<script>` before reveal's (a child theme's `scripts` block) | `curl "$S/__mock/early?on=1"` and then a content build |
| a theme script that adds a `<body>` child at load (item 28) | `curl --data-binary '<script>document.addEventListener("ready", () => document.body.append(document.createElement("aside")))</script>' $S/__mock/head` and then a content build (the open page reloads once, item 19); reveal's `ready` event bubbles to `document`, so this listener runs when a `Reveal.on('ready', …)` registered before `Reveal.initialize` (a `theme.toml [scripts]` entry) does; use `"DOMContentLoaded"` for the other case |
| parse error | `curl "$S/__mock/emit?kind=error"` (by default `slides.md:41: unknown directive 'include'`; override with `&message=…&file=…&line=…`) |
| "no good build yet" page | `curl "$S/__mock/emit?kind=error&nodoc=1"` and then reload the page |
| `hello` with the error details (proposed §2 addition) | `curl "$S/__mock/hello-error?on=1"` |
| `GET /` fails, the event stream stays up | `curl "$S/__mock/failroot?on=1"` (`on=0` to undo) |
| server unreachable | `curl "$S/__mock/down?ms=3000"`, or stop the process with Ctrl-C |
| server restart | `curl $S/__mock/restart` (build id 1, current files), or start the process again |
| speaker-view frames | open `$S/__mock/frames` (the two deck iframes of reveal's speaker view) |
| scroll view | open `$S/?view=scroll#/3` |
| start over | `curl $S/__mock/reset` |

`curl $S/__mock/log` returns and clears the list of requests the page made, including
`Sec-Fetch-Dest`, so you can see exactly what the client fetched.

Useful console expressions: `Reveal.getIndices()`, `__presenterLiveReload.state`
(`buildId`: the build this page shows; `connected`; `retryMs`: the next retry delay;
`lastStatus`: `ok`, `error` or `stale` (a content update is postponed); `errorId`: the build
whose error the overlay shows or the user dismissed),
`document.getElementById('presenter-theme').getAttribute('href')` and `__qaMarker`.

### Scripted run

`tests/fixtures/live-reload/qa.mjs` runs items 1–27 of the checklist against the mock in
headless Chrome (over the DevTools protocol) and prints one PASS/FAIL line per scenario; the
item numbers are in brackets. It needs Node ≥ 22, `python3` and Chrome or Chromium, and
takes about two and a half minutes:

```sh
node tests/fixtures/live-reload/qa.mjs [--only ABCDEGHJRSF] [--chrome PATH] [--out DIR]
```

Item 28 is not scripted yet: check it by hand, with the item-28 row of the table above
(run it once with `"ready"` and once with `"DOMContentLoaded"`, with a slide edit, a
reconnect and a `body_start` edit each), or with `presenter serve` as described in B. The
fourth run below scripted it in sessions K, L, M and N of a copy of `qa.mjs` that is not in
the repository; porting those sessions into `qa.mjs` is a follow-up for T24.

### B. `presenter serve` (T23)

Run `cargo run -- serve examples/basic/slides.md` and repeat the checklist by editing the
real files: the theme's CSS for items 1 and 2; `slides.md` for items 3–8, 11–16 and 21–23;
the theme's `deck.html` and `theme.toml` for items 18, 19 and 27 (for item 27, put markup
in the `body_end` block, and override `scripts` in a child theme to call `live_reload()`
first). For items 9, 10 and 17, stop the server process, edit a file and start it again.
Open the speaker view with `S` for item 20, and the page with `?view=scroll` for item 25.
For item 28, add a `theme.toml [scripts]` entry whose script calls
`Reveal.on('ready', () => document.body.append(…))` or appends on `DOMContentLoaded`.
Item 24 needs the mock, which can fail `GET /` on purpose.

## Checklist

| # | step | expected |
|---|---|---|
| 1 | Edit the theme CSS while on a slide | no reload (the marker survives); same slide; `#presenter-theme` href is `/__presenter/theme.css?v=<build_id>`; each old `<link>` is removed only after its replacement has loaded (no flash of unstyled content) |
| 2 | Edit a theme-chain stylesheet (`style("extra.css")`, `@<theme>/…` path) | its `<link>` gets `?v=<build_id>` and the new rules apply; vendored stylesheets (`/__presenter/asset/vendor/…`) are not requested again |
| 3 | Edit the text of the current slide | in-place swap with no reload; same slide and fragment; the client requests only `GET /`, `theme.css?v=<id>` and the non-vendor `/__presenter/asset/` stylesheets |
| 4 | Add a slide after the current one | the new slide exists and can be reached; the current slide is unchanged |
| 5 | Add a slide before the current one | if the current slide has an id, it is still shown (the URL hash is unchanged); without an id, the position is kept, as on a full reload with `#/h/v` |
| 6 | Remove the current slide (last horizontal; bottom of a vertical stack) | no reload; indices are clamped to the new counts |
| 7 | Introduce a parse error | a fixed overlay at the top right (≤ 50 % width, monospace, z-index 10000) shows `build error (<kind>)`, the message, `file:line` and `(click to dismiss)`; the deck stays visible; clicking hides the overlay and the next error shows it again |
| 8 | Fix the error | the overlay disappears on the next good build (css or content); the first good build after an error also refreshes the slides |
| 9 | Stop the server | the deck stays usable; the client retries after 0.5, 1, 2, 4 and then every 5 s (`connection lost, retrying in … ms`); no reload |
| 10 | Restart the server | if `hello` carries a different `build_id`: exactly one full reload, on the same slide (URL hash); if it carries the same id: the slides and styles are refreshed in place, without a reload |
| 11 | Edit a slide with a stepped code block (`data-line-numbers="1\|2"`) while on a step | after the swap, highlighting, line numbers, `data-trim` de-indentation and the current step are the same as after a page load |
| 12 | Edit a math slide | KaTeX is rendered in the swapped slides |
| 13 | Change the deck title | `document.title` is updated without a reload |
| 14 | Swap while in overview mode and paused | both states are kept |
| 15 | Add a `data-visibility="hidden"` slide | it stays hidden after the swap, as on load |
| 16 | Burst of builds (content/css alternating) | applied in order; the final state matches the last build |
| 17 | Builds that happen while disconnected | one full reload on reconnect |
| 18 | A build whose document has other stylesheets or scripts (e.g. `reveal_theme` changed) | one full reload |
| 19 | A template edit outside the slides: an inline `<style>` in the head, `body_start`/`body_end` markup, `<html lang>` | one full reload, and the change is shown; a later slide edit on such a page is still swapped in place |
| 20 | Speaker view open (`S`) while the server restarts or builds are missed | both previews (current and upcoming slide) reload, not only one |
| 21 | Slides that contain a `<script>` (notebook HTML outputs) | every content build reloads in full, so the scripts run |
| 22 | Open the page while the latest build failed, or before any build succeeded | on the deck, an overlay is shown right away (`details in the terminal`, since §2's `hello` has no error details; with the proposed `hello.error`, the details); on the fallback page, the server's pre-rendered overlay is kept unchanged, also across reconnects; the first good build reloads into the deck |
| 23 | Dismiss the error overlay, then lose and regain the connection while the same build still fails | the overlay stays dismissed and unchanged |
| 24 | `GET /` fails during a content build, then a css build arrives | the content update is postponed (build id not stored), and the css build also applies the slides |
| 25 | reveal's scroll view (`view: "scroll"`, or a viewport narrower than reveal's `scrollActivationWidth`) | a content build reloads in full |
| 26 | Console during the whole run | no uncaught exceptions; every client message starts with `[presenter]` |
| 27 | Markup in `body_end` (parsed after the client's `<script>`), or the client's `<script>` emitted before reveal's (a child theme overriding the `scripts` block) | a slide edit and a reconnect are still swapped in place, with no reload; editing the `body_end` markup or moving the script reloads once (item 19) |
| 28 | A theme script adds a `<body>` child at load: in a reveal `ready` handler (which runs before `DOMContentLoaded`), or in a `DOMContentLoaded` handler registered before the client's | a slide edit and a reconnect are still swapped in place, with no reload, and the added element stays; a template edit outside the slides (head, `body_start`, `body_end`) still reloads once (item 19) |

## Results

### 2026-10-08: mock server, fourth run (W3FIX: baseline as served)

Environment as in the third run (macOS, Google Chrome 154.0.8037.98 headless over the
DevTools protocol, Node 26.10.0, Python 3.14.8). `live-reload.js` was 7938 bytes, sha256
`1cebf58521d507d7…`. The client now takes its baseline from the document as served (a
`GET /` when it starts) instead of the page as parsed at `DOMContentLoaded`; see the design
notes. Defect fixed: with the 7907-byte client of the third run, a theme script that added
a `<body>` child in a reveal `ready` handler or in an earlier `DOMContentLoaded` handler
made every content build and every reconnect reload in full (`full reload: page changed
outside the slides`), because the baseline contained the added element and no served
document does.

Method:
- `node tests/fixtures/live-reload/qa.mjs` (all sessions, 2 min 21 s): 45 of 45 passed, so
  every row of the third run's table below holds for this client too.
- Item 28 and the new baseline rule, with a scratch copy of `qa.mjs` (not in the
  repository, see "Scripted run") that added four sessions to the same mock (set up through
  the existing `/__mock/head`, `/__mock/body` and `/__mock/bodyend` routes, with scripts
  like the one in the setup table above):
  - K, a head `<script>` whose `DOMContentLoaded` listener appends a `<div>` to `<body>`
    (registered before the client's): a slide edit (K1) and a reconnect with the same build
    id (K2) were swapped in place; the marker survived, the indices stayed `{h:1,v:1}`, the
    only requests were `GET /` and `theme.css?v=3`.
  - L, a `ready` listener that appends a `<div>`, plus a `body_end` footer: the page recorded
    the order `head`, `ready` handler, `DOMContentLoaded`. A slide edit (L1), a reconnect (L2)
    and a second slide edit (L3) were all in place.
  - M, the same `ready` listener: changing `body_start` markup (M1), `body_end` markup (M3) and
    the head (an added `<style>`, M4) each reloaded exactly once and showed the change, with
    the script-added element present again; a slide edit after the reload (M2) was in place.
  - N, a race: a head script that, once, changes `body_start` and emits a build with
    synchronous requests while the page (build 2) is being parsed, so the client's `GET /`
    returns build 3. The next content build reloaded once (the baseline was empty because
    `#presenter-theme` said `?v=2` and the fetched document `?v=3`) and showed the raced
    footer; a slide edit after that was in place.
- Check of the checks: K1, K2, L1, L2 and L3 failed against the 7907-byte client (each one
  reloaded). N1 failed against this client with the `?v=` comparison removed (the raced
  footer was never shown).
- The coordinator's CDP helper (`cdp.mjs`) loaded the mock with both a `ready` and a
  `DOMContentLoaded` script and a `body_end` footer: both added elements were present, a slide
  edit was swapped in place (marker kept, `{h:1,v:1}`, log `slides updated, build 3`), and so
  was a reconnect after `/__mock/down?ms=300`. The screenshot showed the edited slide with
  both script-added elements. No console message other than `[presenter] …` appeared.

| # | result | evidence |
|---|---|---|
| 1–27 | pass | `qa.mjs`, 45 of 45 scenarios (third run's table below) |
| 28 | pass | sessions K, L and M above; the CDP helper check |

### 2026-10-08: mock server, third run (T22 review fixes, round 2)

Environment: macOS, Google Chrome 154.0.8037.98 (headless, driven over the DevTools
protocol), Node 26.10.0, Python 3.14.8. `live-reload.js` was 7907 bytes, sha256
`a349bc4b5d193dd8…`. This run replaces the earlier ones, which missed a defect:
- The first run (7988-byte client) used a mock that sent a non-spec `hello.error`, so
  item 22 was tested only with that. It also did not cover items 2, 5, 19, 20 and 23–25.
- The second run (7846-byte client, 40 of 40 passed) tested item 19 only with
  `body_start` markup, which the mock put before `.reveal`. That client took its baseline
  of the page when its script ran. At that point, deck.html's `body_end` markup, which comes
  after the script, was not parsed yet. So with any `body_end` markup, every slide edit and
  every reconnect reloaded in full. The mock now has a `body_end` placeholder after the
  client's `<script>`, and it can emit the script before reveal's. Item 27 covers both cases.

Method: `node tests/fixtures/live-reload/qa.mjs` (45 scenarios in 11 browser sessions,
2 min 21 s). The page was opened at the slide given below. Builds were triggered through the
mock's control routes, and the assertions ran in the page. `window.__qaMarker` detected full
reloads, and `performance.getEntriesByType('navigation')` told reloads from first loads.
`/__mock/log` recorded the requests. Every item passed (45 of 45). Separately, the
coordinator's CDP helper loaded the mock with a `body_end` footer: the error overlay looked
right on a screenshot, and the good content build after it was swapped in place (the marker
survived, the indices stayed `{h:1,v:1}`). As a check of the checks, session J (item 27) ran against the
previous 7846-byte client. It failed J1, J2 and J5: each slide edit and reconnect logged
`full reload: page changed outside the slides`. It passed J3 and J4, which expect a reload.
In the second run, the same comparison against the 7988-byte client failed 19 of 39
scenarios, among them every scenario added for a review finding.

| # | result | evidence |
|---|---|---|
| 1 | pass | Opened at `#/1/1`. After the CSS build, the indices were still `{h:1,v:1}` and the marker was kept; `#presenter-theme` was `/__presenter/theme.css?v=3` and the new color applied. A MutationObserver showed that every replacement link already had a loaded sheet when its old link was removed (3 of 3), and exactly one `#presenter-theme` remained. |
| 2 | pass | The document linked `/__presenter/asset/extra.css` and `/__presenter/asset/@corporate/styles/hl.css`. A css build requested exactly `theme.css?v=3`, `extra.css?v=3` and `@corporate/styles/hl.css?v=3`, and the new rules of all three applied. `white.css` and the other vendored stylesheets were not requested and kept their href. |
| 3 | pass | After the content build, the indices were still `{h:1,v:1}` and the same section had `present`. The requests were `GET /` (`fetch`), `theme.css?v=4`, `extra.css?v=4` and `@corporate/styles/hl.css?v=4`. |
| 4 | pass | 6 → 7 slides; the indices were unchanged; `Reveal.slide(5)` reached the new slide. |
| 5 | pass | On `#/math` (h=3), a slide inserted before `#intro`: the page showed `#math` at h=4, and the hash was still `#/math` 1.2 s later. Without an id on the current slide (h=3), the same insert kept h=3, which now holds the slide that came before it (`#code`), as a reload of `#/3` would show. |
| 6 | pass | From the last slide (h=5), removing it gave h=4 (the new last slide), and the hash became `#/last`. From the bottom of a vertical stack (1/1), removing it gave `{h:1,v:0}`. |
| 7 | pass | The overlay was `position: fixed`, `z-index: 10000`, monospace, 351 px wide at a 1280 px viewport, 13 px from the top right. Its text was `build error (parse)` / `slides.md:41: unknown directive 'include'` / `slides.md:41` / `(click to dismiss)`. The deck stayed visible (screenshot checked) and the error triggered no requests. A click hid the overlay, and the next error (`theme.scss:3`) showed it again with the new text. |
| 8 | pass | A css build after the error hid the overlay and refreshed the slides (an edit made meanwhile appeared) and the theme stylesheet, with no vendored request. An error followed by a content build also hid the overlay and showed the new text. |
| 9 | pass | With the mock answering 503 for 2.5 s, the client retried after 500, 1000 and 2000 ms, reconnected at 3.5 s, reset the delay to 500 ms, did not reload, refreshed in place (same build id) and applied the next edit. With the server process killed for 16 s, it retried after 500, 1000, 2000, 4000, 5000 and 5000 ms, and the deck (marker) stayed until the restart. |
| 10 | pass | A mock restart (build id 2 → 1) and a real process restart (4 → 1) each caused exactly one reload; the slide came back from the hash (`#/math`, `#/stack-bottom`) and no second reload followed. A restart that kept the id (1 → 1) with a changed deck: the edit appeared after the reconnect through `GET /`, the marker survived and the indices were kept. |
| 11 | pass | On step 0 of the code slide (`{h:2,v:0,f:0}`), the code block was identical before and after the swap: two `code` elements, `.code-wrapper`, `hljs`, the line-number tables, the first line `fn main() {` with its indentation trimmed, the step clone still `visible current-fragment`, and the same `innerHTML` length. The indices were kept. |
| 12 | pass | The swapped math slide had one `.katex`. |
| 13 | pass | `document.title` became `New title` with no reload. |
| 14 | pass | `Reveal.isOverview()` and `Reveal.isPaused()` were both still true after the swap. |
| 15 | pass | The hidden slide was not in the DOM after the swap. |
| 16 | pass | After 5 builds, the last edit was shown, the stored build id was the last one, and the only `#presenter-theme` pointed at `?v=<last>`. |
| 17 | pass | The page reloaded once (`build 1 -> 2`) and showed the edit made while it was disconnected, on `#/math`. |
| 18 | pass | An extra reveal theme stylesheet in the new document caused one reload, and the new theme was applied. |
| 19 | pass | Each of these caused exactly one reload and was then shown: an inline `<style>` added to the head (`letter-spacing: 7px` applied), a `body_start` footer added, its text changed (`footer v1` → `footer v2`), the `body_end` markup changed (`footer v2` shown, hash `#/stack-bottom` kept), the client's `<script>` moved before reveal's, and `<html lang>` changed to `de`. A slide edit afterwards, on the page with the head `<style>`, the highlight plugin's own `<style>` and the footer, was still swapped in place. |
| 20 | pass | `/__mock/frames` (the speaker view's two iframes, whose URLs differ in their query) stored the build id under two keys. A build made while both were disconnected reloaded both frames, and both showed the new slide (7 slides, build 2). |
| 21 | pass | Adding a slide script caused a reload, and the script ran. A later text edit, with the script unchanged, reloaded again. |
| 22 | pass | Deck opened while build 2 had failed, with the §2 `hello`: the overlay read `build error` / `details in the terminal` / `(click to dismiss)` over a working deck. With `hello.error` turned on, a reload showed `build error (parse)` / `boom` / `slides.md:7`. On the fallback page, the pre-rendered `build error (parse)` / `first` / `slides.md:41` stayed unchanged, with no second overlay, also after a reconnect. The first good build reloaded into the deck (`cannot update in place`). |
| 23 | pass | After a click hid the overlay, a reconnect (`hello` with the same failing build) left it hidden with the same text, and the client made no request besides the event stream. |
| 24 | pass | With `GET /` answering 503, a content build logged `GET / failed, update postponed`; the build id stayed 1 and the state became `stale`. The next css build fetched `/` too and showed the edit, and stored its id; a later reconnect changed nothing. |
| 25 | pass | Opened with `?view=scroll`: a content build reloaded once (`cannot update in place`), and the edit was shown. |
| 26 | pass | No exceptions occurred in any session, and every client message was `[presenter] …`. The only other console lines were Chrome's own `Failed to load resource` errors for the deliberate failures: `/__presenter/events` during the outages (503 and `ERR_CONNECTION_REFUSED`) and `GET /` in item 24 (503). |
| 27 | pass | Page opened at `#/1/1` with a `body_end` footer after the client's `<script>` (the last child of `<body>`). A slide edit was swapped in place: the marker survived, the edited slide was a new element, the indices stayed `{h:1,v:1}`, and the only requests were `GET /` and `theme.css?v=3`. A reconnect (`hello` with the same build id 3) refreshed in place too: the requests were the event stream, `GET /` and `theme.css?v=3`, and the log read `slides updated, build 3`. After the client's `<script>` was moved before `reveal.js` (one reload, item 19), a slide edit and a reconnect were again both in place (build 6). |

Still to do: repeat this checklist against `presenter serve` once T23 has landed.

## Design notes (behaviour beyond the §3 pseudo-code)

- **Slides swap.** The children of `.reveal .slides` are swapped, not the element itself:
  reveal caches that element at init, attaches its `click`/`transitionend` listeners to it
  and sets its layout styles on it. Hidden slides are removed (`Reveal.removeHiddenSlides()`),
  math is rendered and the highlight plugin's own on-load pass is run again
  (`Reveal.getPlugin('highlight').init(Reveal)`, which does `data-trim`, escaping and
  `highlightBlock` for every `pre code`; after a swap they are all new) *before*
  `Reveal.sync()`/`setState()`, so that reveal sees the step fragments `highlightBlock`
  creates and can restore the fragment index.
- **Where the swap lands.** On the slide that has the old current slide's id, if one exists
  (that is where a reload's `#/<id>` hash lands); otherwise on the old indices clamped to the
  new counts (where a reload's `#/h/v` lands). reveal's own clamping is not used, because
  with `loop: true` it wraps an index past the end to the first slide.
- **Stylesheets.** Every applied build re-requests `#presenter-theme` and each
  `<link rel=stylesheet>` under `/__presenter/asset/` outside `vendor/`: the theme chain's
  files and `@<theme>/…` paths (theme-format.md §4) can change, vendored files cannot.
  Content builds re-request them too, because `theme.toml` and `presenter.toml` edits, and
  batches that mix CSS and other files, are content builds.
- **When the client reloads instead of swapping.** It compares the new document with the one
  shown, outside the slides: the elements of `<head>` (except `<title>`, which is updated in
  place, and `<meta>`) and the children of `<body>` other than `.reveal`, as HTML with
  `?v=<id>` removed, plus `<html lang>`. Both sides are documents as the server sent them,
  never the live DOM: scripts change the live page at load (the highlight plugin appends a
  `<style>` to `<head>`, a theme script may append to `<body>` in a reveal `ready` handler,
  which runs before `DOMContentLoaded`, or in any `DOMContentLoaded` handler), and markup can
  follow the client's script (deck.html's `body_end` block). So when the client starts, it
  fetches `GET /` once and uses that document as the baseline if its `#presenter-theme` href
  (`?v=<build id>`) equals the page's, i.e. if it is the same build. If a build landed in
  between, or the request failed, there is no baseline and the next content build reloads in
  full. After a swap, the swapped-in document is the baseline. Builds wait for this request.
  Any difference, slides that contain a `<script>`, a page without a reveal deck (T21's
  fallback page), and reveal's scroll and print views all lead to a full reload. reveal's
  `hash: true` (the presenter default) then restores the slide.
- **After an error or a postponed update.** A content update is postponed when `GET /`
  fails (`lastStatus` becomes `stale`, the build id is not stored). After an error build or
  a postponed update, the next good build of any kind is applied as content and css, since
  it may hold any change made in between.
- **Build ids.** The applied build id is kept in memory, and in `sessionStorage` under a key
  per frame (`presenter:build_id:` + path + query), because reveal's speaker view shows the
  deck in two frames of one tab. It is stored once an update has been applied, and before
  any reload. On a reconnect, `hello` is compared with the in-memory id; on a page's first
  `hello`, with the stored one, so a reloaded page does not reload again. A reconnect whose
  `hello` has the same id refreshes the slides and styles in place, since a restarted server
  numbers its builds from 1 again.
- **Errors in `hello`.** §2's `hello` has no error details. When it reports an error, the
  client shows `details in the terminal`, keeps an overlay the server pre-rendered (T21's
  fallback page) unchanged, and uses `hello.error` when a server sends it (proposed addition
  to §2, same shape as in the `build` event). An overlay that is shown or was dismissed for a
  build is left alone when a reconnect reports the same build.

## Known limitations

- Inserting a slide before the current one in a deck whose slides have no ids keeps the
  position, not the slide (as a full reload would). Give slides ids (`{#id}`) to avoid this.
- Editing a theme script file (`theme.toml [scripts]`) does not run the new code: its URL is
  unchanged, so the client swaps the slides only. Reload by hand. A serve-mode `script()`
  URL that carried a content hash would make this a full reload automatically.
- Attributes of `<body>` and `.reveal`, and markup inside `.reveal` but outside `.slides`,
  are not compared (reveal changes them at runtime), so template edits there need a manual
  reload.
- Elements that scripts add to the page are never compared, so they never cause a reload;
  they stay as they are across swaps (only the children of `.slides` are replaced).
- The client fetches `GET /` once per page load (per frame in the speaker view) for its
  baseline. If a build finishes between the page load and that request, the next content
  build reloads in full even when only the slides changed.
- A build that finishes after the page was served but before the event stream connects is
  not shown until the next build: on a page's first connection, `hello` is taken as the
  build the page shows. Comparing it with the `?v=<build id>` of the page's
  `#presenter-theme` href would catch this. Seen in the race scenario of the fourth run.
- In reveal's scroll view (also active automatically below `scrollActivationWidth`, 435 px
  by default) and print view, every content build reloads in full.
- After any lost connection, the client fetches `/` once and swaps the slides, even if
  nothing changed: the protocol has no server instance id to tell a restart from a network
  hiccup.
- For up to 1 s after a swap that moved the slide, the URL hash can still name the previous
  slide, because reveal debounces `replaceState`. This matters only if a full reload happens
  within that second.
