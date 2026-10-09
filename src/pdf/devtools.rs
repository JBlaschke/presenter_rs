//! One headless Chrome page driven over the DevTools protocol: the PDF export's machinery
//! (module docs of [`crate::pdf`]) opened up for the headless-browser smoke tests
//! (`tests/browser.rs`, feature `browser-tests`) and for tools that need to look at a
//! rendered deck.
//!
//! [`Page::launch`] starts Chrome exactly as the PDF export does (step 1 there: a private
//! temporary profile, headless, `--remote-debugging-pipe`, `--no-sandbox` as root on Linux,
//! and the cleanup watchdog), then opens one page (`Target.createTarget` with a flat
//! session) and enables its `Page`, `Runtime`, `Inspector` and `Log` domains.
//! [`Page::navigate`] loads a URL and waits for its `load` event; [`Page::evaluate`]
//! evaluates a JavaScript expression in the page, awaits it when it is a promise and
//! returns its JSON value. Each call must finish within the page's timeout, counted from
//! the start of that call.
//!
//! [`Page::errors`] lists what went wrong in the page: uncaught exceptions,
//! `console.error` messages, `error` entries of the `Log` domain (a resource that failed to
//! load, for example) and dismissed JavaScript dialogs. Chrome's events are read while a
//! call waits for its reply, so an error shows up there once a later call has returned:
//! `evaluate("0")` collects everything that happened before it.
//!
//! Dropping the page asks Chrome to exit (`Browser.close`), kills it if it is still running
//! after [`CLOSE_GRACE`], and removes the profile. macOS and Linux only, like the export.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{
    Browser, CLOSE_GRACE, Cdp, Deck, Failure, TempDir, exception_text, placement,
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
        page.session = page.run("opening a page", |cdp, _| open_page(cdp))?;
        Ok(page)
    }

    /// Loads `url` in the page and waits for its `load` event. When `url` differs from the
    /// current page's only in its fragment (`#/2`), Chrome stays on the page and fires no
    /// `load` event: then this returns as soon as Chrome has accepted the navigation.
    ///
    /// Errors: Chrome cannot load it (`net::ERR_…`), the page crashes, Chrome exits or the
    /// timeout passes.
    pub fn navigate(&mut self, url: &str) -> Result<()> {
        self.run("loading a page", |cdp, session| {
            let navigation = cdp.call(Some(session), "Page.navigate", json!({ "url": url }))?;
            if let Some(error) = navigation.get("errorText").and_then(Value::as_str) {
                return Err(Failure::Other(format!("cannot load {url}: {error}")));
            }
            // A same-document navigation has no loader of its own.
            if navigation.get("loaderId").is_none() {
                return Ok(());
            }
            while !cdp.loaded {
                let message = cdp.next_message()?;
                cdp.handle_event(&message)?;
            }
            Ok(())
        })
    }

    /// Evaluates the JavaScript `expression` in the page, awaits its value when it is a
    /// promise, and returns that value as JSON (`null` for `undefined`; values that JSON
    /// cannot hold, like DOM nodes, come back as Chrome serializes them by value).
    ///
    /// Errors: the expression throws or its promise rejects (the error's first line), or
    /// Chrome fails as for [`Page::navigate`].
    pub fn evaluate(&mut self, expression: &str) -> Result<Value> {
        let evaluated = self.run("evaluating JavaScript", |cdp, session| {
            let params = json!({
                "expression": expression,
                "awaitPromise": true,
                "returnByValue": true,
            });
            cdp.call(Some(session), "Runtime.evaluate", params)
        })?;
        if let Some(details) = evaluated.get("exceptionDetails") {
            return Err(Error::Other(format!(
                "JavaScript threw: {}",
                exception_text(details)
            )));
        }
        Ok(evaluated["result"]
            .get("value")
            .cloned()
            .unwrap_or(Value::Null))
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
        step: impl FnOnce(&mut Cdp<'_>, &str) -> std::result::Result<T, Failure>,
    ) -> Result<T> {
        let deadline = Instant::now() + self.timeout;
        let mut cdp = Cdp::new(&mut self.browser, &self.nothing_served, deadline);
        cdp.next_id = self.last_id;
        cdp.stage = stage;
        let result = step(&mut cdp, &self.session);
        self.last_id = cdp.next_id;
        self.errors.append(&mut cdp.page_errors);
        result.map_err(|failure| self.error(failure))
    }

    /// The error for a failed call: the reason, the browser and Chrome's last stderr lines.
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
            let mut cdp = Cdp::new(&mut self.browser, &self.nothing_served, deadline);
            cdp.next_id = self.last_id;
            cdp.close_browser();
        }
        self.browser.stop(CLOSE_GRACE);
        // Then `browser` (already stopped) and `profile` (removed) are dropped, in order.
    }
}

/// Opens a blank page with a flat session and enables the domains whose events [`Cdp`]
/// handles; returns the session id.
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
    Ok(session)
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
}
