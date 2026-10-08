//! `presenter serve` (cli.md `serve`, live-reload.md §4–§5): builds the deck, serves it,
//! watches its inputs and live-reloads every open browser.
//!
//! # Lifecycle
//!
//! 1. The Ctrl-C handler is installed first (step 5), then the file watcher
//!    ([`crate::serve::watch::Watcher`]) starts on [`watch_set_before_build`], so that a
//!    change saved during the first build is not lost.
//! 2. The first build (id 1, serve mode, missing assets are warnings) runs **before** the
//!    port is bound, so `GET /` is never empty. A failed first build does not stop the
//!    command: its error is printed and stored ([`server::apply_error`]; `GET /` then shows
//!    the error overlay) and the failed build's inputs are watched until it is fixed.
//! 3. The server binds `host:port` (`--host`/`--port`, else `[serve]` of `presenter.toml`,
//!    else `127.0.0.1:3000`; port 0 picks a free one) and prints
//!    `Serving http://<address>:<port>/` as the **first line on stdout** (always, also with
//!    `-q`: with port 0 it is the only place the port is shown). A port in use ends the
//!    command with exit 1 and the hint to use `--port 0`, also when the other server
//!    listens on the wildcard address (`0.0.0.0` or a dual-stack `[::]`), which macOS would
//!    let the bind shadow: unless the port is 0, the address is first probed with a
//!    connection. Other bind failures exit 1 too, with a hint at `--host` when the address
//!    cannot be used or resolved. `--open` (or `[serve] open`)
//!    then opens that URL with the `open` crate (at the loopback address, `127.0.0.1` or
//!    `[::1]`, when the server listens on an unspecified one such as `0.0.0.0`); a failure
//!    to open is a warning. `--open` is not covered by tests.
//! 4. Every change batch of the watcher is rebuilt on the blocking pool (batches that queue
//!    up meanwhile are merged into one rebuild, classified `css` or `content` by
//!    [`classify`]); the result is stored ([`server::apply_build`] or
//!    [`server::apply_error`]), the watch set is updated to the new build's inputs (after a
//!    failure: the failed build's inputs plus those of the last good build) and a
//!    [`BuildEvent`] with the build's duration is broadcast to the browsers. stderr gets
//!    one line per rebuild: `rebuilt in <n> ms (css|content)` (not with `-q`), or the
//!    build's error; diagnostics are printed as `presenter build` prints them. Rebuild
//!    errors never stop the server.
//! 5. Ctrl-C (SIGINT) shuts the server down gracefully ([`server::serve_until`]) and the
//!    command exits 0. The handler is installed before anything else happens (only the
//!    arguments are checked and the async runtime is started before it), so a Ctrl-C
//!    during the first build exits 0 too: the build is abandoned and nothing is bound.
//!
//! The address and the theme flags are read once: a changed `[serve]` section needs a
//! restart, and a build whose `[serve]` address differs from the one the server was
//! started with prints a warning saying so (once per change); every other input (deck,
//! `presenter.toml`, theme, images) is rebuilt live. When `presenter.toml` cannot be
//! loaded at start, the server uses the default `[serve]` settings (with the command
//! line applied) and prints a warning saying so; the first build reports the error.
//!
//! # Versioned deck-asset URLs
//!
//! A file asset's id names its path, not its content, so an edited image keeps its URL
//! `/__presenter/deck-asset/<id>`, and a browser that swaps the slides in place shows the
//! image it already has for that URL. Before a build is stored, every such URL of a file
//! asset therefore gets `?v=<version>`, a number that changes whenever the file's
//! modification time or size does: the swapped slides then load an edited image, and only
//! that one. The route ignores the query, and the client disregards `?v=<digits>` when it
//! compares pages (live-reload.md §3), so neither the protocol nor the client changes; the
//! URL shape differs from live-reload.md §1 by the query only (a contract note for the
//! coordinator). Images that theme templates reference with `asset()` (e.g. the title
//! slide's logo) are served from `/__presenter/asset/…` and are not versioned: editing one
//! is a `css` build, after which an open page keeps showing the old image until a reload.
//!
//! # `--site-preview`
//!
//! Serves each build the way `presenter build --site` lays it out: the document is rendered
//! in site mode exactly as a site build writes it (relative `assets/<name>-<hash>.<ext>`
//! URLs) and served from memory, `GET /` the document and `GET /assets/{name}` the files a
//! site build writes to `assets/` (whole files, no byte ranges). The page has **no**
//! live-reload client, since a site build has none (and the client's stylesheet swap
//! assumes serve-mode URLs): it is still rebuilt on every change, but the browser must be
//! refreshed by hand, and build errors show on stderr only (and in `/__presenter/state`),
//! never in a browser overlay. A note on stderr after the URL line says so (not with `-q`).
//! Before the first good build, `GET /` is the dev server's error page, which reloads into
//! the deck once a build succeeds.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::io::{self, Write as _};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, PoisonError, RwLock, mpsc};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc as tokio_mpsc};

use super::{
    GlobalArgs, ServeArgs, display_path, input_kind, locate, print_stdout, report,
    report_diagnostics,
};
use crate::assets::emit::{OutputMode, SERVE_ASSET_PREFIX};
use crate::assets::registry::{AssetData, ID_LEN};
use crate::config::{CliOverrides, Config, ServeConfig};
use crate::error::{Diagnostic, Diagnostics, Error};
use crate::pipeline::{self, Build, BuildFailure, BuildOptions, SITE_ASSETS_DIR};
use crate::render::BuildInfo;
use crate::serve::events::{BuildEvent, ErrorPayload};
use crate::serve::server::{self, ServeState, Shared};
use crate::serve::watch::{
    ChangeBatch, ChangeKind, WatchSet, Watcher, classify, watch_set_before_build, watch_set_for,
    watch_set_for_failed,
};

/// The files of the latest site-preview build by path below `assets/` (`--site-preview`).
type SiteFiles = Arc<RwLock<BTreeMap<String, Bytes>>>;

/// Runs `presenter serve` (module docs). Returns only on Ctrl-C (exit 0, also during the
/// first build) or when the server cannot start: an input whose format cannot be told
/// (exit 2), the port cannot be bound or the watcher cannot start (exit 1).
pub fn run(args: &ServeArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let opts = serve_options(args, global)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("presenter-serve")
        .build()
        .context("cannot start the server runtime")?;
    let served = runtime.block_on(start(opts, args.site_preview, global));
    // A build still running is abandoned: serve mode writes nothing to disk.
    runtime.shutdown_background();
    served?;
    Ok(ExitCode::SUCCESS)
}

/// Steps 1–5 of the module docs: installs the Ctrl-C handler, starts the watcher, runs the
/// first build on the blocking pool, then serves. Returns `Ok` on Ctrl-C, also when it
/// comes during the first build (the build is then abandoned and nothing is bound).
async fn start(opts: BuildOptions, site_preview: bool, global: &GlobalArgs) -> anyhow::Result<()> {
    // First of all, so that from here on Ctrl-C always ends the command with exit 0.
    let mut shutdown = Box::pin(interrupt().context("cannot listen for Ctrl-C")?);
    let config = serve_config(&opts, global);
    let (tx, batches) = mpsc::channel();
    let watcher = Watcher::start(&watch_set_before_build(&opts), tx)?;
    let mut session = Session::new(opts, site_preview, global.clone(), watcher, &config);
    tracing::info!(
        "first build of {}",
        display_path(&session.opts.input).display()
    );
    let first = tokio::task::spawn_blocking(move || {
        session.build(ChangeKind::Content);
        session
    });
    let session = tokio::select! {
        biased;
        () = &mut shutdown => {
            tracing::info!("interrupted during the first build");
            return Ok(());
        }
        session = first => session.context("the first build stopped")?,
    };
    serve(session, batches, &config, shutdown).await
}

/// The pipeline options of `args` for serve mode: the input and its format (by extension;
/// an unknown one is a usage error), `--config`, `--theme`, `--port`, `--host` and `--open`
/// as overrides, [`OutputMode::Serve`] (site mode with `--site-preview`) and missing assets
/// as warnings. The build id is set per attempt ([`Session::build_info`]).
fn serve_options(args: &ServeArgs, global: &GlobalArgs) -> anyhow::Result<BuildOptions> {
    let output = if args.site_preview {
        // `assets/<name>`, referenced as `assets/<name>`: what `build --site` writes.
        pipeline::output_mode(false, Path::new(""))
    } else {
        OutputMode::Serve
    };
    Ok(BuildOptions {
        format: Some(input_kind(&args.input)?),
        config_path: global.config.clone(),
        cli: CliOverrides {
            theme: args.theme.clone(),
            port: args.port,
            host: args.host.clone(),
            open: args.open.then_some(true),
            ..CliOverrides::default()
        },
        output,
        missing_asset_is_error: false,
        ..BuildOptions::new(&args.input)
    })
}

/// The `[serve]` settings with the command line applied. When `presenter.toml` cannot be
/// loaded, the defaults with the command line applied, and a warning saying so (the first
/// build reports the error itself).
fn serve_config(opts: &BuildOptions, global: &GlobalArgs) -> ServeConfig {
    match pipeline::load_config(opts) {
        Ok((config, _)) => config.serve,
        Err(err) => {
            tracing::debug!("cannot load the configuration: {err}");
            let mut config = Config::default();
            config.apply_cli(&opts.cli);
            let note = format!(
                "the [serve] settings cannot be read (the build error below says why), so \
                 the server listens on {} until it is restarted",
                display_address(&config.serve.host, config.serve.port)
            );
            let mut diagnostics = Diagnostics::new();
            diagnostics.push(Diagnostic::warning(SERVE_CONFIG_CODE, note));
            report_diagnostics(&diagnostics, global);
            config.serve
        }
    }
}

/// The diagnostic code of the warnings about `[serve]` settings that are not in effect.
const SERVE_CONFIG_CODE: &str = "serve-config";

/// Binds the listener, prints the URL, opens the browser if asked, then serves until
/// `shutdown` (Ctrl-C) while [`rebuild_loop`] handles the watcher's batches.
async fn serve(
    session: Session,
    batches: mpsc::Receiver<ChangeBatch>,
    config: &ServeConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let listener = bind(&config.host, config.port).await?;
    let addr = listener
        .local_addr()
        .context("cannot read the server's address")?;
    print_stdout(&format!("Serving {}", url_for(addr)))?;
    if session.site_preview && !session.global.quiet {
        let note = "note: --site-preview pages do not live-reload and show no build errors: \
                    refresh the browser after a rebuild";
        let _ = writeln!(std::io::stderr().lock(), "{note}");
    }
    if config.open {
        open_browser(url_for(browser_addr(addr)));
    }
    let router = session.router();
    let (tx, rx) = tokio_mpsc::unbounded_channel();
    // The watcher sends on a std channel; this thread hands its batches to the async side
    // and ends when the watcher (owned by the rebuild loop) or the loop is gone.
    std::thread::Builder::new()
        .name("presenter-batches".into())
        .spawn(move || {
            for batch in batches {
                if tx.send(batch).is_err() {
                    break;
                }
            }
        })
        .context("cannot start the rebuild thread")?;
    let rebuilds = tokio::spawn(rebuild_loop(session, rx));
    let served = server::serve_until(listener, router, shutdown).await;
    rebuilds.abort();
    tracing::info!("dev server stopped");
    Ok(served?)
}

/// How long [`bind`] waits for a server that may already answer on the address.
const PROBE_TIMEOUT: Duration = Duration::from_millis(300);

/// Binds `host:port` (each address `host` resolves to, in order, until one binds).
///
/// A port that another server already answers on is in use (live-reload.md §4: an error
/// with the hint to use `--port 0`), also when the bind itself succeeds: the listener
/// sets `SO_REUSEADDR` (so that a restart is not refused while connections of the previous
/// run linger in `TIME_WAIT`), and on macOS and the BSDs that lets it bind `127.0.0.1:P`
/// although another server listens on `0.0.0.0:P` or a dual-stack `[::]:P`. So, unless the
/// port is 0, each address is first probed with a connection (an unspecified address at
/// its loopback address). Errors name the address and hint at the fix ([`bind_error`]).
async fn bind(host: &str, port: u16) -> anyhow::Result<TcpListener> {
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|err| {
            let err = io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                format!("cannot resolve {host}: {err}"),
            );
            bind_error(host, port, &err)
        })?
        .collect();
    let taken = if port == 0 {
        None
    } else {
        answering(&addrs).await
    };
    let listener = TcpListener::bind(addrs.as_slice())
        .await
        .map_err(|err| bind_error(host, port, &err))?;
    match taken {
        Some(addr) => {
            let err = io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("another server already answers on {addr}"),
            );
            Err(bind_error(host, port, &err))
        }
        None => Ok(listener),
    }
}

/// The first of `addrs` (an unspecified address at its loopback address) where a server
/// accepts a connection within [`PROBE_TIMEOUT`].
async fn answering(addrs: &[SocketAddr]) -> Option<SocketAddr> {
    for addr in addrs.iter().map(|addr| browser_addr(*addr)) {
        let connect = tokio::net::TcpStream::connect(addr);
        if let Ok(Ok(_)) = tokio::time::timeout(PROBE_TIMEOUT, connect).await {
            return Some(addr);
        }
    }
    None
}

/// The error for a failed bind of `host:port`: names the address, then a hint by the
/// kind of `err`: `--port 0` for a port in use (or one that needs privileges), the host
/// settings for an address that cannot be used here (or resolved).
fn bind_error(host: &str, port: u16, err: &io::Error) -> anyhow::Error {
    let address = display_address(host, port);
    let hint = match err.kind() {
        io::ErrorKind::AddrInUse => Some("use --port 0 to pick a free port"),
        io::ErrorKind::PermissionDenied if port != 0 => Some("use --port 0 to pick a free port"),
        io::ErrorKind::AddrNotAvailable => {
            Some("check --host, or host in [serve] of presenter.toml")
        }
        _ => None,
    };
    match hint {
        Some(hint) => anyhow::anyhow!("cannot listen on {address}: {err} ({hint})"),
        None => anyhow::anyhow!("cannot listen on {address}: {err}"),
    }
}

/// `host:port` for messages (an IPv6 address in brackets).
fn display_address(host: &str, port: u16) -> String {
    match host.parse::<IpAddr>() {
        Ok(ip) => SocketAddr::new(ip, port).to_string(),
        Err(_) => format!("{host}:{port}"),
    }
}

/// A future that completes on Ctrl-C (SIGINT). The handler is installed by this call, not
/// when the future is first polled, so no Ctrl-C after it is missed.
#[cfg(unix)]
fn interrupt() -> std::io::Result<impl Future<Output = ()> + Send + 'static> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigint = signal(SignalKind::interrupt())?;
    Ok(async move {
        sigint.recv().await;
    })
}

/// A future that completes on Ctrl-C. The handler is installed by this call.
#[cfg(windows)]
fn interrupt() -> std::io::Result<impl Future<Output = ()> + Send + 'static> {
    let mut ctrl_c = tokio::signal::windows::ctrl_c()?;
    Ok(async move {
        ctrl_c.recv().await;
    })
}

/// `http://<addr>/` (an IPv6 address in brackets).
fn url_for(addr: SocketAddr) -> String {
    format!("http://{addr}/")
}

/// The address a browser on this machine reaches `addr` at: `addr`, or the loopback
/// address of its family when it is unspecified (`0.0.0.0`, `::`).
fn browser_addr(addr: SocketAddr) -> SocketAddr {
    match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), addr.port())
        }
        IpAddr::V6(ip) if ip.is_unspecified() => {
            SocketAddr::new(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST), addr.port())
        }
        _ => addr,
    }
}

/// Opens `url` in the default browser, off the async threads (launchers can block). A
/// failure is a warning.
fn open_browser(url: String) {
    tokio::task::spawn_blocking(move || {
        if let Err(err) = open::that(&url) {
            tracing::warn!("cannot open {url} in a browser: {err}");
        }
    });
}

/// Receives change batches and rebuilds on the blocking pool, one rebuild at a time; the
/// batches that arrive meanwhile are merged into the next rebuild. Runs until the task is
/// aborted at shutdown (the session's watcher keeps the channel open), or until a rebuild
/// panics outside the build itself (which [`try_build`] turns into a build error).
async fn rebuild_loop(
    mut session: Session,
    mut batches: tokio_mpsc::UnboundedReceiver<ChangeBatch>,
) {
    while let Some(first) = batches.recv().await {
        let mut queued = vec![first];
        while let Ok(more) = batches.try_recv() {
            queued.push(more);
        }
        let job = tokio::task::spawn_blocking(move || {
            session.rebuild(queued);
            session
        });
        session = match job.await {
            Ok(session) => session,
            Err(err) => {
                tracing::error!("live reload stopped: {err}");
                return;
            }
        };
    }
}

/// The batches of `queued` as one: their paths (sorted, without duplicates), classified
/// with `theme_dirs`.
fn merge_batches(queued: Vec<ChangeBatch>, theme_dirs: &[PathBuf]) -> ChangeBatch {
    let paths: BTreeSet<PathBuf> = queued.into_iter().flat_map(|batch| batch.paths).collect();
    let paths: Vec<PathBuf> = paths.into_iter().collect();
    let kind = classify(&paths, theme_dirs);
    ChangeBatch { paths, kind }
}

/// `css` or `content`, as in the build event.
fn kind_name(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Css => "css",
        ChangeKind::Content => "content",
    }
}

/// The served deck and everything a rebuild needs. Owned by one thread at a time: the
/// main thread for the first build, then the rebuild loop (moved into each blocking job).
struct Session {
    /// Serve-mode pipeline options (the build id is set per attempt).
    opts: BuildOptions,
    /// `--site-preview` (module docs).
    site_preview: bool,
    /// `-q` for printing.
    global: GlobalArgs,
    /// What the server serves.
    state: Shared,
    /// The browsers' build events.
    events: broadcast::Sender<BuildEvent>,
    /// The latest site-preview build's files (empty without `--site-preview`).
    site_files: SiteFiles,
    /// Watches the inputs of the latest attempt.
    watcher: Watcher,
    /// What `watcher` watches.
    set: WatchSet,
    /// The inputs of the latest successful build.
    last_good: Option<WatchSet>,
    /// The `[serve]` settings the server was started with (read once).
    listening: ServeConfig,
    /// The last [`restart_note`] printed, so that it is printed once per change.
    restart_note: Option<String>,
}

impl Session {
    /// A session with nothing built yet, for a server started with `listening`.
    fn new(
        opts: BuildOptions,
        site_preview: bool,
        global: GlobalArgs,
        watcher: Watcher,
        listening: &ServeConfig,
    ) -> Self {
        Session {
            opts,
            site_preview,
            global,
            state: Arc::new(RwLock::new(ServeState::default())),
            events: server::event_channel(),
            site_files: SiteFiles::default(),
            watcher,
            set: WatchSet::default(),
            last_good: None,
            listening: listening.clone(),
            restart_note: None,
        }
    }

    /// The dev server's routes, plus `/assets/{*path}` with `--site-preview`.
    fn router(&self) -> Router {
        let router = server::router(Arc::clone(&self.state), self.events.clone());
        if !self.site_preview {
            return router;
        }
        router.merge(
            Router::new()
                .route(&format!("/{SITE_ASSETS_DIR}/{{*path}}"), get(site_file))
                .with_state(Arc::clone(&self.site_files)),
        )
    }

    /// The template context's `build` for attempt `id`: serve mode with live reload, or
    /// with `--site-preview` exactly what `build --site` uses.
    fn build_info(&self, id: u64) -> BuildInfo {
        if self.site_preview {
            BuildInfo::build(false)
        } else {
            BuildInfo::serve(id)
        }
    }

    /// One rebuild for the batches in `queued`: logs the change, builds, prints
    /// `rebuilt in <n> ms (<kind>)` (or the error) and broadcasts the build event.
    fn rebuild(&mut self, queued: Vec<ChangeBatch>) {
        let batch = merge_batches(queued, &self.set.theme_dirs);
        tracing::info!(kind = kind_name(batch.kind), "changed: {:?}", batch.paths);
        let event = self.build(batch.kind);
        if let BuildEvent::Ok {
            kind, duration_ms, ..
        } = &event
            && !self.global.quiet
        {
            let line = format!("rebuilt in {duration_ms} ms ({})", kind_name(*kind));
            let _ = writeln!(std::io::stderr().lock(), "{line}");
        }
        server::publish(&self.events, event);
    }

    /// Builds the deck as the next attempt, stores the result, watches the attempt's
    /// inputs and prints its diagnostics (and error). Returns the event to broadcast; a
    /// successful build reports `kind`.
    fn build(&mut self, kind: ChangeKind) -> BuildEvent {
        let opts = BuildOptions {
            build: self.build_info(server::next_build_id(&self.state)),
            ..self.opts.clone()
        };
        let start = Instant::now();
        let result = try_build(&opts);
        let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        match result {
            Ok(build) => {
                let build_id = self.store(build);
                BuildEvent::Ok {
                    build_id,
                    kind,
                    duration_ms,
                }
            }
            Err(BuildFailure { error, diagnostics }) => {
                let build_id = server::apply_error(&self.state, &error);
                let payload = ErrorPayload::from(&error);
                let mut set = watch_set_for_failed(&self.opts);
                if let Some(good) = &self.last_good {
                    set.merge(good);
                }
                self.watch(set);
                report_diagnostics(&diagnostics, &self.global);
                report::report_error(&locate(error, &self.opts.input));
                BuildEvent::Error {
                    build_id,
                    error: payload,
                }
            }
        }
    }

    /// Stores a successful build (and its site files), watches its inputs and prints its
    /// diagnostics. Returns its id.
    fn store(&mut self, mut build: Build) -> u64 {
        if self.site_preview {
            let files = std::mem::take(&mut build.document.files)
                .into_iter()
                .filter_map(|file| Some((site_path(&file.path)?, Bytes::from(file.bytes))))
                .collect();
            // Before the document, so that the files of a served document always exist.
            *self
                .site_files
                .write()
                .unwrap_or_else(PoisonError::into_inner) = files;
        } else {
            version_file_assets(&mut build);
        }
        let build_id = server::apply_build(&self.state, &build);
        let set = watch_set_for(&build);
        self.last_good = Some(set.clone());
        self.watch(set);
        report_diagnostics(&build.diagnostics, &self.global);
        self.note_restart(&build);
        build_id
    }

    /// Warns (once per change) when the `[serve]` address of `build`'s configuration is not
    /// the one the server listens on: it is read only at start.
    fn note_restart(&mut self, build: &Build) {
        let note = restart_note(&self.listening, &build.config.serve);
        if let Some(text) = note
            .as_ref()
            .filter(|text| self.restart_note.as_ref() != Some(*text))
        {
            let mut diagnostic = Diagnostic::warning(SERVE_CONFIG_CODE, text.clone());
            if let Some(path) = &build.config_path {
                diagnostic = diagnostic.with_path(path);
            }
            let mut diagnostics = Diagnostics::new();
            diagnostics.push(diagnostic);
            report_diagnostics(&diagnostics, &self.global);
        }
        self.restart_note = note;
    }

    /// Has the watcher watch `set`; a directory that cannot be watched is a warning.
    fn watch(&mut self, set: WatchSet) {
        if let Err(err) = self.watcher.update(&set) {
            tracing::warn!("{err}");
        }
        self.set = set;
    }
}

/// [`pipeline::try_build_deck`], with a panic of the build turned into an error: a bug in
/// one build must not end the server.
fn try_build(opts: &BuildOptions) -> Result<Build, BuildFailure> {
    std::panic::catch_unwind(AssertUnwindSafe(|| pipeline::try_build_deck(opts))).unwrap_or_else(
        |panic| {
            let message = panic
                .downcast_ref::<&str>()
                .map(|text| text.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            Err(BuildFailure::from(Error::Other(format!(
                "the build panicked (a bug in presenter): {message}"
            ))))
        },
    )
}

/// The warning for a configuration whose `[serve]` address (`configured`) is not the one
/// the server listens on (`listening`), else `None`.
fn restart_note(listening: &ServeConfig, configured: &ServeConfig) -> Option<String> {
    let now = display_address(&listening.host, listening.port);
    let wanted = display_address(&configured.host, configured.port);
    (now != wanted).then(|| {
        format!(
            "[serve] now asks for {wanted}, but the server was started with {now}: restart \
             presenter serve to apply it"
        )
    })
}

/// Appends `?v=<version>` ([`file_version`]) to every `/__presenter/deck-asset/<id>` URL
/// of a file asset in the serve-mode document of `build` (module docs), in one pass over
/// the document. A file that cannot be read is left alone (the asset route answers 404
/// for it).
fn version_file_assets(build: &mut Build) {
    let versions: BTreeMap<&str, u64> = build
        .assets
        .iter()
        .filter_map(|asset| match &asset.data {
            AssetData::File(path) => Some((asset.id.as_str(), file_version(path)?)),
            _ => None,
        })
        .collect();
    if !versions.is_empty() {
        build.document.html = versioned_urls(&build.document.html, &versions);
    }
}

/// `html` with `?v=<version>` after each `/__presenter/deck-asset/<id>` whose id (always
/// [`ID_LEN`] characters) has a version in `versions`.
fn versioned_urls(html: &str, versions: &BTreeMap<&str, u64>) -> String {
    let mut out = String::with_capacity(html.len() + 24 * versions.len());
    let mut rest = html;
    while let Some(at) = rest.find(SERVE_ASSET_PREFIX) {
        let id_start = at + SERVE_ASSET_PREFIX.len();
        let id = rest.get(id_start..id_start + ID_LEN);
        let end = id.map_or(id_start, |id| id_start + id.len());
        out.push_str(&rest[..end]);
        if let Some(version) = id.and_then(|id| versions.get(id)) {
            out.push_str("?v=");
            out.push_str(&version.to_string());
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// A number that changes when the file at `path` is rewritten: a hash of its modification
/// time and size (decimal digits only, as the client expects of `?v=`).
fn file_version(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    let mut hasher = DefaultHasher::new();
    meta.modified().ok()?.hash(&mut hasher);
    meta.len().hash(&mut hasher);
    Some(hasher.finish())
}

/// The URL path below `/assets/` of a site file written to `assets/<path>`; `None` for a
/// file outside it (site builds write none).
fn site_path(path: &Path) -> Option<String> {
    let relative = path.strip_prefix(SITE_ASSETS_DIR).ok()?;
    let parts: Option<Vec<&str>> = relative.iter().map(|part| part.to_str()).collect();
    let parts = parts?;
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// `GET /assets/{*path}` with `--site-preview`: the file of the latest build, else 404.
async fn site_file(State(files): State<SiteFiles>, UrlPath(path): UrlPath<String>) -> Response {
    let bytes = files
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&path)
        .cloned();
    match bytes {
        Some(bytes) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            let headers = [
                (header::CONTENT_TYPE, mime.essence_str().to_string()),
                (header::CACHE_CONTROL, "no-store".to_string()),
            ];
            (headers, bytes).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            format!("not found: /{SITE_ASSETS_DIR}/{path}\n"),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv6Addr;

    use super::*;

    #[test]
    fn bind_errors_name_the_address_and_hint_by_kind() {
        let in_use = io::Error::from(io::ErrorKind::AddrInUse);
        let message = bind_error("127.0.0.1", 3000, &in_use).to_string();
        assert!(
            message.starts_with("cannot listen on 127.0.0.1:3000: "),
            "{message}"
        );
        assert!(
            message.ends_with("(use --port 0 to pick a free port)"),
            "{message}"
        );
        assert!(
            bind_error("::1", 80, &in_use)
                .to_string()
                .contains("[::1]:80")
        );
        assert!(
            bind_error("localhost", 80, &in_use)
                .to_string()
                .contains("localhost:80")
        );
        // A privileged port: --port 0 helps; with port 0 already, it does not.
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        assert!(
            bind_error("127.0.0.1", 80, &denied)
                .to_string()
                .contains("--port 0")
        );
        assert!(
            !bind_error("127.0.0.1", 0, &denied)
                .to_string()
                .contains("--port 0")
        );
        // The host is the problem: point at the host settings, not at the port.
        let elsewhere = io::Error::from(io::ErrorKind::AddrNotAvailable);
        let message = bind_error("192.0.2.1", 0, &elsewhere).to_string();
        assert!(
            message.ends_with("(check --host, or host in [serve] of presenter.toml)"),
            "{message}"
        );
        assert!(!message.contains("--port 0"), "{message}");
        let other = io::Error::other("boom");
        assert_eq!(
            bind_error("127.0.0.1", 3000, &other).to_string(),
            "cannot listen on 127.0.0.1:3000: boom"
        );
    }

    #[tokio::test]
    async fn a_port_in_use_is_a_bind_error() {
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        let err = bind("127.0.0.1", port).await.unwrap_err();
        assert!(err.to_string().contains("--port 0"), "{err}");
        let free = bind("127.0.0.1", 0).await.unwrap();
        assert_ne!(free.local_addr().unwrap().port(), 0);
    }

    /// With `SO_REUSEADDR` (which the listener sets), macOS lets `127.0.0.1:P` be bound
    /// while another server listens on `0.0.0.0:P` or a dual-stack `[::]:P`; the probe in
    /// `bind` reports the port as in use anyway (on Linux the bind itself fails).
    #[tokio::test]
    async fn a_port_held_on_a_wildcard_address_is_in_use() {
        let v4 = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let port = v4.local_addr().unwrap().port();
        let err = bind("127.0.0.1", port).await.unwrap_err().to_string();
        assert!(
            err.starts_with(&format!("cannot listen on 127.0.0.1:{port}: ")),
            "{err}"
        );
        assert!(err.ends_with("(use --port 0 to pick a free port)"), "{err}");
        drop(v4);
        // A dual-stack [::] listener, where the system has IPv6 and makes it dual-stack.
        let Ok(v6) = std::net::TcpListener::bind("[::]:0") else {
            return;
        };
        let port = v6.local_addr().unwrap().port();
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            let err = bind("127.0.0.1", port).await.unwrap_err().to_string();
            assert!(err.contains("--port 0"), "{err}");
        }
    }

    /// The probe does not mistake a port whose previous server is gone, with connections
    /// left in `TIME_WAIT` on the server side, for one in use (a restart on the same port).
    #[tokio::test]
    async fn a_port_left_in_time_wait_can_be_bound_again() {
        let first = bind("127.0.0.1", 0).await.unwrap();
        let addr = first.local_addr().unwrap();
        let client = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (accepted, _) = first.accept().await.unwrap();
        // The server side closes first, so its end of the connection enters TIME_WAIT.
        drop(accepted);
        drop(first);
        drop(client);
        let again = bind("127.0.0.1", addr.port()).await.unwrap();
        assert_eq!(again.local_addr().unwrap(), addr);
    }

    #[tokio::test]
    async fn an_address_of_another_machine_is_a_host_error() {
        // TEST-NET-1 (RFC 5737) is no address of this machine; port 0 skips the probe.
        let err = bind("192.0.2.1", 0).await.unwrap_err().to_string();
        assert!(err.starts_with("cannot listen on 192.0.2.1:0: "), "{err}");
        assert!(!err.contains("--port 0"), "{err}");
    }

    #[test]
    fn a_changed_serve_address_needs_a_restart() {
        let started = ServeConfig::default();
        assert_eq!(restart_note(&started, &ServeConfig::default()), None);
        let open = ServeConfig {
            open: true,
            ..ServeConfig::default()
        };
        assert_eq!(
            restart_note(&started, &open),
            None,
            "only the address counts"
        );
        let moved = ServeConfig {
            port: 4000,
            ..ServeConfig::default()
        };
        assert_eq!(
            restart_note(&started, &moved).as_deref(),
            Some(
                "[serve] now asks for 127.0.0.1:4000, but the server was started with \
                 127.0.0.1:3000: restart presenter serve to apply it"
            )
        );
    }

    #[test]
    fn versioned_urls_take_one_pass() {
        let a = "0123456789ab";
        let b = "ba9876543210";
        let versions = BTreeMap::from([(a, 7_u64), (b, 42)]);
        let html = format!(
            "<img src=\"/__presenter/deck-asset/{a}\"><img src=\"/__presenter/deck-asset/{b}\">\
             <a href=\"/__presenter/deck-asset/{a}\">ä</a>\
             <img src=\"/__presenter/deck-asset/ffffffffffff\">/__presenter/deck-asset/ä"
        );
        assert_eq!(
            versioned_urls(&html, &versions),
            format!(
                "<img src=\"/__presenter/deck-asset/{a}?v=7\"><img src=\"/__presenter/deck-asset/{b}?v=42\">\
                 <a href=\"/__presenter/deck-asset/{a}?v=7\">ä</a>\
                 <img src=\"/__presenter/deck-asset/ffffffffffff\">/__presenter/deck-asset/ä"
            )
        );
        assert_eq!(versioned_urls("no assets", &versions), "no assets");
        assert_eq!(
            versioned_urls("/__presenter/deck-asset/0123", &versions),
            "/__presenter/deck-asset/0123"
        );
    }

    #[test]
    fn urls_and_browser_addresses() {
        let v4 = SocketAddr::from(([127, 0, 0, 1], 3000));
        assert_eq!(url_for(v4), "http://127.0.0.1:3000/");
        let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, 8080));
        assert_eq!(url_for(v6), "http://[::1]:8080/");
        assert_eq!(browser_addr(SocketAddr::from(([0, 0, 0, 0], 3000))), v4);
        assert_eq!(
            browser_addr(SocketAddr::from((Ipv6Addr::UNSPECIFIED, 8080))),
            v6
        );
        let lan = SocketAddr::from(([192, 168, 1, 5], 3000));
        assert_eq!(browser_addr(lan), lan);
    }

    #[test]
    fn queued_batches_merge_and_are_classified_again() {
        let theme = PathBuf::from("/deck/theme");
        let batch = |paths: &[&str], kind| ChangeBatch {
            paths: paths.iter().map(PathBuf::from).collect(),
            kind,
        };
        let merged = merge_batches(
            vec![
                batch(&["/deck/theme/styles/b.css"], ChangeKind::Css),
                batch(
                    &["/deck/theme/styles/a.css", "/deck/theme/styles/b.css"],
                    ChangeKind::Css,
                ),
            ],
            std::slice::from_ref(&theme),
        );
        assert_eq!(
            merged.paths,
            vec![
                PathBuf::from("/deck/theme/styles/a.css"),
                PathBuf::from("/deck/theme/styles/b.css")
            ]
        );
        assert_eq!(merged.kind, ChangeKind::Css);
        let mixed = merge_batches(
            vec![
                batch(&["/deck/theme/styles/a.css"], ChangeKind::Css),
                batch(&["/deck/slides.md"], ChangeKind::Content),
            ],
            &[theme],
        );
        assert_eq!(mixed.kind, ChangeKind::Content);
        assert_eq!(kind_name(ChangeKind::Css), "css");
        assert_eq!(kind_name(ChangeKind::Content), "content");
    }

    #[test]
    fn file_asset_urls_change_when_the_file_does() {
        let dir = tempfile::tempdir().unwrap();
        let deck = dir.path().join("a.md");
        let image = dir.path().join("p.png");
        std::fs::write(&deck, "# A\n\n![one](p.png)\n\n![two](p.png)\n").unwrap();
        std::fs::write(&image, b"\x89PNG one").unwrap();
        let built = || {
            pipeline::build_deck(&BuildOptions {
                output: OutputMode::Serve,
                build: BuildInfo::serve(1),
                missing_asset_is_error: false,
                ..BuildOptions::new(&deck)
            })
            .unwrap()
        };
        let urls = |html: &str| -> Vec<String> {
            let pattern =
                regex::Regex::new(r"/__presenter/deck-asset/[0-9a-f]{12}[^\x22]*").unwrap();
            pattern
                .find_iter(html)
                .map(|m| m.as_str().to_string())
                .collect()
        };
        let mut build = built();
        let id = build.assets.iter().next().unwrap().id.clone();
        assert_eq!(
            urls(&build.document.html),
            vec![format!("/__presenter/deck-asset/{id}"); 2]
        );
        version_file_assets(&mut build);
        let first = urls(&build.document.html);
        assert_eq!(first.len(), 2);
        assert_eq!(first[0], first[1]);
        let version = first[0]
            .strip_prefix(&format!("/__presenter/deck-asset/{id}?v="))
            .unwrap();
        assert!(version.bytes().all(|b| b.is_ascii_digit()), "{version}");
        // Same file: same URL.
        let mut again = built();
        version_file_assets(&mut again);
        assert_eq!(urls(&again.document.html), first);
        // Rewritten (another size): another URL.
        std::fs::write(&image, b"\x89PNG two, longer").unwrap();
        let mut edited = built();
        version_file_assets(&mut edited);
        assert_ne!(urls(&edited.document.html)[0], first[0]);
        // A file that cannot be read has no version (its URL is left as rendered).
        std::fs::remove_file(&image).unwrap();
        assert_eq!(file_version(&image), None);
    }

    #[test]
    fn site_files_are_served_below_assets() {
        assert_eq!(
            site_path(Path::new("assets/theme-0123.css")).as_deref(),
            Some("theme-0123.css")
        );
        assert_eq!(
            site_path(Path::new("assets/sub/x.png")).as_deref(),
            Some("sub/x.png")
        );
        assert_eq!(site_path(Path::new("assets")), None);
        assert_eq!(site_path(Path::new("other/x.png")), None);
    }

    #[test]
    fn try_build_passes_build_errors_through() {
        // `try_build` only adds the panic guard; a real build error passes through.
        let opts = BuildOptions::new("/no/such/deck.md");
        let failure = try_build(&opts).unwrap_err();
        assert!(
            failure.error.to_string().contains("deck.md"),
            "{}",
            failure.error
        );
    }

    #[test]
    fn serve_options_use_serve_mode_and_keep_missing_assets_warnings() {
        let args = ServeArgs {
            input: PathBuf::from("talk.ipynb"),
            theme: Some("./t".into()),
            port: Some(0),
            host: None,
            open: false,
            site_preview: false,
        };
        let opts = serve_options(&args, &GlobalArgs::default()).unwrap();
        assert_eq!(opts.output, OutputMode::Serve);
        assert_eq!(opts.format, Some(crate::ir::SourceKind::Notebook));
        assert!(!opts.missing_asset_is_error);
        assert_eq!(opts.cli.theme.as_deref(), Some("./t"));
        assert_eq!(opts.cli.port, Some(0));
        assert_eq!(opts.cli.open, None);
        let config = serve_config(&opts, &GlobalArgs::default());
        assert_eq!((config.host.as_str(), config.port), ("127.0.0.1", 0));

        let site = ServeArgs {
            input: PathBuf::from("deck.md"),
            open: true,
            site_preview: true,
            ..args
        };
        let opts = serve_options(&site, &GlobalArgs::default()).unwrap();
        assert_eq!(
            opts.output,
            OutputMode::Site {
                assets_dir: PathBuf::from(SITE_ASSETS_DIR),
                url_prefix: format!("{SITE_ASSETS_DIR}/"),
            }
        );
        assert_eq!(opts.cli.open, Some(true));

        let unknown = ServeArgs {
            input: PathBuf::from("deck.txt"),
            ..site
        };
        let err = serve_options(&unknown, &GlobalArgs::default()).unwrap_err();
        assert_eq!(super::super::exit_code_for(&err), super::super::EXIT_USAGE);
    }
}
