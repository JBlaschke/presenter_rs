# Spec: Configuration

**Status:** contract. Types in `src/config.rs` (T01); loading/merging in T10 and T16;
consumed everywhere.

## 1. Sources and precedence (highest first)

1. CLI flags (`--theme`, `--out`, `--standalone/--site`, `--port`, …)
2. Deck front matter (Markdown) or notebook metadata (`metadata.presenter`, first-cell front matter)
3. Project file `presenter.toml` — found by walking up from the source file's directory to the
   filesystem root; the first one wins; `--config PATH` overrides discovery
4. The theme chain's `theme.toml` `[reveal]` tables (root first, child wins)
5. Built-in defaults (§2)

Merging is key-by-key for tables (`[reveal]` deep-merges one level: top-level keys of the
reveal options). Lists replace (except theme style/script entries, see theme-format.md).

## 2. `presenter.toml` schema with defaults

```toml
[deck]
theme = "default"          # name or path (relative to this file)
lang = "en"
# title/author/date may be set here as defaults; front matter overrides

[build]
out_dir = "dist"           # `presenter build x.md` writes dist/x.html (or dist/x/ in site mode)
standalone = true          # false → site mode: index.html + assets/
minify = false             # reserved; no-op in this plan

[slides]
split = "rules"            # "rules" (--- / --) | "headings"
slide_level = 2            # headings mode: level that starts a vertical slide
incremental_lists = false  # T43: top-level list items become fragments

[markdown]
smart_punctuation = false  # comrak parse.smart
sourcepos = false          # emit data-sourcepos on elements (serve may force true later)

[reveal]                   # passed to Reveal.initialize after merging (any reveal option)
hash = true
slideNumber = false
transition = "slide"
controls = true
progress = true
center = true
width = 1280               # reveal default is 960×700; we default to 16:9 HD
height = 720
plugins = ["highlight", "notes", "zoom", "search"]   # names we know: highlight, notes, zoom, search, math(ignored: KaTeX auto)
                                                     # plugins are NOT reveal options; the renderer strips this key

[math]
enabled = "auto"           # "auto" (when deck.features.math) | "always" | "never"
katex_options = {}         # passed to katex.render for every element (e.g. { macros = { "\\R" = "\\mathbb{R}" } })

[notebook]
mime_priority = ["text/html", "image/svg+xml", "image/png", "image/jpeg", "image/gif", "image/webp",
                 "text/markdown", "text/latex", "application/json", "text/plain"]
hide_input_tags  = ["hide-input", "remove-input"]
hide_output_tags = ["hide-output", "remove-output"]
remove_cell_tags = ["remove-cell", "hide-cell", "skip"]
split_markdown_cells = false   # honour --- / -- inside markdown cells (off: nbconvert behaviour)
show_prompts = true            # cell.html shows In [n]/Out [n]

[serve]
host = "127.0.0.1"
port = 3000
open = false
```

Unknown keys are an error with a "did you mean" hint when within edit distance 2 of a known
key (use the `strsim` crate or a tiny Levenshtein). `#[serde(deny_unknown_fields)]` on every
config struct.

## 3. Front matter mapping (Markdown and notebooks)

| front matter key | goes to |
|---|---|
| `title`, `subtitle`, `author`, `date`, `description`, `lang`, `theme` | `DeckMeta.*` |
| `reveal:` (map) | `DeckMeta.reveal`, merged over `[reveal]` |
| `slides:`, `markdown:`, `math:`, `notebook:` (maps) | override the same `presenter.toml` tables for this deck |
| anything else | `DeckMeta.vars` |

## 4. Rust shape (`src/config.rs`)

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)] #[serde(default, deny_unknown_fields)]
pub struct Config { pub deck: DeckConfig, pub build: BuildConfig, pub slides: SlidesConfig,
                    pub markdown: MarkdownConfig, pub reveal: Map<String, Value>, pub math: MathConfig,
                    pub notebook: NotebookConfig, pub serve: ServeConfig }
// one struct per table, all `#[serde(default, deny_unknown_fields)]`, with `impl Default` giving §2 values.

impl Config {
    pub fn load(path: &Path) -> Result<Config>;                 // parse TOML, apply defaults
    pub fn discover(start_dir: &Path) -> Result<Option<(PathBuf, Config)>>;
    pub fn merge_front_matter(&mut self, meta: &DeckMeta) -> Result<()>;  // §3 rows 2–3
    pub fn merge_theme_reveal(&mut self, theme_reveal: &Map<String, Value>); // theme defaults UNDER config
    pub fn apply_cli(&mut self, overrides: &CliOverrides);
    pub fn reveal_options_json(&self) -> String;               // strips "plugins", serializes deterministically (BTreeMap order)
    pub fn enabled_plugins(&self) -> Vec<PluginId>;            // from reveal.plugins, validated
}
```

Precedence is implemented by calling: defaults → `load/discover` → `merge_theme_reveal`
(note: theme is *below* project config, so the renderer applies theme defaults by merging the
project/front-matter reveal map **over** the theme map) → `merge_front_matter` → `apply_cli`.
Write a unit test for every precedence pair.

## 5. Determinism
`reveal_options_json` must serialize keys in sorted order and numbers without trailing
`.0`. Snapshot tests depend on it.
