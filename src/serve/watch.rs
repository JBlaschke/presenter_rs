//! File watcher, debouncing, rebuild loop and change classification (live-reload.md §2,
//! §5). Implemented by T20; [`ChangeKind`] is the T00 contract type shared with the dev
//! server (T21).
//!
//! # What is watched
//!
//! A [`WatchSet`] lists the inputs of one build ([`watch_set_for`], or
//! [`watch_set_for_failed`] when the build failed, or [`watch_set_before_build`] while the
//! first build runs): the source file, the files it includes (`Deck.includes`; for a deck
//! that does not parse, those [`scan_includes`] finds), the `presenter.toml` that was used,
//! every file asset
//! (and every missing one), each disk theme of the chain (when the theme does not resolve,
//! every directory it may be in) and, in a debug build, `assets/themes/default/` of this
//! crate (rust-embed reads it from disk then). All paths are canonical (`/tmp/x` on macOS
//! is `/private/tmp/x`), so they compare equal to the paths the OS reports.
//!
//! Files are watched through their **parent directory**, non-recursively: editors that
//! save by writing a temporary file and renaming it over the original (vim, VS Code "safe
//! write") replace the file, and a watch on the file itself would be lost. Events in those
//! directories count only for the listed files. Theme directories are watched recursively
//! and every path below them counts, except the [ignore list](is_ignored) (`.git/`, editor
//! backup, swap and auto-save files). A theme's `dist/` or `target/` directory counts like
//! any other (audit F18): it may hold inputs of the build, such as a bundled script entry
//! or a vendored library's stylesheet, and the build's own output is not mistaken for a
//! change (`build --watch` drops it, `serve` writes nothing).
//!
//! A directory to watch that does not exist (the parent of a missing image, a theme not
//! created yet, the deck's own directory while a `git checkout` or a generator replaces
//! it) is watched through its **nearest existing ancestor**, non-recursively. Creating,
//! removing or renaming that directory or any directory on the way to it counts as a
//! change, so a rebuild follows and the next [`Watcher::update`] watches one level deeper,
//! until the directory itself is watched.
//!
//! # Debouncing and batches
//!
//! [`Watcher`] hands the backend's events (notify's recommended watcher) to a coalescing
//! thread, which gathers the relevant paths until none has arrived for [`DEBOUNCE`]
//! (150 ms; at most [`MAX_BATCH_WAIT`] after the first; [`Watcher::start_with`] takes
//! other times, a [`Debounce`]), so a save touching several files (or a `git checkout`)
//! becomes **one** [`ChangeBatch`]: its relevant paths, sorted, and their [`ChangeKind`]
//! ([`classify`]). Any event kind counts as a change (create, modify, remove, rename,
//! metadata) except a mere access: inotify reports every `open()`, and the build itself
//! opens its inputs. When the backend lost events (a rescan), the batch lists every
//! watched file. A watched directory that is created or removed (deleted and recreated,
//! e.g. by `git checkout`) counts as a change too, and the next [`Watcher::update`]
//! watches it again.
//!
//! The events are not passed through `notify-debouncer-full` (which live-reload.md §5
//! names): it drops a creation followed by a removal of the same path, and everything it
//! holds below a removed directory. FSEvents reports flags cumulatively, so a directory
//! created a little earlier and then removed (or replaced) arrives as "created, removed"
//! and that debouncer emits nothing for it: exactly the removals and re-creations this
//! watcher must see.
//!
//! Changing what is watched can lose changes: a directory that [`Watcher::update`] starts
//! watching may have changed before its watch existed (a file written into a recreated
//! directory while the rebuild ran), FSEvents restarts its stream on every watch and
//! unwatch, dropping what it had not delivered yet, and a file that only now becomes an
//! input did not count before. So `update` sends a batch itself for the inputs modified
//! since the last batch was sent (by modification time): when it watched or unwatched a
//! directory, the listed files and the files below the recursively watched directories
//! outside the ignore list; else the files new in the set. A change can still be missed on
//! a file system with coarse timestamps.
//!
//! # Threads
//!
//! [`Watcher::start`] starts the backend's thread (FSEvents run loop or inotify) and the
//! coalescing thread `presenter-watch`; batches are sent from the latter on the `Sender`
//! given to [`Watcher::start`] (and from [`Watcher::update`], as above). The receiving side
//! decides where to rebuild: `presenter build --watch` blocks on it
//! ([`run_build_watch_with`]); an async server should receive on a dedicated thread (or
//! `spawn_blocking`) and forward to its rebuild task. [`Watcher`] is `Send`; keep it alive
//! as long as changes are wanted (dropping it stops both threads; the coalescing thread
//! then flushes what it holds and drops its `Sender`, so the channel closes) and call
//! [`Watcher::update`] with the inputs of every build, from the thread that owns it. Start
//! it **before** the first build (on [`watch_set_before_build`]), so that a change saved
//! while that build runs is not lost ([`run_build_watch_with`] does).

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::assets::registry::{AssetData, AssetRegistry, normalize_lexically};
use crate::config::Config;
use crate::error::{Diagnostics, Error, Result};
use crate::ir::{Deck, DeckMeta, Features, Parsed, SourceInfo, SourceKind};
use crate::markdown::include::scan_includes;
use crate::markdown::render::missing_asset_path;
use crate::pipeline::{self, Build, BuildFailure, BuildOptions};
use crate::theme::resolve::{MANIFEST_FILE, MAX_CHAIN_LEN};

/// The debounce time (live-reload.md §5): a batch is sent once no relevant change has
/// arrived for this long.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// A batch is sent at the latest this long after its first path, even while changes keep
/// arriving.
pub const MAX_BATCH_WAIT: Duration = Duration::from_secs(2);

/// How a [`Watcher`] coalesces changes into batches (module docs): a batch is sent once no
/// relevant change has arrived for `quiet`, and at the latest `max_wait` after its first
/// change. The default is the contract of live-reload.md §5 ([`DEBOUNCE`],
/// [`MAX_BATCH_WAIT`]), which [`Watcher::start`] uses; a test can widen `quiet` with
/// [`Watcher::start_with`], so that the time the OS takes to deliver the events of two
/// changes made together cannot split them into two batches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Debounce {
    /// A batch is sent once no relevant change has arrived for this long.
    pub quiet: Duration,
    /// A batch is sent at the latest this long after its first change, even while changes
    /// keep arriving (so a `quiet` longer than this never waits longer than this).
    pub max_wait: Duration,
}

impl Default for Debounce {
    fn default() -> Debounce {
        Debounce {
            quiet: DEBOUNCE,
            max_wait: MAX_BATCH_WAIT,
        }
    }
}

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
/// [`watch_set_for`], [`watch_set_for_failed`], [`watch_set_before_build`] or
/// [`WatchSet::new`]; the watcher canonicalizes whatever it is given, so a hand-written set
/// works too.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchSet {
    /// Files whose changes count: the source, the files it includes, `presenter.toml`, the
    /// deck's file assets
    /// (including missing ones, so that creating them, and any directories they need,
    /// rebuilds).
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

/// The inputs of a successful build (live-reload.md §5): the source file, every file it
/// includes (`Deck.includes`), the `presenter.toml` that was used, every file asset and every
/// asset reported missing, the chain's disk themes and, in a debug build, this crate's
/// `assets/themes/default/`.
pub fn watch_set_for(build: &Build) -> WatchSet {
    let mut files = vec![build.deck.source.path.clone()];
    files.extend(build.deck.includes.iter().cloned());
    files.extend(build.config_path.iter().cloned());
    files.extend(asset_files(&build.assets, &build.diagnostics));
    WatchSet::new(files, with_builtin_theme(build.theme.watch_dirs()))
}

/// The inputs of a failed build, as far as they are known: the source file, the
/// `presenter.toml` (`--config`, else the one discovery finds, even when it is invalid)
/// and, in a debug build, this crate's `assets/themes/default/`. When the source still
/// parses (the build failed later: a missing asset, the theme, a template), also the files
/// it includes, the deck's file assets including the missing ones, and the theme's
/// directories; when it does not parse (also when an included file does not), what
/// [`watch_set_before_build`] finds, which includes the files [`scan_includes`] finds. A theme that does not resolve
/// (not found, an invalid `theme.toml`, a parent that is missing) contributes every
/// directory it may be in (its name below the config file's directory, the deck's
/// directory and the current directory, and where each parent named by `extends` is
/// looked for), so that fixing or creating it rebuilds.
///
/// This parses the deck again; [`try_build_watched`] builds and returns this set from the
/// build's own parse instead.
pub fn watch_set_for_failed(opts: &BuildOptions) -> WatchSet {
    match pipeline::parse_only(opts) {
        Ok((parsed, config, config_path)) => {
            ParsedInputs::new(opts, &parsed, &config, config_path.as_deref()).watch_set(opts)
        }
        Err(err) => unparsed_watch_set(opts, &err),
    }
}

/// Builds `opts` ([`pipeline::try_build_deck`]; nothing is written) and returns the result
/// with the inputs to watch: [`watch_set_for`] the build, or when it failed, the set
/// [`watch_set_for_failed`] gives, taken from the failed build's own parse
/// ([`pipeline::try_build_deck_inspecting`]), so the deck is parsed once.
pub fn try_build_watched(
    opts: &BuildOptions,
) -> (std::result::Result<Build, BuildFailure>, WatchSet) {
    let mut inputs = None;
    let result = pipeline::try_build_deck_inspecting(opts, |parsed, config, config_path| {
        inputs = Some(ParsedInputs::new(opts, parsed, config, config_path));
    });
    let set = match (&result, inputs) {
        (Ok(build), _) => watch_set_for(build),
        (Err(_), Some(inputs)) => inputs.watch_set(opts),
        (Err(failure), None) => unparsed_watch_set(opts, &failure.error),
    };
    (result, set)
}

/// The watch set of a deck that does not parse (`err`): [`watch_set_before_build`].
fn unparsed_watch_set(opts: &BuildOptions, err: &Error) -> WatchSet {
    tracing::debug!("watching a deck that does not parse: {err}");
    watch_set_before_build(opts)
}

/// What [`watch_set_for_failed`] watches of a deck that parses, kept so that the set can
/// be made after the parsed deck is gone (moved into the build's later stages).
struct ParsedInputs {
    /// The source, the files it includes, the deck's file assets (missing ones included)
    /// and the `presenter.toml` that was used.
    files: Vec<PathBuf>,
    /// The deck without its slides: what [`theme_dirs`] reads (`meta.theme`, `source`).
    deck: Deck,
    /// The config with the front matter merged.
    config: Config,
    /// The `presenter.toml` that was used.
    config_path: Option<PathBuf>,
}

impl ParsedInputs {
    /// The inputs of `parsed`, which [`pipeline::try_parse_only`] returned for `opts` with
    /// `config` and `config_path`.
    fn new(
        opts: &BuildOptions,
        parsed: &Parsed,
        config: &Config,
        config_path: Option<&Path>,
    ) -> ParsedInputs {
        let mut files = vec![opts.input.clone()];
        files.extend(parsed.deck.includes.iter().cloned());
        files.extend(asset_files(&parsed.assets, &parsed.diagnostics));
        files.extend(config_path.map(Path::to_path_buf));
        let deck = Deck {
            meta: parsed.deck.meta.clone(),
            slides: Vec::new(),
            features: Features::default(),
            source: parsed.deck.source.clone(),
            assets: Vec::new(),
            includes: Vec::new(),
        };
        ParsedInputs {
            files,
            deck,
            config: config.clone(),
            config_path: config_path.map(Path::to_path_buf),
        }
    }

    /// The watch set: the files, the theme directories ([`theme_dirs`]) and the built-in
    /// theme in a debug build.
    fn watch_set(self, opts: &BuildOptions) -> WatchSet {
        let themes = theme_dirs(opts, &self.deck, &self.config, self.config_path.as_deref());
        WatchSet::new(self.files, with_builtin_theme(themes))
    }
}

/// The inputs of a build of `opts` that are known without parsing the deck: the source
/// file, the files a Markdown source includes as far as [`scan_includes`] finds them (an
/// included file whose error stopped the build among them, so that fixing it rebuilds),
/// the `presenter.toml` (even when it is invalid), the theme named by `--theme` or
/// `[deck] theme` (as [`watch_set_for_failed`] watches it; a front matter `theme` is not
/// known yet) and, in a debug build, this crate's `assets/themes/default/`. Cheap (it
/// reads the config and the theme manifests only): watch this while the first build runs,
/// then [`Watcher::update`] to the build's own set.
pub fn watch_set_before_build(opts: &BuildOptions) -> WatchSet {
    let deck = placeholder_deck(opts);
    let (config_path, themes) = match pipeline::load_config(opts) {
        Ok((config, config_path)) => {
            let themes = theme_dirs(opts, &deck, &config, config_path.as_deref());
            (config_path, themes)
        }
        Err(_) => {
            // `[deck] theme` is unknown, but `--theme` still resolves (the defaults' theme
            // is built in, so it adds nothing).
            let config_path = config_file(opts);
            let themes = theme_dirs(opts, &deck, &Config::default(), config_path.as_deref());
            (config_path, themes)
        }
    };
    let includes = match deck.source.kind {
        SourceKind::Markdown => scan_includes(&opts.input),
        SourceKind::Notebook => Vec::new(),
    };
    let files = std::iter::once(opts.input.clone())
        .chain(includes)
        .chain(config_path);
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

/// The theme directories to watch for `deck`: the chain's disk themes when the theme
/// resolves ([`pipeline::resolve_theme`]), else [`theme_candidates`].
fn theme_dirs(
    opts: &BuildOptions,
    deck: &Deck,
    config: &Config,
    config_path: Option<&Path>,
) -> Vec<PathBuf> {
    match pipeline::resolve_theme(opts, deck, config, config_path) {
        Ok(theme) => theme.watch_dirs(),
        Err(err) => {
            tracing::debug!("watching where the theme may be: {err}");
            theme_candidates(opts, deck, config, config_path)
        }
    }
}

/// Where a theme that does not resolve is or may appear, searched like
/// [`pipeline::resolve_theme`] does: the theme spec (`--theme`, the front matter's
/// `theme`, `[deck] theme`) below the config file's directory, the deck's directory and
/// the current directory (the spec itself when absolute); then, from the first of those
/// with a `theme.toml`, each parent named by `extends` (below the extending theme) as far
/// as the manifests parse. These include the theme whose manifest is invalid and the
/// directory a missing parent is looked for in.
fn theme_candidates(
    opts: &BuildOptions,
    deck: &Deck,
    config: &Config,
    config_path: Option<&Path>,
) -> Vec<PathBuf> {
    let spec = opts
        .cli
        .theme
        .as_deref()
        .or(deck.meta.theme.as_deref())
        .unwrap_or(&config.deck.theme);
    let cwd = std::env::current_dir().ok();
    let roots = config_path
        .and_then(Path::parent)
        .into_iter()
        .chain([deck.source.base_dir.as_path()])
        .chain(cwd.as_deref());
    // `join` with an absolute spec is the spec itself.
    let mut dirs: Vec<PathBuf> = roots.map(|root| root.join(spec)).collect();
    let mut theme = dirs
        .iter()
        .find(|dir| dir.join(MANIFEST_FILE).is_file())
        .cloned();
    for _ in 0..MAX_CHAIN_LEN {
        let Some(parent) = theme.take().and_then(|dir| Some(dir.join(extends(&dir)?))) else {
            break;
        };
        if parent.join(MANIFEST_FILE).is_file() {
            theme = Some(parent.clone());
        }
        dirs.push(parent);
    }
    dirs
}

/// The `extends` of the `theme.toml` in `dir`, if that file is TOML (its other keys are
/// not checked).
fn extends(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(MANIFEST_FILE)).ok()?;
    let manifest: toml::Table = text.parse().ok()?;
    manifest.get("extends")?.as_str().map(str::to_string)
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
        includes: Vec::new(),
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
/// (`dist` and `target` are not among them: a theme may keep inputs there, audit F18.)
const IGNORED_DIRS: [&str; 1] = [".git"];

/// Whether `rel` (a path relative to a recursively watched directory) is on the ignore
/// list: anything inside `.git/`, and editor files: backups (`*~`), Emacs lock and
/// auto-save files (`.#*`, `#*#`), vim swap files (`*.swp`, `*.swx`, and `.*.swa` …
/// `.*.swo` for further sessions) and vim's `4913` probe, and `.DS_Store`.
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
        || (name.len() > 1 && name.starts_with('#') && name.ends_with('#'))
        || name.ends_with(".swp")
        || name.ends_with(".swx")
        || is_vim_swap(name)
        || name == "4913"
        || name == ".DS_Store"
}

/// Whether `name` is a vim swap file of a later session: `.<file>.sw<a…p>` (vim counts
/// down from `.swp`).
fn is_vim_swap(name: &str) -> bool {
    let Some(stem) = name.strip_prefix('.') else {
        return false;
    };
    match stem.rsplit_once('.') {
        Some((file, ext)) => {
            !file.is_empty()
                && ext.len() == 3
                && ext.starts_with("sw")
                && ext.as_bytes()[2].is_ascii_lowercase()
                && ext.as_bytes()[2] <= b'p'
        }
        None => false,
    }
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

/// The directories to watch for `targets` and the targets that do not exist: each target
/// that exists, as it asks; for each missing one its nearest existing ancestor,
/// non-recursively, unless a directory already watched reports changes there.
fn plan_watches(
    targets: &BTreeMap<PathBuf, RecursiveMode>,
) -> (BTreeMap<PathBuf, RecursiveMode>, BTreeSet<PathBuf>) {
    let (mut wanted, missing): (BTreeMap<_, _>, BTreeMap<_, _>) = targets
        .iter()
        .map(|(dir, mode)| (dir.clone(), *mode))
        .partition(|(dir, _)| dir.is_dir());
    for dir in missing.keys() {
        if let Some(ancestor) = existing_ancestor(dir)
            && !covers(&wanted, &ancestor)
        {
            wanted.insert(ancestor, RecursiveMode::NonRecursive);
        }
    }
    (wanted, missing.into_keys().collect())
}

/// Whether changes of the entries of `dir` reach a watch in `watched`: `dir` itself is
/// watched, or it lies below a recursively watched directory.
fn covers(watched: &BTreeMap<PathBuf, RecursiveMode>, dir: &Path) -> bool {
    watched.contains_key(dir)
        || watched
            .iter()
            .any(|(root, mode)| *mode == RecursiveMode::Recursive && dir.starts_with(root))
}

/// The nearest proper ancestor of `path` that is an existing directory.
fn existing_ancestor(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .skip(1)
        .find(|dir| dir.is_dir())
        .map(Path::to_path_buf)
}

/// State shared between the [`Watcher`] and its coalescing thread.
#[derive(Debug)]
struct Shared {
    /// What counts.
    filter: Filter,
    /// Targets of the filter that do not exist (watched through an ancestor).
    missing: BTreeSet<PathBuf>,
    /// Watched directories, and directories on the way to a missing target, that were
    /// created or removed since the last update.
    recreated: BTreeSet<PathBuf>,
    /// When the last batch was sent (at first: when the watcher started).
    since: SystemTime,
}

impl Shared {
    /// Whether a creation, removal or rename of `path` changes what can be watched: it is
    /// a target, or a directory on the way to a missing one.
    fn is_structural(&self, path: &Path) -> bool {
        self.filter.targets.contains_key(path)
            || self.missing.iter().any(|missing| missing.starts_with(path))
    }
}

/// Locks `shared`; a panic while it was held cannot leave it inconsistent (every update
/// replaces whole fields), so a poisoned lock is used as is.
fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Watches the inputs of a build and sends one [`ChangeBatch`] per burst of changes
/// (module docs: what is watched, debouncing, threads).
pub struct Watcher {
    /// The backend watcher; dropping it stops its thread, and then the coalescing thread.
    backend: RecommendedWatcher,
    /// Filter and recreated directories, shared with the coalescing thread.
    shared: Arc<Mutex<Shared>>,
    /// The directories currently watched, and how.
    watched: BTreeMap<PathBuf, RecursiveMode>,
    /// Where [`Watcher::update`] sends its catch-up batches.
    tx: Sender<ChangeBatch>,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher")
            .field("watched", &self.watched)
            .finish_non_exhaustive()
    }
}

impl Watcher {
    /// Starts watching `set`; batches go to `tx` until the watcher is dropped. Changes are
    /// coalesced with [`Debounce::default`] (the contract: [`DEBOUNCE`],
    /// [`MAX_BATCH_WAIT`]).
    ///
    /// Errors: the backend cannot be started, or a directory that exists cannot be watched
    /// (e.g. the inotify watch limit). A directory that does not exist is watched through
    /// its nearest existing ancestor (module docs).
    pub fn start(set: &WatchSet, tx: Sender<ChangeBatch>) -> Result<Watcher> {
        Watcher::start_with(set, tx, Debounce::default())
    }

    /// [`Watcher::start`] coalescing changes with `debounce` instead of the default.
    pub fn start_with(
        set: &WatchSet,
        tx: Sender<ChangeBatch>,
        debounce: Debounce,
    ) -> Result<Watcher> {
        let (raw_tx, raw_rx) = mpsc::channel::<notify::Result<Event>>();
        let backend =
            notify::recommended_watcher(raw_tx).map_err(|e| watch_error("start", None, e))?;
        let shared = Arc::new(Mutex::new(Shared {
            filter: Filter::default(),
            missing: BTreeSet::new(),
            recreated: BTreeSet::new(),
            since: SystemTime::now(),
        }));
        let thread_shared = Arc::clone(&shared);
        let thread_tx = tx.clone();
        std::thread::Builder::new()
            .name("presenter-watch".into())
            .spawn(move || coalesce(&raw_rx, &thread_tx, &thread_shared, debounce))?;
        let mut watcher = Watcher {
            backend,
            shared,
            watched: BTreeMap::new(),
            tx,
        };
        watcher.update(set)?;
        Ok(watcher)
    }

    /// Watches `set` instead of the current set (call it after every build): directories
    /// no longer needed are unwatched, new ones watched, ones that were recreated since
    /// the last update (or could not be watched before) are watched again; a missing one
    /// is watched through its nearest existing ancestor. Then the inputs modified since the
    /// last batch that may have been missed are sent as a batch (module docs).
    ///
    /// Errors: as [`Watcher::start`]; the other directories are still updated.
    pub fn update(&mut self, set: &WatchSet) -> Result<()> {
        let filter = Filter::new(set);
        let (wanted, missing) = plan_watches(&filter.targets);
        let (files, recursive) = (filter.files.clone(), filter.recursive.clone());
        // Swap the filter first, so that events of newly watched directories count.
        let (recreated, since, old_files) = {
            let mut shared = lock(&self.shared);
            let old = std::mem::replace(&mut shared.filter, filter);
            shared.missing = missing;
            (
                std::mem::take(&mut shared.recreated),
                shared.since,
                old.files,
            )
        };
        let stale: Vec<PathBuf> = self
            .watched
            .iter()
            .filter(|(dir, mode)| wanted.get(*dir) != Some(*mode) || recreated.contains(*dir))
            .map(|(dir, _)| dir.clone())
            .collect();
        let unwatched = !stale.is_empty();
        for dir in stale {
            self.watched.remove(&dir);
            if let Err(err) = self.backend.unwatch(&dir) {
                tracing::debug!("unwatch {}: {err}", dir.display());
            }
        }
        let (watched, result) = self.watch_all(wanted);
        if unwatched || watched {
            self.catch_up(files.iter(), &recursive, since);
        } else {
            self.catch_up(files.difference(&old_files), &[], since);
        }
        result
    }

    /// Watches every directory of `wanted` that is not watched yet. A directory that has
    /// disappeared in the meantime is watched through its nearest existing ancestor.
    /// Returns whether a directory was newly watched, and the first error.
    fn watch_all(&mut self, wanted: BTreeMap<PathBuf, RecursiveMode>) -> (bool, Result<()>) {
        let mut queue: Vec<(PathBuf, RecursiveMode)> = wanted
            .into_iter()
            .filter(|(dir, _)| !self.watched.contains_key(dir))
            .collect();
        let mut added = false;
        let mut first_error = None;
        while let Some((dir, mode)) = queue.pop() {
            // An ancestor queued for a directory that vanished may be wanted itself: watch
            // it once, recursively when either asks for that.
            let done = self
                .watched
                .get(&dir)
                .is_some_and(|watched| *watched == mode || *watched == RecursiveMode::Recursive);
            if done {
                continue;
            }
            match self.backend.watch(&dir, mode) {
                Ok(()) => {
                    tracing::debug!("watching {} ({mode:?})", dir.display());
                    self.watched.insert(dir, mode);
                    added = true;
                }
                Err(err) if is_not_found(&err) => {
                    tracing::debug!("not watching {} (missing)", dir.display());
                    lock(&self.shared).missing.insert(dir.clone());
                    if let Some(ancestor) = existing_ancestor(&dir)
                        && !covers(&self.watched, &ancestor)
                    {
                        queue.push((ancestor, RecursiveMode::NonRecursive));
                    }
                }
                Err(err) => {
                    first_error.get_or_insert(watch_error("watch", Some(&dir), err));
                }
            }
        }
        (added, first_error.map_or(Ok(()), Err))
    }

    /// Sends a batch for the inputs modified at or after `since`: of `files`, and the files
    /// below the `recursive` directories that are not on the ignore list. Their changes may
    /// have been missed while the watch set changed (module docs). A batch sent counts as
    /// the last batch ([`Shared::since`]), so later updates do not report the same changes
    /// again.
    fn catch_up<'a>(
        &self,
        files: impl Iterator<Item = &'a PathBuf>,
        recursive: &[PathBuf],
        since: SystemTime,
    ) {
        // Whatever changes from now on is reported by the watches (or the next update).
        let scanned = SystemTime::now();
        let mut paths: BTreeSet<PathBuf> = files
            .filter(|file| modified_since(file, since))
            .cloned()
            .collect();
        for root in recursive {
            modified_below(root, root, since, &mut paths);
        }
        if paths.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = paths.into_iter().collect();
        tracing::debug!("changed while the watches changed: {paths:?}");
        let kind = {
            let mut shared = lock(&self.shared);
            shared.since = shared.since.max(scanned);
            classify(&paths, &shared.filter.theme_dirs)
        };
        // Nobody receiving is not an error of the update.
        let _ = self.tx.send(ChangeBatch { paths, kind });
    }

    /// The directories currently watched (canonical), recursive ones flagged `true`. This
    /// includes the ancestors watched for missing directories.
    pub fn watched_dirs(&self) -> Vec<(PathBuf, bool)> {
        self.watched
            .iter()
            .map(|(dir, mode)| (dir.clone(), *mode == RecursiveMode::Recursive))
            .collect()
    }
}

/// Whether `path` is a file modified at or after `since`.
fn modified_since(path: &Path, since: SystemTime) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .is_ok_and(|modified| modified >= since)
}

/// Adds the files below `dir` (inside the recursively watched `root`) that were modified at
/// or after `since` to `found`, skipping the ignore list. Symbolic links are not followed.
fn modified_below(root: &Path, dir: &Path, since: SystemTime, found: &mut BTreeSet<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.strip_prefix(root).is_ok_and(is_ignored) {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => modified_below(root, &path, since, found),
            Ok(kind) if kind.is_file() && modified_since(&path, since) => {
                found.insert(path);
            }
            _ => {}
        }
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

/// The coalescing thread: gathers the backend's events into batches (module docs; the
/// window is `debounce`) and sends them on `tx`. Returns when the backend is gone or
/// nobody receives.
fn coalesce(
    raw: &Receiver<notify::Result<Event>>,
    tx: &Sender<ChangeBatch>,
    shared: &Mutex<Shared>,
    debounce: Debounce,
) {
    let mut pending = BTreeSet::new();
    // When the batch being gathered got its first and its latest relevant change.
    let mut window: Option<(Instant, Instant)> = None;
    loop {
        let next = match window {
            None => raw.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some((first, last)) => raw.recv_timeout(
                debounce
                    .quiet
                    .saturating_sub(last.elapsed())
                    .min(debounce.max_wait.saturating_sub(first.elapsed())),
            ),
        };
        match next {
            Ok(result) => {
                if collect(result, shared, &mut pending) {
                    let now = Instant::now();
                    window = Some(window.map_or((now, now), |(first, _)| (first, now)));
                }
                // Irrelevant events keep coming without a timeout: check the clock too.
                let due = window.is_some_and(|(first, last)| {
                    last.elapsed() >= debounce.quiet || first.elapsed() >= debounce.max_wait
                });
                if !due {
                    continue;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                send_batch(tx, shared, &mut pending);
                return;
            }
        }
        window = None;
        if !send_batch(tx, shared, &mut pending) {
            return;
        }
    }
}

/// Adds the relevant paths of one backend event to `pending` and returns whether there
/// were any; records created or removed directories that change what can be watched
/// ([`Shared::is_structural`]) in [`Shared::recreated`] (they are changes too); logs
/// errors.
fn collect(
    result: notify::Result<Event>,
    shared: &Mutex<Shared>,
    pending: &mut BTreeSet<PathBuf>,
) -> bool {
    let event = match result {
        Ok(event) => event,
        Err(err) => {
            tracing::warn!("file watcher: {err}");
            return false;
        }
    };
    let mut shared = lock(shared);
    if event.need_rescan() {
        tracing::debug!("file watcher: events were lost; treating every file as changed");
        pending.extend(shared.filter.files.iter().cloned());
        return !shared.filter.files.is_empty();
    }
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    let structural = matches!(
        event.kind,
        EventKind::Create(_)
            | EventKind::Remove(_)
            | EventKind::Modify(notify::event::ModifyKind::Name(_))
    );
    let mut relevant = false;
    for path in event.paths.iter().map(|path| canonical(path)) {
        if structural && shared.is_structural(&path) {
            shared.recreated.insert(path.clone());
        } else if !shared.filter.matches(&path) {
            continue;
        }
        pending.insert(path);
        relevant = true;
    }
    relevant
}

/// Sends `pending` (emptied) as one batch, classified with the current theme
/// directories, and records the time in [`Shared::since`]. Returns false when nobody
/// receives any more. Nothing is sent for no paths.
fn send_batch(
    tx: &Sender<ChangeBatch>,
    shared: &Mutex<Shared>,
    pending: &mut BTreeSet<PathBuf>,
) -> bool {
    if pending.is_empty() {
        return true;
    }
    let paths: Vec<PathBuf> = std::mem::take(pending).into_iter().collect();
    let kind = {
        let mut shared = lock(shared);
        shared.since = SystemTime::now();
        classify(&paths, &shared.filter.theme_dirs)
    };
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

/// The blocking loop of `presenter build --watch`: builds `opts` and writes it to `out`,
/// then rebuilds and rewrites on every [`ChangeBatch`] (a `Css` batch rebuilds too, since
/// the output embeds the stylesheet), handing every [`WatchStatus`] to `report` (the CLI
/// prints them); the loop ends (with `Ok`) as soon as `report` returns
/// [`ControlFlow::Break`], or when the watcher's thread stops. Errors only when the watcher
/// cannot be started.
///
/// The watcher starts before the first build, on [`watch_set_before_build`], so that a
/// change saved while that build runs triggers a rebuild, and it watches the inputs of each
/// build before that build is reported, so that whatever is changed in response to the
/// report counts. A failed build is reported and the loop goes on, watching
/// [`watch_set_for_failed`] (taken from the failed build's own parse: [`try_build_watched`])
/// together with the inputs of the last successful build. Changes
/// to `out` itself (or below it, for a site), and to the directories above it that the
/// build creates inside a watched theme directory, are ignored, so an output inside a
/// watched directory does not loop. Batches that queue up during a rebuild are merged into
/// one. The output mode and path stay those of `opts`/`out` (a changed `[build]` section of
/// `presenter.toml` needs a restart).
pub fn run_build_watch_with(
    opts: &BuildOptions,
    out: &Path,
    mut report: impl FnMut(WatchStatus<'_>) -> ControlFlow<()>,
) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let mut watcher = Watcher::start(&watch_set_before_build(opts), tx)?;
    let mut last_good = None;
    let mut build = |watcher: &mut Watcher, report: &mut _| {
        build_and_write(opts, out, watcher, &mut last_good, report)
    };
    let ControlFlow::Continue(mut set) = build(&mut watcher, &mut report) else {
        return Ok(());
    };
    if report(WatchStatus::Watching(&set)).is_break() {
        return Ok(());
    }
    while let Some(batch) = next_batch(&rx, out, &set) {
        if report(WatchStatus::Changed(&batch)).is_break() {
            return Ok(());
        }
        let ControlFlow::Continue(next) = build(&mut watcher, &mut report) else {
            return Ok(());
        };
        set = next;
    }
    Ok(())
}

/// One build of the loop: builds and writes, has `watcher` watch the build's inputs,
/// reports, and returns the set now watched (or `Break` when `report` asks to stop).
fn build_and_write(
    opts: &BuildOptions,
    out: &Path,
    watcher: &mut Watcher,
    last_good: &mut Option<WatchSet>,
    report: &mut impl FnMut(WatchStatus<'_>) -> ControlFlow<()>,
) -> ControlFlow<(), WatchSet> {
    let (result, mut set) = try_build_watched(opts);
    match result {
        Ok(build) => {
            let written = pipeline::write_output(&build, out, &opts.output);
            watch(watcher, &set);
            match written {
                Ok(written) => report(WatchStatus::Built {
                    build: &build,
                    written: &written,
                })?,
                Err(error) => report(WatchStatus::Failed(BuildFailure {
                    error,
                    diagnostics: build.diagnostics.clone(),
                }))?,
            }
            *last_good = Some(set.clone());
            ControlFlow::Continue(set)
        }
        Err(failure) => {
            if let Some(good) = last_good {
                set.merge(good);
            }
            watch(watcher, &set);
            report(WatchStatus::Failed(failure))?;
            ControlFlow::Continue(set)
        }
    }
}

/// [`Watcher::update`] to `set`; an error (some directory cannot be watched) is logged.
fn watch(watcher: &mut Watcher, set: &WatchSet) {
    if let Err(err) = watcher.update(set) {
        tracing::warn!("{err}");
    }
}

/// The next batch to rebuild for: every batch waiting in `rx` merged, without the output's
/// own paths ([`is_output_path`]), reclassified with the theme directories of `set`.
/// `None` when the watcher is gone.
fn next_batch(rx: &Receiver<ChangeBatch>, out: &Path, set: &WatchSet) -> Option<ChangeBatch> {
    let out = canonical(out);
    let roots: Vec<&PathBuf> = set.dirs_recursive.iter().chain(&set.theme_dirs).collect();
    loop {
        let mut paths: BTreeSet<PathBuf> = rx.recv().ok()?.paths.into_iter().collect();
        while let Ok(more) = rx.try_recv() {
            paths.extend(more.paths);
        }
        paths.retain(|path| !is_output_path(path, &out, &roots));
        if !paths.is_empty() {
            let paths: Vec<PathBuf> = paths.into_iter().collect();
            let kind = classify(&paths, &set.theme_dirs);
            return Some(ChangeBatch { paths, kind });
        }
    }
}

/// Whether a change of `path` is the build's own writing: `path` is the output `out` or
/// below it (a site), or a directory above `out` that lies inside one of the recursively
/// watched `roots` (writing the output creates it; changes to the roots themselves count).
fn is_output_path(path: &Path, out: &Path, roots: &[&PathBuf]) -> bool {
    path.starts_with(out)
        || (out.starts_with(path)
            && roots
                .iter()
                .any(|root| path != root.as_path() && path.starts_with(root)))
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
            "assets/lib/.git/HEAD",
            "styles/x.css~",
            "styles/.#x.css",
            "styles/.x.css.swp",
            "styles/.x.css.swx",
            // vim's swap files of further sessions count down from .swp.
            "styles/.x.css.swo",
            "templates/.deck.html.swn",
            "styles/.x.css.swa",
            // Emacs auto-save files.
            "styles/#x.css#",
            "#theme.toml#",
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
            // A theme's own dist/ and target/ may hold inputs (audit F18).
            "target/debug/x.css",
            "dist/slides.html",
            "scripts/dist/lib.js",
            "assets/lib/dist/lib.css",
            "styles/sub/target/x.css",
            "styles/x.swp.css",
            "styles/x.swo",
            "styles/.x.css.swq",
            "styles/.x.css.sw",
            "styles/.swo",
            "assets/#",
            "assets/#hash",
            "assets/hash#",
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
        // The filter holds canonical paths (on Windows: `\\?\D:\w`, on the current drive).
        let c = |path: &str| canonical(Path::new(path));
        let targets: Vec<(PathBuf, bool)> = filter
            .targets
            .iter()
            .map(|(dir, mode)| (dir.clone(), *mode == RecursiveMode::Recursive))
            .collect();
        assert_eq!(
            targets,
            vec![
                (c("/w"), false),
                (c("/w/img"), false),
                (c("/w/other"), true),
                (c("/w/theme"), true)
            ]
        );
        assert!(filter.matches(&c("/w/slides.md")));
        assert!(filter.matches(&c("/w/theme/styles/x.css")));
        assert!(filter.matches(&c("/w/theme/extra.toml")));
        assert!(!filter.matches(&c("/w/notes.txt")));
        assert!(!filter.matches(&c("/w/img/unrelated.png")));
        assert!(!filter.matches(&c("/w/theme/.git/HEAD")));
        assert!(!filter.matches(&c("/w/theme/styles/x.css~")));
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
        let set = WatchSet::new([root.join("slides.md")], [theme.clone()]);
        let next = next_batch(&rx, &out, &set).unwrap();
        assert_eq!(
            next.paths,
            vec![theme.join("styles/x.css"), theme.join("styles/y.css")]
        );
        assert_eq!(next.kind, ChangeKind::Css);

        // A batch of output paths only is skipped; a closed channel ends the loop.
        tx.send(batch(&["theme/out/index.html"])).unwrap();
        drop(tx);
        assert_eq!(next_batch(&rx, &out, &set), None);
    }

    #[test]
    fn next_batch_drops_the_directories_the_output_needs_inside_a_theme() {
        let dir = tempfile::tempdir().unwrap();
        let root = canonical(dir.path());
        let theme = root.join("theme");
        let set = WatchSet::new([root.join("slides.md")], [theme.clone()]);
        let out = theme.join("build/sub/out.html");
        let (tx, rx) = mpsc::channel();
        // Writing the output created `theme/build/sub/`; the theme directory itself and
        // directories above it are other changes.
        let paths = ["theme/build", "theme/build/sub", "theme/build/sub/out.html"];
        tx.send(ChangeBatch {
            paths: paths.iter().map(|name| root.join(name)).collect(),
            kind: ChangeKind::Content,
        })
        .unwrap();
        tx.send(ChangeBatch {
            paths: vec![theme.clone(), root.clone(), theme.join("build-notes")],
            kind: ChangeKind::Content,
        })
        .unwrap();
        let next = next_batch(&rx, &out, &set).unwrap();
        assert_eq!(
            next.paths,
            vec![root.clone(), theme.clone(), theme.join("build-notes")]
        );

        // Outside the theme, directories above the output are changes (a missing image's
        // directory may be one of them).
        let flat_out = root.join("figs/out.html");
        tx.send(ChangeBatch {
            paths: vec![root.join("figs")],
            kind: ChangeKind::Content,
        })
        .unwrap();
        let next = next_batch(&rx, &flat_out, &set).unwrap();
        assert_eq!(next.paths, vec![root.join("figs")]);
    }

    #[test]
    fn missing_targets_are_watched_through_their_nearest_existing_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let root = canonical(dir.path());
        std::fs::create_dir_all(root.join("t2")).unwrap();
        std::fs::create_dir_all(root.join("other")).unwrap();
        use RecursiveMode::{NonRecursive, Recursive};
        let targets: BTreeMap<PathBuf, RecursiveMode> = [
            (root.clone(), NonRecursive),
            (root.join("a/b"), NonRecursive),
            (root.join("theme"), Recursive),
            (root.join("t2"), Recursive),
            (root.join("t2/x/y"), NonRecursive),
            (root.join("other/c/d"), NonRecursive),
        ]
        .into_iter()
        .collect();
        let (wanted, missing) = plan_watches(&targets);
        // `a/b` and `theme` are reported through the watched root, `t2/x/y` through the
        // recursive `t2`; `other/c/d` needs `other` watched.
        assert_eq!(
            wanted.into_iter().collect::<Vec<_>>(),
            vec![
                (root.clone(), NonRecursive),
                (root.join("other"), NonRecursive),
                (root.join("t2"), Recursive),
            ]
        );
        assert_eq!(
            missing.iter().cloned().collect::<Vec<_>>(),
            vec![
                root.join("a/b"),
                root.join("other/c/d"),
                root.join("t2/x/y"),
                root.join("theme"),
            ]
        );

        // Creating or removing a directory on the way to a missing target (or a target)
        // changes what can be watched; other directories do not.
        let shared = Shared {
            filter: Filter {
                targets,
                ..Filter::default()
            },
            missing,
            recreated: BTreeSet::new(),
            since: SystemTime::now(),
        };
        assert!(shared.is_structural(&root));
        for path in ["a", "a/b", "other", "other/c", "theme", "t2"] {
            assert!(shared.is_structural(&root.join(path)), "{path}");
        }
        for path in ["b", "a/b/c", "other/x", "t2/z"] {
            assert!(!shared.is_structural(&root.join(path)), "{path}");
        }
    }

    #[test]
    fn theme_candidates_follow_the_chain_as_far_as_it_reads() {
        let dir = tempfile::tempdir().unwrap();
        let root = canonical(dir.path());
        let write = |rel: &str, text: &str| {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write("slides.md", "# Hi\n");
        write(
            "child/theme.toml",
            "name = \"child\"\nextends = \"parent\"\n",
        );
        write(
            "child/parent/theme.toml",
            "name = \"p\"\nextends = \"../gone\"\nx = 1\n",
        );
        let opts = BuildOptions {
            cli: crate::config::CliOverrides {
                theme: Some("child".into()),
                ..Default::default()
            },
            ..BuildOptions::new(root.join("slides.md"))
        };
        let deck = placeholder_deck(&opts);
        let config_path = root.join("presenter.toml");
        let dirs = theme_candidates(&opts, &deck, &Config::default(), Some(&config_path));
        let dirs: BTreeSet<PathBuf> = canonical_set(dirs);
        let cwd = canonical(&std::env::current_dir().unwrap().join("child"));
        let expected: BTreeSet<PathBuf> = [
            root.join("child"),
            root.join("child/parent"),
            root.join("child/gone"),
            cwd,
        ]
        .into_iter()
        .collect();
        assert_eq!(dirs, expected);

        // An unreadable manifest ends the chain there.
        write("child/theme.toml", "name = \"child\"\nextends = [\n");
        let dirs = canonical_set(theme_candidates(&opts, &deck, &Config::default(), None));
        assert!(dirs.contains(&root.join("child")));
        assert!(!dirs.contains(&root.join("child/parent")));
    }

    #[test]
    fn watcher_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Watcher>();
    }
}
