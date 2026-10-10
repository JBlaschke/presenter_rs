//! One headless Chrome page driven over the DevTools protocol: the PDF export's machinery
//! (module docs of [`crate::pdf`]) opened up for the headless-browser smoke tests
//! (`tests/browser.rs`). Compiled only with the `browser-tests` feature, so the default
//! library's API does not grow for a test-only need.
//!
//! [`Page::launch`] starts Chrome exactly as the PDF export does (step 1 there: a private
//! temporary profile, headless, `--remote-debugging-pipe`, `--no-sandbox` as root on Linux,
//! and the cleanup watchdog), then opens one page (`Target.createTarget` with a flat
//! session), enables its `Page`, `Runtime`, `Inspector` and `Log` domains and turns on its
//! lifecycle events. [`Page::navigate`] loads a URL and waits for the `load` event of the
//! document that navigation created (the `load` lifecycle event with the navigation's
//! `loaderId`, so the load of a document the page navigated to by itself before does not
//! count), or, when that document navigates its frame again before its own `load` (a script
//! that sets `location`, a live-reload client reloading on `hello`), of the document that
//! replaced it: a newer document of the same frame that starts (`init`) after the awaited
//! one did is awaited instead. [`Page::evaluate`] evaluates a JavaScript expression in the page,
//! awaits it when it is a promise and returns its JSON value. Each call must finish within
//! the page's timeout, counted from the start of that call.
//!
//! [`Page::errors`] lists what went wrong in the page: uncaught exceptions,
//! `console.error` messages, `error` entries of the `Log` domain (a resource that failed to
//! load, for example) and dismissed JavaScript dialogs. Chrome's events are read while a
//! call waits for its reply, so an error shows up there once a later call has returned:
//! `evaluate("0")` collects everything that happened before it.
//!
//! A failed call is an [`Error::Other`]. When Chrome itself fails (it exits, closes its
//! pipe, does not answer in time, or the page crashes) the message ends with Chrome's last
//! stderr lines; when Chrome refuses a request or cannot do it (a page that cannot be
//! loaded, `Runtime.evaluate` in a page that is being replaced: `Inspected target navigated
//! or closed`) the message is just that, at once.
//!
//! A snap browser has a private `/tmp` and cannot read the system's temporary directory
//! (module docs of [`crate::pdf`], step 1): pages for it to open as `file://` URLs go into
//! [`readable_temp_dir`].
//!
//! Dropping the page asks Chrome to exit (`Browser.close`), kills it if it is still running
//! after [`CLOSE_GRACE`], and removes the profile. macOS and Linux only, like the export.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{
    Browser, CLOSE_GRACE, Cdp, Deck, DeckSource, Failure, TempDir, exception_text, placement,
    resolve_executable, string_field, system_temp_dir,
};
use crate::error::{Error, Result};

/// A headless Chrome with one open page (module docs). Dropping it stops Chrome and
/// removes its profile.
pub struct Page {
    /// The browser as given to [`Page::launch`], for error messages.
    chrome: PathBuf,
    /// The running browser. Declared before `profile`: fields are dropped in order, and
    /// Chrome must be gone before its profile directory is removed.
    browser: Browser,
    /// The temporary directory with Chrome's profile (and its watchdog), removed on drop.
    profile: TempDir,
    /// What [`Cdp`] would serve to a deck being printed: nothing here.
    nothing_served: Deck<'static>,
    /// The page's flat DevTools session.
    session: String,
    /// The last DevTools request id used.
    last_id: u64,
    /// How long each call may take.
    timeout: Duration,
    /// The page's errors collected so far.
    errors: Vec<String>,
}

/// Why a call to the page failed.
enum CallError {
    /// Chrome is fine but refused the request or could not do it; the reason.
    Refused(String),
    /// Driving Chrome failed: it exited, closed its pipe, did not answer in time, or the
    /// page crashed or was closed.
    Browser(Failure),
}

impl From<Failure> for CallError {
    fn from(failure: Failure) -> CallError {
        CallError::Browser(failure)
    }
}

/// A directory that `chrome` can read files from, for pages to open as `file://` URLs: the
/// system's temporary directory (`$TMPDIR`, `/tmp` when that is unset or empty), or for a
/// snap browser its `~/snap/<name>/common` (module docs of [`crate::pdf`], step 1). `None`
/// for a snap without that directory (one that has never run): it can read no temporary
/// directory at all; running it once creates the directory.
pub fn readable_temp_dir(chrome: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match placement(chrome, home.as_deref()) {
        (Some(dir), _) => Some(dir),
        (None, DeckSource::File) => Some(system_temp_dir(std::env::temp_dir())),
        (None, DeckSource::Served) => None,
    }
}

impl Page {
    /// Starts `chrome` headless and opens a blank page (module docs). `chrome` is resolved
    /// like `--chrome` ([`super::find_chrome`] step 1); each DevTools call, the start-up
    /// included, must finish within `timeout`.
    ///
    /// Errors (all [`Error::Other`]): `chrome` is not an executable; the temporary profile
    /// cannot be created; Chrome cannot be started, exits or does not answer in time (with
    /// the browser's path and its last stderr lines); not macOS or Linux.
    pub fn launch(chrome: &Path, timeout: Duration) -> Result<Page> {
        if cfg!(not(unix)) {
            return Err(Error::Other(
                "driving Chrome needs macOS or Linux (Chrome is started through /bin/sh)".into(),
            ));
        }
        let path_var = std::env::var_os("PATH");
        let executable = resolve_executable(chrome, path_var.as_deref()).ok_or_else(|| {
            Error::Other(format!(
                "cannot start {}: not an executable file",
                chrome.display()
            ))
        })?;
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let (base, _) = placement(chrome, home.as_deref());
        let base = base.unwrap_or_else(|| system_temp_dir(std::env::temp_dir()));
        let mut profile = TempDir::new(&base, "presenter-devtools")?;
        let browser = Browser::launch(&executable, &profile.path().join("profile"))?;
        profile.watch(browser.pid());
        tracing::debug!("started {} for a DevTools page", executable.display());
        let mut page = Page {
            chrome: chrome.to_path_buf(),
            browser,
            profile,
            nothing_served: Deck {
                url: String::new(),
                served: None,
            },
            session: String::new(),
            last_id: 0,
            timeout,
            errors: Vec::new(),
        };
        page.session = page.run("opening a page", |cdp, _| {
            open_page(cdp).map_err(CallError::from)
        })?;
        Ok(page)
    }

    /// Loads `url` in the page and waits for the `load` event of the document it creates,
    /// or of the document that replaced it before its `load` (module docs). When `url`
    /// differs from the current page's only in its fragment (`#/2`), Chrome stays on the
    /// page and fires no `load` event: then this returns as soon as Chrome has accepted the
    /// navigation.
    ///
    /// Errors: Chrome cannot load it (`net::ERR_…`; for a `file://` URL with a hint about
    /// [`readable_temp_dir`]), the page crashes, Chrome exits or the timeout passes.
    pub fn navigate(&mut self, url: &str) -> Result<()> {
        self.run("loading a page", |cdp, session| {
            // Every lifecycle event from the request on, in order: those that arrive before
            // the reply are judged once the reply names the loader.
            let mut events = Vec::new();
            let params = json!({ "url": url });
            let navigation = call(cdp, session, "Page.navigate", params, &mut |message| {
                events.extend(Lifecycle::of(message));
            })?;
            if let Some(error) = navigation.get("errorText").and_then(Value::as_str) {
                return Err(CallError::Refused(load_error(url, error)));
            }
            // A same-document navigation has no loader of its own.
            let Some(loader) = navigation.get("loaderId").and_then(Value::as_str) else {
                return Ok(());
            };
            let frame = navigation.get("frameId").and_then(Value::as_str);
            let mut awaited = LoadWait::new(frame, loader);
            let mut judged = 0;
            loop {
                for event in &events[judged..] {
                    if awaited.loaded(event) {
                        return Ok(());
                    }
                }
                judged = events.len();
                let message = cdp.next_message()?;
                events.extend(Lifecycle::of(&message));
                cdp.handle_event(&message)?;
            }
        })
    }

    /// Evaluates the JavaScript `expression` in the page, awaits its value when it is a
    /// promise, and returns that value as JSON. `undefined` is `null`; the numbers JSON
    /// cannot hold, `NaN`, `Infinity`, `-Infinity` and `-0`, and BigInts are strings in
    /// JavaScript's spelling (`"NaN"`, `"-0"`, `"1n"`); objects come back as Chrome
    /// serializes them by value (functions, DOM nodes and `Map`s as `{}`; inside an array
    /// or object, `NaN` and the like as `null`, as with `JSON.stringify`).
    ///
    /// Errors: the expression throws or its promise rejects (`JavaScript threw: ` and
    /// Chrome's description of the exception, for an `Error` the first line of its stack,
    /// for an object its class, like `Object`; for a thrown string, `null` or `undefined`,
    /// Chrome's text and the value, like `Uncaught boom`), or Chrome fails as for
    /// [`Page::navigate`].
    pub fn evaluate(&mut self, expression: &str) -> Result<Value> {
        let evaluated = self.run("evaluating JavaScript", |cdp, session| {
            let params = json!({
                "expression": expression,
                "awaitPromise": true,
                "returnByValue": true,
            });
            call(cdp, session, "Runtime.evaluate", params, &mut |_| {})
        })?;
        if let Some(details) = evaluated.get("exceptionDetails") {
            return Err(Error::Other(format!(
                "JavaScript threw: {}",
                exception_text(details)
            )));
        }
        Ok(result_value(&evaluated["result"]))
    }

    /// The page's errors so far (module docs), oldest first: `console.error: …` for a
    /// console message, the first line of an uncaught exception, a `Log` entry's text with
    /// its URL, `dismissed a JavaScript …` for a dialog.
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    /// Runs `step` on a DevTools connection to the page's browser, with the page's session
    /// and timeout, and keeps the errors the page reported meanwhile. `stage` names what is
    /// going on in error messages.
    fn run<T>(
        &mut self,
        stage: &'static str,
        step: impl FnOnce(&mut Cdp<'_>, &str) -> std::result::Result<T, CallError>,
    ) -> Result<T> {
        let deadline = Instant::now() + self.timeout;
        let mut cdp = Cdp::new(
            &mut self.browser,
            &self.nothing_served,
            deadline,
            self.timeout,
        );
        cdp.next_id = self.last_id;
        cdp.stage = stage;
        let result = step(&mut cdp, &self.session);
        self.last_id = cdp.next_id;
        self.errors.append(&mut cdp.page_errors);
        result.map_err(|error| match error {
            CallError::Refused(reason) => Error::Other(format!("Chrome (DevTools): {reason}")),
            CallError::Browser(failure) => self.error(failure),
        })
    }

    /// The error when Chrome failed: the reason, the browser and Chrome's last stderr
    /// lines.
    fn error(&self, failure: Failure) -> Error {
        let mut message = format!(
            "Chrome (DevTools) failed: {}\n  browser: {}",
            failure.describe(self.timeout),
            self.chrome.display()
        );
        let stderr = self.browser.stderr_tail();
        if stderr.is_empty() {
            message.push_str("\n  Chrome wrote nothing on stderr");
        } else {
            message.push_str(&format!(
                "\n  Chrome's stderr (last {} lines):",
                stderr.len()
            ));
            for line in stderr {
                message.push_str("\n    ");
                message.push_str(&line);
            }
        }
        Error::Other(message)
    }
}

impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page")
            .field("chrome", &self.chrome)
            .field("pid", &self.browser.pid())
            .field("profile", &self.profile.path())
            .field("timeout", &self.timeout)
            .field("errors", &self.errors)
            .finish_non_exhaustive()
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        {
            let deadline = Instant::now() + CLOSE_GRACE;
            let mut cdp = Cdp::new(
                &mut self.browser,
                &self.nothing_served,
                deadline,
                CLOSE_GRACE,
            );
            cdp.next_id = self.last_id;
            cdp.close_browser();
        }
        self.browser.stop(CLOSE_GRACE);
        // Then `browser` (already stopped) and `profile` (removed) are dropped, in order.
    }
}

/// Opens a blank page with a flat session, enables the domains whose events [`Cdp`]
/// handles and the page's lifecycle events; returns the session id.
fn open_page(cdp: &mut Cdp<'_>) -> std::result::Result<String, Failure> {
    let target = cdp.call(None, "Target.createTarget", json!({ "url": "about:blank" }))?;
    let target_id = string_field(&target, "targetId")?;
    let attach = json!({ "targetId": target_id, "flatten": true });
    let attached = cdp.call(None, "Target.attachToTarget", attach)?;
    let session = string_field(&attached, "sessionId")?;
    for enable in [
        "Page.enable",
        "Runtime.enable",
        "Inspector.enable",
        "Log.enable",
    ] {
        cdp.call(Some(&session), enable, json!({}))?;
    }
    let lifecycle = json!({ "enabled": true });
    cdp.call(Some(&session), "Page.setLifecycleEventsEnabled", lifecycle)?;
    Ok(session)
}

/// Sends `method` to the page `session` and returns its result, like [`Cdp::call`]: every
/// other message that arrives meanwhile is shown to `watch` and then handled by
/// [`Cdp::handle_event`]. An error reply is [`CallError::Refused`].
fn call(
    cdp: &mut Cdp<'_>,
    session: &str,
    method: &str,
    params: Value,
    watch: &mut dyn FnMut(&Value),
) -> std::result::Result<Value, CallError> {
    let id = cdp.send(Some(session), method, params)?;
    loop {
        let mut message = cdp.next_message()?;
        if message.get("id").and_then(Value::as_u64) != Some(id) {
            watch(&message);
            cdp.handle_event(&message)?;
            continue;
        }
        if let Some(error) = message.get("error") {
            let text = error.get("message").and_then(Value::as_str);
            let text = text.unwrap_or("unknown error");
            return Err(CallError::Refused(format!("{method}: {text}")));
        }
        return Ok(message["result"].take());
    }
}

/// One `Page.lifecycleEvent`: a document (`loader`) of a frame reached a stage (`name`:
/// `init` when it starts, `DOMContentLoaded`, `load`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Lifecycle {
    /// The frame's id.
    frame: String,
    /// The document's loader id.
    loader: String,
    /// The stage.
    name: String,
}

impl Lifecycle {
    /// The lifecycle event `message` reports, if it is one.
    fn of(message: &Value) -> Option<Lifecycle> {
        if message["method"] != "Page.lifecycleEvent" {
            return None;
        }
        let params = &message["params"];
        let field = |name: &str| params[name].as_str().map(str::to_string);
        Some(Lifecycle {
            frame: field("frameId")?,
            loader: field("loaderId")?,
            name: field("name")?,
        })
    }
}

/// The document [`Page::navigate`] waits for (module docs): at first the one its navigation
/// created; when a newer document of the same frame starts after that one did, that one.
#[derive(Debug)]
struct LoadWait {
    /// The navigated frame, if Chrome named it (else every frame counts).
    frame: Option<String>,
    /// The awaited document's loader.
    loader: String,
    /// Whether the awaited document has started (its `init` event came).
    started: bool,
}

impl LoadWait {
    /// Waits for the document `loader` of `frame`.
    fn new(frame: Option<&str>, loader: &str) -> LoadWait {
        LoadWait {
            frame: frame.map(str::to_string),
            loader: loader.to_string(),
            started: false,
        }
    }

    /// Takes the page's next lifecycle event (in the order Chrome sent them); true once
    /// the awaited document has fired `load`. Events of a document that started before
    /// the awaited one (a navigation the page made by itself earlier) do not count.
    fn loaded(&mut self, event: &Lifecycle) -> bool {
        if self
            .frame
            .as_ref()
            .is_some_and(|frame| *frame != event.frame)
        {
            return false;
        }
        match event.name.as_str() {
            "init" if event.loader == self.loader => self.started = true,
            "init" if self.started => self.loader.clone_from(&event.loader),
            "load" => return event.loader == self.loader,
            _ => {}
        }
        false
    }
}

/// The reason when Chrome cannot load `url` (`error`, e.g. `net::ERR_FILE_NOT_FOUND`); for
/// a `file://` URL with a hint about sandboxed browsers.
fn load_error(url: &str, error: &str) -> String {
    let mut reason = format!("cannot load {url}: {error}");
    if url.starts_with("file:") {
        reason.push_str(
            "\n  if the file exists, a sandboxed browser (a snap) may be unable to read its \
             directory: put it into `presenter::pdf::devtools::readable_temp_dir`",
        );
    }
    reason
}

/// The JSON value of a `Runtime.RemoteObject` returned by value (see [`Page::evaluate`]).
fn result_value(result: &Value) -> Value {
    if let Some(value) = result.get("value") {
        return value.clone();
    }
    match result.get("unserializableValue") {
        Some(Value::String(spelling)) => Value::String(spelling.clone()),
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_executable_cannot_be_launched() {
        let err =
            Page::launch(Path::new("/nonexistent/chrome"), Duration::from_secs(5)).unwrap_err();
        assert_eq!(
            err.to_string(),
            "cannot start /nonexistent/chrome: not an executable file"
        );
    }

    #[test]
    fn values_json_cannot_hold_keep_their_javascript_spelling() {
        let number = json!({ "type": "number", "value": 1.5, "description": "1.5" });
        assert_eq!(result_value(&number), json!(1.5));
        for spelling in ["NaN", "Infinity", "-Infinity", "-0", "12n"] {
            let remote = json!({ "type": "number", "unserializableValue": spelling });
            assert_eq!(result_value(&remote), json!(spelling));
        }
        assert_eq!(result_value(&json!({ "type": "undefined" })), Value::Null);
        let null = json!({ "type": "object", "subtype": "null", "value": null });
        assert_eq!(result_value(&null), Value::Null);
    }

    #[test]
    fn file_urls_that_cannot_be_loaded_mention_sandboxed_browsers() {
        assert_eq!(
            load_error("file:///tmp/x.html", "net::ERR_FILE_NOT_FOUND"),
            "cannot load file:///tmp/x.html: net::ERR_FILE_NOT_FOUND\n  if the file exists, \
             a sandboxed browser (a snap) may be unable to read its directory: put it into \
             `presenter::pdf::devtools::readable_temp_dir`"
        );
        assert_eq!(
            load_error("http://127.0.0.1:1/", "net::ERR_CONNECTION_REFUSED"),
            "cannot load http://127.0.0.1:1/: net::ERR_CONNECTION_REFUSED"
        );
    }

    /// The lifecycle event `name` of document `loader` in frame `frame`, as Chrome sends it.
    fn event(frame: &str, loader: &str, name: &str) -> Value {
        json!({
            "method": "Page.lifecycleEvent",
            "params": { "frameId": frame, "loaderId": loader, "name": name, "timestamp": 1.0 },
        })
    }

    #[test]
    fn lifecycle_events_are_read_from_their_messages() {
        assert_eq!(
            Lifecycle::of(&event("F", "B", "load")),
            Some(Lifecycle {
                frame: "F".into(),
                loader: "B".into(),
                name: "load".into()
            })
        );
        let fired = json!({ "method": "Page.loadEventFired", "params": {} });
        assert_eq!(Lifecycle::of(&fired), None);
        assert_eq!(Lifecycle::of(&json!({ "id": 3, "result": {} })), None);
    }

    /// Runs `events` (frame, loader, name) through a [`LoadWait`] for loader `A` of frame
    /// `F`; returns the index of the event it finished on.
    fn finishes_at(events: &[(&str, &str, &str)]) -> Option<usize> {
        let mut wait = LoadWait::new(Some("F"), "A");
        events.iter().position(|(frame, loader, name)| {
            wait.loaded(&Lifecycle::of(&event(frame, loader, name)).unwrap())
        })
    }

    #[test]
    fn navigate_waits_for_its_document_or_the_one_that_replaced_it() {
        // Its own document.
        let own = [
            ("F", "A", "init"),
            ("F", "A", "DOMContentLoaded"),
            ("F", "A", "load"),
        ];
        assert_eq!(finishes_at(&own), Some(2));
        // The page navigated by itself before: those events do not count.
        let stale = [
            ("F", "S", "init"),
            ("F", "S", "load"),
            ("F", "A", "init"),
            ("F", "A", "load"),
        ];
        assert_eq!(finishes_at(&stale), Some(3));
        // A script in the new document navigates again before its load (L-T24-1): the
        // replacing document's load counts, then also a further replacement's.
        let redirect = [("F", "A", "init"), ("F", "B", "init"), ("F", "B", "load")];
        assert_eq!(finishes_at(&redirect), Some(2));
        let twice = [
            ("F", "A", "init"),
            ("F", "B", "init"),
            ("F", "C", "init"),
            ("F", "B", "load"),
            ("F", "C", "load"),
        ];
        assert_eq!(finishes_at(&twice), Some(4));
        // Another frame (an iframe) does not count.
        let iframe = [
            ("F", "A", "init"),
            ("I", "X", "init"),
            ("I", "X", "load"),
            ("F", "A", "load"),
        ];
        assert_eq!(finishes_at(&iframe), Some(3));
        assert_eq!(
            finishes_at(&[("F", "A", "init"), ("F", "A", "DOMContentLoaded")]),
            None
        );
        // Without a frame id from Chrome, every frame counts.
        let mut any = LoadWait::new(None, "A");
        assert!(any.loaded(&Lifecycle::of(&event("G", "A", "load")).unwrap()));
    }
}
