//! T45 and T49 end to end: `presenter export pdf` and `presenter export pptx` (docs/spec/cli.md
//! `export`).
//!
//! The tests that print with a real browser run only when `presenter::pdf::find_chrome(None)`
//! finds one (`--chrome`-less search: `$CHROME_PATH`, app bundles, `PATH`); otherwise they
//! print `skipping: no chrome found` and pass. The others use fake browsers (shell scripts)
//! and need no Chrome. Every PDF is written into a temporary directory.
//!
//! Exports that start a browser run with `TMPDIR` set to a private directory, which must be
//! empty afterwards, with no process left that names it (Chrome's `--user-data-dir`): the
//! browser is always stopped and its profile removed, also when presenter is interrupted.

use std::path::{Path, PathBuf};
use std::process::Output;
#[cfg(unix)]
use std::process::Stdio;
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

use assert_cmd::Command;
use presenter::pdf::{find_chrome, page_count};
use presenter::pipeline::{BuildOptions, build_deck};
use regex::Regex;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The binary, run from `dir`, without the developer's `RUST_LOG` / `NO_COLOR` /
/// `CLICOLOR_FORCE`, with a generous safety timeout (the command's own limit is 120 s).
fn presenter(dir: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_presenter"));
    cmd.current_dir(dir)
        .env_remove("RUST_LOG")
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
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

/// The names in `dir` (none when it cannot be read).
#[cfg(unix)]
fn entries(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// The running processes whose command line names `dir`: `(pid, command line)`.
#[cfg(unix)]
fn processes_naming(dir: &Path) -> Vec<(String, String)> {
    let output = std::process::Command::new("ps")
        .args(["-A", "-ww", "-o", "pid=", "-o", "args="])
        .output()
        .expect("ps runs");
    let needle = dir.to_str().unwrap();
    text(&output.stdout)
        .lines()
        .filter(|line| line.contains(needle))
        .filter_map(|line| {
            let (pid, args) = line.trim_start().split_once(' ')?;
            Some((pid.to_string(), args.trim().to_string()))
        })
        .collect()
}

/// Waits up to 20 s until `tmp` (an export's `TMPDIR`) is empty and no process names it;
/// otherwise kills what is left and fails.
#[cfg(unix)]
fn assert_cleaned_up(tmp: &Path) {
    let started = Instant::now();
    loop {
        let files = entries(tmp);
        let processes = processes_naming(tmp);
        if files.is_empty() && processes.is_empty() {
            return;
        }
        if started.elapsed() > Duration::from_secs(20) {
            for (pid, _) in &processes {
                let _ = std::process::Command::new("kill")
                    .args(["-9", pid])
                    .status();
            }
            panic!("left behind after 20 s: files {files:?}, processes {processes:?}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
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
    let private_tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("nested/slides.pdf");
    let output = presenter(&root())
        .env("TMPDIR", private_tmp.path())
        .args(["export", "pdf", "examples/basic/slides.md", "-o"])
        .arg(&out)
        .assert()
        .success()
        .get_output()
        .clone();
    let printed = assert_wrote(&output, "slides.pdf");
    #[cfg(unix)]
    assert_cleaned_up(private_tmp.path());

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

/// `TMPDIR` relative to the current directory: the deck's `file://` URL must name the
/// directory presenter created there (it used to become `file:///reltmp/…`, which Chrome
/// could not load).
#[cfg(unix)]
#[test]
fn a_relative_tmpdir_works() {
    if chrome().is_none() {
        return;
    }
    let dir = project(&[("deck.md", "# One\n\n---\n\n# Two\n")]);
    let reltmp = dir.path().join("reltmp");
    std::fs::create_dir(&reltmp).unwrap();
    let output = presenter(dir.path())
        .env("TMPDIR", "reltmp")
        .args(["export", "pdf", "deck.md", "-o", "rel.pdf"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_eq!(assert_wrote(&output, "rel.pdf"), 2);
    assert_eq!(page_count(&read_pdf(&dir.path().join("rel.pdf"))), 2);
    // Chrome's profile was in reltmp (as an absolute path: the processes name the real one).
    assert_cleaned_up(&std::fs::canonicalize(&reltmp).unwrap());
}

/// Other spellings of a relative `TMPDIR`: `..` after a symbolic link (the system follows
/// the link first, Chrome would drop `lnk/..` from the URL), and an empty `TMPDIR=`, which
/// counts as unset (`/tmp`) and must not put the export's directory into the deck's.
#[cfg(unix)]
#[test]
fn tmpdir_spellings_with_dot_dot_or_empty_work() {
    if chrome().is_none() {
        return;
    }
    let dir = project(&[("deck.md", "# One\n\n---\n\n# Two\n")]);
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(elsewhere.path().join("sub")).unwrap();
    std::fs::create_dir(elsewhere.path().join("t")).unwrap();
    std::os::unix::fs::symlink(elsewhere.path().join("sub"), dir.path().join("lnk")).unwrap();
    let output = presenter(dir.path())
        .env("TMPDIR", "lnk/../t")
        .args(["export", "pdf", "deck.md", "-o", "dotdot.pdf"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_eq!(assert_wrote(&output, "dotdot.pdf"), 2);
    assert!(!dir.path().join("t").exists());
    assert_cleaned_up(&std::fs::canonicalize(elsewhere.path().join("t")).unwrap());

    let output = presenter(dir.path())
        .env("TMPDIR", "")
        .args(["export", "pdf", "deck.md", "-o", "empty.pdf"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_eq!(assert_wrote(&output, "empty.pdf"), 2);
    assert_eq!(page_count(&read_pdf(&dir.path().join("empty.pdf"))), 2);
    let mut left = entries(dir.path());
    left.sort();
    assert_eq!(left, ["deck.md", "dotdot.pdf", "empty.pdf", "lnk"]);
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

/// A deck that opens JavaScript dialogs, with a title and an image description that
/// mention `/Type /Page` (Chrome writes both into the PDF as strings).
#[cfg(unix)]
const DIALOG_DECK: &str = "\
---
title: Inside /Type /Page dictionaries
---

# Dialogs

<script>alert('hello from the deck'); if (confirm('Continue?')) { throw new Error('accepted'); }</script>

![/Type /Page alt](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==)
";

#[cfg(unix)]
#[test]
fn dialogs_are_dismissed_and_pdf_strings_are_not_pages() {
    if chrome().is_none() {
        return;
    }
    let dir = project(&[("deck.md", DIALOG_DECK)]);
    let private_tmp = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let output = presenter(dir.path())
        .env("TMPDIR", private_tmp.path())
        .args(["export", "pdf", "deck.md", "-o", "deck.pdf"])
        .assert()
        .success()
        .get_output()
        .clone();
    // A dialog no longer blocks the page until the 120 s deadline.
    assert!(started.elapsed() < Duration::from_secs(60));
    let stderr = text(&output.stderr);
    for dialog in [
        "the deck opened a JavaScript alert (\"hello from the deck\"); dismissed it",
        "the deck opened a JavaScript confirm (\"Continue?\"); dismissed it",
    ] {
        assert!(stderr.contains(dialog), "{stderr}");
    }
    assert_eq!(assert_wrote(&output, "deck.pdf"), 1);
    let pdf = read_pdf(&dir.path().join("deck.pdf"));
    for string in [
        &b"(Inside /Type /Page dictionaries)"[..],
        &b"(/Type /Page alt)"[..],
    ] {
        assert!(
            pdf.windows(string.len()).any(|window| window == string),
            "the PDF lacks {}",
            String::from_utf8_lossy(string)
        );
    }
    assert_eq!(page_count(&pdf), 1);
    assert_cleaned_up(private_tmp.path());
}

#[cfg(unix)]
#[test]
fn a_failed_print_stops_the_browser_and_cleans_up() {
    if chrome().is_none() {
        return;
    }
    let deck = "# Not printable\n\n<script>window.addEventListener('load', () => \
                { Reveal.isPrintView = () => false; });</script>\n";
    let dir = project(&[("deck.md", deck)]);
    let private_tmp = tempfile::tempdir().unwrap();
    let output = presenter(dir.path())
        .env("TMPDIR", private_tmp.path())
        .args(["export", "pdf", "deck.md", "-o", "deck.pdf"])
        .assert()
        .code(1)
        .stdout("")
        .get_output()
        .clone();
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with(
            "error: Chrome failed to print the deck: reveal.js did not switch to its print \
             view (?print-pdf)\n"
        ),
        "{stderr}"
    );
    assert!(!dir.path().join("deck.pdf").exists());
    assert_cleaned_up(private_tmp.path());
}

/// Tiny PNGs with sizes no other image in a deck has: (width, height, base64).
#[cfg(unix)]
const PICTURES: [(u32, u32, &str); 3] = [
    (
        7,
        5,
        "iVBORw0KGgoAAAANSUhEUgAAAAcAAAAFCAIAAAAG+GGPAAAAEklEQVR4nGP4z8CAibAIUUUUAKEvIt57lX2fAAAAAElFTkSuQmCC",
    ),
    (
        11,
        3,
        "iVBORw0KGgoAAAANSUhEUgAAAAsAAAADCAIAAADKnWIcAAAAEElEQVR4nGNgaGAggKigAgBKExCBCea1EgAAAABJRU5ErkJggg==",
    ),
    (
        13,
        2,
        "iVBORw0KGgoAAAANSUhEUgAAAA0AAAACCAIAAAAM38H+AAAAEElEQVR4nGNgYPhPHCJOHQDyhxnnVFjAQQAAAABJRU5ErkJggg==",
    ),
];

/// The `(width, height)` of every image in a PDF written by Chrome.
#[cfg(unix)]
fn pdf_image_sizes(pdf: &[u8]) -> std::collections::BTreeSet<(u32, u32)> {
    let size =
        regex::bytes::Regex::new(r"/Subtype\s*/Image\s*/Width\s+(\d+)\s*/Height\s+(\d+)").unwrap();
    size.captures_iter(pdf)
        .map(|caps| {
            let number = |i: usize| text(&caps[i]).parse::<u32>().unwrap();
            (number(1), number(2))
        })
        .collect()
}

#[cfg(unix)]
#[test]
fn images_by_absolute_path_and_file_url_are_printed() {
    use base64::Engine as _;

    if chrome().is_none() {
        return;
    }
    // Raw HTML images stay as written in the standalone deck unless they are relative,
    // so the PDF shows them only when the deck is a file:// page, as `presenter build`'s
    // output opened from disk is.
    let dir = project(&[]);
    let pictures = dir.path().canonicalize().unwrap();
    for ((width, height, png), name) in PICTURES.iter().zip(["rel", "abs", "url"]) {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(png)
            .unwrap();
        std::fs::write(pictures.join(format!("{name}.png")), bytes).unwrap();
        assert!(width * height > 0);
    }
    let deck = format!(
        "# Pictures\n\n<img src=\"rel.png\" alt=\"relative\"> \
         <img src=\"{0}/abs.png\" alt=\"absolute\"> \
         <img src=\"file://{0}/url.png\" alt=\"file URL\">\n",
        pictures.display()
    );
    std::fs::write(dir.path().join("deck.md"), deck).unwrap();
    let private_tmp = tempfile::tempdir().unwrap();
    let output = presenter(dir.path())
        .env("TMPDIR", private_tmp.path())
        .args(["export", "pdf", "deck.md", "-o", "deck.pdf"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_eq!(assert_wrote(&output, "deck.pdf"), 1);
    let sizes = pdf_image_sizes(&read_pdf(&dir.path().join("deck.pdf")));
    for (width, height, _) in PICTURES {
        assert!(
            sizes.contains(&(width, height)),
            "no {width}x{height} image in the PDF: {sizes:?}"
        );
    }
    assert_cleaned_up(private_tmp.path());
}

#[cfg(unix)]
#[test]
fn decks_larger_than_one_devtools_message_are_printed() {
    if chrome().is_none() {
        return;
    }
    // A 60 MiB image makes an 85 MB standalone deck: more than Chrome takes in one
    // DevTools message (100 MiB, base64-encoded), so it must be loaded from disk.
    let dir = project(&[("deck.md", "# Big\n\n![big](big.png)\n\n---\n\n# Two\n")]);
    std::fs::write(dir.path().join("big.png"), vec![0u8; 60 << 20]).unwrap();
    let private_tmp = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let output = presenter(dir.path())
        .env("TMPDIR", private_tmp.path())
        .args(["export", "pdf", "deck.md", "-o", "deck.pdf"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(assert_wrote(&output, "deck.pdf"), 2);
    assert_eq!(page_count(&read_pdf(&dir.path().join("deck.pdf"))), 2);
    assert_cleaned_up(private_tmp.path());
}

/// A deck that never gets ready to print, so that an export of it runs until its deadline.
#[cfg(unix)]
const STALLED_DECK: &str = "\
# Never ready

<script>window.addEventListener('load', () => { Reveal.isReady = () => false; });</script>
";

/// Waits until the export `child` (with `TMPDIR` = `tmp`) has a browser running with its
/// profile in `tmp`.
#[cfg(unix)]
fn wait_for_browser(child: &mut std::process::Child, tmp: &Path) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(60) {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("presenter exited before the test interrupted it: {status}");
        }
        let profile_written = entries(tmp)
            .iter()
            .any(|name| !entries(&tmp.join(name).join("profile")).is_empty());
        if profile_written && !processes_naming(tmp).is_empty() {
            // Let Chrome get as far as loading the deck.
            std::thread::sleep(Duration::from_millis(500));
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("no browser started within 60 s");
}

#[cfg(unix)]
#[test]
fn an_interrupted_export_leaves_no_browser_and_no_files() {
    use std::os::unix::process::CommandExt as _;
    use std::os::unix::process::ExitStatusExt as _;

    if chrome().is_none() {
        return;
    }
    let dir = project(&[("stalled.md", STALLED_DECK)]);
    // `kill <pid>` (a supervisor, an IDE's stop button), Ctrl-C in a terminal (SIGINT to the
    // whole process group, Chrome included), and SIGKILL, which presenter cannot handle.
    for (signal, number, group) in [("TERM", 15, false), ("INT", 2, true), ("KILL", 9, false)] {
        let private_tmp = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_presenter"))
            .current_dir(dir.path())
            .env_remove("RUST_LOG")
            .env("TMPDIR", private_tmp.path())
            .args(["export", "pdf", "stalled.md", "-o", "stalled.pdf"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        wait_for_browser(&mut child, private_tmp.path());
        let target = if group {
            format!("-{}", child.id())
        } else {
            child.id().to_string()
        };
        let sent = std::process::Command::new("kill")
            .args(["-s", signal, "--", &target])
            .status()
            .unwrap();
        assert!(sent.success(), "kill -s {signal} {target}");
        let status = child.wait().unwrap();
        // A SIGINT that this test process inherited as ignored ends the export through
        // Chrome's exit instead: exit 1.
        assert!(
            status.signal() == Some(number) || (signal == "INT" && status.code() == Some(1)),
            "SIG{signal}: {status}"
        );
        assert_cleaned_up(private_tmp.path());
        assert!(!dir.path().join("stalled.pdf").exists());
    }
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
    let private_tmp = tempfile::tempdir().unwrap();
    let output = run_settled(|| {
        let mut cmd = presenter(dir.path());
        cmd.env("CHROME_PATH", &fake)
            .env("TMPDIR", private_tmp.path())
            .args(["export", "pdf", "deck.md"]);
        cmd
    });
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(text(&output.stdout), "");
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with(
            "error: Chrome failed to print the deck: Chrome exited (exit status: 4) while \
             starting Chrome\n"
        ),
        "{stderr}"
    );
    assert_cleaned_up(private_tmp.path());
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
fn a_browser_that_never_answers_is_killed_and_cleaned_up() {
    let dir = project(&[("deck.md", "# Slide\n")]);
    let pid_file = dir.path().join("chrome.pid");
    // Starts, keeps every pipe open and never speaks the DevTools protocol; its command
    // line keeps Chrome's arguments, --user-data-dir included.
    let fake = fake_chrome(
        dir.path(),
        "fake-chrome",
        &format!(
            "echo $$ > '{}'\nwhile :; do sleep 1; done",
            pid_file.display()
        ),
    );
    let private_tmp = tempfile::tempdir().unwrap();
    // SIGKILL presenter after 2 s: the fake browser ignores its pipe closing, so the
    // watchdog has to kill it.
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_presenter"))
        .current_dir(dir.path())
        .env_remove("RUST_LOG")
        .env("TMPDIR", private_tmp.path())
        .args(["export", "pdf", "deck.md", "--chrome"])
        .arg(&fake)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while !pid_file.exists() && started.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_secs(2));
    child.kill().unwrap();
    child.wait().unwrap();
    let pid = std::fs::read_to_string(&pid_file).unwrap();
    assert_cleaned_up(private_tmp.path());
    let alive = std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(
        !alive.success(),
        "the fake browser {} still runs",
        pid.trim()
    );
}

#[cfg(unix)]
#[test]
fn a_bare_chrome_name_is_looked_up_on_path() {
    let dir = project(&[("deck.md", "# Slide\n")]);
    let output = presenter(dir.path())
        .env("PATH", dir.path())
        .args(["export", "pdf", "deck.md", "--chrome", "no-such-browser"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with("error: --chrome no-such-browser: not found on PATH\n"),
        "{stderr}"
    );
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

// --- Audit fixes (A1): the PDF never overwrites an input --------------------------------

/// The bytes of each of `files` (relative to `dir`).
fn snapshot(dir: &Path, files: &[&str]) -> Vec<Vec<u8>> {
    files
        .iter()
        .map(|file| std::fs::read(dir.join(file)).unwrap())
        .collect()
}

/// Runs `presenter export pdf <args>` in `dir` with a browser that does not exist and
/// asserts the audit F01 refusal: exit 2 (before the browser is looked for, which would
/// exit 3), `error: refusing to write …` on stderr, `protected` unchanged. Returns stderr.
fn assert_refused(dir: &Path, args: &[&str], protected: &[&str]) -> String {
    let before = snapshot(dir, protected);
    let output = presenter(dir)
        .args(["export", "pdf"])
        .args(args)
        .args(["--chrome", "/nonexistent/chrome"])
        .assert()
        .code(2)
        .stdout("")
        .get_output()
        .clone();
    let stderr = text(&output.stderr);
    assert!(
        stderr
            .lines()
            .last()
            .unwrap()
            .starts_with("error: refusing to write "),
        "{stderr}"
    );
    assert!(!stderr.contains("not an executable"), "{stderr}");
    assert_eq!(
        snapshot(dir, protected),
        before,
        "{args:?} changed an input"
    );
    stderr
}

#[test]
fn export_refuses_to_overwrite_its_input() {
    let dir = project(&[("d.md", "# keep\n"), ("presenter.toml", "")]);
    // No `does not end in .pdf` warning for a file that is not going to be written.
    let stderr = assert_refused(dir.path(), &["d.md", "-o", "d.md"], &["d.md"]);
    assert_eq!(
        stderr,
        "error: refusing to write d.md: it is the input file d.md; choose another output \
         path with -o\n"
    );
    let stderr = assert_refused(
        dir.path(),
        &["d.md", "-o", "presenter.toml"],
        &["presenter.toml"],
    );
    assert!(
        stderr.contains("it is the config file presenter.toml"),
        "{stderr}"
    );
    std::fs::copy(
        root().join("tests/fixtures/ipynb/minimal.ipynb"),
        dir.path().join("nb.ipynb"),
    )
    .unwrap();
    assert_refused(
        dir.path(),
        &["-q", "nb.ipynb", "-o", "nb.ipynb"],
        &["nb.ipynb"],
    );
    assert!(!dir.path().join("dist").exists());
}

#[cfg(unix)]
#[test]
fn export_refuses_links_to_its_input() {
    let dir = project(&[("d.md", "# keep\n")]);
    std::os::unix::fs::symlink("d.md", dir.path().join("alias.pdf")).unwrap();
    std::fs::hard_link(dir.path().join("d.md"), dir.path().join("hard.pdf")).unwrap();
    let stderr = assert_refused(dir.path(), &["d.md", "-o", "alias.pdf"], &["d.md"]);
    assert!(
        stderr.contains("refusing to write alias.pdf: it is the input file d.md"),
        "{stderr}"
    );
    assert_refused(dir.path(), &["d.md", "-o", "hard.pdf"], &["d.md"]);
    assert_refused(dir.path(), &["d.md", "-o", "missing/../d.md"], &["d.md"]);
    let upper = dir.path().join("D.MD");
    if upper.exists() {
        assert_refused(dir.path(), &["d.md", "-o", "D.MD"], &["d.md"]);
    }
    // A link reached after a `..` out of a directory that does not exist yet: work/link
    // points to sub/, so `work/nonexist/../link/../d.md` is d.md.
    std::fs::create_dir_all(dir.path().join("work")).unwrap();
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    std::os::unix::fs::symlink("../sub", dir.path().join("work/link")).unwrap();
    let stderr = assert_refused(
        dir.path(),
        &["d.md", "-o", "work/nonexist/../link/../d.md"],
        &["d.md"],
    );
    assert!(
        stderr.ends_with(
            "error: refusing to write d.md: it is the input file d.md; choose another output \
             path with -o\n"
        ),
        "{stderr}"
    );
    assert!(!dir.path().join("work/nonexist").exists());
}

#[cfg(unix)]
#[test]
fn export_refuses_to_overwrite_an_asset_before_the_browser_starts() {
    let dir = project(&[("d.md", "# Plot\n\n![plot](plot.png)\n")]);
    std::fs::copy(
        root().join("tests/fixtures/md/images/ok.png"),
        dir.path().join("plot.png"),
    )
    .unwrap();
    let marker = dir.path().join("chrome-ran");
    let fake = fake_chrome(
        dir.path(),
        "fake-chrome",
        &format!("touch '{}'\nexit 1", marker.display()),
    );
    let before = snapshot(dir.path(), &["d.md", "plot.png"]);
    let output = run_settled(|| {
        let mut cmd = presenter(dir.path());
        cmd.args(["export", "pdf", "d.md", "-o", "plot.png", "--chrome"])
            .arg(&fake);
        cmd
    });
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = text(&output.stderr);
    assert!(
        stderr.ends_with(
            "error: refusing to write plot.png: it is the deck's asset plot.png; choose \
             another output path with -o\n"
        ),
        "{stderr}"
    );
    assert!(!marker.exists(), "the browser ran");
    assert_eq!(snapshot(dir.path(), &["d.md", "plot.png"]), before);
}

#[test]
fn export_output_arguments_are_checked() {
    let dir = project(&[("d.md", "# Slide\n")]);
    std::fs::create_dir(dir.path().join("existing")).unwrap();
    for out in ["out/", "existing", "pdfs/.", "pdfs/..", "existing/."] {
        presenter(dir.path())
            .args(["export", "pdf", "d.md", "-o", out])
            .args(["--chrome", "/nonexistent/chrome"])
            .assert()
            .code(2)
            .stderr(format!(
                "error: -o {out} names a directory; give the PDF file to write\n"
            ));
    }
    assert!(!dir.path().join("pdfs").exists());
    // A name without .pdf is allowed; its warning comes once the PDF is going to be
    // written, so not when no browser is found (exit 3).
    let output = presenter(dir.path())
        .args(["export", "pdf", "d.md", "-o", "slides.html"])
        .args(["--chrome", "/nonexistent/chrome"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let stderr = text(&output.stderr);
    assert!(stderr.starts_with("error: --chrome "), "{stderr}");
}

#[cfg(unix)]
#[test]
fn a_pdf_name_without_pdf_is_warned_about_just_before_the_browser_starts() {
    let dir = project(&[("d.md", "# Slide\n")]);
    let fake = fake_chrome(
        dir.path(),
        "fake-chrome",
        "echo 'fake chrome: no display' >&2\nexit 4",
    );
    let private_tmp = tempfile::tempdir().unwrap();
    let export = |args: &[&str]| {
        run_settled(|| {
            let mut cmd = presenter(dir.path());
            cmd.env("TMPDIR", private_tmp.path())
                .args(args)
                .arg("--chrome")
                .arg(&fake);
            cmd
        })
    };
    let output = export(&["export", "pdf", "d.md", "-o", "slides.html"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with(
            "warning: -o slides.html does not end in .pdf; the file will hold a PDF\n\
             error: Chrome failed to print the deck: "
        ),
        "{stderr}"
    );
    // -q hides the warning, not the error.
    let output = export(&["-q", "export", "pdf", "d.md", "-o", "slides.html"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with("error: Chrome failed to print the deck: "),
        "{stderr}"
    );
    assert!(!stderr.contains("warning"), "{stderr}");
    // A .pdf name in any case gets no warning.
    let output = export(&["export", "pdf", "d.md", "-o", "slides.PDF"]);
    assert!(text(&output.stderr).starts_with("error: "), "{output:?}");
    assert_cleaned_up(private_tmp.path());
}

#[cfg(unix)]
#[test]
fn export_refuses_to_overwrite_a_theme_file_before_the_browser_starts() {
    let dir = project(&[
        ("d.md", "# Themed\n"),
        ("t/theme.toml", "name = \"t\"\nextends = \"default\"\n"),
    ]);
    let marker = dir.path().join("chrome-ran");
    let fake = fake_chrome(
        dir.path(),
        "fake-chrome",
        &format!("touch '{}'\nexit 1", marker.display()),
    );
    let before = snapshot(dir.path(), &["d.md", "t/theme.toml"]);
    let output = run_settled(|| {
        let mut cmd = presenter(dir.path());
        cmd.args([
            "export",
            "pdf",
            "d.md",
            "--theme",
            "./t",
            "-o",
            "t/theme.toml",
        ])
        .arg("--chrome")
        .arg(&fake);
        cmd
    });
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let shown = Path::new("t/theme.toml").display().to_string();
    assert_eq!(
        text(&output.stderr),
        format!(
            "error: refusing to write {shown}: it is the theme file {shown}; choose another \
             output path with -o\n"
        )
    );
    assert!(!marker.exists(), "the browser ran");
    assert_eq!(snapshot(dir.path(), &["d.md", "t/theme.toml"]), before);
}

/// The limitations `export pdf --help` lists (T45), including the empty last page a slide
/// that spans pages can end with (audit F15, documented rather than removed; wave 8).
#[test]
fn help_lists_the_limitations_of_the_pdf() {
    let output = presenter(Path::new(env!("CARGO_MANIFEST_DIR")))
        .args(["export", "pdf", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = text(&output.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for expected in [
        "Limitations (reveal.js print mode, ?print-pdf):",
        "every fragment step is printed on its own page",
        "continues on extra pages, up to [reveal] pdfMaxPagesPerSlide",
        "can end with a page that is empty but for its background (about one in ten to \
         twenty of them). Nothing is lost.",
        "The PDF is not byte-for-byte reproducible",
    ] {
        assert!(help.contains(expected), "{expected:?} in {help}");
    }
}

// --- `presenter export pptx` ----------------------------------------------------------------

/// The entries of a `.pptx` written by presenter (stored entries only), read through its
/// central directory: `(name, data)` in directory order.
fn zip_entries(zip: &[u8]) -> Vec<(String, Vec<u8>)> {
    let u16_at = |at: usize| u16::from_le_bytes([zip[at], zip[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes(zip[at..at + 4].try_into().unwrap());
    let end = zip.len() - 22;
    assert_eq!(u32_at(end), 0x0605_4b50, "end of central directory");
    let count = usize::from(u16_at(end + 10));
    let mut at = u32_at(end + 16) as usize;
    let mut found = Vec::new();
    for _ in 0..count {
        assert_eq!(u32_at(at), 0x0201_4b50, "central directory record");
        let size = u32_at(at + 24) as usize;
        let name_len = usize::from(u16_at(at + 28));
        let offset = u32_at(at + 42) as usize;
        let name = String::from_utf8(zip[at + 46..at + 46 + name_len].to_vec()).unwrap();
        at += 46 + name_len;
        assert_eq!(u32_at(offset), 0x0403_4b50, "local header of {name}");
        let local_name_len = usize::from(u16_at(offset + 26));
        let extra_len = usize::from(u16_at(offset + 28));
        let start = offset + 30 + local_name_len + extra_len;
        found.push((name, zip[start..start + size].to_vec()));
    }
    found
}

/// The entry `name` of `entries`, as text.
fn entry_text(entries: &[(String, Vec<u8>)], name: &str) -> String {
    let (_, data) = entries
        .iter()
        .find(|(entry, _)| entry == name)
        .unwrap_or_else(|| panic!("no entry {name}"));
    String::from_utf8(data.clone()).unwrap()
}

/// The `(width, height)` of a PNG.
fn png_size(png: &[u8]) -> (u32, u32) {
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "not a PNG");
    let number = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
    (number(16), number(20))
}

/// Reads a written `.pptx` and checks its signature.
fn read_pptx(path: &Path) -> Vec<u8> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(
        bytes.starts_with(b"PK\x03\x04"),
        "not a ZIP: {}",
        path.display()
    );
    bytes
}

/// Asserts the `Wrote <path> (<n> slides, <size>)` line and returns `<n>`.
fn assert_wrote_slides(output: &Output, path_suffix: &str) -> usize {
    let stdout = text(&output.stdout);
    let line = Regex::new(r"^Wrote (.+) \((\d+) slides?, \d+(\.\d)? (B|kB|MB)\)\n$").unwrap();
    let caps = line
        .captures(&stdout)
        .unwrap_or_else(|| panic!("unexpected stdout: {stdout:?}"));
    assert!(caps[1].ends_with(path_suffix), "{stdout}");
    caps[2].parse().unwrap()
}

/// The slide names (`p:cSld/@name`) of a `.pptx`, in slide order.
fn slide_names(entries: &[(String, Vec<u8>)], slides: usize) -> Vec<String> {
    let name = Regex::new(r#"<p:cSld name="([^"]*)">"#).unwrap();
    (1..=slides)
        .map(|n| {
            let xml = entry_text(entries, &format!("ppt/slides/slide{n}.xml"));
            name.captures(&xml).unwrap()[1].to_string()
        })
        .collect()
}

/// The XML of slide `n` of `entries`.
fn slide_xml(entries: &[(String, Vec<u8>)], n: usize) -> String {
    entry_text(entries, &format!("ppt/slides/slide{n}.xml"))
}

/// The text of a slide's XML: the runs of each paragraph joined, one line per paragraph.
fn slide_text(xml: &str) -> String {
    let paragraph = Regex::new(r"<a:p>(.*?)</a:p>").unwrap();
    let run = Regex::new(r"<a:t>([^<]*)</a:t>").unwrap();
    paragraph
        .captures_iter(xml)
        .map(|caps| {
            run.captures_iter(&caps[1])
                .map(|r| r[1].to_string())
                .collect::<Vec<_>>()
                .join("")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The media parts a slide's relationships reference, as `(target, bytes)`.
fn slide_media(entries: &[(String, Vec<u8>)], n: usize) -> Vec<(String, Vec<u8>)> {
    let rels = entry_text(entries, &format!("ppt/slides/_rels/slide{n}.xml.rels"));
    Regex::new(r#"Target="\.\./media/([^"]+)""#)
        .unwrap()
        .captures_iter(&rels)
        .map(|caps| {
            let name = format!("ppt/media/{}", &caps[1]);
            let (_, data) = entries
                .iter()
                .find(|(entry, _)| *entry == name)
                .unwrap_or_else(|| panic!("no entry {name}"));
            (caps[1].to_string(), data.clone())
        })
        .collect()
}

#[test]
fn exports_the_example_deck_to_editable_slides() {
    if chrome().is_none() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let private_tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("nested/slides.pptx");
    let output = presenter(&root())
        .env("TMPDIR", private_tmp.path())
        .args(["export", "pptx", "examples/basic/slides.md", "-o"])
        .arg(&out)
        .assert()
        .success()
        .get_output()
        .clone();
    let slides = assert_wrote_slides(&output, "slides.pptx");
    #[cfg(unix)]
    assert_cleaned_up(private_tmp.path());

    let pptx = read_pptx(&out);
    let entries = zip_entries(&pptx);
    assert_eq!(entries[0].0, "[Content_Types].xml");
    let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
    for n in 1..=slides {
        assert!(names.contains(&format!("ppt/slides/slide{n}.xml").as_str()));
    }
    assert!(!names.contains(&format!("ppt/slides/slide{}.xml", slides + 1).as_str()));

    // One slide per page of the PDF.
    let pdf_out = tmp.path().join("slides.pdf");
    presenter(&root())
        .args(["-q", "export", "pdf", "examples/basic/slides.md", "-o"])
        .arg(&pdf_out)
        .assert()
        .success();
    assert_eq!(slides, page_count(&read_pdf(&pdf_out)));
    let presentation = entry_text(&entries, "ppt/presentation.xml");
    assert!(
        presentation.contains(r#"<p:sldSz cx="12192000" cy="6858000"/>"#),
        "{presentation}"
    );

    // Slides are named after their headings, with their fragment step.
    let slide_names = slide_names(&entries, slides);
    assert_eq!(slide_names[0], "Presenter basics");
    assert_eq!(slide_names[1], "What this deck shows");
    assert!(
        slide_names
            .iter()
            .any(|name| name.starts_with("Fragments, step 1 of ")),
        "{slide_names:?}"
    );
    assert!(
        slide_names
            .iter()
            .any(|name| name == "Code with line stepping, step 3 of 3"),
        "{slide_names:?}"
    );
    let find = |heading: &str| {
        slide_names
            .iter()
            .position(|name| name == heading)
            .unwrap_or_else(|| panic!("no slide {heading}: {slide_names:?}"))
            + 1
    };

    // The title slide: the heading in the title placeholder, centred; the subtitle, the
    // author and the date as text; the notes in its notes slide; no pictures.
    let title = slide_xml(&entries, 1);
    assert!(
        title.contains(r#"<p:nvPr><p:ph type="title"/></p:nvPr>"#),
        "{title}"
    );
    assert!(title.contains(r#"algn="ctr""#), "{title}");
    let text = slide_text(&title);
    for expected in [
        "Presenter basics",
        "Every Markdown feature of presenter in one deck",
        "Johannes Blaschke",
        "2026-10-08",
    ] {
        assert!(text.contains(expected), "{expected:?} in {text}");
    }
    assert!(!title.contains("<p:pic>"), "{title}");
    let notes = entry_text(&entries, "ppt/notesSlides/notesSlide1.xml");
    assert!(
        notes.contains("The title slide uses layout=title"),
        "{notes}"
    );
    assert!(entry_text(&entries, "ppt/slides/_rels/slide1.xml.rels").contains("notesSlide1.xml"));

    // A bulleted list: one text box, a paragraph per item with a hanging bullet.
    let list = slide_xml(&entries, 2);
    assert_eq!(list.matches(r#"<a:buChar char="•"/>"#).count(), 5, "{list}");
    assert!(
        Regex::new(r#"<a:pPr marL="\d+" indent="-\d+" lvl="0""#)
            .unwrap()
            .is_match(&list),
        "{list}"
    );
    let text = slide_text(&list);
    assert!(
        text.contains("title slide") && text.contains("speaker notes"),
        "{text}"
    );
    assert!(list.contains(r#"b="1""#), "{list}");

    // Fragments: the first step shows nothing of the first fragment, the second does.
    let step1 = slide_text(&slide_xml(&entries, find("Fragments, step 1 of 5")));
    let step2 = slide_text(&slide_xml(&entries, find("Fragments, step 2 of 5")));
    assert!(!step1.contains("First click"), "{step1}");
    assert!(step2.contains("First click"), "{step2}");

    // The figure is the deck's own PNG, byte for byte.
    let plot = std::fs::read(root().join("examples/basic/figures/plot.png")).unwrap();
    let media = slide_media(&entries, find("Two columns"));
    assert!(
        media.iter().any(|(_, bytes)| *bytes == plot),
        "{:?}",
        media.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );

    // Code: a paragraph per line in a monospace font, the highlighting as run colours.
    let code = slide_xml(&entries, find("Code with line stepping, step 1 of 3"));
    assert!(
        code.contains(r#"<a:latin typeface="Courier New"/>"#),
        "{code}"
    );
    assert!(code.matches("<a:p>").count() >= 9, "{code}");
    assert!(slide_text(&code).contains("import math"), "{code}");
    assert!(code.contains(r#"wrap="none""#), "{code}");

    // Math: inline math is text, display math a transparent picture.
    let math = slide_xml(&entries, find("Math"));
    assert!(slide_text(&math).contains("circle of radius"), "{math}");
    let media = slide_media(&entries, find("Math"));
    assert_eq!(media.len(), 1, "{media:?}");
    let (width, height) = png_size(&media[0].1);
    assert!(
        width > 1000 && height > 100 && height < 400,
        "{width}×{height}"
    );

    // A table with three columns and four rows.
    let table = slide_xml(&entries, find("A table"));
    assert_eq!(table.matches("<a:gridCol ").count(), 3, "{table}");
    assert_eq!(table.matches("<a:tr ").count(), 4, "{table}");
    assert!(slide_text(&table).contains("a horizontal slide"), "{table}");

    // A footnote's links are hyperlinks.
    let rels = entry_text(
        &entries,
        &format!("ppt/slides/_rels/slide{}.xml.rels", find("A footnote")),
    );
    assert!(
        rels.contains(r#"Target="https://revealjs.com" TargetMode="External""#),
        "{rels}"
    );

    // Backgrounds: a colour, and a picture at 20 % opacity.
    let dark = slide_xml(&entries, find("Backgrounds"));
    assert!(
        dark.contains(r#"<p:bg><p:bgPr><a:solidFill><a:srgbClr val="0F172A"/>"#),
        "{dark}"
    );
    let image = slide_xml(&entries, find("Background images"));
    assert!(image.contains(r#"<a:alphaModFix amt="20000"/>"#), "{image}");
    assert!(
        slide_media(&entries, find("Background images"))
            .iter()
            .any(|(_, bytes)| *bytes == plot)
    );

    // The deck's title and author are in the properties.
    let core = entry_text(&entries, "docProps/core.xml");
    assert!(
        core.contains("<dc:title>Presenter basics</dc:title>"),
        "{core}"
    );
    assert!(
        core.contains("<dc:creator>Johannes Blaschke</dc:creator>"),
        "{core}"
    );
    assert!(!core.contains("dcterms:created"), "{core}");
}

#[test]
fn powerpoint_slides_follow_pdf_separate_fragments_and_go_next_to_the_html() {
    if chrome().is_none() {
        return;
    }
    let dir = project(&[
        ("deck.md", FRAGMENT_DECK),
        ("presenter.toml", "[reveal]\npdfSeparateFragments = false\n"),
        ("separate.toml", "[reveal]\npdfSeparateFragments = true\n"),
    ]);

    // Collapsed fragments: one slide per slide, written to dist/deck.pptx.
    let output = presenter(dir.path())
        .args(["export", "pptx", "deck.md"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_eq!(assert_wrote_slides(&output, "deck.pptx"), 3);
    let entries = zip_entries(&read_pptx(&dir.path().join("dist/deck.pptx")));
    assert_eq!(slide_names(&entries, 3), ["One", "Two", "Three"]);
    let two = slide_text(&slide_xml(&entries, 2));
    assert!(
        two.contains("First step") && two.contains("Second step"),
        "{two}"
    );

    // reveal's default: the fragment slide becomes one slide per step.
    let output = presenter(dir.path())
        .args(["--config", "separate.toml", "export", "pptx", "deck.md"])
        .args(["-o", "separate.pptx"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_eq!(assert_wrote_slides(&output, "separate.pptx"), 5);
    let entries = zip_entries(&read_pptx(&dir.path().join("separate.pptx")));
    assert_eq!(
        slide_names(&entries, 5),
        [
            "One",
            "Two, step 1 of 3",
            "Two, step 2 of 3",
            "Two, step 3 of 3",
            "Three"
        ]
    );
    let steps: Vec<String> = (2..=4)
        .map(|n| slide_text(&slide_xml(&entries, n)))
        .collect();
    assert!(!steps[0].contains("First step"), "{steps:?}");
    assert!(
        steps[1].contains("First step") && !steps[1].contains("Second step"),
        "{steps:?}"
    );
    assert!(steps[2].contains("Second step"), "{steps:?}");
}

#[test]
fn powerpoint_scale_sizes_the_photographs_and_the_file_is_reproducible() {
    if chrome().is_none() {
        return;
    }
    // An inline SVG is photographed: at --scale 1 at its own size, at the default 2 twice.
    let deck = "# Only slide\n\n<svg width=\"120\" height=\"60\"><rect width=\"120\" height=\"60\" fill=\"teal\"/></svg>\n";
    let dir = project(&[("deck.md", deck)]);
    let export = |args: &[&str], out: &str| {
        presenter(dir.path())
            .args(["-q", "export", "pptx", "deck.md", "-o", out])
            .args(args)
            .assert()
            .success()
            .stdout("");
        read_pptx(&dir.path().join(out))
    };
    let first = export(&["--scale", "1"], "one.pptx");
    let entries = zip_entries(&first);
    assert_eq!(slide_names(&entries, 1), ["Only slide"]);
    let media = slide_media(&entries, 1);
    assert_eq!(media.len(), 1, "{media:?}");
    assert_eq!(png_size(&media[0].1), (120, 60));
    let scaled = export(&[], "two.pptx");
    let media = slide_media(&zip_entries(&scaled), 1);
    assert_eq!(png_size(&media[0].1), (240, 120));
    // Only the 1980-01-01 ZIP timestamp and no dates in the properties: the same deck
    // gives the same file.
    assert_eq!(export(&["--scale", "1"], "three.pptx"), first);
}

#[test]
fn a_missing_browser_exits_3_for_powerpoint_too() {
    let dir = project(&[("deck.md", "# Slide\n")]);
    let output = presenter(dir.path())
        .args([
            "export",
            "pptx",
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
    assert!(!dir.path().join("dist").exists());
}

#[test]
fn powerpoint_output_arguments_are_checked() {
    let dir = project(&[("d.md", "# Slide\n"), ("presenter.toml", "")]);
    std::fs::create_dir(dir.path().join("existing")).unwrap();
    for out in ["out/", "existing", "existing/."] {
        presenter(dir.path())
            .args(["export", "pptx", "d.md", "-o", out])
            .args(["--chrome", "/nonexistent/chrome"])
            .assert()
            .code(2)
            .stderr(format!(
                "error: -o {out} names a directory; give the PowerPoint file to write\n"
            ));
    }
    // Never over an input: exit 2 before the browser is looked for.
    for (out, input) in [
        ("d.md", "input file d.md"),
        ("presenter.toml", "config file presenter.toml"),
    ] {
        let output = presenter(dir.path())
            .args(["export", "pptx", "d.md", "-o", out])
            .args(["--chrome", "/nonexistent/chrome"])
            .assert()
            .code(2)
            .stdout("")
            .get_output()
            .clone();
        assert_eq!(
            text(&output.stderr),
            format!(
                "error: refusing to write {out}: it is the {input}; choose another output \
                 path with -o\n"
            )
        );
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("d.md")).unwrap(),
        "# Slide\n"
    );
    // An unknown input format is a usage error too.
    std::fs::write(dir.path().join("deck.txt"), "# Slide\n").unwrap();
    presenter(dir.path())
        .args(["export", "pptx", "deck.txt"])
        .assert()
        .code(2);
}

#[cfg(unix)]
#[test]
fn a_powerpoint_name_without_pptx_is_warned_about_just_before_the_browser_starts() {
    let dir = project(&[("d.md", "# Slide\n")]);
    let fake = fake_chrome(
        dir.path(),
        "fake-chrome",
        "echo 'fake chrome: no display' >&2\nexit 4",
    );
    let private_tmp = tempfile::tempdir().unwrap();
    let export = |args: &[&str]| {
        run_settled(|| {
            let mut cmd = presenter(dir.path());
            cmd.env("TMPDIR", private_tmp.path())
                .args(args)
                .arg("--chrome")
                .arg(&fake);
            cmd
        })
    };
    let output = export(&["export", "pptx", "d.md", "-o", "slides.ppt"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with(
            "warning: -o slides.ppt does not end in .pptx; the file will hold a PowerPoint \
             presentation\nerror: Chrome failed to capture the deck: Chrome exited (exit \
             status: 4) while starting Chrome\n"
        ),
        "{stderr}"
    );
    assert!(stderr.contains("    fake chrome: no display"), "{stderr}");
    assert!(!dir.path().join("slides.ppt").exists());
    // -q hides the warning, not the error; a .pptx name in any case gets no warning.
    let output = export(&["-q", "export", "pptx", "d.md", "-o", "slides.ppt"]);
    assert!(
        text(&output.stderr).starts_with("error: Chrome failed to capture the deck: "),
        "{output:?}"
    );
    let output = export(&["export", "pptx", "d.md", "-o", "slides.PPTX"]);
    assert!(text(&output.stderr).starts_with("error: "), "{output:?}");
    assert_cleaned_up(private_tmp.path());
}

#[cfg(unix)]
#[test]
fn powerpoint_build_errors_stop_before_the_browser_starts() {
    let dir = project(&[("deck.md", "# Slide\n\n![plot](missing.png)\n")]);
    let marker = dir.path().join("chrome-ran");
    let fake = fake_chrome(
        dir.path(),
        "fake-chrome",
        &format!("touch '{}'\nexit 1", marker.display()),
    );
    let output = run_settled(|| {
        let mut cmd = presenter(dir.path());
        cmd.args(["export", "pptx", "deck.md", "--chrome"])
            .arg(&fake);
        cmd
    });
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        text(&output.stderr).contains("asset not found"),
        "{output:?}"
    );
    assert!(!marker.exists(), "the browser ran despite the build error");
    assert!(!dir.path().join("dist").exists());
}

#[cfg(unix)]
#[test]
fn a_failed_capture_stops_the_browser_and_writes_nothing() {
    if chrome().is_none() {
        return;
    }
    let deck = "# Not capturable\n\n<script>window.addEventListener('load', () => \
                { Reveal.isPrintView = () => false; });</script>\n";
    let dir = project(&[("deck.md", deck)]);
    let private_tmp = tempfile::tempdir().unwrap();
    let output = presenter(dir.path())
        .env("TMPDIR", private_tmp.path())
        .args(["export", "pptx", "deck.md", "-o", "deck.pptx"])
        .assert()
        .code(1)
        .stdout("")
        .get_output()
        .clone();
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with(
            "error: Chrome failed to capture the deck: reveal.js did not switch to its print \
             view (?print-pdf)\n"
        ),
        "{stderr}"
    );
    assert!(!dir.path().join("deck.pptx").exists());
    assert_cleaned_up(private_tmp.path());
}

/// The limitations `export pptx --help` lists.
#[test]
fn help_lists_what_the_powerpoint_file_holds() {
    let output = presenter(Path::new(env!("CARGO_MANIFEST_DIR")))
        .args(["export", "pptx", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = text(&output.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for expected in [
        "What the file holds (from reveal.js' print mode, ?print-pdf):",
        "Every page of the PDF export becomes one editable slide",
        "tables are tables, images are the deck's image files",
        "every fragment step is a slide of its own",
        "continues on extra slides, up to [reveal] pdfMaxPagesPerSlide",
        "exits with code 3",
        "--scale <N>",
    ] {
        assert!(help.contains(expected), "{expected:?} in {help}");
    }
}
