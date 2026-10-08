//! YAML front matter → [`DeckMeta`] (markdown-dialect.md §2.1, config.md §3). Implemented by T10.
//!
//! There are two ways to get at the YAML text, and one parser for it:
//! - Markdown files: comrak extracts the block as `NodeValue::FrontMatter(literal)`;
//!   [`strip_delimiters`] turns that literal into the YAML between the `---` lines.
//! - Sources comrak does not see (the first markdown cell of a notebook):
//!   [`split_front_matter`] finds the block in plain text, by the same rules as comrak.
//! - [`parse_front_matter`] turns the YAML into a [`DeckMeta`]; [`meta_from_json`] applies
//!   the same key rules to a JSON object (a notebook's `metadata.presenter`,
//!   notebook-mapping.md §5).
//!
//! The per-deck config overrides (`slides:`, `markdown:`, `math:`, `notebook:` maps) are
//! ordinary entries of [`DeckMeta::vars`]. They are applied, together with
//! [`DeckMeta::reveal`], by
//! [`Config::merge_front_matter`](crate::config::Config::merge_front_matter), which takes
//! `&mut self`, so merge into a copy that you keep:
//!
//! ```
//! # use presenter::{config::Config, markdown::frontmatter::parse_front_matter};
//! # let config = Config::default();
//! let meta = parse_front_matter("slides: {split: headings}")?;
//! let mut deck_config = config.clone();
//! deck_config.merge_front_matter(&meta)?;
//! assert_eq!(deck_config.slides.split, presenter::config::SlideSplit::Headings);
//! # Ok::<(), presenter::error::Error>(())
//! ```
//!
//! # YAML flavour (`serde_norway` 0.9)
//! `serde_norway` resolves plain scalars with the **YAML 1.2 core schema**, not YAML 1.1:
//! - booleans are only `true`/`false` (also `True`, `TRUE`, `False`, `FALSE`); the YAML 1.1
//!   spellings `yes`, `no`, `on`, `off`, `y`, `n` are **strings** (unlike PyYAML), so
//!   `incremental_lists: yes` under `slides:` is rejected as "expected a boolean";
//! - null is `null`, `Null`, `NULL`, `~` or an empty value;
//! - integers are decimal, `0x…`, `0o…` or `0b…`; a leading zero (`010`) makes a string;
//!   an integer outside the 64-bit range has no JSON number form and becomes the string of
//!   its decimal digits in `vars` and `reveal` (`n: 99999999999999999999` →
//!   `"99999999999999999999"`), up to 128 bits; serde_norway reads even longer digit
//!   strings as floats (`1e42`);
//! - floats include `.inf`/`.nan`, which have no JSON form and become `null` in `vars`;
//! - there is no timestamp type: `date: 2026-10-08` is the string `"2026-10-08"`.
//!
//! The string fields of [`DeckMeta`] take any scalar as its text exactly as written
//! (`title: 42` → `"42"`, `subtitle: 1.50` → `"1.50"`, `author: yes` → `"yes"`).
//! Keys must be unique in every mapping (a repeated key is an error, as YAML requires).
//! Custom tags (`!expr x`) are rejected, except on the string fields, where the scalar's
//! text is used and the tag ignored.

use std::collections::BTreeSet;
use std::fmt;

use serde::Deserialize;
use serde::de::{
    self, DeserializeSeed, Deserializer, EnumAccess, IgnoredAny, MapAccess, SeqAccess, Visitor,
};
use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::ir::DeckMeta;

/// The front matter delimiter line (dialect §1: `front_matter_delimiter = "---"`).
const DELIMITER: &str = "---";

/// Byte-order mark that may precede the opening delimiter (comrak skips it too).
const BOM: char = '\u{feff}';

/// The front matter error for a root that is not a mapping (dialect §2.1).
const NOT_A_MAPPING: &str = "expected a mapping";

/// The keys that map to the [`DeckMeta`] string fields.
const STRING_FIELDS: [&str; 7] = [
    "title",
    "subtitle",
    "author",
    "date",
    "description",
    "lang",
    "theme",
];

/// Keys whose value must be a mapping (or empty), each with an example for error messages:
/// `reveal` (→ [`DeckMeta::reveal`]) and the config tables of config.md §3 (→ `vars`).
const TABLES: [(&str, &str); 5] = [
    ("reveal", "reveal: {transition: fade}"),
    ("slides", "slides: {split: headings}"),
    ("markdown", "markdown: {smart_punctuation: true}"),
    ("math", "math: {enabled: never}"),
    ("notebook", "notebook: {show_prompts: false}"),
];

/// `std`'s two-parameter `Result`, for serde trait methods (the crate's `Result` has one).
type DeResult<T, E> = std::result::Result<T, E>;

/// Text-based extraction for sources comrak does not see (notebook cells).
/// Returns (yaml_without_delimiters, rest_of_text, number_of_lines_consumed) when `text`
/// starts with a line `---` and a closing `---` line exists.
///
/// Both delimiter lines must be exactly `---` (no trailing spaces), terminated by `\n` or
/// `\r\n`; the closing one may also end the text. As in comrak, the line right after the
/// opening delimiter never closes the block, so `---\n---\n` is not front matter (an empty
/// block needs a blank line: `---\n\n---\n`). The YAML excludes the line break before the
/// closing delimiter, and `rest_of_text` starts right after the closing line's break (a
/// following blank line stays in it). `number_of_lines_consumed` counts the opening line,
/// the YAML lines and the closing line, so `rest_of_text` begins on line `consumed + 1`. A
/// leading byte-order mark is skipped.
///
/// ```
/// use presenter::markdown::frontmatter::split_front_matter;
/// assert_eq!(split_front_matter("---\na: 1\n---\nbody"), Some(("a: 1", "body", 3)));
/// assert_eq!(split_front_matter("# no front matter"), None);
/// ```
pub fn split_front_matter(text: &str) -> Option<(&str, &str, usize)> {
    let text = text.strip_prefix(BOM).unwrap_or(text);
    let mut lines = lines_with_ends(text);
    let (first, yaml_start) = lines.next()?;
    // `yaml_start == first.len()` means the opening line has no line break.
    if first != DELIMITER || yaml_start == first.len() {
        return None;
    }
    let mut line_start = yaml_start;
    for (consumed, (line, end)) in (2..).zip(lines) {
        // comrak searches for `\n---` only after the opening line's break, so the closing
        // delimiter is line 3 at the earliest.
        if consumed > 2 && line == DELIMITER {
            let yaml = without_line_break(&text[yaml_start..line_start]);
            return Some((yaml, &text[end..], consumed));
        }
        line_start = end;
    }
    None
}

/// Strip the `---` delimiter lines from comrak's `NodeValue::FrontMatter` literal.
///
/// comrak's literal is the raw block: the opening `---` line, the YAML, the closing `---`
/// line and, when present, one following blank line (`\n` or `\r\n` line breaks, possibly
/// no break at all when the block ends the document). The result is the YAML without the
/// line break before the closing delimiter — the same text [`split_front_matter`] returns
/// for the same block. A delimiter line that is missing is simply not stripped.
pub fn strip_delimiters(literal: &str) -> &str {
    let mut body = literal.strip_prefix(BOM).unwrap_or(literal);
    if let Some(rest) = body
        .strip_prefix(DELIMITER)
        .and_then(strip_leading_line_break)
    {
        body = rest;
    }
    // Drops the blank line comrak keeps after the block and the closing line's break.
    let body = body.trim_end_matches(['\r', '\n']);
    if body == DELIMITER {
        return "";
    }
    match body.strip_suffix(DELIMITER) {
        Some(rest) if rest.ends_with('\n') => without_line_break(rest),
        _ => body,
    }
}

/// Parse YAML into deck metadata. Unknown keys go to `meta.vars`.
///
/// - Empty YAML (or only comments, or `null`) → [`DeckMeta::default()`].
/// - A root that is not a mapping → [`Error::FrontMatter`]`("expected a mapping")`.
/// - `title subtitle author date description lang theme` → the [`DeckMeta`] fields; any
///   scalar is taken as its YAML text (`date: 2026-10-08` → `"2026-10-08"`, `title: 42` →
///   `"42"`), `null` leaves the field `None`, a sequence or mapping is an error.
/// - `reveal` → [`DeckMeta::reveal`]. It must be a mapping; an empty value (`reveal:`, e.g.
///   with all its options commented out) counts as an empty mapping.
/// - Every other key → [`DeckMeta::vars`] in the order written, values as typed by YAML
///   (see the module docs). This includes the `slides`/`markdown`/`math`/`notebook` config
///   overrides, which stay in `vars` (config.md §3); like `reveal` they must be mappings
///   or empty (`slides: headings` is an error). Non-string scalar keys become their text.
/// - A key repeated in any mapping is an error (``duplicate key `title` ``), reported at
///   the repeated key.
///
/// Every failure, including YAML syntax errors, is an [`Error::FrontMatter`]. Line numbers
/// in messages are those of the source the block came from: the YAML is taken to start on
/// line 2, right after the opening `---` on line 1 (always the case for a Markdown file
/// and for a notebook cell), so `title: a: b` as the first YAML line is reported on line 2.
pub fn parse_front_matter(yaml: &str) -> Result<DeckMeta> {
    // One leading line break shifts serde_norway's line numbers to source lines.
    let source = format!("\n{yaml}");
    let root = serde_norway::Deserializer::from_str(&source)
        .deserialize_any(RootVisitor)
        .map_err(|err| Error::FrontMatter(err.to_string()))?;
    let meta = root.ok_or_else(|| Error::FrontMatter(NOT_A_MAPPING.to_string()))?;
    tracing::debug!(
        vars = meta.vars.len(),
        reveal = meta.reveal.len(),
        "parsed front matter"
    );
    Ok(meta)
}

/// Deck metadata from a JSON value, with the key rules of [`parse_front_matter`]: for a
/// notebook's `metadata.presenter` object ("same keys as Markdown front matter",
/// notebook-mapping.md §5).
///
/// `null` → [`DeckMeta::default()`]; any other non-object → `"expected a mapping"`. A
/// number or boolean for a string field becomes its JSON text (`42` → `"42"`, `1.50` →
/// `"1.5"`). Messages start with the offending key and carry no line numbers.
pub fn meta_from_json(value: &Value) -> Result<DeckMeta> {
    let object = match value {
        Value::Null => return Ok(DeckMeta::default()),
        Value::Object(object) => object,
        _ => return Err(Error::FrontMatter(NOT_A_MAPPING.to_string())),
    };
    let mut meta = DeckMeta::default();
    for (key, value) in object {
        let value = match value {
            Value::Bool(_) | Value::Number(_) if STRING_FIELDS.contains(&key.as_str()) => {
                Value::String(value.to_string())
            }
            _ => value.clone(),
        };
        let entry = EntrySeed {
            meta: &mut meta,
            key: key.clone(),
        };
        entry
            .deserialize(value)
            .map_err(|err| Error::FrontMatter(format!("{key}: {err}")))?;
    }
    Ok(meta)
}

/// The front matter root: `Some(meta)` for a mapping or an empty document, `None` for any
/// other value (reported as [`NOT_A_MAPPING`]).
struct RootVisitor;

impl<'de> Visitor<'de> for RootVisitor {
    type Value = Option<DeckMeta>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a mapping")
    }

    fn visit_unit<E: de::Error>(self) -> DeResult<Self::Value, E> {
        Ok(Some(DeckMeta::default()))
    }

    fn visit_none<E: de::Error>(self) -> DeResult<Self::Value, E> {
        Ok(Some(DeckMeta::default()))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> DeResult<Self::Value, A::Error> {
        let mut meta = DeckMeta::default();
        let mut seen = BTreeSet::new();
        while let Some(key) = map.next_key_seed(KeySeed(&mut seen))? {
            map.next_value_seed(EntrySeed {
                meta: &mut meta,
                key,
            })?;
        }
        Ok(Some(meta))
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> DeResult<Self::Value, E> {
        Ok(None)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> DeResult<Self::Value, E> {
        Ok(None)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> DeResult<Self::Value, E> {
        Ok(None)
    }

    fn visit_i128<E: de::Error>(self, _: i128) -> DeResult<Self::Value, E> {
        Ok(None)
    }

    fn visit_u128<E: de::Error>(self, _: u128) -> DeResult<Self::Value, E> {
        Ok(None)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> DeResult<Self::Value, E> {
        Ok(None)
    }

    fn visit_str<E: de::Error>(self, _: &str) -> DeResult<Self::Value, E> {
        Ok(None)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> DeResult<Self::Value, A::Error> {
        // serde_norway checks that the whole sequence was consumed.
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(None)
    }

    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> DeResult<Self::Value, A::Error> {
        unsupported_tag(data)
    }
}

/// One top-level entry: routes the value for `key` into `meta` (see [`parse_front_matter`]).
struct EntrySeed<'m> {
    meta: &'m mut DeckMeta,
    key: String,
}

impl<'de> DeserializeSeed<'de> for EntrySeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, value: D) -> DeResult<(), D::Error> {
        let EntrySeed { meta, key } = self;
        if let Some(field) = string_field(meta, &key) {
            // `Option<String>` asks for a string, and serde_norway then hands over a
            // scalar's text exactly as written (`1.50`, `2026-10-08`); `null` gives `None`.
            *field = Option::<String>::deserialize(value)?;
        } else if let Some((_, example)) = TABLES.iter().find(|(name, _)| *name == key) {
            let table = value.deserialize_any(TableVisitor { example })?;
            if key == "reveal" {
                meta.reveal = table.unwrap_or_default();
            } else {
                meta.vars
                    .insert(key, table.map_or(Value::Null, Value::Object));
            }
        } else {
            let value = JsonVisitor.deserialize(value)?;
            meta.vars.insert(key, value);
        }
        Ok(())
    }
}

/// The [`DeckMeta`] string field for `key`, if it is one of [`STRING_FIELDS`].
fn string_field<'m>(meta: &'m mut DeckMeta, key: &str) -> Option<&'m mut Option<String>> {
    Some(match key {
        "title" => &mut meta.title,
        "subtitle" => &mut meta.subtitle,
        "author" => &mut meta.author,
        "date" => &mut meta.date,
        "description" => &mut meta.description,
        "lang" => &mut meta.lang,
        "theme" => &mut meta.theme,
        _ => return None,
    })
}

/// A mapping key as text (a non-string scalar key becomes its YAML text). A key already in
/// the set is an error; raising it while the key is read lets serde_norway report the
/// key's own line and column.
struct KeySeed<'s>(&'s mut BTreeSet<String>);

impl<'de> DeserializeSeed<'de> for KeySeed<'_> {
    type Value = String;

    fn deserialize<D: Deserializer<'de>>(self, key: D) -> DeResult<String, D::Error> {
        key.deserialize_str(self)
    }
}

impl<'de> Visitor<'de> for KeySeed<'_> {
    type Value = String;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a scalar key")
    }

    fn visit_str<E: de::Error>(self, key: &str) -> DeResult<String, E> {
        if self.0.insert(key.to_owned()) {
            Ok(key.to_owned())
        } else {
            Err(E::custom(format_args!("duplicate key `{key}`")))
        }
    }
}

/// A value that must be a mapping: `Some(map)`, or `None` when empty (`null`). Anything
/// else is an "invalid type … expected a mapping, e.g. `example`" error.
struct TableVisitor {
    example: &'static str,
}

impl<'de> Visitor<'de> for TableVisitor {
    type Value = Option<Map<String, Value>>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "a mapping, e.g. `{}`", self.example)
    }

    fn visit_unit<E: de::Error>(self) -> DeResult<Self::Value, E> {
        Ok(None)
    }

    fn visit_none<E: de::Error>(self) -> DeResult<Self::Value, E> {
        Ok(None)
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> DeResult<Self::Value, A::Error> {
        json_map(map).map(Some)
    }

    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> DeResult<Self::Value, A::Error> {
        unsupported_tag(data)
    }
}

/// Any YAML value as JSON, for `vars` and `reveal`: integers outside the 64-bit range
/// become their decimal text, `.inf`/`.nan` become `null`, keys must be unique and custom
/// tags are errors.
#[derive(Clone, Copy)]
struct JsonVisitor;

impl<'de> DeserializeSeed<'de> for JsonVisitor {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, value: D) -> DeResult<Value, D::Error> {
        value.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any YAML value")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> DeResult<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> DeResult<Value, E> {
        Ok(Value::from(v))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> DeResult<Value, E> {
        Ok(Value::from(v))
    }

    fn visit_i128<E: de::Error>(self, v: i128) -> DeResult<Value, E> {
        // serde_norway only calls this for integers that do not fit an i64.
        Ok(Value::String(v.to_string()))
    }

    fn visit_u128<E: de::Error>(self, v: u128) -> DeResult<Value, E> {
        // serde_norway only calls this for integers that do not fit a u64.
        Ok(Value::String(v.to_string()))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> DeResult<Value, E> {
        // `null` for infinities and NaN, which JSON cannot represent.
        Ok(Value::from(v))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> DeResult<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> DeResult<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E: de::Error>(self) -> DeResult<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E: de::Error>(self) -> DeResult<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> DeResult<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(self)? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> DeResult<Value, A::Error> {
        json_map(map).map(Value::Object)
    }

    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> DeResult<Value, A::Error> {
        unsupported_tag(data)
    }
}

/// A mapping's entries as JSON, in the order written; a repeated key is an error.
fn json_map<'de, A: MapAccess<'de>>(mut map: A) -> DeResult<Map<String, Value>, A::Error> {
    let mut seen = BTreeSet::new();
    let mut entries = Map::new();
    while let Some(key) = map.next_key_seed(KeySeed(&mut seen))? {
        let value = map.next_value_seed(JsonVisitor)?;
        entries.insert(key, value);
    }
    Ok(entries)
}

/// The error for a value with a custom YAML tag (serde_norway presents those as enums).
fn unsupported_tag<'de, A: EnumAccess<'de>, T>(data: A) -> DeResult<T, A::Error> {
    let (tag, _value) = data.variant::<String>()?;
    Err(de::Error::custom(format_args!(
        "YAML tags such as `!{tag}` are not supported"
    )))
}

/// Lines of `text` as `(content, end)`: `content` without its `\n` / `\r\n`, `end` the byte
/// offset just past the line break (`text.len()` for an unterminated last line).
fn lines_with_ends(text: &str) -> impl Iterator<Item = (&str, usize)> {
    text.split_inclusive('\n').scan(0, |offset, raw| {
        *offset += raw.len();
        Some((without_line_break(raw), *offset))
    })
}

/// `s` without one trailing `\n` or `\r\n`.
fn without_line_break(s: &str) -> &str {
    match s.strip_suffix('\n') {
        Some(rest) => rest.strip_suffix('\r').unwrap_or(rest),
        None => s,
    }
}

/// `s` without one leading `\n` or `\r\n`; `None` when it does not start with a line break.
fn strip_leading_line_break(s: &str) -> Option<&str> {
    s.strip_prefix('\n').or_else(|| s.strip_prefix("\r\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, SlideSplit};
    use comrak::nodes::NodeValue;

    fn meta(yaml: &str) -> DeckMeta {
        parse_front_matter(yaml).unwrap()
    }

    fn front_matter_error(yaml: &str) -> String {
        match parse_front_matter(yaml) {
            Err(Error::FrontMatter(message)) => message,
            other => panic!("expected Error::FrontMatter for {yaml:?}, got {other:?}"),
        }
    }

    fn var_keys(meta: &DeckMeta) -> Vec<&str> {
        meta.vars.keys().map(String::as_str).collect()
    }

    // ---- parse_front_matter ------------------------------------------------------------

    #[test]
    fn empty_yaml_is_default() {
        for yaml in ["", "\n", "   \n\n", "# only a comment\n", "~", "null\n"] {
            assert_eq!(meta(yaml), DeckMeta::default(), "{yaml:?}");
        }
    }

    #[test]
    fn known_keys_go_to_fields_unknown_keys_to_vars_in_order() {
        let yaml = "\
zeta: last letter
title: Rust for data people
subtitle: A gentle start
author: J. Blaschke
date: 2026-10-08
description: Talk at PyData
lang: de
theme: ./theme
reveal:
  transition: fade
  width: 1920
alpha: [1, 2]
";
        let meta = meta(yaml);
        assert_eq!(meta.title.as_deref(), Some("Rust for data people"));
        assert_eq!(meta.subtitle.as_deref(), Some("A gentle start"));
        assert_eq!(meta.author.as_deref(), Some("J. Blaschke"));
        assert_eq!(meta.date.as_deref(), Some("2026-10-08"));
        assert_eq!(meta.description.as_deref(), Some("Talk at PyData"));
        assert_eq!(meta.lang.as_deref(), Some("de"));
        assert_eq!(meta.theme.as_deref(), Some("./theme"));
        assert_eq!(
            Value::Object(meta.reveal.clone()),
            serde_json::json!({ "transition": "fade", "width": 1920 })
        );
        // Exactly the unknown keys, in the order written (not sorted).
        assert_eq!(var_keys(&meta), ["zeta", "alpha"]);
        assert_eq!(meta.vars["zeta"], "last letter");
        assert_eq!(meta.vars["alpha"], serde_json::json!([1, 2]));
    }

    #[test]
    fn vars_preserve_author_order() {
        let meta = meta("c: 1\na: 2\nb: 3\ntitle: T\nd: 4\n");
        assert_eq!(var_keys(&meta), ["c", "a", "b", "d"]);
        assert_eq!(
            serde_json::to_string(&meta.vars).unwrap(),
            r#"{"c":1,"a":2,"b":3,"d":4}"#
        );
    }

    #[test]
    fn scalars_for_string_fields_keep_their_yaml_text() {
        let meta = meta(
            "date: 2026-10-08\ntitle: 42\nsubtitle: 1.50\nauthor: true\n\
             description: yes\nlang: 0x1F\ntheme: 2026-10-08T09:30:00Z\n",
        );
        assert_eq!(meta.date.as_deref(), Some("2026-10-08"));
        assert_eq!(meta.title.as_deref(), Some("42"));
        assert_eq!(meta.subtitle.as_deref(), Some("1.50"));
        assert_eq!(meta.author.as_deref(), Some("true"));
        assert_eq!(meta.description.as_deref(), Some("yes"));
        assert_eq!(meta.lang.as_deref(), Some("0x1F"));
        assert_eq!(meta.theme.as_deref(), Some("2026-10-08T09:30:00Z"));
    }

    #[test]
    fn quoted_block_and_null_string_fields() {
        let meta = meta("title: \"42\"\nsubtitle: |\n  two\n  lines\nauthor: ~\ndate:\nlang: ''\n");
        assert_eq!(meta.title.as_deref(), Some("42"));
        assert_eq!(meta.subtitle.as_deref(), Some("two\nlines\n"));
        assert_eq!(meta.author, None);
        assert_eq!(meta.date, None);
        assert_eq!(meta.lang.as_deref(), Some(""));
        assert!(
            meta.vars.is_empty(),
            "null known keys must not leak into vars"
        );
    }

    #[test]
    fn non_scalar_string_field_is_an_error() {
        let message = front_matter_error("title: [a, b]\n");
        assert!(message.contains("title"), "{message}");
        let message = front_matter_error("author:\n  name: X\n");
        assert!(message.contains("author"), "{message}");
    }

    #[test]
    fn reveal_must_be_a_mapping() {
        assert_eq!(
            front_matter_error("reveal: fade\n"),
            "reveal: invalid type: string \"fade\", expected a mapping, \
             e.g. `reveal: {transition: fade}` at line 2 column 9"
        );
        for yaml in ["reveal: [fade]\n", "reveal: 3\n", "reveal: true\n"] {
            let message = front_matter_error(yaml);
            assert!(message.starts_with("reveal: invalid type"), "{message}");
            assert!(message.contains("expected a mapping"), "{message}");
        }
        // An empty `reveal:` (null, e.g. all options commented out) counts as empty.
        for yaml in [
            "reveal:\n",
            "reveal: ~\n",
            "reveal: null\n",
            "reveal:\n  # x: 1\n",
        ] {
            let meta = meta(yaml);
            assert!(meta.reveal.is_empty(), "{yaml:?}");
            assert!(meta.vars.is_empty(), "{yaml:?}");
        }
        assert!(meta("reveal: {}\n").vars.is_empty());
    }

    #[test]
    fn config_tables_must_be_mappings() {
        assert_eq!(
            front_matter_error("slides: headings\n"),
            "slides: invalid type: string \"headings\", expected a mapping, \
             e.g. `slides: {split: headings}` at line 2 column 9"
        );
        assert_eq!(
            front_matter_error("title: T\nmath: never\n"),
            "math: invalid type: string \"never\", expected a mapping, \
             e.g. `math: {enabled: never}` at line 3 column 7"
        );
        for yaml in [
            "slides: [split]\n",
            "markdown: 3\n",
            "notebook: true\n",
            "math: [never]\n",
        ] {
            let message = front_matter_error(yaml);
            let key = yaml.split(':').next().unwrap();
            assert!(
                message.starts_with(&format!("{key}: invalid type")),
                "{message}"
            );
            assert!(message.contains("expected a mapping, e.g. `"), "{message}");
        }
        // Empty tables stay in vars as null, and merging them changes nothing.
        let meta = meta("slides:\nmath: ~\n");
        assert_eq!(var_keys(&meta), ["slides", "math"]);
        assert_eq!(meta.vars["slides"], Value::Null);
        let mut config = Config::default();
        config.merge_front_matter(&meta).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn non_mapping_root_is_an_error() {
        for yaml in [
            "- a\n- b\n",
            "just a sentence\n",
            "42\n",
            "\"quoted\"\n",
            "true\n",
        ] {
            assert_eq!(front_matter_error(yaml), "expected a mapping", "{yaml:?}");
        }
        assert_eq!(
            parse_front_matter("- a\n").unwrap_err().to_string(),
            "front matter: expected a mapping"
        );
    }

    #[test]
    fn yaml_syntax_errors_are_front_matter_errors() {
        // Line numbers are source lines: the YAML starts on line 2, after the opening `---`.
        assert_eq!(
            front_matter_error("title: Rust: the good parts\nauthor: Me\n"),
            "mapping values are not allowed in this context at line 2 column 12"
        );
        assert_eq!(
            front_matter_error("a: 1\n  b: 2\n"),
            "mapping values are not allowed in this context at line 3 column 4"
        );
        assert_eq!(
            front_matter_error("key: *undefined_alias\n"),
            "unknown anchor at line 2 column 6"
        );
        let message = front_matter_error("title: T\nauthor: Me\nlist:\n\t- a\n");
        assert!(message.contains("at line 5 column 1"), "{message}");
        let message = front_matter_error("title: T\nlist: [unclosed\n");
        assert!(message.contains("line 3"), "{message}");
        // CRLF line breaks count once.
        assert_eq!(
            front_matter_error("title: T\r\nkey: a: b\r\n"),
            "mapping values are not allowed in this context at line 3 column 7"
        );
        // A type error is reported at the value's own line too.
        assert_eq!(
            front_matter_error("lang: de\ntitle: [a, b]\n"),
            "title: invalid type: sequence, expected a string at line 3 column 8"
        );
    }

    #[test]
    fn error_lines_match_the_markdown_file() {
        let document = "---\ntitle: Rust: the good parts\nauthor: Me\n---\n\n# Slide\n";
        let literal = comrak_literal(document).unwrap();
        let message = match parse_front_matter(strip_delimiters(&literal)) {
            Err(Error::FrontMatter(message)) => message,
            other => panic!("expected a front matter error, got {other:?}"),
        };
        let line = document
            .lines()
            .position(|line| line.starts_with("title:"))
            .unwrap()
            + 1;
        assert_eq!(line, 2);
        assert!(
            message.contains(&format!("at line {line} column 12")),
            "{message}"
        );
        // The same through the text-based split (notebook cells).
        let (yaml, _, _) = split_front_matter(document).unwrap();
        assert_eq!(front_matter_error(yaml), message);
    }

    #[test]
    fn vars_keep_yaml_types_with_yaml_1_2_scalars() {
        let meta = meta(
            "count: 3\nratio: 0.5\nflag: true\nyes_word: yes\non_word: on\nno_word: no\n\
             octal_like: 010\nhex: 0x10\npublished: 2026-10-08\nnothing: ~\nbig: .inf\n\
             nested:\n  list: [a, 1]\n  map: {k: v}\n42: numeric key\n",
        );
        let expected = serde_json::json!({
            "count": 3, "ratio": 0.5, "flag": true,
            "yes_word": "yes", "on_word": "on", "no_word": "no",
            "octal_like": "010", "hex": 16, "published": "2026-10-08",
            "nothing": null, "big": null,
            "nested": { "list": ["a", 1], "map": { "k": "v" } },
            "42": "numeric key",
        });
        assert_eq!(Value::Object(meta.vars), expected);
    }

    #[test]
    fn config_tables_stay_in_vars() {
        let meta = meta(
            "slides:\n  split: headings\nmarkdown: {smart_punctuation: true}\n\
             math: {enabled: never}\nnotebook: {show_prompts: false}\nvenue: PyData\n",
        );
        assert_eq!(
            var_keys(&meta),
            ["slides", "markdown", "math", "notebook", "venue"]
        );
        assert_eq!(
            meta.vars["slides"],
            serde_json::json!({ "split": "headings" })
        );
    }

    #[test]
    fn duplicate_keys_are_errors_at_the_repeated_key() {
        assert_eq!(
            front_matter_error("title: A\ntitle: B\n"),
            "duplicate key `title` at line 3 column 1"
        );
        assert_eq!(
            front_matter_error("a: 1\nb: 2\na: 3\n"),
            "duplicate key `a` at line 4 column 1"
        );
        // A second `reveal:` block would otherwise silently drop the first one.
        assert_eq!(
            front_matter_error("reveal: {transition: fade}\nreveal: {width: 10}\n"),
            "duplicate key `reveal` at line 3 column 1"
        );
        assert_eq!(
            front_matter_error("slides:\n  split: rules\nslides:\n  split: headings\n"),
            "duplicate key `slides` at line 4 column 1"
        );
        // Nested mappings, inside reveal and inside vars.
        assert_eq!(
            front_matter_error("reveal:\n  transition: fade\n  transition: zoom\n"),
            "reveal: duplicate key `transition` at line 4 column 3"
        );
        assert_eq!(
            front_matter_error("list: [{a: 1, a: 2}]\n"),
            "list[0]: duplicate key `a` at line 2 column 15"
        );
        // A non-string key is compared by its text.
        assert!(front_matter_error("1: a\n1: b\n").starts_with("duplicate key `1`"));
        // The same key in different mappings is fine.
        let meta = meta("a: {x: 1}\nb: {x: 2}\n");
        assert_eq!(meta.vars["b"]["x"], 2);
    }

    #[test]
    fn integers_outside_64_bits_keep_their_digits() {
        let parsed = meta(
            "title: 99999999999999999999\nlonger: 18446744073709551616\n\
             neg: -99999999999999999999\nmax: 18446744073709551615\nmin: -9223372036854775808\n\
             reveal: {big: 340282366920938463463374607431768211455}\n",
        );
        assert_eq!(parsed.title.as_deref(), Some("99999999999999999999"));
        assert_eq!(parsed.vars["longer"], "18446744073709551616");
        assert_eq!(parsed.vars["neg"], "-99999999999999999999");
        // In range: still numbers.
        assert_eq!(parsed.vars["max"], u64::MAX);
        assert_eq!(parsed.vars["min"], i64::MIN);
        assert_eq!(parsed.reveal["big"], u128::MAX.to_string());
        // Beyond 128 bits serde_norway itself reads the scalar as a float.
        let huge = meta("huge: 999999999999999999999999999999999999999999\n");
        assert_eq!(huge.vars["huge"], Value::from(1e42));
    }

    #[test]
    fn custom_tags_are_rejected_with_a_clear_message() {
        assert_eq!(
            front_matter_error("x: !expr foo\n"),
            "x: YAML tags such as `!expr` are not supported at line 2 column 4"
        );
        assert_eq!(
            front_matter_error("reveal: {width: !px 10}\n"),
            "reveal.width: YAML tags such as `!px` are not supported at line 2 column 17"
        );
        assert!(front_matter_error("slides: !t {split: headings}\n").contains("`!t`"));
        assert!(front_matter_error("!root\na: 1\n").contains("`!root`"));
        // Core-schema tags are plain YAML and work.
        let core = meta("title: !!str 42\nn: !!int 7\n");
        assert_eq!(core.title.as_deref(), Some("42"));
        assert_eq!(core.vars["n"], 7);
        // On a string field the scalar text is used and the tag ignored (documented).
        assert_eq!(meta("title: !t x\n").title.as_deref(), Some("x"));
    }

    #[test]
    fn crlf_yaml_parses() {
        let meta = meta("title: Hello\r\nauthor: Me\r\nvenue: PyData\r\n");
        assert_eq!(meta.title.as_deref(), Some("Hello"));
        assert_eq!(meta.author.as_deref(), Some("Me"));
        assert_eq!(meta.vars["venue"], "PyData");
    }

    // ---- split_front_matter ------------------------------------------------------------

    #[test]
    fn split_basic() {
        assert_eq!(
            split_front_matter("---\na: 1\n---\nbody"),
            Some(("a: 1", "body", 3))
        );
        assert_eq!(
            split_front_matter("---\na: 1\nb: 2\n---\n\n# Slide\n"),
            Some(("a: 1\nb: 2", "\n# Slide\n", 4))
        );
    }

    #[test]
    fn split_requires_leading_delimiter_line() {
        assert_eq!(split_front_matter("body\n---\na: 1\n---\n"), None);
        assert_eq!(split_front_matter("\n---\na: 1\n---\n"), None);
        assert_eq!(split_front_matter(" ---\na: 1\n---\n"), None);
        assert_eq!(split_front_matter("--- \na: 1\n---\n"), None);
        assert_eq!(split_front_matter("----\na: 1\n---\n"), None);
        assert_eq!(split_front_matter("---"), None);
        assert_eq!(split_front_matter(""), None);
    }

    #[test]
    fn split_unterminated_is_none() {
        assert_eq!(split_front_matter("---\na: 1\n"), None);
        assert_eq!(split_front_matter("---\na: 1\nbody"), None);
        assert_eq!(split_front_matter("---\na: 1\n--- \nbody"), None);
        assert_eq!(split_front_matter("---\na: 1\n----\nbody"), None);
        assert_eq!(split_front_matter("---\n"), None);
    }

    #[test]
    fn split_crlf() {
        assert_eq!(
            split_front_matter("---\r\na: 1\r\n---\r\nbody"),
            Some(("a: 1", "body", 3))
        );
        assert_eq!(
            split_front_matter("---\r\na: 1\r\nb: 2\r\n---\r\n\r\nbody\r\n"),
            Some(("a: 1\r\nb: 2", "\r\nbody\r\n", 4))
        );
    }

    #[test]
    fn split_edge_cases() {
        // Closing delimiter at the end of the text.
        assert_eq!(split_front_matter("---\na: 1\n---"), Some(("a: 1", "", 3)));
        assert_eq!(
            split_front_matter("---\na: 1\n---\n"),
            Some(("a: 1", "", 3))
        );
        // As in comrak, the line after the opening delimiter never closes the block: an
        // empty block needs a blank line.
        assert_eq!(split_front_matter("---\n---\nbody"), None);
        assert_eq!(split_front_matter("---\n---"), None);
        assert_eq!(split_front_matter("---\r\n---\r\nbody"), None);
        assert_eq!(
            split_front_matter("---\n---\n---\nbody"),
            Some(("---", "body", 3))
        );
        assert_eq!(
            split_front_matter("---\n\n---\nbody"),
            Some(("", "body", 3))
        );
        // Byte-order mark.
        assert_eq!(
            split_front_matter("\u{feff}---\na: 1\n---\nbody"),
            Some(("a: 1", "body", 3))
        );
        // Only the first closing line counts.
        assert_eq!(
            split_front_matter("---\na: 1\n---\nb\n---\nc"),
            Some(("a: 1", "b\n---\nc", 3))
        );
        // Blank lines inside the YAML are kept, minus the final line break.
        assert_eq!(
            split_front_matter("---\na: 1\n\n---\nbody"),
            Some(("a: 1\n", "body", 4))
        );
    }

    #[test]
    fn split_then_parse() {
        let (yaml, rest, consumed) =
            split_front_matter("---\r\ntitle: Hi\r\ndate: 2026-10-08\r\n---\r\n# Slide\r\n")
                .unwrap();
        assert_eq!((rest, consumed), ("# Slide\r\n", 4));
        let meta = meta(yaml);
        assert_eq!(meta.title.as_deref(), Some("Hi"));
        assert_eq!(meta.date.as_deref(), Some("2026-10-08"));
    }

    // ---- strip_delimiters (against real comrak output) -----------------------------------

    /// comrak options per markdown-dialect.md §1 (with config defaults).
    fn comrak_options() -> comrak::Options<'static> {
        let mut o = comrak::Options::default();
        o.extension.front_matter_delimiter = Some("---".into());
        o.extension.table = true;
        o.extension.strikethrough = true;
        o.extension.tasklist = true;
        o.extension.autolink = true;
        o.extension.footnotes = true;
        o.extension.description_lists = true;
        o.extension.alerts = true;
        o.extension.math_dollars = true;
        o.extension.math_code = true;
        o.extension.block_directive = true;
        o.extension.header_attributes = true;
        o.extension.fenced_code_attributes = true;
        o.extension.link_attributes = true;
        o.extension.inline_code_attributes = true;
        o.parse.smart = false;
        o.parse.relaxed_autolinks = false;
        o.render.r#unsafe = true;
        o.render.escape = false;
        o.render.hardbreaks = false;
        o.render.github_pre_lang = false;
        o.render.full_info_string = false;
        o.render.sourcepos = false;
        o
    }

    /// The literal of the document's `FrontMatter` node, if comrak produced one.
    fn comrak_literal(document: &str) -> Option<String> {
        let arena = comrak::Arena::new();
        let root = comrak::parse_document(&arena, document, &comrak_options());
        root.children().find_map(|node| match &node.data().value {
            NodeValue::FrontMatter(literal) => Some(literal.clone()),
            _ => None,
        })
    }

    #[test]
    fn comrak_literal_shape_is_as_documented() {
        // Both delimiter lines plus the trailing blank line, verbatim.
        assert_eq!(
            comrak_literal("---\na: 1\n---\n\n# Slide\n").as_deref(),
            Some("---\na: 1\n---\n\n")
        );
        assert_eq!(
            comrak_literal("---\r\na: 1\r\n---\r\n\r\n# Slide\r\n").as_deref(),
            Some("---\r\na: 1\r\n---\r\n\r\n")
        );
        assert_eq!(comrak_literal("# Slide\n"), None);
    }

    #[test]
    fn strip_delimiters_on_comrak_literals() {
        let documents = [
            ("---\na: 1\n---\n# Slide\n", "a: 1"),
            ("---\na: 1\nb: 2\n---\n\n# Slide\n", "a: 1\nb: 2"),
            ("---\r\na: 1\r\nb: 2\r\n---\r\n# Slide\r\n", "a: 1\r\nb: 2"),
            ("---\r\na: 1\r\n---\r\n\r\n# Slide\r\n", "a: 1"),
            ("---\na: 1\n---", "a: 1"),
            ("---\na: 1\n---\n", "a: 1"),
            ("\u{feff}---\na: 1\n---\n\nbody", "a: 1"),
            ("---\na: 1\n\n---\n\nbody", "a: 1\n"),
            (
                "---\nlist:\n  - x\nkey: --- not a delimiter\n---\nbody",
                "list:\n  - x\nkey: --- not a delimiter",
            ),
        ];
        for (document, yaml) in documents {
            let literal = comrak_literal(document)
                .unwrap_or_else(|| panic!("comrak found no front matter in {document:?}"));
            assert_eq!(strip_delimiters(&literal), yaml, "literal {literal:?}");
            // Same YAML as the text-based extraction.
            let (split_yaml, _, _) = split_front_matter(document).unwrap();
            assert_eq!(split_yaml, yaml, "document {document:?}");
        }
    }

    #[test]
    fn strip_delimiters_without_delimiters() {
        assert_eq!(strip_delimiters("---\n---\n"), "");
        assert_eq!(strip_delimiters("---\r\n---\r\n\r\n"), "");
        assert_eq!(strip_delimiters("a: 1\n"), "a: 1");
        assert_eq!(strip_delimiters("---\na: ---\n"), "a: ---");
    }

    /// Every document of up to five lines built from `---`, `a: 1`, an empty line and `x`
    /// (LF or CRLF, with or without a final line break): comrak and [`split_front_matter`]
    /// agree on whether there is front matter, on its YAML, and on where the block ends
    /// (comrak's literal also takes one following empty line). Lines like `----` or `--- `
    /// and mixed line breaks are left out: there comrak's substring search has quirks that
    /// the line-based split deliberately does not copy.
    #[test]
    fn split_agrees_with_comrak_on_generated_documents() {
        const ATOMS: [&str; 4] = ["---", "a: 1", "", "x"];
        let mut documents = Vec::new();
        let mut sequences: Vec<Vec<&str>> = vec![Vec::new()];
        for _ in 0..5 {
            sequences = sequences
                .iter()
                .flat_map(|seq| {
                    ATOMS.iter().map(move |atom| {
                        let mut longer = seq.clone();
                        longer.push(atom);
                        longer
                    })
                })
                .collect();
            for seq in &sequences {
                for eol in ["\n", "\r\n"] {
                    documents.push(seq.join(eol));
                    documents.push(seq.join(eol) + eol);
                }
            }
        }
        assert_eq!(documents.len(), 4 * (4 + 16 + 64 + 256 + 1024));
        let mut with_front_matter = 0;
        for document in &documents {
            let literal = comrak_literal(document);
            let split = split_front_matter(document);
            let (Some(literal), Some((yaml, rest, consumed))) = (&literal, split) else {
                assert_eq!(
                    literal.is_some(),
                    split.is_some(),
                    "{document:?}: comrak {literal:?}, split {split:?}"
                );
                continue;
            };
            with_front_matter += 1;
            assert_eq!(strip_delimiters(literal), yaml, "{document:?}");
            let block = &document[..document.len() - rest.len()];
            assert_eq!(block.lines().count(), consumed, "{document:?}");
            let extra = literal
                .strip_prefix(block)
                .unwrap_or_else(|| panic!("{document:?}: literal {literal:?}, block {block:?}"));
            assert!(
                ["", "\n", "\r\n"].contains(&extra) && rest.starts_with(extra),
                "{document:?}: literal {literal:?}, block {block:?}"
            );
        }
        assert!(with_front_matter > 100, "{with_front_matter}");
    }

    #[test]
    fn fixture_front_matter_agrees_between_comrak_and_text_split() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/md");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|ext| ext != "md") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let Some(literal) = comrak_literal(&text) else {
                assert_eq!(split_front_matter(&text), None, "{}", path.display());
                continue;
            };
            let (yaml, _, _) = split_front_matter(&text).unwrap();
            assert_eq!(strip_delimiters(&literal), yaml, "{}", path.display());
            parse_front_matter(yaml).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            checked += 1;
        }
        assert!(
            checked > 0,
            "no fixture with front matter found in {}",
            dir.display()
        );
    }

    // ---- meta_from_json -------------------------------------------------------------------

    #[test]
    fn json_object_follows_the_front_matter_key_rules() {
        let value = serde_json::json!({
            "zeta": 1,
            "title": 42,
            "subtitle": 1.50,
            "author": true,
            "date": "2026-10-08",
            "lang": null,
            "reveal": { "width": 1.50, "transition": "fade" },
            "slides": { "split": "headings" },
            "alpha": [1, { "k": "v" }],
        });
        let meta = meta_from_json(&value).unwrap();
        assert_eq!(meta.title.as_deref(), Some("42"));
        assert_eq!(meta.subtitle.as_deref(), Some("1.5"));
        assert_eq!(meta.author.as_deref(), Some("true"));
        assert_eq!(meta.date.as_deref(), Some("2026-10-08"));
        assert_eq!(meta.lang, None);
        assert_eq!(
            Value::Object(meta.reveal.clone()),
            serde_json::json!({ "width": 1.5, "transition": "fade" })
        );
        assert_eq!(var_keys(&meta), ["zeta", "slides", "alpha"]);
        assert_eq!(meta.vars["alpha"], serde_json::json!([1, { "k": "v" }]));
        // Same result as the equivalent YAML.
        let yaml = meta_from_json(&serde_json::json!({ "title": "T", "venue": "PyData" }));
        assert_eq!(yaml.unwrap(), self::meta("title: T\nvenue: PyData\n"));
    }

    #[test]
    fn json_errors_match_the_yaml_rules() {
        assert_eq!(meta_from_json(&Value::Null).unwrap(), DeckMeta::default());
        for value in [
            serde_json::json!([1]),
            serde_json::json!("x"),
            serde_json::json!(3),
        ] {
            assert_eq!(
                meta_from_json(&value).unwrap_err().to_string(),
                "front matter: expected a mapping"
            );
        }
        let message = |value: Value| match meta_from_json(&value) {
            Err(Error::FrontMatter(message)) => message,
            other => panic!("expected Error::FrontMatter for {value}, got {other:?}"),
        };
        assert_eq!(
            message(serde_json::json!({ "reveal": "fade" })),
            "reveal: invalid type: string \"fade\", expected a mapping, \
             e.g. `reveal: {transition: fade}`"
        );
        assert_eq!(
            message(serde_json::json!({ "slides": "headings" })),
            "slides: invalid type: string \"headings\", expected a mapping, \
             e.g. `slides: {split: headings}`"
        );
        assert_eq!(
            message(serde_json::json!({ "title": ["a"] })),
            "title: invalid type: sequence, expected a string"
        );
        // Empty tables, as in YAML.
        let meta = meta_from_json(&serde_json::json!({ "reveal": null, "math": null })).unwrap();
        assert!(meta.reveal.is_empty());
        assert_eq!(meta.vars["math"], Value::Null);
    }

    // ---- Config::merge_front_matter through parsed front matter -------------------------

    #[test]
    fn merge_slides_split_leaves_other_slides_keys_untouched() {
        let mut config = Config::default();
        config.slides.slide_level = 3;
        config.slides.incremental_lists = true;
        let meta = meta("title: T\nslides:\n  split: headings\n");
        config.merge_front_matter(&meta).unwrap();
        assert_eq!(config.slides.split, SlideSplit::Headings);
        assert_eq!(config.slides.slide_level, 3);
        assert!(config.slides.incremental_lists);
        // The override stays in vars, and applying it again changes nothing.
        assert!(meta.vars.contains_key("slides"));
        let once = config.clone();
        config.merge_front_matter(&meta).unwrap();
        assert_eq!(config, once);
    }

    #[test]
    fn merge_unknown_slides_key_is_an_error_with_hint() {
        let mut config = Config::default();
        let before = config.clone();
        let meta = meta("slides: {splt: x}\n");
        let message = match config.merge_front_matter(&meta) {
            Err(Error::FrontMatter(message)) => message,
            other => panic!("expected Error::FrontMatter, got {other:?}"),
        };
        assert!(message.contains("splt"), "{message}");
        assert!(message.contains("did you mean 'split'?"), "{message}");
        assert_eq!(
            config, before,
            "a failed merge must leave the config unchanged"
        );
    }

    #[test]
    fn merge_other_tables_and_reveal() {
        let mut config = Config::default();
        config.reveal.insert("controls".into(), Value::from(false));
        let meta = meta(
            "markdown: {smart_punctuation: true}\nmath: {enabled: never}\n\
             notebook: {show_prompts: false}\nreveal: {transition: fade}\n",
        );
        config.merge_front_matter(&meta).unwrap();
        assert!(config.markdown.smart_punctuation);
        assert!(!config.markdown.sourcepos);
        assert_eq!(config.math.enabled, crate::config::MathMode::Never);
        assert!(!config.notebook.show_prompts);
        assert_eq!(config.reveal["transition"], "fade");
        assert_eq!(config.reveal["controls"], false);
    }

    #[test]
    fn merge_rejects_yaml_1_1_booleans_in_config_tables() {
        let mut config = Config::default();
        let meta = meta("slides: {incremental_lists: yes}\n");
        let err = config.merge_front_matter(&meta).unwrap_err();
        assert!(matches!(err, Error::FrontMatter(_)), "{err}");
        assert!(err.to_string().contains("slides"), "{err}");
    }
}
