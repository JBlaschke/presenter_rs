//! reveal.js initialisation options and plugin wiring (theme-format.md §4 `reveal`,
//! config.md §4).
//!
//! Option precedence (config.md §1 and its contract changes): built-in defaults
//! ([`Config::builtin_reveal_defaults`]) < the theme chain's `[reveal]` < `presenter.toml`
//! `[reveal]` < front matter `reveal:` < the command line. `plugins` is not a reveal option:
//! it selects the plugin scripts ([`plugins`]) and is stripped from [`reveal_config_json`].
//! The renderer works on the effective config and uses [`Config::reveal_options_json`];
//! [`reveal_config_json`] computes the same JSON from a config that is not merged yet.

use serde_json::{Map, Value};

use crate::config::{Config, MathMode, PluginId, canonical_json};
use crate::ir::{DeckMeta, Features};
use crate::theme::Theme;

/// A reveal.js plugin as the templates include it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginInfo {
    /// Which plugin.
    pub id: PluginId,
    /// The global the plugin script defines, passed to `Reveal.initialize({ plugins })`,
    /// e.g. `"RevealHighlight"`.
    pub js_ident: &'static str,
    /// The plugin script, a `vendor/…` helper path, e.g.
    /// `"vendor/reveal/dist/plugin/highlight.js"`.
    pub script: &'static str,
}

impl PluginInfo {
    /// The script and global of plugin `id` (reveal.js 6: `dist/plugin/<name>.js`).
    pub fn of(id: PluginId) -> PluginInfo {
        let (js_ident, script) = match id {
            PluginId::Highlight => ("RevealHighlight", "vendor/reveal/dist/plugin/highlight.js"),
            PluginId::Notes => ("RevealNotes", "vendor/reveal/dist/plugin/notes.js"),
            PluginId::Zoom => ("RevealZoom", "vendor/reveal/dist/plugin/zoom.js"),
            PluginId::Search => ("RevealSearch", "vendor/reveal/dist/plugin/search.js"),
        };
        PluginInfo {
            id,
            js_ident,
            script,
        }
    }
}

/// The plugins to include, in the order of the effective `reveal.plugins` list
/// ([`Config::enabled_plugins`]):
/// - `highlight` only when the deck has code (`features.code`): its script is ~900 KB;
/// - `notes` only when the deck has speaker notes (`features.notes`) or `notes` is listed
///   explicitly in `config.reveal.plugins` (the speaker view is useful without notes too,
///   but the built-in default list alone does not pull it in);
/// - `zoom` and `search` whenever enabled.
///
/// Pass the effective config ([`crate::render::context::effective_config`]) so plugins
/// listed by the theme or the front matter count as explicit.
pub fn plugins(config: &Config, features: &Features) -> Vec<PluginInfo> {
    let explicit = explicitly_listed(config);
    config
        .enabled_plugins()
        .into_iter()
        .filter(|id| match id {
            PluginId::Highlight => features.code,
            PluginId::Notes => features.notes || explicit.contains(&PluginId::Notes),
            PluginId::Zoom | PluginId::Search => true,
        })
        .map(PluginInfo::of)
        .collect()
}

/// The plugins named in `config.reveal.plugins` itself (not the built-in default list).
fn explicitly_listed(config: &Config) -> Vec<PluginId> {
    let Some(Value::Array(items)) = config.reveal.get("plugins") else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(Value::as_str)
        .filter_map(PluginId::from_name)
        .collect()
}

/// The `Reveal.initialize` options as JSON: built-ins ← `theme.reveal_defaults()` ←
/// `config.reveal` ← `meta.reveal` (right wins, top-level keys), `plugins` removed, keys
/// sorted recursively, integral numbers without a trailing `.0`. `width`/`height` come from
/// the built-in defaults (1280×720) unless a layer sets them.
///
/// For a `presenter.toml` config that is not merged yet. Do not pass an effective config
/// with command-line reveal options: `meta.reveal` would be applied over them again (the
/// renderer calls [`Config::reveal_options_json`] on the effective config instead).
pub fn reveal_config_json(config: &Config, meta: &DeckMeta, theme: &Theme) -> String {
    let mut merged = config.clone();
    merged.merge_theme_reveal(&theme.reveal_defaults());
    for (key, value) in &meta.reveal {
        merged.reveal.insert(key.clone(), value.clone());
    }
    merged.reveal_options_json()
}

/// True when KaTeX is included: `math.enabled = "always"`, or `"auto"` and the deck has
/// math. The default theme's templates apply the same rule to the KaTeX stylesheet and
/// scripts.
pub fn math_enabled(config: &Config, features: &Features) -> bool {
    match config.math.enabled {
        MathMode::Always => true,
        MathMode::Auto => features.math,
        MathMode::Never => false,
    }
}

/// The `<script>` emitted by the `reveal_init()` helper:
/// `Reveal.initialize(Object.assign(<config_json>, { plugins: [<idents>] }));`, preceded by
/// `window.__presenterKatexOptions = <json>;` when `katex_options` is given (math enabled).
///
/// JSON is embedded with `<` written as `\u003c`, so option values can never close the
/// script element.
pub fn reveal_init_script(
    config_json: &str,
    plugins: &[PluginInfo],
    katex_options: Option<&Map<String, Value>>,
) -> String {
    let mut script = String::from("<script>");
    if let Some(options) = katex_options {
        let options = canonical_json(&Value::Object(options.clone())).to_string();
        script.push_str(&format!(
            "window.__presenterKatexOptions = {};\n",
            script_safe_json(&options)
        ));
    }
    let idents: Vec<&str> = plugins.iter().map(|p| p.js_ident).collect();
    script.push_str(&format!(
        "Reveal.initialize(Object.assign({}, {{ plugins: [{}] }}));</script>",
        script_safe_json(config_json),
        idents.join(", ")
    ));
    script
}

/// JSON text safe inside an HTML `<script>`: `<` (only possible inside JSON strings)
/// becomes the JSON escape `\u003c`, which also neutralizes `<!--`.
fn script_safe_json(json: &str) -> String {
    json.replace('<', "\\u003c")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("not an object: {other}"),
        }
    }

    fn config_with(reveal: Value) -> Config {
        Config {
            reveal: object(reveal),
            ..Config::default()
        }
    }

    fn idents(plugins: &[PluginInfo]) -> Vec<&'static str> {
        plugins.iter().map(|p| p.js_ident).collect()
    }

    #[test]
    fn default_plugins_follow_features() {
        let config = Config::default();
        let none = Features::default();
        assert_eq!(
            idents(&plugins(&config, &none)),
            ["RevealZoom", "RevealSearch"]
        );
        let all = Features {
            math: true,
            code: true,
            notes: true,
            notebook: false,
        };
        assert_eq!(
            idents(&plugins(&config, &all)),
            [
                "RevealHighlight",
                "RevealNotes",
                "RevealZoom",
                "RevealSearch"
            ]
        );
        let scripts: Vec<&str> = plugins(&config, &all).iter().map(|p| p.script).collect();
        assert_eq!(
            scripts,
            [
                "vendor/reveal/dist/plugin/highlight.js",
                "vendor/reveal/dist/plugin/notes.js",
                "vendor/reveal/dist/plugin/zoom.js",
                "vendor/reveal/dist/plugin/search.js"
            ]
        );
    }

    #[test]
    fn explicit_lists_select_and_order_plugins() {
        let config = config_with(json!({"plugins": ["search", "notes", "highlight"]}));
        // notes: listed explicitly, so included without notes in the deck.
        assert_eq!(
            idents(&plugins(&config, &Features::default())),
            ["RevealSearch", "RevealNotes"]
        );
        let code = Features {
            code: true,
            ..Features::default()
        };
        assert_eq!(
            idents(&plugins(&config, &code)),
            ["RevealSearch", "RevealNotes", "RevealHighlight"]
        );
        // Not listed: never included, even when the deck has code and notes.
        let config = config_with(json!({"plugins": ["zoom"]}));
        let all = Features {
            code: true,
            notes: true,
            ..Features::default()
        };
        assert_eq!(idents(&plugins(&config, &all)), ["RevealZoom"]);
        let config = config_with(json!({"plugins": []}));
        assert!(plugins(&config, &all).is_empty());
    }

    #[test]
    fn math_rule() {
        let mut config = Config::default();
        let math = Features {
            math: true,
            ..Features::default()
        };
        let plain = Features::default();
        assert!(math_enabled(&config, &math));
        assert!(!math_enabled(&config, &plain));
        config.math.enabled = MathMode::Always;
        assert!(math_enabled(&config, &plain));
        config.math.enabled = MathMode::Never;
        assert!(!math_enabled(&config, &math));
    }

    #[test]
    fn init_script_shape() {
        let plugins = [
            PluginInfo::of(PluginId::Highlight),
            PluginInfo::of(PluginId::Notes),
        ];
        let script = reveal_init_script(r#"{"hash":true}"#, &plugins, None);
        assert_eq!(
            script,
            "<script>Reveal.initialize(Object.assign({\"hash\":true}, { plugins: [RevealHighlight, RevealNotes] }));</script>"
        );
        let options = object(json!({"macros": {"\\R": "\\mathbb{R}"}, "a": 1.0}));
        let script = reveal_init_script(r#"{"x":"</script>"}"#, &[], Some(&options));
        assert_eq!(
            script,
            "<script>window.__presenterKatexOptions = {\"a\":1,\"macros\":{\"\\\\R\":\"\\\\mathbb{R}\"}};\nReveal.initialize(Object.assign({\"x\":\"\\u003c/script>\"}, { plugins: [] }));</script>"
        );
    }
}
