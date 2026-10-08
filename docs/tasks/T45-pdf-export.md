# T45 — PDF export via headless Chrome

| | |
|---|---|
| Milestone | M4 |
| Size | S |
| Depends on | T16 |
| Parallel-safe with | T40–T44, T46, T47 |
| Files you own | `src/cli/export.rs`, `src/pdf.rs`, `tests/cli_export.rs` |

## Goal
`presenter export pdf deck.md` produces a PDF using reveal's print mode and an installed
Chrome/Chromium, with clear guidance when none is found.

## Read first
- `docs/spec/cli.md` (export); reveal.js docs "PDF export" (`?print-pdf`, `pdfSeparateFragments`, `pdfMaxPagesPerSlide`)

## Steps
1. `pdf.rs`: `pub fn find_chrome(explicit: Option<&Path>) -> Option<PathBuf>` — order: explicit flag,
   `$CHROME_PATH`, macOS app bundles (Google Chrome, Chromium, Microsoft Edge, Brave, Arc), Linux
   `which` of `google-chrome`, `google-chrome-stable`, `chromium`, `chromium-browser`, `microsoft-edge`.
2. Build standalone HTML into a temp dir; run
   `chrome --headless=new --disable-gpu --no-pdf-header-footer --run-all-compositor-stages-before-draw --virtual-time-budget=10000 --print-to-pdf=<out> "file://<tmp>/deck.html?print-pdf"`;
   `--virtual-time-budget` lets KaTeX/highlight finish. Timeout 120 s. Surface Chrome's stderr on failure.
3. Reveal print tips in the output: the config `[reveal] pdfSeparateFragments` is passed through like any option.
4. Exit 3 with install hints when no Chrome; exit 1 when Chrome fails.

## Acceptance criteria
- Unit tests for `find_chrome` with `CHROME_PATH` pointing at a temp executable and with an explicit path.
- Integration test runs only when `find_chrome(None)` is `Some` (otherwise prints `skipping`): exports
  `examples/basic/slides.md` and asserts the file starts with `%PDF-` and is > 20 KB.
- Documented limitations in the command's `--help`: fragments collapse unless `pdfSeparateFragments`, animations are static.

## Amendments (coordinator, 2026-10-08)
- Chrome's one-shot `--print-to-pdf` (like `--dump-dom`/`--screenshot`) HANGS in the coordinator's
  environment, and it cannot wait for KaTeX/highlight anyway. Implement the export by driving
  Chrome over the DevTools protocol: spawn headless Chrome with `--remote-debugging-port=0` and a
  temporary `--user-data-dir`, read the `DevTools listening on ws://…` line from stderr, open a page
  target, navigate to `file://…/deck.html?print-pdf`, wait until `Reveal.isReady()` (and fonts/KaTeX
  are done), call `Page.printToPDF` (`preferCSSPageSize: true`, `printBackground: true`) and write
  the PDF. A small synchronous WebSocket client crate (e.g. `tungstenite`; verify the version and
  API in ~/.cargo/registry/src after `cargo add`) is an acceptable additive dependency. Keep exit 3
  (no Chrome), exit 1 (Chrome failed, with its stderr), and the 120 s overall timeout; always kill
  Chrome and remove the temp profile. The coordinator's scratch helper `tools/cdp.mjs` shows a
  protocol sequence that works here. The integration test must actually pass on this machine.
- T16 put `export::PDF_LIMITATIONS` in `src/cli/export.rs` and a wording test
  (`export_pdf_help_documents_the_limitations`) in `src/cli/mod.rs`; move that test into
  `src/cli/export.rs` (you own the wording) and delete it from `cli/mod.rs`.
- Replace the `stub_exits_2` test in `src/cli/export.rs` along with the stub.
