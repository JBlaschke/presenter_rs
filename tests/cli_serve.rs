//! T23 end to end: the `presenter serve` binary (`docs/spec/cli.md` serve,
//! `docs/spec/live-reload.md` §4–§5). Each test copies a deck into a temporary directory,
//! starts `presenter serve … --port 0` as a child process (killed when the test ends), reads
//! the URL from the first stdout line and talks to the server over HTTP. `--open` is not
//! exercised (it would start a browser).

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::Value;

/// How long a rebuild may take to show up in `/__presenter/state` (task acceptance: 10 s).
const REBUILD_TIMEOUT: Duration = Duration::from_secs(10);

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(rel: &str) -> PathBuf {
    root().join("tests/fixtures").join(rel)
}

/// The text of `tests/fixtures/md/minimal.md` (one slide).
fn minimal() -> String {
    std::fs::read_to_string(fixture("md/minimal.md")).unwrap()
}

/// A temporary directory with `files` (path, contents) written into it.
fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, contents) in files {
        write(&dir.path().join(path), contents);
    }
    dir
}

/// Writes `contents` to `path`, creating its directory.
fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// A running `presenter serve`; killed (and reaped) when dropped.
struct Server {
    child: Child,
    /// `http://127.0.0.1:<port>/`, from the first stdout line.
    url: String,
    /// Everything the child wrote on stderr so far.
    stderr: Arc<Mutex<String>>,
    /// The stdout lines after the first one.
    stdout_rest: mpsc::Receiver<String>,
}

impl Server {
    /// Starts `presenter serve <args>` in `dir` and waits for its first stdout line, which
    /// must be `Serving http://127.0.0.1:<port>/`.
    fn start(dir: &Path, args: &[&str]) -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_presenter"))
            .arg("serve")
            .args(args)
            .current_dir(dir)
            .env_remove("RUST_LOG")
            .env_remove("NO_COLOR")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&stderr);
        let mut err = child.stderr.take().unwrap();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = err.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        let (tx, rx) = mpsc::channel();
        let out = child.stdout.take().unwrap();
        thread::spawn(move || {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut server = Server {
            child,
            url: String::new(),
            stderr,
            stdout_rest: rx,
        };
        let first = server
            .stdout_rest
            .recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("no URL on stdout; stderr:\n{}", server.stderr()));
        let pattern = Regex::new(r"^Serving (http://127\.0\.0\.1:[1-9][0-9]*/)$").unwrap();
        let captures = pattern
            .captures(&first)
            .unwrap_or_else(|| panic!("first stdout line: {first:?}"));
        server.url = captures[1].to_string();
        server
    }

    fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    /// Waits until `pattern` matches stderr at least `count` times; returns stderr. The
    /// server writes a rebuild's line after it stores the build, so a test that saw the
    /// build in `/__presenter/state` must wait for the line. Fails after [`REBUILD_TIMEOUT`].
    fn wait_for_stderr(&self, pattern: &str, count: usize) -> String {
        let pattern = Regex::new(pattern).unwrap();
        let start = Instant::now();
        loop {
            let stderr = self.stderr();
            if pattern.find_iter(&stderr).count() >= count {
                return stderr;
            }
            assert!(
                start.elapsed() < REBUILD_TIMEOUT,
                "{count} × {pattern} not on stderr within {REBUILD_TIMEOUT:?}:\n{stderr}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Sends SIGINT and waits (at most 10 s) for the child to exit; returns its exit code.
    #[cfg(unix)]
    fn interrupt(&mut self) -> Option<i32> {
        interrupt(&mut self.child, &self.stderr)
    }

    /// `GET path`: status, `Content-Type` and body (as text, lossily).
    fn get(&self, path: &str) -> (u16, String, String) {
        let url = format!("{}{}", self.url, path.trim_start_matches('/'));
        let mut response = agent(Duration::from_secs(10)).get(&url).call().unwrap();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = response.body_mut().read_to_vec().unwrap();
        let body = String::from_utf8_lossy(&body).into_owned();
        (response.status().as_u16(), content_type, body)
    }

    /// `GET path`, which must be `200 OK`, as text.
    fn text(&self, path: &str) -> String {
        let (status, _, body) = self.get(path);
        assert_eq!(status, 200, "GET {path}: {body}");
        body
    }

    /// `GET /__presenter/state`.
    fn state(&self) -> Value {
        serde_json::from_str(&self.text("/__presenter/state")).unwrap()
    }

    /// Polls `/__presenter/state` until a build after `after` matches `status` (`ok` or
    /// `error`); returns that state. Fails after [`REBUILD_TIMEOUT`].
    fn wait_for(&self, after: u64, status: &str) -> Value {
        let start = Instant::now();
        loop {
            let state = self.state();
            if build_id(&state) > after && state["status"] == status {
                return state;
            }
            assert!(
                start.elapsed() < REBUILD_TIMEOUT,
                "no {status} build after {after} within {REBUILD_TIMEOUT:?}: {state}\nstderr:\n{}",
                self.stderr()
            );
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A ureq agent that returns error statuses as responses, ignores proxy settings and gives
/// up after `timeout` (the whole call, body included).
fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .proxy(None)
        .timeout_global(Some(timeout))
        .build()
        .into()
}

fn build_id(state: &Value) -> u64 {
    state["build_id"].as_u64().unwrap()
}

/// The number of `<section` tags in `html`.
fn sections(html: &str) -> usize {
    html.matches("<section").count()
}

/// The task's acceptance test: serve a copy of `minimal.md`, add a slide, break the theme
/// in the front matter, fix it; the server keeps serving throughout.
#[test]
fn serve_rebuilds_on_change_and_survives_errors() {
    let dir = project(&[("a.md", &minimal())]);
    let deck = dir.path().join("a.md");
    let server = Server::start(dir.path(), &["a.md", "--port", "0"]);
    let first = server.state();
    assert_eq!(first["build_id"], 1, "{first}");
    assert_eq!(first["status"], "ok", "{first}");
    assert_eq!(first["error"], Value::Null);
    let (status, content_type, html) = server.get("/");
    assert_eq!(status, 200);
    assert_eq!(content_type, "text/html; charset=utf-8");
    assert!(html.contains("/__presenter/live-reload.js"), "{html}");
    let before = sections(&html);
    assert!(before >= 1);

    // Append a slide: one more <section> once the rebuild is in.
    write(&deck, &format!("{}\n---\n\n# Second\n", minimal()));
    let ok = server.wait_for(1, "ok");
    assert_eq!(sections(&server.text("/")), before + 1);

    // A theme that does not exist: the build fails, the last good document stays.
    let broken = format!(
        "---\ntheme: ./no-such-theme\n---\n\n{}\n---\n\n# Second\n",
        minimal()
    );
    write(&deck, &broken);
    let failed = server.wait_for(build_id(&ok), "error");
    let message = failed["error"]["message"].as_str().unwrap();
    assert!(message.contains("no-such-theme"), "{failed}");
    assert_eq!(failed["error"]["kind"], "theme", "{failed}");
    assert_eq!(sections(&server.text("/")), before + 1);

    // Fixed: ok again.
    write(
        &deck,
        &format!("{}\n---\n\n# Second\n\n---\n\n# Third\n", minimal()),
    );
    let fixed = server.wait_for(build_id(&failed), "ok");
    assert_eq!(fixed["error"], Value::Null);
    assert_eq!(sections(&server.text("/")), before + 2);

    // One line per rebuild on stderr: the timing, or the error.
    let stderr = server.wait_for_stderr(r"(?m)^rebuilt in \d+ ms \(content\)$", 2);
    assert!(
        stderr
            .lines()
            .any(|line| line.starts_with("error: ") && line.contains("no-such-theme")),
        "{stderr}"
    );
}

/// The child-theme fixture in `dir/theme`, the deck `dir/a.md` using it and a
/// `presenter.toml` whose `[serve] port = 0` replaces `--port`.
fn themed_project() -> tempfile::TempDir {
    let theme = fixture("themes/child-theme");
    let read = |rel: &str| std::fs::read_to_string(theme.join(rel)).unwrap();
    project(&[
        ("theme/theme.toml", &read("theme.toml")),
        ("theme/styles/child.css", &read("styles/child.css")),
        (
            "theme/templates/layouts/title.html",
            &read("templates/layouts/title.html"),
        ),
        (
            "presenter.toml",
            "[deck]\ntheme = \"theme\"\n\n[serve]\nport = 0\n",
        ),
        ("a.md", &minimal()),
    ])
}

/// Reads server-sent events from `GET /__presenter/events` on a thread and hands each
/// `(event, data)` to the returned channel.
fn events(server: &Server) -> mpsc::Receiver<(String, Value)> {
    let url = format!("{}__presenter/events", server.url);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let response = agent(Duration::from_secs(60)).get(&url).call().unwrap();
        let mut reader = BufReader::new(response.into_body().into_reader());
        let mut event = String::new();
        let mut line = String::new();
        while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
            let text = line.trim_end();
            if let Some(name) = text.strip_prefix("event: ") {
                event = name.to_string();
            } else if let Some(data) = text.strip_prefix("data: ") {
                let data = serde_json::from_str(data).unwrap();
                if tx.send((std::mem::take(&mut event), data)).is_err() {
                    break;
                }
            }
            line.clear();
        }
    });
    rx
}

/// The next `build` event (other events are skipped).
fn next_build(events: &mpsc::Receiver<(String, Value)>) -> Value {
    loop {
        let (name, data) = events.recv_timeout(REBUILD_TIMEOUT).expect("a build event");
        if name == "build" {
            return data;
        }
    }
}

/// live-reload.md §2: a theme stylesheet change is a `css` build, a template or deck
/// change a `content` build; each event carries the build's duration.
#[test]
fn builds_are_broadcast_as_css_or_content() {
    let dir = themed_project();
    let server = Server::start(dir.path(), &["a.md"]);
    assert_eq!(server.state()["status"], "ok", "{}", server.stderr());
    let events = events(&server);
    let (name, hello) = events.recv_timeout(REBUILD_TIMEOUT).unwrap();
    assert_eq!(name, "hello");
    assert_eq!(hello["build_id"], 1, "{hello}");

    let css = dir.path().join("theme/styles/child.css");
    let mut text = std::fs::read_to_string(&css).unwrap();
    text.push_str("\n.reveal h1 { letter-spacing: 1px; }\n");
    write(&css, &text);
    let build = next_build(&events);
    assert_eq!(build["status"], "ok", "{build}");
    assert_eq!(build["kind"], "css", "{build}");
    assert!(build["duration_ms"].is_u64(), "{build}");
    assert!(
        server
            .text("/__presenter/theme.css")
            .contains("letter-spacing: 1px")
    );

    let layout = dir.path().join("theme/templates/layouts/title.html");
    let mut text = std::fs::read_to_string(&layout).unwrap();
    text.push_str("\n{# edited #}\n");
    write(&layout, &text);
    let build = next_build(&events);
    assert_eq!(build["kind"], "content", "{build}");

    write(
        &dir.path().join("a.md"),
        &format!("{}\n---\n\n# More\n", minimal()),
    );
    let build = next_build(&events);
    assert_eq!(build["kind"], "content", "{build}");
    assert_eq!(build["status"], "ok", "{build}");
    server.wait_for_stderr(r"(?m)^rebuilt in \d+ ms \(css\)$", 1);
}

/// An edited image is a content build, and its URL in the document changes (a browser that
/// swaps the slides in place would otherwise keep the old image); the new URL serves the
/// new bytes.
#[test]
fn an_edited_image_gets_a_new_url() {
    let deck = format!("{}\n![plot](fig/p.png)\n", minimal());
    let dir = project(&[("a.md", &deck)]);
    let image = dir.path().join("fig/p.png");
    std::fs::create_dir_all(image.parent().unwrap()).unwrap();
    std::fs::write(&image, b"\x89PNG first").unwrap();
    let server = Server::start(dir.path(), &["a.md", "--port", "0"]);
    let url = Regex::new(r#"src="(/__presenter/deck-asset/[0-9a-f]{12}\?v=[0-9]+)""#).unwrap();
    let html = server.text("/");
    let before = url.captures(&html).expect("a versioned image URL")[1].to_string();
    let (status, content_type, _) = server.get(&before);
    assert_eq!((status, content_type.as_str()), (200, "image/png"));

    std::fs::write(&image, b"\x89PNG second, longer").unwrap();
    server.wait_for(1, "ok");
    let html = server.text("/");
    let after = url.captures(&html).expect("a versioned image URL")[1].to_string();
    assert_ne!(after, before);
    assert_eq!(
        after.split('?').next(),
        before.split('?').next(),
        "same asset id"
    );
    let response = agent(Duration::from_secs(10))
        .get(&format!("{}{}", server.url, after.trim_start_matches('/')))
        .call()
        .unwrap()
        .into_body()
        .read_to_vec()
        .unwrap();
    assert_eq!(response, b"\x89PNG second, longer");
    server.wait_for_stderr(r"(?m)^rebuilt in \d+ ms \(content\)$", 1);
}

/// A first build that fails still starts the server (with the error page), and its inputs
/// are watched: creating the missing theme fixes it.
#[test]
fn a_failed_first_build_still_serves_and_is_watched() {
    let deck = format!("---\ntheme: mytheme\n---\n\n{}", minimal());
    let dir = project(&[("a.md", &deck)]);
    let server = Server::start(dir.path(), &["a.md", "--port", "0"]);
    let state = server.state();
    assert_eq!(state["build_id"], 1, "{state}");
    assert_eq!(state["status"], "error", "{state}");
    let page = server.text("/");
    assert!(page.contains("id=\"presenter-overlay\""), "{page}");
    assert!(page.contains("mytheme"), "{page}");
    assert!(server.stderr().contains("error: "), "{}", server.stderr());

    write(
        &dir.path().join("mytheme/theme.toml"),
        "name = \"mytheme\"\nextends = \"default\"\n",
    );
    server.wait_for(1, "ok");
    let html = server.text("/");
    assert!(html.contains("class=\"reveal\""), "{html}");
    assert!(html.contains("Hello, presenter"), "{html}");
}

/// cli.md: `--site-preview` serves the site layout (relative `assets/` URLs, no live-reload
/// client) and still rebuilds; `-q` keeps the URL line but drops the rebuild lines.
#[test]
fn site_preview_serves_the_site_layout() {
    let dir = project(&[("a.md", &minimal())]);
    let server = Server::start(dir.path(), &["-q", "a.md", "--port", "0", "--site-preview"]);
    let html = server.text("/");
    assert!(!html.contains("live-reload.js"), "{html}");
    assert!(!html.contains("/__presenter/"), "{html}");
    let theme = Regex::new(r#"href="(assets/theme-[0-9a-f]+\.css)""#).unwrap();
    let href = theme.captures(&html).expect("a site theme stylesheet")[1].to_string();
    let (status, content_type, css) = server.get(&href);
    assert_eq!(status, 200);
    assert_eq!(content_type, "text/css");
    assert!(css.contains(".reveal"), "{css}");
    let reveal = Regex::new(r#"src="(assets/reveal-[0-9a-f]+\.js)""#).unwrap();
    let script = reveal.captures(&html).expect("reveal.js")[1].to_string();
    assert_eq!(server.get(&script).0, 200);
    assert_eq!(server.get("/assets/nope-0000.css").0, 404);

    write(
        &dir.path().join("a.md"),
        &format!("{}\n---\n\n# Second\n", minimal()),
    );
    server.wait_for(1, "ok");
    assert_eq!(sections(&server.text("/")), sections(&html) + 1);
    assert!(
        !server.stderr().contains("rebuilt in"),
        "{}",
        server.stderr()
    );
}

/// live-reload.md §4: a port that cannot be bound ends the command with exit 1 and the
/// hint to use `--port 0`; nothing is printed on stdout. Also when the other server listens
/// on the wildcard address, which macOS would otherwise let presenter shadow.
#[test]
fn a_port_in_use_exits_1_with_a_hint() {
    let dir = project(&[("a.md", &minimal())]);
    for holder in ["127.0.0.1:0", "0.0.0.0:0"] {
        let taken = std::net::TcpListener::bind(holder).unwrap();
        let port = taken.local_addr().unwrap().port().to_string();
        let output = assert_cmd::Command::new(env!("CARGO_BIN_EXE_presenter"))
            .args(["serve", "a.md", "--port", &port])
            .current_dir(dir.path())
            .env_remove("RUST_LOG")
            .timeout(Duration::from_secs(60))
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{holder}: {stderr}");
        assert!(output.stdout.is_empty(), "{holder}: {:?}", output.stdout);
        assert!(stderr.contains(&format!("127.0.0.1:{port}")), "{stderr}");
        assert!(stderr.contains("--port 0"), "{stderr}");
    }
}

/// An input whose format cannot be told is a usage error (exit 2) before anything starts.
#[test]
fn an_unknown_input_format_exits_2() {
    let dir = project(&[("deck.txt", "# Hi\n")]);
    let output = assert_cmd::Command::new(env!("CARGO_BIN_EXE_presenter"))
        .args(["serve", "deck.txt", "--port", "0"])
        .current_dir(dir.path())
        .timeout(Duration::from_secs(60))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}

/// Sends SIGINT to `child` and waits (at most 10 s) for it to exit; returns its exit code
/// (`None`: ended by a signal).
#[cfg(unix)]
fn interrupt(child: &mut Child, stderr: &Mutex<String>) -> Option<i32> {
    let pid = child.id().to_string();
    let kill = Command::new("kill").args(["-INT", &pid]).status().unwrap();
    assert!(kill.success());
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status.code();
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "still running after SIGINT; stderr:\n{}",
            stderr.lock().unwrap()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// cli.md: Ctrl-C (SIGINT) exits 0, also with a browser connected to the event stream.
#[cfg(unix)]
#[test]
fn ctrl_c_exits_0() {
    let dir = project(&[("a.md", &minimal())]);
    let mut server = Server::start(dir.path(), &["a.md", "--port", "0"]);
    let events = events(&server);
    assert_eq!(events.recv_timeout(REBUILD_TIMEOUT).unwrap().0, "hello");
    assert_eq!(server.interrupt(), Some(0), "{}", server.stderr());
    // The child has exited: its stdout ends, and nothing followed the URL.
    let rest: Vec<String> = server.stdout_rest.iter().collect();
    assert!(rest.is_empty(), "only the URL on stdout: {rest:?}");
}

/// cli.md: Ctrl-C exits 0 also during the first build (before the URL is printed): the
/// handler is installed before it starts. The deck is large enough that a debug build of it
/// takes a while; the signal is sent once the log says the first build has started.
#[cfg(unix)]
#[test]
fn ctrl_c_during_the_first_build_exits_0() {
    let slides: String = (0..3000)
        .map(|i| format!("## Slide {i}\n\nText with *emphasis* and `code`.\n\n---\n\n"))
        .collect();
    let dir = project(&[("big.md", &slides)]);
    let mut child = Command::new(env!("CARGO_BIN_EXE_presenter"))
        .args(["-v", "serve", "big.md", "--port", "0"])
        .current_dir(dir.path())
        .env_remove("RUST_LOG")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&stderr);
    let mut err = child.stderr.take().unwrap();
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = err.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    });
    let start = Instant::now();
    while !stderr.lock().unwrap().contains("first build of big.md") {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "the first build never started:\n{}",
            stderr.lock().unwrap()
        );
        thread::sleep(Duration::from_millis(5));
    }
    let code = interrupt(&mut child, &stderr);
    assert_eq!(code, Some(0), "{}", stderr.lock().unwrap());
}

/// A `presenter.toml` that cannot be loaded at start: the server still starts, on the
/// default `[serve]` settings with the command line applied, and says so on stderr. Once
/// fixed, a `[serve]` address other than the one in use gets a note that a restart is
/// needed.
#[test]
fn an_unreadable_config_is_reported_and_serve_changes_need_a_restart() {
    let dir = project(&[
        ("presenter.toml", "[deck]\nlang = 5\n"),
        ("a.md", &minimal()),
    ]);
    let server = Server::start(dir.path(), &["a.md", "--port", "0"]);
    assert_eq!(server.state()["status"], "error");
    let stderr = server.wait_for_stderr(r"(?m)^warning: the \[serve\] settings cannot be read", 1);
    assert!(
        stderr.contains("listens on 127.0.0.1:0 until it is restarted"),
        "{stderr}"
    );

    write(
        &dir.path().join("presenter.toml"),
        "[serve]\nhost = \"localhost\"\n",
    );
    server.wait_for(1, "ok");
    let stderr = server.wait_for_stderr(r"presenter\.toml: warning: \[serve\] now asks for", 1);
    assert!(
        stderr.contains(
            "[serve] now asks for localhost:0, but the server was started with 127.0.0.1:0: \
             restart presenter serve to apply it"
        ),
        "{stderr}"
    );
}
