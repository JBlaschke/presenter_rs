//! File watcher, debouncing, rebuild loop and change classification (live-reload.md §2,
//! §5). Implemented by T20; [`ChangeKind`] is the T00 contract type shared with the dev
//! server (T21).
//!
//! # What is watched
//!
//! A [`WatchSet`] lists the inputs of one build ([`watch_set_for`], or
//! [`watch_set_for_failed`] when the build failed): the source file, the `presenter.toml`
//! that was used, every file asset (and every missing one), each disk theme of the chain
//! and, in a debug build, `assets/themes/default/` of this crate (rust-embed reads it from
//! disk then). All paths are canonical (`/tmp/x` on macOS is `/private/tmp/x`), so they
//! compare equal to the paths the OS reports.
//!
//! Files are watched through their **parent directory**, non-recursively: editors that
//! save by writing a temporary file and renaming it over the original (vim, VS Code "safe
//! write") replace the file, and a watch on the file itself would be lost. Events in those
//! directories count only for the listed files. Theme directories are watched recursively
//! and every path below them counts, except the [ignore list](is_ignored) (`.git/`,
//! `target/`, `dist/`, editor backup and swap files).
//!
//! # Debouncing and batches
//!
//! [`Watcher`] runs `notify-debouncer-full` with a 150 ms timeout ([`DEBOUNCE`]), which
//! emits each path once it has been quiet that long. A coalescing thread then gathers what
//! the debouncer emits until it has been quiet for another [`SETTLE`] (at most
//! [`MAX_BATCH_WAIT`] in total), so a save touching several files (or a `git checkout`)
//! becomes **one** [`ChangeBatch`]: its relevant paths, sorted, and their [`ChangeKind`]
//! ([`classify`]). Any event kind counts as a change (create, modify, remove, rename,
//! metadata) except a mere access: inotify reports every `open()`, and the build itself
//! opens its inputs. When the backend lost events (a rescan), the batch lists every
//! watched file. A watched directory that is created or removed (deleted and recreated,
//! e.g. by `git checkout`) counts as a change too, and the next [`Watcher::update`]
//! watches it again.
//!
//! # Threads
//!
//! [`Watcher::start`] starts the backend's thread (FSEvents run loop or inotify), the
//! debouncer's timer thread and the coalescing thread `presenter-watch`; batches are sent
//! from the latter on the `Sender` given to [`Watcher::start`]. The receiving side decides
//! where to rebuild: `presenter build --watch` blocks on it ([`run_build_watch`]); an async
//! server should receive on a dedicated thread (or `spawn_blocking`) and forward to its
//! rebuild task. [`Watcher`] is `Send`; keep it alive as long as changes are wanted
//! (dropping it stops all three threads; the coalescing thread then flushes what it holds
//! and drops the `Sender`) and call [`Watcher::update`] with the inputs of every build,
//! from the thread that owns it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use notify_debouncer_full::notify::{self, EventKind, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};

use crate::assets::registry::{AssetData, AssetRegistry, normalize_lexically};
use crate::error::{Diagnostics, Error, Result};
use crate::ir::{Deck, DeckMeta, Features, SourceInfo, SourceKind};
use crate::markdown::render::missing_asset_path;
use crate::pipeline::{self, Build, BuildFailure, BuildOptions};

/// The debounce timeout handed to `notify-debouncer-full` (live-reload.md §5).
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// How long the debouncer must stay quiet before the paths it emitted become a batch. The
/// debouncer emits each path separately once that path is quiet, so writes to two files
/// 50 ms apart come out up to ~90 ms apart (50 ms plus its 37.5 ms tick).
pub const SETTLE: Duration = DEBOUNCE;

/// A batch is sent at the latest this long after its first path, even while changes keep
/// arriving.
pub const MAX_BATCH_WAIT: Duration = Duration::from_secs(2);

/// What kind of rebuild a change batch requires (live-reload.md §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    /// Only stylesheets/theme assets changed.
    Css,
    /// Anything else.
    Content,
}

/// The inputs of one build, as the [`Watcher`] watches them (module docs). Build it with
/// [`watch_set_for`], [`watch_set_for_failed`] or [`WatchSet::new`]; the watcher
/// canonicalizes whatever it is given, so a hand-written set works too.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchSet {
    /// Files whose changes count: the source, `presenter.toml`, the deck's file assets
    /// (including missing ones, so that creating them rebuilds).
    pub files: Vec<PathBuf>,
    /// Directories watched recursively; every change below them counts except the ignore
    /// list. These are the theme directories.
    pub dirs_recursive: Vec<PathBuf>,
    /// Directories watched non-recursively, one per directory: the parents of `files`.
    /// Events there count only for `files` (the watcher also watches the parent of a file
    /// whose directory is missing from this list).
    pub dirs_flat: Vec<PathBuf>,
    /// The theme directories of the chain (also watched recursively), for [`classify`].
    pub theme_dirs: Vec<PathBuf>,
}

impl WatchSet {
    /// A set watching `files` (through their parent directories) and the theme directories
    /// `theme_dirs` (recursively). Every path is made canonical; the lists are sorted and
    /// without duplicates.
    pub fn new(
        files: impl IntoIterator<Item = PathBuf>,
        theme_dirs: impl IntoIterator<Item = PathBuf>,
    ) -> WatchSet {
        let files = canonical_set(files);
        let theme_dirs = canonical_set(theme_dirs);
        let dirs_flat: BTreeSet<PathBuf> = files
            .iter()
            .filter_map(|file| file.parent().map(Path::to_path_buf))
            .collect();
        WatchSet {
            files: files.into_iter().collect(),
            dirs_recursive: theme_dirs.iter().cloned().collect(),
            dirs_flat: dirs_flat.into_iter().collect(),
            theme_dirs: theme_dirs.into_iter().collect(),
        }
    }

    /// Adds every path of `other` (each list stays sorted and without duplicates).
    pub fn merge(&mut self, other: &WatchSet) {
        let union = |mine: &mut Vec<PathBuf>, theirs: &[PathBuf]| {
            let all: BTreeSet<PathBuf> = mine.drain(..).chain(theirs.iter().cloned()).collect();
            mine.extend(all);
        };
        union(&mut self.files, &other.files);
        union(&mut self.dirs_recursive, &other.dirs_recursive);
        union(&mut self.dirs_flat, &other.dirs_flat);
        union(&mut self.theme_dirs, &other.theme_dirs);
    }
}

/// The inputs of a successful build (live-reload.md §5): the source file, the
/// `presenter.toml` that was used, every file asset and every asset reported missing, the
/// chain's disk themes and, in a debug build, this crate's `assets/themes/default/`.
pub fn watch_set_for(build: &Build) -> WatchSet {
    let mut files = vec![build.deck.source.path.clone()];
    files.extend(build.config_path.iter().cloned());
    files.extend(asset_files(&build.assets, &build.diagnostics));
    WatchSet::new(files, with_builtin_theme(build.theme.watch_dirs()))
}

/// The inputs of a failed build, as far as they are known: the source file, the
/// `presenter.toml` (`--config`, else the one discovery finds, even when it is invalid)
/// and, in a debug build, this crate's `assets/themes/default/`. When the source still
/// parses (the build failed later: a missing asset, the theme, a template), also the
/// deck's file assets including the missing ones, and the theme chain's directories if
/// the theme resolves; when it does not parse, the theme named by `--theme` or
/// `presenter.toml` if that resolves.
pub fn watch_set_for_failed(opts: &BuildOptions) -> WatchSet {
    let mut files = vec![opts.input.clone()];
    let mut themes = Vec::new();
    match pipeline::parse_only(opts) {
        Ok((parsed, config, config_path)) => {
            files.extend(asset_files(&parsed.assets, &parsed.diagnostics));
            if let Ok(theme) =
                pipeline::resolve_theme(opts, &parsed.deck, &config, config_path.as_deref())
            {
                themes = theme.watch_dirs();
            }
            files.extend(config_path);
        }
        Err(err) => {
            tracing::debug!("watching a deck that does not parse: {err}");
            match pipeline::load_config(opts) {
                Ok((config, config_path)) => {
                    // The front matter is unknown; resolve `--theme` / `[deck] theme`
                    // exactly like the pipeline does, with a deck that has no meta.
                    let deck = placeholder_deck(opts);
                    if let Ok(theme) =
                        pipeline::resolve_theme(opts, &deck, &config, config_path.as_deref())
                    {
                        themes = theme.watch_dirs();
                    }
                    files.extend(config_path);
                }
                Err(_) => files.extend(config_file(opts)),
            }
        }
    }
    WatchSet::new(files, with_builtin_theme(themes))
}

/// The files of `assets` and the missing files named by `missing-asset` diagnostics.
fn asset_files<'a>(
    assets: &'a AssetRegistry,
    diagnostics: &'a Diagnostics,
) -> impl Iterator<Item = PathBuf> + 'a {
    let present = assets.iter().filter_map(|asset| match &asset.data {
        AssetData::File(path) => Some(path.clone()),
        AssetData::Bytes(_) => None,
    });
    present.chain(diagnostics.iter().filter_map(missing_asset_path))
}

/// `dirs` plus this crate's `assets/themes/default/` in a debug build, where rust-embed
/// reads the built-in theme from disk (live-reload.md §5).
fn with_builtin_theme(mut dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    if cfg!(debug_assertions) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/themes/default");
        if dir.is_dir() {
            dirs.push(dir);
        }
    }
    dirs
}

/// The `presenter.toml` a build of `opts` uses, found without parsing it: `--config`, else
/// the first one walking up from the input's directory.
fn config_file(opts: &BuildOptions) -> Option<PathBuf> {
    if let Some(path) = &opts.config_path {
        return Some(path.clone());
    }
    let input = std::path::absolute(&opts.input).ok()?;
    input
        .parent()?
        .ancestors()
        .map(|dir| dir.join(crate::config::CONFIG_FILE_NAME))
        .find(|candidate| candidate.is_file())
}

/// An empty deck for `opts.input`, so that [`pipeline::resolve_theme`] can resolve the
/// theme of a deck that does not parse (it reads only `meta.theme` and `source.base_dir`).
fn placeholder_deck(opts: &BuildOptions) -> Deck {
    let path = std::path::absolute(&opts.input)
        .map(|path| normalize_lexically(&path))
        .unwrap_or_else(|_| opts.input.clone());
    let base_dir = path.parent().unwrap_or(Path::new("/")).to_path_buf();
    let kind = opts
        .format
        .or_else(|| pipeline::detect_kind(&opts.input).ok())
        .unwrap_or(SourceKind::Markdown);
    Deck {
        meta: DeckMeta::default(),
        slides: Vec::new(),
        features: Features::default(),
        source: SourceInfo {
            path,
            kind,
            base_dir,
        },
        assets: Vec::new(),
    }
}

/// A coalesced set of changes ([`Watcher`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeBatch {
    /// The changed paths that are inputs of the build, canonical and sorted.
    pub paths: Vec<PathBuf>,
    /// [`classify`] of `paths` with the watch set's theme directories.
    pub kind: ChangeKind,
}

/// The rebuild a change of `paths` needs (live-reload.md §2): [`ChangeKind::Css`] when
/// **every** path is a stylesheet (`.css`, `.scss`, `.sass`, any case) or lies under the
/// `styles/` or `assets/` directory of one of `theme_dirs`, else [`ChangeKind::Content`]
/// (also for no paths). `theme.toml`, templates, `presenter.toml`, the source and images
/// are content. Paths are compared as given; the [`Watcher`] passes canonical ones.
pub fn classify(paths: &[PathBuf], theme_dirs: &[PathBuf]) -> ChangeKind {
    if !paths.is_empty() && paths.iter().all(|path| is_style_path(path, theme_dirs)) {
        ChangeKind::Css
    } else {
        ChangeKind::Content
    }
}

/// Whether a change of `path` only needs the stylesheet reloaded ([`classify`]).
fn is_style_path(path: &Path, theme_dirs: &[PathBuf]) -> bool {
    let stylesheet = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            ["css", "scss", "sass"]
                .iter()
                .any(|style| ext.eq_ignore_ascii_case(style))
        });
    stylesheet
        || theme_dirs.iter().any(|dir| {
            path.strip_prefix(dir).is_ok_and(|rel| {
                matches!(rel.components().next(), Some(Component::Normal(first))
                    if first == "styles" || first == "assets")
            })
        })
}

/// Directory names whose contents never count below a recursively watched directory.
const IGNORED_DIRS: [&str; 3] = [".git", "target", "dist"];

/// Whether `rel` (a path relative to a recursively watched directory) is on the ignore
/// list: anything inside `.git/`, `target/` or `dist/`, and editor backup, lock and swap
/// files (`*~`, `.#*`, `*.swp`, `*.swx`, vim's `4913` probe) and `.DS_Store`.
pub fn is_ignored(rel: &Path) -> bool {
    let in_ignored_dir = rel.components().any(|component| match component {
        Component::Normal(name) => IGNORED_DIRS.iter().any(|dir| name == *dir),
        _ => false,
    });
    let Some(name) = rel.file_name().and_then(|name| name.to_str()) else {
        return in_ignored_dir;
    };
    in_ignored_dir
        || name.ends_with('~')
        || name.starts_with(".#")
        || name.ends_with(".swp")
        || name.ends_with(".swx")
        || name == "4913"
        || name == ".DS_Store"
}

/// `path` with symlinks resolved, like the paths the OS reports: canonicalized when it
/// exists, else its parent canonicalized (recursively) joined with its name.
pub fn canonical(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    let absolute = std::path::absolute(path)
        .map(|path| normalize_lexically(&path))
        .unwrap_or_else(|_| path.to_path_buf());
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => canonical(parent).join(name),
        _ => absolute,
    }
}

/// The canonical paths of `paths`, sorted and without duplicates.
fn canonical_set(paths: impl IntoIterator<Item = PathBuf>) -> BTreeSet<PathBuf> {
    paths.into_iter().map(|path| canonical(&path)).collect()
}

/// Which paths count, shared by [`Watcher::update`] and the coalescing thread.
#[derive(Debug, Default)]
struct Filter {
    /// [`WatchSet::files`], canonical.
    files: BTreeSet<PathBuf>,
    /// The recursively watched directories (no one inside another).
    recursive: Vec<PathBuf>,
    /// [`WatchSet::theme_dirs`], canonical.
    theme_dirs: Vec<PathBuf>,
    /// Every directory to watch and how.
    targets: BTreeMap<PathBuf, RecursiveMode>,
}

impl Filter {
    /// The filter for `set`: recursive targets are `dirs_recursive` and `theme_dirs`
    /// (without directories inside another one), flat targets are `dirs_flat` and the
    /// parents of `files` that no recursive target already covers.
    fn new(set: &WatchSet) -> Filter {
        let files = canonical_set(set.files.iter().cloned());
        let theme_dirs = canonical_set(set.theme_dirs.iter().cloned());
        let all_recursive = canonical_set(set.dirs_recursive.iter().cloned())
            .into_iter()
            .chain(theme_dirs.iter().cloned())
            .collect::<BTreeSet<_>>();
        let covered = |dir: &Path, roots: &BTreeSet<PathBuf>| {
            roots
                .iter()
                .any(|root| dir != root && dir.starts_with(root))
        };
        let recursive: Vec<PathBuf> = all_recursive
            .iter()
            .filter(|dir| !covered(dir, &all_recursive))
            .cloned()
            .collect();
        let flat = canonical_set(set.dirs_flat.iter().cloned())
            .into_iter()
            .chain(
                files
                    .iter()
                    .filter_map(|f| f.parent().map(Path::to_path_buf)),
            )
            .filter(|dir| !recursive.iter().any(|root| dir.starts_with(root)));
        let mut targets: BTreeMap<PathBuf, RecursiveMode> =
            flat.map(|dir| (dir, RecursiveMode::NonRecursive)).collect();
        for dir in &recursive {
            targets.insert(dir.clone(), RecursiveMode::Recursive);
        }
        Filter {
            files,
            recursive,
            theme_dirs: theme_dirs.into_iter().collect(),
            targets,
        }
    }

    /// Whether a change of `path` (canonical) counts: one of the files, or below a
    /// recursively watched directory and not on the ignore list.
    fn matches(&self, path: &Path) -> bool {
        self.files.contains(path)
            || self
                .recursive
                .iter()
                .any(|root| path.strip_prefix(root).is_ok_and(|rel| !is_ignored(rel)))
    }
}

/// State shared between the [`Watcher`] and its coalescing thread.
#[derive(Debug, Default)]
struct Shared {
    /// What counts.
    filter: Filter,
    /// Watched directories that were created or removed since the last update.
    recreated: BTreeSet<PathBuf>,
}

/// Locks `shared`; a panic while it was held cannot leave it inconsistent (every update
/// replaces whole fields), so a poisoned lock is used as is.
fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Watches the inputs of a build and sends one [`ChangeBatch`] per burst of changes
/// (module docs: what is watched, debouncing, threads).
pub struct Watcher {
    /// The debounced backend watcher; dropping it stops the backend and timer threads.
    debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
    /// Filter and recreated directories, shared with the coalescing thread.
    shared: Arc<Mutex<Shared>>,
    /// The directories currently watched, and how.
    watched: BTreeMap<PathBuf, RecursiveMode>,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher")
            .field("watched", &self.watched)
            .finish_non_exhaustive()
    }
}

impl Watcher {
    /// Starts watching `set`; batches go to `tx` until the watcher is dropped.
    ///
    /// Errors: the backend cannot be started, or a directory that exists cannot be watched
    /// (e.g. the inotify watch limit). Directories that do not exist are skipped (and
    /// retried by every [`Watcher::update`]).
    pub fn start(set: &WatchSet, tx: Sender<ChangeBatch>) -> Result<Watcher> {
        let (raw_tx, raw_rx) = mpsc::channel::<DebounceEventResult>();
        let debouncer =
            new_debouncer(DEBOUNCE, None, raw_tx).map_err(|e| watch_error("start", None, e))?;
        let shared = Arc::new(Mutex::new(Shared::default()));
        let thread_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("presenter-watch".into())
            .spawn(move || coalesce(&raw_rx, &tx, &thread_shared))?;
        let mut watcher = Watcher {
            debouncer,
            shared,
            watched: BTreeMap::new(),
        };
        watcher.update(set)?;
        Ok(watcher)
    }

    /// Watches `set` instead of the current set (call it after every build): directories
    /// no longer needed are unwatched, new ones watched, ones that were recreated since
    /// the last update (or could not be watched before) are watched again. Events of
    /// unchanged directories are not interrupted.
    ///
    /// Errors: as [`Watcher::start`]; the other directories are still updated.
    pub fn update(&mut self, set: &WatchSet) -> Result<()> {
        let filter = Filter::new(set);
        let targets = filter.targets.clone();
        // Swap the filter first, so that events of newly watched directories count.
        let recreated = {
            let mut shared = lock(&self.shared);
            shared.filter = filter;
            std::mem::take(&mut shared.recreated)
        };
        let stale: Vec<PathBuf> = self
            .watched
            .iter()
            .filter(|(dir, mode)| targets.get(*dir) != Some(*mode) || recreated.contains(*dir))
            .map(|(dir, _)| dir.clone())
            .collect();
        for dir in stale {
            self.watched.remove(&dir);
            if let Err(err) = self.debouncer.unwatch(&dir) {
                tracing::debug!("unwatch {}: {err}", dir.display());
            }
        }
        let mut first_error = None;
        for (dir, mode) in targets {
            if self.watched.contains_key(&dir) {
                continue;
            }
            match self.debouncer.watch(&dir, mode) {
                Ok(()) => {
                    tracing::debug!("watching {} ({mode:?})", dir.display());
                    self.watched.insert(dir, mode);
                }
                Err(err) if is_not_found(&err) => {
                    tracing::debug!("not watching {} (missing)", dir.display());
                }
                Err(err) => {
                    first_error.get_or_insert(watch_error("watch", Some(&dir), err));
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// The directories currently watched (canonical), recursive ones flagged `true`.
    pub fn watched_dirs(&self) -> Vec<(PathBuf, bool)> {
        self.watched
            .iter()
            .map(|(dir, mode)| (dir.clone(), *mode == RecursiveMode::Recursive))
            .collect()
    }
}

/// Whether a notify error means the path does not exist.
fn is_not_found(err: &notify::Error) -> bool {
    match &err.kind {
        notify::ErrorKind::PathNotFound => true,
        notify::ErrorKind::Io(io) => io.kind() == std::io::ErrorKind::NotFound,
        _ => false,
    }
}

/// A notify error as an [`Error::Other`]: `file watcher: cannot <what> [<dir>]: <err>`.
fn watch_error(what: &str, dir: Option<&Path>, err: notify::Error) -> Error {
    let dir = dir
        .map(|dir| format!(" {}", dir.display()))
        .unwrap_or_default();
    Error::Other(format!("file watcher: cannot {what}{dir}: {err}"))
}

/// The coalescing thread: gathers the debouncer's output into batches (module docs) and
/// sends them on `tx`. Returns when the debouncer is gone or nobody receives.
fn coalesce(raw: &Receiver<DebounceEventResult>, tx: &Sender<ChangeBatch>, shared: &Mutex<Shared>) {
    let mut pending = BTreeSet::new();
    let mut since: Option<Instant> = None;
    loop {
        let next = match since {
            None => raw.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(start) => {
                raw.recv_timeout(SETTLE.min(MAX_BATCH_WAIT.saturating_sub(start.elapsed())))
            }
        };
        match next {
            Ok(result) => {
                collect(result, shared, &mut pending);
                if pending.is_empty() {
                    continue;
                }
                let start = *since.get_or_insert_with(Instant::now);
                if start.elapsed() < MAX_BATCH_WAIT {
                    continue;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                send_batch(tx, shared, &mut pending);
                return;
            }
        }
        since = None;
        if !send_batch(tx, shared, &mut pending) {
            return;
        }
    }
}

/// Adds the relevant paths of one debouncer emission to `pending`; records watched
/// directories that were created or removed in [`Shared::recreated`]; logs errors.
fn collect(result: DebounceEventResult, shared: &Mutex<Shared>, pending: &mut BTreeSet<PathBuf>) {
    let events = match result {
        Ok(events) => events,
        Err(errors) => {
            for err in errors {
                tracing::warn!("file watcher: {err}");
            }
            return;
        }
    };
    let mut rescan = false;
    let mut changed = Vec::new();
    for event in &events {
        if event.need_rescan() {
            rescan = true;
        } else if !matches!(event.kind, EventKind::Access(_)) {
            let structural = matches!(
                event.kind,
                EventKind::Create(_)
                    | EventKind::Remove(_)
                    | EventKind::Modify(notify::event::ModifyKind::Name(_))
            );
            changed.extend(event.paths.iter().map(|path| (canonical(path), structural)));
        }
    }
    let mut shared = lock(shared);
    if rescan {
        tracing::debug!("file watcher: events were lost; treating every file as changed");
        pending.extend(shared.filter.files.iter().cloned());
    }
    for (path, structural) in changed {
        if structural && shared.filter.targets.contains_key(&path) {
            shared.recreated.insert(path.clone());
            pending.insert(path);
        } else if shared.filter.matches(&path) {
            pending.insert(path);
        }
    }
}

/// Sends `pending` (emptied) as one batch, classified with the current theme
/// directories. Returns false when nobody receives any more. Nothing is sent for no paths.
fn send_batch(
    tx: &Sender<ChangeBatch>,
    shared: &Mutex<Shared>,
    pending: &mut BTreeSet<PathBuf>,
) -> bool {
    if pending.is_empty() {
        return true;
    }
    let paths: Vec<PathBuf> = std::mem::take(pending).into_iter().collect();
    let kind = classify(&paths, &lock(shared).filter.theme_dirs);
    tracing::debug!(?kind, paths = paths.len(), "change batch");
    tx.send(ChangeBatch { paths, kind }).is_ok()
}

/// What [`run_build_watch_with`] reports, in this order: the first build (`Built` or
/// `Failed`), `Watching`, then `Changed` followed by `Built` or `Failed` for every batch.
#[derive(Debug)]
pub enum WatchStatus<'a> {
    /// A build succeeded and was written: the paths written, the document first
    /// ([`pipeline::write_output`]).
    Built {
        /// The build.
        build: &'a Build,
        /// The files written, the document first.
        written: &'a [PathBuf],
    },
    /// A build failed, or its output could not be written (then `diagnostics` are the
    /// build's and `error` the write error).
    Failed(BuildFailure),
    /// The watcher is running, watching this set.
    Watching(&'a WatchSet),
    /// These changes arrived (output paths removed); a rebuild follows.
    Changed(&'a ChangeBatch),
}

/// Blocking loop used by `build --watch`: builds `opts` and writes it to `out`, then
/// rebuilds and rewrites on every [`ChangeBatch`] (a `Css` batch rebuilds too, since the
/// output embeds the stylesheet). Status goes to `tracing` (info: builds and changes;
/// error: failures); the CLI uses [`run_build_watch_with`] to print it instead.
///
/// Runs until the process ends; returns only when the watcher cannot be started (or,
/// with `Ok`, if its thread stops).
pub fn run_build_watch(opts: BuildOptions, out: PathBuf) -> Result<()> {
    run_build_watch_with(&opts, &out, log_status)
}

/// [`run_build_watch`] that hands every [`WatchStatus`] to `report`.
///
/// A failed build is reported and the loop goes on, watching
/// [`watch_set_for_failed`] together with the inputs of the last successful build. Changes
/// to `out` itself (or below it, for a site) are ignored, so an output inside a watched
/// directory does not loop. Batches that queue up during a rebuild are merged into one.
/// The output mode and path stay those of `opts`/`out` (a changed `[build]` section of
/// `presenter.toml` needs a restart).
pub fn run_build_watch_with(
    opts: &BuildOptions,
    out: &Path,
    mut report: impl FnMut(WatchStatus<'_>),
) -> Result<()> {
    let mut last_good = None;
    let mut set = build_and_write(opts, out, &mut last_good, &mut report);
    let (tx, rx) = mpsc::channel();
    let mut watcher = Watcher::start(&set, tx)?;
    report(WatchStatus::Watching(&set));
    while let Some(batch) = next_batch(&rx, out, &set.theme_dirs) {
        report(WatchStatus::Changed(&batch));
        set = build_and_write(opts, out, &mut last_good, &mut report);
        if let Err(err) = watcher.update(&set) {
            tracing::warn!("{err}");
        }
    }
    Ok(())
}

/// One build of the loop: builds, writes, reports, and returns what to watch next.
fn build_and_write(
    opts: &BuildOptions,
    out: &Path,
    last_good: &mut Option<WatchSet>,
    report: &mut impl FnMut(WatchStatus<'_>),
) -> WatchSet {
    match pipeline::try_build_deck(opts) {
        Ok(build) => {
            let set = watch_set_for(&build);
            match pipeline::write_output(&build, out, &opts.output) {
                Ok(written) => report(WatchStatus::Built {
                    build: &build,
                    written: &written,
                }),
                Err(error) => report(WatchStatus::Failed(BuildFailure {
                    error,
                    diagnostics: build.diagnostics.clone(),
                })),
            }
            *last_good = Some(set.clone());
            set
        }
        Err(failure) => {
            report(WatchStatus::Failed(failure));
            let mut set = watch_set_for_failed(opts);
            if let Some(good) = last_good {
                set.merge(good);
            }
            set
        }
    }
}

/// The next batch to rebuild for: every batch waiting in `rx` merged, without the paths of
/// the output `out`, reclassified. `None` when the watcher is gone.
fn next_batch(
    rx: &Receiver<ChangeBatch>,
    out: &Path,
    theme_dirs: &[PathBuf],
) -> Option<ChangeBatch> {
    loop {
        let mut paths: BTreeSet<PathBuf> = rx.recv().ok()?.paths.into_iter().collect();
        while let Ok(more) = rx.try_recv() {
            paths.extend(more.paths);
        }
        let out = canonical(out);
        paths.retain(|path| !path.starts_with(&out));
        if !paths.is_empty() {
            let paths: Vec<PathBuf> = paths.into_iter().collect();
            let kind = classify(&paths, theme_dirs);
            return Some(ChangeBatch { paths, kind });
        }
    }
}

/// The status of [`run_build_watch`], logged.
fn log_status(status: WatchStatus<'_>) {
    match status {
        WatchStatus::Built { build, written } => {
            for diagnostic in build.diagnostics.iter() {
                tracing::warn!("{diagnostic}");
            }
            if let Some(document) = written.first() {
                tracing::info!(
                    "wrote {} ({} slides)",
                    document.display(),
                    build.document.slide_count
                );
            }
        }
        WatchStatus::Failed(BuildFailure { error, diagnostics }) => {
            for diagnostic in diagnostics.iter() {
                tracing::warn!("{diagnostic}");
            }
            tracing::error!("{error}");
        }
        WatchStatus::Watching(set) => {
            tracing::info!(
                "watching {} files and {} theme directories",
                set.files.len(),
                set.dirs_recursive.len()
            );
        }
        WatchStatus::Changed(batch) => {
            tracing::info!(kind = ?batch.kind, "changed: {:?}", batch.paths);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn change_kind_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&ChangeKind::Css).unwrap(), "\"css\"");
        assert_eq!(
            serde_json::to_string(&ChangeKind::Content).unwrap(),
            "\"content\""
        );
        let parsed: ChangeKind = serde_json::from_str("\"css\"").unwrap();
        assert_eq!(parsed, ChangeKind::Css);
    }

    #[test]
    fn stylesheets_anywhere_are_css() {
        let themes = paths(&["/p/theme"]);
        for file in [
            "/p/a.css",
            "/p/x/b.scss",
            "/elsewhere/c.sass",
            "/p/D.CSS",
            "/p/e.Scss",
        ] {
            assert_eq!(
                classify(&paths(&[file]), &themes),
                ChangeKind::Css,
                "{file}"
            );
        }
        assert_eq!(
            classify(&paths(&["/p/a.css", "/q/b.scss"]), &[]),
            ChangeKind::Css
        );
    }

    #[test]
    fn theme_styles_and_assets_dirs_are_css() {
        let themes = paths(&["/p/child", "/p/parent"]);
        for file in [
            "/p/child/styles/vars.toml",
            "/p/child/styles/fonts/x.woff2",
            "/p/parent/assets/logo.png",
            "/p/child/assets",
        ] {
            assert_eq!(
                classify(&paths(&[file]), &themes),
                ChangeKind::Css,
                "{file}"
            );
        }
    }

    #[test]
    fn everything_else_is_content() {
        let themes = paths(&["/p/theme"]);
        for file in [
            "/p/slides.md",
            "/p/talk.ipynb",
            "/p/presenter.toml",
            "/p/theme/theme.toml",
            "/p/theme/templates/deck.html",
            "/p/theme/templates/layouts/title.html",
            "/p/figures/plot.png",
            // `styles/`/`assets/` count only directly inside a theme directory.
            "/p/styles/logo.png",
            "/p/theme/templates/styles/x.html",
            "/p/theme-styles/x.png",
            "/p/theme",
        ] {
            assert_eq!(
                classify(&paths(&[file]), &themes),
                ChangeKind::Content,
                "{file}"
            );
        }
    }

    #[test]
    fn one_content_path_makes_the_batch_content() {
        let themes = paths(&["/p/theme"]);
        let batch = paths(&["/p/theme/styles/x.css", "/p/slides.md"]);
        assert_eq!(classify(&batch, &themes), ChangeKind::Content);
        assert_eq!(classify(&[], &themes), ChangeKind::Content);
    }

    #[test]
    fn ignore_list() {
        for rel in [
            ".git/index",
            ".git",
            "target/debug/x.css",
            "dist/slides.html",
            "styles/sub/dist/x.css",
            "styles/x.css~",
            "styles/.#x.css",
            "styles/.x.css.swp",
            "styles/.x.css.swx",
            "templates/4913",
            ".DS_Store",
        ] {
            assert!(is_ignored(Path::new(rel)), "{rel}");
        }
        for rel in [
            "styles/x.css",
            "templates/deck.html",
            "theme.toml",
            "assets/distant.png",
            "styles/x.swp.css",
            "",
        ] {
            assert!(!is_ignored(Path::new(rel)), "{rel}");
        }
    }

    #[test]
    fn canonical_resolves_missing_files_through_their_parent() {
        let dir = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(canonical(dir.path()), real);
        assert_eq!(
            canonical(&dir.path().join("missing/x.png")),
            real.join("missing/x.png")
        );
        assert_eq!(canonical(&dir.path().join("a/../b.md")), real.join("b.md"));
    }

    #[test]
    fn new_sorts_and_derives_flat_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = canonical(dir.path());
        let set = WatchSet::new(
            [
                dir.path().join("slides.md"),
                dir.path().join("fig/b.png"),
                dir.path().join("fig/a.png"),
                dir.path().join("slides.md"),
            ],
            [dir.path().join("theme")],
        );
        assert_eq!(
            set.files,
            vec![
                root.join("fig/a.png"),
                root.join("fig/b.png"),
                root.join("slides.md")
            ]
        );
        assert_eq!(set.dirs_flat, vec![root.clone(), root.join("fig")]);
        assert_eq!(set.dirs_recursive, vec![root.join("theme")]);
        assert_eq!(set.theme_dirs, set.dirs_recursive);
    }

    #[test]
    fn filter_targets_skip_covered_dirs() {
        let set = WatchSet {
            files: paths(&["/w/slides.md", "/w/theme/extra.toml"]),
            dirs_recursive: paths(&["/w/theme", "/w/theme/parent"]),
            dirs_flat: paths(&["/w/img"]),
            theme_dirs: paths(&["/w/theme", "/w/other"]),
        };
        let filter = Filter::new(&set);
        let targets: Vec<(&str, bool)> = filter
            .targets
            .iter()
            .map(|(dir, mode)| (dir.to_str().unwrap(), *mode == RecursiveMode::Recursive))
            .collect();
        assert_eq!(
            targets,
            vec![
                ("/w", false),
                ("/w/img", false),
                ("/w/other", true),
                ("/w/theme", true)
            ]
        );
        assert!(filter.matches(Path::new("/w/slides.md")));
        assert!(filter.matches(Path::new("/w/theme/styles/x.css")));
        assert!(filter.matches(Path::new("/w/theme/extra.toml")));
        assert!(!filter.matches(Path::new("/w/notes.txt")));
        assert!(!filter.matches(Path::new("/w/img/unrelated.png")));
        assert!(!filter.matches(Path::new("/w/theme/.git/HEAD")));
        assert!(!filter.matches(Path::new("/w/theme/styles/x.css~")));
    }

    #[test]
    fn merge_unions_every_list() {
        let mut a = WatchSet {
            files: paths(&["/a.md"]),
            dirs_flat: paths(&["/"]),
            ..WatchSet::default()
        };
        let b = WatchSet {
            files: paths(&["/a.md", "/b/c.png"]),
            dirs_recursive: paths(&["/t"]),
            dirs_flat: paths(&["/", "/b"]),
            theme_dirs: paths(&["/t"]),
        };
        a.merge(&b);
        assert_eq!(a.files, paths(&["/a.md", "/b/c.png"]));
        assert_eq!(a.dirs_flat, paths(&["/", "/b"]));
        assert_eq!(a.dirs_recursive, paths(&["/t"]));
        assert_eq!(a.theme_dirs, paths(&["/t"]));
    }

    #[test]
    fn next_batch_merges_queued_batches_and_drops_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let root = canonical(dir.path());
        let theme = root.join("theme");
        let (tx, rx) = mpsc::channel();
        let batch = |names: &[&str]| ChangeBatch {
            paths: names.iter().map(|name| root.join(name)).collect(),
            kind: ChangeKind::Content,
        };
        // The output (a site directory here) lies inside the watched theme directory.
        tx.send(batch(&["theme/out/index.html", "theme/styles/x.css"]))
            .unwrap();
        tx.send(batch(&["theme/out/assets/a.png", "theme/styles/y.css"]))
            .unwrap();
        tx.send(batch(&["theme/out/index.html"])).unwrap();
        let out = dir.path().join("theme/out");
        let next = next_batch(&rx, &out, std::slice::from_ref(&theme)).unwrap();
        assert_eq!(
            next.paths,
            vec![theme.join("styles/x.css"), theme.join("styles/y.css")]
        );
        assert_eq!(next.kind, ChangeKind::Css);

        // A batch of output paths only is skipped; a closed channel ends the loop.
        tx.send(batch(&["theme/out/index.html"])).unwrap();
        drop(tx);
        assert_eq!(next_batch(&rx, &out, &[theme]), None);
    }

    #[test]
    fn watcher_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Watcher>();
    }
}
