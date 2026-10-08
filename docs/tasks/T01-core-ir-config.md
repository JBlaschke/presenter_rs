# T01 — Core IR, config, error types, fixture corpus

| | |
|---|---|
| Milestone | M0 |
| Size | M |
| Depends on | T00 |
| Parallel-safe with | nothing (wave 2 starts after this merges) |
| Files you own | `src/ir.rs`, `src/config.rs`, `src/error.rs`, `tests/ir_roundtrip.rs`, `tests/config.rs`, `tests/fixtures/ir/sample-deck.json`, `tests/fixtures/md/*.md` (inputs only), `tests/fixtures/config/presenter.toml` |

## Goal
Make the contracts compile: the IR exactly as in `docs/spec/ir.md`, the config structs
with defaults and precedence helpers from `docs/spec/config.md`, the error and diagnostic
types, and the Markdown fixture inputs every later task will test against.

## Read first
- `docs/spec/ir.md` (all), `docs/spec/config.md` (all)
- `docs/spec/markdown-dialect.md` §11 (fixture list) and the sections each fixture exercises

## Steps
1. `src/ir.rs`: transcribe the types verbatim (keep doc comments). Add `impl Default for Slide`.
   Add small constructors used by parsers: `Slide::new()`, `Block::html(String)`,
   `Features::merge(&mut self, other)`.
2. `src/error.rs`: the `Error` enum and `Result` alias from ir.md §5, plus
   ```rust
   #[derive(Debug, Clone, PartialEq, Eq, Serialize)] pub enum Level { Warning, Error }
   #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
   pub struct Diagnostic { pub level: Level, pub code: &'static str, pub message: String,
                           pub path: Option<PathBuf>, pub span: Option<SourceSpan> }
   pub struct Diagnostics(Vec<Diagnostic>) // push, warn(), error(), iter, has_errors(), extend
   ```
   and `impl From<toml::de::Error> for Error` (→ `Config` with the TOML message, which includes line/col).
3. `src/config.rs`: structs per config.md §4 with `#[serde(default, deny_unknown_fields)]`
   and `Default` impls giving §2 values; `CliOverrides { theme, out, standalone, port, host, open }`;
   `load`, `discover` (walk up until root; stop at the first `presenter.toml`), `apply_cli`,
   `merge_theme_reveal` (theme values only where the key is absent), `merge_front_matter`
   (uses `DeckMeta.reveal` and the `slides/markdown/math/notebook` maps in `DeckMeta.vars`
   — move them out of `vars` when they are objects), `reveal_options_json` (sorted keys,
   `plugins` removed), `enabled_plugins` → `Vec<PluginId>` where
   `enum PluginId { Highlight, Notes, Zoom, Search }` (`"math"` accepted and ignored with a
   debug log; unknown names → `Error::Config`).
   Unknown TOML keys: catch the serde error and append a "did you mean" suggestion when a
   known key is within Levenshtein distance 2 (write a 15-line Levenshtein, no new crate).
4. `tests/fixtures/ir/sample-deck.json`: the canonical IR example (ir.md §6, expanded to
   ~6 slides covering every variant: title layout, attrs, vertical children, notes, math,
   image placeholder, a notebook cell with each `CellOutput` kind).
5. Markdown fixture inputs (sources only; expected outputs are produced by T11/T12):
   `minimal.md`, `separators.md`, `attributes.md`, `directives.md`, `code.md`, `math.md`,
   `images.md` (+ `images/ok.png` 1×1 PNG), `notes.md`. Each fixture exercises exactly the
   dialect section it is named after; put a comment at the top listing the behaviours.
6. `tests/fixtures/presenter.toml`: every key from config.md §2 set to a non-default value
   (used by config tests).

## Acceptance criteria
- `serde_json::from_str::<Deck>(sample)` → `to_string_pretty` → `from_str` → equal `Deck`; and
  re-serialized JSON equals the fixture when both are parsed to `serde_json::Value`.
- `Config::default()` equals `toml::from_str::<Config>("")`.
- `tests/fixtures/presenter.toml` loads and every field differs from default (assert a few).
- Unknown key `[buidl]` → error message contains `did you mean 'build'`.
- Precedence tests: for `reveal.transition`, each pair (theme < file < front matter < CLI) is covered.
- `reveal_options_json` of defaults is exactly
  `{"center":true,"controls":true,"hash":true,"height":720,"progress":true,"slideNumber":false,"transition":"slide","width":1280}`.
- `cargo doc --no-deps` has no warnings for these files (`#![warn(missing_docs)]`).

## Out of scope
Parsing Markdown or YAML (T10/T11/T12), theme files (T02/T13).

## Hints (verified)
- `serde_json::Map` is `BTreeMap`-backed unless `preserve_order` is on. T00 enables
  `preserve_order` so front-matter `vars` keep author order; for `reveal_options_json`
  sort explicitly by collecting into a `BTreeMap<String, Value>` first.
- `toml` 1.x error `Display` already includes `at line N column M`; keep it.

## Amendments (coordinator, 2026-10-08)
- Contract changes in `docs/spec/ir.md` §3–§4 and `docs/spec/config.md` §3–§4 (read the
  "Contract change" boxes) are part of this task:
  - `src/ir.rs` also defines `pub struct Parsed { pub deck: Deck, pub assets: AssetRegistry, pub diagnostics: Diagnostics }`.
  - **You also own `src/assets/registry.rs` for this task** and implement the full ir.md §3 API
    (`AssetRegistry`, `Asset`, `AssetData`, `new`, `register_file`, `register_bytes`, `get`,
    `iter`, `infos`, `placeholder`; `Default` too) with unit tests. T14 extends it later.
    `infos` fills `origin` as `file:<path relative to base_dir>` (fall back to the absolute
    path when not under `base_dir`) or `inline:<byte length>`. Iteration order is by id (BTreeMap).
  - `Config.reveal` holds only explicit keys; add `Config::builtin_reveal_defaults()` and
    `Config::effective_reveal()`; `reveal_options_json`/`enabled_plugins` use the effective map.
  - `merge_front_matter` reads `slides/markdown/math/notebook` from `meta.vars` without removing
    them, and is idempotent.
- `Diagnostic` also derives `Deserialize` is **not** required (`code: &'static str`); add
  `Diagnostics::into_vec`, `Diagnostics::len/is_empty`, and `impl IntoIterator`.
- Do not edit `docs/tasks/README.md`; the coordinator maintains the status table.
- The config fixture lives at `tests/fixtures/config/presenter.toml` (not `tests/fixtures/`), so that config discovery from the md/ipynb fixtures never finds it.
