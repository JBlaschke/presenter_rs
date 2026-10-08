#!/usr/bin/env python3
"""Mock of `presenter serve` for exercising assets/presenter/live-reload.js by hand.

Python 3 standard library only. Serves a reveal.js deck built from the vendored assets with
the routes of docs/spec/live-reload.md §1 (GET /, /__presenter/theme.css,
/__presenter/asset/{vendor,presenter}/..., /__presenter/live-reload.js,
/__presenter/events (SSE), /__presenter/state) plus control routes to fake rebuilds:

  POST /__mock/slides           body = new inner HTML of <div class="slides"> (no event);
                                GET returns the current one
  POST /__mock/css              body = new theme CSS (no event)
  POST /__mock/head             body = extra HTML appended to <head> (no event)
  GET  /__mock/title?t=...      set the <title> (no event)
  GET  /__mock/emit?kind=css|content|error[&message=..&file=..&line=..][&nodoc=1]
                                next build_id; broadcast a `build` event (an ok build makes
                                the current slides/CSS/title the served document; an error
                                with nodoc=1 serves the "no successful build yet" page)
  GET  /__mock/down?ms=N        drop SSE clients and answer /__presenter/events with 503 for N ms
  GET  /__mock/restart          like a restarted server: build_id back to 1, SSE clients dropped
  GET  /__mock/reset            restore the initial deck, CSS and build_id 1
  GET  /__mock/log              JSON list of requests since the last call (then cleared)

Usage (from the repository root):
  python3 tests/fixtures/live-reload/mock-server.py [--port 8765]
then open http://127.0.0.1:8765/ and drive it with curl, e.g.
  curl -X POST --data-binary @slides.html http://127.0.0.1:8765/__mock/slides
  curl 'http://127.0.0.1:8765/__mock/emit?kind=content'
The QA procedure using it is docs/dev/live-reload-qa.md.
"""

import argparse
import json
import mimetypes
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

ASSETS = Path(__file__).resolve().parents[3] / "assets"

INITIAL_SLIDES = """
<section id="intro"><h1>Live reload</h1><p>mock deck</p></section>
<section>
  <section id="stack-top"><h2>Vertical stack</h2><p>top</p></section>
  <section id="stack-bottom"><h2>Vertical stack</h2><p>bottom</p></section>
</section>
<section id="code"><h2>Code</h2>
<pre><code class="language-rust" data-trim data-line-numbers="1|2">
    fn main() {
        println!("hello");
    }
</code></pre>
<p class="fragment">a fragment</p></section>
<section id="math"><h2>Math</h2><p><span data-math-style="inline">e^{i\\pi} + 1 = 0</span></p></section>
<section id="last"><h2>Last slide</h2></section>
"""
INITIAL_CSS = ".reveal h2 { color: rgb(0, 0, 255); }\n"
INITIAL_TITLE = "Mock deck"

# Stand-in for assets/presenter/math.js (T15) while it is a placeholder; same contract.
MATH_FALLBACK = """if (!window.__presenterRenderMath) {
  window.__presenterRenderMath = (root) => root.querySelectorAll('[data-math-style]').forEach((el) =>
    katex.render(el.textContent, el, { displayMode: el.dataset.mathStyle === 'display', throwOnError: false }));
  window.__presenterRenderMath(document);
}"""

# What the server sends while no build has succeeded yet (live-reload.md §1).
FALLBACK = """<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><title>Build failed</title></head>
<body><script src="/__presenter/live-reload.js"></script></body></html>
"""

PAGE = """<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>{title}</title>
<link rel="stylesheet" href="/__presenter/asset/vendor/reveal/dist/reset.css">
<link rel="stylesheet" href="/__presenter/asset/vendor/reveal/dist/reveal.css">
<link rel="stylesheet" href="/__presenter/asset/vendor/reveal/dist/theme/white.css">
<link rel="stylesheet" href="/__presenter/asset/vendor/reveal/dist/plugin/highlight/monokai.css">
<link rel="stylesheet" href="/__presenter/asset/vendor/katex/katex.min.css">{head}
<link id="presenter-theme" rel="stylesheet" href="/__presenter/theme.css?v={build_id}">
</head>
<body>
<div class="reveal">
<div class="slides">{slides}</div>
</div>
<script src="/__presenter/asset/vendor/reveal/dist/reveal.js"></script>
<script src="/__presenter/asset/vendor/reveal/dist/plugin/highlight.js"></script>
<script src="/__presenter/asset/vendor/reveal/dist/plugin/notes.js"></script>
<script src="/__presenter/asset/vendor/katex/katex.min.js"></script>
<script src="/__presenter/asset/presenter/math.js"></script>
<script>{math}</script>
<script>Reveal.initialize(Object.assign({{"hash":true,"transition":"none","width":1280,"height":720}}, {{ plugins: [RevealHighlight, RevealNotes] }}));</script>
<script src="/__presenter/live-reload.js"></script>
</body>
</html>
"""


class Mock:
    """Shared server state; every field is guarded by `cond`."""

    def __init__(self):
        self.cond = threading.Condition()
        self.generation = 0  # bumped to drop all SSE clients
        self.reset()

    def reset(self):
        self.build_id = 1
        self.status, self.error = "ok", None
        self.slides, self.css, self.title, self.head = INITIAL_SLIDES, INITIAL_CSS, INITIAL_TITLE, ""
        self.served = self.snapshot()
        self.events = []  # data of the `build` events, in order
        self.down_until = 0.0
        self.log = []

    def snapshot(self):
        return dict(slides=self.slides, css=self.css, title=self.title, head=self.head)

    def emit(self, kind, query):
        self.build_id += 1
        if kind == "error":
            line = query.get("line", ["41"])[0]
            self.status = "error"
            self.error = {
                "message": query.get("message", ["slides.md:41: unknown directive 'include'"])[0],
                "file": query.get("file", ["slides.md"])[0],
                "line": int(line) if line else None,
                "kind": "parse",
            }
            data = {"status": "error", "build_id": self.build_id, "error": self.error}
            if "nodoc" in query:
                self.served = None
        else:
            self.status, self.error = "ok", None
            self.served = self.snapshot()
            data = {"status": "ok", "build_id": self.build_id, "kind": kind, "duration_ms": 5}
        self.events.append(data)
        self.cond.notify_all()
        return data

    def drop_clients(self):
        self.generation += 1
        self.cond.notify_all()


MOCK = Mock()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"  # one request per connection; SSE streams until closed

    def log_message(self, fmt, *args):  # keep the terminal quiet
        pass

    def send(self, status, body, ctype, cache="no-store"):
        data = body.encode() if isinstance(body, str) else body
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", cache)
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        path = urlsplit(self.path).path
        body = self.rfile.read(int(self.headers.get("Content-Length") or 0)).decode()
        field = {"/__mock/slides": "slides", "/__mock/css": "css", "/__mock/head": "head"}.get(path)
        if field is None:
            return self.do_GET()
        with MOCK.cond:
            setattr(MOCK, field, body)
        self.send(200, "ok", "text/plain")

    def do_GET(self):
        url = urlsplit(self.path)
        path, query = url.path, parse_qs(url.query, keep_blank_values=True)
        if not path.startswith("/__mock/"):
            with MOCK.cond:
                MOCK.log.append(f"{self.command} {self.path} [{self.headers.get('Sec-Fetch-Dest', '-')}]")
        if path.startswith("/__mock/"):
            return self.control(path, query)
        if path == "/__presenter/events":
            return self.events()
        if path == "/favicon.ico":  # keep the browser console free of 404 noise
            return self.send(204, "", "image/x-icon")
        with MOCK.cond:
            served, build_id = MOCK.served, MOCK.build_id
            state = {"build_id": build_id, "status": MOCK.status, "error": MOCK.error}
        if path == "/":
            page = PAGE.format(build_id=build_id, math=MATH_FALLBACK, **served) if served else FALLBACK
            return self.send(200, page, "text/html; charset=utf-8")
        if path == "/__presenter/theme.css":
            return self.send(200, served["css"] if served else "", "text/css; charset=utf-8")
        if path == "/__presenter/state":
            return self.send(200, json.dumps(state), "application/json")
        if path == "/__presenter/live-reload.js":
            return self.file(ASSETS / "presenter" / "live-reload.js", "no-store")
        if path.startswith("/__presenter/asset/"):
            rel = path[len("/__presenter/asset/"):]
            target = (ASSETS / rel).resolve()
            if rel.split("/")[0] in ("vendor", "presenter") and ASSETS in target.parents:
                return self.file(target, "max-age=3600" if rel.startswith("vendor/") else "no-store")
        self.send(404, "not found", "text/plain")

    def file(self, target, cache):
        if not target.is_file():
            return self.send(404, "not found", "text/plain")
        ctype = mimetypes.guess_type(target.name)[0] or "application/octet-stream"
        self.send(200, target.read_bytes(), ctype, cache)

    def control(self, path, query):
        with MOCK.cond:
            if path == "/__mock/emit":
                return self.send(200, json.dumps(MOCK.emit(query.get("kind", ["content"])[0], query)), "application/json")
            if path == "/__mock/slides":
                return self.send(200, MOCK.slides, "text/html; charset=utf-8")
            if path == "/__mock/title":
                MOCK.title = query.get("t", [""])[0]
            elif path == "/__mock/down":
                MOCK.down_until = time.monotonic() + int(query.get("ms", ["3000"])[0]) / 1000
                MOCK.drop_clients()
            elif path == "/__mock/restart":
                MOCK.build_id, MOCK.events = 1, []
                MOCK.drop_clients()
            elif path == "/__mock/reset":
                MOCK.drop_clients()
                MOCK.reset()
            elif path == "/__mock/log":
                entries, MOCK.log = MOCK.log, []
                return self.send(200, json.dumps(entries), "application/json")
            else:
                return self.send(404, "unknown control route", "text/plain")
        self.send(200, "ok", "text/plain")

    def events(self):
        with MOCK.cond:
            if time.monotonic() < MOCK.down_until:
                return self.send(503, "down", "text/plain")
            generation, seen = MOCK.generation, len(MOCK.events)
            hello = {"build_id": MOCK.build_id, "status": MOCK.status}
            if MOCK.error:
                hello["error"] = MOCK.error
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        try:
            self.wfile.write(f"event: hello\ndata: {json.dumps(hello)}\n\n".encode())
            self.wfile.flush()
            while True:
                with MOCK.cond:
                    MOCK.cond.wait_for(lambda: MOCK.generation != generation or len(MOCK.events) > seen, 15)
                    if MOCK.generation != generation:
                        return
                    fresh, seen = MOCK.events[seen:], len(MOCK.events)
                chunk = "".join(f"event: build\ndata: {json.dumps(d)}\n\n" for d in fresh) or ": keep-alive\n\n"
                self.wfile.write(chunk.encode())
                self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            return


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--port", type=int, default=8765)
    args = parser.parse_args()
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.daemon_threads = True
    print(f"mock presenter serve on http://127.0.0.1:{args.port}/ (Ctrl-C to stop)", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
