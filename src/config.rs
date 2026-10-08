//! Configuration structs, `presenter.toml` loading and precedence merging.
//!
//! Contract: `docs/spec/config.md` (§2 schema and defaults, §3 front matter mapping, §4 Rust
//! shape, including the "Contract change" boxes). Sources, highest precedence first:
//! CLI flags > front matter > `presenter.toml` > theme `[reveal]` > built-in defaults.
//! The intended call order is
//!
//! ```text
//! Config::discover / Config::load        (or Config::default())
//!   → merge_theme_reveal(theme defaults)  fills only keys the file did not set
//!   → merge_front_matter(&deck.meta)      overrides; idempotent
//!   → apply_cli(&overrides)               overrides everything
//! ```
//!
//! `Config.reveal` holds only explicitly configured reveal options; the built-in §2 values
//! live in [`Config::builtin_reveal_defaults`] and are applied underneath by
//! [`Config::effective_reveal`].
//!
//! `reveal.plugins` is validated where it enters the config: [`Config::from_toml_str`] /
//! [`Config::load`] reject unknown names with [`Error::Config`], [`Config::merge_front_matter`]
//! with [`Error::FrontMatter`]. [`Config::enabled_plugins`] itself never fails (it skips and
//! logs anything invalid that came in through the theme); [`Config::validate_plugins`] is the
//! strict check over the merged map.
//!
//! TOML dates (`date = 2026-10-08`) are accepted wherever a string or an arbitrary value is
//! expected and are stored as their text, like YAML scalars in front matter (dialect §2.1).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::assets::registry::normalize_lexically;
use crate::error::{Error, Result};
use crate::ir::DeckMeta;

/// File name looked for by [`Config::discover`].
pub const CONFIG_FILE_NAME: &str = "presenter.toml";

/// The whole `presenter.toml` (config.md §2). Every table is optional.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// `[deck]`: deck-wide defaults.
    pub deck: DeckConfig,
    /// `[build]`: output options.
    pub build: BuildConfig,
    /// `[slides]`: how Markdown is split into slides.
    pub slides: SlidesConfig,
    /// `[markdown]`: comrak parse/render switches.
    pub markdown: MarkdownConfig,
    /// `[reveal]`: explicitly configured `Reveal.initialize` options (plus `plugins`).
    /// Empty by default; see [`Config::effective_reveal`] for the merged view.
    #[serde(deserialize_with = "json_map")]
    pub reveal: Map<String, Value>,
    /// `[math]`: KaTeX inclusion and options.
    pub math: MathConfig,
    /// `[notebook]`: notebook rendering options.
    pub notebook: NotebookConfig,
    /// `[serve]`: dev-server options.
    pub serve: ServeConfig,
}

/// `[deck]` table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeckConfig {
    /// Theme name or path (relative to the config file). Default `"default"`.
    pub theme: String,
    /// `<html lang>` when the deck does not set one. Default `"en"`.
    pub lang: String,
    /// Default deck title; front matter overrides. Any TOML scalar is accepted and stored
    /// as its text (`title = 42` → `"42"`).
    #[serde(deserialize_with = "scalar_text")]
    pub title: Option<String>,
    /// Default author; front matter overrides. Any TOML scalar, stored as text.
    #[serde(deserialize_with = "scalar_text")]
    pub author: Option<String>,
    /// Default date; front matter overrides. A TOML date/time (`date = 2026-10-08`) is
    /// stored as its RFC 3339 text (`"2026-10-08"`; `T` as the date/time separator), any
    /// other scalar as its text; no further parsing.
    #[serde(deserialize_with = "scalar_text")]
    pub date: Option<String>,
}

impl Default for DeckConfig {
    fn default() -> Self {
        DeckConfig {
            theme: "default".to_string(),
            lang: "en".to_string(),
            title: None,
            author: None,
            date: None,
        }
    }
}

/// `[build]` table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BuildConfig {
    /// Output directory for `presenter build` when `-o` is not given. Default `"dist"`.
    pub out_dir: PathBuf,
    /// `true`: one self-contained HTML file; `false`: site mode. Default `true`.
    pub standalone: bool,
    /// Reserved; no-op. Default `false`.
    pub minify: bool,
}

impl Default for BuildConfig {
    fn default() -> Self {
        BuildConfig {
            out_dir: PathBuf::from("dist"),
            standalone: true,
            minify: false,
        }
    }
}

/// How Markdown is split into slides (`slides.split`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SlideSplit {
    /// `---` / `--` separator lines (dialect §2.2). The default.
    #[default]
    Rules,
    /// Headings start slides (dialect §2.3, T43).
    Headings,
}

/// `[slides]` table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SlidesConfig {
    /// `"rules"` or `"headings"`. Default `"rules"`.
    pub split: SlideSplit,
    /// Headings mode: heading level that starts a vertical slide. Default `2`.
    pub slide_level: u8,
    /// Top-level list items become fragments (T43). Default `false`.
    pub incremental_lists: bool,
}

impl Default for SlidesConfig {
    fn default() -> Self {
        SlidesConfig {
            split: SlideSplit::Rules,
            slide_level: 2,
            incremental_lists: false,
        }
    }
}

/// `[markdown]` table. Both switches default to `false`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MarkdownConfig {
    /// comrak `parse.smart` (curly quotes, en/em dashes).
    pub smart_punctuation: bool,
    /// Emit `data-sourcepos` attributes.
    pub sourcepos: bool,
}

/// When KaTeX is included (`math.enabled`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MathMode {
    /// When `deck.features.math` is set. The default.
    #[default]
    Auto,
    /// Always.
    Always,
    /// Never.
    Never,
}

/// `[math]` table.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MathConfig {
    /// `"auto"`, `"always"` or `"never"`. Default `"auto"`.
    pub enabled: MathMode,
    /// Passed to `katex.render` for every element. Default `{}`.
    #[serde(deserialize_with = "json_map")]
    pub katex_options: Map<String, Value>,
}

/// `[notebook]` table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotebookConfig {
    /// MIME types in order of preference for `display_data`/`execute_result`.
    pub mime_priority: Vec<String>,
    /// Tags that set `hide_input`.
    pub hide_input_tags: Vec<String>,
    /// Tags that set `hide_output`.
    pub hide_output_tags: Vec<String>,
    /// Tags that remove a cell entirely.
    pub remove_cell_tags: Vec<String>,
    /// Honour `---` / `--` inside markdown cells. Default `false` (nbconvert behaviour).
    pub split_markdown_cells: bool,
    /// `cell.html` shows `In [n]`/`Out [n]`. Default `true`.
    pub show_prompts: bool,
}

impl Default for NotebookConfig {
    fn default() -> Self {
        let strings = |items: &[&str]| items.iter().map(|s| s.to_string()).collect();
        NotebookConfig {
            mime_priority: strings(&[
                "text/html",
                "image/svg+xml",
                "image/png",
                "image/jpeg",
                "image/gif",
                "image/webp",
                "text/markdown",
                "text/latex",
                "application/json",
                "text/plain",
            ]),
            hide_input_tags: strings(&["hide-input", "remove-input"]),
            hide_output_tags: strings(&["hide-output", "remove-output"]),
            remove_cell_tags: strings(&["remove-cell", "hide-cell", "skip"]),
            split_markdown_cells: false,
            show_prompts: true,
        }
    }
}

/// `[serve]` table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServeConfig {
    /// Interface to bind. Default `"127.0.0.1"`.
    pub host: String,
    /// Port; `0` picks a free one. Default `3000`.
    pub port: u16,
    /// Open the browser on start. Default `false`.
    pub open: bool,
}

impl Default for ServeConfig {
    fn default() -> Self {
        ServeConfig {
            host: "127.0.0.1".to_string(),
            port: 3000,
            open: false,
        }
    }
}

/// Values given on the command line; `None`/empty means "not given".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CliOverrides {
    /// `--theme NAME|PATH` → `deck.theme`.
    pub theme: Option<String>,
    /// `-o/--out PATH`: the output file (standalone) or directory (site). Not a config key,
    /// so [`Config::apply_cli`] leaves it alone; the pipeline uses it directly.
    pub out: Option<PathBuf>,
    /// `--standalone` (`Some(true)`) / `--site` (`Some(false)`) → `build.standalone`.
    pub standalone: Option<bool>,
    /// `--port N` → `serve.port`.
    pub port: Option<u16>,
    /// `--host H` → `serve.host`.
    pub host: Option<String>,
    /// `--open` → `serve.open`.
    pub open: Option<bool>,
}

/// A reveal.js plugin presenter knows how to include (`reveal.plugins`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginId {
    /// Code highlighting (`RevealHighlight`).
    Highlight,
    /// Speaker notes (`RevealNotes`).
    Notes,
    /// Alt-click zoom (`RevealZoom`).
    Zoom,
    /// Ctrl-Shift-F search (`RevealSearch`).
    Search,
}

impl PluginId {
    /// Every plugin, in the default order.
    pub const ALL: [PluginId; 4] = [
        PluginId::Highlight,
        PluginId::Notes,
        PluginId::Zoom,
        PluginId::Search,
    ];

    /// The name used in `reveal.plugins`, e.g. `"highlight"`.
    pub fn name(self) -> &'static str {
        match self {
            PluginId::Highlight => "highlight",
            PluginId::Notes => "notes",
            PluginId::Zoom => "zoom",
            PluginId::Search => "search",
        }
    }

    /// Parses a `reveal.plugins` entry (exact, lowercase). `"math"` is not a plugin id.
    pub fn from_name(name: &str) -> Option<PluginId> {
        Self::ALL.into_iter().find(|id| id.name() == name)
    }
}

impl Config {
    /// Parses a TOML document into a config (missing keys take their defaults). Unknown
    /// keys fail with [`Error::Config`] and a "did you mean" hint when one applies; so does
    /// an invalid `[reveal] plugins` list (see [`Config::validate_plugins`]).
    pub fn from_toml_str(text: &str) -> Result<Config> {
        let config: Config = toml::from_str(text)?;
        check_plugins(&config.reveal).map_err(Error::Config)?;
        Ok(config)
    }

    /// Reads and parses `path` (see [`Config::from_toml_str`]); errors name the file.
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("cannot read {}: {e}", path.display())))?;
        Self::from_toml_str(&text).map_err(|e| match e {
            Error::Config(message) => Error::Config(format!("{}: {message}", path.display())),
            other => other,
        })
    }

    /// Walks up from `start_dir` (the source file's directory; relative paths are taken
    /// from the current directory) to the filesystem root and loads the first
    /// `presenter.toml` found. Returns its absolute path with the config, or `None`.
    pub fn discover(start_dir: &Path) -> Result<Option<(PathBuf, Config)>> {
        let start = if start_dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            start_dir
        };
        let start = normalize_lexically(&std::path::absolute(start)?);
        for dir in start.ancestors() {
            let candidate = dir.join(CONFIG_FILE_NAME);
            if candidate.is_file() {
                let config = Config::load(&candidate)?;
                return Ok(Some((candidate, config)));
            }
        }
        Ok(None)
    }

    /// Applies front matter (config.md §3 rows 2–3): `meta.reveal` is merged over
    /// `self.reveal` key by key, and the `slides`, `markdown`, `math` and `notebook` maps in
    /// `meta.vars` (when they are objects; other values are ordinary vars) override only
    /// the keys they contain. They stay in `meta.vars`. Idempotent. Unknown or ill-typed keys
    /// and an invalid `reveal.plugins` list fail with [`Error::FrontMatter`] (with a "did you
    /// mean" hint) and leave `self` unchanged.
    pub fn merge_front_matter(&mut self, meta: &DeckMeta) -> Result<()> {
        check_plugins(&meta.reveal).map_err(Error::FrontMatter)?;
        let mut next = self.clone();
        for (key, value) in &meta.reveal {
            next.reveal.insert(key.clone(), value.clone());
        }
        overlay_table(&mut next.slides, "slides", meta)?;
        overlay_table(&mut next.markdown, "markdown", meta)?;
        overlay_table(&mut next.math, "math", meta)?;
        overlay_table(&mut next.notebook, "notebook", meta)?;
        *self = next;
        Ok(())
    }

    /// Adds the theme chain's `[reveal]` defaults *under* the configured values: a key is
    /// taken from `theme_reveal` only when `self.reveal` does not have it.
    pub fn merge_theme_reveal(&mut self, theme_reveal: &Map<String, Value>) {
        for (key, value) in theme_reveal {
            if !self.reveal.contains_key(key) {
                self.reveal.insert(key.clone(), value.clone());
            }
        }
    }

    /// Applies command-line overrides (highest precedence). `out` is not a config key and
    /// is ignored here.
    pub fn apply_cli(&mut self, overrides: &CliOverrides) {
        if let Some(theme) = &overrides.theme {
            self.deck.theme = theme.clone();
        }
        if let Some(standalone) = overrides.standalone {
            self.build.standalone = standalone;
        }
        if let Some(port) = overrides.port {
            self.serve.port = port;
        }
        if let Some(host) = &overrides.host {
            self.serve.host = host.clone();
        }
        if let Some(open) = overrides.open {
            self.serve.open = open;
        }
    }

    /// Applies reveal options given on the command line over every other source (the top
    /// of the `[reveal]` precedence chain). No flag in `docs/spec/cli.md` sets reveal options
    /// yet, so nothing calls this today; an invalid `plugins` list fails with
    /// [`Error::Config`] and leaves `self` unchanged.
    pub fn apply_cli_reveal(&mut self, reveal: &Map<String, Value>) -> Result<()> {
        check_plugins(reveal).map_err(Error::Config)?;
        for (key, value) in reveal {
            self.reveal.insert(key.clone(), value.clone());
        }
        Ok(())
    }

    /// The config.md §2 `[reveal]` values, including `plugins`.
    pub fn builtin_reveal_defaults() -> Map<String, Value> {
        let plugins: Vec<Value> = PluginId::ALL.iter().map(|p| p.name().into()).collect();
        [
            ("hash", Value::from(true)),
            ("slideNumber", Value::from(false)),
            ("transition", Value::from("slide")),
            ("controls", Value::from(true)),
            ("progress", Value::from(true)),
            ("center", Value::from(true)),
            ("width", Value::from(1280)),
            ("height", Value::from(720)),
            ("plugins", Value::from(plugins)),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
    }

    /// [`Config::builtin_reveal_defaults`] overlaid by `self.reveal`.
    pub fn effective_reveal(&self) -> Map<String, Value> {
        let mut reveal = Self::builtin_reveal_defaults();
        for (key, value) in &self.reveal {
            reveal.insert(key.clone(), value.clone());
        }
        reveal
    }

    /// The `Reveal.initialize` options as compact JSON: [`Config::effective_reveal`] without
    /// `plugins`, object keys sorted (recursively), integral floats written as integers.
    pub fn reveal_options_json(&self) -> String {
        let sorted: BTreeMap<String, Value> = self
            .effective_reveal()
            .into_iter()
            .filter(|(key, _)| key != "plugins")
            .map(|(key, value)| (key, canonical_json(&value)))
            .collect();
        Value::Object(sorted.into_iter().collect()).to_string()
    }

    /// The plugins named in `reveal.plugins` of [`Config::effective_reveal`], in order,
    /// without duplicates. `"math"` is accepted and ignored (KaTeX is automatic). Never
    /// fails: the sources that can be validated already were (see the module docs), and
    /// anything invalid that still got in (e.g. from a theme's `[reveal]`) is skipped with a
    /// warning. Use [`Config::validate_plugins`] for a strict check.
    pub fn enabled_plugins(&self) -> Vec<PluginId> {
        let Some(value) = self.effective_reveal().get("plugins").cloned() else {
            return Vec::new();
        };
        let (plugins, problems) = plugin_list(&value);
        for problem in problems {
            tracing::warn!("{problem}; ignored");
        }
        plugins
    }

    /// Strict check of `reveal.plugins` in [`Config::effective_reveal`]: a list of known
    /// plugin names (or `"math"`). Anything else is an [`Error::Config`] with a "did you
    /// mean" hint.
    pub fn validate_plugins(&self) -> Result<()> {
        check_plugins(&self.effective_reveal()).map_err(Error::Config)
    }
}

/// The plugins in a `reveal.plugins` value (known names, in order, deduplicated) and one
/// message per problem (not a list, a non-string entry, an unknown name).
fn plugin_list(value: &Value) -> (Vec<PluginId>, Vec<String>) {
    let mut plugins = Vec::new();
    let mut problems = Vec::new();
    let Value::Array(items) = value else {
        problems.push(format!(
            "reveal.plugins must be a list of plugin names, got {value}"
        ));
        return (plugins, problems);
    };
    for item in items {
        let Some(name) = item.as_str() else {
            problems.push(format!(
                "reveal.plugins entries must be strings, got {item}"
            ));
            continue;
        };
        match PluginId::from_name(name) {
            Some(id) if !plugins.contains(&id) => plugins.push(id),
            Some(_) => {}
            None if name == "math" => {
                tracing::debug!(
                    "reveal.plugins: \"math\" ignored (KaTeX is included automatically)"
                );
            }
            None => problems.push(unknown_plugin(name)),
        }
    }
    (plugins, problems)
}

/// `Err` with every problem of `reveal["plugins"]` joined by `"; "`; `Ok` when the key is
/// absent or valid.
fn check_plugins(reveal: &Map<String, Value>) -> std::result::Result<(), String> {
    let Some(value) = reveal.get("plugins") else {
        return Ok(());
    };
    let (_, problems) = plugin_list(value);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// Message for an unknown `reveal.plugins` entry, with a "did you mean" hint.
fn unknown_plugin(name: &str) -> String {
    let known = ["highlight", "notes", "zoom", "search", "math"];
    let hint = closest(name, known)
        .map(|suggestion| format!(" (did you mean '{suggestion}'?)"))
        .unwrap_or_default();
    format!(
        "unknown reveal plugin '{name}'{hint}; known plugins: {}",
        known.join(", ")
    )
}

/// Deserializes a JSON-like map, turning TOML dates/times into their text.
fn json_map<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Map<String, Value>, D::Error> {
    let mut map = Map::<String, Value>::deserialize(deserializer)?;
    for value in map.values_mut() {
        stringify_toml_datetimes(value);
    }
    Ok(map)
}

/// Replaces every TOML date/time inside `value` by its text (see [`toml_datetime_text`]).
fn stringify_toml_datetimes(value: &mut Value) {
    match value {
        Value::Object(map) => match toml_datetime_text(map) {
            Some(text) => *value = Value::String(text),
            None => map.values_mut().for_each(stringify_toml_datetimes),
        },
        Value::Array(items) => items.iter_mut().for_each(stringify_toml_datetimes),
        _ => {}
    }
}

/// The `toml` deserializer hands a date/time to a `serde_json::Value` as a one-entry object
/// (a private marker key mapped to the date text). Returns that date's text (`2026-10-08`)
/// when `map` is such an object, recognised by `toml`'s own `Datetime` deserializer.
fn toml_datetime_text(map: &Map<String, Value>) -> Option<String> {
    if map.len() != 1 || !map.values().all(Value::is_string) {
        return None;
    }
    serde_json::from_value::<toml::value::Datetime>(Value::Object(map.clone()))
        .ok()
        .map(|datetime| datetime.to_string())
}

/// Deserializes an optional string field that also accepts any other scalar (number,
/// boolean, TOML date/time) and stores its text. Tables and arrays are errors.
fn scalar_text<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error> {
    let not_scalar = |kind: &str| {
        serde::de::Error::custom(format!(
            "expected a string, number, boolean or date, got {kind}"
        ))
    };
    let text = match Option::<Value>::deserialize(deserializer)? {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(text)) => text,
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        Some(Value::Object(map)) => {
            toml_datetime_text(&map).ok_or_else(|| not_scalar("a table"))?
        }
        Some(Value::Array(_)) => return Err(not_scalar("an array")),
    };
    Ok(Some(text))
}

/// Overlays the object `meta.vars[name]` (if it is one) onto `table`, key by key.
fn overlay_table<T: Serialize + DeserializeOwned>(
    table: &mut T,
    name: &str,
    meta: &DeckMeta,
) -> Result<()> {
    let Some(Value::Object(overrides)) = meta.vars.get(name) else {
        return Ok(());
    };
    let mut merged = match serde_json::to_value(&*table)? {
        Value::Object(map) => map,
        other => {
            return Err(Error::Other(format!(
                "config table {name} serialized to a non-object: {other}"
            )));
        }
    };
    for (key, value) in overrides {
        merged.insert(key.clone(), value.clone());
    }
    *table = serde_json::from_value(Value::Object(merged)).map_err(|e| {
        Error::FrontMatter(format!("{name}: {}", with_unknown_key_hint(e.to_string())))
    })?;
    Ok(())
}

/// Sorts object keys recursively and turns integral floats (`1280.0`) into integers.
fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<&String, &Value> = map.iter().collect();
            Value::Object(
                sorted
                    .into_iter()
                    .map(|(key, value)| (key.clone(), canonical_json(value)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical_json).collect()),
        Value::Number(number) if number.is_f64() => match number.as_f64() {
            // |f| < 2^53 and integral, so the cast is exact.
            Some(f) if f.fract() == 0.0 && f.abs() < MAX_EXACT_INT => Value::from(f as i64),
            _ => value.clone(),
        },
        other => other.clone(),
    }
}

/// 2^53: every integer below it is exactly representable in both `f64` and `i64`.
const MAX_EXACT_INT: f64 = 9_007_199_254_740_992.0;

/// For a serde "unknown field `x`, expected …" or "unknown variant `x`, expected …" message
/// (as produced for `deny_unknown_fields` structs and enums), the expected name closest to
/// `x` if it is within Levenshtein distance 2.
pub fn unknown_key_hint(message: &str) -> Option<String> {
    let (_, rest) = message
        .split_once("unknown field `")
        .or_else(|| message.split_once("unknown variant `"))?;
    let (unknown, rest) = rest.split_once('`')?;
    let (_, expected) = rest.split_once("expected ")?;
    let expected = expected.lines().next().unwrap_or_default();
    // Names are the backtick-quoted parts: "`a`, `b` or `c`" → a, b, c.
    let candidates = expected.split('`').skip(1).step_by(2);
    closest(unknown, candidates).map(str::to_string)
}

/// `message` (trailing whitespace trimmed) with ` (did you mean '…'?)` appended when
/// [`unknown_key_hint`] finds a suggestion.
pub fn with_unknown_key_hint(message: String) -> String {
    let trimmed = message.trim_end();
    match unknown_key_hint(trimmed) {
        Some(suggestion) => format!("{trimmed} (did you mean '{suggestion}'?)"),
        None => trimmed.to_string(),
    }
}

/// The candidate with the smallest edit distance to `word` (first one on ties), if that
/// distance is 1 or 2.
fn closest<'c>(word: &str, candidates: impl IntoIterator<Item = &'c str>) -> Option<&'c str> {
    candidates
        .into_iter()
        .map(|candidate| (levenshtein(word, candidate), candidate))
        .filter(|(distance, _)| (1..=2).contains(distance))
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate)
}

/// Levenshtein edit distance over `char`s (insertions, deletions, substitutions).
fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut current = Vec::with_capacity(b.len() + 1);
        current.push(i + 1);
        for (j, cb) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(ca != *cb);
            let deletion = previous[j + 1] + 1;
            let insertion = current[j] + 1;
            current.push(substitution.min(deletion).min(insertion));
        }
        previous = current;
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_distances() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("build", "build"), 0);
        assert_eq!(levenshtein("buidl", "build"), 2);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("splt", "split"), 1);
        assert_eq!(levenshtein("sérve", "serve"), 1);
    }

    #[test]
    fn hint_parses_serde_messages() {
        let many = "unknown field `buidl`, expected one of `deck`, `build`, `slides`";
        assert_eq!(unknown_key_hint(many).as_deref(), Some("build"));
        let two = "unknown variant `heading`, expected `rules` or `headings`";
        assert_eq!(unknown_key_hint(two).as_deref(), Some("headings"));
        let one = "unknown field `opem`, expected `open`";
        assert_eq!(unknown_key_hint(one).as_deref(), Some("open"));
        let far = "unknown field `zzzzzz`, expected one of `deck`, `build`";
        assert_eq!(unknown_key_hint(far), None);
        assert_eq!(unknown_key_hint("invalid type: string"), None);
        let none = "unknown field `x`, there are no fields";
        assert_eq!(unknown_key_hint(none), None);
    }

    #[test]
    fn hint_ties_pick_the_first_candidate() {
        let msg = "unknown field `ab`, expected one of `aa`, `bb`, `ac`";
        assert_eq!(unknown_key_hint(msg).as_deref(), Some("aa"));
    }

    #[test]
    fn with_hint_trims_and_appends() {
        assert_eq!(
            with_unknown_key_hint("unknown field `buidl`, expected `build`\n".into()),
            "unknown field `buidl`, expected `build` (did you mean 'build'?)"
        );
        assert_eq!(with_unknown_key_hint("other\n".into()), "other");
    }

    #[test]
    fn canonical_json_sorts_and_drops_trailing_zero() {
        let value = serde_json::json!({"b": 1280.0, "a": {"y": 0.5, "x": [2.0, -3.0]}});
        assert_eq!(
            canonical_json(&value).to_string(),
            r#"{"a":{"x":[2,-3],"y":0.5},"b":1280}"#
        );
    }

    #[test]
    fn plugin_names_round_trip() {
        for id in PluginId::ALL {
            assert_eq!(PluginId::from_name(id.name()), Some(id));
            assert_eq!(serde_json::to_value(id).unwrap(), Value::from(id.name()));
        }
        assert_eq!(PluginId::from_name("math"), None);
        assert_eq!(PluginId::from_name("Highlight"), None);
    }
}
