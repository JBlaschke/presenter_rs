//! Attribute parsing for `{#id .class key=value}` blocks. Implemented by T11.
//!
//! Contract: `docs/spec/markdown-dialect.md` §3. Two layers:
//!
//! - [`ParsedAttrs`] is the raw, pandoc-style attribute list. comrak produces it for headings,
//!   fenced code, links and images (`node.data().attrs`, converted with `From`); the separator
//!   form `--- {…}` is parsed by [`parse_attr_block`] with the same grammar plus two
//!   conveniences comrak lacks: single-quoted values and bare keys (`{auto-animate}`).
//! - [`SlideAttrs`] is the slide-level normalization (§3.2): `#id` → id, `.class` → classes,
//!   `layout=` consumed, shorthand keys expanded to reveal's `data-*` attributes by
//!   [`normalize_key`], unknown keys kept verbatim and recorded for `check`.

use std::collections::BTreeMap;

use crate::error::{Error, Result};
use crate::ir::Slide;

/// A raw attribute block: `{#id .class key=value}` in source order, before slide
/// normalization.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedAttrs {
    /// The `#id`, if any (the last one wins).
    pub id: Option<String>,
    /// The `.class` entries, in source order (not deduplicated).
    pub classes: Vec<String>,
    /// `key=value` pairs in source order; a bare `key` is `(key, "")`. Keys keep their case.
    pub pairs: Vec<(String, String)>,
}

impl ParsedAttrs {
    /// True when there is no id, class or pair.
    pub fn is_empty(&self) -> bool {
        self.id.is_none() && self.classes.is_empty() && self.pairs.is_empty()
    }
}

/// comrak's parsed attributes (headings, fenced code, links, images, inline code).
impl From<&comrak::nodes::Attributes> for ParsedAttrs {
    fn from(attrs: &comrak::nodes::Attributes) -> Self {
        ParsedAttrs {
            id: attrs.id.clone(),
            classes: attrs.classes.clone(),
            pairs: attrs.pairs.clone(),
        }
    }
}

/// Parses `{#id .cls key=value key2="v 2" bare}` (braces optional). Errors on unbalanced quotes.
///
/// Grammar (dialect §3.1): one leading `{` and one trailing `}` are skipped; the rest is split
/// on whitespace outside quotes. Each item is `#id`, `.class`, `key=value` (split at the first
/// `=` outside quotes) or a bare `key` (→ `(key, "")`). Double or single quotes group
/// whitespace anywhere in an item and are removed; inside quotes a backslash escapes the next
/// character. Unquoted values run to the next whitespace, so `background=#111` works here
/// although comrak's heading grammar rejects it. Items with an empty id, class or key (`#`,
/// `.`, `=v`) are ignored. Every block comrak accepts on a heading parses to the same
/// [`ParsedAttrs`] here (shared unit tests).
pub fn parse_attr_block(s: &str) -> Result<ParsedAttrs> {
    let trimmed = s.trim();
    let inner = trimmed.strip_prefix('{').unwrap_or(trimmed);
    let inner = inner.strip_suffix('}').unwrap_or(inner);
    let mut out = ParsedAttrs::default();
    for token in tokenize(inner).map_err(|quote| {
        Error::Other(format!(
            "unbalanced {quote} quote in attribute block {{{inner}}}"
        ))
    })? {
        token.classify_into(&mut out);
    }
    Ok(out)
}

/// One whitespace-separated item of an attribute block, with its quotes removed.
#[derive(Default)]
struct Token {
    /// The item's text without quotes and escapes.
    text: String,
    /// Byte offset in `text` of the first `=` that was outside quotes.
    eq: Option<usize>,
    /// The item's first character when it was outside quotes (only then is `#`/`.` a marker).
    lead: Option<char>,
    /// Whether any character (quoted or not) has been read.
    started: bool,
}

impl Token {
    fn classify_into(self, out: &mut ParsedAttrs) {
        match self.lead {
            Some('#') => {
                let id = &self.text['#'.len_utf8()..];
                if !id.is_empty() {
                    out.id = Some(id.to_string());
                }
            }
            Some('.') => {
                let class = &self.text['.'.len_utf8()..];
                if !class.is_empty() {
                    out.classes.push(class.to_string());
                }
            }
            _ => match self.eq {
                Some(at) => {
                    let (key, value) = (&self.text[..at], &self.text[at + '='.len_utf8()..]);
                    if !key.is_empty() {
                        out.pairs.push((key.to_string(), value.to_string()));
                    }
                }
                None if !self.text.is_empty() => out.pairs.push((self.text, String::new())),
                None => {}
            },
        }
    }
}

/// Splits on whitespace outside quotes. `Err(quote)` names the quote character left open.
fn tokenize(s: &str) -> std::result::Result<Vec<Token>, char> {
    let mut tokens = Vec::new();
    let mut current: Option<Token> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            tokens.extend(current.take());
            continue;
        }
        let token = current.get_or_insert_with(Token::default);
        match c {
            '"' | '\'' => read_quoted(&mut chars, c, &mut token.text)?,
            '=' if token.eq.is_none() => {
                token.eq = Some(token.text.len());
                token.text.push(c);
            }
            _ => {
                if !token.started {
                    token.lead = Some(c);
                }
                token.text.push(c);
            }
        }
        token.started = true;
    }
    tokens.extend(current);
    Ok(tokens)
}

/// Reads a quoted run up to the closing `quote` (already past the opening one), appending its
/// content with backslash escapes resolved. `Err(quote)` when the input ends first.
fn read_quoted(
    chars: &mut std::str::Chars<'_>,
    quote: char,
    out: &mut String,
) -> std::result::Result<(), char> {
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next().ok_or(quote)?),
            c if c == quote => return Ok(()),
            c => out.push(c),
        }
    }
    Err(quote)
}

/// Slide-level attributes after normalization (dialect §3.2): consumes `layout`, expands
/// shorthands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlideAttrs {
    /// `#id` (or an `id=` pair) → `Slide.id`.
    pub id: Option<String>,
    /// `layout=NAME` → `Slide.layout`; never emitted as an attribute.
    pub layout: Option<String>,
    /// `.class` entries (and `class=` pairs), deduplicated, first occurrence order.
    pub classes: Vec<String>,
    /// Normalized `<section>` attributes (lowercase keys; never `id`, `class` or `layout`).
    pub attrs: BTreeMap<String, String>,
    /// Lowercased keys [`normalize_key`] did not recognize (still present in `attrs`
    /// verbatim); the splitter reports each as an `unknown-slide-attr` warning.
    pub unknown_keys: Vec<String>,
}

impl SlideAttrs {
    /// Normalizes a raw attribute block for a slide.
    ///
    /// Keys are lowercased. `id=` and `class=` pairs behave like `#id` and `.class` (a
    /// `class=` value may hold several space-separated classes); `layout=` sets
    /// [`SlideAttrs::layout`]; every other pair goes through [`normalize_key`]. Empty ids and
    /// layouts are ignored (a slide's layout must be non-empty). Within one block a later
    /// duplicate key wins.
    pub fn from_parsed(p: ParsedAttrs) -> SlideAttrs {
        let mut out = SlideAttrs {
            id: p.id.filter(|id| !id.is_empty()),
            ..SlideAttrs::default()
        };
        for class in p.classes {
            out.add_classes(&class);
        }
        for (key, value) in p.pairs {
            let key = key.to_lowercase();
            match key.as_str() {
                "id" => {
                    if !value.is_empty() {
                        out.id = Some(value);
                    }
                }
                "class" => out.add_classes(&value),
                "layout" => {
                    if !value.is_empty() {
                        out.layout = Some(value);
                    }
                }
                _ => {
                    let (emitted, known) = normalize_key(&key, &value);
                    if !known {
                        push_unique(&mut out.unknown_keys, emitted.clone());
                    }
                    out.attrs.insert(emitted, value);
                }
            }
        }
        out
    }

    /// Merges `later` into `self`; `later` wins for `id`, `layout` and equal `attrs` keys;
    /// classes and unknown keys accumulate (deduplicated, order kept).
    pub fn merge(&mut self, later: SlideAttrs) {
        if later.id.is_some() {
            self.id = later.id;
        }
        if later.layout.is_some() {
            self.layout = later.layout;
        }
        for class in later.classes {
            push_unique(&mut self.classes, class);
        }
        self.attrs.extend(later.attrs);
        for key in later.unknown_keys {
            push_unique(&mut self.unknown_keys, key);
        }
    }

    /// Writes the attributes into `slide` with the same precedence as [`SlideAttrs::merge`]:
    /// `id` and `layout` replace the slide's when set, classes are appended (deduplicated),
    /// `attrs` entries are inserted (overwriting equal keys). `unknown_keys` is not part of
    /// the IR and is dropped.
    pub fn apply_to(self, slide: &mut Slide) {
        if let Some(id) = self.id {
            slide.id = Some(id);
        }
        if let Some(layout) = self.layout {
            slide.layout = layout;
        }
        for class in self.classes {
            push_unique(&mut slide.classes, class);
        }
        slide.attrs.extend(self.attrs);
    }

    /// True when nothing would change on a slide (no id, layout, class or attribute).
    pub fn is_empty(&self) -> bool {
        self.id.is_none()
            && self.layout.is_none()
            && self.classes.is_empty()
            && self.attrs.is_empty()
    }

    fn add_classes(&mut self, classes: &str) {
        for class in classes.split_whitespace() {
            push_unique(&mut self.classes, class.to_string());
        }
    }
}

fn push_unique(list: &mut Vec<String>, item: String) {
    if !list.contains(&item) {
        list.push(item);
    }
}

/// Shorthands that become `data-<key>` (dialect §3.2, second and third table rows).
const DATA_SHORTHANDS: &[&str] = &[
    "auto-animate",
    "auto-animate-duration",
    "auto-animate-easing",
    "auto-animate-id",
    "auto-animate-restart",
    "auto-animate-unmatched",
    "autoslide",
    "background-color",
    "background-gradient",
    "background-iframe",
    "background-image",
    "background-interactive",
    "background-opacity",
    "background-position",
    "background-repeat",
    "background-size",
    "background-transition",
    "background-video",
    "background-video-loop",
    "background-video-muted",
    "menu-title",
    "state",
    "timing",
    "transition",
    "transition-speed",
    "visibility",
];

/// CSS named colors (CSS Color 4, 148 names) plus `transparent` and `currentcolor`, sorted.
#[rustfmt::skip]
const CSS_COLOR_KEYWORDS: &[&str] = &[
    "aliceblue", "antiquewhite", "aqua", "aquamarine", "azure", "beige",
    "bisque", "black", "blanchedalmond", "blue", "blueviolet", "brown",
    "burlywood", "cadetblue", "chartreuse", "chocolate", "coral", "cornflowerblue",
    "cornsilk", "crimson", "currentcolor", "cyan", "darkblue", "darkcyan",
    "darkgoldenrod", "darkgray", "darkgreen", "darkgrey", "darkkhaki", "darkmagenta",
    "darkolivegreen", "darkorange", "darkorchid", "darkred", "darksalmon", "darkseagreen",
    "darkslateblue", "darkslategray", "darkslategrey", "darkturquoise", "darkviolet", "deeppink",
    "deepskyblue", "dimgray", "dimgrey", "dodgerblue", "firebrick", "floralwhite",
    "forestgreen", "fuchsia", "gainsboro", "ghostwhite", "gold", "goldenrod",
    "gray", "green", "greenyellow", "grey", "honeydew", "hotpink",
    "indianred", "indigo", "ivory", "khaki", "lavender", "lavenderblush",
    "lawngreen", "lemonchiffon", "lightblue", "lightcoral", "lightcyan", "lightgoldenrodyellow",
    "lightgray", "lightgreen", "lightgrey", "lightpink", "lightsalmon", "lightseagreen",
    "lightskyblue", "lightslategray", "lightslategrey", "lightsteelblue", "lightyellow", "lime",
    "limegreen", "linen", "magenta", "maroon", "mediumaquamarine", "mediumblue",
    "mediumorchid", "mediumpurple", "mediumseagreen", "mediumslateblue", "mediumspringgreen",
    "mediumturquoise", "mediumvioletred", "midnightblue", "mintcream", "mistyrose", "moccasin",
    "navajowhite", "navy", "oldlace", "olive", "olivedrab", "orange",
    "orangered", "orchid", "palegoldenrod", "palegreen", "paleturquoise", "palevioletred",
    "papayawhip", "peachpuff", "peru", "pink", "plum", "powderblue",
    "purple", "rebeccapurple", "red", "rosybrown", "royalblue", "saddlebrown",
    "salmon", "sandybrown", "seagreen", "seashell", "sienna", "silver",
    "skyblue", "slateblue", "slategray", "slategrey", "snow", "springgreen",
    "steelblue", "tan", "teal", "thistle", "tomato", "transparent",
    "turquoise", "violet", "wheat", "white", "whitesmoke", "yellow",
    "yellowgreen",
];

/// Maps a slide attribute key to the `<section>` attribute it emits (dialect §3.2 table) and
/// whether the key is known. The key is lowercased first.
///
/// - `background` → `data-background-color` when `value` is a colour (see below), otherwise
///   `data-background-image`;
/// - `background-color`, `background-image`, … `background-transition`, `transition`,
///   `transition-speed`, `auto-animate*`, `visibility`, `state`, `timing`, `autoslide`,
///   `menu-title` → `data-<key>`;
/// - keys starting with `data-` or `aria-` → verbatim;
/// - anything else → verbatim, with `known == false` (`check` warns).
///
/// A `background` value is a colour when it starts with `#`, matches `^(rgb|hsl)a?\(`
/// (case-sensitive, exactly as the spec's pattern) or is a CSS colour keyword (148 named
/// colours, `transparent`, `currentcolor`; ASCII case-insensitive like CSS keywords).
/// `id`, `class` and `layout` are handled by [`SlideAttrs::from_parsed`] before this is called.
pub fn normalize_key(key: &str, value: &str) -> (String /* emitted key */, bool /* known */) {
    let key = key.to_lowercase();
    if key == "background" {
        let emitted = if is_css_color(value) {
            "data-background-color"
        } else {
            "data-background-image"
        };
        return (emitted.to_string(), true);
    }
    if DATA_SHORTHANDS.binary_search(&key.as_str()).is_ok() {
        return (format!("data-{key}"), true);
    }
    let known = key.starts_with("data-") || key.starts_with("aria-");
    (key, known)
}

/// The `background` colour test of dialect §3.2.
fn is_css_color(value: &str) -> bool {
    if value.starts_with('#') {
        return true;
    }
    let function = ["rgb(", "rgba(", "hsl(", "hsla("];
    if function.iter().any(|prefix| value.starts_with(prefix)) {
        return true;
    }
    CSS_COLOR_KEYWORDS
        .binary_search(&value.to_ascii_lowercase().as_str())
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::split::dialect_options;
    use comrak::nodes::NodeValue;

    fn attrs(id: Option<&str>, classes: &[&str], pairs: &[(&str, &str)]) -> ParsedAttrs {
        ParsedAttrs {
            id: id.map(str::to_string),
            classes: classes.iter().map(|c| c.to_string()).collect(),
            pairs: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    /// Attribute blocks comrak accepts on a heading; both parsers must agree on each.
    const SHARED_CASES: &[&str] = &[
        "{}",
        "{#intro}",
        "{.a .b}",
        "{k=v}",
        "{k=\"v 2\"}",
        "{#intro .big k=v}",
        "{layout=section .x}",
        "{auto-animate=\"\"}",
        "{.b .shared #merged transition=zoom auto-animate=\"\"}",
        "{.oken #yip m=26 .sure x=3% #yap}",
        "{#\"has space, will travel\" .\"ok\\\"en\" title=\"是非 \\\"not\\\"\"}",
        "{ #a   .b\tc=d }",
        "{data-background-image=\"img/a b.png\" target=_blank width=50% x:y=1}",
        "{background=\"#111\" class=\"one two\"}",
    ];

    fn comrak_heading_attrs(block: &str) -> Option<ParsedAttrs> {
        let arena = comrak::Arena::new();
        let options = dialect_options();
        let root = comrak::parse_document(&arena, &format!("# Heading {block}\n"), &options);
        let heading = root.first_child()?;
        let data = heading.data();
        assert!(matches!(data.value, NodeValue::Heading(_)), "{block}");
        data.attrs.as_deref().map(ParsedAttrs::from)
    }

    #[test]
    fn parse_attr_block_agrees_with_comrak_on_heading_attributes() {
        for block in SHARED_CASES {
            let from_comrak = comrak_heading_attrs(block)
                .unwrap_or_else(|| panic!("comrak did not parse {block} as heading attributes"));
            let ours = parse_attr_block(block).unwrap();
            assert_eq!(ours, from_comrak, "{block}");
        }
    }

    #[test]
    fn parse_attr_block_examples() {
        let cases: &[(&str, ParsedAttrs)] = &[
            (
                "{#id .cls key=value key2=\"v 2\" bare}",
                attrs(
                    Some("id"),
                    &["cls"],
                    &[("key", "value"), ("key2", "v 2"), ("bare", "")],
                ),
            ),
            // braces are optional
            (
                "#id .cls key=value",
                attrs(Some("id"), &["cls"], &[("key", "value")]),
            ),
            ("", ParsedAttrs::default()),
            ("{ }", ParsedAttrs::default()),
            // the separator form allows what comrak's heading grammar does not
            ("{auto-animate}", attrs(None, &[], &[("auto-animate", "")])),
            (
                "{.dark background=#111 layout=title}",
                attrs(
                    None,
                    &["dark"],
                    &[("background", "#111"), ("layout", "title")],
                ),
            ),
            (
                "aria-label='Single quoted' k=\"it's\" j='say \"hi\"'",
                attrs(
                    None,
                    &[],
                    &[
                        ("aria-label", "Single quoted"),
                        ("k", "it's"),
                        ("j", "say \"hi\""),
                    ],
                ),
            ),
            (
                "background=\"rgba(0, 0, 0, 0.5)\" background-opacity=0.4",
                attrs(
                    None,
                    &[],
                    &[
                        ("background", "rgba(0, 0, 0, 0.5)"),
                        ("background-opacity", "0.4"),
                    ],
                ),
            ),
            // only the first unquoted `=` splits; quoted `=` and `}` are content
            (
                "a=b=c d=\"x=}\"",
                attrs(None, &[], &[("a", "b=c"), ("d", "x=}")]),
            ),
            // backslash escapes inside quotes, literal outside
            (
                r#"k="a\"b\\c" w=C:\x"#,
                attrs(None, &[], &[("k", r#"a"b\c"#), ("w", r"C:\x")]),
            ),
            // empty id / class / key items are ignored
            ("{# . =v \"\" .ok}", attrs(None, &["ok"], &[])),
            // the last id wins, keys keep their case (SlideAttrs lowercases them)
            ("#a #b Data-X=1", attrs(Some("b"), &[], &[("Data-X", "1")])),
            ("k= x", attrs(None, &[], &[("k", ""), ("x", "")])),
        ];
        for (input, expected) in cases {
            assert_eq!(&parse_attr_block(input).unwrap(), expected, "{input}");
        }
    }

    #[test]
    fn parse_attr_block_rejects_unbalanced_quotes() {
        for input in [
            "{k=\"v}",
            "k='v",
            "#\"id",
            "{.a k=\"x\\\"}",
            "k=\"a\" j=\"b",
            "k=\"trailing backslash\\",
        ] {
            let err = parse_attr_block(input).unwrap_err();
            assert!(
                matches!(&err, Error::Other(m) if m.contains("unbalanced")),
                "{input}: {err:?}"
            );
        }
    }

    #[test]
    fn normalize_key_follows_the_shorthand_table() {
        let cases: &[(&str, &str, &str, bool)] = &[
            ("background", "#111", "data-background-color", true),
            ("background", "#", "data-background-color", true),
            ("background", "rgb(1, 2, 3)", "data-background-color", true),
            (
                "background",
                "rgba(0, 0, 0, 0.5)",
                "data-background-color",
                true,
            ),
            (
                "background",
                "hsl(10 20% 30%)",
                "data-background-color",
                true,
            ),
            (
                "background",
                "hsla(10, 20%, 30%, 0.5)",
                "data-background-color",
                true,
            ),
            ("background", "navy", "data-background-color", true),
            ("background", "Navy", "data-background-color", true),
            ("background", "rebeccapurple", "data-background-color", true),
            ("background", "transparent", "data-background-color", true),
            ("background", "currentColor", "data-background-color", true),
            ("background", "bg.png", "data-background-image", true),
            ("background", "navy.png", "data-background-image", true),
            (
                "background",
                "https://example.org/x.jpg",
                "data-background-image",
                true,
            ),
            ("background", "rgbx(1)", "data-background-image", true),
            ("background", "RGB(0, 0, 0)", "data-background-image", true),
            ("background", "url(x.png)", "data-background-image", true),
            ("background", "", "data-background-image", true),
            ("BACKGROUND", "#fff", "data-background-color", true),
            ("data-foo", "1", "data-foo", true),
            ("Data-Foo", "1", "data-foo", true),
            ("aria-label", "x", "aria-label", true),
            ("data-background", "#f00", "data-background", true),
            ("k", "v", "k", false),
            ("style", "color: red", "style", false),
            ("Weird-Key", "", "weird-key", false),
        ];
        for (key, value, emitted, known) in cases {
            assert_eq!(
                normalize_key(key, value),
                (emitted.to_string(), *known),
                "{key}={value}"
            );
        }
        for key in DATA_SHORTHANDS {
            assert_eq!(normalize_key(key, "x"), (format!("data-{key}"), true));
        }
    }

    #[test]
    fn keyword_and_shorthand_tables_are_complete_and_sorted() {
        assert!(CSS_COLOR_KEYWORDS.windows(2).all(|w| w[0] < w[1]));
        assert!(DATA_SHORTHANDS.windows(2).all(|w| w[0] < w[1]));
        // 148 CSS Color 4 named colours + transparent + currentcolor
        assert_eq!(CSS_COLOR_KEYWORDS.len(), 150);
        // dialect §3.2: 13 `background-*` keys + 13 others
        assert_eq!(DATA_SHORTHANDS.len(), 26);
    }

    fn slide_attrs(block: &str) -> SlideAttrs {
        SlideAttrs::from_parsed(parse_attr_block(block).unwrap())
    }

    #[test]
    fn from_parsed_normalizes_separator_examples() {
        let s = slide_attrs("{.dark background=#111 layout=title}");
        assert_eq!(s.classes, ["dark"]);
        assert_eq!(s.layout.as_deref(), Some("title"));
        assert_eq!(
            s.attrs,
            BTreeMap::from([("data-background-color".to_string(), "#111".to_string())])
        );
        assert!(s.unknown_keys.is_empty());

        let s = slide_attrs("{background=bg.png}");
        assert_eq!(s.attrs["data-background-image"], "bg.png");

        let s = slide_attrs("{auto-animate}");
        assert_eq!(
            s.attrs,
            BTreeMap::from([("data-auto-animate".to_string(), String::new())])
        );

        let s = slide_attrs("{#intro .big k=v}");
        assert_eq!(s.id.as_deref(), Some("intro"));
        assert_eq!(s.classes, ["big"]);
        assert_eq!(
            s.attrs,
            BTreeMap::from([("k".to_string(), "v".to_string())])
        );
        assert_eq!(s.unknown_keys, ["k"]);
    }

    #[test]
    fn from_parsed_never_emits_id_class_or_layout() {
        let s = slide_attrs("{.a ID=x Class=\"b a c\" LAYOUT=wide Transition=fade K=1 k=2}");
        assert_eq!(s.id.as_deref(), Some("x"));
        assert_eq!(s.classes, ["a", "b", "c"]);
        assert_eq!(s.layout.as_deref(), Some("wide"));
        assert_eq!(
            s.attrs,
            BTreeMap::from([
                ("data-transition".to_string(), "fade".to_string()),
                ("k".to_string(), "2".to_string()),
            ])
        );
        assert_eq!(s.unknown_keys, ["k"]);
        // empty id / layout values are ignored
        let s = slide_attrs("{id=\"\" layout=\"\"}");
        assert!(s.is_empty(), "{s:?}");
        // duplicate classes collapse, order of first occurrence kept
        assert_eq!(slide_attrs("{.b .a .b}").classes, ["b", "a"]);
    }

    #[test]
    fn merge_lets_later_win_and_accumulates_classes() {
        let mut s = slide_attrs(
            "{.a .shared #from-separator transition=fade data-state-note=\"from separator\" x=1}",
        );
        s.merge(slide_attrs(
            "{.b .shared #merged transition=zoom auto-animate=\"\" layout=section x=2 y=3}",
        ));
        assert_eq!(s.id.as_deref(), Some("merged"));
        assert_eq!(s.layout.as_deref(), Some("section"));
        assert_eq!(s.classes, ["a", "shared", "b"]);
        assert_eq!(
            s.attrs,
            BTreeMap::from([
                ("data-auto-animate".to_string(), String::new()),
                ("data-state-note".to_string(), "from separator".to_string()),
                ("data-transition".to_string(), "zoom".to_string()),
                ("x".to_string(), "2".to_string()),
                ("y".to_string(), "3".to_string()),
            ])
        );
        assert_eq!(s.unknown_keys, ["x", "y"]);

        // a later block without id/layout keeps the earlier ones
        let mut s = slide_attrs("{#keep layout=title}");
        s.merge(slide_attrs("{.c}"));
        assert_eq!(s.id.as_deref(), Some("keep"));
        assert_eq!(s.layout.as_deref(), Some("title"));
    }

    #[test]
    fn apply_to_fills_slide_fields() {
        let mut slide = Slide::new();
        slide_attrs("{#intro .big .big layout=title background=#111 k=v}").apply_to(&mut slide);
        assert_eq!(slide.id.as_deref(), Some("intro"));
        assert_eq!(slide.layout, "title");
        assert_eq!(slide.classes, ["big"]);
        assert_eq!(
            slide.attrs,
            BTreeMap::from([
                ("data-background-color".to_string(), "#111".to_string()),
                ("k".to_string(), "v".to_string()),
            ])
        );

        // without layout the slide keeps its default; existing classes are kept
        let mut slide = Slide::new();
        slide.classes.push("pre".into());
        slide_attrs("{.pre .x}").apply_to(&mut slide);
        assert_eq!(slide.layout, "default");
        assert_eq!(slide.classes, ["pre", "x"]);
        assert_eq!(slide.id, None);
    }

    #[test]
    fn from_comrak_attributes_copies_every_field() {
        let comrak_attrs = comrak::nodes::Attributes {
            id: Some("i".into()),
            classes: vec!["c".into()],
            pairs: vec![("k".into(), "v".into())],
        };
        let parsed = ParsedAttrs::from(&comrak_attrs);
        assert_eq!(parsed, attrs(Some("i"), &["c"], &[("k", "v")]));
        assert!(!parsed.is_empty());
        assert!(ParsedAttrs::default().is_empty());
    }
}
