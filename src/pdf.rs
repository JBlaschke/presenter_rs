//! PDF export through headless Chrome (`presenter export pdf`, `docs/spec/cli.md` export).
//!
//! [`find_chrome`] locates a Chrome or Chromium executable; [`html_to_pdf`] prints a
//! standalone deck with it, in reveal.js' print view (`?print-pdf`), and
//! [`capture::html_to_pages`] converts every page of that view into editable shapes
//! instead, for `presenter export pptx`. macOS and Linux only: Chrome is started through
//! `/bin/sh`.
//!
//! # How a deck is printed
//!
//! Chrome's one-shot `--print-to-pdf` flag cannot wait for reveal.js to lay the pages out
//! (and hangs in some environments), so [`html_to_pdf`] drives Chrome over the DevTools
//! protocol instead, with `--remote-debugging-pipe`: NUL-terminated JSON messages on Chrome's
//! file descriptors 3 (commands) and 4 (replies and events), flat sessions. Chrome opens no
//! network port, so no other local user or process can drive it.
//!
//! 1. A private temporary directory (mode 0700) holds the deck, `deck.html`, and Chrome's
//!    fresh profile: in the system's temporary directory (`$TMPDIR`; an empty one counts as
//!    unset, `/tmp`), or for a snap browser in its `~/snap/<name>/common` when that exists
//!    (a snap has a private `/tmp`, and `~/snap/<name>/common` is its `$SNAP_USER_COMMON`,
//!    which it can read). Once created, the directory is named by its real path (absolute,
//!    no symbolic links, `.` or `..`): a relative `$TMPDIR` must not make the deck's URL
//!    depend on the current directory. Chrome starts headless with a quiet throwaway
//!    configuration (no first-run UI, sync, extensions or keychain access; plus
//!    `--no-sandbox` when running as root on Linux, which Chrome requires); `/bin/sh` moves
//!    presenter's pipes to descriptors 3 and 4 and `exec`s it. Chrome's own temporary files
//!    go into a `tmp` directory there as well (`TMPDIR` in its environment: on Linux, Chrome
//!    leaves some of them behind), so an export leaves nothing in the system's temporary
//!    directory. A served deck's browser keeps its own `TMPDIR`: it cannot read the
//!    directory.
//! 2. `Target.createTarget` (about:blank) and `Target.attachToTarget {flatten: true}` open a
//!    page; `Page.enable`, `Runtime.enable`, `Inspector.enable`; `Page.navigate` to
//!    `file://<dir>/deck.html?print-pdf`, the same kind of page as the output of
//!    `presenter build` opened from disk (so images given by absolute path or `file://` URL
//!    show, and the deck may be as large as Chrome can load). Then the page's
//!    `Page.loadEventFired`. JavaScript dialogs (`alert`, `confirm`, …) are dismissed, with a
//!    warning.
//!
//!    Fallback: a snap browser without `~/snap/<name>/common` (a snap that has never run)
//!    cannot read the system's temporary directory. presenter then serves the deck itself
//!    at [`SERVED_DECK_URL`] (`Fetch.enable` for `http://presenter.localhost/*`; Chrome's
//!    request for the deck is answered with `Fetch.fulfillRequest`, anything else there
//!    with a 404; a `.localhost` page is a secure context like a `file://` page). That has
//!    two limits: images given by absolute path or `file://` URL do not show, and the deck
//!    must fit into one DevTools message (Chrome takes at most 100 MiB, the deck travels
//!    base64-encoded), so a larger deck is refused before Chrome starts. Running the snap
//!    once creates the directory.
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
//! Chrome is noticed within 0.1 s and a closed DevTools pipe (either direction) at once,
//! and no message larger than Chrome accepts is sent. A failure is an [`Error::Other`]
//! that names the browser, says what failed (`Chrome exited (…) while …`, `timed out after
//! 120 s while …`), lists the page's uncaught errors and dismissed dialogs when there were
//! any, and ends with the last lines Chrome wrote on stderr.
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
//! command line still names the directory) and removes the directory. presenter's own
//! signal handling is left alone, so an interrupted export ends the way an interrupted
//! process does.
//!
//! # Other uses
//!
//! [`capture`] runs steps 1–3 and 5 unchanged and, instead of printing, converts each page
//! of the print view into shapes (`presenter export pptx`).
//!
//! With the `browser-tests` feature, `devtools::Page` starts Chrome the same way and opens
//! one page to navigate and evaluate JavaScript in (the headless-browser smoke tests,
//! `tests/browser.rs`, use it). Without that feature the module is not compiled.

pub mod capture;
#[cfg(feature = "browser-tests")]
pub mod devtools;

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, ErrorKind, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, Command, ExitStatus, Stdio};
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

/// Where Chrome loads the deck from when it cannot read the temporary directory (module
/// docs, step 2, fallback). presenter answers every request for this origin itself, so the
/// name is never resolved and nothing is fetched from the network or the loopback
/// interface.
pub const SERVED_DECK_URL: &str = "http://presenter.localhost/deck.html?print-pdf";

/// The requests presenter answers instead of the network (a `Fetch.enable` pattern).
const SERVED_ORIGIN_PATTERN: &str = "http://presenter.localhost/*";

/// The largest DevTools message presenter sends, NUL terminator included. Chrome's pipe
/// handler drops the connection (and stops answering) when one message does not fit into
/// its 100 MiB read buffer; this leaves room for the read chunk it adds.
const MAX_MESSAGE: usize = 96 << 20;

// Chrome's own limit, 100 MiB per message, must stay out of reach.
const _: () = assert!(MAX_MESSAGE < 100 << 20);

/// Bytes of a `Fetch.fulfillRequest` message besides its base64 body (ids and headers;
/// generous).
const FULFILL_OVERHEAD: usize = 4096;

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
/// or the deck in it cannot be written; a deck to be served (module docs, step 2,
/// fallback) is too large; Chrome cannot be started; Chrome exits, times out or the page
/// fails (with the browser's path, the page's errors and Chrome's stderr); not macOS or
/// Linux.
pub fn html_to_pdf(chrome: &Path, html: &str, timeout: Duration) -> Result<Vec<u8>> {
    in_print_view(chrome, html, timeout, "print", print_pdf)
}

/// Opens the standalone deck `html` in reveal.js' print view with `chrome` (module docs,
/// steps 1–3), runs `job` on the ready page (with the page's DevTools session), stops
/// Chrome (step 5) and returns what the job returned. `verb` says what the job does in an
/// error (`Chrome failed to <verb> the deck: …`). Everything, the job included, must finish
/// within `timeout`, unless the job moves the deadline ([`Cdp::deadline`]). The errors are
/// those of [`html_to_pdf`].
fn in_print_view<T>(
    chrome: &Path,
    html: &str,
    timeout: Duration,
    verb: &'static str,
    job: impl FnOnce(&mut Cdp<'_>, &str) -> std::result::Result<T, Failure>,
) -> Result<T> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let (base, source) = placement(chrome, home.as_deref());
    let base = base.unwrap_or_else(|| system_temp_dir(std::env::temp_dir()));
    run_in(chrome, html, timeout, &base, source, verb, job)
}

/// The system's temporary directory, given [`std::env::temp_dir`]: that returns `$TMPDIR`
/// as set, so an empty `TMPDIR=` gives the empty path, which would put the export's
/// directory into the current directory. An empty `$TMPDIR` counts as unset instead
/// (`/tmp`), as for `mktemp`, Python and Go. A relative one stays relative here
/// ([`TempDir::new`] resolves it).
fn system_temp_dir(temp_dir: PathBuf) -> PathBuf {
    if temp_dir.as_os_str().is_empty() {
        PathBuf::from("/tmp")
    } else {
        temp_dir
    }
}

/// How Chrome gets the deck (module docs, step 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeckSource {
    /// `deck.html` in the export's temporary directory, opened as a `file://` page.
    File,
    /// Served from memory at [`SERVED_DECK_URL`], for a browser that cannot read the
    /// temporary directory.
    Served,
}

/// [`in_print_view`] with the temporary directory created in `base` and the deck handed
/// over as `source` says.
fn run_in<T>(
    chrome: &Path,
    html: &str,
    timeout: Duration,
    base: &Path,
    source: DeckSource,
    verb: &'static str,
    job: impl FnOnce(&mut Cdp<'_>, &str) -> std::result::Result<T, Failure>,
) -> Result<T> {
    if cfg!(not(unix)) {
        return Err(Error::Other(
            "the export needs macOS or Linux (Chrome is started through /bin/sh)".into(),
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
    if source == DeckSource::Served {
        check_servable(html.len(), chrome, base)?;
    }
    let mut dir = TempDir::new(base, "presenter-pdf")?;
    let deck = Deck::new(&dir, html, source)?;
    let scratch = chrome_scratch_dir(&dir, source)?;
    tracing::info!("opening {} with {}", deck.url, executable.display());
    let mut browser =
        Browser::launch(&executable, &dir.path().join("profile"), scratch.as_deref())?;
    dir.watch(browser.pid());
    let result = drive(&mut browser, &deck, deadline, timeout, job);
    browser.stop(if result.is_ok() {
        CLOSE_GRACE
    } else {
        Duration::ZERO
    });
    let result =
        result.map_err(|reason| chrome_error(chrome, verb, &reason, &browser.stderr_tail()));
    // Chrome must be gone before its profile directory is removed.
    drop(browser);
    drop(dir);
    result
}

/// Where Chrome's own temporary files go (module docs, step 1): `<dir>/tmp`, created here,
/// when the browser can read `dir` (a `file://` deck); `None` for a served deck, whose
/// browser cannot, so it keeps the system's.
fn chrome_scratch_dir(dir: &TempDir, source: DeckSource) -> Result<Option<PathBuf>> {
    if source != DeckSource::File {
        return Ok(None);
    }
    let scratch = dir.path().join("tmp");
    std::fs::create_dir(&scratch)
        .map_err(|e| Error::Other(format!("cannot create {}: {e}", scratch.display())))?;
    Ok(Some(scratch))
}

/// The deck as Chrome gets it: the URL to open, and the HTML to answer for it when it is
/// served (module docs, step 2).
struct Deck<'a> {
    /// `file://<dir>/deck.html?print-pdf` or [`SERVED_DECK_URL`].
    url: String,
    /// The HTML that presenter serves at [`SERVED_DECK_URL`]; `None` for a `file://` deck.
    served: Option<&'a str>,
}

impl<'a> Deck<'a> {
    /// Hands `html` over as `source` says: writes `deck.html` into `dir`, or keeps it to
    /// be served.
    fn new(dir: &TempDir, html: &'a str, source: DeckSource) -> Result<Deck<'a>> {
        match source {
            DeckSource::File => {
                let page = dir.path().join("deck.html");
                std::fs::write(&page, html)
                    .map_err(|e| Error::Other(format!("cannot write {}: {e}", page.display())))?;
                let url = format!("{}?print-pdf", file_url(&page));
                Ok(Deck { url, served: None })
            }
            DeckSource::Served => Ok(Deck {
                url: SERVED_DECK_URL.to_string(),
                served: Some(html),
            }),
        }
    }
}

/// The size of the `Fetch.fulfillRequest` message that serves a deck of `html_len` bytes
/// (base64 body, NUL terminator included).
fn served_message_len(html_len: usize) -> usize {
    html_len.div_ceil(3) * 4 + FULFILL_OVERHEAD
}

/// Fails when a deck of `html_len` bytes is too large to be served to `chrome` in one
/// DevTools message (module docs, step 2, fallback). `base` is the temporary directory the
/// browser cannot read.
fn check_servable(html_len: usize, chrome: &Path, base: &Path) -> Result<()> {
    if served_message_len(html_len) <= MAX_MESSAGE {
        return Ok(());
    }
    let limit = (MAX_MESSAGE - FULFILL_OVERHEAD) / 4 * 3;
    let name = snap_name(chrome).unwrap_or_else(|| "<name>".into());
    Err(Error::Other(format!(
        "the deck is too large to print with {}: {html_len} bytes of HTML, but this \
         browser cannot read {} (a snap that has never run), so it gets the deck in one \
         DevTools message, which holds at most {limit} bytes\n  run the browser once \
         (that creates ~/snap/{name}/common, which presenter then uses), or name a browser \
         that is not a snap with --chrome PATH",
        chrome.display(),
        base.display()
    )))
}

/// `file://` URL of an absolute path, percent-encoding everything but unreserved
/// characters, `/` and `:`.
fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.first() != Some(&b'/') {
        url.push('/');
    }
    for &byte in bytes {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                url.push(char::from(byte));
            }
            _ => {
                let _ = write!(url, "%{byte:02X}");
            }
        }
    }
    url
}

/// Where an export's temporary directory goes (`None`: the system's temporary directory)
/// and how Chrome gets the deck (module docs, steps 1 and 2). A snap has a private `/tmp`
/// that presenter can neither see nor clean, so for a snap the directory goes to its
/// `<home>/snap/<name>/common`, which it can read, when that exists; otherwise the deck is
/// served.
fn placement(chrome: &Path, home: Option<&Path>) -> (Option<PathBuf>, DeckSource) {
    let Some(name) = snap_name(chrome) else {
        return (None, DeckSource::File);
    };
    let common = home.map(|home| home.join("snap").join(name).join("common"));
    match common.filter(|dir| dir.is_dir()) {
        Some(dir) => (Some(dir), DeckSource::File),
        None => (None, DeckSource::Served),
    }
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

/// Steps 2, 3 and 5 of the module docs on a launched browser, with `job` as step 4. The
/// error is the reason, without the browser's name and stderr ([`chrome_error`] adds
/// those).
fn drive<T>(
    browser: &mut Browser,
    deck: &Deck<'_>,
    deadline: Instant,
    timeout: Duration,
    job: impl FnOnce(&mut Cdp<'_>, &str) -> std::result::Result<T, Failure>,
) -> std::result::Result<T, String> {
    let mut cdp = Cdp::new(browser, deck, deadline, timeout);
    let result = open_deck(&mut cdp).and_then(|page| job(&mut cdp, &page));
    match result {
        Ok(value) => {
            cdp.close_browser();
            Ok(value)
        }
        Err(failure) => {
            let mut reason = failure.describe(cdp.timeout);
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

/// The error for a failed export (`verb`: `print`, `capture`): the reason, the browser and
/// Chrome's last stderr lines.
fn chrome_error(chrome: &Path, verb: &str, reason: &str, stderr: &[String]) -> Error {
    let mut message = format!(
        "Chrome failed to {verb} the deck: {reason}\n  browser: {}",
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

/// Steps 2 and 3 of the module docs: open a page, load the deck in print view and wait for
/// reveal.js to lay the pages out. Returns the page's DevTools session.
fn open_deck(cdp: &mut Cdp) -> std::result::Result<String, Failure> {
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
    if cdp.deck.served.is_some() {
        let patterns = json!({ "patterns": [{ "urlPattern": SERVED_ORIGIN_PATTERN }] });
        cdp.call(page, "Fetch.enable", patterns)?;
    }
    for domain in ["Page.enable", "Runtime.enable", "Inspector.enable"] {
        cdp.call(page, domain, json!({}))?;
    }

    cdp.stage = "loading the deck";
    cdp.loaded = false;
    let url = cdp.deck.url.clone();
    let navigation = cdp.call(page, "Page.navigate", json!({ "url": url }))?;
    if let Some(error) = navigation.get("errorText").and_then(Value::as_str) {
        return Err(Failure::Other(load_error(cdp.deck, error)));
    }
    while !cdp.loaded {
        let message = cdp.next_message()?;
        cdp.handle_event(&message)?;
    }

    cdp.stage = "waiting for reveal.js to lay out the pages";
    wait_until_ready(cdp, page)?;
    Ok(session)
}

/// Step 4 of the module docs: prints the page `session` and returns the PDF.
fn print_pdf(cdp: &mut Cdp, session: &str) -> std::result::Result<Vec<u8>, Failure> {
    let page = Some(session);
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

/// The reason when Chrome cannot open `deck` (`error`, e.g. `net::ERR_FILE_NOT_FOUND`).
/// A `file://` deck that cannot be read means a confined browser that presenter did not
/// recognize as one: the hint names the workaround.
fn load_error(deck: &Deck<'_>, error: &str) -> String {
    let mut reason = format!("cannot load {}: {error}", deck.url);
    if deck.served.is_none() {
        reason.push_str(
            "\n  a sandboxed browser may be unable to read the temporary directory: set \
             TMPDIR to a directory it can read",
        );
    }
    reason
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

/// Evaluates the JavaScript `expression` in the page `session`, awaits it when it is a
/// promise, and returns its value (by value; `undefined` is `null`). An exception is a
/// failure that quotes it.
fn evaluate(
    cdp: &mut Cdp,
    session: Option<&str>,
    expression: &str,
) -> std::result::Result<Value, Failure> {
    let params = json!({
        "expression": expression,
        "awaitPromise": true,
        "returnByValue": true,
    });
    let mut evaluated = cdp.call(session, "Runtime.evaluate", params)?;
    if let Some(details) = evaluated.get("exceptionDetails") {
        return Err(Failure::Other(format!(
            "JavaScript in the page threw: {}",
            exception_text(details)
        )));
    }
    Ok(evaluated["result"]["value"].take())
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

/// A `Runtime.ExceptionDetails` as one line: the exception's description (for an `Error`,
/// the first line of its stack: `TypeError: …`), or for a thrown value without one, like
/// `throw 'boom'`, Chrome's text and the value (`Uncaught boom`), else Chrome's text.
fn exception_text(details: &Value) -> String {
    let exception = &details["exception"];
    let text = details["text"].as_str();
    let text = match (exception["description"].as_str(), exception.get("value")) {
        (Some(description), _) => description.to_string(),
        (None, Some(value)) => {
            let value = match value {
                Value::String(value) => value.clone(),
                other => other.to_string(),
            };
            format!("{} {value}", text.unwrap_or("Uncaught"))
        }
        (None, None) => text.unwrap_or("unknown error").to_string(),
    };
    text.lines().next().unwrap_or("").to_string()
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
    /// The deck.
    deck: &'a Deck<'a>,
    /// When everything must be done. A job may move it (and `timeout` with it).
    deadline: Instant,
    /// How long everything may take, for the timeout message: `deadline` minus the start.
    timeout: Duration,
    /// The last request id used.
    next_id: u64,
    /// What is going on, for error messages.
    stage: &'static str,
    /// Whether `Page.loadEventFired` arrived since `loaded` was last reset.
    loaded: bool,
    /// The page's uncaught exceptions, `console.error` messages, dismissed dialogs and
    /// `error` entries of the `Log` domain (only `devtools::Page` enables that domain).
    page_errors: Vec<String>,
}

impl<'a> Cdp<'a> {
    /// A connection to `browser` that opens `deck` and must be done by `deadline`
    /// (`timeout` from now).
    fn new(
        browser: &'a mut Browser,
        deck: &'a Deck<'a>,
        deadline: Instant,
        timeout: Duration,
    ) -> Cdp<'a> {
        Cdp {
            browser,
            deck,
            deadline,
            timeout,
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
        if bytes.len() >= MAX_MESSAGE {
            return Err(Failure::Other(format!(
                "{method}: the DevTools message is {} bytes, more than Chrome accepts \
                 ({MAX_MESSAGE} bytes)",
                bytes.len()
            )));
        }
        let commands = self.browser.commands.as_ref();
        if commands.is_some_and(|commands| commands.send(bytes).is_ok()) {
            Ok(self.next_id)
        } else {
            Err(self.gone())
        }
    }

    /// The next message from Chrome, waiting at most until the deadline and failing as soon
    /// as Chrome has exited or either pipe has closed.
    fn next_message(&mut self) -> std::result::Result<Value, Failure> {
        loop {
            let left = remaining(self.deadline).ok_or(Failure::Timeout(self.stage))?;
            match self.browser.replies.recv_timeout(left.min(POLL)) {
                Ok(Incoming::Message(bytes)) => {
                    return serde_json::from_slice(&bytes).map_err(|e| {
                        let text = String::from_utf8_lossy(&bytes);
                        let shown: String = text.chars().take(SHOWN_MESSAGE).collect();
                        Failure::Other(format!("invalid DevTools message ({e}): {shown}"))
                    });
                }
                Ok(Incoming::WriteFailed(error)) => return Err(self.unwritable(&error)),
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(status) = self.browser.try_wait() {
                        return Err(self.exited(status));
                    }
                }
                Ok(Incoming::Closed) | Err(RecvTimeoutError::Disconnected) => {
                    return Err(self.gone());
                }
            }
        }
    }

    /// The failure when a command could not be written (`error`): Chrome exited, or closed
    /// its command pipe.
    fn unwritable(&mut self, error: &std::io::Error) -> Failure {
        match self.browser.wait_exit(Duration::from_secs(1)) {
            Some(status) => self.exited(status),
            None => Failure::Other(format!(
                "Chrome closed its DevTools command pipe ({error}) while {}",
                self.stage
            )),
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
            // Only with `Log.enable`, which `devtools::Page` sends (the export does not).
            Some("Log.entryAdded") => {
                let entry = &params["entry"];
                let text = entry["text"].as_str().unwrap_or("");
                tracing::debug!("page log ({}): {text}", entry["level"]);
                if entry["level"] == "error" {
                    self.page_errors.push(match entry["url"].as_str() {
                        Some(url) if !url.is_empty() => format!("{text} ({url})"),
                        _ => text.to_string(),
                    });
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

    /// Answers a paused request for `http://presenter.localhost/…` (only a served deck
    /// enables that): the deck for [`SERVED_DECK_URL`], 404 for anything else.
    fn serve(&mut self, session: Option<&str>, params: &Value) -> std::result::Result<(), Failure> {
        let url = params["request"]["url"].as_str().unwrap_or("");
        let (code, body) = match self.deck.served {
            Some(html) if url == SERVED_DECK_URL => {
                (200, base64::engine::general_purpose::STANDARD.encode(html))
            }
            _ => {
                tracing::debug!("the deck requested {url}: 404");
                (404, String::new())
            }
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
    /// Chrome's messages from its descriptor 4, read by a reader thread, and the writer
    /// thread's failure.
    replies: Receiver<Incoming>,
    /// Its last [`STDERR_TAIL`] stderr lines, filled by a reader thread.
    stderr: Arc<Mutex<VecDeque<String>>>,
    /// Disconnected when that thread reaches the end of Chrome's stderr.
    stderr_closed: Receiver<()>,
}

impl Browser {
    /// Starts `chrome` (module docs, step 1) with its profile in `profile`, its own
    /// temporary files in `scratch` when given (`TMPDIR` in its environment), and the
    /// threads for its pipes.
    fn launch(chrome: &Path, profile: &Path, scratch: Option<&Path>) -> Result<Browser> {
        let mut user_data_dir = OsString::from("--user-data-dir=");
        user_data_dir.push(profile);
        tracing::debug!("starting {} {}", chrome.display(), CHROME_FLAGS.join(" "));
        let mut command = Command::new("/bin/sh");
        if let Some(scratch) = scratch {
            command.env("TMPDIR", scratch);
        }
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
        let write_failures = from_chrome.clone();
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
        spawn_thread("chrome-commands", move || {
            write_messages(input, &to_chrome, &write_failures);
        })?;
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

/// What the threads on Chrome's DevTools pipes report to [`Cdp::next_message`].
#[derive(Debug)]
enum Incoming {
    /// A message from Chrome, without its NUL terminator.
    Message(Vec<u8>),
    /// Chrome's reply pipe reached its end (or failed).
    Closed,
    /// A command could not be written to Chrome's command pipe.
    WriteFailed(std::io::Error),
}

/// The body of the writer thread: writes each message to Chrome's command pipe with its
/// NUL terminator, until the channel closes (then the pipe is closed) or a write fails,
/// which it reports to `failures` (so that a wait for a reply ends at once).
fn write_messages(
    mut input: impl std::io::Write,
    messages: &Receiver<Vec<u8>>,
    failures: &Sender<Incoming>,
) {
    for mut message in messages {
        message.push(0);
        if let Err(e) = input.write_all(&message).and_then(|()| input.flush()) {
            tracing::debug!("cannot write to Chrome's DevTools pipe: {e}");
            let _ = failures.send(Incoming::WriteFailed(e));
            return;
        }
    }
}

/// The body of the reader thread: sends each NUL-terminated message from Chrome to
/// `messages`, then [`Incoming::Closed`] when the pipe closes; stops early when nobody
/// listens any more.
fn read_messages(output: impl std::io::Read, messages: &Sender<Incoming>) {
    let mut reader = BufReader::new(output);
    loop {
        let mut message = Vec::new();
        match reader.read_until(0, &mut message) {
            Ok(0) | Err(_) => {
                let _ = messages.send(Incoming::Closed);
                return;
            }
            Ok(_) => {}
        }
        if message.last() == Some(&0) {
            message.pop();
        }
        if messages.send(Incoming::Message(message)).is_err() {
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
    ///
    /// The directory is then named by its real path ([`std::fs::canonicalize`]; made
    /// absolute if that fails): that path becomes the deck's `file://` URL and Chrome's
    /// `--user-data-dir`. A relative `base` (`TMPDIR=tmp`; [`std::env::temp_dir`] returns
    /// `$TMPDIR` as set) would make them depend on Chrome's current directory, and a `..`
    /// after a symbolic link (`TMPDIR=lnk/../t`) would name another directory in the URL:
    /// the system follows `lnk` before `..`, Chrome drops `lnk/..` from the URL.
    fn new(base: &Path, prefix: &str) -> Result<TempDir> {
        let created = create_private_dir(base, prefix)?;
        let path = std::fs::canonicalize(&created).unwrap_or_else(|_| absolute(&created));
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
    // Only Unix sets the mode, so the binding is never mutated elsewhere.
    #[cfg_attr(not(unix), allow(unused_mut))]
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
    #[cfg(all(test, unix))]
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

    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static PROCESSES: Mutex<()> = Mutex::new(());
        PROCESSES.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The names in `dir`.
    #[cfg(unix)]
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

        // A snap reads the deck from its common directory when that exists; else the
        // deck is served and the profile goes to the system's temp directory.
        let home = tempfile::tempdir().unwrap();
        let snap = Path::new("/snap/bin/chromium");
        let served = (None, DeckSource::Served);
        assert_eq!(placement(snap, Some(home.path())), served);
        let common = home.path().join("snap/chromium/common");
        std::fs::create_dir_all(&common).unwrap();
        assert_eq!(
            placement(snap, Some(home.path())),
            (Some(common), DeckSource::File)
        );
        assert_eq!(placement(snap, None), served);
        // Any other browser reads the deck from the system's temp directory.
        let other = (None, DeckSource::File);
        assert_eq!(placement(&plain, Some(home.path())), other);
        assert_eq!(placement(Path::new("/usr/bin/chromium"), None), other);
    }

    #[test]
    fn file_urls_are_percent_encoded() {
        assert_eq!(
            file_url(Path::new("/tmp/a b/#deck%?.html")),
            "file:///tmp/a%20b/%23deck%25%3F.html"
        );
        assert_eq!(file_url(Path::new("/x/deck.html")), "file:///x/deck.html");
        assert_eq!(
            file_url(Path::new("/Users/zoë/snap~1/d.html")),
            "file:///Users/zo%C3%AB/snap~1/d.html"
        );
    }

    #[test]
    fn unreadable_file_decks_suggest_another_temporary_directory() {
        let file = Deck {
            url: "file:///tmp/p/deck.html?print-pdf".into(),
            served: None,
        };
        assert_eq!(
            load_error(&file, "net::ERR_FILE_NOT_FOUND"),
            "cannot load file:///tmp/p/deck.html?print-pdf: net::ERR_FILE_NOT_FOUND\n  a \
             sandboxed browser may be unable to read the temporary directory: set TMPDIR to \
             a directory it can read"
        );
        let served = Deck {
            url: SERVED_DECK_URL.into(),
            served: Some("<p>deck</p>"),
        };
        assert_eq!(
            load_error(&served, "net::ERR_FAILED"),
            format!("cannot load {SERVED_DECK_URL}: net::ERR_FAILED")
        );
    }

    #[test]
    fn served_decks_must_fit_into_one_devtools_message() {
        assert_eq!(served_message_len(0), FULFILL_OVERHEAD);
        assert_eq!(served_message_len(3), 4 + FULFILL_OVERHEAD);
        assert_eq!(served_message_len(4), 8 + FULFILL_OVERHEAD);
        // The largest servable deck: about 75 MB of HTML.
        let limit = (MAX_MESSAGE - FULFILL_OVERHEAD) / 4 * 3;
        assert!(served_message_len(limit) <= MAX_MESSAGE);
        assert!(served_message_len(limit + 1) > MAX_MESSAGE);
        let chrome = Path::new("/snap/bin/chromium");
        let base = Path::new("/tmp");
        assert!(check_servable(limit, chrome, base).is_ok());
        let message = check_servable(limit + 1, chrome, base)
            .unwrap_err()
            .to_string();
        assert_eq!(
            message,
            format!(
                "the deck is too large to print with /snap/bin/chromium: {} bytes of HTML, \
                 but this browser cannot read /tmp (a snap that has never run), so it gets \
                 the deck in one DevTools message, which holds at most {limit} bytes\n  \
                 run the browser once (that creates ~/snap/chromium/common, which presenter \
                 then uses), or name a browser that is not a snap with --chrome PATH",
                limit + 1
            )
        );
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
        assert!(path.starts_with(std::fs::canonicalize(base.path()).unwrap()));
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

    /// `path` (absolute) as a path relative to the current directory: `..` up to the root,
    /// then `path` below it.
    #[cfg(unix)]
    fn relative_to_cwd(path: &Path) -> PathBuf {
        let cwd = std::env::current_dir().unwrap();
        let up = cwd.components().skip(1).map(|_| Path::new(".."));
        up.collect::<PathBuf>()
            .join(path.strip_prefix("/").unwrap())
    }

    /// The file a deck's `file://…?print-pdf` URL names for Chrome, which resolves `.` and
    /// `..` in the URL lexically (before the system sees the path, so `lnk/..` is dropped
    /// even when `lnk` is a symbolic link).
    #[cfg(unix)]
    fn file_chrome_loads(url: &str) -> PathBuf {
        use std::path::Component;
        let page = url
            .strip_prefix("file://")
            .and_then(|url| url.strip_suffix("?print-pdf"))
            .unwrap_or_else(|| panic!("{url}"));
        assert!(page.starts_with('/') && !page.contains('%'), "{url}");
        let mut path = PathBuf::new();
        for component in Path::new(page).components() {
            match component {
                Component::ParentDir => {
                    path.pop();
                }
                Component::CurDir => {}
                other => path.push(other),
            }
        }
        path
    }

    /// Creates a [`TempDir`] in `base` and checks that its path is real (absolute, no
    /// symbolic link, `.` or `..`) and that the deck's URL names the deck written into it;
    /// returns the directory.
    #[cfg(unix)]
    fn assert_deck_url_names_the_deck(base: &Path) -> TempDir {
        let dir = TempDir::new(base, "presenter-test").unwrap();
        let path = dir.path();
        assert!(path.is_absolute(), "{path:?}");
        assert_eq!(std::fs::canonicalize(path).unwrap(), path);
        let deck = Deck::new(&dir, "<p>deck</p>", DeckSource::File).unwrap();
        let page = file_chrome_loads(&deck.url);
        assert_eq!(
            std::fs::read_to_string(&page).ok().as_deref(),
            Some("<p>deck</p>"),
            "{} names the deck",
            deck.url
        );
        dir
    }

    /// `TMPDIR=tmp` makes [`std::env::temp_dir`] relative: the deck's `file://` URL (and
    /// Chrome's profile) must still name the directory that was created, not `/tmp`.
    #[cfg(unix)]
    #[test]
    fn a_relative_temporary_base_gives_absolute_paths_and_urls() {
        let _serial = serial();
        let base = tempfile::tempdir().unwrap();
        let relative = relative_to_cwd(base.path());
        assert!(relative.is_relative() && relative.is_dir(), "{relative:?}");
        let dir = assert_deck_url_names_the_deck(&relative);
        assert_eq!(entries(base.path()).len(), 1, "created below the base");
        drop(dir);
        assert!(entries(base.path()).is_empty());
    }

    /// `TMPDIR=lnk/../t` with `lnk` a symbolic link: the system follows `lnk` before `..`,
    /// but Chrome drops `lnk/..` from the URL, so the URL must name the real directory.
    #[cfg(unix)]
    #[test]
    fn a_temporary_base_with_dot_dot_after_a_symbolic_link_gives_the_real_directory() {
        let _serial = serial();
        let base = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(base.path().join("elsewhere/sub")).unwrap();
        std::fs::create_dir(base.path().join("elsewhere/t")).unwrap();
        std::os::unix::fs::symlink("elsewhere/sub", base.path().join("lnk")).unwrap();
        for spelling in [
            base.path().join("lnk/../t"),
            base.path().join("lnk/./../t/."),
        ] {
            let dir = assert_deck_url_names_the_deck(&spelling);
            assert_eq!(entries(&base.path().join("elsewhere/t")).len(), 1);
            drop(dir);
            assert!(entries(&base.path().join("elsewhere/t")).is_empty());
        }
        // Relative to the current directory as well.
        let relative = relative_to_cwd(base.path()).join("lnk/../t");
        let dir = assert_deck_url_names_the_deck(&relative);
        assert_eq!(entries(&base.path().join("elsewhere/t")).len(), 1);
        drop(dir);
        assert!(!base.path().join("t").exists());
    }

    /// `TMPDIR=` (set, but empty) counts as unset: `/tmp`, not the current directory.
    #[test]
    fn an_empty_tmpdir_means_tmp() {
        assert_eq!(system_temp_dir(PathBuf::new()), Path::new("/tmp"));
        assert_eq!(system_temp_dir("/var/tmp".into()), Path::new("/var/tmp"));
        assert_eq!(system_temp_dir("reltmp".into()), Path::new("reltmp"));
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
        print_deck_with(chrome, "<p>deck</p>", DeckSource::File, timeout)
    }

    /// Prints `html` like [`html_to_pdf`], with the temporary directory in `base` and the
    /// deck handed over as `source` says.
    #[cfg(unix)]
    fn print_in(
        chrome: &Path,
        html: &str,
        timeout: Duration,
        base: &Path,
        source: DeckSource,
    ) -> Result<Vec<u8>> {
        run_in(chrome, html, timeout, base, source, "print", print_pdf)
    }

    /// [`print_with`] for the deck `html`, handed over as `source` says.
    #[cfg(unix)]
    fn print_deck_with(
        chrome: &Path,
        html: &str,
        source: DeckSource,
        timeout: Duration,
    ) -> (String, Duration) {
        let base = tempfile::tempdir().unwrap();
        for _ in 0..5 {
            let started = Instant::now();
            let err = print_in(chrome, html, timeout, base.path(), source).unwrap_err();
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
    fn chrome_gets_a_temporary_directory_inside_the_exports() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let fake = script(dir.path(), "chrome", "echo \"tmpdir=$TMPDIR\" >&2\nexit 7");
        let reported = |source: DeckSource| {
            let (message, _) =
                print_deck_with(&fake, "<p>deck</p>", source, Duration::from_secs(20));
            message
                .lines()
                .find_map(|line| line.trim().strip_prefix("tmpdir="))
                .unwrap_or_else(|| panic!("no tmpdir line in {message}"))
                .to_string()
        };
        // A file deck: Chrome's own temporary files go into the export's directory.
        let tmpdir = reported(DeckSource::File);
        assert!(tmpdir.contains("/presenter-pdf-"), "{tmpdir}");
        assert!(tmpdir.ends_with("/tmp"), "{tmpdir}");
        // A served deck's browser cannot read that directory: its TMPDIR stays as it is.
        let tmpdir = reported(DeckSource::Served);
        assert!(!tmpdir.contains("/presenter-pdf-"), "{tmpdir}");
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

    #[cfg(unix)]
    #[test]
    fn a_closed_command_pipe_fails_fast() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        // Reads the first command, closes the command pipe, then opens a dialog, which
        // presenter answers: that write fails while the reply pipe stays open.
        let fake = script(
            dir.path(),
            "chrome",
            "head -c 1 <&3 >/dev/null\nexec 3<&-\n\
             printf '{\"method\":\"Page.javascriptDialogOpening\",\
             \"params\":{\"type\":\"alert\",\"message\":\"hi\"}}\\0' >&4\n\
             exec sleep 30",
        );
        let (message, elapsed) = print_with(&fake, Duration::from_secs(20));
        assert!(
            message.starts_with(
                "Chrome failed to print the deck: Chrome closed its DevTools command pipe ("
            ),
            "{message}"
        );
        assert!(
            message.contains(
                ") while starting Chrome\n  page errors:\n    dismissed a \
                 JavaScript alert: hi\n"
            ),
            "{message}"
        );
        assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    }

    #[test]
    fn write_failures_and_closed_pipes_are_reported() {
        /// A pipe whose reader is gone.
        struct Broken;
        impl std::io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (commands, to_chrome) = mpsc::channel();
        let (from_chrome, replies) = mpsc::channel();
        commands.send(b"{}".to_vec()).unwrap();
        write_messages(Broken, &to_chrome, &from_chrome);
        match replies.try_recv() {
            Ok(Incoming::WriteFailed(e)) => assert_eq!(e.kind(), ErrorKind::BrokenPipe),
            other => panic!("{other:?}"),
        }
        // Messages are delivered with their NUL terminator removed, then the end.
        let output: &[u8] = b"{\"id\":1}\0{\"id\":2}\0";
        read_messages(output, &from_chrome);
        let received: Vec<Incoming> = replies.try_iter().collect();
        assert!(
            matches!(
                received.as_slice(),
                [Incoming::Message(a), Incoming::Message(b), Incoming::Closed]
                    if a == b"{\"id\":1}" && b == b"{\"id\":2}"
            ),
            "{received:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_deck_too_large_to_serve_is_refused_before_chrome_starts() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("started");
        let fake = script(
            dir.path(),
            "chrome",
            &format!("touch '{}'\nexit 1", marker.display()),
        );
        let limit = (MAX_MESSAGE - FULFILL_OVERHEAD) / 4 * 3;
        let html = "x".repeat(limit + 1);
        let (message, _) = print_deck_with(&fake, &html, DeckSource::Served, DEFAULT_TIMEOUT);
        assert!(
            message.starts_with("the deck is too large to print with "),
            "{message}"
        );
        assert!(!marker.exists(), "the browser was started");
    }

    /// A real browser for the tests that print, or `None` after saying why the test is
    /// skipped.
    #[cfg(unix)]
    fn real_chrome() -> Option<PathBuf> {
        let found = find_chrome(None);
        if found.is_none() {
            eprintln!("skipping: no chrome found");
        }
        found
    }

    /// A deck built from `markdown` like `presenter export pdf` builds it (standalone).
    #[cfg(unix)]
    fn standalone_deck(markdown: &str) -> String {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("deck.md");
        std::fs::write(&input, markdown).unwrap();
        let options = crate::pipeline::BuildOptions::new(&input);
        crate::pipeline::build_deck(&options).unwrap().document.html
    }

    #[cfg(unix)]
    #[test]
    fn a_served_deck_prints_like_a_file_deck() {
        let _serial = serial();
        let Some(chrome) = real_chrome() else {
            return;
        };
        // The second slide asks the served origin for an image: a 404, not a failure.
        let html =
            standalone_deck("# One\n\n---\n\n# Two\n\n<img src=\"/missing.png\" alt=\"gone\">\n");
        for source in [DeckSource::Served, DeckSource::File] {
            let base = tempfile::tempdir().unwrap();
            let timeout = Duration::from_secs(60);
            let pdf = print_in(&chrome, &html, timeout, base.path(), source)
                .unwrap_or_else(|e| panic!("{source:?}: {e}"));
            assert!(pdf.starts_with(b"%PDF-"), "{source:?}");
            assert_eq!(page_count(&pdf), 2, "{source:?}");
            assert!(entries(base.path()).is_empty(), "{source:?}");
        }
    }

    #[test]
    fn a_missing_executable_cannot_be_started() {
        let err = html_to_pdf(
            Path::new("/nonexistent/chrome"),
            "<p>x</p>",
            DEFAULT_TIMEOUT,
        )
        .unwrap_err();
        // (On Windows the export stops earlier: it needs macOS or Linux.)
        let expected = if cfg!(unix) {
            "cannot start /nonexistent/chrome: not an executable file"
        } else {
            "the export needs macOS or Linux (Chrome is started through /bin/sh)"
        };
        assert_eq!(err.to_string(), expected);
    }

    #[test]
    fn exceptions_read_as_their_description_or_the_thrown_value() {
        let error = json!({
            "text": "Uncaught",
            "exception": {
                "type": "object", "subtype": "error", "className": "TypeError",
                "description": "TypeError: x is null\n    at <anonymous>:1:1",
            },
        });
        assert_eq!(exception_text(&error), "TypeError: x is null");
        let string = json!({
            "text": "Uncaught",
            "exception": { "type": "string", "value": "boom\nsecond line" },
        });
        assert_eq!(exception_text(&string), "Uncaught boom");
        let rejected = json!({
            "text": "Uncaught (in promise)",
            "exception": { "type": "object", "subtype": "null", "value": null },
        });
        assert_eq!(exception_text(&rejected), "Uncaught (in promise) null");
        let number = json!({
            "text": "Uncaught",
            "exception": { "type": "number", "value": 3, "description": "3" },
        });
        assert_eq!(exception_text(&number), "3");
        let undefined = json!({ "text": "Uncaught", "exception": { "type": "undefined" } });
        assert_eq!(exception_text(&undefined), "Uncaught");
        assert_eq!(exception_text(&json!({})), "unknown error");
    }
}
