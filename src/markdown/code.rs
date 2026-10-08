//! Code fence rendering (`lines=`, language classes) for reveal's highlight plugin. Implemented by T12.
//!
//! Contract: `docs/spec/markdown-dialect.md` §5. comrak parses the `{…}` block after the
//! language into node attributes (the info string is then just the language), and
//! [`code_attrs`] applies the §5 table to both:
//!
//! | info / attribute | effect |
//! |---|---|
//! | first word of info | `class="language-<word>"` on `<code>`; none → no class |
//! | `lines="SPEC"` | `data-line-numbers="SPEC"` |
//! | `.numbered` | `data-line-numbers=""` unless `lines=` is given |
//! | `start=N` | `data-ln-start-from="N"` |
//! | `trim=false` | no `data-trim` (default: present) |
//! | `escape=false` | `data-noescape`, source emitted unescaped |
//! | `.plain` | `class="nohighlight"` on `<code>` (replaces the language class) |
//! | `#id`, other `.classes` | on `<pre>` (`id=` / `class=` pairs too) |
//! | other `key=value` | `data-key="value"` on `<code>` (a key already starting with `data-` is kept) |
//!
//! [`render_code_block`] writes `<pre …><code …>source</code></pre>` with the trailing newline
//! of the fence content removed and the source escaped with `html_escape::encode_text`
//! (unless `escape=false`). `<code>` attributes are written in a fixed order: `class`,
//! `data-trim`, `data-line-numbers`, `data-ln-start-from`, `data-noescape`, then every other
//! key alphabetically. Attribute values are escaped with `encode_double_quoted_attribute`.

use std::collections::BTreeMap;

use html_escape::{encode_double_quoted_attribute, encode_text};

use crate::markdown::attrs::{ParsedAttrs, is_valid_attr_name};

/// Keys written first on `<code>`, in this order (after `class`).
const ORDERED_KEYS: [&str; 4] = [
    "data-trim",
    "data-line-numbers",
    "data-ln-start-from",
    "data-noescape",
];

/// Keys written as bare boolean attributes when their value is empty.
const FLAG_KEYS: [&str; 2] = ["data-trim", "data-noescape"];

/// Attributes of one fenced code block after applying the dialect §5 table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodeAttrs {
    /// First word of the info string; `<code class="language-<lang>">`. `None` when the info
    /// string is empty or `.plain` is set (highlight.js would otherwise still pick the
    /// language class).
    pub lang: Option<String>,
    /// `#id` (or an `id=` pair) for `<pre>`.
    pub pre_id: Option<String>,
    /// Classes for `<pre>`: every `.class` except `numbered` and `plain`, plus `class=` pairs.
    pub pre_classes: Vec<String>,
    /// Further `<code>` attributes: `class` (`nohighlight` for `.plain`), `data-trim`,
    /// `data-line-numbers`, `data-ln-start-from`, `data-noescape` and `data-<key>` for every
    /// other pair. `data-trim` and `data-noescape` have empty values and are written bare.
    pub code_attrs: BTreeMap<String, String>,
    /// `false` for `escape=false`: the source is emitted as written.
    pub escape: bool,
}

/// Applies the dialect §5 table to a fence's info string and its comrak-parsed attributes.
///
/// Keys are matched ASCII case-insensitively and emitted lowercased. `trim=` and `escape=`
/// switch off only for the value `false` (any case). Pairs whose key is not a valid
/// attribute name ([`is_valid_attr_name`]) are dropped (comrak's grammar never produces
/// them, but `attrs` may come from elsewhere).
pub fn code_attrs(info: &str, attrs: Option<&ParsedAttrs>) -> CodeAttrs {
    let mut out = CodeAttrs {
        lang: info.split_whitespace().next().map(str::to_string),
        escape: true,
        ..CodeAttrs::default()
    };
    let (mut trim, mut numbered, mut plain, mut lines) = (true, false, false, None);
    let mut add_class = |out: &mut CodeAttrs, class: &str| match class {
        "numbered" => numbered = true,
        "plain" => plain = true,
        "" => {}
        _ if !out.pre_classes.iter().any(|c| c == class) => out.pre_classes.push(class.into()),
        _ => {}
    };
    if let Some(attrs) = attrs {
        out.pre_id = attrs.id.clone().filter(|id| !id.is_empty());
        for class in &attrs.classes {
            add_class(&mut out, class);
        }
        for (key, value) in &attrs.pairs {
            let key = key.to_ascii_lowercase();
            match key.as_str() {
                "lines" => lines = Some(value.clone()),
                "start" => {
                    out.code_attrs
                        .insert("data-ln-start-from".into(), value.clone());
                }
                "trim" => trim = !value.eq_ignore_ascii_case("false"),
                "escape" => out.escape = !value.eq_ignore_ascii_case("false"),
                "id" if !value.is_empty() => out.pre_id = Some(value.clone()),
                "id" => {}
                "class" => value
                    .split_whitespace()
                    .for_each(|c| add_class(&mut out, c)),
                _ if !is_valid_attr_name(&key) => {
                    tracing::debug!("code fence attribute `{key}` is not a valid name; dropped");
                }
                _ if key.starts_with("data-") => {
                    out.code_attrs.insert(key, value.clone());
                }
                _ => {
                    out.code_attrs.insert(format!("data-{key}"), value.clone());
                }
            }
        }
    }
    if plain {
        out.lang = None;
        out.code_attrs.insert("class".into(), "nohighlight".into());
    }
    match lines {
        Some(spec) => {
            out.code_attrs.insert("data-line-numbers".into(), spec);
        }
        None if numbered => {
            out.code_attrs
                .insert("data-line-numbers".into(), String::new());
        }
        None => {}
    }
    if trim {
        out.code_attrs.insert("data-trim".into(), String::new());
    } else {
        out.code_attrs.remove("data-trim");
    }
    if !out.escape {
        out.code_attrs.insert("data-noescape".into(), String::new());
    }
    out
}

/// `<pre …><code …>source</code></pre>` (no trailing newline) for a code fence's literal
/// content `source`; see the module docs for the attribute order and escaping.
pub fn render_code_block(source: &str, attrs: &CodeAttrs) -> String {
    render_code_block_with(source, attrs, None)
}

/// [`render_code_block`] with an optional `data-sourcepos` value for `<pre>` (written first,
/// as comrak does when `render.sourcepos` is on).
pub(crate) fn render_code_block_with(
    source: &str,
    attrs: &CodeAttrs,
    sourcepos: Option<&str>,
) -> String {
    let mut html = String::from("<pre");
    if let Some(pos) = sourcepos {
        push_attr(&mut html, "data-sourcepos", pos);
    }
    if let Some(id) = &attrs.pre_id {
        push_attr(&mut html, "id", id);
    }
    if !attrs.pre_classes.is_empty() {
        push_attr(&mut html, "class", &attrs.pre_classes.join(" "));
    }
    html.push_str("><code");
    let classes: Vec<String> = attrs
        .lang
        .iter()
        .map(|lang| format!("language-{lang}"))
        .chain(attrs.code_attrs.get("class").cloned())
        .collect();
    if !classes.is_empty() {
        push_attr(&mut html, "class", &classes.join(" "));
    }
    let ordered = ORDERED_KEYS
        .iter()
        .filter_map(|key| attrs.code_attrs.get_key_value(*key));
    let rest = attrs
        .code_attrs
        .iter()
        .filter(|(key, _)| key.as_str() != "class" && !ORDERED_KEYS.contains(&key.as_str()));
    for (key, value) in ordered.chain(rest) {
        if value.is_empty() && FLAG_KEYS.contains(&key.as_str()) {
            html.push(' ');
            html.push_str(key);
        } else {
            push_attr(&mut html, key, value);
        }
    }
    html.push('>');
    let source = strip_trailing_newline(source);
    if attrs.escape {
        html.push_str(&encode_text(source));
    } else {
        html.push_str(source);
    }
    html.push_str("</code></pre>");
    html
}

/// Appends ` key="value"` with the value escaped for a double-quoted attribute.
pub(crate) fn push_attr(html: &mut String, key: &str, value: &str) {
    html.push(' ');
    html.push_str(key);
    html.push_str("=\"");
    html.push_str(&encode_double_quoted_attribute(value));
    html.push('"');
}

/// `source` without one trailing `\n` (or `\r\n`).
pub(crate) fn strip_trailing_newline(source: &str) -> &str {
    let source = source.strip_suffix('\n').unwrap_or(source);
    source.strip_suffix('\r').unwrap_or(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::attrs::parse_attr_block;

    fn render(info: &str, block: &str, source: &str) -> String {
        let attrs = (!block.is_empty()).then(|| parse_attr_block(block).unwrap());
        render_code_block(source, &code_attrs(info, attrs.as_ref()))
    }

    #[test]
    fn language_class_and_escaping() {
        assert_eq!(
            render("python", "", "a < b && c > d\n"),
            r#"<pre><code class="language-python" data-trim>a &lt; b &amp;&amp; c &gt; d</code></pre>"#
        );
    }

    #[test]
    fn no_info_string_means_no_class() {
        assert_eq!(render("", "", "x\n"), "<pre><code data-trim>x</code></pre>");
        assert_eq!(code_attrs("", None).lang, None);
        assert_eq!(
            code_attrs("rust extra words", None).lang.as_deref(),
            Some("rust")
        );
    }

    #[test]
    fn lines_numbered_and_start() {
        assert_eq!(
            render("python", r#"{lines="1-2|4"}"#, "x\n"),
            r#"<pre><code class="language-python" data-trim data-line-numbers="1-2|4">x</code></pre>"#
        );
        assert_eq!(
            render("rust", "{.numbered}", "x\n"),
            r#"<pre><code class="language-rust" data-trim data-line-numbers="">x</code></pre>"#
        );
        assert_eq!(
            render("rust", r#"{.numbered lines="2"}"#, "x\n"),
            r#"<pre><code class="language-rust" data-trim data-line-numbers="2">x</code></pre>"#
        );
        assert_eq!(
            render("js", r#"{lines="1|2" start=10}"#, "x\n"),
            r#"<pre><code class="language-js" data-trim data-line-numbers="1|2" data-ln-start-from="10">x</code></pre>"#
        );
    }

    #[test]
    fn trim_false_and_escape_false() {
        assert_eq!(
            render("text", "{trim=false}", "    x\n"),
            r#"<pre><code class="language-text">    x</code></pre>"#
        );
        assert_eq!(
            render("html", "{escape=false}", "<mark>x</mark> &amp;\n"),
            r#"<pre><code class="language-html" data-trim data-noescape><mark>x</mark> &amp;</code></pre>"#
        );
        // only `false` switches off
        assert_eq!(
            render("text", "{trim=true escape=yes}", "<\n"),
            r#"<pre><code class="language-text" data-trim>&lt;</code></pre>"#
        );
    }

    #[test]
    fn plain_replaces_the_language_class() {
        assert_eq!(
            render("python", "{.plain}", "x\n"),
            r#"<pre><code class="nohighlight" data-trim>x</code></pre>"#
        );
        assert_eq!(
            render("", "{.plain}", "x\n"),
            r#"<pre><code class="nohighlight" data-trim>x</code></pre>"#
        );
    }

    #[test]
    fn id_and_classes_go_on_pre() {
        assert_eq!(
            render("python", "{#pre-id .big .wide .big}", "x\n"),
            r#"<pre id="pre-id" class="big wide"><code class="language-python" data-trim>x</code></pre>"#
        );
        assert_eq!(
            render("python", r#"{id=p class="a numbered b"}"#, "x\n"),
            r#"<pre id="p" class="a b"><code class="language-python" data-trim data-line-numbers="">x</code></pre>"#
        );
    }

    #[test]
    fn other_pairs_become_data_attributes() {
        assert_eq!(
            render(
                "python",
                r#"{title=Example foo=bar data-x="a&quot;b" Upper=1}"#,
                "x\n"
            ),
            r#"<pre><code class="language-python" data-trim data-foo="bar" data-title="Example" data-upper="1" data-x="a&amp;quot;b">x</code></pre>"#
        );
        assert_eq!(
            render("python", r#"{title='say "hi"'}"#, "x\n"),
            r#"<pre><code class="language-python" data-trim data-title="say &quot;hi&quot;">x</code></pre>"#
        );
    }

    #[test]
    fn the_combined_example_follows_the_table() {
        let block = r#"{lines="1-2|4" .numbered start=10 trim=false escape=false #snippet .big}"#;
        assert_eq!(
            render("python", block, "print(df.<mark>describe</mark>())\n"),
            r#"<pre id="snippet" class="big"><code class="language-python" data-line-numbers="1-2|4" data-ln-start-from="10" data-noescape>print(df.<mark>describe</mark>())</code></pre>"#
        );
    }

    #[test]
    fn only_one_trailing_newline_is_removed() {
        assert_eq!(strip_trailing_newline("a\n\n"), "a\n");
        assert_eq!(strip_trailing_newline("a\r\n"), "a");
        assert_eq!(strip_trailing_newline("a"), "a");
        assert_eq!(strip_trailing_newline(""), "");
    }

    #[test]
    fn sourcepos_goes_first_on_pre() {
        let attrs = code_attrs("py", parse_attr_block("{#x}").ok().as_ref());
        assert_eq!(
            render_code_block_with("y\n", &attrs, Some("3:1-5:3")),
            r#"<pre data-sourcepos="3:1-5:3" id="x"><code class="language-py" data-trim>y</code></pre>"#
        );
    }
}
