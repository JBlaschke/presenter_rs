# T21 — Dev server (axum) with SSE

| | |
|---|---|
| Milestone | M2 |
| Size | M |
| Depends on | T16 |
| Parallel-safe with | T20, T22, T24 |
| Files you own | `src/serve/mod.rs`, `src/serve/server.rs`, `src/serve/events.rs`, `tests/server.rs` |

## Goal
Serve the latest build from memory and push build events to connected browsers, exactly
as `docs/spec/live-reload.md` §1–§2 and §4 describe.

## Read first
- `docs/spec/live-reload.md` (all)
- `docs/spec/theme-format.md` §4 "serve" column

## API to implement
```rust
// events.rs
#[derive(Clone, Serialize)] #[serde(tag = "status", rename_all = "lowercase")]
pub enum BuildEvent { Ok { build_id: u64, kind: ChangeKind, duration_ms: u64 }, Error { build_id: u64, error: ErrorPayload } }
#[derive(Clone, Serialize)] pub struct ErrorPayload { pub message: String, pub file: Option<String>, pub line: Option<usize>, pub kind: String }
impl From<&crate::error::Error> for ErrorPayload { … }

// server.rs
pub struct ServeState { pub build_id: u64, pub document: Option<String>, pub theme_css: String,
                        pub assets: AssetRegistry, pub theme: Option<Theme>, pub last_error: Option<ErrorPayload> }
pub type Shared = Arc<RwLock<ServeState>>;
pub fn router(state: Shared, events: tokio::sync::broadcast::Sender<BuildEvent>) -> axum::Router;
pub async fn serve(listener: tokio::net::TcpListener, router: axum::Router) -> Result<()>;   // with graceful shutdown on ctrl-c
pub fn apply_build(state: &Shared, build: &Build) -> u64;        // stores document/css/assets, bumps build_id, returns it
pub fn apply_error(state: &Shared, err: &Error) -> u64;
```

## Steps
1. Routes per live-reload.md §1. `GET /` → `Html(document)` or the fallback page (inline
   minimal HTML that loads `/__presenter/live-reload.js` and shows `last_error`).
2. `/__presenter/events`: `Sse::new(stream)` where the stream first yields `hello` with the
   current `build_id`/status, then maps `BroadcastStream` items to `Event::default().event("build").json_data(ev)`;
   `.keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))`. Lagged receivers
   (`RecvError::Lagged`) just continue.
3. `/__presenter/theme.css` → `text/css`, `Cache-Control: no-store`.
4. `/__presenter/asset/{*path}` → `vendor::get` for `vendor/`/`presenter/` prefixes, else
   `theme.read_asset`; MIME via `mime_guess`; 404 otherwise.
5. `/__presenter/deck-asset/{id}` → `assets.read(id)`; 404 if unknown.
6. `/__presenter/state` → JSON.
7. Serve mode rendering is T15's job (`OutputMode::Serve`); this task only stores and serves.

## Acceptance criteria
- `tests/server.rs`: build `tests/fixtures/md/minimal.md` into a `ServeState`, bind
  `127.0.0.1:0`, spawn `serve`; with `ureq`: `GET /` 200 containing `<div class="slides">`;
  `GET /__presenter/theme.css` 200 `text/css`; `GET /__presenter/asset/vendor/reveal/dist/reveal.js` 200 and > 100 KB;
  `GET /__presenter/asset/vendor/does-not-exist.js` 404; `GET /__presenter/state` JSON with `build_id`.
- SSE: open `/__presenter/events` with a 2 s read timeout, read the first frame → `event: hello`;
  then `apply_build` + broadcast → next frame `event: build` with `"status":"ok"`.
- Graceful shutdown: dropping a `CancellationToken`/oneshot ends `serve` within 1 s (test).

## Out of scope
Watching files (T20), the client script (T22), CLI (T23).

## Hints (verified)
- axum 0.8: path wildcards are `/{*path}` (new syntax), handlers take `Path(String)`;
  `axum::response::sse::{Sse, Event, KeepAlive}`; `tokio_stream::wrappers::BroadcastStream`
  needs the `tokio-stream` crate (`features = ["sync"]`) — add it as a dependency (additive `Cargo.toml` change).
- `axum::serve(listener, router).with_graceful_shutdown(signal)`.
