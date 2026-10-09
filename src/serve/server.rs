//! axum dev server: document, theme CSS, assets and SSE endpoints
//! (`docs/spec/live-reload.md` §1, §2 and §4).
//!
//! The server keeps the latest build in memory ([`ServeState`], shared as [`Shared`]) and
//! pushes one `build` event per rebuild to every connected browser through a
//! [`broadcast`] channel ([`event_channel`]). The rebuild loop (T20/T23) renders with
//! `OutputMode::Serve` and `BuildInfo::serve(`[`next_build_id`]`(&state))`, stores the result
//! with [`apply_build`] or [`apply_error_payload`] (with `ErrorPayload::for_input`, so a front
//! matter error is located in the deck source) and then [`publish`]es the matching
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
//!
//! Both asset routes answer single byte-range requests (`206 Partial Content`, which media
//! elements rely on; Safari plays no video without it), and a file-backed deck asset is
//! streamed from disk rather than read into memory as a whole.
//!
//! A stylesheet of the theme chain that the asset route serves (one a template links with
//! `style()`) has its relative `url()`s rewritten to the absolute, versioned URLs of the
//! files they name (`/__presenter/asset/<path>?v=<content version>`,
//! [`crate::render::html::serve_stylesheet`]), so an edited image shows up after the
//! client re-requests the stylesheet (live-reload.md §4, audit F03).
//!
//! **Host check (live-reload.md §4, audit F07).** [`serve_until`] answers only requests
//! whose `Host` header names this server: `localhost`, any IP address (a DNS rebinding
//! attack needs a name; this also keeps `--host 0.0.0.0` usable from other machines at
//! the machine's address) or a name it was given ([`AllowedHosts::with_name`], the
//! `--host` of `presenter serve`), with the port it listens on (no port for port 80). Any
//! other request, on every route, gets `403 Forbidden`: a web page that rebinds its own
//! name to `127.0.0.1` cannot read the deck, its assets, the theme files or the build
//! events. A request without a `Host` header (HTTP/1.0; browsers always send one) passes.

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::future::{Future, IntoFuture};
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
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
use crate::assets::registry::{AssetData, AssetRegistry};
use crate::error::{Error, Result};
use crate::pipeline::Build;
use crate::render::html::{
    SERVE_HELPER_PREFIX, SERVE_LIVE_RELOAD_JS, SERVE_THEME_CSS, serve_stylesheet,
};
use crate::render::{normalize_helper_path, resolve_helper_path};
use crate::serve::events::{BuildEvent, ErrorPayload, Hello, Status};
use crate::serve::files::{bytes_response, file_response, whole_response};
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
    apply_error_payload(state, ErrorPayload::from(err))
}

/// [`apply_error`] with the overlay payload made by the caller: the rebuild loop passes
/// `ErrorPayload::for_input(&err, input)`, which locates a missing asset or front matter
/// error in the deck source and quotes its line (T44).
pub fn apply_error_payload(state: &Shared, payload: ErrorPayload) -> u64 {
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

/// Serves `router` on `listener` until ctrl-c (SIGINT), then shuts down gracefully: exactly
/// [`serve_until`] with ctrl-c as the shutdown signal.
pub async fn serve(listener: TcpListener, router: Router) -> Result<()> {
    serve_until(listener, router, ctrl_c()).await
}

/// Serves `router` on `listener` until `shutdown` completes, then shuts down gracefully:
/// no new connections are accepted, every open event stream ends, and the call returns once
/// the remaining requests have finished (at most [`SHUTDOWN_GRACE`] later; a client that
/// never completes its request would otherwise hold the server).
///
/// Only requests for the listener's address pass the Host check (module docs): exactly
/// [`serve_until_allowing`] with [`AllowedHosts::new`] of that address.
///
/// `router` is normally [`router`]`(…)`, possibly merged with more routes. Errors: the
/// listener's address cannot be read, or an I/O error of the server ([`Error::Io`]); axum
/// 0.8 handles accept errors itself and never returns one, so in practice this returns
/// `Ok(())` after the shutdown.
pub async fn serve_until<F>(listener: TcpListener, router: Router, shutdown: F) -> Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let hosts = AllowedHosts::new(listener.local_addr()?);
    serve_until_allowing(listener, router, shutdown, hosts).await
}

/// [`serve_until`] answering the requests whose `Host` header `hosts` allows (module docs);
/// every other request gets `403 Forbidden`.
pub async fn serve_until_allowing<F>(
    listener: TcpListener,
    router: Router,
    shutdown: F,
    hosts: AllowedHosts,
) -> Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let (down_tx, down_rx) = watch::channel(false);
    let app = router
        .layer(Extension(ShutdownSignal(down_rx.clone())))
        .layer(middleware::from_fn_with_state(Arc::new(hosts), check_host));
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

/// The `Host` headers the dev server answers (module docs, "Host check"): `localhost`,
/// every IP address and the names added with [`AllowedHosts::with_name`], each with the
/// port the server listens on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedHosts {
    /// The port the server listens on.
    port: u16,
    /// The host names allowed besides IP addresses, lowercase.
    names: BTreeSet<String>,
}

impl AllowedHosts {
    /// The hosts of a server listening on `addr`: `localhost` and every IP address, with
    /// `addr`'s port (the bound one: not 0).
    pub fn new(addr: SocketAddr) -> Self {
        AllowedHosts {
            port: addr.port(),
            names: BTreeSet::from(["localhost".to_string()]),
        }
    }

    /// The same hosts and the host name `name` (the `--host` the server was started with;
    /// an IP address or `localhost` adds nothing new), compared ignoring ASCII case.
    pub fn with_name(mut self, name: &str) -> Self {
        let name = name.trim().to_ascii_lowercase();
        if !name.is_empty() {
            self.names.insert(name);
        }
        self
    }

    /// True if a request whose `Host` header is `host` (`None`: it has none) is for this
    /// server: an allowed name or any IP address (IPv6 in brackets), with the server's
    /// port, or without a port when that is 80.
    pub fn allows(&self, host: Option<&str>) -> bool {
        let Some(host) = host else {
            return true;
        };
        let Some((name, port)) = split_host(host) else {
            return false;
        };
        let port = port.unwrap_or(80);
        port == self.port
            && (name.parse::<IpAddr>().is_ok() || self.names.contains(&name.to_ascii_lowercase()))
    }
}

/// The name and port of a `Host` header value (`name`, `name:port`, `[v6]`, `[v6]:port`),
/// without the brackets of an IPv6 address; `None` when it is malformed.
fn split_host(host: &str) -> Option<(&str, Option<u16>)> {
    let (name, port) = match host.strip_prefix('[') {
        Some(rest) => {
            let (address, after) = rest.split_once(']')?;
            address.parse::<std::net::Ipv6Addr>().ok()?;
            match after {
                "" => (address, None),
                _ => (address, Some(after.strip_prefix(':')?)),
            }
        }
        None => match host.rsplit_once(':') {
            Some((name, port)) => (name, Some(port)),
            None => (host, None),
        },
    };
    let port = match port {
        Some(port) => Some(port.parse::<u16>().ok()?),
        None => None,
    };
    (!name.is_empty()).then_some((name, port))
}

/// The Host check (module docs): passes a request that `hosts` allows to `next`, answers
/// any other with `403 Forbidden`.
async fn check_host(
    State(hosts): State<Arc<AllowedHosts>>,
    request: Request,
    next: Next,
) -> Response {
    let host = match request.headers().get(header::HOST) {
        // A value that is not visible ASCII is no host this server has.
        Some(value) => Some(value.to_str().unwrap_or_default()),
        None => request
            .uri()
            .authority()
            .map(|authority| authority.as_str()),
    };
    if hosts.allows(host) {
        return next.run(request).await;
    }
    let host = host.unwrap_or_default().to_string();
    tracing::info!("refused a request for host {host:?} (not this server)");
    let message = format!(
        "presenter serve answers requests for localhost, an IP address or its --host name \
         on port {}, not for host {host:?} (a protection against DNS rebinding): open the \
         address it printed\n",
        hosts.port
    );
    (StatusCode::FORBIDDEN, message).into_response()
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
    whole_response(css.into_bytes(), "text/css", NO_STORE)
}

/// `GET /__presenter/asset/{*path}` (axum percent-decodes `path`).
async fn helper_asset(
    State(app): State<App>,
    Path(path): Path<String>,
    request: HeaderMap,
) -> Response {
    let theme = read(&app.state).theme.clone();
    blocking(move || helper_asset_response(theme.as_ref(), &path, &request)).await
}

/// `GET /__presenter/deck-asset/{id}`: the asset's bytes (or the requested range of them)
/// with its MIME type; 404 if the id is not registered or its file is gone.
async fn deck_asset(
    State(app): State<App>,
    Path(id): Path<String>,
    request: HeaderMap,
) -> Response {
    // Only the registration is copied under the lock (a path, or the bytes of an in-memory
    // asset); the file is read after the lock is released.
    let asset = read(&app.state).assets.get(&id).cloned();
    let Some(asset) = asset else {
        return not_found(&format!("{SERVE_ASSET_PREFIX}{id}"));
    };
    match asset.data {
        AssetData::Bytes(bytes) => bytes_response(bytes, &asset.mime, NO_STORE, &request),
        AssetData::File(path) => {
            match file_response(path.clone(), &asset.mime, NO_STORE, &request).await {
                Ok(response) => response,
                Err(err) if is_missing(&err) => {
                    not_found(&Error::MissingAsset { path, span: None }.to_string())
                }
                Err(err) => server_error(&format!("{}: {err}", path.display())),
            }
        }
    }
}

/// Whether `err` means that a file does not exist (as [`AssetRegistry::read`] decides).
fn is_missing(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// `GET /__presenter/live-reload.js`.
async fn live_reload_js() -> Response {
    blocking(|| match crate::vendor::get(LIVE_RELOAD_KEY) {
        Some(bytes) => whole_response(bytes.into_owned(), "text/javascript", NO_STORE),
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

/// `GET /__presenter/events`: [`event_stream`] with a keep-alive every 15 s.
async fn build_events(
    State(app): State<App>,
    shutdown: Option<Extension<ShutdownSignal>>,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    event_stream(&app, shutdown.map(|Extension(signal)| signal), KEEP_ALIVE)
}

/// `hello` with the current build, then one `build` event per [`publish`]ed [`BuildEvent`]
/// ([`build_stream`]), and a keep-alive comment after `keep_alive` without an event. The
/// stream ends when `shutdown` fires.
fn event_stream(
    app: &App,
    shutdown: Option<ShutdownSignal>,
    keep_alive: Duration,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>> + use<>> {
    // Subscribe before reading the state, so that no build stored after `hello` is missed.
    // A build stored in between is reported twice (in `hello` and as an event); the client
    // then refreshes the page it already shows, which changes nothing.
    let receiver = app.events.subscribe();
    let hello = read(&app.state).hello();
    let builds = build_stream(receiver).filter_map(|event| sse_event("build", &event));
    let stream = tokio_stream::iter(sse_event("hello", &hello))
        .chain(builds)
        .map(Ok);
    let stream = UntilShutdown::new(stream, shutdown);
    Sse::new(stream).keep_alive(KeepAlive::new().interval(keep_alive))
}

/// The build events `receiver` gets. A receiver that fell more than [`EVENT_CAPACITY`]
/// events behind skips the oldest ones (logged) and goes on with the newest.
fn build_stream(
    receiver: broadcast::Receiver<BuildEvent>,
) -> impl Stream<Item = BuildEvent> + Send + Unpin + 'static {
    BroadcastStream::new(receiver).filter_map(|item| match item {
        Ok(event) => Some(event),
        Err(BroadcastStreamRecvError::Lagged(skipped)) => {
            tracing::debug!("event stream lagged behind; {skipped} build event(s) skipped");
            None
        }
    })
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

/// The response for helper path `path` to a request with `request` headers: the file (or
/// the requested range of it) with its MIME type, 404 if there is none. Before any build
/// succeeded there is no theme, and only `vendor/…` and `presenter/…` files exist.
fn helper_asset_response(theme: Option<&Theme>, path: &str, request: &HeaderMap) -> Response {
    let found = match theme {
        Some(theme) => resolve_helper_path(theme, path),
        None => Ok(builtin_file(path)),
    };
    match found {
        Ok(Some((bytes, mime))) => {
            let key = normalize_helper_path(path).unwrap_or_default();
            let vendor = key.starts_with("vendor/");
            let cache = if vendor { VENDOR_CACHE } else { NO_STORE };
            let bytes = match theme {
                Some(theme) if mime == "text/css" && !is_builtin_key(&key) => {
                    theme_stylesheet(theme, &key, bytes)
                }
                _ => bytes,
            };
            bytes_response(bytes, &mime, cache, request)
        }
        Ok(None) => not_found(&format!("{SERVE_HELPER_PREFIX}{path}")),
        Err(err) => server_error(&err.to_string()),
    }
}

/// True for a normalized helper path of the embedded tree (`vendor/…`, `presenter/…`).
fn is_builtin_key(key: &str) -> bool {
    key.starts_with("vendor/") || key.starts_with("presenter/")
}

/// The stylesheet at helper path `key` of `theme`'s chain (`bytes`) as served: its relative
/// `url()`s rewritten to versioned URLs ([`serve_stylesheet`], module docs); as it is when
/// it is not UTF-8 or cannot be rewritten (the browser then resolves it as written).
fn theme_stylesheet(theme: &Theme, key: &str, bytes: Vec<u8>) -> Vec<u8> {
    let Ok(css) = std::str::from_utf8(&bytes) else {
        return bytes;
    };
    match serve_stylesheet(theme, key, css) {
        Ok(css) => css.into_bytes(),
        Err(err) => {
            tracing::debug!("serving {key} as written: {err}");
            bytes
        }
    }
}

/// A `vendor/…` or `presenter/…` helper path's bytes and MIME type.
fn builtin_file(path: &str) -> Option<(Vec<u8>, String)> {
    let key = normalize_helper_path(path)?;
    if !is_builtin_key(&key) {
        return None;
    }
    let bytes = crate::vendor::get(&key)?.into_owned();
    let mime = mime_guess::from_path(&key).first_or_octet_stream();
    Some((bytes, mime.essence_str().to_string()))
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

/// `404 Not Found` naming `what`.
fn not_found(what: &str) -> Response {
    (StatusCode::NOT_FOUND, format!("not found: {what}\n")).into_response()
}

/// `500 Internal Server Error` with `message` (also logged).
fn server_error(message: &str) -> Response {
    tracing::warn!("dev server: {message}");
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{message}\n")).into_response()
}

/// The inline style of the error overlay, as the client script's `showOverlay` sets it.
const OVERLAY_STYLE: &str = "position:fixed;top:1em;right:1em;max-width:50%;max-height:80vh;\
overflow:auto;z-index:10000;background:#222;color:#eee;border-left:4px solid #e55;\
padding:1em;font:13px monospace;white-space:pre-wrap;cursor:pointer";

/// The page `GET /` serves until a build succeeds: the client script (which reloads into
/// the deck on the first good build) and, after a failed build, the error overlay
/// pre-rendered as the client's `showOverlay` renders it: same style, same text, hidden on
/// click. (The client re-renders it from `hello`, with any field this page does not show.)
fn fallback_page(error: Option<&ErrorPayload>) -> String {
    let (title, lead, overlay) = match error {
        Some(err) => (
            "presenter: build error",
            "The deck could not be built. Fix the error and save: this page reloads by itself.",
            format!(
                "<div id=\"presenter-overlay\" style=\"{OVERLAY_STYLE}\" \
onclick=\"this.style.display='none'\">{}</div>\n",
                overlay_html(err)
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

/// The overlay text of `err`, line by line as the client's `showOverlay` builds it:
/// `build error (kind)`, the message, `file:line` when the file is known, then
/// `(click to dismiss)`.
fn overlay_text(err: &ErrorPayload) -> String {
    let mut lines = vec![
        match err.kind.as_str() {
            "" => "build error".to_string(),
            kind => format!("build error ({kind})"),
        },
        match err.message.as_str() {
            "" => "unknown error".to_string(),
            message => message.to_string(),
        },
    ];
    if let Some(file) = err.file.as_deref().filter(|file| !file.is_empty()) {
        lines.push(match err.line {
            Some(line) if line != 0 => format!("{file}:{line}"),
            _ => file.to_string(),
        });
    }
    lines.push("(click to dismiss)".to_string());
    lines.join("\n")
}

/// The inline style of the overlay's excerpt `<pre>`, as the client script sets it.
const EXCERPT_STYLE: &str = "margin:.5em 0;color:#fc6";

/// The overlay's content as HTML, as the client's `showOverlay` builds it: the
/// [`overlay_text`], with the error's excerpt (T44), when it has one, in a `<pre>` before
/// `(click to dismiss)`.
fn overlay_html(err: &ErrorPayload) -> String {
    let text = overlay_text(err);
    let excerpt = err.excerpt.as_deref().filter(|excerpt| !excerpt.is_empty());
    let (Some(excerpt), Some(head)) = (excerpt, text.strip_suffix("\n(click to dismiss)")) else {
        return html_escape::encode_text(&text).into_owned();
    };
    format!(
        "{}<pre style=\"{EXCERPT_STYLE}\">{}</pre>(click to dismiss)",
        html_escape::encode_text(head),
        html_escape::encode_text(excerpt)
    )
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
        // A payload made by the caller is stored as given.
        let mut payload = ErrorPayload::from(&Error::FrontMatter("bad".into()));
        payload.file = Some("slides.md".into());
        assert_eq!(apply_error_payload(&state, payload.clone()), 4);
        assert_eq!(read(&state).hello().error, Some(payload));
        assert_eq!(read(&state).build_id, 4);
    }

    /// A parse error in `slides.md` at line 3, as an overlay payload.
    fn parse_payload(message: &str) -> ErrorPayload {
        ErrorPayload::from(&Error::Parse {
            path: PathBuf::from("slides.md"),
            line: Some(3),
            message: message.into(),
        })
    }

    #[test]
    fn fallback_page_loads_the_client_and_shows_the_error() {
        let err = parse_payload("<unknown> & \"bad\"");
        let page = fallback_page(Some(&err));
        assert!(page.starts_with("<!DOCTYPE html>"), "{page}");
        assert!(page.contains(r#"<script src="/__presenter/live-reload.js"></script>"#));
        // The client's overlay: its style (a pointer cursor too), hidden on click.
        assert!(page.contains(&format!(
            "<div id=\"presenter-overlay\" style=\"{OVERLAY_STYLE}\" \
             onclick=\"this.style.display='none'\">"
        )));
        assert!(OVERLAY_STYLE.ends_with(";white-space:pre-wrap;cursor:pointer"));
        assert!(
            page.contains(
                "build error (parse)\nslides.md:3: &lt;unknown&gt; &amp; \"bad\"\nslides.md:3\n\
                 (click to dismiss)</div>"
            ),
            "{page}"
        );
        assert!(!page.contains("class=\"slides\""));

        // The excerpt (T44) in a `<pre>`, as the client shows it.
        let mut err = err;
        err.excerpt = Some("--- {title=\"<ab}".into());
        let page = fallback_page(Some(&err));
        assert!(
            page.contains(&format!(
                "\nslides.md:3<pre style=\"{EXCERPT_STYLE}\">--- {{title=\"&lt;ab}}</pre>\
                 (click to dismiss)</div>"
            )),
            "{page}"
        );
        err.excerpt = Some(String::new());
        assert!(fallback_page(Some(&err)).contains("\nslides.md:3\n(click to dismiss)</div>"));

        let waiting = fallback_page(None);
        assert!(waiting.contains(r#"<script src="/__presenter/live-reload.js"></script>"#));
        assert!(!waiting.contains("presenter-overlay"));
    }

    #[test]
    fn overlay_text_leaves_out_an_unknown_location() {
        let mut err = ErrorPayload::from(&Error::MissingAsset {
            path: PathBuf::from("/deck/x.png"),
            span: Some(crate::ir::SourceSpan {
                start_line: 4,
                end_line: 4,
                cell: None,
            }),
        });
        assert_eq!((err.file.as_deref(), err.line), (None, Some(4)));
        assert_eq!(
            overlay_text(&err),
            "build error (missing-asset)\nasset not found: /deck/x.png\n(click to dismiss)"
        );
        err.file = Some("styles/theme.css".into());
        err.line = None;
        assert_eq!(
            overlay_text(&err),
            "build error (missing-asset)\nasset not found: /deck/x.png\nstyles/theme.css\n\
             (click to dismiss)"
        );
        // Empty fields read as the client reads them.
        err.kind.clear();
        err.message.clear();
        err.file = None;
        assert_eq!(
            overlay_text(&err),
            "build error\nunknown error\n(click to dismiss)"
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
        let response = helper_asset_response(None, "logo.png", &HeaderMap::new());
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
        let none = HeaderMap::new();
        let vendor = helper_asset_response(Some(&theme), "vendor/reveal/dist/reveal.js", &none);
        assert_eq!(vendor.status(), StatusCode::OK);
        assert_eq!(
            header_of(&vendor, header::CONTENT_TYPE).as_deref(),
            Some("text/javascript; charset=utf-8")
        );
        assert_eq!(
            header_of(&vendor, header::CACHE_CONTROL).as_deref(),
            Some(VENDOR_CACHE)
        );
        let own = helper_asset_response(Some(&theme), "@default/theme.toml", &none);
        assert_eq!(own.status(), StatusCode::OK);
        assert_eq!(
            header_of(&own, header::CACHE_CONTROL).as_deref(),
            Some(NO_STORE)
        );
        let png = whole_response(vec![1, 2], "image/png", NO_STORE);
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

    fn ok_event(build_id: u64) -> BuildEvent {
        BuildEvent::Ok {
            build_id,
            kind: ChangeKind::Content,
            duration_ms: 1,
        }
    }

    #[tokio::test]
    async fn a_lagging_browser_skips_the_oldest_events_and_keeps_going() {
        let events = event_channel();
        let mut stream = build_stream(events.subscribe());
        let total = EVENT_CAPACITY as u64 + 6;
        for id in 1..=total {
            publish(&events, ok_event(id));
        }
        // The channel keeps the newest EVENT_CAPACITY events; the 6 oldest are skipped.
        for id in 7..=total {
            let event = tokio::time::timeout(Duration::from_secs(1), stream.next()).await;
            assert_eq!(event.unwrap().map(|e| e.build_id()), Some(id));
        }
        // And the stream goes on.
        publish(&events, ok_event(total + 1));
        let event = tokio::time::timeout(Duration::from_secs(1), stream.next()).await;
        assert_eq!(event.unwrap(), Some(ok_event(total + 1)));
        let quiet = tokio::time::timeout(Duration::from_millis(50), stream.next()).await;
        assert!(quiet.is_err(), "no more events");
    }

    #[tokio::test]
    async fn the_event_stream_sends_hello_keep_alives_and_builds() {
        let app = App {
            state: shared(),
            events: event_channel(),
        };
        apply_error(&app.state, &Error::Config("bad".into()));
        let interval = Duration::from_millis(30);
        let sse = event_stream(&app, None, interval).into_response();
        let mut body = sse.into_body().into_data_stream();
        let mut next = async || {
            let chunk = tokio::time::timeout(Duration::from_secs(2), body.next()).await;
            String::from_utf8(chunk.unwrap().unwrap().unwrap().to_vec()).unwrap()
        };
        assert_eq!(
            next().await,
            "event: hello\ndata: {\"build_id\":1,\"status\":\"error\",\"error\":\
             {\"message\":\"config: bad\",\"file\":null,\"line\":null,\"kind\":\"config\"}}\n\n"
        );
        // Nothing happens: a keep-alive comment (an empty one, axum's default).
        assert_eq!(next().await, ":\n\n");
        publish(&app.events, ok_event(2));
        let mut chunk = next().await;
        while chunk == ":\n\n" {
            chunk = next().await;
        }
        assert_eq!(
            chunk,
            "event: build\ndata: {\"status\":\"ok\",\"build_id\":2,\"kind\":\"content\",\
             \"duration_ms\":1}\n\n"
        );
    }

    #[test]
    fn hosts_are_this_server_by_name_or_address_and_port() {
        let hosts = AllowedHosts::new(SocketAddr::from(([127, 0, 0, 1], 4321)));
        for host in [
            "localhost:4321",
            "LocalHost:4321",
            "127.0.0.1:4321",
            "[::1]:4321",
            "192.168.1.5:4321",
            "[fe80::1]:4321",
        ] {
            assert!(hosts.allows(Some(host)), "{host}");
        }
        // No Host header at all (HTTP/1.0): browsers always send one.
        assert!(hosts.allows(None));
        for host in [
            "evil.example:4321",
            "evil.example",
            "localhost.evil.example:4321",
            "127.0.0.1.nip.io:4321",
            "localhost:80",
            "localhost:3000",
            "localhost",
            "127.0.0.1",
            "localhost:",
            "localhost:x",
            ":4321",
            "[::1",
            "[evil]:4321",
            "[::1]4321",
            "",
        ] {
            assert!(!hosts.allows(Some(host)), "{host}");
        }
        // A configured name; port 80 may be left out.
        let named =
            AllowedHosts::new(SocketAddr::from(([0, 0, 0, 0], 80))).with_name("MyMac.local");
        for host in [
            "mymac.local",
            "MYMAC.LOCAL:80",
            "localhost",
            "10.0.0.2",
            "[::1]",
        ] {
            assert!(named.allows(Some(host)), "{host}");
        }
        assert!(!named.allows(Some("other.local")));
        assert!(!named.allows(Some("mymac.local:8080")));
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
