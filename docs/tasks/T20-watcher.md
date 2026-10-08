# T20 — File watcher, debounce, rebuild loop, change classification

| | |
|---|---|
| Milestone | M2 |
| Size | M |
| Depends on | T16 |
| Parallel-safe with | T21, T22, T24, T17, T31, T32 |
| Files you own | `src/serve/watch.rs`, `src/cli/build.rs` (`--watch` wiring, additive), `tests/watch.rs` |

## Goal
Notice when any input of the last build changes (source, config, theme chain, referenced
images), coalesce bursts, classify the change as `css` or `content`, and drive rebuilds.
Used by `presenter serve` (T23) and `presenter build --watch`.

## Read first
- `docs/spec/live-reload.md` §2 (kind classification), §4–§5 (lifecycle, watch set)

## API to implement
```rust
pub struct WatchSet { pub files: Vec<PathBuf>, pub dirs_recursive: Vec<PathBuf>, pub dirs_flat: Vec<PathBuf>, pub theme_dirs: Vec<PathBuf> }
pub fn watch_set_for(build: &Build) -> WatchSet;                 // live-reload.md §5
pub fn watch_set_for_failed(opts: &BuildOptions) -> WatchSet;    // just the source file + config + theme dir if known
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum ChangeKind { Css, Content }
pub struct ChangeBatch { pub paths: Vec<PathBuf>, pub kind: ChangeKind }
pub fn classify(paths: &[PathBuf], theme_dirs: &[PathBuf]) -> ChangeKind;
pub struct Watcher { /* notify_debouncer_full::Debouncer<RecommendedWatcher, RecommendedCache> */ }
impl Watcher {
    pub fn start(set: &WatchSet, tx: std::sync::mpsc::Sender<ChangeBatch>) -> Result<Watcher>;
    pub fn update(&mut self, set: &WatchSet) -> Result<()>;      // re-watch after a build changed the inputs
}
/// Blocking loop used by `build --watch`: rebuild on every batch, print status lines.
pub fn run_build_watch(opts: BuildOptions, out: PathBuf) -> Result<()>;
```

## Steps
1. `notify_debouncer_full::new_debouncer(Duration::from_millis(150), None, handler)`; the
   handler receives `DebounceEventResult`; collect `event.paths`, ignore events whose paths
   are all in an ignore list (`.git/`, `target/`, `dist/`, editor temp files `*~`, `.#*`, `*.swp`, `4913`),
   send one `ChangeBatch`.
2. Watch files by watching their **parent directory** non-recursively (editors replace files
   via rename; watching the file itself loses the watch). Filter events to the watched file set
   plus anything under recursive dirs.
3. `classify`: all paths end in `.css/.scss/.sass` or are under `<theme>/styles/` or `<theme>/assets/` → `Css`; else `Content`.
4. `update`: compute the diff between old and new sets; `unwatch` removed paths, `watch` added ones.
5. `run_build_watch`: initial build (errors printed, keep going), then loop on `rx.recv()`;
   on `Content` rebuild + write; on `Css` rebuild too (standalone output embeds CSS) but log "css".

## Acceptance criteria
- `tests/watch.rs` (tempdir): create `a.md` + `theme/styles/x.css`; start watcher; append to `a.md` → a batch
  with kind `Content` within 5 s; touch `x.css` → `Css`; write both within 50 ms → one batch `Content`.
  Use generous timeouts (macOS FSEvents can take ~1 s); mark the test `#[ignore]` on CI only if it proves flaky and say so.
- Rename-replace (`write to a.md.tmp` then `rename` over `a.md`) still produces a batch and later edits are still seen.
- `classify` unit tests for each rule.
- `presenter build examples/basic/slides.md --watch -o /tmp/x.html` rebuilds on edit (manual check in PR).

## Out of scope
HTTP server, SSE, browser client (T21/T22/T23).

## Hints (verified)
- `notify-debouncer-full` 0.7 re-exports `notify`. Depending on the minor version, watching is either
  `debouncer.watch(&path, RecursiveMode::Recursive)` or `debouncer.watcher().watch(...)` — check
  docs.rs for 0.7 before writing code; both forms have existed.
- notify 8 on macOS defaults to FSEvents; events may arrive with paths that are symlink-resolved (`/private/tmp/...` vs `/tmp/...`): canonicalize both the watch set and incoming paths before comparing.
