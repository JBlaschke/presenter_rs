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
> - Host check: allowed are `localhost`, any IP address (a rebinding attack needs a DNS
>   name; this keeps `--host 0.0.0.0` usable from other machines at the machine's address)
>   and the `--host`/`[serve] host` value, each with the bound port (no port for port 80).
>   Any other `Host` gets `403 Forbidden`, on every route; a request without `Host`
>   (HTTP/1.0) passes. Another name for the machine (`mymac.local`) works only when the
>   server was started with it as `--host`.
> - Content version: the first 8 bytes of the file's BLAKE3 hash, in decimal (digits only,
>   which the client disregards when it compares pages). Files of the embedded `vendor/…`
>   and `presenter/…` trees, and the URLs of `script()` and `style()` (re-requested by the
>   client on every build), are not versioned. A query the `url()` has itself follows:
>   `…?v=<n>&x=1`.
> - Sass: a thread with a 256 MiB stack per compilation and a 10 s limit. grass offers no
>   recursion limit and cannot be stopped, so (1) a `@mixin` that always includes itself or a
>   `@function` that always calls itself before any `@return` (directly or through others of
>   the same `.scss` file) is reported as an `Error::Style` before grass runs; recursion that
>   only a condition guards, or that goes through several files or the indented `.sass`
>   syntax, still overflows the stack if it never ends; (2) a compilation that times out
>   keeps running in the background (a CPU core, and memory if it emits CSS) until it ends by
>   itself, i.e. for an endless loop until presenter exits: the error says so. While it runs,
>   compiling the same files again fails at once instead of starting another one. The serve
>   runtime's threads, where the pipeline runs, have the 8 MiB stack of `presenter build`'s
>   main thread.

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
the source file; `presenter.toml` if found; every theme directory in the chain
(recursive); the parent directories of all file assets (non-recursive, per directory);
`assets/themes/default/` when running a debug build (so working on the built-in theme
live-reloads too). Debounce 150 ms (implemented by T20 directly on `notify` with its own coalescing thread: a batch goes out after 150 ms without a relevant event, capped at 2 s; `notify-debouncer-full` dropped events for rename-replace editors). Editors that write via rename
(vim, VS Code "safe write") produce `Remove`+`Create`; treat any event kind as a change and
re-add watches for paths that were recreated.

> **Note (A2, 2026-10-09; audit F18):** below the recursively watched theme directories only
> `.git/` and editor files (backups, swap, lock and auto-save files, `.DS_Store`) are
> ignored. Directories named `dist/` or `target/` count like any other: a theme may keep
> inputs there (a bundled script entry, a vendored library's stylesheet).
