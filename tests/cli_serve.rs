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

/// A running `presenter serve`; killed (and reaped) when dropped, also when a test fails.
struct Server {
    child: Child,
    /// `http://127.0.0.1:<port>/`, from the first stdout line (empty until [`Server::start`]
    /// reads it).
    url: String,
    /// Everything the child wrote on stderr so far.
    stderr: Arc<Mutex<String>>,
    /// The stdout lines not read yet ([`Server::start`] reads the first one).
    stdout_rest: mpsc::Receiver<String>,
}

impl Server {
    /// Starts `presenter serve <args>` in `dir` and waits for its first stdout line, which
    /// must be `Serving http://127.0.0.1:<port>/`.
    fn start(dir: &Path, args: &[&str]) -> Server {
        let mut command = Command::new(env!("CARGO_BIN_EXE_presenter"));
        command.arg("serve").args(args).env_remove("NO_COLOR");
        let mut server = Server::spawn(dir, command);
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

    /// Spawns `command` (the `presenter` binary with its arguments) in `dir`, without
    /// `RUST_LOG`, collecting stderr and the stdout lines; does not wait for anything.
    fn spawn(dir: &Path, mut command: Command) -> Server {
        let mut child = command
            .current_dir(dir)
            .env_remove("RUST_LOG")
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
        Server {
            child,
            url: String::new(),
            stderr,
            stdout_rest: rx,
        }
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
/// takes a while; the signal is sent once the log says the first build has started, and
/// nothing on stdout shows that it arrived before the URL line.
#[cfg(unix)]
#[test]
fn ctrl_c_during_the_first_build_exits_0() {
    let slides: String = (0..3000)
        .map(|i| format!("## Slide {i}\n\nText with *emphasis* and `code`.\n\n---\n\n"))
        .collect();
    let dir = project(&[("big.md", &slides)]);
    let mut command = Command::new(env!("CARGO_BIN_EXE_presenter"));
    command
        .args(["-v", "serve", "big.md", "--port", "0"])
        .env("NO_COLOR", "1");
    let mut server = Server::spawn(dir.path(), command);
    let start = Instant::now();
    while !server.stderr().contains("first build of big.md") {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "the first build never started:\n{}",
            server.stderr()
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(server.interrupt(), Some(0), "{}", server.stderr());
    // The child has exited, so its stdout has ended: the URL line never came.
    let stdout: Vec<String> = server.stdout_rest.iter().collect();
    assert!(
        stdout.is_empty(),
        "the signal came after the first build: {stdout:?}\n{}",
        server.stderr()
    );
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

/// Polls `/__presenter/state` like [`Server::wait_for`], for at most `timeout`; each poll
/// must be answered within 2 s (the server stays responsive while a build runs).
fn wait_for_within(server: &Server, after: u64, status: &str, timeout: Duration) -> Value {
    let start = Instant::now();
    loop {
        let asked = Instant::now();
        let state = server.state();
        assert!(
            asked.elapsed() < Duration::from_secs(2),
            "GET /__presenter/state took {:?}",
            asked.elapsed()
        );
        if build_id(&state) > after && state["status"] == status {
            return state;
        }
        assert!(
            start.elapsed() < timeout,
            "no {status} build after {after} within {timeout:?}: {state}\nstderr:\n{}",
            server.stderr()
        );
        thread::sleep(Duration::from_millis(100));
    }
}

/// A deck `a.md` using the theme `t/` (extends default) whose only style entry is
/// `styles/theme.scss`, with `scss` in it.
fn scss_project(scss: &str) -> tempfile::TempDir {
    project(&[
        (
            "t/theme.toml",
            "name = \"t\"\nextends = \"default\"\n[styles]\nentries = [\"styles/theme.scss\"]\n",
        ),
        ("t/styles/theme.scss", scss),
        ("a.md", &format!("---\ntheme: ./t\n---\n\n{}", minimal())),
    ])
}

/// Audit F02/F06 (live-reload.md §4, cli.md: rebuild errors do not stop the server): a
/// theme stylesheet that would overflow the Sass compiler's stack or never finish is a
/// style error in the overlay, while the server keeps answering; deep but finite
/// recursion compiles; fixing the file brings the deck back.
#[test]
fn runaway_stylesheets_never_stop_the_server() {
    let dir = scss_project("h1 { color: red; }\n");
    let scss = dir.path().join("t/styles/theme.scss");
    let mut server = Server::start(dir.path(), &["a.md", "--port", "0"]);
    assert_eq!(server.state()["status"], "ok", "{}", server.stderr());
    let document = server.text("/");

    // A mixin without its base case: an error, not a crash.
    write(
        &scss,
        "@mixin pad($n) { padding: $n; @include pad($n - 1); }\nbody { @include pad(3px); }\n",
    );
    let failed = server.wait_for(1, "error");
    let error = &failed["error"];
    assert_eq!(error["kind"], "style", "{failed}");
    assert_eq!(error["file"], "@t/styles/theme.scss", "{failed}");
    let message = error["message"].as_str().unwrap();
    assert!(
        message.contains("@mixin pad always includes itself"),
        "{message}"
    );
    assert_eq!(server.text("/"), document, "the last good document stays");

    // Deep but finite: 1500 nested @includes and 800 nested rules compile.
    let deep = format!(
        "@mixin pad($n) {{ @if $n > 0 {{ @include pad($n - 1); }} @else {{ .deep {{ x: y; }} }} }}\n\
         @include pad(1500);\n{}color: red;{}\n",
        "a{".repeat(800),
        "}".repeat(800)
    );
    write(&scss, &deep);
    let deep_ok = server.wait_for(build_id(&failed), "ok");
    assert!(server.text("/__presenter/theme.css").contains(".deep {"));

    // An endless loop: an error once the time limit has passed; the server answers
    // meanwhile.
    write(&scss, "$i: 0;\n@while $i < 1 { $x: 1; }\n");
    let timed_out = wait_for_within(
        &server,
        build_id(&deep_ok),
        "error",
        Duration::from_secs(40),
    );
    let message = timed_out["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("the Sass compilation did not finish within 10 s"),
        "{timed_out}"
    );

    // Fixed: ok again.
    write(&scss, "h1 { color: blue; }\n");
    let fixed = wait_for_within(&server, build_id(&timed_out), "ok", Duration::from_secs(20));
    assert_eq!(fixed["error"], Value::Null);
    assert!(
        server
            .text("/__presenter/theme.css")
            .contains("color: blue;")
    );
    assert!(
        server.child.try_wait().unwrap().is_none(),
        "the server is running"
    );
}

/// Audit F02: a recursive stylesheet present when `presenter serve` starts: the URL is
/// printed and the error overlay served (the process used to abort before both).
#[test]
fn a_recursive_stylesheet_at_startup_still_serves() {
    let dir = scss_project("@function f() { @return f(); }\nh1 { width: f(); }\n");
    let server = Server::start(dir.path(), &["a.md", "--port", "0"]);
    let state = server.state();
    assert_eq!(state["status"], "error", "{state}");
    assert_eq!(state["error"]["kind"], "style", "{state}");
    let page = server.text("/");
    assert!(page.contains("id=\"presenter-overlay\""), "{page}");
    assert!(page.contains("@function f always calls itself"), "{page}");
    assert!(server.stderr().contains("error: "), "{}", server.stderr());
}

/// Audit F03 (live-reload.md §4): in serve mode theme files carry content versions. An
/// edited image that the theme stylesheet's `url()` names is a `css` build whose
/// stylesheet names the new version; an edited image that the document loads with
/// `asset()` (the title slide's logo) is reported as a `content` build, so the client
/// swaps the slides, whose document names the new version. The new URLs serve the new
/// files.
#[test]
fn edited_theme_images_get_new_urls() {
    let dir = project(&[
        (
            "t/theme.toml",
            "name = \"t\"\nextends = \"default\"\n[styles]\nentries = [\"styles/t.css\"]\n\
             [vars]\nlogo = \"logo.svg\"\n",
        ),
        (
            "t/styles/t.css",
            ".reveal h1 { background-image: url(../assets/bg.svg); }\n",
        ),
        (
            "t/assets/bg.svg",
            "<svg xmlns=\"http://www.w3.org/2000/svg\" fill=\"red\"/>",
        ),
        (
            "t/assets/logo.svg",
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\"/>",
        ),
        ("a.md", "---\ntheme: ./t\n---\n\n# Title {layout=title}\n"),
    ]);
    let server = Server::start(dir.path(), &["a.md", "--port", "0"]);
    assert_eq!(server.state()["status"], "ok", "{}", server.stderr());
    let events = events(&server);
    assert_eq!(events.recv_timeout(REBUILD_TIMEOUT).unwrap().0, "hello");
    let bg_url = |css: &str| -> String {
        let pattern =
            Regex::new(r"url\((/__presenter/asset/@t/assets/bg\.svg\?v=[0-9]+)\)").unwrap();
        pattern.captures(css).unwrap_or_else(|| panic!("{css}"))[1].to_string()
    };
    let logo_url = |html: &str| -> String {
        let pattern =
            Regex::new(r#"<img class="logo" src="(/__presenter/asset/logo\.svg\?v=[0-9]+)""#)
                .unwrap();
        pattern.captures(html).unwrap_or_else(|| panic!("{html}"))[1].to_string()
    };
    let bg = bg_url(&server.text("/__presenter/theme.css"));
    let logo = logo_url(&server.text("/"));

    // The stylesheet's image: a css build, and the stylesheet names the new version.
    let blue = "<svg xmlns=\"http://www.w3.org/2000/svg\" fill=\"blue\"/>";
    write(&dir.path().join("t/assets/bg.svg"), blue);
    let build = next_build(&events);
    assert_eq!(
        (build["status"].as_str(), build["kind"].as_str()),
        (Some("ok"), Some("css")),
        "{build}"
    );
    let new_bg = bg_url(&server.text("/__presenter/theme.css"));
    assert_ne!(new_bg, bg);
    assert_eq!(server.text(&new_bg), blue);
    assert_eq!(
        logo_url(&server.text("/")),
        logo,
        "the document did not change"
    );

    // The document's image: reported as a content build, and the document names the new
    // version.
    let wide = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"200\"/>";
    write(&dir.path().join("t/assets/logo.svg"), wide);
    let build = next_build(&events);
    assert_eq!(
        (build["status"].as_str(), build["kind"].as_str()),
        (Some("ok"), Some("content")),
        "{build}"
    );
    let new_logo = logo_url(&server.text("/"));
    assert_ne!(new_logo, logo);
    assert_eq!(server.text(&new_logo), wide);
    server.wait_for_stderr(r"(?m)^rebuilt in \d+ ms \(content\)$", 1);
}

/// The status of `GET path` sent to `server` with the `Host` header `host` (`None`: an
/// HTTP/1.0 request without one) over a plain TCP connection (ureq sets the header
/// itself). Only the status line is read, so an event stream does not hold it.
fn status_for_host(server: &Server, path: &str, host: Option<&str>) -> u16 {
    use std::io::Write as _;
    let addr = server
        .url
        .trim_start_matches("http://")
        .trim_end_matches('/');
    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let request = match host {
        Some(host) => format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"),
        None => format!("GET {path} HTTP/1.0\r\n\r\n"),
    };
    stream.write_all(request.as_bytes()).unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    let status = line.split_whitespace().nth(1);
    status
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("status line {line:?}"))
}

/// live-reload.md §4 (audit F07): the dev server answers only requests for itself
/// (`localhost`, an IP address, with the port it listens on: the one port 0 picked), so a
/// web page that rebinds its own name to 127.0.0.1 cannot read the deck, its theme files
/// or the build events.
#[test]
fn requests_for_other_hosts_are_refused() {
    let dir = project(&[("a.md", &minimal())]);
    let server = Server::start(dir.path(), &["a.md", "--port", "0"]);
    let port: u16 = server
        .url
        .trim_end_matches('/')
        .rsplit(':')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let foreign = format!("evil.example:{port}");
    for path in [
        "/",
        "/__presenter/state",
        "/__presenter/events",
        "/__presenter/theme.css",
        "/__presenter/asset/@default/theme.toml",
        "/__presenter/asset/vendor/reveal/dist/reveal.js",
        "/__presenter/live-reload.js",
        "/no/such/page",
    ] {
        assert_eq!(
            status_for_host(&server, path, Some(&foreign)),
            403,
            "{path}"
        );
    }
    for host in [
        "evil.example".to_string(),
        "localhost".to_string(),
        format!("localhost:{}", port.wrapping_add(1)),
        format!("127.0.0.1.evil.example:{port}"),
    ] {
        assert_eq!(status_for_host(&server, "/", Some(&host)), 403, "{host}");
    }
    for host in [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("LocalHost:{port}"),
        format!("[::1]:{port}"),
    ] {
        assert_eq!(status_for_host(&server, "/", Some(&host)), 200, "{host}");
        assert_eq!(
            status_for_host(&server, "/__presenter/events", Some(&host)),
            200,
            "{host}"
        );
    }
    assert_eq!(
        status_for_host(&server, "/", None),
        200,
        "HTTP/1.0 without a Host header"
    );
    // ureq's own requests (Host 127.0.0.1:<port>) pass, as every other test shows.
    assert_eq!(server.state()["status"], "ok");
}

/// Audit F11: a front matter error is located in the deck source in `/__presenter/state`,
/// the `hello` and `build` events and the overlay page (file, line, `a.md:2:` prefix and the
/// line itself), as the terminal shows it; on the first build and on a rebuild.
#[test]
fn front_matter_errors_are_located_in_the_deck() {
    let broken = format!("---\ntitle: a: b\n---\n\n{}", minimal());
    let dir = project(&[("a.md", &broken)]);
    let server = Server::start(dir.path(), &["a.md", "--port", "0"]);
    let state = server.state();
    let error = state["error"].clone();
    assert_eq!(error["kind"], "front-matter", "{state}");
    assert_eq!(error["file"], "a.md", "{state}");
    assert_eq!(error["line"], 2, "{state}");
    assert_eq!(error["excerpt"], "title: a: b", "{state}");
    let message = error["message"].as_str().unwrap();
    assert!(message.starts_with("a.md:2: front matter: "), "{message}");
    let events = events(&server);
    let (name, hello) = events.recv_timeout(REBUILD_TIMEOUT).unwrap();
    assert_eq!(name, "hello");
    assert_eq!(hello["error"], error, "{hello}");
    let page = server.text("/");
    assert!(page.contains("a.md:2: front matter: "), "{page}");
    assert!(page.contains(">title: a: b</pre>"), "{page}");

    // Fixed, then broken again: the build event carries the same located error.
    write(&dir.path().join("a.md"), &minimal());
    assert_eq!(next_build(&events)["status"], "ok");
    write(&dir.path().join("a.md"), &broken);
    let build = next_build(&events);
    assert_eq!(build["status"], "error", "{build}");
    assert_eq!(build["error"], error, "{build}");
}

/// Audit F33: a `presenter.toml` error is located in that file: its name (relative, as the
/// terminal shows it) and line, and `toml`'s explanation without the lines it quotes, the
/// offending line being the excerpt.
#[test]
fn config_errors_are_located_in_presenter_toml() {
    let dir = project(&[
        (
            "presenter.toml",
            "[deck]\ntitle = \"x\"\n\n[slides]\nsplt = \"rules\"\n",
        ),
        ("a.md", &minimal()),
    ]);
    let server = Server::start(dir.path(), &["a.md", "--port", "0"]);
    let state = server.state();
    let error = &state["error"];
    assert_eq!(error["kind"], "config", "{state}");
    assert_eq!(error["file"], "presenter.toml", "{state}");
    assert_eq!(error["line"], 5, "{state}");
    assert_eq!(error["excerpt"], "splt = \"rules\"", "{state}");
    let message = error["message"].as_str().unwrap();
    assert!(
        message.starts_with("presenter.toml:5: unknown field `splt`, expected one of"),
        "{message}"
    );
    assert!(!message.contains(" | "), "{message}");
    let page = server.text("/");
    assert_eq!(
        page.matches("splt = &quot;rules&quot;").count() + page.matches("splt = \"rules\"").count(),
        1,
        "{page}"
    );
}
