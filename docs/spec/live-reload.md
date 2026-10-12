# Spec: Live reload protocol (`presenter serve`)

**Status:** contract between T20 (watcher), T21 (server), T22 (client), T23 (CLI).

## 1. Endpoints (axum 0.8)

| route | response |
|---|---|
| `GET /` | the latest successfully rendered document (`text/html; charset=utf-8`), serve-mode variant (helpers emit URLs, `live_reload()` emits the client script tag). If no build has succeeded yet, a minimal page with the error overlay pre-populated. |
| `GET /__presenter/theme.css` | the compiled theme stylesheet of the latest build. `Cache-Control: no-store`. |
| `GET /__presenter/asset/{*path}` | vendor (`vendor/…`), presenter (`presenter/…`) or theme-chain assets, resolved exactly like the `asset()` helper. `Cache-Control: no-store` for theme assets, `max-age=3600` for vendor. |
| `GET /__presenter/deck-asset/{id}` | a registered deck asset (image, notebook output) by id. |
| `GET /__presenter/events` | `text/event-stream` (SSE). Sends `hello` immediately, then one `build` event per rebuild. Keep-alive comment every 15 s (`axum::response::sse::KeepAlive`). |
| `GET /__presenter/live-reload.js` | the client script (`assets/presenter/live-reload.js`). |
| `GET /__presenter/state` | JSON `{ "build_id": n, "status": "ok"|"error", "error": {...}|null }` for tests and debugging. |

In serve mode the renderer uses URL-emitting helpers (theme-format.md §4, "serve" column),
and deck asset placeholders `asset:<id>` are rewritten to `/__presenter/deck-asset/<id>`.

> **Note (A2, 2026-10-09; audit F03, F07, F11, F33):** every route answers `403` to a
> request for another host (§4). `GET /__presenter/asset/{*path}` serves a stylesheet of the
> theme chain (one a template links with `style()`) with its relative `url()`s rewritten to
> the absolute, versioned URLs of the files they name (`/__presenter/asset/<path>?v=<n>`, §4);
> a reference that names nothing stays as written, and vendor stylesheets are served as they
> are. The `error` object of `/__presenter/state` and the events locates a front matter error
> in the deck source (`file`, `line`, a `slides.md:2: ` prefix, `excerpt`) and a
> `presenter.toml` error in that file (`presenter.toml:23: <what is wrong>` without the
> source lines `toml` quotes; the offending line is the `excerpt`).

## 2. Events

```
event: hello
data: {"build_id": 17, "status": "ok"}

event: build
data: {"build_id": 18, "status": "ok", "kind": "css", "duration_ms": 42}

event: build
data: {"build_id": 19, "status": "ok", "kind": "content", "duration_ms": 180}

event: build
data: {"build_id": 20, "status": "error", "error": {"message": "slides.md:41: unknown directive 'include'", "file": "slides.md", "line": 41, "kind": "parse"}}
```

When the latest build failed, `hello` also carries the `error` object
(`{"build_id": 20, "status": "error", "error": {...}}`) so a freshly opened page can show the
overlay (addition, 2026-10-08, from T22).

`kind` classification (T20): if **every** changed path is a stylesheet (`.css`, `.scss`,
`.sass`) or lives under a theme's `styles/` or `assets/` directory → `css`; otherwise →
`content`. A change to `theme.toml`, `presenter.toml`, any template, the source file, or a
referenced image → `content`.

`build_id` increases by one per attempt (including failed ones). The server keeps only the
latest successful document and the latest status.

> **Note (A2, 2026-10-09; audit F03):** a batch classified `css` whose build changed a
> versioned theme-file URL in the document itself (`/__presenter/asset/<path>?v=<n>` from
> `asset()`, e.g. the title slide's logo) is reported as `content`: only a content swap
> updates what the document loads. The paths' classification above is unchanged.

## 3. Client behaviour (`assets/presenter/live-reload.js`, T22; plain ES2020, no deps)

```
connect EventSource('/__presenter/events')
on hello:   if (stored build_id exists && hello.build_id != stored) → fullReload()
            store build_id
on build ok, kind == "css":
            link = document.getElementById('presenter-theme')
            link.href = '/__presenter/theme.css?v=' + build_id      // browser swaps without flash of unstyled content
            also re-request any <link rel=stylesheet> whose href starts with /__presenter/asset/ and contains "theme"
            Reveal.layout()
on build ok, kind == "content":
            html = await fetch('/').text()
            next = new DOMParser().parseFromString(html, 'text/html')
            newSlides = next.querySelector('.reveal .slides')
            indices = Reveal.getIndices(); state = Reveal.getState()
            document.querySelector('.reveal .slides').replaceWith(newSlides)
            try { Reveal.sync(); Reveal.setState(state) /* falls back to Reveal.slide(h, v, f) */ ; rerunKatex(); rehighlight(); }
            catch (e) { fullReload() }
            if (<title> changed) document.title = …
            hideOverlay()
on build error: showOverlay(error)   // fixed-position panel: message, file:line, dismiss on click; keeps the deck visible
on EventSource error: exponential backoff reconnect (0.5 s → 5 s); on reconnect the hello handler reloads if needed
fullReload(): location.reload()      // reveal `hash: true` restores the slide from the URL fragment
```

- `rerunKatex()`: `assets/presenter/math.js` exposes `window.__presenterRenderMath(root)`;
  call it on the new slides when `window.katex` exists.
- `rehighlight()`: reveal's highlight plugin highlights on init; after a DOM swap call
  `Reveal.getPlugin('highlight').highlightBlock(el)` for each `pre code` in the new slides
  (guard with `if (Reveal.getPlugin)`).
- If the number of top-level `<section>`s changed, `Reveal.sync()` still works in 6.x
  (it now updates slide classes); keep the current indices clamped to the new count.
- Never throw out of an event handler; log to `console.debug` with prefix `[presenter]`.

> **Note (A2, 2026-10-09; audit F03):** after an in-place content swap the client also
> takes the new `?v=` URLs of theme files the page loads outside the slides (`src`, and
> `href` of a `<link>` that is not a stylesheet, below `/__presenter/asset/`; e.g. an
> `asset()` logo in `deck.html`): the page signatures matched, so these elements correspond
> in order. Scripts are left alone (they would not run again).

## 4. Server lifecycle (T21/T23)
- Requests whose `Host` header is not the address the server listens on, `localhost`, `127.0.0.1`
  or `[::1]` (with the bound port) are rejected with 421/403, so a web page cannot read the deck
  or theme files through DNS rebinding (audit F07). In serve mode, URLs of theme files referenced
  from CSS or `asset()` carry a content version (`?v=…`) like deck assets, so an edited theme image
  shows up after a css/content swap (audit F03).
- Theme stylesheet compilation (SCSS) runs on a dedicated thread with a large stack and a time
  limit; a runaway or deeply recursive stylesheet becomes an `Error::Style`, never a crash or a
  wedged server (audit F02/F06).

> **Note (A2, 2026-10-09), how the decisions above are implemented:**
> - Host check: allowed are the address the server listens on, `localhost`, `127.0.0.1`,
>   `[::1]` and the `--host`/`[serve] host` value, each with the bound port (no port for
>   port 80). A server listening on every address (`--host 0.0.0.0` or `::`) listens on
>   each of the machine's addresses, so any IP address is allowed then (other devices
>   reach it at the machine's LAN address; a rebinding attack needs a DNS name). An
>   IPv4-mapped IPv6 address counts as the IPv4 address. Any other `Host` gets
>   `403 Forbidden`, on every route; a request without `Host` (HTTP/1.0) passes. Another
>   name for the machine (`mymac.local`) works only when the server was started with it
>   as `--host`.
> - Content version: the first 8 bytes of the file's BLAKE3 hash, in decimal (digits only,
>   which the client disregards when it compares pages). For a file on disk it is
>   remembered while the file's size and modification time stay the same, so a rebuild
>   stats the theme files it names instead of reading them (an edit that keeps both keeps
>   the old URL, as for deck assets). Files of the embedded `vendor/…` and `presenter/…`
>   trees, and the URLs of `script()` and `style()` (re-requested by the client on every
>   build), are not versioned. A query the `url()` has itself follows: `…?v=<n>&x=1`.
> - Sass: `presenter` compiles each Sass file in a worker process, itself started with a
>   hidden first argument, on a thread with a 256 MiB stack, and waits at most 10 s for
>   it. A compilation that takes longer is an `Error::Style`, and its worker is killed,
>   which frees the CPU and memory it used (an endless loop that emits CSS grows the
>   worker's memory until then). A recursion that never ends, whatever guards it, in one
>   file or several, overflows the worker's stack and ends only the worker: an
>   `Error::Style` that names the `@mixin` or `@function` (file, line of the call, the line
>   quoted) when the SCSS files read show it, else says the stack ran out. After a timeout
>   or a stack overflow, compiling the same entry with the files it read unchanged fails at
>   once with the same error. The serve runtime's threads, where the pipeline runs, have
>   the 8 MiB stack of `presenter build`'s main thread.
>
> **Clarification (W8FIX, 2026-10-09):** "names the `@mixin` or `@function`" holds when the
> files read show one recursion that always recurses, or only one that a condition guards.
> When they show several guarded ones (a correct helper among them, such as a `str-replace`
> that recurses inside an `@if`), any of them may be the one that never ends, so none is
> blamed at a line: the error is the entry's and lists each with its file and the line of
> its call (`… any of which may be it: @function str-replace (@t/styles/_lib.scss:4),
> @function spacing (@t/styles/_spacing.scss:3)`).

- State: `Arc<RwLock<ServeState { build_id, document: Option<String>, theme_css: String,
  assets: AssetRegistry, status: Status }>>` plus a `tokio::sync::broadcast::Sender<BuildEvent>`
  (capacity 64) that the SSE handler subscribes to.
- The watcher runs on a dedicated std thread (notify callbacks) and sends `Vec<PathBuf>`
  change batches over an `std::sync::mpsc`/`tokio::sync::mpsc` channel to the rebuild task,
  which runs the pipeline on `spawn_blocking`, updates state, and broadcasts.
- First build happens before binding the port so `GET /` is never empty in tests.
- Bind failure on the requested port → error with the hint to use `--port 0`.

## 5. Watch set (T20)
Computed from each successful build (and from the failed attempt's inputs when parsing failed):
the source file and every file in `Deck.includes` (markdown-dialect §4.1); `presenter.toml` if found; every theme directory in the chain
(recursive); the parent directories of all file assets (non-recursive, per directory);
`assets/themes/default/` when running a debug build (so working on the built-in theme
live-reloads too). Debounce 150 ms (implemented by T20 directly on `notify` with its own coalescing thread: a batch goes out after 150 ms without a relevant event, capped at 2 s; `notify-debouncer-full` dropped events for rename-replace editors). Editors that write via rename
(vim, VS Code "safe write") produce `Remove`+`Create`; treat any event kind as a change and
re-add watches for paths that were recreated.

> **Note (2026-10-11):** one exception to "any event kind is a change", on macOS only: an
> event for a path whose inode change time lies before the last batch was sent (at first,
> before the watcher started) is dropped, a directory's creation included. FSEvents reports
> changes made shortly before its stream was created, and it creates a stream on every
> watch, so the files written to set a project up, or the change behind the last rebuild,
> came back as a second batch and a spurious rebuild (seen as the `content` build after
> `hello` that `tests/cli_serve.rs` did not expect, on loaded macOS runners). The build that
> the last batch started read the path after that change. Whole-second change times never
> qualify. Not on Linux: its file times come from a coarse clock up to a tick behind the
> system clock, so a change made right after a batch looked older than the batch and was
> lost; inotify and Windows report nothing from before a watch anyway.

> **Note (A2, 2026-10-09; audit F18):** below the recursively watched theme directories only
> `.git/` and editor files (backups, swap, lock and auto-save files, `.DS_Store`) are
> ignored. Directories named `dist/` or `target/` count like any other: a theme may keep
> inputs there (a bundled script entry, a vendored library's stylesheet).
