//! Template context builder (`docs/spec/theme-format.md` §4): `Deck` + theme + config →
//! the object `deck.html` receives (and `presenter dump --context` prints).
//!
//! Shape: `deck` (the IR), `meta`, `slides` (each slide and each vertical child decorated
//! with `layout_template`), `features`, `config` (the effective config as given in
//! [`RenderInput::config`]; its `reveal` table is the full option map including the
//! built-in defaults and `plugins`), `reveal` (`config_json`, `plugins`, `plugin_scripts`),
//! `theme` (`name`, `chain`, `reveal_theme`, `highlight_style`, `vars`, `scripts`) and
//! `build`.
//!
//! Theme-relative files are given as `@<theme>/<path>` helper paths (the theme-format §4
//! contract change), so templates pass `theme.scripts` entries and a path-valued
//! `theme.highlight_style` to `script()`/`style()` unchanged.

use serde::Serialize;
use serde_json::{Map, Value};

use super::RenderInput;
use super::reveal;
use crate::config::Config;
use crate::error::{Diagnostic, Diagnostics, Result};
use crate::ir::{Deck, DeckMeta, Slide};
use crate::theme::Theme;

/// Builds the §4 context for `input` (whose config is the effective one, see
/// [`RenderInput::config`]), appending an `unknown-layout` warning to `diagnostics` for
/// every slide whose layout the theme chain does not provide (such a slide gets
/// `layout_template = "layouts/default.html"`).
///
/// Fails when a `[scripts] entries` file of the theme is missing
/// ([`Theme::script_entries`]) or the `highlight_style` file is missing or outside its theme
/// ([`Theme::highlight_style_file`]).
pub fn build_context(
    input: &RenderInput<'_>,
    diagnostics: &mut Diagnostics,
) -> Result<minijinja::Value> {
    Ok(minijinja::Value::from_serialize(context_json(
        input,
        diagnostics,
    )?))
}

/// [`build_context`] as a JSON object, keys in §4 order (what `presenter dump --context`
/// prints).
pub fn context_json(input: &RenderInput<'_>, diagnostics: &mut Diagnostics) -> Result<Value> {
    let deck = input.deck;
    let config = input.config;
    let mut context = Map::new();
    context.insert("deck".into(), to_json(deck)?);
    context.insert("meta".into(), to_json(&deck.meta)?);
    context.insert(
        "slides".into(),
        decorated_slides(deck, input.theme, diagnostics)?,
    );
    context.insert("features".into(), to_json(&deck.features)?);
    context.insert("config".into(), config_json(config)?);
    context.insert("reveal".into(), reveal_json(config, deck));
    context.insert("theme".into(), theme_json(input.theme)?);
    context.insert("build".into(), to_json(&input.build)?);
    Ok(Value::Object(context))
}

/// `config` with the theme's `[reveal]` defaults filled in where `config.reveal` has no
/// value and the deck's front matter merged over it (config.md §1): the effective config
/// without the command-line layer, which the caller applies afterwards
/// ([`Config::apply_cli`], [`Config::apply_cli_reveal`]). Both merges are idempotent.
///
/// Fails when the front matter's config tables do not merge
/// ([`Config::merge_front_matter`]).
pub fn effective_config(config: &Config, meta: &DeckMeta, theme: &Theme) -> Result<Config> {
    let mut config = config.clone();
    config.merge_theme_reveal(&theme.reveal_defaults());
    config.merge_front_matter(meta)?;
    Ok(config)
}

fn to_json<T: Serialize + ?Sized>(value: &T) -> Result<Value> {
    Ok(serde_json::to_value(value)?)
}

/// The serialized config with `reveal` replaced by the effective option map (built-in
/// defaults included), so templates see the values reveal.js will use.
fn config_json(config: &Config) -> Result<Value> {
    let mut value = to_json(config)?;
    if let Value::Object(map) = &mut value {
        map.insert(
            "reveal".into(),
            Value::Object(sorted(config.effective_reveal())),
        );
    }
    Ok(value)
}

/// `map` with its top-level keys in sorted order.
fn sorted(map: Map<String, Value>) -> Map<String, Value> {
    let mut entries: Vec<(String, Value)> = map.into_iter().collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries.into_iter().collect()
}

/// The context's `reveal` object.
fn reveal_json(config: &Config, deck: &Deck) -> Value {
    let plugins = reveal::plugins(config, &deck.features);
    let mut map = Map::new();
    map.insert("config_json".into(), config.reveal_options_json().into());
    map.insert(
        "plugins".into(),
        plugins
            .iter()
            .map(|p| p.js_ident)
            .collect::<Vec<_>>()
            .into(),
    );
    map.insert(
        "plugin_scripts".into(),
        plugins.iter().map(|p| p.script).collect::<Vec<_>>().into(),
    );
    Value::Object(map)
}

/// The context's `theme` object.
fn theme_json(theme: &Theme) -> Result<Value> {
    let scripts: Vec<String> = theme
        .script_entries()?
        .into_iter()
        .map(|file| format!("@{}/{}", file.theme, file.rel_path))
        .collect();
    let chain: Vec<&str> = theme.chain.iter().map(|dir| dir.name.as_str()).collect();
    let mut map = Map::new();
    map.insert("name".into(), theme.name().into());
    map.insert("chain".into(), chain.into());
    map.insert("reveal_theme".into(), theme.reveal_theme().into());
    map.insert("highlight_style".into(), highlight_style(theme)?.into());
    map.insert("vars".into(), Value::Object(theme.vars()));
    map.insert("scripts".into(), scripts.into());
    Ok(Value::Object(map))
}

/// `highlight_style` for templates: a built-in name as is, a path as `@<theme>/<path>`
/// relative to the theme that set it. A path that is missing or leaves the theme directory
/// is an [`crate::error::Error::Theme`] ([`Theme::highlight_style_file`]).
fn highlight_style(theme: &Theme) -> Result<String> {
    Ok(match theme.highlight_style_file()? {
        Some(file) => format!("@{}/{}", file.theme, file.rel_path),
        None => theme.highlight_style(),
    })
}

/// `deck.slides` as JSON, every slide and child decorated with `layout_template`.
fn decorated_slides(deck: &Deck, theme: &Theme, diagnostics: &mut Diagnostics) -> Result<Value> {
    let mut slides = Vec::with_capacity(deck.slides.len());
    for (i, slide) in deck.slides.iter().enumerate() {
        let label = format!("slide {}", i + 1);
        let mut value = decorated(slide, &label, deck, theme, diagnostics)?;
        let mut children = Vec::with_capacity(slide.children.len());
        for (j, child) in slide.children.iter().enumerate() {
            let label = format!("slide {}.{}", i + 1, j + 1);
            children.push(decorated(child, &label, deck, theme, diagnostics)?);
        }
        if let Value::Object(map) = &mut value {
            map.insert("children".into(), Value::Array(children));
        }
        slides.push(value);
    }
    Ok(Value::Array(slides))
}

/// One slide as JSON plus `layout_template`.
fn decorated(
    slide: &Slide,
    label: &str,
    deck: &Deck,
    theme: &Theme,
    diagnostics: &mut Diagnostics,
) -> Result<Value> {
    let template = if theme.has_layout(&slide.layout) {
        format!("layouts/{}.html", slide.layout)
    } else {
        diagnostics.push(unknown_layout(slide, label, deck, theme));
        "layouts/default.html".to_string()
    };
    let mut value = to_json(slide)?;
    if let Value::Object(map) = &mut value {
        map.insert("layout_template".into(), template.into());
    }
    Ok(value)
}

/// The `unknown-layout` warning for `slide` ([`Theme::unknown_layout_message`], shared with
/// `presenter check`).
fn unknown_layout(slide: &Slide, label: &str, deck: &Deck, theme: &Theme) -> Diagnostic {
    let mut diagnostic = Diagnostic::warning(
        "unknown-layout",
        theme.unknown_layout_message(label, &slide.layout),
    )
    .with_path(&deck.source.path);
    diagnostic.span = slide.span.clone();
    diagnostic
}
