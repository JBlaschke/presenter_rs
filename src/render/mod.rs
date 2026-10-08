//! HTML rendering: template context, helpers and reveal.js configuration.
//!
//! Contract: `docs/spec/theme-format.md` §4–§5, `docs/spec/config.md` §4,
//! `docs/spec/vendored-assets.md` §2–§4, `docs/spec/live-reload.md` §1. Implemented by T15.
//!
//! [`render`] turns `Deck + AssetRegistry + Theme + Config + OutputMode` into the final HTML
//! document: it builds the §4 context ([`context::build_context`]), compiles the theme
//! stylesheet ([`html::compile_theme_css`]), renders `deck.html` in a fresh environment of the
//! theme chain with the §4 helpers (`style`, `script`, `asset`, `theme_style`, `reveal_init`,
//! `live_reload`) and filters (`attrs`, `markdown`), and finally resolves every `asset:<id>`
//! placeholder for the output mode. Nothing is written to disk: site-mode files come back in
//! [`RenderedDocument::files`] for the caller (T16) to write.
//!
//! Helper paths (`style(path)`, `script(path)`, `asset(path)` and the dev server's
//! `/__presenter/asset/{*path}` route) are resolved by one function,
//! [`resolve_helper_path`], which understands the four forms of §4: `vendor/…`,
//! `presenter/…`, `@<theme>/<path>` and anything else (the chain's `assets/` directories).

pub mod context;
pub mod html;
pub mod reveal;

use serde::Serialize;

use crate::assets::emit::{EmittedFile, OutputMode};
use crate::assets::registry::AssetRegistry;
use crate::config::Config;
use crate::error::Diagnostics;
use crate::ir::Deck;
use crate::theme::Theme;

pub use context::build_context;
pub use html::{compile_theme_css, render, resolve_helper_path};

/// Which command is rendering: `presenter build` or `presenter serve`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildMode {
    /// A one-off build (`presenter build`); serialized as `"build"`.
    Build,
    /// The dev server (`presenter serve`); serialized as `"serve"`. Templates are rendered
    /// with minijinja's debug mode on, so errors carry the source excerpt.
    Serve,
}

/// The template context's `build` object (theme-format.md §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BuildInfo {
    /// Build or serve.
    pub mode: BuildMode,
    /// True for a single self-contained file ([`OutputMode::Standalone`]).
    pub standalone: bool,
    /// True when `live_reload()` must emit the live-reload client script (serve mode).
    pub live_reload: bool,
    /// Serve rebuild counter (`?v=<id>` on the theme stylesheet URL); 0 in build mode.
    pub id: u64,
    /// The presenter version (`env!("CARGO_PKG_VERSION")`).
    pub version: &'static str,
}

impl BuildInfo {
    /// `presenter build`: id 0, no live reload; `standalone` says whether the output is a
    /// single file (`true`) or a site directory.
    pub fn build(standalone: bool) -> BuildInfo {
        BuildInfo {
            mode: BuildMode::Build,
            standalone,
            live_reload: false,
            id: 0,
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    /// `presenter serve`, rebuild number `id`: not standalone, live reload on.
    pub fn serve(id: u64) -> BuildInfo {
        BuildInfo {
            mode: BuildMode::Serve,
            standalone: false,
            live_reload: true,
            id,
            version: env!("CARGO_PKG_VERSION"),
        }
    }
}

/// Everything [`render`] needs.
#[derive(Debug, Clone)]
pub struct RenderInput<'a> {
    /// The parsed deck (its HTML uses `asset:<id>` placeholders).
    pub deck: &'a Deck,
    /// The assets the deck's placeholders name.
    pub assets: &'a AssetRegistry,
    /// The resolved theme chain.
    pub theme: &'a Theme,
    /// The project configuration. The renderer applies the theme's `[reveal]` defaults
    /// (under it) and the deck's front matter (over it) itself; both merges are idempotent,
    /// so an already merged config works too.
    pub config: &'a Config,
    /// How helpers emit files and how placeholders are resolved.
    pub output: OutputMode,
    /// The context's `build` object.
    pub build: BuildInfo,
}

/// The output of [`render`].
#[derive(Debug, Clone)]
pub struct RenderedDocument {
    /// The final HTML document, every placeholder resolved for the output mode.
    pub html: String,
    /// The compiled theme stylesheet (all `[styles] entries` of the chain, parent first)
    /// resolved for the output mode: inlined into `html` (standalone, `data:` URIs), written
    /// as `assets/theme-<hash>.css` (site; that file is also in [`RenderedDocument::files`])
    /// or served at `/__presenter/theme.css` (serve; its assets are served from
    /// `/__presenter/deck-asset/<id>` out of [`RenderedDocument::assets`]).
    pub theme_css: String,
    /// Site mode: files to write next to the document (assets, copied stylesheets and
    /// scripts, the theme stylesheet), in first-use order. Empty in the other modes.
    pub files: Vec<EmittedFile>,
    /// Number of slides: top-level slides plus their vertical children (for the CLI summary).
    pub slide_count: usize,
    /// Warnings found while rendering: `unknown-layout` (a slide's layout is not a template
    /// of the chain; the default layout is used) and `standalone-background-video` (a local
    /// background video cannot play from a `data:` URI).
    pub diagnostics: Diagnostics,
    /// The deck's registry plus everything the renderer registered (theme stylesheet
    /// `url()` targets, files copied by helpers). The dev server serves
    /// `/__presenter/deck-asset/<id>` from this registry.
    pub assets: AssetRegistry,
}
