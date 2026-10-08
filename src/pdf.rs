//! PDF export through headless Chrome (`presenter export pdf`, `docs/spec/cli.md` export).
//!
//! [`find_chrome`] locates a Chrome or Chromium executable; [`html_to_pdf`] prints a
//! standalone deck with it, in reveal.js' print view (`?print-pdf`).
//!
//! # How a deck is printed
//!
//! Chrome's one-shot `--print-to-pdf` flag cannot wait for reveal.js to lay the pages out
//! (and hangs in some environments), so [`html_to_pdf`] drives Chrome over the DevTools
//! protocol instead, on one WebSocket with flat sessions:
//!
//! 1. The HTML is written to `deck.html` in a new temporary directory (mode 0700 on Unix).
//!    Chrome starts headless with `--remote-debugging-port=0` and a fresh profile in
//!    `<tmp>/profile` (plus `--no-sandbox` when running as root on Linux, which Chrome
//!    requires); the `DevTools listening on ws://…` line on its stderr names the port.
//! 2. `Target.createTarget` (about:blank) and `Target.attachToTarget {flatten: true}` open
//!    a page; `Page.enable`, `Runtime.enable`, `Inspector.enable`; `Page.navigate` to
//!    `file://<tmp>/deck.html?print-pdf`; then the page's `Page.loadEventFired`.
//! 3. `Runtime.evaluate` of [`READY_SCRIPT`] (awaited): waits for `Reveal.isReady()`, the
//!    end of reveal's print layout (`html.print-pdf` set, `loading-scroll-mode` gone from
//!    the viewport, i.e. after its `pdf-ready` event), `document.fonts.ready` and two
//!    animation frames. KaTeX and highlight.js run before reveal is ready.
//! 4. `Page.printToPDF {preferCSSPageSize, printBackground, margins 0, ReturnAsStream}`
//!    (reveal sets `@page { size: …; margin: 0 }`), read with `IO.read` until `eof`.
//! 5. `Browser.close`; Chrome gets [`CLOSE_GRACE`] to exit, then it is killed.
//!
//! Everything shares one deadline (`timeout`, [`DEFAULT_TIMEOUT`] for the CLI). Chrome is
//! always stopped and the temporary directory removed, on errors too. A failure is an
//! [`Error::Other`] that names the browser, says what failed (`timed out after 120 s while
//! …` for the deadline), lists the page's uncaught errors when there were any, and ends with
//! the last lines Chrome wrote on stderr.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, ErrorKind};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};
use tungstenite::handshake::HandshakeError;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket};

use crate::error::{Error, Result};

/// How long `presenter export pdf` lets Chrome take, from start to the last PDF byte.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// How long Chrome may take to exit after `Browser.close` before it is killed.
pub const CLOSE_GRACE: Duration = Duration::from_secs(5);

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
/// a quiet throwaway profile (no first-run UI, sync, extensions or keychain access) and a
/// DevTools port chosen by Chrome.
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
    "--remote-debugging-port=0",
];

/// What precedes the browser's WebSocket URL on Chrome's stderr.
const DEVTOOLS_PREFIX: &str = "DevTools listening on ";

/// How many of Chrome's last stderr lines an error shows.
const STDERR_TAIL: usize = 30;

/// Bytes requested per `IO.read` of the printed PDF.
const READ_CHUNK: u64 = 1 << 20;

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
fn resolve_executable(path: &Path, path_var: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    if path.extension().is_some_and(|ext| ext == "app") && path.is_dir() {
        let exe = path.join("Contents/MacOS").join(path.file_stem()?);
        return is_executable(&exe).then(|| absolute(&exe));
    }
    if is_executable(path) {
        return Some(absolute(path));
    }
    let bare = path.components().count() == 1 && path.parent() == Some(Path::new(""));
    if bare { which(path, path_var?) } else { None }
}

/// The first executable `name` in the directories of `path_var` (a `PATH` value).
fn which(name: &Path, path_var: &std::ffi::OsStr) -> Option<PathBuf> {
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
/// docs) and returns the PDF's bytes. All of it must finish within `timeout`.
///
/// Errors (all [`Error::Other`]): the temporary directory or file cannot be written;
/// Chrome cannot be started; Chrome exits, times out or the page fails (with the browser's
/// path, the page's errors and Chrome's stderr).
pub fn html_to_pdf(chrome: &Path, html: &str, timeout: Duration) -> Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    let dir = TempDir::new("presenter-pdf")?;
    let page = dir.path().join("deck.html");
    std::fs::write(&page, html)
        .map_err(|e| Error::Other(format!("cannot write {}: {e}", page.display())))?;
    let url = format!("{}?print-pdf", file_url(&page));
    tracing::info!("printing {url} with {}", chrome.display());
    let (mut browser, devtools) = Browser::launch(chrome, &dir.path().join("profile"))?;
    let result = drive(&mut browser, &devtools, &url, deadline, timeout);
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

/// The number of pages of a PDF written by Chrome: the `/Type /Page` dictionaries (Chrome
/// does not compress its object dictionaries). 0 when there are none.
pub fn page_count(pdf: &[u8]) -> usize {
    const TYPE: &[u8] = b"/Type";
    const PAGE: &[u8] = b"/Page";
    let mut count = 0;
    let mut rest = pdf;
    while let Some(at) = rest.windows(TYPE.len()).position(|window| window == TYPE) {
        rest = &rest[at + TYPE.len()..];
        let value = rest.trim_ascii_start();
        let is_page = value.starts_with(PAGE)
            && !value
                .get(PAGE.len())
                .is_some_and(|next| next.is_ascii_alphanumeric());
        count += usize::from(is_page);
    }
    count
}

/// Steps 1–5 of the module docs on a launched browser. The error is the reason, without
/// the browser's name and stderr ([`chrome_error`] adds those).
fn drive(
    browser: &mut Browser,
    devtools: &Receiver<String>,
    url: &str,
    deadline: Instant,
    timeout: Duration,
) -> std::result::Result<Vec<u8>, String> {
    let ws_url = wait_for_devtools(browser, devtools, deadline).map_err(|f| f.describe(timeout))?;
    tracing::debug!("Chrome DevTools at {ws_url}");
    let mut cdp = Cdp::connect(&ws_url, deadline).map_err(|f| f.describe(timeout))?;
    match print_page(&mut cdp, url) {
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

/// Waits for the `DevTools listening on ws://…` line and returns the URL.
fn wait_for_devtools(
    browser: &mut Browser,
    devtools: &Receiver<String>,
    deadline: Instant,
) -> std::result::Result<String, Failure> {
    const STAGE: &str = "waiting for Chrome to open its DevTools port";
    let left = remaining(deadline).ok_or(Failure::Timeout(STAGE))?;
    match devtools.recv_timeout(left) {
        Ok(url) => Ok(url),
        Err(RecvTimeoutError::Timeout) => Err(Failure::Timeout(STAGE)),
        Err(RecvTimeoutError::Disconnected) => Err(Failure::Other(
            match browser.wait_exit(Duration::from_secs(1)) {
                Some(status) => {
                    format!("Chrome exited ({status}) before opening its DevTools port")
                }
                None => "Chrome closed its stderr before opening its DevTools port".to_string(),
            },
        )),
    }
}

/// Steps 2–4 of the module docs: open a page, load the deck in print view, wait for
/// reveal.js and print.
fn print_page(cdp: &mut Cdp, url: &str) -> std::result::Result<Vec<u8>, Failure> {
    cdp.stage = "opening a page";
    let target = cdp.call(None, "Target.createTarget", json!({ "url": "about:blank" }))?;
    let target_id = string_field(&target, "targetId")?;
    let attach = json!({ "targetId": target_id, "flatten": true });
    let session = string_field(
        &cdp.call(None, "Target.attachToTarget", attach)?,
        "sessionId",
    )?;
    let page = Some(session.as_str());
    for domain in ["Page.enable", "Runtime.enable", "Inspector.enable"] {
        cdp.call(page, domain, json!({}))?;
    }

    cdp.stage = "loading the deck";
    cdp.loaded = false;
    let navigation = cdp.call(page, "Page.navigate", json!({ "url": url }))?;
    if let Some(error) = navigation.get("errorText").and_then(Value::as_str) {
        return Err(Failure::Other(format!("cannot open {url}: {error}")));
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
            b'\\' if cfg!(windows) => url.push('/'),
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                url.push(char::from(byte))
            }
            _ => {
                let _ = write!(url, "%{byte:02X}");
            }
        }
    }
    url
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
    /// An I/O error during `stage`: a timeout, or `<stage>: <error>`.
    fn io(stage: &'static str, err: &std::io::Error) -> Failure {
        match err.kind() {
            ErrorKind::WouldBlock | ErrorKind::TimedOut => Failure::Timeout(stage),
            _ => Failure::Other(format!("{stage}: {err}")),
        }
    }

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

/// A DevTools protocol connection to the browser (one WebSocket, flat sessions).
struct Cdp {
    /// The browser's WebSocket.
    socket: WebSocket<TcpStream>,
    /// When everything must be done.
    deadline: Instant,
    /// The last request id used.
    next_id: u64,
    /// What is going on, for timeout messages.
    stage: &'static str,
    /// Whether `Page.loadEventFired` arrived since `loaded` was last reset.
    loaded: bool,
    /// The page's uncaught exceptions and `console.error` messages.
    page_errors: Vec<String>,
}

impl Cdp {
    /// Connects to the browser endpoint `url` (`ws://127.0.0.1:<port>/devtools/browser/…`).
    fn connect(url: &str, deadline: Instant) -> std::result::Result<Cdp, Failure> {
        const STAGE: &str = "connecting to Chrome's DevTools port";
        let addr = url
            .strip_prefix("ws://")
            .and_then(|rest| rest.split('/').next())
            .and_then(|authority| authority.to_socket_addrs().ok()?.next())
            .ok_or_else(|| Failure::Other(format!("unexpected DevTools address {url}")))?;
        let left = remaining(deadline).ok_or(Failure::Timeout(STAGE))?;
        let stream = TcpStream::connect_timeout(&addr, left).map_err(|e| Failure::io(STAGE, &e))?;
        stream
            .set_read_timeout(Some(left))
            .and_then(|()| stream.set_write_timeout(Some(left)))
            .and_then(|()| stream.set_nodelay(true))
            .map_err(|e| Failure::io(STAGE, &e))?;
        // The peer is our own Chrome child; a large deck's messages may exceed the defaults.
        let config = WebSocketConfig::default()
            .max_message_size(None)
            .max_frame_size(None);
        let (socket, _) = tungstenite::client::client_with_config(url, stream, Some(config))
            .map_err(|e| match e {
                HandshakeError::Interrupted(_) => Failure::Timeout(STAGE),
                HandshakeError::Failure(e) => Failure::Other(format!("{STAGE}: {e}")),
            })?;
        Ok(Cdp {
            socket,
            deadline,
            next_id: 0,
            stage: STAGE,
            loaded: false,
            page_errors: Vec::new(),
        })
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

    /// Sends `method` and returns its request id.
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
        self.socket
            .send(Message::text(request.to_string()))
            .map_err(|e| self.socket_failure(e))?;
        Ok(self.next_id)
    }

    /// The next JSON message from the browser, waiting at most until the deadline.
    fn next_message(&mut self) -> std::result::Result<Value, Failure> {
        loop {
            let left = remaining(self.deadline).ok_or(Failure::Timeout(self.stage))?;
            self.socket
                .get_mut()
                .set_read_timeout(Some(left))
                .map_err(|e| Failure::io(self.stage, &e))?;
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    return serde_json::from_str(&text).map_err(|e| {
                        Failure::Other(format!("invalid DevTools message ({e}): {text}"))
                    });
                }
                Ok(Message::Close(_)) => {
                    return Err(Failure::Other(format!(
                        "Chrome closed the DevTools connection while {}",
                        self.stage
                    )));
                }
                Ok(_) => {}
                Err(e) => return Err(self.socket_failure(e)),
            }
        }
    }

    /// A WebSocket error as a [`Failure`] of the current stage.
    fn socket_failure(&self, err: tungstenite::Error) -> Failure {
        match err {
            tungstenite::Error::Io(e) => Failure::io(self.stage, &e),
            other => Failure::Other(format!(
                "DevTools connection failed while {}: {other}",
                self.stage
            )),
        }
    }

    /// Records the events presenter cares about; a crashed or closed page is an error.
    fn handle_event(&mut self, message: &Value) -> std::result::Result<(), Failure> {
        let params = &message["params"];
        match message.get("method").and_then(Value::as_str) {
            Some("Page.loadEventFired") => self.loaded = true,
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

/// A running Chrome. Dropping it kills Chrome if it is still running.
struct Browser {
    /// The Chrome process.
    child: Child,
    /// Its last [`STDERR_TAIL`] stderr lines, filled by a reader thread.
    stderr: Arc<Mutex<VecDeque<String>>>,
}

impl Browser {
    /// Starts `chrome` (headless, [`CHROME_FLAGS`], profile in `profile`). The receiver
    /// gets the DevTools WebSocket URL once Chrome prints it; it is disconnected when
    /// Chrome's stderr closes.
    fn launch(chrome: &Path, profile: &Path) -> Result<(Browser, Receiver<String>)> {
        let mut user_data_dir = OsString::from("--user-data-dir=");
        user_data_dir.push(profile);
        tracing::debug!("starting {} {}", chrome.display(), CHROME_FLAGS.join(" "));
        let mut command = Command::new(chrome);
        command.args(CHROME_FLAGS);
        if running_as_root() {
            // Chrome refuses to start as root (containers, CI) with its sandbox on.
            command.arg("--no-sandbox");
        }
        let mut child = command
            .arg(user_data_dir)
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Other(format!("cannot start {}: {e}", chrome.display())))?;
        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        let (sender, receiver) = mpsc::channel();
        if let Some(pipe) = child.stderr.take() {
            let log = Arc::clone(&stderr);
            let spawned = std::thread::Builder::new()
                .name("chrome-stderr".into())
                .spawn(move || read_stderr(pipe, &log, &sender));
            if let Err(e) = spawned {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Other(format!("cannot read Chrome's stderr: {e}")));
            }
        }
        Ok((Browser { child, stderr }, receiver))
    }

    /// Chrome's last stderr lines.
    fn stderr_tail(&self) -> Vec<String> {
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

    /// Gives Chrome `grace` to exit, then kills it, and reaps it.
    fn stop(&mut self, grace: Duration) {
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

/// The body of the stderr reader thread: keeps the last [`STDERR_TAIL`] lines in `log`
/// and sends the DevTools URL to `devtools`.
fn read_stderr(pipe: ChildStderr, log: &Mutex<VecDeque<String>>, devtools: &Sender<String>) {
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
        let url = devtools_url(&text).map(str::to_string);
        // The line is logged before the URL is sent, so a failure to connect to it can
        // show it.
        {
            let mut lines = log.lock().unwrap_or_else(PoisonError::into_inner);
            if lines.len() == STDERR_TAIL {
                lines.pop_front();
            }
            lines.push_back(text);
        }
        if let Some(url) = url {
            // The receiver is gone once the URL was taken or the export gave up.
            let _ = devtools.send(url);
        }
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

/// The WebSocket URL of a `DevTools listening on ws://…` stderr line.
fn devtools_url(line: &str) -> Option<&str> {
    let start = line.find(DEVTOOLS_PREFIX)? + DEVTOOLS_PREFIX.len();
    let url = line[start..].trim();
    url.starts_with("ws://").then_some(url)
}

/// A temporary directory, removed (with its contents) on drop.
struct TempDir(PathBuf);

impl TempDir {
    /// Creates `<system temp dir>/<prefix>-<pid>-<n>` (mode 0700 on Unix).
    fn new(prefix: &str) -> Result<TempDir> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir();
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
                Ok(()) => return Ok(TempDir(path)),
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

    /// The directory.
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Chrome's helper processes may still be finishing writes into the profile.
        for attempt in 0..20 {
            match std::fs::remove_dir_all(&self.0) {
                Ok(()) => return,
                Err(e) if e.kind() == ErrorKind::NotFound => return,
                Err(e) if attempt == 19 => {
                    tracing::warn!("cannot remove {}: {e}", self.0.display());
                }
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
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
    fn devtools_urls_are_read_from_stderr_lines() {
        assert_eq!(
            devtools_url("DevTools listening on ws://127.0.0.1:9222/devtools/browser/ab-c"),
            Some("ws://127.0.0.1:9222/devtools/browser/ab-c")
        );
        assert_eq!(
            devtools_url("[0101/x:INFO] DevTools listening on ws://[::1]:1/devtools/browser/x "),
            Some("ws://[::1]:1/devtools/browser/x")
        );
        assert_eq!(devtools_url("DevTools listening on http://x"), None);
        assert_eq!(devtools_url("[ERROR] something else"), None);
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

    #[test]
    fn file_urls_are_percent_encoded() {
        assert_eq!(
            file_url(Path::new("/tmp/a b/#deck%.html")),
            "file:///tmp/a%20b/%23deck%25.html"
        );
        assert_eq!(file_url(Path::new("/x/deck.html")), "file:///x/deck.html");
    }

    #[test]
    fn pages_are_counted_without_the_page_tree() {
        let pdf = b"%PDF-1.4\n1 0 obj\n<</Type /Pages /Count 2>>\n2 0 obj\n<</Type /Page>>\n\
                    3 0 obj\n<</Type/Page/Parent 1 0 R>>\n4 0 obj\n<</Type /PageLabel>>\n";
        assert_eq!(page_count(pdf), 2);
        assert_eq!(page_count(b"%PDF-1.4"), 0);
    }

    #[test]
    fn temporary_directories_are_private_and_removed() {
        let dir = TempDir::new("presenter-test").unwrap();
        let path = dir.path().to_path_buf();
        std::fs::create_dir_all(path.join("profile/nested")).unwrap();
        std::fs::write(path.join("profile/nested/file"), "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        let other = TempDir::new("presenter-test").unwrap();
        assert_ne!(other.path(), path);
        drop(dir);
        assert!(!path.exists());
    }

    /// [`html_to_pdf`] with a fake Chrome; retries the rare `Text file busy` of executing
    /// a script that was just written while another test thread forks.
    #[cfg(unix)]
    fn print_with(chrome: &Path, timeout: Duration) -> (String, Duration) {
        for _ in 0..5 {
            let started = Instant::now();
            let err = html_to_pdf(chrome, "<p>deck</p>", timeout).unwrap_err();
            let message = err.to_string();
            if !message.contains("Text file busy") {
                return (message, started.elapsed());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("{} stayed busy", chrome.display());
    }

    #[cfg(unix)]
    #[test]
    fn a_chrome_that_exits_reports_its_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let fake = script(
            dir.path(),
            "chrome",
            "echo 'fake chrome: no display' >&2\necho 'second line' >&2\nexit 7",
        );
        let (message, _) = print_with(&fake, Duration::from_secs(20));
        assert!(
            message.starts_with("Chrome failed to print the deck: Chrome exited (exit status: 7)"),
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
    fn an_unreachable_devtools_port_fails_fast_and_kills_chrome() {
        let dir = tempfile::tempdir().unwrap();
        // Port 1 is never Chrome; `exec` makes the kill reach the sleeping process.
        let fake = script(
            dir.path(),
            "chrome",
            "echo 'DevTools listening on ws://127.0.0.1:1/devtools/browser/x' >&2\nexec sleep 30",
        );
        let (message, elapsed) = print_with(&fake, Duration::from_secs(20));
        assert!(
            message.contains("connecting to Chrome's DevTools port: "),
            "{message}"
        );
        assert!(
            message.contains("DevTools listening on ws://127.0.0.1:1"),
            "{message}"
        );
        assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_silent_chrome_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let fake = script(dir.path(), "chrome", "exec sleep 30");
        let (message, elapsed) = print_with(&fake, Duration::from_secs(1));
        assert!(
            message
                .contains("timed out after 1 s while waiting for Chrome to open its DevTools port"),
            "{message}"
        );
        assert!(
            message.contains("Chrome wrote nothing on stderr"),
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
        assert!(
            err.to_string()
                .starts_with("cannot start /nonexistent/chrome: "),
            "{err}"
        );
    }
}
