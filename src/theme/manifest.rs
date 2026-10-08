//! `theme.toml` manifest parsing (`docs/spec/theme-format.md` §2).
//!
//! Every table is optional and unknown keys are errors (with a "did you mean" hint), so a
//! typo in a theme fails loudly instead of being ignored. `name` is required. TOML
//! dates/times inside `[reveal]` and `[vars]` are stored as their text, like in
//! `presenter.toml`.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer};
use serde_json::{Map, Number, Value};

use crate::error::{Error, Result};

/// The stock reveal.js themes `[styles] reveal_theme` may name (besides `"none"`).
pub const REVEAL_THEMES: [&str; 14] = [
    "black",
    "white",
    "league",
    "beige",
    "night",
    "serif",
    "simple",
    "solarized",
    "moon",
    "dracula",
    "sky",
    "blood",
    "black-contrast",
    "white-contrast",
];

/// A parsed `theme.toml`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeManifest {
    /// Theme name (required); used in error messages, `theme list` and `@<name>/` template
    /// names.
    pub name: String,
    /// Parent theme: a built-in theme name or a path relative to this theme's directory.
    pub extends: Option<String>,
    /// One-line description for `theme list`.
    pub description: Option<String>,
    /// `[styles]`.
    pub styles: StylesSection,
    /// `[scripts]`.
    pub scripts: ScriptsSection,
    /// `[reveal]`: reveal option defaults of this theme (deep-merged along the chain).
    #[serde(deserialize_with = "json_map")]
    pub reveal: Map<String, Value>,
    /// `[layouts]`: documentation of the layouts this theme (or its parents) provide,
    /// layout name → description.
    pub layouts: BTreeMap<String, String>,
    /// `[vars]`: free-form values exposed to templates as `theme.vars.*` (deep-merged along
    /// the chain).
    #[serde(deserialize_with = "json_map")]
    pub vars: Map<String, Value>,
}

/// The `[styles]` table.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StylesSection {
    /// Stylesheet entry points relative to the theme directory, compiled in order.
    pub entries: Vec<String>,
    /// Stock reveal theme included first: one of [`REVEAL_THEMES`] or `"none"`.
    pub reveal_theme: Option<String>,
    /// `"monokai"`, `"zenburn"`, `"none"` or a path relative to the theme directory.
    pub highlight_style: Option<String>,
}

/// The `[scripts]` table.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScriptsSection {
    /// Script files relative to the theme directory, included before `Reveal.initialize`.
    pub entries: Vec<String>,
}

impl ThemeManifest {
    /// Parses and validates the text of a `theme.toml`.
    ///
    /// `theme` (the theme name or path as requested) and `origin` (where the text came
    /// from, e.g. the file path) only label errors. Fails with [`Error::Theme`] on TOML
    /// syntax errors and unknown keys (the message keeps the `line N, column M` from
    /// `toml` and adds a "did you mean" hint), a missing or empty `name`, and an unknown
    /// `reveal_theme`.
    pub fn from_toml_str(text: &str, theme: &str, origin: &str) -> Result<ThemeManifest> {
        let error = |message: String| Error::Theme {
            name: theme.to_string(),
            message: format!("{origin}: {message}"),
        };
        let manifest: ThemeManifest = toml::from_str(text)
            .map_err(|e| error(crate::config::with_unknown_key_hint(e.to_string())))?;
        if manifest.name.trim().is_empty() {
            return Err(error("missing required key 'name'".to_string()));
        }
        if let Some(reveal_theme) = &manifest.styles.reveal_theme
            && reveal_theme != "none"
            && !REVEAL_THEMES.contains(&reveal_theme.as_str())
        {
            return Err(error(format!(
                "unknown [styles] reveal_theme '{reveal_theme}' (expected one of: {}, none)",
                REVEAL_THEMES.join(", ")
            )));
        }
        Ok(manifest)
    }
}

/// Deserializes a TOML table into a JSON map, turning dates/times into their text.
fn json_map<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Map<String, Value>, D::Error> {
    let table = toml::Table::deserialize(deserializer)?;
    table
        .into_iter()
        .map(|(key, value)| Ok((key, toml_to_json(value).map_err(serde::de::Error::custom)?)))
        .collect()
}

/// Converts a TOML value to JSON. Dates/times become strings (`2026-10-08`); non-finite
/// floats, which JSON cannot represent, are an error.
fn toml_to_json(value: toml::Value) -> std::result::Result<Value, String> {
    Ok(match value {
        toml::Value::String(s) => Value::String(s),
        toml::Value::Integer(i) => Value::from(i),
        toml::Value::Float(f) => Value::Number(
            Number::from_f64(f).ok_or_else(|| format!("non-finite float {f} is not supported"))?,
        ),
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Datetime(dt) => Value::String(dt.to_string()),
        toml::Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(toml_to_json)
                .collect::<std::result::Result<_, _>>()?,
        ),
        toml::Value::Table(table) => Value::Object(
            table
                .into_iter()
                .map(|(key, value)| Ok((key, toml_to_json(value)?)))
                .collect::<std::result::Result<_, String>>()?,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(text: &str) -> Result<ThemeManifest> {
        ThemeManifest::from_toml_str(text, "t", "t/theme.toml")
    }

    fn theme_message(err: Error) -> String {
        match err {
            Error::Theme { name, message } => {
                assert_eq!(name, "t");
                message
            }
            other => panic!("expected Error::Theme, got {other:?}"),
        }
    }

    #[test]
    fn parses_every_section() {
        let manifest = parse(
            r##"
            name = "mytheme"
            extends = "default"
            description = "Dark theme"
            [styles]
            entries = ["styles/a.css", "styles/b.scss"]
            reveal_theme = "black"
            highlight_style = "zenburn"
            [scripts]
            entries = ["scripts/extra.js"]
            [reveal]
            transition = "fade"
            width = 1280
            nested = { a = 1.5, b = [true, "x"] }
            [layouts]
            title = "Centered title slide"
            [vars]
            accent = "#2a76dd"
            released = 2026-10-08
            "##,
        )
        .unwrap();
        assert_eq!(manifest.name, "mytheme");
        assert_eq!(manifest.extends.as_deref(), Some("default"));
        assert_eq!(manifest.description.as_deref(), Some("Dark theme"));
        assert_eq!(manifest.styles.entries, ["styles/a.css", "styles/b.scss"]);
        assert_eq!(manifest.styles.reveal_theme.as_deref(), Some("black"));
        assert_eq!(manifest.styles.highlight_style.as_deref(), Some("zenburn"));
        assert_eq!(manifest.scripts.entries, ["scripts/extra.js"]);
        assert_eq!(
            Value::Object(manifest.reveal),
            json!({"transition": "fade", "width": 1280, "nested": {"a": 1.5, "b": [true, "x"]}})
        );
        assert_eq!(manifest.layouts["title"], "Centered title slide");
        assert_eq!(
            Value::Object(manifest.vars),
            json!({"accent": "#2a76dd", "released": "2026-10-08"})
        );
    }

    #[test]
    fn minimal_manifest_has_empty_sections() {
        let manifest = parse(r#"name = "x""#).unwrap();
        assert_eq!(
            manifest,
            ThemeManifest {
                name: "x".into(),
                ..ThemeManifest::default()
            }
        );
    }

    #[test]
    fn unknown_keys_are_theme_errors_with_position_and_hint() {
        let message = theme_message(parse("name = \"x\"\n[styles]\nentrys = []\n").unwrap_err());
        assert!(message.starts_with("t/theme.toml: "), "{message}");
        assert!(message.contains("line 3"), "{message}");
        assert!(message.contains("did you mean 'entries'"), "{message}");

        let message = theme_message(parse("name = \"x\"\ncolour = 1\n").unwrap_err());
        assert!(message.contains("unknown field `colour`"), "{message}");
    }

    #[test]
    fn syntax_errors_keep_line_numbers() {
        let message = theme_message(parse("name = \"x\"\nextends = = 1\n").unwrap_err());
        assert!(message.contains("line 2"), "{message}");
    }

    #[test]
    fn name_is_required() {
        let message = theme_message(parse("description = \"no name\"").unwrap_err());
        assert!(message.contains("missing required key 'name'"), "{message}");
        assert!(parse("name = \"  \"").is_err());
    }

    #[test]
    fn reveal_theme_must_be_a_stock_theme_or_none() {
        for ok in ["white", "black-contrast", "none"] {
            parse(&format!("name = \"x\"\n[styles]\nreveal_theme = \"{ok}\"")).unwrap();
        }
        let message =
            theme_message(parse("name = \"x\"\n[styles]\nreveal_theme = \"whte\"").unwrap_err());
        assert!(
            message.contains("unknown [styles] reveal_theme 'whte'"),
            "{message}"
        );
    }

    #[test]
    fn every_stock_reveal_theme_is_vendored() {
        for name in REVEAL_THEMES {
            let path = format!("vendor/reveal/dist/theme/{name}.css");
            assert!(crate::vendor::get(&path).is_some(), "{path}");
        }
    }

    #[test]
    fn non_finite_floats_are_rejected() {
        assert!(parse("name = \"x\"\n[vars]\nx = nan\n").is_err());
    }
}
