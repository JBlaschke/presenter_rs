//! Dev server integration tests (T21): `docs/spec/live-reload.md` §1, §2 and §4.
//!
//! Each test builds a deck in serve mode, stores it in a `ServeState`, serves it on
//! `127.0.0.1:0` from a background runtime and talks to it over HTTP with `ureq`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use presenter::assets::emit::OutputMode;
use presenter::error::Error;
use presenter::pipeline::{Build, BuildOptions, build_deck};
use presenter::render::BuildInfo;
use presenter::serve::events::BuildEvent;
use presenter::serve::server::{self, ServeState, Shared};
use presenter::serve::watch::ChangeKind;
use serde_json::{Value, json};
use tokio::sync::{broadcast, oneshot};

/// A dev server running on its own runtime thread. Dropping it shuts the server down.
struct TestServer {
    addr: SocketAddr,
    state: Shared,
    events: broadcast::Sender<BuildEvent>,
    shutdown: Option<oneshot::Sender<()>>,
    /// Receives `serve_until`'s result when it returns.
    done: mpsc::Receiver<presenter::Result<()>>,
}

impl TestServer {
    fn start(state: Shared) -> TestServer {
        let events = server::event_channel();
        let router = server::router(Arc::clone(&state), events.clone());
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let (addr_tx, addr_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let result = runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();
                // Dropping the sender (or sending) ends the server.
                let shutdown = async move {
                    let _ = shutdown_rx.await;
                };
                server::serve_until(listener, router, shutdown).await
            });
            let _ = done_tx.send(result);
        });
        let addr = addr_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        TestServer {
            addr,
            state,
            events,
            shutdown: Some(shutdown_tx),
            done: done_rx,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// `GET path`, any status.
    fn get(&self, path: &str) -> Response {
        let mut response = agent(Duration::from_secs(10))
            .get(&self.url(path))
            .call()
            .unwrap_or_else(|err| panic!("GET {path}: {err}"));
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string()
        };
        let (content_type, cache_control) = (header("content-type"), header("cache-control"));
        let body = response
            .body_mut()
            .with_config()
            .limit(64 * 1024 * 1024)
            .read_to_vec()
            .unwrap();
        Response {
            status: response.status().as_u16(),
            content_type,
            cache_control,
            body,
        }
    }

    /// `GET path`, which must be `200 OK`, as text.
    fn text(&self, path: &str) -> String {
        let response = self.get(path);
        assert_eq!(response.status, 200, "GET {path}");
        String::from_utf8(response.body).unwrap()
    }
}

struct Response {
    status: u16,
    content_type: String,
    cache_control: String,
    body: Vec<u8>,
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

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel)
}

/// `input` built for serve mode as build `id`.
fn serve_build(input: &Path, id: u64) -> Build {
    build_deck(&BuildOptions {
        output: OutputMode::Serve,
        build: BuildInfo::serve(id),
        missing_asset_is_error: false,
        ..BuildOptions::new(input)
    })
    .unwrap()
}

/// A state holding `input`'s serve-mode build (build 1).
fn state_with(input: &Path) -> (Shared, Build) {
    let state: Shared = Arc::new(RwLock::new(ServeState::default()));
    let build = serve_build(input, server::next_build_id(&state));
    assert_eq!(server::apply_build(&state, &build), 1);
    (state, build)
}

/// Every distinct `/__presenter/…` URL in `attr="…"` attributes of `html`.
fn presenter_urls(html: &str) -> Vec<String> {
    let mut urls: Vec<String> = ["src=\"", "href=\""]
        .iter()
        .flat_map(|attr| html.split(attr).skip(1))
        .filter_map(|rest| rest.split('"').next())
        .filter(|url| url.starts_with("/__presenter/"))
        .map(str::to_string)
        .collect();
    urls.sort();
    urls.dedup();
    urls
}

/// Every `url(/__presenter/…)` in `css`, without its `?query#fragment`.
fn css_urls(css: &str) -> Vec<String> {
    let mut urls: Vec<String> = css
        .split("url(")
        .skip(1)
        .filter_map(|rest| rest.split(')').next())
        .map(|url| url.trim_matches(|c| c == '"' || c == '\''))
        .filter(|url| url.starts_with("/__presenter/"))
        .map(|url| url.split(['?', '#']).next().unwrap_or(url).to_string())
        .collect();
    urls.sort();
    urls.dedup();
    urls
}

#[test]
fn serves_the_document_theme_css_assets_and_state() {
    let (state, build) = state_with(&fixture("md/minimal.md"));
    let server = TestServer::start(state);

    let index = server.get("/");
    assert_eq!(index.status, 200);
    assert_eq!(index.content_type, "text/html; charset=utf-8");
    assert_eq!(index.cache_control, "no-store");
    let html = String::from_utf8(index.body).unwrap();
    assert!(html.contains(r#"<div class="slides">"#), "{html}");
    assert_eq!(html, build.document.html);
    assert!(html.contains(r#"<script src="/__presenter/live-reload.js"></script>"#));
    assert!(html.contains(r#"href="/__presenter/theme.css?v=1""#));

    let css = server.get("/__presenter/theme.css?v=1");
    assert_eq!(css.status, 200);
    assert!(
        css.content_type.starts_with("text/css"),
        "{}",
        css.content_type
    );
    assert_eq!(css.cache_control, "no-store");
    assert_eq!(css.body, build.document.theme_css.as_bytes());

    let reveal = server.get("/__presenter/asset/vendor/reveal/dist/reveal.js");
    assert_eq!(reveal.status, 200);
    assert!(
        reveal.body.len() > 100 * 1024,
        "{} bytes",
        reveal.body.len()
    );
    assert!(reveal.content_type.starts_with("text/javascript"));
    assert_eq!(reveal.cache_control, "max-age=3600");

    let missing = server.get("/__presenter/asset/vendor/does-not-exist.js");
    assert_eq!(missing.status, 404);

    let status = server.get("/__presenter/state");
    assert_eq!(status.status, 200);
    assert!(status.content_type.starts_with("application/json"));
    let status: Value = serde_json::from_slice(&status.body).unwrap();
    assert_eq!(
        status,
        json!({"build_id": 1, "status": "ok", "error": null})
    );

    let client = server.get("/__presenter/live-reload.js");
    assert_eq!(client.status, 200);
    assert!(client.content_type.starts_with("text/javascript"));
    assert_eq!(client.cache_control, "no-store");
    let on_disk = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/presenter/live-reload.js"),
    )
    .unwrap();
    assert_eq!(client.body, on_disk);

    assert_eq!(server.get("/favicon.ico").status, 204);
    assert_eq!(server.get("/nope").status, 404);
    assert_eq!(
        server.get("/__presenter/deck-asset/000000000000").status,
        404
    );
}

#[test]
fn every_url_of_the_served_document_and_stylesheet_resolves() {
    let (state, build) = state_with(&fixture("md/math.md"));
    let server = TestServer::start(state);
    let html = server.text("/");
    let urls = presenter_urls(&html);
    assert!(
        urls.iter()
            .any(|url| url.starts_with("/__presenter/asset/vendor/")),
        "{urls:?}"
    );
    let css_urls = css_urls(&build.document.theme_css);
    for url in urls.iter().chain(&css_urls) {
        let response = server.get(url);
        assert_eq!(response.status, 200, "GET {url}");
        assert!(!response.body.is_empty(), "GET {url}");
    }
    // KaTeX's stylesheet refers to its fonts relatively; the browser asks for them here.
    let katex = urls
        .iter()
        .find(|url| url.ends_with("/katex.min.css"))
        .expect("math.md loads KaTeX");
    let base = katex.rsplit_once('/').unwrap().0;
    // Only the woff2 fonts are vendored (vendored-assets.md); browsers pick them first.
    let fonts: Vec<String> = css_urls_relative(&server.text(katex))
        .into_iter()
        .filter(|font| font.ends_with(".woff2"))
        .collect();
    assert!(!fonts.is_empty());
    for font in fonts {
        let url = format!("{base}/{font}");
        let response = server.get(&url);
        assert_eq!(response.status, 200, "GET {url}");
        assert_eq!(response.content_type, "font/woff2", "GET {url}");
    }
}

/// The relative `url(…)`s of a stylesheet, without their `?query#fragment`.
fn css_urls_relative(css: &str) -> Vec<String> {
    css.split("url(")
        .skip(1)
        .filter_map(|rest| rest.split(')').next())
        .map(|url| url.trim_matches(|c| c == '"' || c == '\''))
        .filter(|url| !url.starts_with("data:") && !url.contains("//"))
        .map(|url| url.split(['?', '#']).next().unwrap_or(url).to_string())
        .collect()
}

#[test]
fn deck_assets_are_served_by_id() {
    let (state, build) = state_with(&fixture("md/images.md"));
    let server = TestServer::start(state);
    let html = server.text("/");
    let urls: Vec<String> = presenter_urls(&html)
        .into_iter()
        .filter(|url| url.starts_with("/__presenter/deck-asset/"))
        .collect();
    assert_eq!(urls.len(), 1, "{urls:?}");
    let png = server.get(&urls[0]);
    assert_eq!(png.status, 200);
    assert_eq!(png.content_type, "image/png");
    assert_eq!(png.cache_control, "no-store");
    assert_eq!(
        png.body,
        std::fs::read(fixture("md/images/ok.png")).unwrap()
    );
    let id = urls[0].rsplit('/').next().unwrap();
    assert!(build.assets.get(id).is_some());
    assert_eq!(
        server.get("/__presenter/deck-asset/ffffffffffff").status,
        404
    );
}

/// A deck whose theme `mytheme` (extends default) has an extra stylesheet with one
/// `url()` next to it and one found through the theme's `assets/` (theme-format.md §5).
fn theme_deck(dir: &Path) -> PathBuf {
    let theme = dir.join("mytheme");
    std::fs::create_dir_all(theme.join("styles/img")).unwrap();
    std::fs::create_dir_all(theme.join("assets")).unwrap();
    std::fs::write(
        theme.join("theme.toml"),
        "name = \"mytheme\"\nextends = \"default\"\n\n[styles]\nentries = [\"styles/extra.css\"]\n",
    )
    .unwrap();
    std::fs::write(
        theme.join("styles/extra.css"),
        ".logo { background: url(logo.png); }\n.b { background: url(img/b.png); }\n",
    )
    .unwrap();
    std::fs::write(theme.join("styles/img/b.png"), b"b-bytes").unwrap();
    std::fs::write(theme.join("assets/logo.png"), b"logo-bytes").unwrap();
    std::fs::write(theme.join("assets/a b.png"), b"space-bytes").unwrap();
    std::fs::write(dir.join("secret.txt"), b"secret").unwrap();
    let deck = dir.join("deck.md");
    std::fs::write(&deck, "---\ntheme: ./mytheme\n---\n\n# Themed\n").unwrap();
    deck
}

#[test]
fn theme_assets_resolve_in_every_helper_path_form() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, _) = state_with(&theme_deck(tmp.path()));
    let server = TestServer::start(state);

    // The served stylesheet links its url()s through the asset route.
    let css = server.text("/__presenter/theme.css");
    let urls = css_urls(&css);
    for expected in [
        "/__presenter/asset/@mytheme/assets/logo.png",
        "/__presenter/asset/@mytheme/styles/img/b.png",
    ] {
        assert!(
            urls.iter().any(|url| url == expected),
            "{expected} in {urls:?}"
        );
    }
    let cases: [(&str, &[u8]); 6] = [
        ("/__presenter/asset/@mytheme/assets/logo.png", b"logo-bytes"),
        ("/__presenter/asset/@mytheme/styles/img/b.png", b"b-bytes"),
        // The chain's assets/ form, as `asset("logo.png")` writes it.
        ("/__presenter/asset/logo.png", b"logo-bytes"),
        // Percent-encoded, as the renderer writes it.
        ("/__presenter/asset/a%20b.png", b"space-bytes"),
        (
            "/__presenter/asset/@mytheme/styles/extra.css",
            b".logo { background: url(logo.png); }\n.b { background: url(img/b.png); }\n",
        ),
        (
            "/__presenter/asset/@default/theme.toml",
            &std::fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/themes/default/theme.toml"),
            )
            .unwrap(),
        ),
    ];
    for (url, bytes) in cases {
        let response = server.get(url);
        assert_eq!(response.status, 200, "GET {url}");
        assert_eq!(response.body, bytes, "GET {url}");
        assert_eq!(response.cache_control, "no-store", "GET {url}");
    }
    assert_eq!(
        server.get("/__presenter/asset/logo.png").content_type,
        "image/png"
    );
    let presenter_js = server.get("/__presenter/asset/presenter/live-reload.js");
    assert_eq!(presenter_js.status, 200);
    assert_eq!(presenter_js.cache_control, "no-store");

    // Nothing outside the vendor tree and the theme directories is reachable.
    for url in [
        "/__presenter/asset/%2E%2E/secret.txt",
        "/__presenter/asset/@mytheme/%2E%2E/secret.txt",
        "/__presenter/asset/@mytheme/..%2F..%2Fsecret.txt",
        "/__presenter/asset/@nope/theme.toml",
        "/__presenter/asset/%2Fetc%2Fpasswd",
        "/__presenter/asset/vendor/%2E%2E/%2E%2E/Cargo.toml",
        "/__presenter/asset/mytheme/theme.toml",
    ] {
        assert_eq!(server.get(url).status, 404, "GET {url}");
    }
}

/// One SSE frame: its `event:` name and its `data:` as JSON.
struct Frame {
    event: String,
    data: Value,
}

/// An open `/__presenter/events` stream.
struct Events {
    reader: BufReader<ureq::BodyReader<'static>>,
}

impl Events {
    /// Opens the stream; every read must finish within 2 s of opening it.
    fn open(server: &TestServer) -> Events {
        let response = agent(Duration::from_secs(2))
            .get(&server.url("/__presenter/events"))
            .call()
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let content_type = response.headers().get("content-type").unwrap();
        assert_eq!(content_type.to_str().unwrap(), "text/event-stream");
        Events {
            reader: BufReader::new(response.into_body().into_reader()),
        }
    }

    /// The next frame with an event name (keep-alive comments are skipped); `None` at the
    /// end of the stream.
    fn next(&mut self) -> Option<Frame> {
        let (mut event, mut data) = (None, String::new());
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line).unwrap() == 0 {
                return None;
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if let Some(event) = event.take() {
                    let data = serde_json::from_str(&data).unwrap();
                    return Some(Frame { event, data });
                }
            } else if let Some(name) = line.strip_prefix("event:") {
                event = Some(name.trim().to_string());
            } else if let Some(chunk) = line.strip_prefix("data:") {
                data.push_str(chunk.trim_start());
            }
        }
    }
}

#[test]
fn events_send_hello_then_one_build_event_per_rebuild() {
    let input = fixture("md/minimal.md");
    let (state, _) = state_with(&input);
    let server = TestServer::start(state);
    // Built up front, so that the 2 s budget of the stream covers only the server.
    let rebuild = serve_build(&input, server::next_build_id(&server.state));

    let mut events = Events::open(&server);
    let hello = events.next().unwrap();
    assert_eq!(hello.event, "hello");
    assert_eq!(hello.data, json!({"build_id": 1, "status": "ok"}));

    let build_id = server::apply_build(&server.state, &rebuild);
    assert_eq!(build_id, 2);
    let sent = server::publish(
        &server.events,
        BuildEvent::Ok {
            build_id,
            kind: ChangeKind::Css,
            duration_ms: 42,
        },
    );
    assert_eq!(sent, 1);
    let build = events.next().unwrap();
    assert_eq!(build.event, "build");
    assert_eq!(
        build.data,
        json!({"build_id": 2, "status": "ok", "kind": "css", "duration_ms": 42})
    );
    assert!(server.text("/").contains("/__presenter/theme.css?v=2"));

    let err = Error::Parse {
        path: PathBuf::from("slides.md"),
        line: Some(41),
        message: "unknown directive 'include'".into(),
    };
    let build_id = server::apply_error(&server.state, &err);
    server::publish(
        &server.events,
        BuildEvent::Error {
            build_id,
            error: (&err).into(),
        },
    );
    let failed = events.next().unwrap();
    assert_eq!(failed.event, "build");
    assert_eq!(
        failed.data,
        json!({"build_id": 3, "status": "error", "error": {
            "message": "slides.md:41 unknown directive 'include'",
            "file": "slides.md", "line": 41, "kind": "parse"}})
    );
    // The last good document stays; the status reports the failure.
    assert!(server.text("/").contains(r#"<div class="slides">"#));
    let status: Value = serde_json::from_str(&server.text("/__presenter/state")).unwrap();
    assert_eq!(status["build_id"], 3);
    assert_eq!(status["status"], "error");
    assert_eq!(status["error"]["kind"], "parse");

    // A browser that connects now gets the error in `hello` (live-reload.md §2 addition).
    let mut late = Events::open(&server);
    let hello = late.next().unwrap();
    assert_eq!(hello.event, "hello");
    assert_eq!(hello.data["build_id"], 3);
    assert_eq!(hello.data["status"], "error");
    assert_eq!(hello.data["error"]["line"], 41);
}

#[test]
fn before_the_first_good_build_the_page_shows_the_error() {
    let state: Shared = Arc::new(RwLock::new(ServeState::default()));
    let err = Error::Template {
        template: "layouts/title.html".into(),
        line: Some(4),
        message: "unknown filter <x>".into(),
    };
    assert_eq!(server::apply_error(&state, &err), 1);
    let server = TestServer::start(state);

    let page = server.get("/");
    assert_eq!(page.status, 200);
    assert_eq!(page.content_type, "text/html; charset=utf-8");
    let html = String::from_utf8(page.body).unwrap();
    assert!(html.contains(r#"<script src="/__presenter/live-reload.js"></script>"#));
    assert!(html.contains("id=\"presenter-overlay\""), "{html}");
    assert!(html.contains(
        "build error (template)\ntemplate 'layouts/title.html' line 4: unknown filter &lt;x&gt;\nlayouts/title.html:4"
    ));
    assert!(!html.contains("class=\"slides\""));

    let mut events = Events::open(&server);
    let hello = events.next().unwrap();
    assert_eq!(hello.event, "hello");
    assert_eq!(
        hello.data,
        json!({"build_id": 1, "status": "error", "error": {
            "message": "template 'layouts/title.html' line 4: unknown filter <x>",
            "file": "layouts/title.html", "line": 4, "kind": "template"}})
    );
    // Built-in files are served even without a theme.
    let reveal = server.get("/__presenter/asset/vendor/reveal/dist/reveal.css");
    assert_eq!(reveal.status, 200);
    assert_eq!(server.get("/__presenter/asset/logo.png").status, 404);
    assert_eq!(server.text("/__presenter/theme.css"), "");
}

#[test]
fn dropping_the_shutdown_sender_ends_serve_within_a_second() {
    let (state, _) = state_with(&fixture("md/minimal.md"));
    let mut server = TestServer::start(state);
    // An open event stream must not keep the server alive.
    let mut events = Events::open(&server);
    assert_eq!(events.next().unwrap().event, "hello");

    let start = Instant::now();
    drop(server.shutdown.take());
    let result = server
        .done
        .recv_timeout(Duration::from_secs(1))
        .expect("serve_until did not return within 1 s");
    assert!(result.is_ok(), "{result:?}");
    assert!(start.elapsed() < Duration::from_secs(1));
    // The stream was closed cleanly.
    assert!(events.next().is_none());
    // And the port no longer accepts requests.
    let refused = agent(Duration::from_secs(2)).get(&server.url("/")).call();
    assert!(refused.is_err());
}

#[test]
fn a_stalled_request_delays_shutdown_by_at_most_the_grace_period() {
    let (state, _) = state_with(&fixture("md/minimal.md"));
    let mut server = TestServer::start(state);
    // A client that never finishes its request would hold a plain graceful shutdown forever.
    let mut stalled = std::net::TcpStream::connect(server.addr).unwrap();
    stalled
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n")
        .unwrap();
    // An idle keep-alive connection does not delay it at all.
    let mut idle = std::net::TcpStream::connect(server.addr).unwrap();
    idle.write_all(b"GET /__presenter/state HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut buf = [0u8; 256];
    assert!(idle.read(&mut buf).unwrap() > 0);
    thread::sleep(Duration::from_millis(100));

    let start = Instant::now();
    drop(server.shutdown.take());
    let result = server
        .done
        .recv_timeout(Duration::from_secs(4))
        .expect("serve_until did not return within the grace period");
    assert!(result.is_ok(), "{result:?}");
    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
}

#[test]
fn a_dropped_browser_does_not_disturb_the_others() {
    let (state, build) = state_with(&fixture("md/minimal.md"));
    let server = TestServer::start(state);
    let gone = Events::open(&server);
    drop(gone);
    let mut events = Events::open(&server);
    assert_eq!(events.next().unwrap().event, "hello");
    let build_id = server::apply_build(&server.state, &build);
    server::publish(
        &server.events,
        BuildEvent::Ok {
            build_id,
            kind: ChangeKind::Content,
            duration_ms: 5,
        },
    );
    assert_eq!(events.next().unwrap().data["build_id"], 2);
    // Read the rest of a small response to make sure the connection is still usable.
    let mut body = String::new();
    agent(Duration::from_secs(2))
        .get(&server.url("/__presenter/state"))
        .call()
        .unwrap()
        .into_body()
        .into_reader()
        .read_to_string(&mut body)
        .unwrap();
    assert!(body.contains("\"build_id\":2"));
}
