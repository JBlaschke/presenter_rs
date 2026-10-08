//! T45 end to end: `presenter export pdf` (docs/spec/cli.md `export`).
//!
//! The tests that print with a real browser run only when `presenter::pdf::find_chrome(None)`
//! finds one (`--chrome`-less search: `$CHROME_PATH`, app bundles, `PATH`); otherwise they
//! print `skipping: no chrome found` and pass. The others use fake browsers (shell scripts)
//! and need no Chrome. Every PDF is written into a temporary directory.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use assert_cmd::Command;
use presenter::pdf::{find_chrome, page_count};
use presenter::pipeline::{BuildOptions, build_deck};
use regex::Regex;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The binary, run from `dir`, without the developer's `RUST_LOG` / `NO_COLOR`, with a
/// generous safety timeout (the command's own limit is 120 s).
fn presenter(dir: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_presenter"));
    cmd.current_dir(dir)
        .env_remove("RUST_LOG")
        .env_remove("NO_COLOR")
        .timeout(Duration::from_secs(180));
    cmd
}

/// A real browser, or `None` after printing why the test is skipped.
fn chrome() -> Option<PathBuf> {
    let found = find_chrome(None);
    if found.is_none() {
        eprintln!("skipping: no chrome found");
    }
    found
}

/// A temporary directory with `files` (path, contents) written into it.
fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, contents) in files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    dir
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("UTF-8 output")
}

/// Asserts the `Wrote <path> (<n> pages, <size>)` line and returns `<n>`.
fn assert_wrote(output: &Output, path_suffix: &str) -> usize {
    let stdout = text(&output.stdout);
    let line = Regex::new(r"^Wrote (.+) \((\d+) pages?, \d+(\.\d)? (B|kB|MB)\)\n$").unwrap();
    let caps = line
        .captures(&stdout)
        .unwrap_or_else(|| panic!("unexpected stdout: {stdout:?}"));
    assert!(caps[1].ends_with(path_suffix), "{stdout}");
    caps[2].parse().unwrap()
}

/// Reads a written PDF and checks its signature.
fn read_pdf(path: &Path) -> Vec<u8> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(bytes.starts_with(b"%PDF-"), "not a PDF: {}", path.display());
    bytes
}

#[test]
fn exports_the_example_deck() {
    if chrome().is_none() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("nested/slides.pdf");
    let output = presenter(&root())
        .args(["export", "pdf", "examples/basic/slides.md", "-o"])
        .arg(&out)
        .assert()
        .success()
        .get_output()
        .clone();
    let printed = assert_wrote(&output, "slides.pdf");

    let pdf = read_pdf(&out);
    assert!(pdf.len() > 20_000, "only {} bytes", pdf.len());
    let pages = page_count(&pdf);
    assert_eq!(pages, printed);
    // One page per slide, plus one per fragment step (reveal's default
    // pdfSeparateFragments = true); the example has a few fragments and line steps.
    let slides = build_deck(&BuildOptions::new(root().join("examples/basic/slides.md")))
        .unwrap()
        .document
        .slide_count;
    assert!(
        pages >= slides && pages <= 2 * slides,
        "{pages} pages for {slides} slides"
    );
}

/// A three-slide deck whose second slide has two fragments.
const FRAGMENT_DECK: &str = "\
# One

---

## Two

:::fragment
First step
:::

:::fragment
Second step
:::

---

## Three
";

#[test]
fn reveal_pdf_options_pass_through_and_the_pdf_goes_next_to_the_html() {
    if chrome().is_none() {
        return;
    }
    let dir = project(&[
        ("deck.md", FRAGMENT_DECK),
        ("presenter.toml", "[reveal]\npdfSeparateFragments = false\n"),
        ("separate.toml", "[reveal]\npdfSeparateFragments = true\n"),
    ]);

    // Collapsed fragments: one page per slide, written to dist/deck.pdf.
    let output = presenter(dir.path())
        .args(["export", "pdf", "deck.md"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_eq!(assert_wrote(&output, "deck.pdf"), 3);
    assert_eq!(page_count(&read_pdf(&dir.path().join("dist/deck.pdf"))), 3);

    // reveal's default: the fragment slide adds one page per step.
    let output = presenter(dir.path())
        .args(["--config", "separate.toml", "export", "pdf", "deck.md"])
        .args(["-o", "separate.pdf"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_eq!(assert_wrote(&output, "separate.pdf"), 5);
    assert_eq!(page_count(&read_pdf(&dir.path().join("separate.pdf"))), 5);
}

#[test]
fn quiet_exports_print_nothing() {
    if chrome().is_none() {
        return;
    }
    let dir = project(&[("deck.md", "# Only slide\n")]);
    presenter(dir.path())
        .args(["-q", "export", "pdf", "deck.md", "-o", "out.pdf"])
        .assert()
        .success()
        .stdout("");
    assert_eq!(page_count(&read_pdf(&dir.path().join("out.pdf"))), 1);
}

#[test]
fn a_missing_browser_exits_3_with_install_hints() {
    let dir = project(&[("deck.md", "# Slide\n")]);
    let output = presenter(dir.path())
        .args([
            "export",
            "pdf",
            "deck.md",
            "--chrome",
            "/nonexistent/chrome",
        ])
        .assert()
        .code(3)
        .stdout("")
        .get_output()
        .clone();
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with("error: --chrome /nonexistent/chrome: not an executable file\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("brew install --cask google-chrome"),
        "{stderr}"
    );
    assert!(stderr.contains("sudo apt install chromium"), "{stderr}");
    assert!(
        stderr.contains("CHROME_PATH environment variable"),
        "{stderr}"
    );
    assert!(!dir.path().join("dist").exists());
}

/// Writes an executable shell script `name` with `body` into `dir`.
#[cfg(unix)]
fn fake_chrome(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Runs `cmd` until its stderr does not report `Text file busy` (executing a script that
/// was just written while another test thread forks can fail that way, rarely).
#[cfg(unix)]
fn run_settled(mut make: impl FnMut() -> Command) -> Output {
    for _ in 0..5 {
        let output = make().output().unwrap();
        if !text(&output.stderr).contains("Text file busy") {
            return output;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the fake browser stayed busy");
}

#[cfg(unix)]
#[test]
fn a_failing_browser_from_chrome_path_exits_1_with_its_stderr() {
    let dir = project(&[("deck.md", "# Slide\n")]);
    let fake = fake_chrome(
        dir.path(),
        "fake-chrome",
        "echo 'fake chrome: cannot open display' >&2\nexit 4",
    );
    let output = run_settled(|| {
        let mut cmd = presenter(dir.path());
        cmd.env("CHROME_PATH", &fake)
            .args(["export", "pdf", "deck.md"]);
        cmd
    });
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(text(&output.stdout), "");
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with(
            "error: Chrome failed to print the deck: Chrome exited (exit status: 4) before \
             opening its DevTools port\n"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("browser: {}", fake.display())),
        "{stderr}"
    );
    assert!(
        stderr.contains("    fake chrome: cannot open display"),
        "{stderr}"
    );
    assert!(!dir.path().join("dist/deck.pdf").exists());
}

#[cfg(unix)]
#[test]
fn build_errors_stop_before_the_browser_starts() {
    let dir = project(&[("deck.md", "# Slide\n\n![plot](missing.png)\n")]);
    let marker = dir.path().join("chrome-ran");
    let fake = fake_chrome(
        dir.path(),
        "fake-chrome",
        &format!("touch '{}'\nexit 1", marker.display()),
    );
    let output = run_settled(|| {
        let mut cmd = presenter(dir.path());
        cmd.args(["export", "pdf", "deck.md", "--chrome"])
            .arg(&fake);
        cmd
    });
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = text(&output.stderr);
    assert!(stderr.contains("asset not found"), "{stderr}");
    assert!(!marker.exists(), "the browser ran despite the build error");
}

#[test]
fn unknown_input_formats_are_usage_errors() {
    let dir = project(&[("deck.txt", "# Slide\n")]);
    presenter(dir.path())
        .args(["export", "pdf", "deck.txt"])
        .assert()
        .code(2);
}

#[test]
fn pages_that_never_get_ready_fail_with_the_reason_and_page_errors() {
    let Some(chrome) = chrome() else {
        return;
    };
    let err = presenter::pdf::html_to_pdf(&chrome, "<p>no reveal.js here</p>", DEADLINE)
        .unwrap_err()
        .to_string();
    assert!(
        err.starts_with(
            "Chrome failed to print the deck: the page does not load reveal.js (no global Reveal)"
        ),
        "{err}"
    );

    let stuck = "<script>window.Reveal = { isReady: () => false };\
                 throw new Error('boom in the deck');</script>";
    let started = std::time::Instant::now();
    let err = presenter::pdf::html_to_pdf(&chrome, stuck, Duration::from_secs(8))
        .unwrap_err()
        .to_string();
    assert!(
        err.starts_with(
            "Chrome failed to print the deck: timed out after 8 s while waiting for reveal.js \
             to lay out the pages\n  page errors:\n    Error: boom in the deck\n"
        ),
        "{err}"
    );
    assert!(
        err.contains(&format!("browser: {}", chrome.display())),
        "{err}"
    );
    assert!(started.elapsed() < Duration::from_secs(20));
}

/// Time limit for the library calls above (generous; they fail within seconds).
const DEADLINE: Duration = Duration::from_secs(60);
