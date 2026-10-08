//! `presenter serve` (cli.md `serve`, live-reload.md §4–§5): builds the deck, serves it,
//! watches its inputs and live-reloads every open browser.
//!
//! # Lifecycle
//!
//! 1. The file watcher ([`crate::serve::watch::Watcher`]) starts on
//!    [`watch_set_before_build`], so that a change saved during the first build is not lost.
//! 2. The first build (id 1, serve mode, missing assets are warnings) runs **before** the
//!    port is bound, so `GET /` is never empty. A failed first build does not stop the
//!    command: its error is printed and stored ([`server::apply_error`]; `GET /` then shows
//!    the error overlay) and the failed build's inputs are watched until it is fixed.
//! 3. The server binds `host:port` (`--host`/`--port`, else `[serve]` of `presenter.toml`,
//!    else `127.0.0.1:3000`; port 0 picks a free one) and prints
//!    `Serving http://<address>:<port>/` as the **first line on stdout** (always, also with
//!    `-q`: with port 0 it is the only place the port is shown). A bind failure ends the
//!    command with exit 1 and the hint to use `--port 0`. `--open` (or `[serve] open`) then
//!    opens that URL with the `open` crate (`localhost` for an unspecified address such as
//!    `0.0.0.0`); a failure to open is a warning. `--open` is not covered by tests.
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
//!    command exits 0. The handler is installed before the URL is printed, so a Ctrl-C
//!    after that line always takes this path.
//!
//! The address and the theme flags are read once: a changed `[serve]` section needs a
//! restart; every other input (deck, `presenter.toml`, theme, images) is rebuilt live.
//!
//! # Versioned deck-asset URLs
//!
//! A file asset's id names its path, not its content, so an edited image keeps its URL
//! `/__presenter/deck-asset/<id>`, and a browser that swaps the slides in place shows the
//! image it already has for that URL. Before a build is stored, every such URL of a file
//! asset therefore gets `?v=<version>`, a number that changes whenever the file's
//! modification time or size does: the swapped slides then load an edited image, and only
//! that one. The route ignores the query, and the client disregards `?v=<digits>` when it
//! compares pages (live-reload.md §3), so neither the protocol nor the client changes.
//!
//! # `--site-preview`
//!
//! Serves each build the way `presenter build --site` lays it out: the document is rendered
//! in site mode exactly as a site build writes it (relative `assets/<name>-<hash>.<ext>`
//! URLs) and served from memory, `GET /` the document and `GET /assets/{name}` the files a
//! site build writes to `assets/` (whole files, no byte ranges). The page has **no**
//! live-reload client, since a site build has none (and the client's stylesheet swap
//! assumes serve-mode URLs): it is still rebuilt on every change, but the browser must be
//! refreshed by hand, and build errors show on stderr only (and in `/__presenter/state`).
//! Before the first good build, `GET /` is the dev server's error page, which reloads into
//! the deck once a build succeeds.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::io::Write as _;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, PoisonError, RwLock, mpsc};
use std::time::Instant;

use anyhow::Context as _;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc as tokio_mpsc};

use super::{GlobalArgs, ServeArgs, input_kind, locate, print_stdout, report, report_diagnostics};
use crate::assets::emit::{OutputMode, SERVE_ASSET_PREFIX};
use crate::assets::registry::AssetData;
use crate::config::{CliOverrides, Config, ServeConfig};
use crate::error::Error;
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

/// Runs `presenter serve` (module docs). Returns only on Ctrl-C (exit 0) or when the
/// server cannot start: an input whose format cannot be told (exit 2), the port cannot be
/// bound or the watcher cannot start (exit 1).
pub fn run(args: &ServeArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let opts = serve_options(args, global)?;
    let config = serve_config(&opts);
    let (tx, batches) = mpsc::channel();
    let watcher = Watcher::start(&watch_set_before_build(&opts), tx)?;
    let mut session = Session::new(opts, args.site_preview, global.clone(), watcher);
    session.build(ChangeKind::Content);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("presenter-serve")
        .build()
        .context("cannot start the server runtime")?;
    let served = runtime.block_on(serve(session, batches, &config));
    // A rebuild still running is abandoned: serve mode writes nothing to disk.
    runtime.shutdown_background();
    served?;
    Ok(ExitCode::SUCCESS)
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
/// loaded, the defaults with the command line applied (the first build reports the error).
fn serve_config(opts: &BuildOptions) -> ServeConfig {
    match pipeline::load_config(opts) {
        Ok((config, _)) => config.serve,
        Err(err) => {
            tracing::debug!("serving with the default [serve] settings: {err}");
            let mut config = Config::default();
            config.apply_cli(&opts.cli);
            config.serve
        }
    }
}

/// Binds the listener, prints the URL, opens the browser if asked, then serves until
/// Ctrl-C while [`rebuild_loop`] handles the watcher's batches.
async fn serve(
    session: Session,
    batches: mpsc::Receiver<ChangeBatch>,
    config: &ServeConfig,
) -> anyhow::Result<()> {
    let listener = bind(&config.host, config.port).await?;
    let shutdown = interrupt().context("cannot listen for Ctrl-C")?;
    let addr = listener
        .local_addr()
        .context("cannot read the server's address")?;
    print_stdout(&format!("Serving {}", url_for(addr)))?;
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

/// Binds `host:port`. Errors name the address and suggest `--port 0` (live-reload.md §4).
async fn bind(host: &str, port: u16) -> anyhow::Result<TcpListener> {
    TcpListener::bind((host, port))
        .await
        .map_err(|err| bind_error(host, port, &err))
}

/// The error for a failed bind of `host:port`.
fn bind_error(host: &str, port: u16, err: &std::io::Error) -> anyhow::Error {
    let address = match host.parse::<IpAddr>() {
        Ok(ip) => SocketAddr::new(ip, port).to_string(),
        Err(_) => format!("{host}:{port}"),
    };
    anyhow::anyhow!("cannot listen on {address}: {err} (use --port 0 to pick a free port)")
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
/// batches that arrive meanwhile are merged into the next rebuild. Ends when the watcher
/// stops (or the task is aborted on shutdown).
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
    tracing::warn!("the file watcher stopped; live reload is off");
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
}

impl Session {
    /// A session with nothing built yet.
    fn new(opts: BuildOptions, site_preview: bool, global: GlobalArgs, watcher: Watcher) -> Self {
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
        build_id
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

/// Appends `?v=<version>` ([`file_version`]) to every `/__presenter/deck-asset/<id>` URL
/// of a file asset in the serve-mode document of `build` (module docs). Ids have a fixed
/// length, so one id's URL is never the start of another's. A file that cannot be read is
/// left alone (the asset route answers 404 for it).
fn version_file_assets(build: &mut Build) {
    for asset in build.assets.iter() {
        let AssetData::File(path) = &asset.data else {
            continue;
        };
        let Some(version) = file_version(path) else {
            continue;
        };
        let url = format!("{SERVE_ASSET_PREFIX}{}", asset.id);
        if build.document.html.contains(&url) {
            build.document.html = build
                .document
                .html
                .replace(&url, &format!("{url}?v={version}"));
        }
    }
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
    fn bind_errors_name_the_address_and_suggest_port_0() {
        let err = std::io::Error::from(std::io::ErrorKind::AddrInUse);
        let message = bind_error("127.0.0.1", 3000, &err).to_string();
        assert!(
            message.starts_with("cannot listen on 127.0.0.1:3000: "),
            "{message}"
        );
        assert!(
            message.ends_with("(use --port 0 to pick a free port)"),
            "{message}"
        );
        assert!(bind_error("::1", 80, &err).to_string().contains("[::1]:80"));
        assert!(
            bind_error("localhost", 80, &err)
                .to_string()
                .contains("localhost:80")
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
        let config = serve_config(&opts);
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
