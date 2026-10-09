//! Attribute parsing for `{#id .class key=value}` blocks. Implemented by T11.
//!
//! Contract: `docs/spec/markdown-dialect.md` §3. Two layers:
//!
//! - [`ParsedAttrs`] is the raw, pandoc-style attribute list. comrak produces it for headings,
//!   fenced code, links and images (`node.data().attrs`, converted with `From`); the separator
//!   form `--- {…}` is parsed by [`parse_attr_block`] with the same grammar plus two
//!   conveniences comrak lacks: single-quoted values and bare keys (`{auto-animate}`).
//!   comrak also rejects unquoted values with characters other than letters, digits and
//!   `- _ . : %` (`background=#222`, `lines=1|3`); such a block on a heading, fence, link,
//!   image or inline code is read again with [`read_element_attrs`] (the same tokenizer; no
//!   bare keys), see [`recover`](crate::markdown::recover) (A3).
//! - [`SlideAttrs`] is the slide-level normalization (§3.2): `#id` → id, `.class` → classes,
//!   `layout=` consumed, shorthand keys expanded to reveal's `data-*` attributes by
//!   [`normalize_key`], unknown keys kept verbatim and recorded for `check`, keys that are not
//!   valid attribute names ([`is_valid_attr_name`]) dropped and recorded.

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
/// although comrak's heading grammar rejects it. Items with an empty key (`=v`) and a bare `#`
/// or `.` are ignored; a quoted empty id or class (`#""`, `.""`) is kept as `""`, as comrak
/// does. Keys are not validated here (see [`is_valid_attr_name`]). Every block comrak accepts
/// on a heading parses to the same [`ParsedAttrs`] here (shared unit tests, including a
/// differential run over random blocks).
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

/// How an attribute block that comrak left as text reads with the dialect §3.1 grammar
/// ([`read_element_attrs`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElementAttrs {
    /// Every item is an `#id`, a `.class` or a `key=value` pair with a valid key name: the
    /// attributes, exactly as [`parse_attr_block`] reads the block.
    Attrs(ParsedAttrs),
    /// Not meant as attributes (`{1, 2}`, `{curly braces}`, R Markdown's `{r}`): the block
    /// stays text and nothing is reported.
    Text,
    /// Meant as attributes (some item is a valid one, or the block is a bare slide shorthand
    /// such as `{auto-animate}`), but not valid; the reason, for a warning.
    Invalid(String),
}

/// Reads `block` (`{…}`, braces included) as the attributes of a heading, fenced code,
/// link, image or inline code (dialect §3.1, A3 contract note): with [`parse_attr_block`]'s
/// tokenizer, so single-quoted values and unquoted values with any character but whitespace
/// work, but stricter in what an item may be. Each item must be `#id`, `.class` (both
/// non-empty or quoted) or `key=value` with an unquoted key that [`is_valid_attr_name`]
/// accepts; a bare key (`{auto-animate}`) needs a value there (`auto-animate=""`), as §3.2
/// says. The strictness keeps ordinary text in braces as text.
///
/// [`ElementAttrs::Invalid`] when the block has an unbalanced quote and starts like
/// attributes (`#x`, `.x` or `key=`), or when some item is invalid while another one is
/// valid or the block names a known slide key without a value (`{.dark auto-animate}`,
/// `{auto-animate}`); [`ElementAttrs::Text`] for anything else that is not attributes.
pub fn read_element_attrs(block: &str) -> ElementAttrs {
    let Some(inner) = block
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
    else {
        return ElementAttrs::Text;
    };
    let tokens = match tokenize(inner) {
        Ok(tokens) => tokens,
        Err(quote) if starts_like_attrs(inner) => {
            return ElementAttrs::Invalid(format!("unbalanced {quote} quote"));
        }
        Err(_) => return ElementAttrs::Text,
    };
    let mut out = ParsedAttrs::default();
    let (mut valid, mut known_bare, mut problem) = (0, false, None);
    for token in tokens {
        match token.element_problem() {
            None => {
                valid += 1;
                token.classify_into(&mut out);
            }
            Some(reason) => {
                known_bare |= token.is_known_bare_key();
                problem.get_or_insert(reason);
            }
        }
    }
    match problem {
        None => ElementAttrs::Attrs(out),
        Some(reason) if valid > 0 || known_bare => ElementAttrs::Invalid(reason),
        Some(_) => ElementAttrs::Text,
    }
}

/// Whether the inside of a block starts like an attribute item: `#x`, `.x` or `key=`.
fn starts_like_attrs(inner: &str) -> bool {
    let inner = inner.trim_start();
    if let Some(rest) = inner.strip_prefix(['#', '.']) {
        return rest.starts_with(|c: char| !c.is_whitespace());
    }
    inner
        .split_once('=')
        .is_some_and(|(key, _)| is_valid_attr_name(key))
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
    /// Whether the item contained a quoted run (so `#""` is an empty id, not a bare `#`).
    quoted: bool,
}

impl Token {
    /// Why this item is not a valid element attribute ([`read_element_attrs`]); `None` when
    /// it is one.
    fn element_problem(&self) -> Option<String> {
        match (self.lead, self.eq) {
            (Some(marker @ ('#' | '.')), _) => {
                let rest = &self.text[marker.len_utf8()..];
                (rest.is_empty() && !self.quoted)
                    .then(|| format!("`{marker}` without a name after it"))
            }
            (lead, Some(at)) => {
                let key = &self.text[..at];
                (lead.is_none() || !is_valid_attr_name(key))
                    .then(|| format!("`{key}` is not a valid attribute name"))
            }
            (_, None) if is_valid_attr_name(&self.text) => Some(format!(
                "`{key}` has no value: write `{key}=\"\"` (a key without a value works only in \
                 `--- {{…}}` and `<!-- .slide: -->`)",
                key = self.text
            )),
            (_, None) => Some(format!(
                "`{}` is not an attribute (`#id`, `.class` or `key=value`)",
                self.text
            )),
        }
    }

    /// A bare key (no `=`) that names a slide attribute: a shorthand of dialect §3.2, a
    /// `data-`/`aria-` key, `layout`, `id` or `class`.
    fn is_known_bare_key(&self) -> bool {
        if self.eq.is_some() || matches!(self.lead, Some('#' | '.')) || self.text.is_empty() {
            return false;
        }
        let key = self.text.to_lowercase();
        matches!(key.as_str(), "layout" | "id" | "class") || normalize_key(&key, "").1
    }

    fn classify_into(self, out: &mut ParsedAttrs) {
        match self.lead {
            Some('#') => {
                let id = &self.text['#'.len_utf8()..];
                if !id.is_empty() || self.quoted {
                    out.id = Some(id.to_string());
                }
            }
            Some('.') => {
                let class = &self.text['.'.len_utf8()..];
                if !class.is_empty() || self.quoted {
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
            '"' | '\'' => {
                read_quoted(&mut chars, c, &mut token.text)?;
                token.quoted = true;
            }
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
    /// Keys (as written) dropped because they are not valid attribute names
    /// ([`is_valid_attr_name`]); the splitter reports each as an `invalid-slide-attr` warning.
    /// Not part of the IR. (Additive to the T11 task-file struct.)
    pub invalid_keys: Vec<String>,
    /// Whether the `background` shorthand was used. Like the CSS shorthand it replaces any
    /// background colour and image set before it, so when these attributes are merged as the
    /// later source ([`SlideAttrs::merge`], [`SlideAttrs::apply_to`]) the earlier
    /// `data-background-color` and `data-background-image` are removed first. Not part of the
    /// IR. (Additive to the T11 task-file struct.)
    pub background_shorthand: bool,
}

/// The two attributes the `background` shorthand can write (dialect §3.2).
const BACKGROUND_KEYS: [&str; 2] = ["data-background-color", "data-background-image"];

impl SlideAttrs {
    /// Normalizes a raw attribute block for a slide.
    ///
    /// Keys are lowercased. Keys that are not valid attribute names are dropped and recorded
    /// in [`SlideAttrs::invalid_keys`]. `id=` and `class=` pairs behave like `#id` and `.class`
    /// (a `class=` value may hold several space-separated classes); `layout=` sets
    /// [`SlideAttrs::layout`]; every other pair goes through [`normalize_key`]. Empty ids,
    /// classes and layouts are ignored (a slide's layout must be non-empty). Within one block
    /// a later duplicate key wins, and a `background` shorthand also replaces any background
    /// colour or image written before it (`{background-color=navy background=bg.png}` keeps
    /// only the image; `{background=bg.png background-color=navy}` keeps both).
    pub fn from_parsed(p: ParsedAttrs) -> SlideAttrs {
        let mut out = SlideAttrs {
            id: p.id.filter(|id| !id.is_empty()),
            ..SlideAttrs::default()
        };
        for class in p.classes {
            out.add_classes(&class);
        }
        for (written, value) in p.pairs {
            if !is_valid_attr_name(&written) {
                push_unique(&mut out.invalid_keys, written);
                continue;
            }
            let key = written.to_lowercase();
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
                    if key == "background" {
                        remove_background(&mut out.attrs);
                        out.background_shorthand = true;
                    }
                    out.attrs.insert(emitted, value);
                }
            }
        }
        out
    }

    /// Merges `later` into `self`; `later` wins for `id`, `layout` and equal `attrs` keys, and
    /// a `background` shorthand in `later` first removes `self`'s background colour and image
    /// (dialect §3.2 "later wins"); classes, unknown and invalid keys accumulate
    /// (deduplicated, order kept).
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
        if later.background_shorthand {
            remove_background(&mut self.attrs);
            self.background_shorthand = true;
        }
        self.attrs.extend(later.attrs);
        for key in later.unknown_keys {
            push_unique(&mut self.unknown_keys, key);
        }
        for key in later.invalid_keys {
            push_unique(&mut self.invalid_keys, key);
        }
    }

    /// Writes the attributes into `slide` with the same precedence as [`SlideAttrs::merge`]:
    /// `id` and `layout` replace the slide's when set, classes are appended (deduplicated),
    /// `attrs` entries are inserted (overwriting equal keys; a `background` shorthand first
    /// removes the slide's background colour and image). `unknown_keys`, `invalid_keys` and
    /// `background_shorthand` are not part of the IR and are dropped.
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
        if self.background_shorthand {
            remove_background(&mut slide.attrs);
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

fn remove_background(attrs: &mut BTreeMap<String, String>) {
    for key in BACKGROUND_KEYS {
        attrs.remove(key);
    }
}

/// Whether `key` may become a `<section>` attribute name: comrak's attribute-key grammar
/// (dialect §3.1), an ASCII letter followed by ASCII letters, digits, `-`, `_`, `:` or `.`.
/// The separator parser ([`parse_attr_block`]) accepts any key (quoted keys, `{a} {b}`), so
/// [`SlideAttrs::from_parsed`] uses this to keep malformed names out of the HTML.
pub fn is_valid_attr_name(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
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
        // empty quoted id / class are kept as `""`, like comrak
        "{#\"\"}",
        "{.\"\" .a}",
        // escapes and characters that are Markdown syntax outside the block
        "{title=\"a \\\"q\\\" b\"}",
        "{data-path=\"C:\\\\dir\"}",
        "{data-x=\"&lt;b&gt;\" data-y=\"<b>\"}",
        "{background=\"img/_draft_.png\" data-z=\"a*b*c $x$ `c`\"}",
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

    /// A small deterministic generator (64-bit LCG), so the differential test needs no
    /// dependency and always checks the same blocks.
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) % n as u64) as usize
        }
    }

    /// Differential check: random blocks over the grammar's alphabet; whenever comrak accepts
    /// one as heading attributes, `parse_attr_block` must return the same `ParsedAttrs`.
    #[test]
    fn parse_attr_block_agrees_with_comrak_on_random_blocks() {
        let alphabet: Vec<char> = "ab#.=\" \\'-_:%1\tZé".chars().collect();
        let mut rng = Lcg(0x5eed);
        let mut accepted = 0;
        for _ in 0..20_000 {
            let len = rng.below(12);
            let body: String = (0..len)
                .map(|_| alphabet[rng.below(alphabet.len())])
                .collect();
            let block = format!("{{{body}}}");
            let Some(from_comrak) = comrak_heading_attrs(&block) else {
                continue;
            };
            accepted += 1;
            assert_eq!(
                parse_attr_block(&block).ok(),
                Some(from_comrak),
                "{block:?}"
            );
        }
        assert!(
            accepted > 1_000,
            "only {accepted} blocks accepted by comrak"
        );
    }

    /// End to end: the separator form `--- {…}` (read from the source) and the heading form
    /// `# H {…}` give the same slide attributes (dialect §3.1 "the two must agree").
    #[test]
    fn separator_and_heading_forms_agree() {
        use crate::markdown::split::{SplitMode, split_with_source};
        for block in SHARED_CASES {
            let separator = format!("# A\n\n--- {block}\n\nbody\n");
            let heading = format!("# H {block}\n");
            let [from_separator, from_heading] = [separator, heading].map(|text| {
                let arena = comrak::Arena::new();
                let root = comrak::parse_document(&arena, &text, &dialect_options());
                let deck = split_with_source(root, &text, SplitMode::Rules).unwrap();
                let (slide, _) = deck.slides.into_iter().last().unwrap();
                slide.attrs
            });
            assert_eq!(from_separator, from_heading, "{block}");
            assert_eq!(from_separator, slide_attrs(block), "{block}");
        }
    }

    /// A3: blocks comrak rejects on a heading but §3.1 allows; the heading form reads them
    /// as the separator form does (`markdown::recover`).
    const RECOVERED_CASES: &[&str] = &[
        "{.dark background=#222}",
        "{background=figures/bg.png}",
        "{background=https://ex.com/a.png?x=1&y=2}",
        "{lines=1|3 .numbered}",
        "{lines=1,3}",
        "{k='v w' #id}",
        "{title='say \"hi\"' data-x='a\\'b'}",
        "{data-x=a(b);c data-y=+!*@~$^}",
    ];

    #[test]
    fn read_element_attrs_reads_what_parse_attr_block_reads() {
        for block in SHARED_CASES.iter().chain(RECOVERED_CASES) {
            assert_eq!(
                read_element_attrs(block),
                ElementAttrs::Attrs(parse_attr_block(block).unwrap()),
                "{block}"
            );
        }
        for block in RECOVERED_CASES {
            assert_eq!(comrak_heading_attrs(block), None, "comrak accepts {block}");
        }
    }

    #[test]
    fn read_element_attrs_keeps_text_and_reports_mistakes() {
        for text in [
            "{1, 2, 3}",
            "{curly braces}",
            "{r}",
            "{python}",
            "{it's}",
            "{=v}",
            "no braces",
        ] {
            assert_eq!(read_element_attrs(text), ElementAttrs::Text, "{text}");
        }
        let invalid = |block: &str| match read_element_attrs(block) {
            ElementAttrs::Invalid(reason) => reason,
            other => panic!("{block}: {other:?}"),
        };
        assert_eq!(
            invalid("{.dark auto-animate}"),
            "`auto-animate` has no value: write `auto-animate=\"\"` (a key without a value \
             works only in `--- {…}` and `<!-- .slide: -->`)"
        );
        assert!(invalid("{auto-animate}").starts_with("`auto-animate` has no value"));
        assert!(invalid("{layout}").starts_with("`layout` has no value"));
        assert_eq!(invalid("{k='v}"), "unbalanced ' quote");
        assert_eq!(invalid("{.a k=\"v}"), "unbalanced \" quote");
        assert_eq!(
            invalid("{r, echo=FALSE}"),
            "`r,` is not an attribute (`#id`, `.class` or `key=value`)"
        );
        assert_eq!(invalid("{.a 1x=2}"), "`1x` is not a valid attribute name");
        assert_eq!(invalid("{.a \"k\"=v}"), "`k` is not a valid attribute name");
        assert_eq!(invalid("{# .a}"), "`#` without a name after it");
    }

    /// Every block comrak accepts on a heading reads the same with `read_element_attrs`.
    #[test]
    fn read_element_attrs_agrees_with_comrak_on_random_blocks() {
        let alphabet: Vec<char> = "ab#.=\" \\'-_:%1\tZé/|".chars().collect();
        let mut rng = Lcg(0xa3);
        let mut accepted = 0;
        for _ in 0..20_000 {
            let len = rng.below(12);
            let body: String = (0..len)
                .map(|_| alphabet[rng.below(alphabet.len())])
                .collect();
            let block = format!("{{{body}}}");
            let Some(from_comrak) = comrak_heading_attrs(&block) else {
                continue;
            };
            accepted += 1;
            assert_eq!(
                read_element_attrs(&block),
                ElementAttrs::Attrs(from_comrak),
                "{block:?}"
            );
        }
        assert!(
            accepted > 1_000,
            "only {accepted} blocks accepted by comrak"
        );
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
            // bare `#` / `.` and empty keys are ignored; quoted empty id / class are kept
            ("{# . =v \"\" .ok}", attrs(None, &["ok"], &[])),
            ("{#\"\" .\"\" .''}", attrs(Some(""), &["", ""], &[])),
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

    /// The `{key: value}` map of a slide's attrs, for short assertions.
    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn background_shorthand_behaves_like_the_css_shorthand() {
        let image = ("data-background-image", "bg.png");
        let navy = ("data-background-color", "navy");
        // within one block: a later `background` replaces colour and image written before it
        for (block, expected) in [
            ("{background=bg.png background=navy}", vec![navy]),
            ("{background=navy background=bg.png}", vec![image]),
            ("{background-color=navy background=bg.png}", vec![image]),
            ("{data-background-image=x.png background=navy}", vec![navy]),
            // a longhand after the shorthand only sets its own attribute
            (
                "{background=bg.png background-color=navy}",
                vec![navy, image],
            ),
            (
                "{background-image=bg.png background-color=navy}",
                vec![navy, image],
            ),
        ] {
            let s = slide_attrs(block);
            assert_eq!(s.attrs, map(&expected), "{block}");
            assert_eq!(
                s.background_shorthand,
                block.contains("background="),
                "{block}"
            );
        }

        // across sources: the later source's shorthand wins, a later longhand adds to it
        let merged = |earlier: &str, later: &str| {
            let mut s = slide_attrs(earlier);
            s.merge(slide_attrs(later));
            s.attrs
        };
        assert_eq!(
            merged("{background=photo.jpg}", "{background=navy}"),
            map(&[navy])
        );
        assert_eq!(
            merged("{background=#000}", "{background=bg.png}"),
            map(&[image])
        );
        assert_eq!(
            merged("{background-image=bg.png .x}", "{background=navy}"),
            map(&[navy])
        );
        assert_eq!(
            merged("{background=bg.png}", "{background-color=navy}"),
            map(&[navy, image])
        );
        assert_eq!(
            merged("{background=bg.png data-x=1}", "{transition=fade}"),
            map(&[image, ("data-transition", "fade"), ("data-x", "1")])
        );

        // apply_to: the shorthand also replaces what the slide already has
        let mut slide = Slide::new();
        slide.attrs = map(&[image, ("data-x", "1")]);
        slide_attrs("{background=navy}").apply_to(&mut slide);
        assert_eq!(slide.attrs, map(&[navy, ("data-x", "1")]));
        let mut slide = Slide::new();
        slide.attrs = map(&[image]);
        slide_attrs("{background-color=navy}").apply_to(&mut slide);
        assert_eq!(slide.attrs, map(&[navy, image]));
    }

    #[test]
    fn invalid_attribute_names_are_dropped() {
        for (key, valid) in [
            ("k", true),
            ("data-x", true),
            ("aria-label", true),
            ("x:y.z_w-1", true),
            ("Data-X", true),
            ("", false),
            ("1x", false),
            ("-x", false),
            ("_x", false),
            ("a b", false),
            (">x", false),
            ("a}", false),
            ("{b", false),
            ("a\"b", false),
            ("é", false),
            ("a=b", false),
        ] {
            assert_eq!(is_valid_attr_name(key), valid, "{key:?}");
        }
        let s = slide_attrs("{\"a b\"=c '>x'=y ok=1 \"a b\"=d Data-Z=2}");
        assert_eq!(s.invalid_keys, ["a b", ">x"]);
        assert_eq!(s.attrs, map(&[("data-z", "2"), ("ok", "1")]));
        assert_eq!(s.unknown_keys, ["ok"]);
        // invalid keys alone change nothing on the slide, and they accumulate on merge
        let mut s = slide_attrs("{a} {b}");
        assert!(s.is_empty());
        assert_eq!(s.invalid_keys, ["a}", "{b"]);
        s.merge(slide_attrs("{'1'=x '{b'=y}"));
        assert_eq!(s.invalid_keys, ["a}", "{b", "1"]);
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
