//! T24: headless-browser smoke tests (opt-in) for what snapshot tests cannot see:
//! JavaScript errors, reveal.js not initializing or not navigating, KaTeX or highlight.js
//! not running, and live reload not swapping the theme stylesheet in place or not
//! reloading when it must.
//!
//! Run them with `cargo test --features browser-tests --test browser`; without the feature
//! this file compiles to nothing. They drive Google Chrome (or Chromium) through
//! `presenter::pdf::devtools::Page`, the DevTools-protocol client of `presenter export pdf`
//! (compiled only with the feature), found the same way as for the export
//! (`presenter::pdf::find_chrome`: `$CHROME_PATH`, the macOS app bundles, then `PATH`).
//! Without a browser each test prints `skipping: no chrome found` and passes, unless
//! `PRESENTER_REQUIRE_CHROME` is set (CI sets it, so that a runner without Chrome fails
//! instead of passing silently).
//!
//! Everything happens in temporary directories the browser can read
//! (`presenter::pdf::devtools::readable_temp_dir`: `$TMPDIR`, or `~/snap/<name>/common` for
//! a snap Chromium, which cannot see the system's `/tmp`); the pages are `file://` and
//! `http://127.0.0.1` URLs, so no network access is needed. The tests take turns: each
//! starts a browser (and the serve tests a server).

#![cfg(all(feature = "browser-tests", unix))]

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use presenter::pdf::devtools::{Page, readable_temp_dir};
use presenter::pdf::find_chrome;
use regex::Regex;
use serde_json::{Value, json};

/// How long one DevTools call may take; the waits below give up well before that.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Resolves to true once reveal.js is ready, or to false after 20 s.
const REVEAL_READY: &str = r#"(async () => {
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const ready = () => typeof Reveal !== 'undefined' && Reveal.isReady();
  for (let waited = 0; !ready(); waited += 50) {
    if (waited >= 20000) return false;
    await sleep(50);
  }
  return true;
})()"#;

/// What the standalone smoke test asserts on, read once reveal.js is ready.
const DECK_FACTS: &str = r#"({
  total: Reveal.getTotalSlides(),
  katex: document.querySelectorAll('.katex').length,
  hljs: document.querySelectorAll('pre code.hljs').length,
})"#;

/// Steps through the whole deck from where it is with `Reveal.next()` (every fragment,
/// including the code slide's line steps, and every slide of a vertical stack) until the
/// last slide has no fragment left, then opens and closes the overview. Returns the number
/// of steps, the slides visited, the final position and whether the overview opened.
const DECK_WALK: &str = r#"(async () => {
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const at = () => { const { h, v } = Reveal.getIndices(); return h + '/' + (v ?? 0); };
  const visited = new Set([at()]);
  let steps = 0;
  while (!Reveal.isLastSlide() || Reveal.availableFragments().next) {
    if (steps >= 1000) return { error: 'still stepping after 1000 steps', at: at() };
    Reveal.next();
    steps += 1;
    await sleep(10);
    visited.add(at());
  }
  const past = Reveal.getSlidePastCount();
  Reveal.toggleOverview(true);
  await sleep(100);
  const overview = Reveal.isOverview();
  Reveal.toggleOverview(false);
  await sleep(100);
  return { steps, visited: visited.size, past, overview, closed: !Reveal.isOverview() };
})()"#;

/// Waits (at most 20 s) for reveal.js and the live-reload client's first `hello`, then
/// leaves markers that a reload (on `window`) or an in-place content update (on the first
/// slide's element, which that replaces) would wipe out, and returns the theme link's
/// `href`, the probe property's value and the number of navigations so far.
const LIVE_READY: &str = r#"(async () => {
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const live = () => window.__presenterLiveReload?.state;
  const ready = () => typeof Reveal !== 'undefined' && Reveal.isReady() && live()?.connected;
  for (let waited = 0; !ready(); waited += 50) {
    if (waited >= 20000) return { error: 'not ready', live: live() ?? null };
    await sleep(50);
  }
  window.__presenterSmokeMarker = 'still here';
  document.querySelector('.reveal .slides section').__presenterSmokeMarker = 'still here';
  return {
    href: document.getElementById('presenter-theme')?.getAttribute('href') ?? null,
    probe: getComputedStyle(document.documentElement).getPropertyValue('--smoke-probe').trim(),
    navigations: performance.getEntriesByType('navigation').length,
  };
})()"#;

/// Called with the theme link's old `href`: waits (at most 20 s) until a single
/// `#presenter-theme` link with another `href` is left and the edited stylesheet applies
/// (the probe property reads `after`). Returns what it saw, with `timedOut` on failure.
const CSS_SWAPPED: &str = r#"async (old) => {
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const look = () => ({
    links: document.querySelectorAll('link#presenter-theme').length,
    href: document.getElementById('presenter-theme')?.getAttribute('href') ?? null,
    probe: getComputedStyle(document.documentElement).getPropertyValue('--smoke-probe').trim(),
  });
  for (let waited = 0; waited < 20000; waited += 50) {
    const now = look();
    if (now.links === 1 && now.href !== old && now.probe === 'after') return now;
    await sleep(50);
  }
  return { ...look(), timedOut: true };
}"#;

/// What the page went through since [`LIVE_READY`]: its markers (`null` once wiped out),
/// the types of its navigation entries (`navigate`, or `reload` after a reload), how often
/// it fetched `GET /` (the client does once at start and once per in-place content update),
/// reveal.js' `transition` setting, whether the live-reload client is connected (it has had
/// its `hello`) and the client's build id (`null` until then).
const PAGE_STATE: &str = r#"({
  marker: window.__presenterSmokeMarker ?? null,
  slideMarker: document.querySelector('.reveal .slides section')?.__presenterSmokeMarker ?? null,
  navigations: performance.getEntriesByType('navigation').map((entry) => entry.type),
  rootFetches: performance.getEntriesByType('resource')
    .filter((entry) => new URL(entry.name).pathname === '/').length,
  transition: typeof Reveal !== 'undefined' && Reveal.isReady() ? Reveal.getConfig().transition : null,
  connected: window.__presenterLiveReload?.state.connected ?? false,
  buildId: window.__presenterLiveReload?.state.buildId ?? null,
})"#;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A browser to test with, or `None` after printing why the test is skipped (a failure
/// instead when `PRESENTER_REQUIRE_CHROME` is set and not empty).
fn chrome() -> Option<PathBuf> {
    let found = find_chrome(None);
    if found.is_none() {
        let required = std::env::var_os("PRESENTER_REQUIRE_CHROME");
        assert!(
            required.is_none_or(|value| value.is_empty()),
            "no chrome found, but PRESENTER_REQUIRE_CHROME is set"
        );
        eprintln!("skipping: no chrome found");
    }
    found
}

/// Makes the tests take turns: one browser (and server) at a time.
fn serial() -> MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    TURN.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A new temporary directory that `chrome` can read files from (module docs).
fn scratch(chrome: &Path) -> tempfile::TempDir {
    let base = readable_temp_dir(chrome).unwrap_or_else(|| {
        panic!(
            "{} is a snap that has never run, so it can read no temporary directory: run it \
             once (that creates ~/snap/<name>/common), or set CHROME_PATH to another browser",
            chrome.display()
        )
    });
    tempfile::tempdir_in(&base).unwrap_or_else(|e| panic!("{}: {e}", base.display()))
}

/// A copy of `examples/<name>` (without a `dist/` left by an earlier build) in a
/// [`scratch`] directory.
fn copy_example(chrome: &Path, name: &str) -> tempfile::TempDir {
    let dir = scratch(chrome);
    copy_dir(&root().join("examples").join(name), dir.path());
    std::fs::remove_dir_all(dir.path().join("dist")).ok();
    dir
}

/// Copies the contents of `from` into the existing directory `to`, recursively.
fn copy_dir(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir(&target).unwrap();
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// `file://` URL of the absolute path `path`, percent-encoding all but unreserved
/// characters and `/`.
fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for byte in path.to_str().expect("a UTF-8 path").bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            url.push(char::from(byte));
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url
}

/// Starts Chrome with one page, failing the test with the error if that does not work.
fn open_page(chrome: &Path) -> Page {
    Page::launch(chrome, CALL_TIMEOUT).unwrap_or_else(|e| panic!("{e}"))
}

/// Evaluates `expression` in `page`; a failure fails the test with the error.
fn evaluate(page: &mut Page, expression: &str) -> Value {
    page.evaluate(expression)
        .unwrap_or_else(|e| panic!("{e}\nexpression: {expression}"))
}

/// Fails unless the page has reported no errors (after collecting the pending ones).
fn assert_no_page_errors(page: &mut Page) {
    evaluate(page, "0");
    assert!(
        page.errors().is_empty(),
        "the page reported errors:\n{}",
        page.errors().join("\n")
    );
}

/// The build id `N` of a theme link `href` of the form `/__presenter/theme.css?v=N`.
fn theme_version(href: &Value) -> u64 {
    let pattern = Regex::new(r"^/__presenter/theme\.css\?v=(\d+)$").unwrap();
    let href = href
        .as_str()
        .unwrap_or_else(|| panic!("no theme href: {href}"));
    let captures = pattern
        .captures(href)
        .unwrap_or_else(|| panic!("unexpected theme href {href:?}"));
    captures[1].parse().unwrap()
}

/// The task's first test: `presenter build` the example deck (standalone), open the file,
/// wait for reveal.js, and check that it, KaTeX and highlight.js ran, that the deck steps
/// through every slide and fragment and opens its overview, all without errors.
#[test]
fn the_standalone_example_runs_in_chrome() {
    let _turn = serial();
    let Some(chrome) = chrome() else {
        return;
    };
    let dir = copy_example(&chrome, "basic");
    let output = Command::new(env!("CARGO_BIN_EXE_presenter"))
        .args(["build", "slides.md"])
        .current_dir(dir.path())
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "presenter build failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let html = dir.path().join("dist/slides.html");
    assert!(html.is_file(), "{} was not written", html.display());

    let mut page = open_page(&chrome);
    page.navigate(&file_url(&html))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        evaluate(&mut page, REVEAL_READY),
        true,
        "Reveal.isReady() stayed false; page errors: {:?}",
        page.errors()
    );
    let facts = evaluate(&mut page, DECK_FACTS);
    eprintln!("standalone example: {facts}");
    let count = |key: &str| facts[key].as_u64().unwrap_or_else(|| panic!("{facts}"));
    let total = count("total");
    assert!(total >= 10, "Reveal.getTotalSlides(): {facts}");
    assert!(count("katex") > 0, "KaTeX rendered nothing: {facts}");
    assert!(
        count("hljs") > 0,
        "highlight.js highlighted nothing: {facts}"
    );
    assert_no_page_errors(&mut page);

    let walk = evaluate(&mut page, DECK_WALK);
    eprintln!("walked through the deck: {walk}");
    assert!(walk.get("error").is_none(), "{walk}");
    assert_eq!(
        walk["visited"], total,
        "not every slide was reached: {walk}"
    );
    assert_eq!(walk["past"], total - 1, "not on the last slide: {walk}");
    let steps = walk["steps"].as_u64().unwrap_or(0);
    assert!(steps > total - 1, "no fragment was stepped through: {walk}");
    assert_eq!(walk["overview"], true, "the overview did not open: {walk}");
    assert_eq!(walk["closed"], true, "the overview did not close: {walk}");
    assert_no_page_errors(&mut page);
}

/// The DevTools page the other tests rely on, error collection first: a page that logs a
/// console error, throws an uncaught exception and a string, and refers to a missing image
/// reports all four. A fragment-only navigation stays on the document; values JSON cannot
/// hold keep their JavaScript spelling; an expression that throws and a page that cannot
/// be loaded are errors.
#[test]
fn the_devtools_page_reports_page_errors() {
    let _turn = serial();
    let Some(chrome) = chrome() else {
        return;
    };
    let dir = scratch(&chrome);
    let html = dir.path().join("errors.html");
    std::fs::write(
        &html,
        "<!doctype html><meta charset=\"utf-8\"><title>errors</title>\n\
         <img src=\"missing.png\" alt=\"\">\n\
         <script>console.error('smoke', 42); setTimeout(() => { throw new Error('later'); });\n\
         setTimeout(() => { throw 'a string'; });</script>\n",
    )
    .unwrap();
    let mut page = open_page(&chrome);
    page.navigate(&file_url(&html))
        .unwrap_or_else(|e| panic!("{e}"));
    evaluate(
        &mut page,
        "new Promise((resolve) => setTimeout(resolve, 200))",
    );
    evaluate(&mut page, "0");
    let errors = page.errors().join("\n");
    eprintln!("collected page errors:\n{errors}");
    assert!(errors.contains("console.error: smoke 42"), "{errors}");
    assert!(errors.contains("Error: later"), "{errors}");
    assert!(errors.contains("Uncaught a string"), "{errors}");
    assert!(errors.contains("missing.png"), "{errors}");
    assert_eq!(page.errors().len(), 4, "{errors}");

    // Only the fragment changes: the same document, and no `load` event to wait for.
    evaluate(&mut page, "window.sameDocument = true");
    page.navigate(&format!("{}#fragment", file_url(&html)))
        .unwrap_or_else(|e| panic!("{e}"));
    let same = evaluate(&mut page, "[location.hash, window.sameDocument === true]");
    assert_eq!(same, json!(["#fragment", true]));

    let values = ["NaN", "-Infinity", "-0", "2n ** 70n", "undefined", "1.5"]
        .map(|expression| evaluate(&mut page, expression));
    assert_eq!(
        values,
        [
            json!("NaN"),
            json!("-Infinity"),
            json!("-0"),
            json!("1180591620717411303424n"),
            Value::Null,
            json!(1.5),
        ]
    );

    let thrown = page.evaluate("null.x").unwrap_err().to_string();
    assert!(
        thrown.starts_with("JavaScript threw: TypeError"),
        "{thrown}"
    );
    let thrown = page.evaluate("throw 'boom'").unwrap_err().to_string();
    assert_eq!(thrown, "JavaScript threw: Uncaught boom");
    let rejected = page.evaluate("Promise.reject('nope')").unwrap_err();
    assert_eq!(
        rejected.to_string(),
        "JavaScript threw: Uncaught (in promise) nope"
    );
    let missing = file_url(&dir.path().join("nothing-here.html"));
    let failed = page.navigate(&missing).unwrap_err().to_string();
    assert!(failed.contains("nothing-here.html"), "{failed}");
    assert!(failed.contains("readable_temp_dir"), "{failed}");
    assert!(!failed.contains("stderr"), "{failed}");
}

/// `navigate` waits for the document it loads, even when the page navigated by itself
/// between two calls (whose `load` event Chrome reported while nobody was listening); a
/// request that Chrome refuses because the page is being replaced fails at once, without
/// Chrome's stderr.
#[test]
fn navigate_waits_for_its_own_document_after_the_page_navigated_by_itself() {
    let _turn = serial();
    let Some(chrome) = chrome() else {
        return;
    };
    let dir = scratch(&chrome);
    let pages = [
        (
            "a.html",
            "<script>setTimeout(() => { location.href = 'b.html'; }, 50);</script>",
        ),
        ("b.html", ""),
        (
            "c.html",
            "<script>const end = Date.now() + 1500; while (Date.now() < end) {}</script>",
        ),
    ];
    for (name, body) in pages {
        let title = name.trim_end_matches(".html");
        let html = format!("<!doctype html><title>{title}</title>{body}\n");
        std::fs::write(dir.path().join(name), html).unwrap();
    }
    let mut page = open_page(&chrome);
    page.navigate(&file_url(&dir.path().join("a.html")))
        .unwrap_or_else(|e| panic!("{e}"));
    // a.html goes on to b.html while nothing reads Chrome's events.
    thread::sleep(Duration::from_millis(800));
    let start = Instant::now();
    page.navigate(&file_url(&dir.path().join("c.html")))
        .unwrap_or_else(|e| panic!("{e}"));
    // c.html's script holds its `load` event back for 1.5 s (b.html's had long fired).
    let took = start.elapsed();
    assert!(
        took >= Duration::from_millis(1500),
        "returned after {took:?}"
    );
    let loaded = evaluate(&mut page, "[document.title, document.readyState]");
    assert_eq!(loaded, json!(["c", "complete"]));

    let start = Instant::now();
    let refused = page
        .evaluate("location.reload(), new Promise(() => {})")
        .unwrap_err()
        .to_string();
    eprintln!("refused in {:?}: {refused}", start.elapsed());
    assert!(
        refused.starts_with("Chrome (DevTools): Runtime.evaluate: "),
        "{refused}"
    );
    assert!(!refused.contains("stderr"), "{refused}");
    assert_no_page_errors(&mut page);
}

/// A running `presenter serve`; killed (and reaped) when dropped.
struct Server {
    child: Child,
    /// `http://127.0.0.1:<port>/`, from the first stdout line.
    url: String,
    /// Everything the server wrote on stderr so far.
    stderr: Arc<Mutex<String>>,
}

impl Server {
    /// Starts `presenter serve <args>` in `dir` and waits (at most 60 s) for its first
    /// stdout line, `Serving http://127.0.0.1:<port>/` (printed after the first build).
    fn start(dir: &Path, args: &[&str]) -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_presenter"))
            .arg("serve")
            .args(args)
            .current_dir(dir)
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&stderr);
        let mut errors = child.stderr.take().unwrap();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = errors.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let text = String::from_utf8_lossy(&buf[..n]);
                sink.lock().unwrap().push_str(&text);
            }
        });
        let (tx, rx) = mpsc::channel();
        let output = child.stdout.take().unwrap();
        thread::spawn(move || {
            let mut first = String::new();
            let _ = BufReader::new(output).read_line(&mut first);
            let _ = tx.send(first);
        });
        let mut server = Server {
            child,
            url: String::new(),
            stderr,
        };
        let first = rx
            .recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("no URL on stdout; stderr:\n{}", server.stderr()));
        let pattern = Regex::new(r"^Serving (http://127\.0\.0\.1:[1-9][0-9]*/)$").unwrap();
        let captures = pattern
            .captures(first.trim_end())
            .unwrap_or_else(|| panic!("first stdout line {first:?}; stderr:\n{}", server.stderr()));
        server.url = captures[1].to_string();
        server
    }

    fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Opens `server`'s deck in a new page and waits for [`LIVE_READY`]; returns the page and
/// what that returned.
fn open_live_page(chrome: &Path, server: &Server) -> (Page, Value) {
    let mut page = open_page(chrome);
    page.navigate(&server.url)
        .unwrap_or_else(|e| panic!("{e}\nserver stderr:\n{}", server.stderr()));
    let ready = evaluate(&mut page, LIVE_READY);
    assert!(ready.get("error").is_none(), "{ready}");
    assert_eq!(ready["navigations"], 1, "{ready}");
    (page, ready)
}

/// The task's second test: `presenter serve --port 0` the example deck with a child theme,
/// open it, edit the theme's stylesheet, and check that the client swaps
/// `#presenter-theme` to the next `?v=` and the edit applies, without reloading the page
/// and without an in-place content update (a `css` build: the slides' elements are kept
/// and `GET /` is not fetched again).
#[test]
fn serve_swaps_the_theme_css_without_a_reload() {
    let _turn = serial();
    let Some(chrome) = chrome() else {
        return;
    };
    let dir = copy_example(&chrome, "basic");
    let theme = dir.path().join("smoke-theme");
    std::fs::create_dir_all(theme.join("styles")).unwrap();
    std::fs::write(
        theme.join("theme.toml"),
        "name = \"smoke-theme\"\nextends = \"default\"\n\n[styles]\nentries = [\"styles/smoke.css\"]\n",
    )
    .unwrap();
    let css = theme.join("styles/smoke.css");
    std::fs::write(&css, ":root { --smoke-probe: before; }\n").unwrap();
    let server = Server::start(
        dir.path(),
        &["slides.md", "--theme", "smoke-theme", "--port", "0"],
    );

    let (mut page, before) = open_live_page(&chrome, &server);
    assert_eq!(
        before["probe"], "before",
        "the child theme is not applied: {before}"
    );
    let old = theme_version(&before["href"]);

    std::fs::write(&css, ":root { --smoke-probe: after; }\n").unwrap();
    let swapped = evaluate(&mut page, &format!("({CSS_SWAPPED})({})", before["href"]));
    assert!(
        swapped.get("timedOut").is_none(),
        "the stylesheet was not swapped within 20 s: {swapped}\nserver stderr:\n{}",
        server.stderr()
    );
    let new = theme_version(&swapped["href"]);
    assert!(new > old, "?v= went from {old} to {new}");

    let after = evaluate(&mut page, PAGE_STATE);
    eprintln!("serve: {before} -> {swapped}, {after}");
    assert_eq!(
        after["marker"], "still here",
        "the page was reloaded: {after}"
    );
    assert_eq!(after["navigations"], json!(["navigate"]), "{after}");
    assert_eq!(
        after["slideMarker"], "still here",
        "the slides were replaced (a content update, not a stylesheet swap): {after}"
    );
    assert_eq!(
        after["rootFetches"], 1,
        "GET / was fetched again (a content update, not a stylesheet swap): {after}"
    );
    assert_no_page_errors(&mut page);
}

/// `presenter serve` the example deck, open it, change a reveal.js setting in
/// `presenter.toml` (the page's `Reveal.initialize` call, outside the slides, changes) and
/// check that the client reloads the page exactly once, for the one rebuild the change
/// causes, and that the new setting applies (live reload QA checklist, item 18).
#[test]
fn serve_reloads_the_page_once_when_the_reveal_config_changes() {
    let _turn = serial();
    let Some(chrome) = chrome() else {
        return;
    };
    let dir = copy_example(&chrome, "basic");
    let config = dir.path().join("presenter.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.contains("transition = \"slide\""), "{text}");
    let server = Server::start(dir.path(), &["slides.md", "--port", "0"]);
    let (mut page, before) = open_live_page(&chrome, &server);
    let state = evaluate(&mut page, PAGE_STATE);
    assert_eq!(state["transition"], "slide", "{before}, {state}");

    std::fs::write(
        &config,
        text.replace("transition = \"slide\"", "transition = \"fade\""),
    )
    .unwrap();
    // The page goes away while it reloads: calls may fail meanwhile. The reloaded page
    // counts once its client is connected again (its `hello` sets the build id): reveal.js
    // reports ready on a timer, before the client's script has even been fetched, and a
    // state read in that window has no build id yet (a CI flake, 2026-10-09).
    let deadline = Instant::now() + Duration::from_secs(20);
    let reloaded = loop {
        let state = page.evaluate(PAGE_STATE);
        if let Ok(state) = &state
            && state["navigations"] == json!(["reload"])
            && state["transition"] == "fade"
            && state["connected"] == true
        {
            break state.clone();
        }
        assert!(
            Instant::now() < deadline,
            "no reload within 20 s: {state:?}\nserver stderr:\n{}",
            server.stderr()
        );
        thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(reloaded["marker"], Value::Null, "{reloaded}");
    // The one change caused one rebuild: the reloaded page connected to the build after
    // the one the page was opened with.
    assert_eq!(
        reloaded["buildId"],
        theme_version(&before["href"]) + 1,
        "not the one rebuild after the first build: {reloaded}"
    );

    // Settled: connected again, and no second reload or build follows.
    let again = evaluate(&mut page, LIVE_READY);
    assert!(again.get("error").is_none(), "{again}");
    thread::sleep(Duration::from_millis(1500));
    let after = evaluate(&mut page, PAGE_STATE);
    eprintln!("serve: {before} -> {reloaded} -> {after}");
    assert_eq!(
        after["marker"], "still here",
        "the page reloaded again: {after}"
    );
    assert_eq!(after["navigations"], json!(["reload"]), "{after}");
    assert_eq!(
        after["buildId"], reloaded["buildId"],
        "a build followed the reload: {reloaded} -> {after}"
    );
    assert_no_page_errors(&mut page);
}

/// L-T24-1: a page that navigates its frame again before its own `load` event (an inline
/// script that sets `location`; the live-reload client reloads from its `hello` handler,
/// which can run that early) does not leave `navigate` waiting for a `load` that never
/// comes: it returns once the document that replaced it has loaded.
#[test]
fn navigate_follows_a_page_that_replaces_itself_before_its_load() {
    let _turn = serial();
    let Some(chrome) = chrome() else {
        return;
    };
    let dir = scratch(&chrome);
    std::fs::write(
        dir.path().join("a.html"),
        "<!doctype html><title>a</title><script>location.href = 'b.html';</script>\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("b.html"),
        "<!doctype html><title>b</title>\n",
    )
    .unwrap();
    // Chrome's start-up gets the time a loaded CI machine needs; the navigation gets a
    // short timeout: waiting for a.html's `load` would fail the call after 8 s.
    let mut page = Page::launch(&chrome, Duration::from_secs(60)).unwrap_or_else(|e| panic!("{e}"));
    page.set_timeout(Duration::from_secs(8));
    let start = Instant::now();
    page.navigate(&file_url(&dir.path().join("a.html")))
        .unwrap_or_else(|e| panic!("{e}"));
    let took = start.elapsed();
    assert!(took < Duration::from_secs(5), "returned after {took:?}");
    let loaded = evaluate(&mut page, "[document.title, document.readyState]");
    assert_eq!(loaded, json!(["b", "complete"]));
    assert_no_page_errors(&mut page);
}

/// The heading's background image URL, the title slide's logo's `src` and natural width,
/// the natural width of the corner logo outside the slides, and the resources the page has
/// fetched.
const THEME_IMAGES: &str = r#"(() => {
  const logo = document.querySelector('img.logo');
  const corner = document.querySelector('img.corner');
  const width = (img) => (img && img.complete ? img.naturalWidth : null);
  return {
    bg: getComputedStyle(document.querySelector('.reveal h1')).backgroundImage,
    logo: logo?.getAttribute('src') ?? null,
    width: width(logo),
    corner: width(corner),
    fetched: performance.getEntriesByType('resource').map((entry) => new URL(entry.name).pathname + new URL(entry.name).search),
  };
})()"#;

/// Polls [`THEME_IMAGES`] (at most 20 s) until `done` accepts it; fails with what it saw.
fn wait_for_images(page: &mut Page, server: &Server, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let images = evaluate(page, THEME_IMAGES);
        if done(&images) {
            return images;
        }
        assert!(
            Instant::now() < deadline,
            "not within 20 s: {images}\nserver stderr:\n{}",
            server.stderr()
        );
        thread::sleep(Duration::from_millis(100));
    }
}

/// Audit F03: serve a deck whose child theme sets the heading's background with a
/// stylesheet `url()` and shows a logo through `asset()`, on the title slide and in a corner
/// outside the slides (`deck.html`); open it, edit both images, and check that the page
/// shows them without a reload: the background's URL changes and the new image is fetched
/// (a css swap), and both logos change their natural width (the build is reported as
/// content, so the slides are swapped in place and the corner logo takes its new URL).
#[test]
fn serve_shows_edited_theme_images_without_a_reload() {
    let _turn = serial();
    let Some(chrome) = chrome() else {
        return;
    };
    let dir = scratch(&chrome);
    let svg = |size: u32, fill: &str| {
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{size}\" height=\"{size}\">\
             <rect width=\"{size}\" height=\"{size}\" fill=\"{fill}\"/></svg>\n"
        )
    };
    let files = [
        (
            "t/theme.toml",
            "name = \"t\"\nextends = \"default\"\n[styles]\nentries = [\"styles/t.css\"]\n\
             [vars]\nlogo = \"logo.svg\"\n"
                .to_string(),
        ),
        (
            "t/styles/t.css",
            ".reveal h1 { background-image: url(../assets/bg.svg); }\n".to_string(),
        ),
        (
            "t/templates/deck.html",
            "{% extends \"@default/deck.html\" %}\n{% block body_start %}\
             <img class=\"corner\" src=\"{{ asset('logo.svg') }}\" alt=\"\">{% endblock %}\n"
                .to_string(),
        ),
        ("t/assets/bg.svg", svg(10, "red")),
        ("t/assets/logo.svg", svg(10, "green")),
        (
            "a.md",
            "---\ntheme: ./t\n---\n\n# Title {layout=title}\n".to_string(),
        ),
    ];
    for (path, text) in &files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let server = Server::start(dir.path(), &["a.md", "--port", "0"]);
    let (mut page, _) = open_live_page(&chrome, &server);
    let first = wait_for_images(&mut page, &server, |images| {
        images["width"] == 10 && images["corner"] == 10
    });
    let old_bg = first["bg"].as_str().unwrap().to_string();
    assert!(
        old_bg.contains("/__presenter/asset/@t/assets/bg.svg?v="),
        "{first}"
    );

    std::fs::write(dir.path().join("t/assets/bg.svg"), svg(20, "blue")).unwrap();
    let swapped = wait_for_images(&mut page, &server, |images| images["bg"] != old_bg.as_str());
    let new_bg = swapped["bg"].as_str().unwrap();
    let path = Regex::new(r#"(/__presenter/asset/@t/assets/bg\.svg\?v=[0-9]+)"#).unwrap();
    let fetched_path = path.captures(new_bg).unwrap_or_else(|| panic!("{swapped}"))[1].to_string();
    let swapped = wait_for_images(&mut page, &server, |images| {
        images["fetched"]
            .as_array()
            .unwrap()
            .iter()
            .any(|url| *url == fetched_path.as_str())
    });

    std::fs::write(dir.path().join("t/assets/logo.svg"), svg(200, "green")).unwrap();
    let wide = wait_for_images(&mut page, &server, |images| {
        images["width"] == 200 && images["corner"] == 200
    });
    assert_ne!(wide["logo"], first["logo"], "{wide}");

    let after = evaluate(&mut page, PAGE_STATE);
    eprintln!("serve: {first} -> {swapped} -> {wide}, {after}");
    assert_eq!(
        after["marker"], "still here",
        "the page was reloaded: {after}"
    );
    assert_eq!(after["navigations"], json!(["navigate"]), "{after}");
    assert_no_page_errors(&mut page);
}
