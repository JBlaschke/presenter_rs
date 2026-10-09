//! T45 end to end: `presenter export pdf` (docs/spec/cli.md `export`).
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
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

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
    let stderr = assert_refused(dir.path(), &["d.md", "-o", "d.md"], &["d.md"]);
    assert_eq!(
        stderr,
        "warning: -o d.md does not end in .pdf; the file will hold a PDF\n\
         error: refusing to write d.md: it is the input file d.md; choose another output \
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
    for out in ["out/", "existing"] {
        presenter(dir.path())
            .args(["export", "pdf", "d.md", "-o", out])
            .args(["--chrome", "/nonexistent/chrome"])
            .assert()
            .code(2)
            .stderr(format!(
                "error: -o {out} names a directory; give the PDF file to write\n"
            ));
    }
    // A name without .pdf is allowed, with a warning (then: no browser, exit 3).
    let output = presenter(dir.path())
        .args(["export", "pdf", "d.md", "-o", "slides.html"])
        .args(["--chrome", "/nonexistent/chrome"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with(
            "warning: -o slides.html does not end in .pdf; the file will hold a PDF\nerror: "
        ),
        "{stderr}"
    );
    let output = presenter(dir.path())
        .args(["-q", "export", "pdf", "d.md", "-o", "slides.PDF"])
        .args(["--chrome", "/nonexistent/chrome"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    assert!(text(&output.stderr).starts_with("error: "), "{output:?}");
}
