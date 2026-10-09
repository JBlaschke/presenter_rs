//! T24: headless-browser smoke tests (opt-in) for what snapshot tests cannot see:
//! JavaScript errors, reveal.js not initializing, KaTeX or highlight.js not running, and
//! live reload not swapping the theme stylesheet in place.
//!
//! Run them with `cargo test --features browser-tests --test browser`; without the feature
//! this file compiles to nothing. They drive Google Chrome (or Chromium) through
//! `presenter::pdf::devtools::Page`, the DevTools-protocol client of `presenter export pdf`,
//! found the same way as for the export (`presenter::pdf::find_chrome`: `$CHROME_PATH`, the
//! macOS app bundles, then `PATH`). Without a browser each test prints
//! `skipping: no chrome found` and passes, unless `PRESENTER_REQUIRE_CHROME` is set (CI
//! sets it, so that a runner without Chrome fails instead of passing silently).
//!
//! Everything happens in temporary copies of `examples/basic`; the pages are `file://`
//! and `http://127.0.0.1` URLs, so no network access is needed. The tests take turns: each
//! starts a browser (and the serve test a server).

#![cfg(all(feature = "browser-tests", unix))]

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::Duration;

use presenter::pdf::devtools::Page;
use presenter::pdf::find_chrome;
use regex::Regex;
use serde_json::Value;

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

/// Waits (at most 20 s) for reveal.js and the live-reload client's first `hello`, then
/// leaves a marker on `window` that a reload would wipe out, and returns the theme link's
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

/// Whether the page survived: the marker of [`LIVE_READY`], the types of its navigation
/// entries (`navigate`, or `reload` after a reload) and the client's build id.
const NOT_RELOADED: &str = r#"({
  marker: window.__presenterSmokeMarker ?? null,
  navigations: performance.getEntriesByType('navigation').map((entry) => entry.type),
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

/// A temporary copy of `examples/<name>` (without a `dist/` left by an earlier build).
fn copy_example(name: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
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
/// wait for reveal.js, and check that it, KaTeX and highlight.js ran without errors.
#[test]
fn the_standalone_example_runs_in_chrome() {
    let _turn = serial();
    let Some(chrome) = chrome() else {
        return;
    };
    let dir = copy_example("basic");
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
    assert!(count("total") >= 10, "Reveal.getTotalSlides(): {facts}");
    assert!(count("katex") > 0, "KaTeX rendered nothing: {facts}");
    assert!(
        count("hljs") > 0,
        "highlight.js highlighted nothing: {facts}"
    );
    assert_no_page_errors(&mut page);
}

/// The DevTools page the other tests rely on, error collection first: a page that logs a
/// console error, throws an uncaught exception and refers to a missing image reports all
/// three. A fragment-only navigation stays on the document; an expression that throws and
/// a page that cannot be loaded are errors.
#[test]
fn the_devtools_page_reports_page_errors() {
    let _turn = serial();
    let Some(chrome) = chrome() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let html = dir.path().join("errors.html");
    std::fs::write(
        &html,
        "<!doctype html><meta charset=\"utf-8\"><title>errors</title>\n\
         <img src=\"missing.png\" alt=\"\">\n\
         <script>console.error('smoke', 42); setTimeout(() => { throw new Error('later'); });</script>\n",
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
    assert!(errors.contains("missing.png"), "{errors}");
    assert_eq!(page.errors().len(), 3, "{errors}");

    // Only the fragment changes: the same document, and no `load` event to wait for.
    evaluate(&mut page, "window.sameDocument = true");
    page.navigate(&format!("{}#fragment", file_url(&html)))
        .unwrap_or_else(|e| panic!("{e}"));
    let same = evaluate(&mut page, "[location.hash, window.sameDocument === true]");
    assert_eq!(same, serde_json::json!(["#fragment", true]));

    let thrown = page.evaluate("null.x").unwrap_err().to_string();
    assert!(
        thrown.starts_with("JavaScript threw: TypeError"),
        "{thrown}"
    );
    let missing = file_url(&dir.path().join("nothing-here.html"));
    let failed = page.navigate(&missing).unwrap_err().to_string();
    assert!(failed.contains("nothing-here.html"), "{failed}");
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

/// The task's second test: `presenter serve --port 0` the example deck with a child theme,
/// open it, edit the theme's stylesheet, and check that the client swaps
/// `#presenter-theme` to the next `?v=` and the edit applies, without reloading the page.
#[test]
fn serve_swaps_the_theme_css_without_a_reload() {
    let _turn = serial();
    let Some(chrome) = chrome() else {
        return;
    };
    let dir = copy_example("basic");
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

    let mut page = open_page(&chrome);
    page.navigate(&server.url)
        .unwrap_or_else(|e| panic!("{e}\nserver stderr:\n{}", server.stderr()));
    let before = evaluate(&mut page, LIVE_READY);
    assert!(before.get("error").is_none(), "{before}");
    assert_eq!(
        before["probe"], "before",
        "the child theme is not applied: {before}"
    );
    assert_eq!(before["navigations"], 1, "{before}");
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

    let after = evaluate(&mut page, NOT_RELOADED);
    eprintln!("serve: {before} -> {swapped}, {after}");
    assert_eq!(
        after["marker"], "still here",
        "the page was reloaded: {after}"
    );
    assert_eq!(
        after["navigations"],
        serde_json::json!(["navigate"]),
        "{after}"
    );
    assert_no_page_errors(&mut page);
}
