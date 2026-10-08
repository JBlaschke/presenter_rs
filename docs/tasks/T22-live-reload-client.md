# T22 — Live-reload client script

| | |
|---|---|
| Milestone | M2 |
| Size | M |
| Depends on | nothing to compile against; protocol in `docs/spec/live-reload.md` §2–§3 |
| Parallel-safe with | T20, T21, T24 |
| Files you own | `assets/presenter/live-reload.js`, `tests/fixtures/live-reload/mock-server.py` (optional manual harness), `docs/dev/live-reload-qa.md` |

## Goal
A dependency-free browser script that keeps the open presentation in sync with the server:
swap CSS without reload, swap slide content in place while keeping the current slide, show
build errors in an overlay, and fall back to a full reload when in doubt.

## Read first
- `docs/spec/live-reload.md` §3 (this is your spec; implement it line by line)
- reveal.js API: `Reveal.getIndices()`, `Reveal.slide(h, v, f)`, `Reveal.sync()`, `Reveal.getState()/setState()`, `Reveal.layout()`, `Reveal.getPlugin('highlight')`, `Reveal.isReady()`

## Steps
1. Write `live-reload.js` as an IIFE, ES2020, strict mode, no globals except `window.__presenterLiveReload`
   (object with `connect()`, `disconnect()`, `state` for debugging).
2. Overlay: create lazily a `<div id="presenter-overlay">` with inline styles (fixed, top-right,
   max 50 % width, monospace, dark background, high z-index 10000), content: kind, message,
   `file:line`, and a "dismiss" hint; clicking hides it; it reappears on the next error.
3. Content swap algorithm per spec: fetch `/`, parse with `DOMParser`, replace `.reveal .slides`,
   `Reveal.sync()`, restore state (`setState`, fall back to `slide(h, v, f)` clamped to counts),
   call `window.__presenterRenderMath(slides)` if present, re-highlight `pre code` via the
   highlight plugin's `highlightBlock` if available, update `document.title`.
   Wrap everything in try/catch → `location.reload()` on failure.
4. Stylesheet swap: update `#presenter-theme` href with `?v=<build_id>`; create a new `<link>`,
   wait for `load`, then remove the old one (avoids flash). Call `Reveal.layout()` afterwards.
5. Reconnect: on `EventSource` `error`, close and retry with backoff 500 ms → 5 s; on `hello`
   with a different `build_id` than stored (`sessionStorage`), full reload (server restarted or missed events).
6. `docs/dev/live-reload-qa.md`: manual QA checklist (edit CSS → no reload, slide stays; edit
   slide text → content swap, slide stays; add a slide; remove the current slide → clamped;
   introduce a parse error → overlay; fix → overlay gone; stop server → reconnect; restart → reload).
7. Optional: `tests/fixtures/live-reload/mock-server.py` — a 40-line Python `http.server`
   that serves a static reveal page and emits SSE events on demand, so the script can be
   exercised before T21/T23 exist. Document how to run it.

## Acceptance criteria
- `node --check assets/presenter/live-reload.js` passes (syntax) — add this to CI as a tiny step
  guarded by `node` availability (Node 26 is present locally; ubuntu runners have Node).
- Script size < 8 KB unminified; no external requests except the SSE endpoint and `/`.
- Every QA item in `docs/dev/live-reload-qa.md` checked manually against the mock server or
  against T23 once available; results recorded in the PR.
- No exceptions logged in the console during the QA run; all logs prefixed `[presenter]`.

## Out of scope
Server (T21), watcher (T20), end-to-end automation (T24 covers CSS swap + reload).

## Hints (verified)
- reveal 6: `Reveal.sync()` updates slide classes and dispatches a `sync` event — call it after replacing the slides container, not before.
- `Reveal.getState()` returns `{indexh, indexv, indexf, paused, overview}`; `setState` restores; clamp with `Reveal.getHorizontalSlides().length`.
