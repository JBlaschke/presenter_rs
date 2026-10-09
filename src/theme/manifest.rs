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
    /// `toml` and adds a "did you mean" hint), a missing or empty `name`, a `name` that
    /// cannot be used in `@<name>/` template names (see [`check_theme_name`]), and an
    /// unknown `reveal_theme`.
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
        check_theme_name(&manifest.name).map_err(error)?;
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

/// Checks that `name` can be a theme name. It is the `<name>` of `@<name>/<path>` template
/// names, which end at the first `/`, so it must be non-empty and must not contain `/`,
/// `\`, `@` or control characters, nor start or end with whitespace. The error message
/// says why.
pub fn check_theme_name(name: &str) -> std::result::Result<(), String> {
    let problem = if name.trim().is_empty() {
        "is empty"
    } else if name.trim() != name {
        "starts or ends with whitespace"
    } else if name.contains(['/', '\\', '@']) {
        "contains '/', '\\' or '@'"
    } else if name.chars().any(char::is_control) {
        "contains a control character"
    } else {
        return Ok(());
    };
    Err(format!(
        "invalid theme name {name:?}: it {problem} (the name is used in '@<name>/…' template names)"
    ))
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

// ---------------------------------------------------------------------------------------
// Lines of values in a `theme.toml` text, for located diagnostics. The same rules as
// `presenter theme check` (src/cli/theme.rs `ManifestFile`), so that `check` and
// `theme check` report a problem at the same line (audit F10 review).

/// The 1-based line of `text` (a `theme.toml`) where `value` is written as a quoted TOML
/// string in the value of `key` in `[table]` (an array may span several lines), comments
/// ignored. When it is not written that way (a dotted key, an inline table), the first line
/// that has it outside a comment.
pub(crate) fn line_of_value(text: &str, table: &str, key: &str, value: &str) -> Option<usize> {
    let quoted = [format!("\"{value}\""), format!("'{value}'")];
    let has_value = |code: &str| quoted.iter().any(|q| code.contains(q.as_str()));
    let mut current: Option<String> = None;
    let mut in_value = false;
    let scoped = line_where(text, |line| {
        let code = toml_code(line).trim();
        if let Some(header) = table_header(code) {
            current = Some(header);
            in_value = false;
            return false;
        }
        if let Some((name, rest)) = split_key(code) {
            in_value = current.as_deref() == Some(table) && is_key(name, key);
            return in_value && has_value(rest);
        }
        in_value && has_value(code)
    });
    scoped.or_else(|| line_where(text, |line| has_value(toml_code(line))))
}

/// The 1-based line of `key = …` (bare or quoted key) in the table `[table]` of `text`.
pub(crate) fn line_of_key(text: &str, table: &str, key: &str) -> Option<usize> {
    let mut current: Option<String> = None;
    line_where(text, |line| {
        let code = toml_code(line).trim();
        if let Some(header) = table_header(code) {
            current = Some(header);
            return false;
        }
        current.as_deref() == Some(table)
            && split_key(code).is_some_and(|(name, _)| is_key(name, key))
    })
}

/// The 1-based number of the first line of `text` for which `matches` is true.
fn line_where(text: &str, matches: impl FnMut(&str) -> bool) -> Option<usize> {
    text.lines().position(matches).map(|index| index + 1)
}

/// The byte index of the first `stop` character of `line` outside a TOML string.
fn outside_strings(line: &str, stop: char) -> Option<usize> {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (index, c) in line.char_indices() {
        match quote {
            Some('"') if escaped => escaped = false,
            Some('"') if c == '\\' => escaped = true,
            Some(open) if c == open => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == stop => return Some(index),
            None => {}
        }
    }
    None
}

/// `line` without its comment: the text before the first `#` outside a TOML string.
fn toml_code(line: &str) -> &str {
    outside_strings(line, '#').map_or(line, |at| &line[..at])
}

/// The table a header line (comment removed, trimmed) opens, without brackets or spaces:
/// `[ styles ]` → `styles`. `None` for any other line.
fn table_header(code: &str) -> Option<String> {
    let inner = code.strip_prefix('[')?.strip_suffix(']')?;
    let inner = inner.trim_start_matches('[').trim_end_matches(']');
    Some(inner.chars().filter(|c| !c.is_whitespace()).collect())
}

/// A `key = value` line (comment removed) split at its first `=` outside a string: the
/// trimmed key and the rest. `None` for a line without one (an array continuation line).
fn split_key(code: &str) -> Option<(&str, &str)> {
    let at = outside_strings(code, '=')?;
    Some((code[..at].trim(), &code[at + 1..]))
}

/// True if the written key `name` (bare, `"quoted"` or `'quoted'`) is `key`.
fn is_key(name: &str, key: &str) -> bool {
    name == key
        || ['"', '\''].iter().any(|quote| {
            name.strip_prefix(*quote)
                .and_then(|rest| rest.strip_suffix(*quote))
                .is_some_and(|inner| inner == key)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lines_of_values_follow_tables_strings_and_comments() {
        let text = "name = \"t\" # entries = [\"a.css\"]\n\
                    [scripts]\n\
                    entries = [\"a.css\"]\n\
                    [ styles ]\n\
                    highlight_style = 'a.css'\n\
                    entries = [\n  \"x#y.css\", # \"b.css\"\n  \"a.css\",\n]\n\
                    [reveal]\n\
                    \"plugins\" = [\"zoom\"]\n";
        assert_eq!(line_of_value(text, "styles", "entries", "a.css"), Some(8));
        assert_eq!(line_of_value(text, "scripts", "entries", "a.css"), Some(3));
        assert_eq!(
            line_of_value(text, "styles", "highlight_style", "a.css"),
            Some(5)
        );
        assert_eq!(line_of_value(text, "styles", "entries", "x#y.css"), Some(7));
        // In a comment only: not found in the table; no other line has it either.
        assert_eq!(line_of_value(text, "styles", "entries", "b.css"), None);
        // Not in the table it is looked for in: the first line that has it.
        assert_eq!(line_of_value(text, "reveal", "plugins", "a.css"), Some(3));
        assert_eq!(line_of_key(text, "reveal", "plugins"), Some(11));
        assert_eq!(line_of_key(text, "styles", "plugins"), None);
        // The pieces (moved here from `presenter theme check`'s own copy, wave 8).
        assert_eq!(toml_code("a = \"#\" # c"), "a = \"#\" ");
        assert_eq!(toml_code("a = 'x\\' # c"), "a = 'x\\' ");
        assert_eq!(toml_code("a = \"x\\\"#\" # c"), "a = \"x\\\"#\" ");
        assert_eq!(table_header("[[ a.b ]]").as_deref(), Some("a.b"));
        assert_eq!(table_header("\"x\","), None);
        assert_eq!(split_key("\"a = b\","), None);
        assert_eq!(split_key("'k' = 1"), Some(("'k'", " 1")));
    }

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
    fn names_must_be_usable_in_at_template_names() {
        for ok in ["default", "child-theme", "My Theme", "thème_2"] {
            assert_eq!(check_theme_name(ok), Ok(()), "{ok}");
        }
        for bad in [
            "acme/dark",
            "a\\b",
            "@acme",
            "a@b",
            " x",
            "x ",
            "",
            "a\tb",
            "a\nb",
        ] {
            assert!(check_theme_name(bad).is_err(), "{bad:?}");
        }
        let message = theme_message(parse("name = \"acme/dark\"").unwrap_err());
        assert!(message.starts_with("t/theme.toml: "), "{message}");
        assert!(
            message.contains("invalid theme name \"acme/dark\""),
            "{message}"
        );
        assert!(message.contains("contains '/'"), "{message}");
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
