//! T20: the file watcher (`presenter::serve::watch`) against the real OS backend (FSEvents
//! on macOS, inotify on Linux), the watch sets of successful and failed builds, the rebuild
//! loop in process, and `presenter build --watch` end to end.
//!
//! Every test works in its own temporary directory. Timeouts are generous: FSEvents can
//! take about a second to deliver, and a batch is sent only once the changes have been
//! quiet for the debounce time (150 ms).

use std::fs::OpenOptions;
use std::io::Write as _;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use presenter::pipeline::{self, BuildOptions};
use presenter::serve::watch::{
    ChangeBatch, ChangeKind, DEBOUNCE, WatchSet, WatchStatus, Watcher, canonical,
    run_build_watch_with, try_build_watched, watch_set_before_build, watch_set_for,
    watch_set_for_failed,
};

/// How long a batch may take to arrive.
const BATCH_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to wait to be sure that no (further) batch comes.
const QUIET: Duration = Duration::from_millis(1200);

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn append(path: &Path, text: &str) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

/// Sets the modification time of `path` to now without writing to it, like `touch`.
fn touch(path: &Path) {
    let file = OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now()).unwrap();
}

/// Longer than a batch takes to form, so that the next change is a batch of its own.
fn pause() {
    std::thread::sleep(DEBOUNCE + Duration::from_millis(1000));
}

/// A deck `a.md` and a theme `theme/` with `styles/x.css`, in a fresh temporary directory.
fn deck_and_theme() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let deck = dir.path().join("a.md");
    let theme = dir.path().join("theme");
    let css = theme.join("styles/x.css");
    write(&deck, "# A\n");
    write(&theme.join("theme.toml"), "name = \"t\"\n");
    write(&css, "body { color: red; }\n");
    (dir, deck, theme, css)
}

/// Starts a watcher on `set` and waits until it is surely running, discarding anything
/// it reports for the files just created.
fn start(set: &WatchSet) -> (Watcher, Receiver<ChangeBatch>) {
    let (tx, rx) = mpsc::channel();
    let watcher = Watcher::start(set, tx).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    drain(&rx, Duration::from_millis(400));
    (watcher, rx)
}

/// The next batch; fails the test after [`BATCH_TIMEOUT`].
fn next(rx: &Receiver<ChangeBatch>, what: &str) -> ChangeBatch {
    match rx.recv_timeout(BATCH_TIMEOUT) {
        Ok(batch) => batch,
        Err(err) => panic!("no batch after {what} within {BATCH_TIMEOUT:?}: {err}"),
    }
}

/// Every batch that arrives until none has arrived for `quiet`.
fn drain(rx: &Receiver<ChangeBatch>, quiet: Duration) -> Vec<ChangeBatch> {
    let mut batches = Vec::new();
    loop {
        match rx.recv_timeout(quiet) {
            Ok(batch) => batches.push(batch),
            Err(RecvTimeoutError::Timeout) => return batches,
            Err(RecvTimeoutError::Disconnected) => panic!("watcher stopped"),
        }
    }
}

#[test]
fn edits_produce_classified_batches_and_bursts_coalesce() {
    let (_dir, deck, theme, css) = deck_and_theme();
    let set = WatchSet::new([deck.clone()], [theme]);
    let (_watcher, rx) = start(&set);

    append(&deck, "\nmore\n");
    let batch = next(&rx, "appending to a.md");
    assert_eq!(batch.kind, ChangeKind::Content);
    assert_eq!(batch.paths, vec![canonical(&deck)]);
    assert_eq!(drain(&rx, QUIET), vec![]);

    write(&css, "body { color: blue; }\n");
    let batch = next(&rx, "writing x.css");
    assert_eq!(batch.kind, ChangeKind::Css);
    assert_eq!(batch.paths, vec![canonical(&css)]);
    assert_eq!(drain(&rx, QUIET), vec![]);

    // `touch`: only the modification time changes (FSEvents: inode metadata; inotify:
    // IN_ATTRIB).
    touch(&css);
    let batch = next(&rx, "touching x.css");
    assert_eq!(batch.kind, ChangeKind::Css);
    assert_eq!(batch.paths, vec![canonical(&css)]);
    assert_eq!(drain(&rx, QUIET), vec![]);

    // Both within 50 ms: one batch, content (one path is not a stylesheet).
    append(&deck, "\neven more\n");
    std::thread::sleep(Duration::from_millis(30));
    write(&css, "body { color: green; }\n");
    let batch = next(&rx, "writing both files");
    assert_eq!(batch.kind, ChangeKind::Content);
    assert_eq!(batch.paths, vec![canonical(&deck), canonical(&css)]);
    assert_eq!(drain(&rx, QUIET), vec![], "a second batch for the burst");
}

#[test]
fn rename_replace_is_seen_and_later_edits_too() {
    let (dir, deck, theme, _css) = deck_and_theme();
    let set = WatchSet::new([deck.clone()], [theme]);
    let (_watcher, rx) = start(&set);
    let tmp = dir.path().join("a.md.tmp");

    for round in 1..=2 {
        // How vim and VS Code ("safe write") save: a new file renamed over the old one.
        write(&tmp, &format!("# A\n\nround {round}\n"));
        std::fs::rename(&tmp, &deck).unwrap();
        let batches = drain_at_least_one(&rx, "rename-replacing a.md");
        for batch in &batches {
            assert_eq!(batch.paths, vec![canonical(&deck)], "round {round}");
            assert_eq!(batch.kind, ChangeKind::Content);
        }

        append(&deck, "\nedited after the rename\n");
        let batch = next(&rx, "editing a.md after the rename");
        assert_eq!(batch.paths, vec![canonical(&deck)]);
        assert_eq!(batch.kind, ChangeKind::Content);
        assert_eq!(drain(&rx, QUIET), vec![]);
    }
}

/// The next batch and any that follow it within [`QUIET`].
fn drain_at_least_one(rx: &Receiver<ChangeBatch>, what: &str) -> Vec<ChangeBatch> {
    let mut batches = vec![next(rx, what)];
    batches.extend(drain(rx, QUIET));
    batches
}

#[test]
fn unrelated_and_ignored_paths_do_not_count() {
    let (dir, deck, theme, _css) = deck_and_theme();
    // Creating a directory inside a theme is a change; `templates/` exists beforehand.
    std::fs::create_dir(theme.join("templates")).unwrap();
    let set = WatchSet::new([deck.clone()], [theme.clone()]);
    let (_watcher, rx) = start(&set);

    // Next to the deck but not an input; inside the theme but on the ignore list.
    write(&dir.path().join("notes.txt"), "x");
    write(&dir.path().join("out.html"), "x");
    write(&theme.join("styles/x.css~"), "x");
    write(&theme.join("styles/.x.css.swp"), "x");
    write(&theme.join("templates/4913"), "x");
    write(&theme.join(".git/HEAD"), "x");
    assert_eq!(drain(&rx, QUIET), vec![]);

    write(&theme.join("templates/deck.html"), "{{ x }}");
    write(&dir.path().join("notes.txt"), "y");
    let batch = next(&rx, "writing a template");
    assert_eq!(
        batch.paths,
        vec![canonical(&theme.join("templates/deck.html"))]
    );
    assert_eq!(batch.kind, ChangeKind::Content);
    assert_eq!(drain(&rx, QUIET), vec![]);
}

/// Audit F18: a theme may keep inputs in directories named `dist` or `target` (a bundled
/// script entry, a vendored library's stylesheet), at any depth; editing them counts like
/// any other theme file. Only `.git/` and editor files are ignored.
#[test]
fn theme_files_in_dist_and_target_directories_count() {
    let (_dir, deck, theme, _css) = deck_and_theme();
    let lib = theme.join("scripts/dist/lib.js");
    let vendored = theme.join("styles/target/x.css");
    let top = theme.join("dist/extra.css");
    write(&lib, "window.LIB = 1;\n");
    write(&vendored, "a { b: c; }\n");
    write(&top, "a { b: c; }\n");
    let set = WatchSet::new([deck], [theme.clone()]);
    let (_watcher, rx) = start(&set);

    write(&lib, "window.LIB = 2;\n");
    let batch = next(&rx, "editing scripts/dist/lib.js");
    assert_eq!(batch.paths, vec![canonical(&lib)]);
    assert_eq!(batch.kind, ChangeKind::Content);
    assert_eq!(drain(&rx, QUIET), vec![]);

    write(&vendored, "a { b: d; }\n");
    let batch = next(&rx, "editing styles/target/x.css");
    assert_eq!(batch.paths, vec![canonical(&vendored)]);
    assert_eq!(batch.kind, ChangeKind::Css);
    assert_eq!(drain(&rx, QUIET), vec![]);

    write(&top, "a { b: d; }\n");
    let batch = next(&rx, "editing dist/extra.css");
    assert_eq!(batch.paths, vec![canonical(&top)]);
    assert_eq!(drain(&rx, QUIET), vec![]);
}

#[test]
fn update_watches_the_new_set() {
    let (dir, deck, theme, css) = deck_and_theme();
    let other = dir.path().join("b.md");
    write(&other, "# B\n");
    let (mut watcher, rx) = start(&WatchSet::new([deck.clone()], []));
    let root = canonical(dir.path());
    assert_eq!(watcher.watched_dirs(), vec![(root.clone(), false)]);

    write(&css, "body { color: blue; }\n");
    assert_eq!(drain(&rx, QUIET), vec![], "the theme is not watched yet");

    watcher
        .update(&WatchSet::new([other.clone()], [theme.clone()]))
        .unwrap();
    assert_eq!(
        watcher.watched_dirs(),
        vec![(root.clone(), false), (root.join("theme"), true)]
    );
    // The update sends what changed since the last batch (here: since the start) in the
    // inputs it now watches: the stylesheet written while the theme was not watched.
    let batch = next(&rx, "updating the watch set");
    assert_eq!(batch.paths, vec![canonical(&css)]);
    assert_eq!(batch.kind, ChangeKind::Css);
    std::thread::sleep(Duration::from_millis(300));

    append(&deck, "\nno longer an input\n");
    write(&css, "body { color: green; }\n");
    let batch = next(&rx, "touching x.css after the update");
    assert_eq!(batch.paths, vec![canonical(&css)]);
    assert_eq!(batch.kind, ChangeKind::Css);
    assert_eq!(drain(&rx, QUIET), vec![]);

    append(&other, "\nnow an input\n");
    let batch = next(&rx, "appending to b.md");
    assert_eq!(batch.paths, vec![canonical(&other)]);
    assert_eq!(drain(&rx, QUIET), vec![]);

    // Unwatching the theme again.
    watcher.update(&WatchSet::new([other], [])).unwrap();
    assert_eq!(watcher.watched_dirs(), vec![(root, false)]);
}

#[test]
fn a_dropped_watcher_closes_the_channel() {
    let (_dir, deck, _theme, _css) = deck_and_theme();
    let (watcher, rx) = start(&WatchSet::new([deck], []));
    drop(watcher);
    match rx.recv_timeout(BATCH_TIMEOUT) {
        Err(RecvTimeoutError::Disconnected) => {}
        other => panic!("expected the channel to close, got {other:?}"),
    }
}

/// The built-in theme directory a debug build also watches.
fn builtin_theme_dirs() -> Vec<PathBuf> {
    if cfg!(debug_assertions) {
        vec![canonical(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/themes/default"),
        )]
    } else {
        Vec::new()
    }
}

/// A project: `presenter.toml` choosing the disk theme `mytheme/` (a child of the
/// built-in default), `figures/plot.png`, and `slides.md` with `deck` as its text.
fn project(deck: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir.path().join("presenter.toml"),
        "[deck]\ntheme = \"mytheme\"\n",
    );
    write(
        &dir.path().join("mytheme/theme.toml"),
        "name = \"mytheme\"\nextends = \"default\"\n",
    );
    let png = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/basic/figures/plot.png"),
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("figures")).unwrap();
    std::fs::write(dir.path().join("figures/plot.png"), png).unwrap();
    let input = dir.path().join("slides.md");
    write(&input, deck);
    (dir, input)
}

fn sorted(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    paths
}

#[test]
fn watch_set_of_a_build_lists_every_input() {
    let (dir, input) = project(
        "# Hi\n\n![plot](figures/plot.png)\n\n---\n\n![gone](figures/missing.png)\n\n\
         ![elsewhere](other/none.png)\n",
    );
    let opts = BuildOptions {
        missing_asset_is_error: false,
        ..BuildOptions::new(&input)
    };
    let build = pipeline::build_deck(&opts).unwrap();
    let set = watch_set_for(&build);

    let root = canonical(dir.path());
    assert_eq!(
        set.files,
        sorted(vec![
            root.join("slides.md"),
            root.join("presenter.toml"),
            root.join("figures/plot.png"),
            root.join("figures/missing.png"),
            root.join("other/none.png"),
        ])
    );
    assert_eq!(
        set.dirs_flat,
        vec![root.clone(), root.join("figures"), root.join("other")]
    );
    let mut themes = builtin_theme_dirs();
    themes.push(root.join("mytheme"));
    assert_eq!(set.theme_dirs, sorted(themes));
    assert_eq!(set.dirs_recursive, set.theme_dirs);
}

#[test]
fn watch_set_of_a_failed_build_still_has_the_known_inputs() {
    // A missing asset (an error in `build`): the deck parses, so its assets and theme are known.
    let (dir, input) = project("# Hi\n\n![gone](figures/missing.png)\n");
    let opts = BuildOptions::new(&input);
    assert!(pipeline::build_deck(&opts).is_err());
    let set = watch_set_for_failed(&opts);
    let root = canonical(dir.path());
    assert_eq!(
        set.files,
        sorted(vec![
            root.join("slides.md"),
            root.join("presenter.toml"),
            root.join("figures/missing.png"),
        ])
    );
    let mut themes = builtin_theme_dirs();
    themes.push(root.join("mytheme"));
    assert_eq!(set.theme_dirs, sorted(themes.clone()));

    // Invalid front matter: the deck does not parse; presenter.toml still names the theme.
    write(&input, "---\ntitle: [unclosed\n---\n\n# Hi\n");
    assert!(pipeline::build_deck(&opts).is_err());
    let set = watch_set_for_failed(&opts);
    assert_eq!(
        set.files,
        sorted(vec![root.join("slides.md"), root.join("presenter.toml")])
    );
    assert_eq!(set.theme_dirs, sorted(themes));

    // An invalid presenter.toml is still watched (fixing it must rebuild).
    write(&dir.path().join("presenter.toml"), "[deck\n");
    assert!(pipeline::build_deck(&opts).is_err());
    let set = watch_set_for_failed(&opts);
    assert_eq!(
        set.files,
        sorted(vec![root.join("slides.md"), root.join("presenter.toml")])
    );
    assert_eq!(set.theme_dirs, builtin_theme_dirs());
    assert_eq!(set.dirs_flat, vec![root]);
}

/// A `presenter build --watch` child process, killed when dropped.
struct WatchProcess {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl WatchProcess {
    fn spawn(dir: &Path, args: &[&str]) -> WatchProcess {
        let stdout = dir.join("stdout.txt");
        let stderr = dir.join("stderr.txt");
        let child = Command::new(env!("CARGO_BIN_EXE_presenter"))
            .args(args)
            .current_dir(dir)
            .env_remove("RUST_LOG")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(&stdout).unwrap())
            .stderr(std::fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        WatchProcess {
            child,
            stdout,
            stderr,
        }
    }

    fn stdout(&self) -> String {
        std::fs::read_to_string(&self.stdout).unwrap()
    }

    fn stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap()
    }

    /// Waits up to `timeout` for `done` to hold, failing with the process output.
    fn wait_for(&mut self, what: &str, timeout: Duration, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if done(self) {
                return;
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!(
                    "presenter exited ({status}) while waiting for {what}\nstdout:\n{}\nstderr:\n{}",
                    self.stdout(),
                    self.stderr()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "timed out waiting for {what}\nstdout:\n{}\nstderr:\n{}",
            self.stdout(),
            self.stderr()
        );
    }
}

impl Drop for WatchProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn contains(path: &Path, text: &str) -> bool {
    std::fs::read_to_string(path).is_ok_and(|html| html.contains(text))
}

#[test]
fn build_watch_rebuilds_on_every_change() {
    let dir = tempfile::tempdir().unwrap();
    let deck = dir.path().join("slides.md");
    write(&deck, "# Watch test\n\nFirst slide.\n");
    let out = dir.path().join("out/deck.html");
    let mut process = WatchProcess::spawn(
        dir.path(),
        &["build", "slides.md", "--watch", "-o", "out/deck.html"],
    );
    process.wait_for("the first build", Duration::from_secs(30), |p| {
        p.stderr().contains("Watching for changes (Ctrl-C to stop)")
    });
    assert!(contains(&out, "First slide."));
    assert!(
        process
            .stdout()
            .starts_with("Wrote out/deck.html (1 slide, "),
        "{}",
        process.stdout()
    );

    // An edit rebuilds.
    append(&deck, "\n---\n\n## Marker one\n");
    process.wait_for("the rebuild after an edit", Duration::from_secs(15), |_| {
        contains(&out, "Marker one")
    });

    // A failed build is reported and watching goes on; the missing image is watched.
    append(&deck, "\n---\n\n## Marker two\n\n![figure](fig.png)\n");
    process.wait_for("the missing-asset error", Duration::from_secs(15), |p| {
        p.stderr().contains("asset not found")
    });
    assert!(!contains(&out, "Marker two"));
    let png = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/basic/figures/plot.png"),
    )
    .unwrap();
    std::fs::write(dir.path().join("fig.png"), png).unwrap();
    // The output is written before its `Wrote` line is printed: wait for the line.
    process.wait_for(
        "the rebuild after adding the image",
        Duration::from_secs(15),
        |p| p.stdout().lines().count() == 3,
    );
    assert!(contains(&out, "Marker two"));

    let stdout = process.stdout();
    let wrote: Vec<&str> = stdout.lines().collect();
    assert_eq!(wrote.len(), 3, "{stdout}");
    assert!(
        wrote
            .iter()
            .all(|line| line.starts_with("Wrote out/deck.html ("))
    );
    assert!(wrote[2].contains("(3 slides, "), "{stdout}");
    let stderr = process.stderr();
    assert!(
        stderr.contains("Rebuilding (content): slides.md"),
        "{stderr}"
    );
    assert!(stderr.contains("Rebuilding (content): fig.png"), "{stderr}");
    assert!(stderr.contains("--> slides.md:13\n"), "{stderr}");
}

/// Whether some batch of `batches` lists `path`.
fn mentions(batches: &[ChangeBatch], path: &Path) -> bool {
    batches
        .iter()
        .any(|batch| batch.paths.iter().any(|p| p == path))
}

#[test]
fn a_recreated_directory_is_watched_again() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(dir.path());
    let deck = root.join("slides.md");
    let figs = root.join("figs");
    let png = figs.join("plot.png");
    write(&deck, "# A\n");
    write(&png, "v0");
    let set = WatchSet::new([deck.clone(), png.clone()], []);
    let (mut watcher, rx) = start(&set);
    assert_eq!(
        watcher.watched_dirs(),
        vec![(root.clone(), false), (figs.clone(), false)]
    );

    std::fs::remove_dir_all(&figs).unwrap();
    let batches = drain_at_least_one(&rx, "removing figs/");
    assert!(
        mentions(&batches, &figs) || mentions(&batches, &png),
        "{batches:?}"
    );
    watcher.update(&set).unwrap();
    // `figs/` is gone: the deck's directory (watched anyway) reports its return.
    assert_eq!(watcher.watched_dirs(), vec![(root.clone(), false)]);

    pause();
    write(&png, "v1");
    let batches = drain_at_least_one(&rx, "recreating figs/plot.png");
    assert!(mentions(&batches, &figs), "{batches:?}");
    watcher.update(&set).unwrap();
    assert_eq!(
        watcher.watched_dirs(),
        vec![(root.clone(), false), (figs.clone(), false)]
    );
    // plot.png was written before that batch was sent: the rebuild for it reads it.
    assert_eq!(drain(&rx, QUIET), vec![]);

    append(&png, "v2");
    let batch = next(&rx, "editing the recreated figs/plot.png");
    assert_eq!(batch.paths, vec![png]);
    assert_eq!(drain(&rx, QUIET), vec![]);
}

#[test]
fn the_decks_directory_can_be_removed_and_created_again() {
    // `git checkout` of a branch without it, or a generator doing `rm -rf gen && …`.
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(dir.path());
    let gen_dir = root.join("gen");
    let deck = gen_dir.join("slides.md");
    write(&deck, "# Generated\n");
    let set = WatchSet::new([deck.clone()], []);
    let (mut watcher, rx) = start(&set);

    std::fs::remove_dir_all(&gen_dir).unwrap();
    drain_at_least_one(&rx, "removing gen/");
    watcher.update(&set).unwrap();
    assert_eq!(watcher.watched_dirs(), vec![(root.clone(), false)]);

    // Longer than the batch window: the removal and the creation are separate batches.
    pause();
    write(&deck, "# Generated again\n");
    let batches = drain_at_least_one(&rx, "creating gen/ again");
    assert!(mentions(&batches, &gen_dir), "{batches:?}");
    watcher.update(&set).unwrap();
    assert_eq!(watcher.watched_dirs(), vec![(gen_dir, false)]);

    append(&deck, "\nmore\n");
    let batch = next(&rx, "editing the regenerated deck");
    assert_eq!(batch.paths, vec![deck]);
    assert_eq!(drain(&rx, QUIET), vec![]);
}

#[test]
fn a_missing_file_two_directories_down_is_seen_when_created() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(dir.path());
    let deck = root.join("slides.md");
    let png = root.join("figs/sub/plot.png");
    write(&deck, "# A\n");
    let set = WatchSet::new([deck.clone(), png.clone()], []);
    let (mut watcher, rx) = start(&set);
    assert_eq!(watcher.watched_dirs(), vec![(root.clone(), false)]);

    // One directory at a time: each is a change, and each update watches one level deeper.
    std::fs::create_dir(root.join("figs")).unwrap();
    let batches = drain_at_least_one(&rx, "creating figs/");
    assert!(mentions(&batches, &root.join("figs")), "{batches:?}");
    watcher.update(&set).unwrap();
    assert_eq!(
        watcher.watched_dirs(),
        vec![(root.clone(), false), (root.join("figs"), false)]
    );

    std::fs::create_dir(root.join("figs/sub")).unwrap();
    let batches = drain_at_least_one(&rx, "creating figs/sub/");
    assert!(mentions(&batches, &root.join("figs/sub")), "{batches:?}");
    watcher.update(&set).unwrap();
    assert_eq!(
        watcher.watched_dirs(),
        vec![(root.clone(), false), (root.join("figs/sub"), false)]
    );

    write(&png, "png");
    let batch = next(&rx, "creating figs/sub/plot.png");
    assert_eq!(batch.paths, vec![png.clone()]);
    assert_eq!(drain(&rx, QUIET), vec![]);

    // All at once (`mkdir -p figs/sub && cp …`): one batch for the top directory.
    std::fs::remove_dir_all(root.join("figs")).unwrap();
    drain_at_least_one(&rx, "removing figs/");
    watcher.update(&set).unwrap();
    assert_eq!(watcher.watched_dirs(), vec![(root.clone(), false)]);
    pause();
    write(&png, "png again");
    let batches = drain_at_least_one(&rx, "creating figs/sub/plot.png at once");
    assert!(mentions(&batches, &root.join("figs")), "{batches:?}");
}

#[test]
fn an_update_reports_what_changed_while_the_watches_changed() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(dir.path());
    let deck = root.join("slides.md");
    let png = root.join("other/x.png");
    write(&deck, "# A\n");
    write(&png, "v0");
    let (mut watcher, rx) = start(&WatchSet::new([deck.clone()], []));
    // `other/` is not watched yet; a build reads x.png, which changes before the update
    // that starts watching it.
    write(&png, "v1");
    assert_eq!(drain(&rx, QUIET), vec![]);
    watcher
        .update(&WatchSet::new([deck.clone(), png.clone()], []))
        .unwrap();
    let batch = next(&rx, "updating to a set with a changed file");
    assert_eq!(batch.paths, vec![png.clone()]);
    assert_eq!(drain(&rx, QUIET), vec![]);

    // An update that changes no watch reports nothing.
    watcher
        .update(&WatchSet::new([deck.clone(), png.clone()], []))
        .unwrap();
    assert_eq!(drain(&rx, QUIET), vec![]);

    // Nor do updates that change watches: x.png's change was reported already.
    watcher.update(&WatchSet::new([deck.clone()], [])).unwrap();
    assert_eq!(watcher.watched_dirs(), vec![(root.clone(), false)]);
    watcher
        .update(&WatchSet::new([deck.clone(), png.clone()], []))
        .unwrap();
    assert_eq!(watcher.watched_dirs().len(), 2);
    assert_eq!(drain(&rx, QUIET), vec![]);

    // A change while `other/` is unwatched is reported by the update that watches it.
    watcher.update(&WatchSet::new([deck.clone()], [])).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    write(&png, "v2");
    assert_eq!(drain(&rx, QUIET), vec![]);
    watcher
        .update(&WatchSet::new([deck, png.clone()], []))
        .unwrap();
    let batch = next(&rx, "watching other/ again");
    assert_eq!(batch.paths, vec![png]);
    assert_eq!(drain(&rx, QUIET), vec![]);
}

#[test]
fn watch_set_of_a_failed_build_has_the_theme_that_does_not_resolve() {
    let (dir, input) = project("# Hi\n");
    let root = canonical(dir.path());
    let mytheme = root.join("mytheme");
    let opts = BuildOptions::new(&input);

    // The theme's manifest is invalid TOML.
    write(
        &mytheme.join("theme.toml"),
        "name = \"mytheme\"\nextends = \"default\"\nbogus = [\n",
    );
    assert!(pipeline::build_deck(&opts).is_err());
    let set = watch_set_for_failed(&opts);
    assert!(set.theme_dirs.contains(&mytheme), "{set:?}");
    assert!(set.dirs_recursive.contains(&mytheme), "{set:?}");
    assert!(set.files.contains(&root.join("presenter.toml")), "{set:?}");
    assert!(watch_set_before_build(&opts).theme_dirs.contains(&mytheme));

    // The parent it extends does not exist: the theme and where the parent is looked for.
    write(
        &mytheme.join("theme.toml"),
        "name = \"mytheme\"\nextends = \"defualt\"\n",
    );
    assert!(pipeline::build_deck(&opts).is_err());
    let set = watch_set_for_failed(&opts);
    assert!(set.theme_dirs.contains(&mytheme), "{set:?}");
    assert!(set.theme_dirs.contains(&mytheme.join("defualt")), "{set:?}");

    // There is no theme directory yet: where it will be created.
    std::fs::remove_dir_all(&mytheme).unwrap();
    assert!(pipeline::build_deck(&opts).is_err());
    let set = watch_set_for_failed(&opts);
    assert!(set.theme_dirs.contains(&mytheme), "{set:?}");

    // Front matter naming a theme that does not exist (the deck parses).
    write(&input, "---\ntheme: other\n---\n\n# Hi\n");
    let set = watch_set_for_failed(&opts);
    assert!(set.theme_dirs.contains(&root.join("other")), "{set:?}");
}

/// What the rebuild loop reported, as text: `built <n>`, `failed: <error>`, `watching`,
/// `changed: <kind> <file names>`.
fn status_text(status: &WatchStatus<'_>) -> String {
    match status {
        WatchStatus::Built { build, .. } => format!("built {}", build.document.slide_count),
        WatchStatus::Failed(failure) => format!("failed: {}", failure.error),
        WatchStatus::Watching(_) => "watching".to_string(),
        WatchStatus::Changed(batch) => {
            let names: Vec<String> = batch
                .paths
                .iter()
                .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            format!("changed: {:?} {}", batch.kind, names.join(","))
        }
    }
}

/// `run_build_watch_with` on a thread. Every status goes to the returned receiver as
/// [`status_text`]; `on_status` runs first. The loop stops at the first status that
/// arrives after `stop` was set (then `done` is sent): the flag is read before the status
/// is sent, so setting it in reply to one status stops at the next.
fn run_loop(
    opts: BuildOptions,
    out: PathBuf,
    mut on_status: impl FnMut(&WatchStatus<'_>) + Send + 'static,
) -> (Receiver<String>, Arc<AtomicBool>) {
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    std::thread::spawn(move || {
        let result = run_build_watch_with(&opts, &out, |status| {
            let stop_now = thread_stop.load(Ordering::SeqCst);
            on_status(&status);
            let _ = tx.send(status_text(&status));
            if stop_now {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
        let _ = tx.send(format!("done {result:?}"));
    });
    (rx, stop)
}

/// The next status of [`run_loop`], within 30 s (a build is included).
fn next_status(rx: &Receiver<String>, what: &str) -> String {
    rx.recv_timeout(Duration::from_secs(30))
        .unwrap_or_else(|err| panic!("no status for {what}: {err}"))
}

#[test]
fn a_change_saved_while_the_first_build_runs_is_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let deck = dir.path().join("slides.md");
    write(&deck, "# One\n");
    let out = dir.path().join("out.html");
    let edited = deck.clone();
    let mut builds = 0;
    let (rx, stop) = run_loop(BuildOptions::new(&deck), out.clone(), move |status| {
        if matches!(status, WatchStatus::Built { .. }) {
            builds += 1;
            if builds == 1 {
                // The first build has read the deck: this save must not be lost.
                append(&edited, "\n---\n\n## Saved during the build\n");
            }
        }
    });
    assert_eq!(next_status(&rx, "the first build"), "built 1");
    assert_eq!(next_status(&rx, "watching"), "watching");
    assert_eq!(
        next_status(&rx, "the save during the first build"),
        "changed: Content slides.md"
    );
    stop.store(true, Ordering::SeqCst);
    assert_eq!(next_status(&rx, "the rebuild"), "built 2");
    assert_eq!(next_status(&rx, "the end of the loop"), "done Ok(())");
    assert!(contains(&out, "Saved during the build"));
}

#[test]
fn writing_the_output_inside_a_watched_theme_does_not_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let theme = dir.path().join("theme");
    write(
        &theme.join("theme.toml"),
        "name = \"t\"\nextends = \"default\"\n",
    );
    std::fs::create_dir(theme.join("styles")).unwrap();
    let deck = dir.path().join("slides.md");
    write(&deck, "# One\n");
    let mut opts = BuildOptions::new(&deck);
    opts.cli.theme = Some(theme.display().to_string());
    // The build creates theme/build/sub/ for its output.
    let out = theme.join("build/sub/out.html");
    let (rx, stop) = run_loop(opts, out.clone(), |_| {});
    assert_eq!(next_status(&rx, "the first build"), "built 1");
    assert_eq!(next_status(&rx, "watching"), "watching");
    match rx.recv_timeout(Duration::from_secs(2)) {
        Err(RecvTimeoutError::Timeout) => {}
        other => panic!("writing the output caused {other:?}"),
    }
    assert!(out.is_file());

    // The theme itself is still watched.
    stop.store(true, Ordering::SeqCst);
    write(&theme.join("styles/extra.css"), "p { color: red; }\n");
    assert_eq!(
        next_status(&rx, "a stylesheet in the theme"),
        "changed: Css extra.css"
    );
    assert_eq!(next_status(&rx, "the end of the loop"), "done Ok(())");
}

#[test]
fn build_watch_waits_for_an_invalid_presenter_toml_to_be_fixed() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("presenter.toml"), "[deck\n");
    write(&dir.path().join("slides.md"), "# Config test\n");
    let mut process = WatchProcess::spawn(
        dir.path(),
        &["build", "slides.md", "--watch", "-o", "out.html"],
    );
    process.wait_for("the config error", Duration::from_secs(30), |p| {
        p.stderr().contains("Watching for changes (Ctrl-C to stop)")
    });
    assert!(
        process.stderr().starts_with("error: "),
        "{}",
        process.stderr()
    );
    assert_eq!(process.stdout(), "");

    // Still broken: reported again.
    write(&dir.path().join("presenter.toml"), "[deck]\ntheme = 3\n");
    process.wait_for("the second config error", Duration::from_secs(15), |p| {
        p.stderr().matches("error: ").count() == 2
    });

    write(&dir.path().join("presenter.toml"), "[deck]\n");
    process.wait_for("the first build", Duration::from_secs(15), |p| {
        p.stdout().starts_with("Wrote out.html (1 slide, ")
    });
    let stderr = process.stderr();
    assert!(
        stderr.contains("Rebuilding (content): presenter.toml"),
        "{stderr}"
    );
    assert_eq!(
        stderr.matches("Watching for changes").count(),
        1,
        "{stderr}"
    );

    // Watching goes on like after any first build.
    append(&dir.path().join("slides.md"), "\n---\n\n## Two\n");
    process.wait_for("the rebuild", Duration::from_secs(15), |p| {
        p.stdout().contains("Wrote out.html (2 slides, ")
    });
}

#[test]
fn build_watch_follows_a_theme_being_created_and_fixed() {
    // Theme authoring: presenter.toml names `mytheme`, which does not exist yet.
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir.path().join("presenter.toml"),
        "[deck]\ntheme = \"mytheme\"\n",
    );
    write(&dir.path().join("slides.md"), "# Theme test\n");
    let mut process = WatchProcess::spawn(
        dir.path(),
        &["build", "slides.md", "--watch", "-o", "out.html"],
    );
    process.wait_for("the first build", Duration::from_secs(30), |p| {
        p.stderr().contains("Watching for changes (Ctrl-C to stop)")
    });
    assert!(
        process.stderr().contains("not found"),
        "{}",
        process.stderr()
    );

    // Created with an invalid manifest: a rebuild reports it.
    let manifest = dir.path().join("mytheme/theme.toml");
    write(
        &manifest,
        "name = \"mytheme\"\nextends = \"default\"\nbogus = [\n",
    );
    process.wait_for("the invalid manifest", Duration::from_secs(15), |p| {
        p.stderr().matches("error: ").count() == 2
    });

    // A missing parent theme, then fixed.
    write(&manifest, "name = \"mytheme\"\nextends = \"defualt\"\n");
    process.wait_for("the missing parent", Duration::from_secs(15), |p| {
        p.stderr().matches("error: ").count() == 3
    });
    assert!(process.stderr().contains("defualt"), "{}", process.stderr());
    write(&manifest, "name = \"mytheme\"\nextends = \"default\"\n");
    process.wait_for("the fixed theme", Duration::from_secs(15), |p| {
        p.stdout().starts_with("Wrote out.html (1 slide, ")
    });
}

/// `f`'s result and how many times it parsed a deck on this thread: the pipeline logs one
/// `parsed <file>` debug event per parse ([`pipeline::try_parse_only`]), counted here.
fn counting_parses<T>(f: impl FnOnce() -> T) -> (T, usize) {
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let value = tracing::subscriber::with_default(subscriber, f);
    let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    let parses = log
        .lines()
        .filter(|line| line.contains("presenter::pipeline: parsed "))
        .count();
    (value, parses)
}

#[test]
fn a_failed_build_parses_the_deck_once() {
    let (dir, input) = project("# Hi\n\n![plot](figures/plot.png)\n");
    let opts = BuildOptions::new(&input);
    let out = dir.path().join("out.html");

    // The measure: a successful build parses once.
    let ((result, set), parses) = counting_parses(|| try_build_watched(&opts));
    let build = result.unwrap();
    assert_eq!(parses, 1);
    assert_eq!(set, watch_set_for(&build));

    // Failures after the parse: a missing asset (an error in `build`), a theme that does
    // not resolve, rejected front matter. Each keeps watch_set_for_failed's set, from the
    // one parse of the build.
    for deck in [
        "# Hi\n\n![plot](figures/plot.png)\n\n![gone](figures/missing.png)\n",
        "---\ntheme: other\n---\n\n# Hi\n\n![plot](figures/plot.png)\n",
        "---\nreveal:\n  plugins: [bogus]\n---\n\n# Hi\n\n![gone](figures/missing.png)\n",
    ] {
        write(&input, deck);
        let ((result, set), parses) = counting_parses(|| try_build_watched(&opts));
        assert!(result.is_err(), "{deck}");
        assert_eq!(parses, 1, "{deck}");
        let (expected, reparses) = counting_parses(|| watch_set_for_failed(&opts));
        assert_eq!(reparses, 1, "watch_set_for_failed itself parses: {deck}");
        assert_eq!(set, expected, "{deck}");
    }
    // The rebuild loop: one parse for a failed build, watching the same set.
    write(
        &input,
        "# Hi\n\n![plot](figures/plot.png)\n\n![gone](figures/missing.png)\n",
    );
    let mut watched = None;
    let (result, parses) = counting_parses(|| {
        run_build_watch_with(&opts, &out, |status| {
            if let WatchStatus::Failed(failure) = &status {
                assert!(failure.error.to_string().contains("missing.png"));
            }
            if let WatchStatus::Watching(set) = status {
                watched = Some(set.clone());
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        })
    });
    result.unwrap();
    assert_eq!(parses, 1);
    let root = canonical(dir.path());
    let watched = watched.expect("the loop reported what it watches");
    assert!(watched.files.contains(&root.join("figures/missing.png")));
    assert!(watched.files.contains(&root.join("figures/plot.png")));
    assert_eq!(watched, watch_set_for_failed(&opts));

    // A deck that does not parse: watched like before the first build.
    write(&input, "---\ntitle: [unclosed\n---\n\n# Hi\n");
    let (result, set) = try_build_watched(&opts);
    assert!(result.is_err());
    assert_eq!(set, watch_set_before_build(&opts));
    assert_eq!(set, watch_set_for_failed(&opts));
}
