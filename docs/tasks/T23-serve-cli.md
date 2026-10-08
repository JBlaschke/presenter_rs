# T23 — `serve` CLI wiring and integration tests

| | |
|---|---|
| Milestone | M2 |
| Size | S |
| Depends on | T20, T21, T22 |
| Parallel-safe with | T24, T33, T34 |
| Files you own | `src/cli/serve.rs`, `tests/cli_serve.rs` |

## Goal
`presenter serve deck.md` builds, serves, watches and live-reloads; port 0 picks a free
port and prints the URL first; errors never kill the server.

## Read first
- `docs/spec/cli.md` (serve), `docs/spec/live-reload.md` §4–§5

## Steps
1. Build once with `OutputMode::Serve` and `BuildInfo { mode: Serve, live_reload: true, id: 1 }`
   (on failure: still start, with `apply_error`, and watch the failed set).
2. Bind `TcpListener` (`host:port`; port 0 allowed), print `Serving http://127.0.0.1:PORT/` as the
   **first stdout line**, flush. `--open` → `open::that(url)`.
3. Start the watcher (T20) on a std thread; a tokio task receives batches, runs
   `spawn_blocking(build_deck)`, applies the result, updates the watch set, and broadcasts
   `BuildEvent` with `duration_ms`. Log one line per rebuild (`rebuilt in 180 ms (content)` / the error).
4. Ctrl-C → graceful shutdown → exit 0.

## Acceptance criteria
- `tests/cli_serve.rs`: spawn `presenter serve tmp/a.md --port 0` (copy `minimal.md`), read the
  first stdout line, `GET /` 200; append a slide to `a.md`; poll `/__presenter/state` until
  `build_id` increases (≤ 10 s); `GET /` now has one more `<section`; write an invalid theme
  path into front matter → state `status: "error"` with a message; fix → `ok`; kill the child.
- Port in use → exit 1 with hint `--port 0`.
- `--open` not exercised in tests; documented.

## Out of scope
Any change to the protocol or client.

## Hints
- Use `std::process::Command` + `BufReader` on stdout in the test; set `kill_on_drop` semantics by wrapping the child in a guard struct that kills in `Drop`.

## Amendments (coordinator, 2026-10-08)
- Build on T20's `serve::watch::{Watcher, watch_set_for, watch_set_before_build, watch_set_for_failed, classify}`
  and T21's `serve::server::{ServeState, Shared, router, serve/serve_until, apply_build, apply_error}`,
  `serve::events::BuildEvent` (see their notes). Run T22's QA checklist (docs/dev/live-reload-qa.md)
  against the REAL server with the CDP helper and record the results there (additive section).
