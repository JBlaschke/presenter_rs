//! YAML front matter → [`DeckMeta`] (markdown-dialect.md §2.1, config.md §3). Implemented by T10.
//!
//! There are two ways to get at the YAML text, and one parser for it:
//! - Markdown files: comrak extracts the block as `NodeValue::FrontMatter(literal)`;
//!   [`strip_delimiters`] turns that literal into the YAML between the `---` lines.
//! - Sources comrak does not see (the first markdown cell of a notebook):
//!   [`split_front_matter`] finds the block in plain text.
//! - [`parse_front_matter`] turns the YAML into a [`DeckMeta`]. The per-deck config overrides
//!   (`slides:`, `markdown:`, `math:`, `notebook:` maps) are ordinary entries of
//!   [`DeckMeta::vars`] and are applied with
//!   [`Config::merge_front_matter`](crate::config::Config::merge_front_matter), which also
//!   merges [`DeckMeta::reveal`] over the configured reveal options.
//!
//! # YAML flavour (`serde_norway` 0.9)
//! `serde_norway` resolves plain scalars with the **YAML 1.2 core schema**, not YAML 1.1:
//! - booleans are only `true`/`false` (also `True`, `TRUE`, `False`, `FALSE`); the YAML 1.1
//!   spellings `yes`, `no`, `on`, `off`, `y`, `n` are **strings** (unlike PyYAML), so
//!   `incremental_lists: yes` under `slides:` is rejected as "expected a boolean";
//! - null is `null`, `Null`, `NULL`, `~` or an empty value;
//! - integers are decimal, `0x…`, `0o…` or `0b…`; a leading zero (`010`) makes a string;
//! - floats include `.inf`/`.nan`, which have no JSON form and become `null` in `vars`;
//! - there is no timestamp type: `date: 2026-10-08` is the string `"2026-10-08"`.
//!
//! The string fields of [`DeckMeta`] take any scalar as its text exactly as written
//! (`title: 42` → `"42"`, `subtitle: 1.50` → `"1.50"`, `author: yes` → `"yes"`).

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::ir::DeckMeta;

/// The front matter delimiter line (dialect §1: `front_matter_delimiter = "---"`).
const DELIMITER: &str = "---";

/// Byte-order mark that may precede the opening delimiter (comrak skips it too).
const BOM: char = '\u{feff}';

/// Text-based extraction for sources comrak does not see (notebook cells).
/// Returns (yaml_without_delimiters, rest_of_text, number_of_lines_consumed) when `text`
/// starts with a line `---` and a closing `---` line exists.
///
/// Both delimiter lines must be exactly `---` (no trailing spaces), terminated by `\n` or
/// `\r\n`; the closing one may also end the text. The YAML excludes the line break before
/// the closing delimiter, and `rest_of_text` starts right after the closing line's break
/// (a following blank line stays in it). `number_of_lines_consumed` counts the opening line,
/// the YAML lines and the closing line, so `rest_of_text` begins on line `consumed + 1`. A
/// leading byte-order mark is skipped. An empty block (`---\n---\n`) yields empty YAML.
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
        if line == DELIMITER {
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
/// - `reveal` → [`DeckMeta::reveal`]; it must be a mapping (`null` counts as empty).
/// - Every other key → [`DeckMeta::vars`] in the order written, values as typed by YAML
///   (see the module docs). This includes the `slides`/`markdown`/`math`/`notebook` config
///   overrides, which stay in `vars` (config.md §3). Non-string scalar keys become their
///   text; when a key is repeated the last value wins (repeating a known key is an error).
///
/// Every failure, including YAML syntax errors, is an [`Error::FrontMatter`]; line numbers
/// in messages count from the first YAML line (the line after the opening `---`).
pub fn parse_front_matter(yaml: &str) -> Result<DeckMeta> {
    let entries = match serde_norway::from_str::<Value>(yaml).map_err(yaml_error)? {
        Value::Null => return Ok(DeckMeta::default()),
        Value::Object(entries) => entries,
        _ => return Err(Error::FrontMatter("expected a mapping".to_string())),
    };
    // Second pass for the string fields: deserializing a scalar as a string yields its
    // text exactly as written, which the generic `Value` above has already lost (`1.50`).
    let text: StringFields = serde_norway::from_str(yaml).map_err(yaml_error)?;
    let mut meta = DeckMeta {
        title: text.title,
        subtitle: text.subtitle,
        author: text.author,
        date: text.date,
        description: text.description,
        lang: text.lang,
        theme: text.theme,
        ..DeckMeta::default()
    };
    for (key, value) in entries {
        match key.as_str() {
            "title" | "subtitle" | "author" | "date" | "description" | "lang" | "theme" => {}
            "reveal" => meta.reveal = reveal_map(value)?,
            _ => {
                meta.vars.insert(key, value);
            }
        }
    }
    tracing::debug!(
        vars = meta.vars.len(),
        reveal = meta.reveal.len(),
        "parsed front matter"
    );
    Ok(meta)
}

/// The [`DeckMeta`] string fields. Unknown keys are ignored here (they are read from the
/// generic value); `Option<String>` turns `null` into `None` and any other scalar into its
/// text.
#[derive(Deserialize)]
struct StringFields {
    title: Option<String>,
    subtitle: Option<String>,
    author: Option<String>,
    date: Option<String>,
    description: Option<String>,
    lang: Option<String>,
    theme: Option<String>,
}

/// The `reveal:` value as a map: a mapping as is, `null` as empty, anything else an error.
fn reveal_map(value: Value) -> Result<Map<String, Value>> {
    let kind = match value {
        Value::Object(map) => return Ok(map),
        Value::Null => return Ok(Map::new()),
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a sequence",
    };
    Err(Error::FrontMatter(format!(
        "reveal must be a mapping of reveal.js options, got {kind}"
    )))
}

/// Wraps a YAML parse/shape error.
fn yaml_error(err: serde_norway::Error) -> Error {
    Error::FrontMatter(err.to_string())
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
        let message = front_matter_error("reveal: fade\n");
        assert!(message.contains("reveal"), "{message}");
        assert!(message.contains("mapping"), "{message}");
        assert!(front_matter_error("reveal: [fade]\n").contains("reveal"));
        assert!(front_matter_error("reveal: 3\n").contains("reveal"));
        // An empty `reveal:` (null) is treated like an absent one.
        assert!(meta("reveal:\n").reveal.is_empty());
        assert!(meta("reveal: {}\n").vars.is_empty());
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
        let message = front_matter_error("title: [unclosed\n");
        assert!(message.contains("line"), "{message}");
        front_matter_error("a: 1\n  b: 2\n");
        front_matter_error("key: *undefined_alias\n");
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
    fn duplicate_keys() {
        // Repeating a var: the last value wins, at the first position.
        let meta = meta("a: 1\nb: 2\na: 3\n");
        assert_eq!(var_keys(&meta), ["a", "b"]);
        assert_eq!(meta.vars["a"], 3);
        // Repeating a known field is rejected.
        assert!(front_matter_error("title: A\ntitle: B\n").contains("title"));
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
        // Empty block.
        assert_eq!(split_front_matter("---\n---\nbody"), Some(("", "body", 2)));
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
