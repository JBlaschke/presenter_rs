//! axum dev server: document, theme CSS, assets and SSE endpoints
//! (`docs/spec/live-reload.md` §1, §2 and §4).
//!
//! The server keeps the latest build in memory ([`ServeState`], shared as [`Shared`]) and
//! pushes one `build` event per rebuild to every connected browser through a
//! [`broadcast`] channel ([`event_channel`]). The rebuild loop (T20/T23) renders with
//! `OutputMode::Serve` and `BuildInfo::serve(`[`next_build_id`]`(&state))`, stores the result
//! with [`apply_build`] or [`apply_error`] and then [`publish`]es the matching
//! [`BuildEvent`].
//!
//! | route | response |
//! |---|---|
//! | `GET /` | the latest successful document; before any build succeeded, a minimal page that loads the client script and shows the error |
//! | `GET /__presenter/theme.css` | the latest compiled theme stylesheet |
//! | `GET /__presenter/asset/{*path}` | a helper path ([`resolve_helper_path`]: `vendor/…`, `presenter/…`, `@<theme>/…` or the chain's `assets/`) |
//! | `GET /__presenter/deck-asset/{id}` | a deck asset of the latest build ([`AssetRegistry::read`]) |
//! | `GET /__presenter/events` | Server-Sent Events: `hello`, then one `build` per rebuild, keep-alive every 15 s |
//! | `GET /__presenter/live-reload.js` | the client script (`assets/presenter/live-reload.js`) |
//! | `GET /__presenter/state` | `{"build_id": n, "status": "ok"\|"error", "error": {…}\|null}` |
//! | `GET /favicon.ico` | `204 No Content` (keeps the browser console free of a 404) |
//!
//! Everything is `Cache-Control: no-store` except vendored files (`max-age=3600`, they never
//! change within one binary) and the event stream (`no-cache`, axum's SSE default).
//! Unknown paths and unknown assets are `404 Not Found`.

use std::convert::Infallible;
use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::{Stream, StreamExt};

use crate::assets::emit::SERVE_ASSET_PREFIX;
use crate::assets::registry::AssetRegistry;
use crate::error::{Error, Result};
use crate::pipeline::Build;
use crate::render::html::{SERVE_HELPER_PREFIX, SERVE_LIVE_RELOAD_JS, SERVE_THEME_CSS};
use crate::render::{normalize_helper_path, resolve_helper_path};
use crate::serve::events::{BuildEvent, ErrorPayload, Hello, Status};
use crate::theme::Theme;

/// Capacity of the build-event channel (live-reload.md §4). A browser that falls further
/// behind skips the oldest events and keeps receiving the newer ones.
pub const EVENT_CAPACITY: usize = 64;

/// Interval of the SSE keep-alive comment (live-reload.md §1).
const KEEP_ALIVE: Duration = Duration::from_secs(15);

/// How long [`serve_until`] still waits for open connections once the shutdown signal has
/// fired and every event stream has ended.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// `Cache-Control` of everything that can change between builds.
const NO_STORE: &str = "no-store";

/// `Cache-Control` of vendored files (live-reload.md §1).
const VENDOR_CACHE: &str = "max-age=3600";

/// The client script's path in the embedded asset tree.
const LIVE_RELOAD_KEY: &str = "presenter/live-reload.js";

/// What the dev server serves: the latest build and the latest attempt's status.
///
/// The server keeps only the latest **successful** document (with its stylesheet, deck
/// assets and theme) and the latest status: a failed attempt sets `last_error` and leaves
/// the document of the last good build in place (live-reload.md §2).
#[derive(Debug, Clone, Default)]
pub struct ServeState {
    /// The id of the latest build attempt, failed ones included; 0 before the first.
    pub build_id: u64,
    /// The latest successfully rendered document (serve mode); `None` until a build succeeds.
    pub document: Option<String>,
    /// The latest successful build's compiled theme stylesheet (`/__presenter/theme.css`).
    pub theme_css: String,
    /// The latest successful build's deck assets (`/__presenter/deck-asset/{id}`).
    pub assets: AssetRegistry,
    /// The latest successful build's theme chain (`/__presenter/asset/{*path}`).
    pub theme: Option<Theme>,
    /// The latest attempt's error; `None` when it succeeded.
    pub last_error: Option<ErrorPayload>,
}

impl ServeState {
    /// The `hello` event for this state: the latest attempt's id and status, and its error
    /// when it failed.
    pub fn hello(&self) -> Hello {
        Hello {
            build_id: self.build_id,
            status: match self.last_error {
                Some(_) => Status::Error,
                None => Status::Ok,
            },
            error: self.last_error.clone(),
        }
    }
}

/// The state shared by the server's handlers and the rebuild loop. A `std` lock: it is
/// only ever held briefly and never across an `.await`, so [`apply_build`] and
/// [`apply_error`] can be called from blocking code.
pub type Shared = Arc<RwLock<ServeState>>;

/// A new build-event channel with [`EVENT_CAPACITY`]. Each `/__presenter/events`
/// connection subscribes its own receiver; send with [`publish`].
pub fn event_channel() -> broadcast::Sender<BuildEvent> {
    broadcast::channel(EVENT_CAPACITY).0
}

/// Sends `event` to every connected browser and returns how many receive it. No browser
/// being connected is not an error.
pub fn publish(events: &broadcast::Sender<BuildEvent>, event: BuildEvent) -> usize {
    let build_id = event.build_id();
    let receivers = events.send(event).unwrap_or(0);
    tracing::debug!("build {build_id}: event sent to {receivers} browser(s)");
    receivers
}

/// The id the next [`apply_build`] or [`apply_error`] will return: render the next build
/// with `BuildInfo::serve(next_build_id(&state))` so that the document's `?v=<id>` names it.
pub fn next_build_id(state: &Shared) -> u64 {
    read(state).build_id + 1
}

/// Stores a successful build: its document, theme stylesheet, deck assets and theme; clears
/// the error and bumps `build_id`. Returns the new `build_id`.
pub fn apply_build(state: &Shared, build: &Build) -> u64 {
    let document = build.document.html.clone();
    let theme_css = build.document.theme_css.clone();
    let assets = build.assets.clone();
    let theme = build.theme.clone();
    let mut state = write(state);
    state.build_id += 1;
    state.document = Some(document);
    state.theme_css = theme_css;
    state.assets = assets;
    state.theme = Some(theme);
    state.last_error = None;
    state.build_id
}

/// Stores a failed attempt: sets the error (the last good document stays) and bumps
/// `build_id`. Returns the new `build_id`.
pub fn apply_error(state: &Shared, err: &Error) -> u64 {
    let payload = ErrorPayload::from(err);
    let mut state = write(state);
    state.build_id += 1;
    state.last_error = Some(payload);
    state.build_id
}

/// Handler state: the served build and the event channel to subscribe to.
#[derive(Clone)]
struct App {
    state: Shared,
    events: broadcast::Sender<BuildEvent>,
}

/// The dev server's routes (see the module docs) over `state`; `/__presenter/events`
/// subscribes to `events`.
pub fn router(state: Shared, events: broadcast::Sender<BuildEvent>) -> Router {
    Router::new()
        .route("/", get(index))
        .route(SERVE_THEME_CSS, get(theme_css))
        .route(
            &format!("{SERVE_HELPER_PREFIX}{{*path}}"),
            get(helper_asset),
        )
        .route(&format!("{SERVE_ASSET_PREFIX}{{id}}"), get(deck_asset))
        .route("/__presenter/events", get(build_events))
        .route(SERVE_LIVE_RELOAD_JS, get(live_reload_js))
        .route("/__presenter/state", get(state_json))
        .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }))
        .with_state(App { state, events })
}

/// Serves `router` on `listener` until ctrl-c, then shuts down gracefully (see
/// [`serve_until`]).
pub async fn serve(listener: TcpListener, router: Router) -> Result<()> {
    serve_until(listener, router, ctrl_c()).await
}

/// Serves `router` on `listener` until `shutdown` completes, then shuts down gracefully:
/// no new connections are accepted, every open event stream ends, and the call returns once
/// the remaining requests have finished (at most [`SHUTDOWN_GRACE`] later; a client that
/// never completes its request would otherwise hold the server).
///
/// `router` is normally [`router`]`(…)`, possibly merged with more routes. Errors: an I/O
/// error of the server ([`Error::Io`]); axum 0.8 handles accept errors itself and never
/// returns one, so in practice this returns `Ok(())` after the shutdown.
pub async fn serve_until<F>(listener: TcpListener, router: Router, shutdown: F) -> Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let (down_tx, down_rx) = watch::channel(false);
    let app = router.layer(Extension(ShutdownSignal(down_rx.clone())));
    let signal = async move {
        shutdown.await;
        tracing::info!("dev server shutting down");
        // Ends every event stream, so that open browser tabs do not hold the server.
        let _ = down_tx.send(true);
    };
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(signal)
        .into_future();
    let deadline = async move {
        ShutdownSignal(down_rx).wait().await;
        tokio::time::sleep(SHUTDOWN_GRACE).await;
    };
    tokio::select! {
        result = server => result?,
        () = deadline => tracing::warn!(
            "dev server: connections still open {SHUTDOWN_GRACE:?} after shutdown; closing anyway"
        ),
    }
    Ok(())
}

/// Completes on ctrl-c. If the handler cannot be installed, never completes.
async fn ctrl_c() {
    if let Err(err) = tokio::signal::ctrl_c().await {
        tracing::warn!("cannot listen for ctrl-c: {err}");
        std::future::pending::<()>().await;
    }
}

/// The server's shutdown flag, given to the handlers by [`serve_until`].
#[derive(Clone)]
struct ShutdownSignal(watch::Receiver<bool>);

impl ShutdownSignal {
    /// Completes once the server shuts down (or the flag's sender is gone).
    async fn wait(mut self) {
        let _ = self.0.wait_for(|&down| down).await;
    }
}

/// A stream that ends when the server shuts down (an endless SSE response would otherwise
/// keep the graceful shutdown waiting).
struct UntilShutdown<S> {
    inner: S,
    /// `None` without a signal (a router served by something other than [`serve_until`]).
    shutdown: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
    ended: bool,
}

impl<S> UntilShutdown<S> {
    fn new(inner: S, signal: Option<ShutdownSignal>) -> Self {
        UntilShutdown {
            inner,
            shutdown: signal.map(|signal| -> Pin<Box<dyn Future<Output = ()> + Send>> {
                Box::pin(signal.wait())
            }),
            ended: false,
        }
    }
}

impl<S: Stream + Unpin> Stream for UntilShutdown<S> {
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<S::Item>> {
        if self.ended {
            return Poll::Ready(None);
        }
        if let Some(shutdown) = self.shutdown.as_mut()
            && shutdown.as_mut().poll(cx).is_ready()
        {
            self.ended = true;
            return Poll::Ready(None);
        }
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

/// `GET /`: the latest successful document, else the fallback page.
async fn index(State(app): State<App>) -> Response {
    let html = {
        let state = read(&app.state);
        match &state.document {
            Some(document) => document.clone(),
            None => fallback_page(state.last_error.as_ref()),
        }
    };
    ([(header::CACHE_CONTROL, NO_STORE)], Html(html)).into_response()
}

/// `GET /__presenter/theme.css`.
async fn theme_css(State(app): State<App>) -> Response {
    let css = read(&app.state).theme_css.clone();
    file_response(css.into_bytes(), "text/css", NO_STORE)
}

/// `GET /__presenter/asset/{*path}` (axum percent-decodes `path`).
async fn helper_asset(State(app): State<App>, Path(path): Path<String>) -> Response {
    let theme = read(&app.state).theme.clone();
    blocking(move || helper_asset_response(theme.as_ref(), &path)).await
}

/// `GET /__presenter/deck-asset/{id}`.
async fn deck_asset(State(app): State<App>, Path(id): Path<String>) -> Response {
    blocking(move || deck_asset_response(&app.state, &id)).await
}

/// `GET /__presenter/live-reload.js`.
async fn live_reload_js() -> Response {
    blocking(|| match crate::vendor::get(LIVE_RELOAD_KEY) {
        Some(bytes) => file_response(bytes.into_owned(), "text/javascript", NO_STORE),
        None => server_error(&format!("embedded {LIVE_RELOAD_KEY} is missing")),
    })
    .await
}

/// `GET /__presenter/state`: `{"build_id", "status", "error"}` (`error` is `null` when the
/// latest attempt succeeded).
async fn state_json(State(app): State<App>) -> Response {
    #[derive(Serialize)]
    struct StateJson {
        build_id: u64,
        status: Status,
        error: Option<ErrorPayload>,
    }
    let Hello {
        build_id,
        status,
        error,
    } = read(&app.state).hello();
    let body = StateJson {
        build_id,
        status,
        error,
    };
    ([(header::CACHE_CONTROL, NO_STORE)], Json(body)).into_response()
}

/// `GET /__presenter/events`: `hello` with the current build, then one `build` event per
/// [`publish`]ed [`BuildEvent`]. A browser that lags behind skips the oldest events; the
/// stream ends when the server shuts down.
async fn build_events(
    State(app): State<App>,
    shutdown: Option<Extension<ShutdownSignal>>,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    // Subscribe before reading the state, so that no build stored after `hello` is missed.
    // A build stored in between is reported twice (in `hello` and as an event); the client
    // then refreshes the page it already shows, which changes nothing.
    let receiver = app.events.subscribe();
    let hello = read(&app.state).hello();
    let builds = BroadcastStream::new(receiver).filter_map(|item| match item {
        Ok(event) => sse_event("build", &event),
        Err(BroadcastStreamRecvError::Lagged(skipped)) => {
            tracing::debug!("event stream lagged behind; {skipped} build event(s) skipped");
            None
        }
    });
    let stream = tokio_stream::iter(sse_event("hello", &hello))
        .chain(builds)
        .map(Ok);
    let stream = UntilShutdown::new(stream, shutdown.map(|Extension(signal)| signal));
    Sse::new(stream).keep_alive(KeepAlive::new().interval(KEEP_ALIVE))
}

/// An SSE event named `name` with `data` as JSON; `None` (logged) if it cannot be encoded.
fn sse_event<T: Serialize>(name: &str, data: &T) -> Option<Event> {
    match Event::default().event(name).json_data(data) {
        Ok(event) => Some(event),
        Err(err) => {
            tracing::warn!("cannot encode the '{name}' event: {err}");
            None
        }
    }
}

/// The response for helper path `path`: the file with its MIME type, 404 if there is none.
/// Before any build succeeded there is no theme, and only `vendor/…` and `presenter/…`
/// files exist.
fn helper_asset_response(theme: Option<&Theme>, path: &str) -> Response {
    let found = match theme {
        Some(theme) => resolve_helper_path(theme, path),
        None => Ok(builtin_file(path)),
    };
    match found {
        Ok(Some((bytes, mime))) => {
            let vendor = normalize_helper_path(path).is_some_and(|key| key.starts_with("vendor/"));
            file_response(bytes, &mime, if vendor { VENDOR_CACHE } else { NO_STORE })
        }
        Ok(None) => not_found(&format!("{SERVE_HELPER_PREFIX}{path}")),
        Err(err) => server_error(&err.to_string()),
    }
}

/// A `vendor/…` or `presenter/…` helper path's bytes and MIME type.
fn builtin_file(path: &str) -> Option<(Vec<u8>, String)> {
    let key = normalize_helper_path(path)?;
    if !(key.starts_with("vendor/") || key.starts_with("presenter/")) {
        return None;
    }
    let bytes = crate::vendor::get(&key)?.into_owned();
    let mime = mime_guess::from_path(&key).first_or_octet_stream();
    Some((bytes, mime.essence_str().to_string()))
}

/// The response for deck asset `id`: its bytes with its MIME type, 404 if the id is not
/// registered or its file is gone.
fn deck_asset_response(state: &Shared, id: &str) -> Response {
    let state = read(state);
    let Some(asset) = state.assets.get(id) else {
        return not_found(&format!("{SERVE_ASSET_PREFIX}{id}"));
    };
    match state.assets.read(id) {
        Ok(bytes) => file_response(bytes.into_owned(), &asset.mime, NO_STORE),
        Err(err @ Error::MissingAsset { .. }) => not_found(&err.to_string()),
        Err(err) => server_error(&err.to_string()),
    }
}

/// Runs `f` (file reads) on the blocking pool.
async fn blocking<F>(f: F) -> Response
where
    F: FnOnce() -> Response + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|err| server_error(&format!("request handler failed: {err}")))
}

/// A `200 OK` with `bytes`, `Content-Type: mime` (`; charset=utf-8` for text) and
/// `Cache-Control: cache`.
fn file_response(bytes: Vec<u8>, mime: &str, cache: &'static str) -> Response {
    let text = mime.starts_with("text/") || matches!(mime, "application/javascript");
    let content_type = if text {
        format!("{mime}; charset=utf-8")
    } else {
        mime.to_string()
    };
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, cache.to_string()),
        ],
        bytes,
    )
        .into_response()
}

/// `404 Not Found` naming `what`.
fn not_found(what: &str) -> Response {
    (StatusCode::NOT_FOUND, format!("not found: {what}\n")).into_response()
}

/// `500 Internal Server Error` with `message` (also logged).
fn server_error(message: &str) -> Response {
    tracing::warn!("dev server: {message}");
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{message}\n")).into_response()
}

/// The inline style of the error overlay, as the client script sets it.
const OVERLAY_STYLE: &str = "position:fixed;top:1em;right:1em;max-width:50%;max-height:80vh;\
overflow:auto;z-index:10000;background:#222;color:#eee;border-left:4px solid #e55;\
padding:1em;font:13px monospace;white-space:pre-wrap";

/// The page `GET /` serves until a build succeeds: the client script (which reloads into
/// the deck on the first good build) and, after a failed build, the error overlay
/// pre-rendered with the same text the client would show.
fn fallback_page(error: Option<&ErrorPayload>) -> String {
    let (title, lead, overlay) = match error {
        Some(err) => (
            "presenter: build error",
            "The deck could not be built. Fix the error and save: this page reloads by itself.",
            format!(
                "<div id=\"presenter-overlay\" style=\"{OVERLAY_STYLE}\">{}</div>\n",
                html_escape::encode_text(&overlay_text(err))
            ),
        ),
        None => (
            "presenter: building",
            "The first build has not finished yet: this page reloads by itself.",
            String::new(),
        ),
    };
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<title>{title}</title>\n\
<style>body{{margin:0;padding:2em 1em;font:16px/1.5 system-ui,sans-serif;\
background:#fafafa;color:#222}}</style>\n</head>\n<body>\n<p>{lead}</p>\n{overlay}\
<script src=\"{SERVE_LIVE_RELOAD_JS}\"></script>\n</body>\n</html>\n"
    )
}

/// The overlay text of `err`, line by line as the client builds it: `build error (kind)`,
/// the message, then `file:line` when the file is known.
fn overlay_text(err: &ErrorPayload) -> String {
    let mut lines = vec![format!("build error ({})", err.kind), err.message.clone()];
    if let Some(file) = &err.file {
        lines.push(match err.line {
            Some(line) => format!("{file}:{line}"),
            None => file.clone(),
        });
    }
    lines.join("\n")
}

/// Read access to the state. A poisoned lock is used as is: writers only assign fields.
fn read(state: &Shared) -> RwLockReadGuard<'_, ServeState> {
    state.read().unwrap_or_else(PoisonError::into_inner)
}

/// Write access to the state (see [`read`]).
fn write(state: &Shared) -> RwLockWriteGuard<'_, ServeState> {
    state.write().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::assets::emit::OutputMode;
    use crate::pipeline::{BuildOptions, build_deck};
    use crate::render::BuildInfo;
    use crate::serve::watch::ChangeKind;

    fn shared() -> Shared {
        Arc::new(RwLock::new(ServeState::default()))
    }

    fn minimal_build(id: u64) -> Build {
        let input = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/md/minimal.md");
        build_deck(&BuildOptions {
            output: OutputMode::Serve,
            build: BuildInfo::serve(id),
            missing_asset_is_error: false,
            ..BuildOptions::new(input)
        })
        .unwrap()
    }

    #[test]
    fn builds_and_errors_bump_the_id_and_keep_the_last_good_document() {
        let state = shared();
        assert_eq!(
            read(&state).hello(),
            Hello {
                build_id: 0,
                status: Status::Ok,
                error: None
            }
        );
        assert_eq!(next_build_id(&state), 1);
        let build = minimal_build(1);
        assert_eq!(apply_build(&state, &build), 1);
        {
            let s = read(&state);
            assert_eq!(s.document.as_deref(), Some(build.document.html.as_str()));
            assert_eq!(s.theme_css, build.document.theme_css);
            assert!(s.theme.is_some());
            assert_eq!(s.assets, build.assets);
        }
        let err = Error::Config("bad".into());
        assert_eq!(apply_error(&state, &err), 2);
        let s = read(&state).clone();
        assert_eq!(s.document.as_deref(), Some(build.document.html.as_str()));
        assert_eq!(
            s.hello(),
            Hello {
                build_id: 2,
                status: Status::Error,
                error: Some(ErrorPayload::from(&err)),
            }
        );
        assert_eq!(next_build_id(&state), 3);
        assert_eq!(apply_build(&state, &build), 3);
        assert_eq!(read(&state).last_error, None);
    }

    #[test]
    fn fallback_page_loads_the_client_and_shows_the_error() {
        let err = ErrorPayload {
            message: "slides.md:3 <unknown> & \"bad\"".into(),
            file: Some("slides.md".into()),
            line: Some(3),
            kind: "parse".into(),
        };
        let page = fallback_page(Some(&err));
        assert!(page.starts_with("<!DOCTYPE html>"), "{page}");
        assert!(page.contains(r#"<script src="/__presenter/live-reload.js"></script>"#));
        assert!(
            page.contains(
                "<div id=\"presenter-overlay\" style=\"position:fixed;top:1em;right:1em;"
            )
        );
        assert!(
            page.contains(
                "build error (parse)\nslides.md:3 &lt;unknown&gt; &amp; \"bad\"\nslides.md:3</div>"
            ),
            "{page}"
        );
        assert!(!page.contains("class=\"slides\""));

        let waiting = fallback_page(None);
        assert!(waiting.contains(r#"<script src="/__presenter/live-reload.js"></script>"#));
        assert!(!waiting.contains("presenter-overlay"));
    }

    #[test]
    fn overlay_text_leaves_out_an_unknown_location() {
        let mut err = ErrorPayload {
            message: "m".into(),
            file: None,
            line: Some(4),
            kind: "missing-asset".into(),
        };
        assert_eq!(overlay_text(&err), "build error (missing-asset)\nm");
        err.file = Some("styles/theme.css".into());
        err.line = None;
        assert_eq!(
            overlay_text(&err),
            "build error (missing-asset)\nm\nstyles/theme.css"
        );
    }

    #[test]
    fn helper_paths_without_a_theme_find_only_builtin_files() {
        assert!(builtin_file("presenter/live-reload.js").is_some());
        let (bytes, mime) = builtin_file("vendor/reveal/dist/reveal.css").unwrap();
        assert!(!bytes.is_empty());
        assert_eq!(mime, "text/css");
        assert!(builtin_file("vendor/nope.js").is_none());
        assert!(builtin_file("themes/default/theme.toml").is_none());
        assert!(builtin_file("vendor/../themes/default/theme.toml").is_none());
        assert!(builtin_file("../vendor/reveal/dist/reveal.css").is_none());
        let response = helper_asset_response(None, "logo.png");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn file_responses_carry_type_and_cache_headers() {
        let header_of = |response: &Response, name| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        let theme = minimal_build(1).theme;
        let vendor = helper_asset_response(Some(&theme), "vendor/reveal/dist/reveal.js");
        assert_eq!(vendor.status(), StatusCode::OK);
        assert_eq!(
            header_of(&vendor, header::CONTENT_TYPE).as_deref(),
            Some("text/javascript; charset=utf-8")
        );
        assert_eq!(
            header_of(&vendor, header::CACHE_CONTROL).as_deref(),
            Some(VENDOR_CACHE)
        );
        let own = helper_asset_response(Some(&theme), "@default/theme.toml");
        assert_eq!(own.status(), StatusCode::OK);
        assert_eq!(
            header_of(&own, header::CACHE_CONTROL).as_deref(),
            Some(NO_STORE)
        );
        let png = file_response(vec![1, 2], "image/png", NO_STORE);
        assert_eq!(
            header_of(&png, header::CONTENT_TYPE).as_deref(),
            Some("image/png")
        );
    }

    #[tokio::test]
    async fn event_streams_end_on_shutdown() {
        let (tx, rx) = watch::channel(false);
        let mut stream =
            UntilShutdown::new(tokio_stream::pending::<u8>(), Some(ShutdownSignal(rx)));
        let poll = tokio::time::timeout(Duration::from_millis(50), stream.next()).await;
        assert!(poll.is_err(), "the stream must stay open before shutdown");
        tx.send(true).unwrap();
        let next = tokio::time::timeout(Duration::from_secs(1), stream.next()).await;
        assert_eq!(next.unwrap(), None);
        // Polling again after the end is fine.
        assert_eq!(stream.next().await, None);

        let mut open = UntilShutdown::new(tokio_stream::iter([1u8, 2]), None);
        assert_eq!(open.next().await, Some(1));
        assert_eq!(open.next().await, Some(2));
        assert_eq!(open.next().await, None);
    }

    #[test]
    fn publish_without_browsers_is_not_an_error() {
        let events = event_channel();
        let event = BuildEvent::Ok {
            build_id: 1,
            kind: ChangeKind::Content,
            duration_ms: 3,
        };
        assert_eq!(publish(&events, event.clone()), 0);
        let mut receiver = events.subscribe();
        assert_eq!(publish(&events, event.clone()), 1);
        assert_eq!(receiver.try_recv().unwrap(), event);
    }
}
