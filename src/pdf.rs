//! PDF export through headless Chrome (`presenter export pdf`, `docs/spec/cli.md` export).
//!
//! [`find_chrome`] locates a Chrome or Chromium executable; [`html_to_pdf`] prints a
//! standalone deck with it, in reveal.js' print view (`?print-pdf`). macOS and Linux only:
//! Chrome is started through `/bin/sh`.
//!
//! # How a deck is printed
//!
//! Chrome's one-shot `--print-to-pdf` flag cannot wait for reveal.js to lay the pages out
//! (and hangs in some environments), so [`html_to_pdf`] drives Chrome over the DevTools
//! protocol instead, with `--remote-debugging-pipe`: NUL-terminated JSON messages on Chrome's
//! file descriptors 3 (commands) and 4 (replies and events), flat sessions. Chrome opens no
//! network port, so no other local user or process can drive it.
//!
//! 1. A private temporary directory (mode 0700) holds Chrome's fresh profile: in the
//!    system's temporary directory (`$TMPDIR`), or for a snap browser in its
//!    `~/snap/<name>/common` when that exists (a snap has a private `/tmp`). Chrome starts
//!    headless with a quiet throwaway configuration (no first-run UI, sync, extensions or
//!    keychain access; plus `--no-sandbox` when running as root on Linux, which Chrome
//!    requires); `/bin/sh` moves presenter's pipes to descriptors 3 and 4 and `exec`s it.
//! 2. `Target.createTarget` (about:blank) and `Target.attachToTarget {flatten: true}` open a
//!    page; `Fetch.enable` for `http://presenter.localhost/*`, `Page.enable`,
//!    `Runtime.enable`, `Inspector.enable`; `Page.navigate` to [`DECK_URL`]. presenter answers
//!    Chrome's request for it with the deck (`Fetch.fulfillRequest`; anything else there gets
//!    a 404), so the deck is never written to disk and a confined browser (a snap, with its
//!    private `/tmp`) needs no access to presenter's files. Like the `file://` page of
//!    `presenter build`, a `.localhost` page is a secure context. Then the page's
//!    `Page.loadEventFired`. JavaScript dialogs (`alert`, `confirm`, …) are dismissed, with a
//!    warning.
//! 3. `Runtime.evaluate` of [`READY_SCRIPT`] (awaited): waits for `Reveal.isReady()`, the
//!    end of reveal's print layout (`html.print-pdf` set, `loading-scroll-mode` gone from
//!    the viewport, i.e. after its `pdf-ready` event), `document.fonts.ready` and two
//!    animation frames. KaTeX and highlight.js run before reveal is ready.
//! 4. `Page.printToPDF {preferCSSPageSize, printBackground, margins 0, ReturnAsStream}`
//!    (reveal sets `@page { size: …; margin: 0 }`), read with `IO.read` until `eof`.
//! 5. `Browser.close`, and the command pipe is closed; Chrome gets [`CLOSE_GRACE`] to exit,
//!    then it is killed.
//!
//! Everything shares one deadline (`timeout`, [`DEFAULT_TIMEOUT`] for the CLI); an exited
//! Chrome is noticed within 0.1 s. A failure is an [`Error::Other`] that names the browser,
//! says what failed (`Chrome exited (…) while …`, `timed out after 120 s while …`), lists the
//! page's uncaught errors and dismissed dialogs when there were any, and ends with the last
//! lines Chrome wrote on stderr.
//!
//! # Cleanup
//!
//! [`html_to_pdf`] stops Chrome and removes the temporary directory before it returns, on
//! errors too. If presenter dies during an export instead (Ctrl-C, `kill`, even SIGKILL),
//! Chrome sees its command pipe close and exits, and the export's watchdog removes the
//! directory. The watchdog is a small `/bin/sh` script started next to Chrome, in its own
//! process group and ignoring SIGINT, SIGTERM and SIGHUP, that reads presenter's messages on
//! its stdin: Chrome's process id, then `done` once presenter has cleaned up. When its stdin
//! closes without `done`, it waits for Chrome to exit (killing it after 3 s, while its
//! command line still names the directory) and removes the directory. presenter's own signal handling is left alone, so an interrupted export ends
//! the way an interrupted process does.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, ErrorKind, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};

use crate::error::{Error, Result};

/// How long `presenter export pdf` lets Chrome take, from start to the last PDF byte.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// How long Chrome may take to exit after `Browser.close` before it is killed.
pub const CLOSE_GRACE: Duration = Duration::from_secs(5);

/// Where Chrome loads the deck from (module docs, step 2). presenter answers every request
/// for this origin itself, so the name is never resolved and nothing is fetched from the
/// network or the loopback interface.
pub const DECK_URL: &str = "http://presenter.localhost/deck.html?print-pdf";

/// The requests presenter answers instead of the network (a `Fetch.enable` pattern).
const DECK_ORIGIN_PATTERN: &str = "http://presenter.localhost/*";

/// macOS app bundles searched by [`find_chrome`], in order: bundle and executable name.
/// Each is looked for in `/Applications`, then `~/Applications`.
const MAC_APPS: &[(&str, &str)] = &[
    ("Google Chrome.app", "Google Chrome"),
    ("Chromium.app", "Chromium"),
    ("Microsoft Edge.app", "Microsoft Edge"),
    ("Brave Browser.app", "Brave Browser"),
    ("Arc.app", "Arc"),
];

/// Executable names searched on `PATH` by [`find_chrome`], in order.
pub const PATH_NAMES: &[&str] = &[
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
    "microsoft-edge",
];

/// Chrome's command line, before `--user-data-dir=<profile>` and `about:blank`: headless,
/// a quiet throwaway profile (no first-run UI, sync, extensions or keychain access) and the
/// DevTools protocol on descriptors 3 and 4.
const CHROME_FLAGS: &[&str] = &[
    "--headless=new",
    "--disable-gpu",
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-extensions",
    "--disable-background-networking",
    "--disable-component-update",
    "--disable-sync",
    "--disable-default-apps",
    "--disable-domain-reliability",
    "--disable-client-side-phishing-detection",
    "--no-service-autorun",
    "--password-store=basic",
    "--use-mock-keychain",
    "--hide-scrollbars",
    "--mute-audio",
    "--remote-debugging-pipe",
];

/// Runs Chrome (`$0`, with the arguments `$@`) with the shell's stdin and stdout pipes moved
/// to descriptors 3 and 4, where `--remote-debugging-pipe` expects them. `exec` keeps the
/// process id, so presenter's child is Chrome itself.
const LAUNCH_SCRIPT: &str = r#"exec "$0" "$@" 3<&0 4>&1 0</dev/null 1>/dev/null"#;

/// The watchdog of one export (module docs, cleanup); `$1` is the temporary directory. Its
/// stdin carries `chrome <pid>` and `done` lines. Without `done`, it waits up to 3 s for
/// Chrome to exit, then kills it (checking it every second, for at most 10 s), and removes
/// the directory (retrying while Chrome's helper processes finish writing into it). A
/// process counts as Chrome only while its command line names the directory (Chrome's
/// `--user-data-dir`), so a reused process id is never killed (without `ps`, any running
/// process with that id counts).
const JANITOR_SCRIPT: &str = r#"trap '' INT TERM HUP
dir=$1
chrome=
while read -r word arg; do
  case $word in
    chrome) chrome=$arg ;;
    done) exit 0 ;;
  esac
done
running() {
  [ -n "$chrome" ] || return 1
  if command -v ps >/dev/null 2>&1; then
    ps -ww -p "$chrome" -o args= 2>/dev/null | grep -qF -- "$dir"
  else
    kill -0 "$chrome" 2>/dev/null
  fi
}
n=0
while [ "$n" -lt 10 ] && running; do
  if [ "$n" -ge 3 ]; then kill -9 "$chrome" 2>/dev/null; fi
  sleep 1
  n=$((n + 1))
done
n=0
while [ -e "$dir" ] && [ "$n" -lt 10 ]; do
  rm -rf "$dir" 2>/dev/null || sleep 1
  n=$((n + 1))
done
"#;

/// How many of Chrome's last stderr lines an error shows.
const STDERR_TAIL: usize = 30;

/// How long an error report waits for Chrome's last stderr lines to arrive.
const STDERR_DRAIN: Duration = Duration::from_millis(500);

/// How often a wait for Chrome's next message checks whether Chrome is still running.
const POLL: Duration = Duration::from_millis(100);

/// Bytes requested per `IO.read` of the printed PDF.
const READ_CHUNK: u64 = 1 << 20;

/// At most this much of an invalid DevTools message is shown in an error.
const SHOWN_MESSAGE: usize = 200;

/// Awaited in the page before printing (module docs, step 3). Resolves to
/// `{slides, pages}` (reveal's slide count and its `.pdf-page` count) or `{error}`.
pub const READY_SCRIPT: &str = r#"(async () => {
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  for (let waited = 0; typeof Reveal === 'undefined'; waited += 50) {
    if (waited >= 5000) return { error: 'the page does not load reveal.js (no global Reveal)' };
    await sleep(50);
  }
  while (!Reveal.isReady()) await sleep(50);
  if (Reveal.isPrintView && !Reveal.isPrintView()) {
    return { error: 'reveal.js did not switch to its print view (?print-pdf)' };
  }
  const viewport = Reveal.getViewportElement ? Reveal.getViewportElement() : null;
  while (!document.documentElement.classList.contains('print-pdf')
      || (viewport && viewport.classList.contains('loading-scroll-mode'))) {
    await sleep(50);
  }
  await document.fonts.ready;
  await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
  return { slides: Reveal.getTotalSlides(), pages: document.querySelectorAll('.pdf-page').length };
})()"#;

/// Finds a Chrome or Chromium executable, in this order:
///
/// 1. `explicit` (`--chrome PATH`). When given, it is the only candidate: `None` if it is
///    not an executable file. A macOS `.app` bundle stands for its
///    `Contents/MacOS/<name>`, and a bare name without a path separator (`chromium`) that is
///    not a file in the current directory is looked up on `PATH`.
/// 2. `$CHROME_PATH` (when set and not empty), resolved the same way. If it does not name
///    an executable, a warning is logged and the search goes on.
/// 3. macOS only: the app bundles of Google Chrome, Chromium, Microsoft Edge, Brave and
///    Arc in `/Applications`, then `~/Applications`.
/// 4. [`PATH_NAMES`] on `PATH`: `google-chrome`, `google-chrome-stable`, `chromium`,
///    `chromium-browser`, `microsoft-edge`.
///
/// Relative results are made absolute.
pub fn find_chrome(explicit: Option<&Path>) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let bundles = if cfg!(target_os = "macos") {
        mac_bundles(home.as_deref())
    } else {
        Vec::new()
    };
    find_chrome_in(explicit, &|name| std::env::var_os(name), &bundles)
}

/// [`find_chrome`] with the environment (`var`) and the app bundle candidates given.
fn find_chrome_in(
    explicit: Option<&Path>,
    var: &dyn Fn(&str) -> Option<OsString>,
    bundles: &[PathBuf],
) -> Option<PathBuf> {
    let path_var = var("PATH");
    let path_var = path_var.as_deref();
    if let Some(explicit) = explicit {
        return resolve_executable(explicit, path_var);
    }
    if let Some(env) = var("CHROME_PATH").filter(|value| !value.is_empty()) {
        match resolve_executable(Path::new(&env), path_var) {
            Some(found) => return Some(found),
            None => tracing::warn!(
                "CHROME_PATH={} is not an executable file; looking for Chrome elsewhere",
                Path::new(&env).display()
            ),
        }
    }
    if let Some(bundle) = bundles.iter().find(|bundle| is_executable(bundle)) {
        return Some(bundle.clone());
    }
    PATH_NAMES
        .iter()
        .find_map(|name| which(Path::new(name), path_var?))
}

/// The macOS app bundle executables of [`MAC_APPS`]: each app in `/Applications`, then in
/// `<home>/Applications`.
fn mac_bundles(home: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/Applications")];
    roots.extend(home.map(|home| home.join("Applications")));
    MAC_APPS
        .iter()
        .flat_map(|(app, exe)| {
            roots
                .iter()
                .map(move |root| root.join(app).join("Contents/MacOS").join(exe))
        })
        .collect()
}

/// `path` as an executable: itself, the executable of a `.app` bundle, or a bare name
/// looked up on `path_var`.
fn resolve_executable(path: &Path, path_var: Option<&OsStr>) -> Option<PathBuf> {
    if path.extension().is_some_and(|ext| ext == "app") && path.is_dir() {
        let exe = path.join("Contents/MacOS").join(path.file_stem()?);
        return is_executable(&exe).then(|| absolute(&exe));
    }
    if is_executable(path) {
        return Some(absolute(path));
    }
    if is_bare_name(path) {
        which(path, path_var?)
    } else {
        None
    }
}

/// True for a name without any directory part (`chromium`, not `./chromium`).
pub(crate) fn is_bare_name(path: &Path) -> bool {
    path.components().count() == 1 && path.parent() == Some(Path::new(""))
}

/// The first executable `name` in the directories of `path_var` (a `PATH` value).
fn which(name: &Path, path_var: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path_var)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
        .map(|found| absolute(&found))
}

/// `path` made absolute against the current directory (unchanged if that fails).
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// True for a regular file (after symlinks) that is executable (any `x` bit on Unix).
fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

/// Prints the standalone deck `html` to PDF with the Chrome executable `chrome` (module
/// docs) and returns the PDF's bytes. All of it must finish within `timeout`. `chrome` is
/// resolved like `--chrome` ([`find_chrome`] step 1).
///
/// Errors (all [`Error::Other`]): `chrome` is not an executable; the temporary directory
/// cannot be created; Chrome cannot be started; Chrome exits, times out or the page fails
/// (with the browser's path, the page's errors and Chrome's stderr); not macOS or Linux.
pub fn html_to_pdf(chrome: &Path, html: &str, timeout: Duration) -> Result<Vec<u8>> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let base = temp_base(chrome, home.as_deref()).unwrap_or_else(std::env::temp_dir);
    print_in(chrome, html, timeout, &base)
}

/// [`html_to_pdf`] with the temporary directory created in `base`.
fn print_in(chrome: &Path, html: &str, timeout: Duration, base: &Path) -> Result<Vec<u8>> {
    if cfg!(not(unix)) {
        return Err(Error::Other(
            "PDF export needs macOS or Linux (Chrome is started through /bin/sh)".into(),
        ));
    }
    let deadline = Instant::now() + timeout;
    let path_var = std::env::var_os("PATH");
    let executable = resolve_executable(chrome, path_var.as_deref()).ok_or_else(|| {
        Error::Other(format!(
            "cannot start {}: not an executable file",
            chrome.display()
        ))
    })?;
    let mut dir = TempDir::new(base, "presenter-pdf")?;
    tracing::info!("printing the deck with {}", executable.display());
    let mut browser = Browser::launch(&executable, &dir.path().join("profile"))?;
    dir.watch(browser.pid());
    let result = drive(&mut browser, html, deadline, timeout);
    browser.stop(if result.is_ok() {
        CLOSE_GRACE
    } else {
        Duration::ZERO
    });
    let result = result.map_err(|reason| chrome_error(chrome, &reason, &browser.stderr_tail()));
    // Chrome must be gone before its profile directory is removed.
    drop(browser);
    drop(dir);
    result
}

/// Where an export's temporary directory goes when `chrome` is a snap (module docs, step 2):
/// the snap has a private `/tmp` that presenter can neither see nor clean, so the profile
/// goes to the snap's `<home>/snap/<name>/common` instead, when that exists. `None` means
/// the system's temporary directory.
fn temp_base(chrome: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let dir = home?.join("snap").join(snap_name(chrome)?).join("common");
    dir.is_dir().then_some(dir)
}

/// The snap a browser belongs to: `/snap/bin/<name>[.<app>]`, `/snap/<name>/…`, or a small
/// wrapper script that runs `/snap/bin/<name>` (like Ubuntu's `chromium-browser`).
fn snap_name(chrome: &Path) -> Option<String> {
    if let Ok(rest) = chrome.strip_prefix("/snap") {
        let mut parts = rest.iter().map(OsStr::to_str);
        let first = parts.next()??;
        let name = match (first, parts.next()) {
            ("bin", Some(Some(app))) => app.split('.').next()?,
            ("bin", _) => return None,
            (name, _) => name,
        };
        return Some(name.to_string());
    }
    let mut head = Vec::new();
    std::fs::File::open(chrome)
        .and_then(|file| file.take(4096).read_to_end(&mut head))
        .ok()?;
    if !head.starts_with(b"#!") {
        return None;
    }
    let text = String::from_utf8_lossy(&head);
    let start = text.find("/snap/bin/")? + "/snap/bin/".len();
    let name: String = text[start..]
        .chars()
        .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// The number of pages of a PDF written by Chrome: its `/Type /Page` dictionary entries
/// (Chrome does not compress its object dictionaries). Strings, comments and stream data
/// are skipped, so a title or alt text that mentions `/Type /Page` does not count. 0 when
/// there are none.
pub fn page_count(pdf: &[u8]) -> usize {
    let mut count = 0;
    let mut after_type = false;
    let mut at = 0;
    while at < pdf.len() {
        if is_pdf_space(pdf[at]) {
            at += 1;
            continue;
        }
        let (token, next) = pdf_token(pdf, at);
        count += usize::from(after_type && token == PdfToken::Name(b"Page"));
        after_type = token == PdfToken::Name(b"Type");
        at = next;
    }
    count
}

/// A PDF token, as far as [`page_count`] cares.
#[derive(Debug, PartialEq, Eq)]
enum PdfToken<'a> {
    /// A name, without its `/`.
    Name(&'a [u8]),
    /// Anything else: a number, keyword, delimiter, string, comment or stream.
    Other,
}

/// The token that starts at `pdf[at]` (not white space) and the index after it. Literal
/// strings, comments and the data of a stream are one token each.
fn pdf_token(pdf: &[u8], at: usize) -> (PdfToken<'_>, usize) {
    match pdf[at] {
        b'(' => (PdfToken::Other, skip_pdf_string(pdf, at)),
        b'%' => {
            let end = pdf[at..]
                .iter()
                .position(|&byte| byte == b'\n' || byte == b'\r')
                .map_or(pdf.len(), |len| at + len);
            (PdfToken::Other, end)
        }
        b'/' => {
            let end = pdf_word_end(pdf, at + 1);
            (PdfToken::Name(&pdf[at + 1..end]), end)
        }
        byte if is_pdf_delimiter(byte) => (PdfToken::Other, at + 1),
        _ => {
            let end = pdf_word_end(pdf, at);
            if &pdf[at..end] != b"stream" {
                return (PdfToken::Other, end);
            }
            let data = pdf[end..]
                .windows(9)
                .position(|window| window == b"endstream");
            (PdfToken::Other, data.map_or(pdf.len(), |len| end + len + 9))
        }
    }
}

/// The index after the literal string that starts at `pdf[at]` (`(`): parentheses nest,
/// and a backslash escapes the next byte.
fn skip_pdf_string(pdf: &[u8], at: usize) -> usize {
    let mut depth = 0usize;
    let mut i = at;
    while i < pdf.len() {
        match pdf[i] {
            b'\\' => i += 1,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    pdf.len()
}

/// The end of the run of regular (not white space, not delimiter) bytes from `at`.
fn pdf_word_end(pdf: &[u8], at: usize) -> usize {
    pdf[at..]
        .iter()
        .position(|&byte| is_pdf_space(byte) || is_pdf_delimiter(byte))
        .map_or(pdf.len(), |len| at + len)
}

/// PDF white space: NUL, tab, line feed, form feed, carriage return, space.
fn is_pdf_space(byte: u8) -> bool {
    matches!(byte, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

/// PDF delimiters.
fn is_pdf_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// Steps 2–5 of the module docs on a launched browser. The error is the reason, without
/// the browser's name and stderr ([`chrome_error`] adds those).
fn drive(
    browser: &mut Browser,
    html: &str,
    deadline: Instant,
    timeout: Duration,
) -> std::result::Result<Vec<u8>, String> {
    let mut cdp = Cdp::new(browser, html, deadline);
    match print_page(&mut cdp) {
        Ok(pdf) => {
            cdp.close_browser();
            Ok(pdf)
        }
        Err(failure) => {
            let mut reason = failure.describe(timeout);
            if !cdp.page_errors.is_empty() {
                let _ = write!(reason, "\n  page errors:");
                for error in &cdp.page_errors {
                    let _ = write!(reason, "\n    {error}");
                }
            }
            Err(reason)
        }
    }
}

/// The error for a failed print: the reason, the browser and Chrome's last stderr lines.
fn chrome_error(chrome: &Path, reason: &str, stderr: &[String]) -> Error {
    let mut message = format!(
        "Chrome failed to print the deck: {reason}\n  browser: {}",
        chrome.display()
    );
    if stderr.is_empty() {
        message.push_str("\n  Chrome wrote nothing on stderr");
    } else {
        let _ = write!(
            message,
            "\n  Chrome's stderr (last {} lines):",
            stderr.len()
        );
        for line in stderr {
            let _ = write!(message, "\n    {line}");
        }
    }
    Error::Other(message)
}

/// Steps 2–4 of the module docs: open a page, load the deck in print view, wait for
/// reveal.js and print.
fn print_page(cdp: &mut Cdp) -> std::result::Result<Vec<u8>, Failure> {
    cdp.stage = "starting Chrome";
    let target = cdp.call(None, "Target.createTarget", json!({ "url": "about:blank" }))?;
    cdp.stage = "opening a page";
    let target_id = string_field(&target, "targetId")?;
    let attach = json!({ "targetId": target_id, "flatten": true });
    let session = string_field(
        &cdp.call(None, "Target.attachToTarget", attach)?,
        "sessionId",
    )?;
    let page = Some(session.as_str());
    let patterns = json!({ "patterns": [{ "urlPattern": DECK_ORIGIN_PATTERN }] });
    cdp.call(page, "Fetch.enable", patterns)?;
    for domain in ["Page.enable", "Runtime.enable", "Inspector.enable"] {
        cdp.call(page, domain, json!({}))?;
    }

    cdp.stage = "loading the deck";
    cdp.loaded = false;
    let navigation = cdp.call(page, "Page.navigate", json!({ "url": DECK_URL }))?;
    if let Some(error) = navigation.get("errorText").and_then(Value::as_str) {
        return Err(Failure::Other(format!("cannot load the deck: {error}")));
    }
    while !cdp.loaded {
        let message = cdp.next_message()?;
        cdp.handle_event(&message)?;
    }

    cdp.stage = "waiting for reveal.js to lay out the pages";
    wait_until_ready(cdp, page)?;

    cdp.stage = "printing";
    let params = json!({
        "preferCSSPageSize": true,
        "printBackground": true,
        "marginTop": 0, "marginBottom": 0, "marginLeft": 0, "marginRight": 0,
        "transferMode": "ReturnAsStream",
    });
    let printed = cdp.call(page, "Page.printToPDF", params)?;
    let pdf = read_stream(cdp, page, &string_field(&printed, "stream")?)?;
    if !pdf.starts_with(b"%PDF-") {
        return Err(Failure::Other(format!(
            "Page.printToPDF returned {} bytes that are not a PDF",
            pdf.len()
        )));
    }
    Ok(pdf)
}

/// Evaluates [`READY_SCRIPT`] in the page and checks its result.
fn wait_until_ready(cdp: &mut Cdp, page: Option<&str>) -> std::result::Result<(), Failure> {
    let params = json!({
        "expression": READY_SCRIPT,
        "awaitPromise": true,
        "returnByValue": true,
    });
    let evaluated = cdp.call(page, "Runtime.evaluate", params)?;
    if let Some(details) = evaluated.get("exceptionDetails") {
        return Err(Failure::Other(format!(
            "the readiness check threw: {}",
            exception_text(details)
        )));
    }
    let value = &evaluated["result"]["value"];
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Err(Failure::Other(error.to_string()));
    }
    tracing::debug!(
        slides = %value["slides"],
        pages = %value["pages"],
        "reveal.js print layout ready"
    );
    Ok(())
}

/// Reads the `IO` stream `handle` to its end and closes it.
fn read_stream(
    cdp: &mut Cdp,
    page: Option<&str>,
    handle: &str,
) -> std::result::Result<Vec<u8>, Failure> {
    let mut bytes = Vec::new();
    loop {
        let params = json!({ "handle": handle, "size": READ_CHUNK });
        let chunk = cdp.call(page, "IO.read", params)?;
        let data = chunk.get("data").and_then(Value::as_str).unwrap_or("");
        if chunk.get("base64Encoded").and_then(Value::as_bool) == Some(true) {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|e| Failure::Other(format!("IO.read returned invalid base64: {e}")))?;
            bytes.extend_from_slice(&decoded);
        } else {
            bytes.extend_from_slice(data.as_bytes());
        }
        if chunk.get("eof").and_then(Value::as_bool) != Some(false) {
            break;
        }
    }
    cdp.call(page, "IO.close", json!({ "handle": handle }))?;
    Ok(bytes)
}

/// The string `field` of a DevTools result.
fn string_field(result: &Value, field: &str) -> std::result::Result<String, Failure> {
    result
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| Failure::Other(format!("DevTools reply without {field}: {result}")))
}

/// A `Runtime.ExceptionDetails` as one line.
fn exception_text(details: &Value) -> String {
    let text = details["exception"]["description"]
        .as_str()
        .or_else(|| details["text"].as_str())
        .unwrap_or("unknown error");
    text.lines().next().unwrap_or(text).to_string()
}

/// The time left until `deadline`; `None` when it has passed.
fn remaining(deadline: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
}

/// Why driving Chrome failed.
#[derive(Debug)]
enum Failure {
    /// The deadline passed during this stage.
    Timeout(&'static str),
    /// Anything else.
    Other(String),
}

impl Failure {
    /// The reason as shown to the user.
    fn describe(self, timeout: Duration) -> String {
        match self {
            Failure::Timeout(stage) => {
                format!("timed out after {} s while {stage}", timeout.as_secs())
            }
            Failure::Other(message) => message,
        }
    }
}

/// A DevTools protocol connection to a launched browser (its pipes, flat sessions).
struct Cdp<'a> {
    /// The browser.
    browser: &'a mut Browser,
    /// The deck, served for [`DECK_URL`].
    html: &'a str,
    /// When everything must be done.
    deadline: Instant,
    /// The last request id used.
    next_id: u64,
    /// What is going on, for error messages.
    stage: &'static str,
    /// Whether `Page.loadEventFired` arrived since `loaded` was last reset.
    loaded: bool,
    /// The page's uncaught exceptions, `console.error` messages and dismissed dialogs.
    page_errors: Vec<String>,
}

impl<'a> Cdp<'a> {
    /// A connection to `browser` that serves `html` and must be done by `deadline`.
    fn new(browser: &'a mut Browser, html: &'a str, deadline: Instant) -> Cdp<'a> {
        Cdp {
            browser,
            html,
            deadline,
            next_id: 0,
            stage: "starting Chrome",
            loaded: false,
            page_errors: Vec::new(),
        }
    }

    /// Sends `method` (to the page `session`, or to the browser) and returns its result,
    /// handling the events that arrive meanwhile.
    fn call(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> std::result::Result<Value, Failure> {
        let id = self.send(session, method, params)?;
        loop {
            let mut message = self.next_message()?;
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                self.handle_event(&message)?;
                continue;
            }
            if let Some(error) = message.get("error") {
                let text = error.get("message").and_then(Value::as_str).unwrap_or("");
                return Err(Failure::Other(format!("{method}: {text} ({error})")));
            }
            return Ok(message["result"].take());
        }
    }

    /// Sends `method` without waiting for its reply and returns its request id.
    fn send(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> std::result::Result<u64, Failure> {
        self.next_id += 1;
        let mut request = json!({ "id": self.next_id, "method": method, "params": params });
        if let Some(session) = session {
            request["sessionId"] = Value::from(session);
        }
        tracing::trace!("CDP -> {method}");
        let bytes = request.to_string().into_bytes();
        let commands = self.browser.commands.as_ref();
        if commands.is_some_and(|commands| commands.send(bytes).is_ok()) {
            Ok(self.next_id)
        } else {
            Err(self.gone())
        }
    }

    /// The next message from Chrome, waiting at most until the deadline and failing as soon
    /// as Chrome has exited.
    fn next_message(&mut self) -> std::result::Result<Value, Failure> {
        loop {
            let left = remaining(self.deadline).ok_or(Failure::Timeout(self.stage))?;
            match self.browser.replies.recv_timeout(left.min(POLL)) {
                Ok(bytes) => {
                    return serde_json::from_slice(&bytes).map_err(|e| {
                        let text = String::from_utf8_lossy(&bytes);
                        let shown: String = text.chars().take(SHOWN_MESSAGE).collect();
                        Failure::Other(format!("invalid DevTools message ({e}): {shown}"))
                    });
                }
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(status) = self.browser.try_wait() {
                        return Err(self.exited(status));
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return Err(self.gone()),
            }
        }
    }

    /// The failure when Chrome exited with `status`.
    fn exited(&self, status: ExitStatus) -> Failure {
        Failure::Other(format!("Chrome exited ({status}) while {}", self.stage))
    }

    /// The failure when Chrome's pipe broke: Chrome exited, or closed it.
    fn gone(&mut self) -> Failure {
        match self.browser.wait_exit(Duration::from_secs(1)) {
            Some(status) => self.exited(status),
            None => Failure::Other(format!(
                "Chrome closed its DevTools pipe while {}",
                self.stage
            )),
        }
    }

    /// Handles an event (or a reply nobody waits for): records what presenter cares about,
    /// serves the deck, dismisses dialogs; a crashed or closed page is an error.
    fn handle_event(&mut self, message: &Value) -> std::result::Result<(), Failure> {
        let params = &message["params"];
        let session = message.get("sessionId").and_then(Value::as_str);
        match message.get("method").and_then(Value::as_str) {
            None => {
                if let Some(error) = message.get("error") {
                    tracing::debug!("DevTools request {} failed: {error}", message["id"]);
                }
            }
            Some("Page.loadEventFired") => self.loaded = true,
            Some("Fetch.requestPaused") => self.serve(session, params)?,
            Some("Page.javascriptDialogOpening") => self.dismiss_dialog(session, params)?,
            Some("Runtime.exceptionThrown") => {
                let text = exception_text(&params["exceptionDetails"]);
                tracing::debug!("page exception: {text}");
                self.page_errors.push(text);
            }
            Some("Runtime.consoleAPICalled") => {
                let kind = params["type"].as_str().unwrap_or("log");
                let text = console_text(params);
                tracing::debug!("page console.{kind}: {text}");
                if kind == "error" {
                    self.page_errors.push(format!("console.error: {text}"));
                }
            }
            Some("Inspector.targetCrashed" | "Target.targetCrashed") => {
                return Err(Failure::Other(format!(
                    "the page crashed while {}",
                    self.stage
                )));
            }
            Some("Target.detachedFromTarget") => {
                return Err(Failure::Other(format!(
                    "the page was closed while {}",
                    self.stage
                )));
            }
            _ => {}
        }
        Ok(())
    }

    /// Answers a paused request for `http://presenter.localhost/…`: the deck for
    /// [`DECK_URL`], 404 for anything else.
    fn serve(&mut self, session: Option<&str>, params: &Value) -> std::result::Result<(), Failure> {
        let url = params["request"]["url"].as_str().unwrap_or("");
        let (code, body) = if url == DECK_URL {
            let body = base64::engine::general_purpose::STANDARD.encode(self.html);
            (200, body)
        } else {
            tracing::debug!("the deck requested {url}: 404");
            (404, String::new())
        };
        let response = json!({
            "requestId": params["requestId"],
            "responseCode": code,
            "responseHeaders": [{ "name": "Content-Type", "value": "text/html; charset=utf-8" }],
            "body": body,
        });
        self.send(session, "Fetch.fulfillRequest", response)
            .map(drop)
    }

    /// Dismisses a JavaScript dialog (it would block the page) and records it.
    fn dismiss_dialog(
        &mut self,
        session: Option<&str>,
        params: &Value,
    ) -> std::result::Result<(), Failure> {
        let kind = params["type"].as_str().unwrap_or("dialog");
        let text = params["message"].as_str().unwrap_or("");
        tracing::warn!("the deck opened a JavaScript {kind} ({text:?}); dismissed it");
        self.page_errors
            .push(format!("dismissed a JavaScript {kind}: {text}"));
        let params = json!({ "accept": false });
        self.send(session, "Page.handleJavaScriptDialog", params)
            .map(drop)
    }

    /// Asks the browser to exit (best effort; [`Browser::stop`] makes sure).
    fn close_browser(&mut self) {
        if let Err(failure) = self.send(None, "Browser.close", json!({})) {
            tracing::debug!("Browser.close: {failure:?}");
        }
    }
}

/// The arguments of a `Runtime.consoleAPICalled` event, space-separated.
fn console_text(params: &Value) -> String {
    let args = params["args"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    args.iter()
        .map(|arg| match (&arg["value"], arg["description"].as_str()) {
            (Value::String(text), _) => text.clone(),
            (Value::Null, Some(description)) => description.to_string(),
            (value, _) => value.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A running Chrome and the threads that talk to it. Dropping it kills Chrome if it is
/// still running.
struct Browser {
    /// The Chrome process.
    child: Child,
    /// Messages for Chrome, written to its descriptor 3 by a writer thread. `None` once
    /// closed; the writer then closes the pipe, which makes Chrome exit.
    commands: Option<Sender<Vec<u8>>>,
    /// Chrome's messages from its descriptor 4, read by a reader thread; disconnected when
    /// the pipe closes.
    replies: Receiver<Vec<u8>>,
    /// Its last [`STDERR_TAIL`] stderr lines, filled by a reader thread.
    stderr: Arc<Mutex<VecDeque<String>>>,
    /// Disconnected when that thread reaches the end of Chrome's stderr.
    stderr_closed: Receiver<()>,
}

impl Browser {
    /// Starts `chrome` (module docs, step 1) with its profile in `profile`, and the threads
    /// for its pipes.
    fn launch(chrome: &Path, profile: &Path) -> Result<Browser> {
        let mut user_data_dir = OsString::from("--user-data-dir=");
        user_data_dir.push(profile);
        tracing::debug!("starting {} {}", chrome.display(), CHROME_FLAGS.join(" "));
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(LAUNCH_SCRIPT)
            .arg(chrome)
            .args(CHROME_FLAGS);
        if running_as_root() {
            // Chrome refuses to start as root (containers, CI) with its sandbox on.
            command.arg("--no-sandbox");
        }
        let mut child = command
            .arg(user_data_dir)
            .arg("about:blank")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Other(format!("cannot start {}: {e}", chrome.display())))?;
        let pipes = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let (commands, to_chrome) = mpsc::channel();
        let (from_chrome, replies) = mpsc::channel();
        let (stderr_open, stderr_closed) = mpsc::channel::<()>();
        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        let log = Arc::clone(&stderr);
        // From here on, an early return drops `browser`, which kills Chrome.
        let browser = Browser {
            child,
            commands: Some(commands),
            replies,
            stderr,
            stderr_closed,
        };
        let (Some(input), Some(output), Some(errors)) = pipes else {
            return Err(Error::Other("Chrome was started without its pipes".into()));
        };
        spawn_thread("chrome-commands", move || write_messages(input, &to_chrome))?;
        spawn_thread("chrome-replies", move || {
            read_messages(output, &from_chrome)
        })?;
        spawn_thread("chrome-stderr", move || {
            read_stderr(errors, &log);
            drop(stderr_open);
        })?;
        Ok(browser)
    }

    /// Chrome's process id.
    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Chrome's exit status, if it has exited.
    fn try_wait(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Chrome's last stderr lines, after giving the last ones [`STDERR_DRAIN`] to arrive.
    fn stderr_tail(&self) -> Vec<String> {
        let _ = self.stderr_closed.recv_timeout(STDERR_DRAIN);
        let lines = self.stderr.lock().unwrap_or_else(PoisonError::into_inner);
        lines.iter().cloned().collect()
    }

    /// Waits up to `grace` for Chrome to exit and returns its status.
    fn wait_exit(&mut self, grace: Duration) -> Option<ExitStatus> {
        let until = Instant::now() + grace;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) if Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => return None,
            }
        }
    }

    /// Closes Chrome's command pipe, gives Chrome `grace` to exit, then kills it, and reaps
    /// it.
    fn stop(&mut self, grace: Duration) {
        self.commands = None;
        if self.wait_exit(grace).is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.stop(Duration::ZERO);
    }
}

/// Starts a named thread.
fn spawn_thread(name: &str, body: impl FnOnce() + Send + 'static) -> Result<()> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(body)
        .map(drop)
        .map_err(|e| Error::Other(format!("cannot start a thread to talk to Chrome: {e}")))
}

/// The body of the writer thread: writes each message to Chrome's command pipe with its
/// NUL terminator, until the channel closes (then the pipe is closed) or a write fails.
fn write_messages(mut input: ChildStdin, messages: &Receiver<Vec<u8>>) {
    for mut message in messages {
        message.push(0);
        if let Err(e) = input.write_all(&message).and_then(|()| input.flush()) {
            tracing::debug!("cannot write to Chrome's DevTools pipe: {e}");
            return;
        }
    }
}

/// The body of the reader thread: sends each NUL-terminated message from Chrome to
/// `messages`, until the pipe closes or nobody listens any more.
fn read_messages(output: ChildStdout, messages: &Sender<Vec<u8>>) {
    let mut reader = BufReader::new(output);
    loop {
        let mut message = Vec::new();
        match reader.read_until(0, &mut message) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if message.last() == Some(&0) {
            message.pop();
        }
        if messages.send(message).is_err() {
            return;
        }
    }
}

/// The body of the stderr reader thread: keeps the last [`STDERR_TAIL`] lines in `log`.
fn read_stderr(pipe: ChildStderr, log: &Mutex<VecDeque<String>>) {
    let mut reader = BufReader::new(pipe);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&line).trim_end().to_string();
        if text.is_empty() {
            continue;
        }
        tracing::trace!("chrome: {text}");
        let mut lines = log.lock().unwrap_or_else(PoisonError::into_inner);
        if lines.len() == STDERR_TAIL {
            lines.pop_front();
        }
        lines.push_back(text);
    }
}

/// True when this process runs as root on Linux (effective uid 0 in `/proc/self/status`),
/// where Chrome needs `--no-sandbox`.
fn running_as_root() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    std::fs::read_to_string("/proc/self/status").is_ok_and(|status| status_is_root(&status))
}

/// Whether a `/proc/<pid>/status` text has effective uid 0 (`Uid: real effective …`).
fn status_is_root(status: &str) -> bool {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|uids| uids.split_whitespace().nth(1))
        == Some("0")
}

/// A private temporary directory, removed (with its contents) on drop, and the watchdog
/// that removes it if presenter dies first (module docs, cleanup).
struct TempDir {
    /// The directory.
    path: PathBuf,
    /// Its watchdog; dropped (told `done`) after the directory is removed.
    janitor: Janitor,
}

impl TempDir {
    /// Creates `<base>/<prefix>-<pid>-<n>` (mode 0700 on Unix) and starts its watchdog.
    fn new(base: &Path, prefix: &str) -> Result<TempDir> {
        let path = create_private_dir(base, prefix)?;
        match Janitor::start(&path) {
            Ok(janitor) => Ok(TempDir { path, janitor }),
            Err(e) => {
                remove_dir_patiently(&path);
                Err(Error::Other(format!(
                    "cannot start the cleanup watchdog (/bin/sh): {e}"
                )))
            }
        }
    }

    /// The directory.
    fn path(&self) -> &Path {
        &self.path
    }

    /// Tells the watchdog which Chrome process uses the directory.
    fn watch(&mut self, chrome: u32) {
        self.janitor.tell(&format!("chrome {chrome}"));
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        remove_dir_patiently(&self.path);
    }
}

/// Creates `<base>/<prefix>-<pid>-<n>` with mode 0700 (on Unix), for the first free `n`.
fn create_private_dir(base: &Path, prefix: &str) -> Result<PathBuf> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    let mut attempts = 0;
    loop {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = base.join(format!("{prefix}-{pid}-{n}"));
        attempts += 1;
        match builder.create(&path) {
            Ok(()) => return Ok(path),
            // Left over by an earlier process with the same id.
            Err(e) if e.kind() == ErrorKind::AlreadyExists && attempts < 1_000 => {}
            Err(e) => {
                return Err(Error::Other(format!(
                    "cannot create a temporary directory in {}: {e}",
                    base.display()
                )));
            }
        }
    }
}

/// Removes `path` and its contents, retrying for up to 1 s: Chrome's helper processes may
/// still be finishing writes into the profile.
fn remove_dir_patiently(path: &Path) {
    for attempt in 0..20 {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return,
            Err(e) if e.kind() == ErrorKind::NotFound => return,
            Err(e) if attempt == 19 => {
                tracing::warn!("cannot remove {}: {e}", path.display());
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// The watchdog process of one export (module docs, cleanup; `JANITOR_SCRIPT`). Dropping it
/// tells it `done` and reaps it.
struct Janitor {
    /// The `/bin/sh` process.
    child: Child,
    /// Its stdin; closing it without `done` (presenter died) starts the cleanup.
    input: Option<ChildStdin>,
}

impl Janitor {
    /// Starts the watchdog for the temporary directory `dir`, in its own process group (so
    /// that a Ctrl-C in the terminal does not reach it).
    fn start(dir: &Path) -> std::io::Result<Janitor> {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(JANITOR_SCRIPT)
            .arg("presenter-pdf-janitor")
            .arg(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn()?;
        let input = child.stdin.take();
        Ok(Janitor { child, input })
    }

    /// Sends the watchdog one line (best effort).
    fn tell(&mut self, line: &str) {
        if let Some(input) = &mut self.input
            && let Err(e) = writeln!(input, "{line}")
        {
            tracing::debug!("cannot talk to the cleanup watchdog: {e}");
        }
    }

    /// Closes the watchdog's stdin without `done`, as presenter's death would.
    #[cfg(test)]
    fn abandon(&mut self) {
        self.input = None;
    }
}

impl Drop for Janitor {
    fn drop(&mut self) {
        self.tell("done");
        self.input = None;
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    /// Writes an executable shell script `name` with `body` into `dir`.
    #[cfg(unix)]
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// A fake environment for [`find_chrome_in`].
    fn env(vars: &[(&str, &OsString)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let vars: BTreeMap<String, OsString> = vars
            .iter()
            .map(|(name, value)| (name.to_string(), (*value).clone()))
            .collect();
        move |name| vars.get(name).cloned()
    }

    /// Whether process `pid` exists (`kill -0`).
    #[cfg(unix)]
    fn alive(pid: &str) -> bool {
        Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    /// A long-running process whose command line names `dir`, like Chrome's
    /// `--user-data-dir=<dir>/profile`.
    #[cfg(unix)]
    fn fake_browser_process(dir: &Path) -> Child {
        Command::new("/bin/sh")
            .args(["-c", "while :; do sleep 1; done", "fake-chrome"])
            .arg(format!("--user-data-dir={}", dir.join("profile").display()))
            .spawn()
            .unwrap()
    }

    /// Serializes the tests that start processes. On macOS a pipe gets its close-on-exec
    /// flag only after it is created, so a process started meanwhile by another test thread
    /// can inherit it, and hold it open for as long as that process (or a `sleep` it left
    /// behind) runs: an exit or a closed pipe would then go unnoticed.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static PROCESSES: Mutex<()> = Mutex::new(());
        PROCESSES.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The names in `dir`.
    fn entries(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn explicit_paths_win_and_must_be_executable() {
        let dir = tempfile::tempdir().unwrap();
        let fake = script(dir.path(), "my-chrome", "exit 0");
        let other = script(dir.path(), "other-chrome", "exit 0");
        let chrome_path = other.clone().into_os_string();
        let vars = env(&[("CHROME_PATH", &chrome_path)]);
        let bundles = [other.clone()];
        assert_eq!(find_chrome_in(Some(&fake), &vars, &bundles), Some(fake));
        // An explicit path that is not an executable is not replaced by another browser.
        let missing = dir.path().join("missing");
        assert_eq!(find_chrome_in(Some(&missing), &vars, &bundles), None);
        let plain = dir.path().join("plain.txt");
        std::fs::write(&plain, "not a program").unwrap();
        assert_eq!(find_chrome_in(Some(&plain), &vars, &bundles), None);
        assert_eq!(find_chrome_in(Some(dir.path()), &vars, &bundles), None);
    }

    #[cfg(unix)]
    #[test]
    fn chrome_path_is_used_before_well_known_locations() {
        let dir = tempfile::tempdir().unwrap();
        let fake = script(dir.path(), "fake-chrome", "exit 0");
        let bundle = script(dir.path(), "bundle-chrome", "exit 0");
        let on_path = script(dir.path(), "chromium", "exit 0");
        let chrome_path = fake.clone().into_os_string();
        let path_var = dir.path().as_os_str().to_os_string();
        let bundles = [dir.path().join("absent"), bundle.clone()];

        let vars = env(&[("CHROME_PATH", &chrome_path), ("PATH", &path_var)]);
        assert_eq!(find_chrome_in(None, &vars, &bundles), Some(fake));
        // Without CHROME_PATH: the first existing bundle, then PATH.
        let vars = env(&[("PATH", &path_var)]);
        assert_eq!(find_chrome_in(None, &vars, &bundles), Some(bundle));
        assert_eq!(find_chrome_in(None, &vars, &[]), Some(on_path.clone()));
        // An empty or broken CHROME_PATH is skipped.
        let empty = OsString::new();
        let vars = env(&[("CHROME_PATH", &empty), ("PATH", &path_var)]);
        assert_eq!(find_chrome_in(None, &vars, &[]), Some(on_path.clone()));
        let broken = dir.path().join("nope").into_os_string();
        let vars = env(&[("CHROME_PATH", &broken), ("PATH", &path_var)]);
        assert_eq!(find_chrome_in(None, &vars, &[]), Some(on_path));
        // Nothing anywhere.
        assert_eq!(find_chrome_in(None, &env(&[]), &[]), None);
    }

    #[cfg(unix)]
    #[test]
    fn path_lookup_follows_the_documented_order() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        script(first.path(), "chromium-browser", "exit 0");
        let stable = script(second.path(), "google-chrome-stable", "exit 0");
        let path_var = std::env::join_paths([first.path(), second.path()]).unwrap();
        let vars = env(&[("PATH", &path_var)]);
        // google-chrome-stable comes before chromium-browser in PATH_NAMES.
        assert_eq!(find_chrome_in(None, &vars, &[]), Some(stable.clone()));
        // A bare --chrome name is looked up on PATH; a CHROME_PATH name too.
        let name = Path::new("chromium-browser");
        assert_eq!(
            find_chrome_in(Some(name), &vars, &[]),
            Some(first.path().join("chromium-browser"))
        );
        let chrome_path = OsString::from("google-chrome-stable");
        let vars = env(&[("PATH", &path_var), ("CHROME_PATH", &chrome_path)]);
        assert_eq!(find_chrome_in(None, &vars, &[]), Some(stable));
    }

    #[cfg(unix)]
    #[test]
    fn app_bundles_stand_for_their_executable() {
        let dir = tempfile::tempdir().unwrap();
        let macos = dir.path().join("Chromium.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        let exe = script(&macos, "Chromium", "exit 0");
        let bundle = dir.path().join("Chromium.app");
        assert_eq!(find_chrome_in(Some(&bundle), &env(&[]), &[]), Some(exe));
    }

    #[test]
    fn bare_names_have_no_directory_part() {
        assert!(is_bare_name(Path::new("chromium")));
        assert!(!is_bare_name(Path::new("./chromium")));
        assert!(!is_bare_name(Path::new("bin/chromium")));
        assert!(!is_bare_name(Path::new("/usr/bin/chromium")));
    }

    #[test]
    fn mac_bundles_list_every_app_in_both_application_folders() {
        let bundles = mac_bundles(Some(Path::new("/Users/me")));
        assert_eq!(bundles.len(), 2 * MAC_APPS.len());
        assert_eq!(
            bundles[0],
            Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
        );
        assert_eq!(
            bundles[1],
            Path::new("/Users/me/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
        );
        assert!(bundles.contains(&PathBuf::from(
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"
        )));
        assert_eq!(mac_bundles(None).len(), MAC_APPS.len());
    }

    #[test]
    fn root_is_read_from_the_effective_uid() {
        assert!(status_is_root(
            "Name:\tx\nUid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\n"
        ));
        assert!(status_is_root("Uid:\t1000\t0\t0\t0\n"));
        assert!(!status_is_root("Uid:\t1000\t1000\t1000\t1000\n"));
        assert!(!status_is_root("Name:\tx\n"));
        if !cfg!(target_os = "linux") {
            assert!(!running_as_root());
        }
    }

    #[cfg(unix)]
    #[test]
    fn snap_browsers_keep_their_profile_where_presenter_can_remove_it() {
        assert_eq!(
            snap_name(Path::new("/snap/bin/chromium")).as_deref(),
            Some("chromium")
        );
        assert_eq!(
            snap_name(Path::new("/snap/bin/chromium.chromedriver")).as_deref(),
            Some("chromium")
        );
        assert_eq!(
            snap_name(Path::new("/snap/brave/123/opt/brave.com/brave/brave")).as_deref(),
            Some("brave")
        );
        assert_eq!(snap_name(Path::new("/snap/bin")), None);
        assert_eq!(snap_name(Path::new("/usr/bin/nonexistent-browser")), None);

        // Ubuntu's chromium-browser is a script that runs the snap.
        let dir = tempfile::tempdir().unwrap();
        let wrapper = script(
            dir.path(),
            "chromium-browser",
            "if ! [ -x /snap/bin/chromium ]; then exit 1; fi\nexec /snap/bin/chromium \"$@\"",
        );
        assert_eq!(snap_name(&wrapper).as_deref(), Some("chromium"));
        let plain = script(
            dir.path(),
            "google-chrome",
            "exec /opt/google/chrome/chrome",
        );
        assert_eq!(snap_name(&plain), None);

        // The snap's common directory, when it exists; else the system's temp directory.
        let home = tempfile::tempdir().unwrap();
        let snap = Path::new("/snap/bin/chromium");
        assert_eq!(temp_base(snap, Some(home.path())), None);
        let common = home.path().join("snap/chromium/common");
        std::fs::create_dir_all(&common).unwrap();
        assert_eq!(temp_base(snap, Some(home.path())), Some(common));
        assert_eq!(temp_base(snap, None), None);
        assert_eq!(temp_base(&plain, Some(home.path())), None);
    }

    #[test]
    fn pages_are_counted_without_the_page_tree() {
        let pdf = b"%PDF-1.4\n1 0 obj\n<</Type /Pages /Count 2>>\n2 0 obj\n<</Type /Page>>\n\
                    3 0 obj\n<</Type/Page/Parent 1 0 R>>\n4 0 obj\n<</Type /PageLabel>>\n";
        assert_eq!(page_count(pdf), 2);
        assert_eq!(page_count(b"%PDF-1.4"), 0);
        assert_eq!(page_count(b""), 0);
    }

    #[test]
    fn strings_comments_and_streams_do_not_count_as_pages() {
        let pdf = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3 /Type /Page\n\
            1 0 obj\n<</Title (Inside <</Type /Page>> dictionaries (nested \\) /Type /Page))\n\
            /Producer (Skia/PDF m120)>>\nendobj\n\
            2 0 obj\n<</S /Figure /Alt (/Type /Page alt)>>\nendobj\n\
            3 0 obj\n<</Length 24>>\nstream\n\x00\xff/Type /Page junk ) (\nendstream\nendobj\n\
            4 0 obj\n<</Type /Page /Contents 3 0 R /Annots [<</Type /Annot>>]>>\nendobj\n\
            5 0 obj\n<</Type\n/Page>>\nendobj\n<4142> % hex string, then a comment /Type /Page\n";
        assert_eq!(page_count(pdf), 2);
        // An unterminated string or stream ends the scan.
        assert_eq!(page_count(b"<</Type /Page>> (open /Type /Page"), 1);
        assert_eq!(page_count(b"<</Type /Page>> stream\n/Type /Page"), 1);
    }

    #[cfg(unix)]
    #[test]
    fn temporary_directories_are_private_and_removed() {
        let _serial = serial();
        let base = tempfile::tempdir().unwrap();
        let dir = TempDir::new(base.path(), "presenter-test").unwrap();
        let path = dir.path().to_path_buf();
        assert!(path.starts_with(base.path()));
        std::fs::create_dir_all(path.join("profile/nested")).unwrap();
        std::fs::write(path.join("profile/nested/file"), "x").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        let other = TempDir::new(base.path(), "presenter-test").unwrap();
        assert_ne!(other.path(), path);
        let started = Instant::now();
        drop(dir);
        drop(other);
        assert!(entries(base.path()).is_empty());
        // The watchdogs were told `done` and exited at once.
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn the_watchdog_stops_chrome_and_removes_the_directory_when_presenter_is_gone() {
        let _serial = serial();
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("presenter-pdf-1-0");
        std::fs::create_dir_all(dir.join("profile/Default")).unwrap();
        std::fs::write(dir.join("profile/Default/Cookies"), "x").unwrap();
        // A "Chrome" that uses the directory and ignores its pipe closing.
        let mut chrome = fake_browser_process(&dir);
        let mut janitor = Janitor::start(&dir).unwrap();
        janitor.tell(&format!("chrome {}", chrome.id()));
        let started = Instant::now();
        janitor.abandon();
        // Chrome is presenter's child here, so reap it as soon as the watchdog kills it.
        let status = chrome.wait().unwrap();
        assert!(!status.success());
        drop(janitor);
        assert!(!dir.exists(), "{:?}", entries(base.path()));
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_secs(3) && elapsed < Duration::from_secs(15),
            "{elapsed:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_watchdog_never_kills_a_process_that_does_not_use_the_directory() {
        let _serial = serial();
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("presenter-pdf-1-0");
        std::fs::create_dir_all(dir.join("profile")).unwrap();
        // Chrome exited and its process id now belongs to an unrelated process.
        let mut other = Command::new("sleep").arg("60").spawn().unwrap();
        let mut janitor = Janitor::start(&dir).unwrap();
        janitor.tell(&format!("chrome {}", other.id()));
        let started = Instant::now();
        janitor.abandon();
        drop(janitor);
        assert!(!dir.exists(), "{:?}", entries(base.path()));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(alive(&other.id().to_string()));
        other.kill().unwrap();
        other.wait().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_watchdog_leaves_everything_alone_once_presenter_is_done() {
        let _serial = serial();
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("kept");
        std::fs::create_dir(&dir).unwrap();
        let mut chrome = fake_browser_process(&dir);
        let mut janitor = Janitor::start(&dir).unwrap();
        janitor.tell(&format!("chrome {}", chrome.id()));
        drop(janitor);
        assert!(dir.exists());
        assert!(alive(&chrome.id().to_string()));
        chrome.kill().unwrap();
        chrome.wait().unwrap();
    }

    /// [`print_in`] with a fake Chrome and a private temporary base, which must be empty
    /// afterwards; retries the rare `Text file busy` of executing a script that was just
    /// written while another test thread forks.
    #[cfg(unix)]
    fn print_with(chrome: &Path, timeout: Duration) -> (String, Duration) {
        let base = tempfile::tempdir().unwrap();
        for _ in 0..5 {
            let started = Instant::now();
            let err = print_in(chrome, "<p>deck</p>", timeout, base.path()).unwrap_err();
            let elapsed = started.elapsed();
            assert!(
                entries(base.path()).is_empty(),
                "{:?}",
                entries(base.path())
            );
            let message = err.to_string();
            if !message.contains("Text file busy") {
                return (message, elapsed);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("{} stayed busy", chrome.display());
    }

    #[cfg(unix)]
    #[test]
    fn a_chrome_that_exits_reports_its_stderr() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let fake = script(
            dir.path(),
            "chrome",
            "echo 'fake chrome: no display' >&2\necho 'second line' >&2\nexit 7",
        );
        let (message, _) = print_with(&fake, Duration::from_secs(20));
        assert!(
            message.starts_with(
                "Chrome failed to print the deck: Chrome exited (exit status: 7) while \
                 starting Chrome\n"
            ),
            "{message}"
        );
        assert!(
            message.contains(&format!("browser: {}", fake.display())),
            "{message}"
        );
        assert!(
            message.contains(
                "Chrome's stderr (last 2 lines):\n    fake chrome: no display\n    second line"
            ),
            "{message}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_exit_is_noticed_while_a_descendant_keeps_the_pipes_open() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let fake = script(
            dir.path(),
            "chrome",
            "(sleep 10 &)\necho 'fatal: cannot start' >&2\nexit 4",
        );
        let (message, elapsed) = print_with(&fake, Duration::from_secs(20));
        assert!(
            message.starts_with(
                "Chrome failed to print the deck: Chrome exited (exit status: 4) while \
                 starting Chrome\n"
            ),
            "{message}"
        );
        assert!(message.contains("    fatal: cannot start"), "{message}");
        assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_silent_chrome_times_out_and_is_killed() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let fake = script(
            dir.path(),
            "chrome",
            &format!("echo $$ > '{}'\nexec sleep 30", pid_file.display()),
        );
        let (message, elapsed) = print_with(&fake, Duration::from_secs(2));
        assert!(
            message.contains("timed out after 2 s while starting Chrome"),
            "{message}"
        );
        assert!(
            message.contains("Chrome wrote nothing on stderr"),
            "{message}"
        );
        assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
        // Without a pid file (a heavily loaded machine), the script was killed and reaped
        // before it ran its first line.
        match std::fs::read_to_string(&pid_file) {
            Ok(pid) => assert!(!alive(pid.trim()), "fake Chrome {pid} still runs"),
            Err(e) => assert_eq!(e.kind(), ErrorKind::NotFound),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_chrome_that_answers_garbage_fails_fast() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let fake = script(
            dir.path(),
            "chrome",
            "printf 'not json\\0' >&4\nexec sleep 30",
        );
        let (message, elapsed) = print_with(&fake, Duration::from_secs(20));
        assert!(
            message.starts_with("Chrome failed to print the deck: invalid DevTools message ("),
            "{message}"
        );
        assert!(message.contains("): not json\n"), "{message}");
        assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_chrome_that_closes_its_pipe_fails_fast() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let fake = script(dir.path(), "chrome", "exec 4>&-\nexec sleep 30");
        let (message, elapsed) = print_with(&fake, Duration::from_secs(20));
        assert!(
            message.starts_with(
                "Chrome failed to print the deck: Chrome closed its DevTools pipe while \
                 starting Chrome\n"
            ),
            "{message}"
        );
        assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    }

    #[test]
    fn a_missing_executable_cannot_be_started() {
        let err = html_to_pdf(
            Path::new("/nonexistent/chrome"),
            "<p>x</p>",
            DEFAULT_TIMEOUT,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "cannot start /nonexistent/chrome: not an executable file"
        );
    }
}
