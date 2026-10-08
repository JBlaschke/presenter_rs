//! Jupyter notebook frontend (nbformat 4).
//!
//! Contract: `docs/spec/notebook-mapping.md`. [`parse`] (T31) turns one `.ipynb` file into a
//! [`Parsed`] deck:
//!
//! 1. [`nbformat::parse_notebook`] reads the JSON leniently (T30); its warnings (skipped
//!    outputs, unknown slide types, …) start the diagnostics;
//! 2. [`convert::convert`] merges the deck metadata (`metadata.title`, `metadata.presenter`,
//!    first-markdown-cell front matter) into a clone of `config` and maps the cells to slides
//!    (slide types, tags, notes, raw cells), rendering Markdown cells with the Markdown
//!    frontend's renderer in notebook mode and outputs with [`outputs::render_outputs`] (T32);
//! 3. the diagnostics get the notebook path and are ordered by cell, then line (notebook-level
//!    ones first).
//!
//! Missing local files (in raw HTML cells, `text/html` outputs, Markdown images, slide
//! backgrounds) never fail [`parse`]: they are `missing-asset` warnings, as in the Markdown
//! frontend (ir.md §4).

pub mod convert;
pub mod nbformat;
pub mod outputs;

use std::path::Path;

use crate::assets::registry::AssetRegistry;
use crate::config::Config;
use crate::error::{Diagnostics, Error, Result};
use crate::ir::{Parsed, SourceInfo};

/// Parses a Jupyter notebook (`json` is the `.ipynb` file's contents) into a [`Parsed`] deck.
///
/// Contract: `docs/spec/ir.md` §4 and `docs/spec/notebook-mapping.md` §2–§5; see the module
/// docs for the steps. Pure: besides `json` it only `stat`s files named by relative URLs
/// (resolved against `source.base_dir`) to register them.
///
/// # Errors
/// `Error::Parse` (with `source.path`) for JSON that is not an nbformat ≥ 4 notebook or a
/// separator with an unbalanced quote in a split markdown cell; `Error::FrontMatter` for
/// invalid `metadata.presenter` or first-cell front matter.
pub fn parse(source: &SourceInfo, json: &str, config: &Config) -> Result<Parsed> {
    parse_with(source, json, config, false).map(|(parsed, _)| parsed)
}

/// [`parse`] for a caller that merges the deck metadata into its own config afterwards (the
/// pipeline, ir.md §4): deck metadata the conversion does not read (`reveal` options, the
/// `slides` and `math` tables) that the config rejects does not fail it. The
/// `Error::FrontMatter` [`parse`] would return for it (naming the layer) comes back with the
/// deck, for the caller to report together with the deck's diagnostics. Otherwise as
/// [`parse`].
pub(crate) fn parse_deferring_config(
    source: &SourceInfo,
    json: &str,
    config: &Config,
) -> Result<(Parsed, Option<Error>)> {
    parse_with(source, json, config, true)
}

/// [`parse`]; with `defer`, [`parse_deferring_config`].
fn parse_with(
    source: &SourceInfo,
    json: &str,
    config: &Config,
    defer: bool,
) -> Result<(Parsed, Option<Error>)> {
    let (notebook, mut diagnostics) = nbformat::parse_notebook(&source.path, json)?;
    let mut deck_config = config.clone();
    let mut assets = AssetRegistry::new();
    let (deck_config, diags) = (&mut deck_config, &mut diagnostics);
    let (deck, deferred) = if defer {
        convert::convert_deferring_config(&notebook, source, deck_config, &mut assets, diags)?
    } else {
        let deck = convert::convert(&notebook, source, deck_config, &mut assets, diags)?;
        (deck, None)
    };
    tracing::debug!(
        slides = deck.slides.len(),
        assets = assets.len(),
        warnings = diagnostics.len(),
        "parsed notebook"
    );
    let parsed = Parsed {
        deck,
        assets,
        diagnostics: finish_diagnostics(diagnostics, &source.path),
    };
    Ok((parsed, deferred))
}

/// Sets `path` on every diagnostic that has none and orders them by cell, then line
/// (stable; diagnostics without a span come first).
fn finish_diagnostics(diagnostics: Diagnostics, path: &Path) -> Diagnostics {
    let mut diagnostics = diagnostics.into_vec();
    for diagnostic in &mut diagnostics {
        diagnostic.path.get_or_insert_with(|| path.to_path_buf());
    }
    diagnostics.sort_by_key(|d| d.span.as_ref().map(|span| (span.cell, span.start_line)));
    diagnostics.into()
}
