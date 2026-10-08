//! Build orchestration: parse → resolve theme and config → render → emit (PLAN.md §3.1).
//!
//! Every command that turns a deck into HTML goes through this module: `build` and `dump`
//! (T16), `serve` (T20/T21), `check` (T34) and `export pdf` (T45). The stages:
//!
//! 1. [`load_config`]: `--config PATH`, else [`Config::discover`] from the input's
//!    directory, else the defaults; a relative `[build] out_dir` of that file is made
//!    absolute against the file's directory; the command-line overrides go on top
//!    ([`Config::apply_cli`]).
//! 2. [`parse_only`]: reads the input (UTF-8; errors name the file), picks the frontend by
//!    [`BuildOptions::format`] or the extension ([`detect_kind`]) and parses it into a
//!    [`Parsed`] deck. The returned config also has the deck's front matter merged.
//! 3. [`prepare`]: escalates the first `missing-asset` warning to [`Error::MissingAsset`]
//!    when [`BuildOptions::missing_asset_is_error`] (ir.md §4 contract change), resolves the
//!    theme (`cli.theme` → front matter `theme` → `[deck] theme`, searched in the config
//!    file's directory, the input's directory and the current directory), checks the
//!    theme's `[reveal] plugins` (an [`Error::Theme`] naming the theme) and builds the
//!    **effective** config: [`render::context::effective_config`] (theme `[reveal]` under
//!    the project config, front matter over it), then the command line again, then a strict
//!    check of `reveal.plugins` ([`Config::validate_plugins`]).
//! 4. [`build_deck`]: renders the prepared deck for [`BuildOptions::output`]
//!    ([`render::render`]) and collects the frontend's and the renderer's diagnostics. A
//!    `missing-asset` warning of the renderer (an image in the `markdown` filter's input) is
//!    escalated like the frontend's.
//! 5. [`write_output`]: writes the document (one file, or `index.html` plus the site's
//!    `assets/`); nothing before this stage touches the disk.
//!
//! [`try_parse_only`], [`try_prepare`] and [`try_build_deck`] are [`parse_only`],
//! [`prepare`] and [`build_deck`] that keep the diagnostics collected before a failure
//! ([`BuildFailure`]), so a failed build can still report its warnings and every missing
//! asset, also when the front matter is rejected. [`resolve_theme`] is stage 3's theme
//! lookup on its own (for commands that lint without rendering).
//!
//! Output locations: [`default_output_path`] is `<out_dir>/<stem>.html` (standalone) or
//! `<out_dir>/<stem>/` (site). A relative `out_dir` is relative to the `presenter.toml`
//! that sets it (or whose default it is), like every path in that file; without a
//! `presenter.toml` it is relative to the input's directory. [`output_mode`] gives the
//! matching [`OutputMode`] for an output path.

use std::path::{Path, PathBuf};

use crate::assets::emit::OutputMode;
use crate::assets::registry::{AssetRegistry, normalize_lexically};
use crate::config::{CliOverrides, Config};
use crate::error::{Diagnostics, Error, Level, Result};
use crate::ir::{Deck, Parsed, SourceInfo, SourceKind};
use crate::markdown::render::missing_asset_path;
use crate::render::{self, BuildInfo, RenderInput, RenderedDocument};
use crate::theme::{self, Theme};

/// File name of the document in site mode (`<out>/index.html`).
pub const SITE_INDEX: &str = "index.html";

/// Directory of the emitted files in site mode, relative to the output directory; also
/// the URL prefix the document uses for them (`assets/<name>-<hash>.<ext>`).
pub const SITE_ASSETS_DIR: &str = "assets";

/// The `missing-asset` diagnostic code of the frontends (ir.md §4).
const MISSING_ASSET: &str = "missing-asset";

/// What to build and how. [`BuildOptions::new`] gives the `presenter build` defaults.
///
/// Build other variants with struct update syntax, e.g.
/// `BuildOptions { output: OutputMode::Serve, ..BuildOptions::new(input) }`, so that a
/// field added later does not break the caller ([`Default`] is `BuildOptions::new("")`).
#[derive(Debug, Clone)]
pub struct BuildOptions {
    /// The source file (`.md` or `.ipynb`), as given on the command line.
    pub input: PathBuf,
    /// `--config PATH`: use this `presenter.toml` instead of discovering one.
    pub config_path: Option<PathBuf>,
    /// Command-line overrides (config.md §1, highest precedence).
    pub cli: CliOverrides,
    /// How the document references its assets (standalone, site or serve).
    pub output: OutputMode,
    /// The template context's `build` object.
    pub build: BuildInfo,
    /// Turn the first `missing-asset` warning into [`Error::MissingAsset`] (`build`); `serve`
    /// and `dump` keep it a warning.
    pub missing_asset_is_error: bool,
    /// `--format md|ipynb`: the frontend to use; `None` picks it by extension
    /// ([`detect_kind`]). Additive to the T16 task-file struct (cli.md `build --format`).
    pub format: Option<SourceKind>,
}

impl BuildOptions {
    /// Options for a standalone `presenter build` of `input`: no `--config`, no overrides,
    /// [`OutputMode::Standalone`], [`BuildInfo::build`]`(true)`, missing assets are errors,
    /// format by extension.
    pub fn new(input: impl Into<PathBuf>) -> BuildOptions {
        BuildOptions {
            input: input.into(),
            config_path: None,
            cli: CliOverrides::default(),
            output: OutputMode::Standalone,
            build: BuildInfo::build(true),
            missing_asset_is_error: true,
            format: None,
        }
    }
}

impl Default for BuildOptions {
    /// [`BuildOptions::new`] with an empty input path (set `input` before building).
    fn default() -> BuildOptions {
        BuildOptions::new(PathBuf::new())
    }
}

/// A parsed deck with its theme and effective config, ready to render ([`prepare`]).
#[derive(Debug, Clone)]
pub struct Prepared {
    /// The deck (its HTML uses `asset:<id>` placeholders).
    pub deck: Deck,
    /// The assets the deck's placeholders name.
    pub assets: AssetRegistry,
    /// The resolved theme chain.
    pub theme: Theme,
    /// The effective config (see the module docs, stage 3).
    pub config: Config,
    /// The `presenter.toml` that was used (absolute), if any.
    pub config_path: Option<PathBuf>,
    /// The frontend's diagnostics.
    pub diagnostics: Diagnostics,
}

impl Prepared {
    /// The renderer input for this deck in `output` mode.
    pub fn render_input(&self, output: OutputMode, build: BuildInfo) -> RenderInput<'_> {
        RenderInput {
            deck: &self.deck,
            assets: &self.assets,
            theme: &self.theme,
            config: &self.config,
            output,
            build,
        }
    }
}

/// The result of [`build_deck`].
#[derive(Debug, Clone)]
pub struct Build {
    /// The parsed deck.
    pub deck: Deck,
    /// The deck's assets (what `/__presenter/deck-asset/<id>` serves in serve mode).
    pub assets: AssetRegistry,
    /// The resolved theme chain.
    pub theme: Theme,
    /// The effective config the deck was rendered with.
    pub config: Config,
    /// The `presenter.toml` that was used (absolute), if any.
    pub config_path: Option<PathBuf>,
    /// The rendered document (and, in site mode, the files to write next to it).
    pub document: RenderedDocument,
    /// The frontend's diagnostics followed by the renderer's (`document.diagnostics`).
    pub diagnostics: Diagnostics,
}

/// A failed [`try_parse_only`], [`try_prepare`] or [`try_build_deck`]: the error and the
/// diagnostics collected before it, so that a failed build still reports its warnings.
#[derive(Debug)]
pub struct BuildFailure {
    /// What stopped the build.
    pub error: Error,
    /// Every diagnostic collected before `error` (the frontend's; when `error` is a missing
    /// asset found while rendering, the renderer's too). When `error` is an escalated
    /// missing asset, the other `missing-asset` diagnostics are here with level error (each
    /// is an error in that build) and the one that became `error` is left out. When `error`
    /// is rejected front matter, which [`try_prepare`] meets before the escalation, every
    /// `missing-asset` diagnostic is here with level error if `missing_asset_is_error`.
    pub diagnostics: Diagnostics,
}

impl From<Error> for BuildFailure {
    /// A failure with no diagnostics (the input could not be read or parsed).
    fn from(error: Error) -> BuildFailure {
        BuildFailure {
            error,
            diagnostics: Diagnostics::new(),
        }
    }
}

/// Parses, resolves and renders `opts.input` (module docs, stages 1–4). Nothing is written.
/// [`try_build_deck`] does the same but keeps the diagnostics of a failed build.
///
/// Errors: the input cannot be read ([`Error::Io`] naming the file) or is not UTF-8
/// ([`Error::Parse`]), an unknown extension ([`Error::Other`]), `presenter.toml`
/// ([`Error::Config`]), the frontend's errors, a missing asset (with
/// `missing_asset_is_error`), the theme ([`Error::Theme`]) and the renderer's errors.
pub fn build_deck(opts: &BuildOptions) -> Result<Build> {
    try_build_deck(opts).map_err(|failure| failure.error)
}

/// [`build_deck`], keeping the diagnostics collected before a failure ([`BuildFailure`]).
pub fn try_build_deck(opts: &BuildOptions) -> std::result::Result<Build, BuildFailure> {
    let prepared = try_prepare(opts)?;
    let document =
        match render::render(prepared.render_input(opts.output.clone(), opts.build.clone())) {
            Ok(document) => document,
            Err(error) => {
                return Err(BuildFailure {
                    error,
                    diagnostics: prepared.diagnostics,
                });
            }
        };
    let Prepared {
        deck,
        assets,
        theme,
        config,
        config_path,
        mut diagnostics,
    } = prepared;
    diagnostics.extend(document.diagnostics.iter().cloned());
    // The frontend's missing assets were escalated by `try_prepare`; these are the
    // renderer's (images in `markdown` filter input).
    let diagnostics = escalate_missing_assets(diagnostics, opts.missing_asset_is_error)?;
    tracing::debug!(
        slides = document.slide_count,
        bytes = document.html.len(),
        "rendered {}",
        deck.source.path.display()
    );
    Ok(Build {
        deck,
        assets,
        theme,
        config,
        config_path,
        document,
        diagnostics,
    })
}

/// Stages 1–3 of the module docs: everything [`build_deck`] does before rendering. Used by
/// `dump --context` (the template context without the document) and by [`build_deck`].
pub fn prepare(opts: &BuildOptions) -> Result<Prepared> {
    try_prepare(opts).map_err(|failure| failure.error)
}

/// [`prepare`], keeping the diagnostics collected before a failure ([`BuildFailure`]).
pub fn try_prepare(opts: &BuildOptions) -> std::result::Result<Prepared, BuildFailure> {
    let (parsed, config, config_path) = try_parse_only(opts)
        .map_err(|failure| raise_missing_assets(failure, opts.missing_asset_is_error))?;
    let Parsed {
        deck,
        assets,
        diagnostics,
    } = parsed;
    let diagnostics = escalate_missing_assets(diagnostics, opts.missing_asset_is_error)?;
    match &config_path {
        Some(path) => tracing::info!("config: {}", path.display()),
        None => tracing::info!("config: no presenter.toml found; using the defaults"),
    }
    match theme_and_config(opts, &deck, &config, config_path.as_deref()) {
        Ok((theme, config)) => Ok(Prepared {
            deck,
            assets,
            theme,
            config,
            config_path,
            diagnostics,
        }),
        Err(error) => Err(BuildFailure { error, diagnostics }),
    }
}

/// The theme of the deck and the effective config (stage 3 of the module docs).
fn theme_and_config(
    opts: &BuildOptions,
    deck: &Deck,
    config: &Config,
    config_path: Option<&Path>,
) -> Result<(Theme, Config)> {
    let theme = resolve_theme(opts, deck, config, config_path)?;
    tracing::info!(
        "theme '{}' (chain: {})",
        theme.name(),
        theme
            .chain
            .iter()
            .map(|dir| dir.name.as_str())
            .collect::<Vec<_>>()
            .join(" → ")
    );
    check_theme_plugins(&theme)?;
    let mut config = render::context::effective_config(config, &deck.meta, &theme)?;
    config.apply_cli(&opts.cli);
    config.validate_plugins()?;
    Ok((theme, config))
}

/// Stages 1–2 of the module docs: the parsed deck, the config and the `presenter.toml`
/// that was used (absolute). The config is the project config with the deck's front
/// matter merged ([`Config::merge_front_matter`]) and the command line applied; the theme's
/// `[reveal]` defaults are not in it (the theme is not resolved here; see [`prepare`]).
/// Missing assets are never errors here: they are `missing-asset` diagnostics in
/// [`Parsed::diagnostics`]. For `dump` and `check`; [`try_parse_only`] keeps the frontend's
/// diagnostics when the front matter is rejected.
pub fn parse_only(opts: &BuildOptions) -> Result<(Parsed, Config, Option<PathBuf>)> {
    try_parse_only(opts).map_err(|failure| failure.error)
}

/// [`parse_only`], keeping the frontend's diagnostics when the config rejects the deck's
/// front matter ([`Config::merge_front_matter`] fails): the failure's diagnostics are the
/// parsed deck's, so the deck's warnings are reported with the error. The frontends merge
/// only the front matter tables that change how they parse (Markdown: `slides`,
/// `markdown`; notebooks: `markdown`, `notebook`) before they parse, and leave the rest
/// (`reveal`, `math`, …) to this merge, so only a rejected table that changes the parse
/// fails the frontend itself, before it has diagnostics. A notebook's error still names
/// the metadata layer it came from (`metadata.presenter: …`, `cell N: …`). Every other
/// error (reading the input, `presenter.toml`, the frontend's errors) comes without
/// diagnostics.
pub fn try_parse_only(
    opts: &BuildOptions,
) -> std::result::Result<(Parsed, Config, Option<PathBuf>), BuildFailure> {
    let kind = match opts.format {
        Some(kind) => kind,
        None => detect_kind(&opts.input)?,
    };
    let text = read_source(&opts.input)?;
    let source = source_info(&opts.input, kind)?;
    let (mut config, config_path) = load_config(opts)?;
    let (parsed, deferred) = match kind {
        SourceKind::Markdown => (
            crate::markdown::parse_deferring_config(&source, &text, &config)?,
            None,
        ),
        SourceKind::Notebook => crate::notebook::parse_deferring_config(&source, &text, &config)?,
    };
    tracing::debug!(
        diagnostics = parsed.diagnostics.len(),
        "parsed {} ({kind:?})",
        source.path.display()
    );
    let merged = match deferred {
        Some(error) => Err(error),
        None => config.merge_front_matter(&parsed.deck.meta),
    };
    if let Err(error) = merged {
        return Err(BuildFailure {
            error,
            diagnostics: parsed.diagnostics,
        });
    }
    config.apply_cli(&opts.cli);
    Ok((parsed, config, config_path))
}

/// Stage 1 of the module docs: the project config (`opts.config_path`, else the first
/// `presenter.toml` found walking up from the input's directory, else the defaults) with
/// `opts.cli` applied, and the absolute path of the file that was used.
///
/// When a file was used, a relative `build.out_dir` (as written, or the default `dist`)
/// is replaced by the absolute path it names relative to that file's directory (PLAN.md
/// §6, config.md §2: paths in `presenter.toml` are relative to it). Without a file it stays
/// relative and [`default_output_path`] puts it next to the input.
///
/// Errors: [`Error::Config`] for an unreadable or invalid file.
pub fn load_config(opts: &BuildOptions) -> Result<(Config, Option<PathBuf>)> {
    let (mut config, path) = match &opts.config_path {
        Some(path) => {
            let path = absolute(path)?;
            (Config::load(&path)?, Some(path))
        }
        None => {
            let input = absolute(&opts.input)?;
            let dir = input.parent().unwrap_or(Path::new("/"));
            match Config::discover(dir)? {
                Some((path, config)) => (config, Some(path)),
                None => (Config::default(), None),
            }
        }
    };
    if let Some(dir) = path.as_deref().and_then(Path::parent)
        && config.build.out_dir.is_relative()
    {
        config.build.out_dir = normalize_lexically(&dir.join(&config.build.out_dir));
    }
    config.apply_cli(&opts.cli);
    Ok((config, path))
}

/// The frontend for `path`, by extension (ASCII case-insensitive): `.md` and `.markdown`
/// are Markdown, `.ipynb` is a notebook. Anything else is an [`Error::Other`] naming the
/// file (commands with a `--format` flag add that hint themselves).
pub fn detect_kind(path: &Path) -> Result<SourceKind> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("md" | "markdown") => Ok(SourceKind::Markdown),
        Some("ipynb") => Ok(SourceKind::Notebook),
        _ => Err(Error::Other(format!(
            "{}: cannot tell the input format from the file name (expected a .md, .markdown \
             or .ipynb file)",
            path.display()
        ))),
    }
}

/// Where `presenter build` writes when `-o` is not given (cli.md `build`):
/// `<out_dir>/<input stem>.html` when `standalone`, else the directory
/// `<out_dir>/<input stem>` (which receives [`SITE_INDEX`] and [`SITE_ASSETS_DIR`]).
///
/// `out_dir` is `config.build.out_dir`. When `config` comes from [`load_config`] (or
/// [`parse_only`], [`prepare`], [`build_deck`]) and a `presenter.toml` was used, it is
/// already absolute (relative to that file). A relative `out_dir` here is resolved against
/// the input's directory; the result is then relative when `input` is.
pub fn default_output_path(input: &Path, config: &Config, standalone: bool) -> PathBuf {
    let dir = input
        .parent()
        .unwrap_or(Path::new(""))
        .join(&config.build.out_dir);
    let stem = input
        .file_stem()
        .map(|stem| stem.to_os_string())
        .unwrap_or_else(|| "slides".into());
    if standalone {
        let mut name = stem;
        name.push(".html");
        dir.join(name)
    } else {
        dir.join(stem)
    }
}

/// The [`OutputMode`] for writing to `out`: [`OutputMode::Standalone`], or site mode with
/// the emitted files in `<out>/assets` referenced as `assets/<name>` from
/// `<out>/index.html`.
pub fn output_mode(standalone: bool, out: &Path) -> OutputMode {
    if standalone {
        OutputMode::Standalone
    } else {
        OutputMode::Site {
            assets_dir: out.join(SITE_ASSETS_DIR),
            url_prefix: format!("{SITE_ASSETS_DIR}/"),
        }
    }
}

/// Writes `build` to `out` and returns the paths written, the document first.
///
/// - [`OutputMode::Standalone`]: `out` is the HTML file.
/// - [`OutputMode::Site`]: `out` is a directory; the document is `<out>/index.html` and
///   every file of `build.document.files` is written at its path (the `assets_dir` the
///   build was rendered with, `<out>/assets` when it came from [`output_mode`]).
/// - [`OutputMode::Serve`]: an error; serve output is never written to disk.
///
/// Parent directories are created. Existing files are overwritten; stale files of an
/// earlier site build are left alone. Errors are [`Error::Io`] naming the path.
pub fn write_output(build: &Build, out: &Path, mode: &OutputMode) -> Result<Vec<PathBuf>> {
    match mode {
        OutputMode::Standalone => {
            write_file(out, build.document.html.as_bytes())?;
            Ok(vec![out.to_path_buf()])
        }
        OutputMode::Site { .. } => {
            let index = out.join(SITE_INDEX);
            write_file(&index, build.document.html.as_bytes())?;
            let mut written = vec![index];
            for file in &build.document.files {
                write_file(&file.path, &file.bytes)?;
                written.push(file.path.clone());
            }
            Ok(written)
        }
        OutputMode::Serve => Err(Error::Other(
            "serve output is held in memory and cannot be written to disk".into(),
        )),
    }
}

/// Writes `bytes` to `path`, creating its parent directories.
fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| io_error(e, "cannot create", parent))?;
    }
    std::fs::write(path, bytes).map_err(|e| io_error(e, "cannot write", path))
}

/// The input's text. Errors name the file as given.
fn read_source(input: &Path) -> Result<String> {
    let bytes = std::fs::read(input).map_err(|e| io_error(e, "cannot read", input))?;
    String::from_utf8(bytes).map_err(|e| Error::Parse {
        path: input.to_path_buf(),
        line: None,
        message: format!("not valid UTF-8: {e}"),
    })
}

/// [`SourceInfo`] for `input`: absolute, lexically normalized path; `base_dir` its parent.
fn source_info(input: &Path, kind: SourceKind) -> Result<SourceInfo> {
    let path = absolute(input)?;
    let base_dir = path.parent().unwrap_or(Path::new("/")).to_path_buf();
    Ok(SourceInfo {
        path,
        kind,
        base_dir,
    })
}

/// `path` made absolute against the current directory and lexically normalized.
fn absolute(path: &Path) -> Result<PathBuf> {
    let abs = std::path::absolute(path).map_err(|e| io_error(e, "cannot resolve", path))?;
    Ok(normalize_lexically(&abs))
}

/// An [`Error::Io`] whose message names `path`.
fn io_error(err: std::io::Error, what: &str, path: &Path) -> Error {
    Error::Io(std::io::Error::new(
        err.kind(),
        format!("{what} {}: {err}", path.display()),
    ))
}

/// With `missing_asset_is_error`, the first `missing-asset` diagnostic becomes a
/// [`BuildFailure`] whose error is [`Error::MissingAsset`] and whose diagnostics are the
/// others, with every other `missing-asset` raised to level error. Otherwise (or without
/// one) `diagnostics` is returned unchanged.
fn escalate_missing_assets(
    diagnostics: Diagnostics,
    missing_asset_is_error: bool,
) -> std::result::Result<Diagnostics, BuildFailure> {
    if !missing_asset_is_error {
        return Ok(diagnostics);
    }
    let Some(error) = missing_asset_error(&diagnostics) else {
        return Ok(diagnostics);
    };
    let mut first = true;
    let rest = diagnostics
        .into_iter()
        .filter_map(|mut diagnostic| {
            if diagnostic.code == MISSING_ASSET {
                if std::mem::take(&mut first) {
                    return None;
                }
                diagnostic.level = Level::Error;
            }
            Some(diagnostic)
        })
        .collect();
    Err(BuildFailure {
        error,
        diagnostics: rest,
    })
}

/// `failure` (from before the `missing-asset` escalation, e.g. rejected front matter) with
/// every `missing-asset` diagnostic raised to level error when `missing_asset_is_error`:
/// each is an error in that build, as in an escalated failure.
fn raise_missing_assets(failure: BuildFailure, missing_asset_is_error: bool) -> BuildFailure {
    if !missing_asset_is_error {
        return failure;
    }
    let diagnostics = failure
        .diagnostics
        .into_iter()
        .map(|mut diagnostic| {
            if diagnostic.code == MISSING_ASSET {
                diagnostic.level = Level::Error;
            }
            diagnostic
        })
        .collect();
    BuildFailure {
        error: failure.error,
        diagnostics,
    }
}

/// The first `missing-asset` diagnostic as [`Error::MissingAsset`] (the missing file from
/// its message, the span from the diagnostic), if there is one.
fn missing_asset_error(diagnostics: &Diagnostics) -> Option<Error> {
    let diagnostic = diagnostics.iter().find(|d| d.code == MISSING_ASSET)?;
    let path = missing_asset_path(diagnostic)
        .or_else(|| diagnostic.path.clone())
        .unwrap_or_default();
    Some(Error::MissingAsset {
        path,
        span: diagnostic.span.clone(),
    })
}

/// Strict check of the theme chain's merged `[reveal] plugins` (themes are not validated
/// when they are loaded). A bad list is an [`Error::Theme`] naming the theme of the chain
/// whose `theme.toml` sets the winning `plugins` value, so the message points at the
/// theme rather than at `presenter.toml`.
fn check_theme_plugins(theme: &Theme) -> Result<()> {
    let probe = Config {
        reveal: theme.reveal_defaults(),
        ..Config::default()
    };
    probe.validate_plugins().map_err(|err| {
        let message = match err {
            Error::Config(message) => message,
            other => other.to_string(),
        };
        let name = theme
            .chain
            .iter()
            .find(|dir| dir.manifest.reveal.contains_key("plugins"))
            .map_or(theme.name(), |dir| dir.name.as_str());
        Error::Theme {
            name: name.to_string(),
            message: format!("theme.toml [reveal]: {message}"),
        }
    })
}

/// Resolves the deck's theme the way [`prepare`] does: `opts.cli.theme`, else the front
/// matter's `theme`, else `[deck] theme` (which [`Config::apply_cli`] already replaced by
/// `cli.theme` when given), searched in the config file's directory, the input's directory
/// and the current directory ([`theme::resolve()`]).
///
/// `deck`, `config` and `config_path` are what [`parse_only`] returns, so that `check`
/// (T34) and `theme list` (T41) find the same theme as `build` without repeating the
/// precedence or the search roots. The theme's `[reveal]` values are not validated here.
///
/// Errors: [`Error::Theme`] when the theme or a parent cannot be found or loaded.
pub fn resolve_theme(
    opts: &BuildOptions,
    deck: &Deck,
    config: &Config,
    config_path: Option<&Path>,
) -> Result<Theme> {
    let spec = opts
        .cli
        .theme
        .as_deref()
        .or(deck.meta.theme.as_deref())
        .unwrap_or(&config.deck.theme);
    let cwd = std::env::current_dir().ok();
    let mut roots: Vec<&Path> = Vec::with_capacity(3);
    if let Some(dir) = config_path.and_then(Path::parent) {
        roots.push(dir);
    }
    roots.push(&deck.source.base_dir);
    if let Some(cwd) = &cwd {
        roots.push(cwd);
    }
    theme::resolve(spec, &roots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Diagnostic;
    use crate::ir::SourceSpan;

    /// A temporary directory with `files` (path, contents) written into it.
    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        for (path, contents) in files {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        dir
    }

    fn fixture_theme() -> String {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/themes/child-theme")
            .display()
            .to_string()
    }

    #[test]
    fn detect_kind_by_extension() {
        assert_eq!(
            detect_kind(Path::new("a/slides.md")).unwrap(),
            SourceKind::Markdown
        );
        assert_eq!(
            detect_kind(Path::new("x.MARKDOWN")).unwrap(),
            SourceKind::Markdown
        );
        assert_eq!(
            detect_kind(Path::new("talk.ipynb")).unwrap(),
            SourceKind::Notebook
        );
        for bad in ["notes.txt", "README", "slides.md.bak"] {
            let err = detect_kind(Path::new(bad)).unwrap_err().to_string();
            assert!(err.contains(bad), "{err}");
            assert!(err.contains(".ipynb"), "{err}");
            // Only `build` has --format; it adds the hint itself (cli::input_format).
            assert!(!err.contains("--format"), "{err}");
        }
    }

    #[test]
    fn out_dir_is_relative_to_the_presenter_toml_that_sets_it() {
        let dir = project(&[
            ("proj/presenter.toml", "[build]\nout_dir = \"out\"\n"),
            ("proj/talk/slides.md", "# One\n"),
            ("plain/presenter.toml", "[deck]\nlang = \"de\"\n"),
            ("plain/talk/slides.md", "# One\n"),
            ("cfg/other.toml", "[build]\nout_dir = \"../built\"\n"),
            ("cfg/abs.toml", "[build]\nout_dir = \"/abs/out\"\n"),
        ]);
        let root = normalize_lexically(&std::path::absolute(dir.path()).unwrap());
        let out_of = |input: &str, config: Option<&str>| {
            let opts = BuildOptions {
                config_path: config.map(|c| dir.path().join(c)),
                ..BuildOptions::new(dir.path().join(input))
            };
            let (config, _) = load_config(&opts).unwrap();
            default_output_path(&opts.input, &config, true)
        };
        // An ancestor's out_dir, as written and as the default.
        assert_eq!(
            out_of("proj/talk/slides.md", None),
            root.join("proj/out/slides.html")
        );
        assert_eq!(
            out_of("plain/talk/slides.md", None),
            root.join("plain/dist/slides.html")
        );
        // --config: relative to that file, wherever the deck is.
        assert_eq!(
            out_of("proj/talk/slides.md", Some("cfg/other.toml")),
            root.join("built/slides.html")
        );
        assert_eq!(
            out_of("proj/talk/slides.md", Some("cfg/abs.toml")),
            PathBuf::from("/abs/out/slides.html")
        );
        // The built deck's config carries the same resolved directory.
        let build = build_deck(&BuildOptions::new(dir.path().join("proj/talk/slides.md")));
        assert_eq!(build.unwrap().config.build.out_dir, root.join("proj/out"));
    }

    #[test]
    fn default_output_paths_follow_out_dir() {
        let mut config = Config::default();
        let input = Path::new("talk/slides.md");
        assert_eq!(
            default_output_path(input, &config, true),
            PathBuf::from("talk/dist/slides.html")
        );
        assert_eq!(
            default_output_path(input, &config, false),
            PathBuf::from("talk/dist/slides")
        );
        assert_eq!(
            default_output_path(Path::new("slides.md"), &config, true),
            PathBuf::from("dist/slides.html")
        );
        config.build.out_dir = PathBuf::from("/abs/out");
        assert_eq!(
            default_output_path(input, &config, true),
            PathBuf::from("/abs/out/slides.html")
        );
    }

    #[test]
    fn output_mode_for_site_uses_the_assets_dir() {
        assert_eq!(
            output_mode(true, Path::new("x.html")),
            OutputMode::Standalone
        );
        assert_eq!(
            output_mode(false, Path::new("out")),
            OutputMode::Site {
                assets_dir: PathBuf::from("out/assets"),
                url_prefix: "assets/".into()
            }
        );
    }

    #[test]
    fn builds_a_deck_with_discovered_config() {
        let dir = project(&[
            ("presenter.toml", "[reveal]\ntransition = \"none\"\n"),
            (
                "deck/slides.md",
                "# One\n\n---\n\n## Two\n\n--\n\n## Three\n",
            ),
        ]);
        let build = build_deck(&BuildOptions::new(dir.path().join("deck/slides.md"))).unwrap();
        assert_eq!(build.deck.slides.len(), 2);
        assert_eq!(build.document.slide_count, 3);
        assert!(build.diagnostics.is_empty(), "{:?}", build.diagnostics);
        let config_path = build.config_path.as_deref().unwrap();
        assert!(config_path.ends_with("presenter.toml"), "{config_path:?}");
        assert!(config_path.is_absolute());
        assert_eq!(build.config.reveal["transition"], "none");
        assert!(build.document.html.contains(r#""transition":"none""#));
        assert!(build.deck.source.path.is_absolute());
        assert_eq!(build.theme.name(), "default");
    }

    #[test]
    fn explicit_config_path_bypasses_discovery() {
        let dir = project(&[
            ("presenter.toml", "[reveal]\ntransition = \"none\"\n"),
            ("other.toml", "[reveal]\ntransition = \"convex\"\n"),
            ("slides.md", "# One\n"),
        ]);
        let mut opts = BuildOptions::new(dir.path().join("slides.md"));
        opts.config_path = Some(dir.path().join("other.toml"));
        let build = build_deck(&opts).unwrap();
        assert_eq!(build.config.reveal["transition"], "convex");

        opts.config_path = Some(dir.path().join("missing.toml"));
        let err = build_deck(&opts).unwrap_err();
        assert!(matches!(err, Error::Config(_)), "{err:?}");
    }

    #[test]
    fn theme_precedence_is_cli_then_front_matter_then_config() {
        let child = fixture_theme();
        let dir = project(&[
            ("presenter.toml", &format!("[deck]\ntheme = {child:?}\n")),
            ("plain.md", "# Plain\n"),
            ("front.md", "---\ntheme: default\n---\n# Front\n"),
        ]);
        let theme_of = |file: &str, cli: Option<&str>| {
            let mut opts = BuildOptions::new(dir.path().join(file));
            opts.cli.theme = cli.map(str::to_string);
            let name = prepare(&opts).unwrap().theme.name().to_string();
            // resolve_theme on parse_only's results (for check/theme list) agrees.
            let (parsed, config, config_path) = parse_only(&opts).unwrap();
            let theme = resolve_theme(&opts, &parsed.deck, &config, config_path.as_deref());
            assert_eq!(theme.unwrap().name(), name);
            name
        };
        assert_eq!(theme_of("plain.md", None), "child-theme");
        assert_eq!(theme_of("front.md", None), "default");
        assert_eq!(theme_of("front.md", Some(&child)), "child-theme");
        assert_eq!(theme_of("plain.md", Some("default")), "default");
    }

    #[test]
    fn theme_reveal_defaults_sit_under_the_project_config_and_front_matter_over_it() {
        let child = fixture_theme();
        let dir = project(&[
            ("presenter.toml", "[reveal]\nslideNumber = false\n"),
            (
                "slides.md",
                &format!("---\ntheme: {child}\nreveal:\n  controls: false\n---\n# One\n"),
            ),
        ]);
        let prepared = prepare(&BuildOptions::new(dir.path().join("slides.md"))).unwrap();
        let reveal = &prepared.config.reveal;
        assert_eq!(reveal["transition"], "fade", "from the theme");
        assert_eq!(
            reveal["slideNumber"], false,
            "presenter.toml over the theme"
        );
        assert_eq!(reveal["controls"], false, "front matter");
    }

    #[test]
    fn missing_assets_are_errors_only_when_asked() {
        let dir = project(&[("slides.md", "# One\n\n![x](figures/none.png)\n")]);
        let mut opts = BuildOptions::new(dir.path().join("slides.md"));
        let err = build_deck(&opts).unwrap_err();
        let Error::MissingAsset { path, span } = &err else {
            panic!("expected MissingAsset, got {err:?}");
        };
        assert!(path.ends_with("figures/none.png"), "{path:?}");
        assert!(path.is_absolute(), "{path:?}");
        assert_eq!(span.as_ref().map(|s| s.start_line), Some(3));

        opts.missing_asset_is_error = false;
        let build = build_deck(&opts).unwrap();
        let codes: Vec<&str> = build.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(codes, ["missing-asset"]);
    }

    #[test]
    fn missing_asset_error_falls_back_to_the_diagnostic_path() {
        let mut diagnostics = Diagnostics::new();
        diagnostics.push(Diagnostic::warning("empty-slide", "x"));
        assert!(missing_asset_error(&diagnostics).is_none());
        diagnostics.push(
            Diagnostic::warning("missing-asset", "no message format")
                .with_path("deck.md")
                .with_span(SourceSpan {
                    start_line: 4,
                    end_line: 4,
                    cell: None,
                }),
        );
        match missing_asset_error(&diagnostics) {
            Some(Error::MissingAsset { path, span }) => {
                assert_eq!(path, PathBuf::from("deck.md"));
                assert_eq!(span.unwrap().start_line, 4);
            }
            other => panic!("expected MissingAsset, got {other:?}"),
        }
    }

    #[test]
    fn failed_builds_keep_the_diagnostics_found_before_the_error() {
        let dir = project(&[(
            "two.md",
            "# A\n\n![a](a1.png)\n\n---\n\n---\n\n![b](b2.png)\n",
        )]);
        let opts = BuildOptions::new(dir.path().join("two.md"));
        let failure = try_build_deck(&opts).unwrap_err();
        let Error::MissingAsset { path, span } = &failure.error else {
            panic!("expected MissingAsset, got {:?}", failure.error);
        };
        assert!(path.ends_with("a1.png"), "{path:?}");
        assert_eq!(span.as_ref().map(|s| s.start_line), Some(3));
        let found: Vec<(&str, Level)> = failure
            .diagnostics
            .iter()
            .map(|d| (d.code, d.level))
            .collect();
        assert_eq!(
            found,
            [
                ("empty-slide", Level::Warning),
                ("missing-asset", Level::Error)
            ],
            "the escalated a1.png is not repeated; b2.png is an error too"
        );
        assert!(
            failure
                .diagnostics
                .iter()
                .any(|d| d.message.contains("b2.png"))
        );

        // A later failure (the theme) keeps the frontend's warnings as they are.
        let opts = BuildOptions {
            cli: CliOverrides {
                theme: Some("nope".into()),
                ..CliOverrides::default()
            },
            missing_asset_is_error: false,
            ..BuildOptions::new(dir.path().join("two.md"))
        };
        let failure = try_build_deck(&opts).unwrap_err();
        assert!(matches!(failure.error, Error::Theme { .. }), "{failure:?}");
        let codes: Vec<&str> = failure.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(codes, ["missing-asset", "empty-slide", "missing-asset"]);
        assert!(
            failure
                .diagnostics
                .iter()
                .all(|d| d.level == Level::Warning)
        );
        // build_deck and prepare report only the error.
        assert!(matches!(build_deck(&opts), Err(Error::Theme { .. })));
        assert!(matches!(prepare(&opts), Err(Error::Theme { .. })));
    }

    /// `(code, level, start line)` of a failure's diagnostics.
    fn found(failure: &BuildFailure) -> Vec<(&'static str, Level, usize)> {
        let line = |d: &Diagnostic| d.span.as_ref().map_or(0, |s| s.start_line);
        failure
            .diagnostics
            .iter()
            .map(|d| (d.code, d.level, line(d)))
            .collect()
    }

    #[test]
    fn rejected_front_matter_keeps_the_diagnostics_found_before_the_error() {
        let dir = project(&[
            (
                "fp.md",
                "---\nreveal:\n  plugins: [bogus]\n---\n# A\n\n![a](a1.png)\n\n---\n\n---\n\n# C\n",
            ),
            (
                "table.md",
                "---\nslides:\n  splt: rules\n---\n# A\n\n---\n\n---\n",
            ),
        ]);
        let opts = BuildOptions::new(dir.path().join("fp.md"));
        let rejected = |failure: &BuildFailure| {
            matches!(&failure.error, Error::FrontMatter(message)
                if message.contains("unknown reveal plugin 'bogus'"))
        };

        // The frontend's warnings come with the error; `build` (missing assets are errors)
        // raises the missing asset to an error, as in an escalated failure.
        let failure = try_build_deck(&opts).unwrap_err();
        assert!(rejected(&failure), "{failure:?}");
        assert_eq!(
            found(&failure),
            [
                ("missing-asset", Level::Error, 7),
                ("empty-slide", Level::Warning, 9)
            ]
        );
        assert_eq!(found(&try_prepare(&opts).unwrap_err()), found(&failure));
        let failure = try_parse_only(&opts).unwrap_err();
        assert!(rejected(&failure), "{failure:?}");
        assert_eq!(
            found(&failure),
            [
                ("missing-asset", Level::Warning, 7),
                ("empty-slide", Level::Warning, 9)
            ],
            "parse_only never treats missing assets as errors"
        );
        let lenient = BuildOptions {
            missing_asset_is_error: false,
            ..opts.clone()
        };
        assert_eq!(
            found(&try_build_deck(&lenient).unwrap_err()),
            found(&failure)
        );
        // The plain functions report only the error.
        assert!(matches!(parse_only(&opts), Err(Error::FrontMatter(_))));
        assert!(matches!(prepare(&opts), Err(Error::FrontMatter(_))));
        assert!(matches!(build_deck(&opts), Err(Error::FrontMatter(_))));

        // A rejected config table changes the parse, so the frontend fails before it has
        // any diagnostics (its warnings would describe a deck split another way).
        let failure = try_build_deck(&BuildOptions::new(dir.path().join("table.md")));
        let failure = failure.unwrap_err();
        assert!(
            matches!(&failure.error, Error::FrontMatter(message) if message.contains("splt")),
            "{failure:?}"
        );
        assert!(failure.diagnostics.is_empty(), "{failure:?}");
    }

    #[test]
    fn rejected_tables_the_frontend_does_not_read_keep_the_diagnostics() {
        // A notebook whose first markdown cell is `first` and whose raw LaTeX cell warns.
        let notebook = |metadata: serde_json::Value, first: &str| {
            serde_json::json!({
                "nbformat": 4, "nbformat_minor": 5, "metadata": metadata,
                "cells": [
                    {"cell_type": "markdown", "metadata": {}, "source": first},
                    {"cell_type": "raw", "metadata": {"raw_mimetype": "text/latex"}, "source": "x"},
                ],
            })
            .to_string()
        };
        let presenter_plugins = notebook(
            serde_json::json!({"presenter": {"reveal": {"plugins": ["bogus"]}}}),
            "# A",
        );
        let front_tables = notebook(
            serde_json::json!({}),
            "---\nslides:\n  splt: rules\nmath:\n  enable: never\n---\n# A",
        );
        let presenter_table = notebook(
            serde_json::json!({"presenter": {"notebook": {"hide_inputs": []}}}),
            "# A",
        );
        let dir = project(&[
            (
                "math.md",
                "---\nmath:\n  enable: never\n---\n# A\n\n---\n\n---\n",
            ),
            (
                "nb.md",
                "---\nnotebook: {hide_inputs: []}\n---\n# A\n\n---\n\n---\n",
            ),
            ("plugins.ipynb", &presenter_plugins),
            ("tables.ipynb", &front_tables),
            ("table.ipynb", &presenter_table),
        ]);
        let raw_skipped = ("raw-cell-skipped", Level::Warning, 1);
        let empty = |line| ("empty-slide", Level::Warning, line);
        let cases = [
            // Markdown reads only the `slides` and `markdown` tables.
            (
                "math.md",
                "math: unknown field `enable`",
                vec![empty(7), empty(9)],
            ),
            (
                "nb.md",
                "notebook: unknown field `hide_inputs`",
                vec![empty(6), empty(8)],
            ),
            // Notebooks read only `markdown` and `notebook`; the error still names the layer.
            (
                "plugins.ipynb",
                "metadata.presenter: unknown reveal plugin 'bogus'",
                vec![raw_skipped],
            ),
            (
                "tables.ipynb",
                "cell 0: slides: unknown field `splt`",
                vec![raw_skipped],
            ),
            // A table the conversion reads fails it, before there are diagnostics.
            (
                "table.ipynb",
                "metadata.presenter: notebook: unknown field `hide_inputs`",
                vec![],
            ),
        ];
        for (name, error, diagnostics) in cases {
            let path = dir.path().join(name);
            let failure = try_build_deck(&BuildOptions::new(path.clone())).unwrap_err();
            assert!(
                matches!(&failure.error, Error::FrontMatter(message) if message.starts_with(error)),
                "{name}: {failure:?}"
            );
            assert_eq!(found(&failure), diagnostics, "{name}");
            // The public frontends still reject the front matter themselves (ir.md §4).
            let kind = detect_kind(&path).unwrap();
            let (source, text) = (
                source_info(&path, kind).unwrap(),
                read_source(&path).unwrap(),
            );
            let config = Config::default();
            let strict = match kind {
                SourceKind::Markdown => crate::markdown::parse(&source, &text, &config).err(),
                SourceKind::Notebook => crate::notebook::parse(&source, &text, &config).err(),
            };
            assert!(
                matches!(&strict, Some(Error::FrontMatter(message)) if message.starts_with(error)),
                "{name}: {strict:?}"
            );
        }
    }

    /// A project whose child theme `t` (next to the deck, so `theme: t` finds it without a
    /// `presenter.toml`) renders `meta.subtitle|markdown` twice on the title slide, and a
    /// deck whose subtitle is the image `subtitle_image`.
    fn filter_project(subtitle_image: &str) -> tempfile::TempDir {
        let layout = "{%- extends \"layouts/default.html\" %}{%- block content %}\
                      <p class=\"subtitle\">{{ meta.subtitle|markdown }}</p>\
                      <p>{{ meta.subtitle|markdown }}</p>{%- endblock %}";
        let deck = format!(
            "---\ntheme: t\nsubtitle: \"![logo]({subtitle_image})\"\n---\n# Title {{layout=title}}\n"
        );
        project(&[
            ("deck/t/theme.toml", "name = \"t\"\nextends = \"default\"\n"),
            ("deck/t/templates/layouts/title.html", layout),
            ("deck/slides.md", &deck),
        ])
    }

    #[test]
    fn markdown_filter_images_are_resolved_and_missing_ones_are_build_errors() {
        let dir = filter_project("logo.png");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/md/images/ok.png"),
            dir.path().join("deck/logo.png"),
        )
        .unwrap();
        let input = dir.path().join("deck/slides.md");
        let build = build_deck(&BuildOptions::new(&input)).unwrap();
        assert!(
            build
                .document
                .html
                .contains(r#"<p class="subtitle"><img src="data:image/png;base64,"#),
            "the filter's image is inlined"
        );
        assert!(build.diagnostics.is_empty(), "{:?}", build.diagnostics);

        let dir = filter_project("nologo.png");
        let input = dir.path().join("deck/slides.md");
        let failure = try_build_deck(&BuildOptions::new(&input)).unwrap_err();
        let Error::MissingAsset { path, span } = &failure.error else {
            panic!("expected MissingAsset, got {:?}", failure.error);
        };
        assert!(path.ends_with("deck/nologo.png"), "{path:?}");
        assert!(span.is_none());
        assert!(failure.diagnostics.is_empty(), "{:?}", failure.diagnostics);

        // Without escalation: one warning (not one per filter call), naming the deck.
        let opts = BuildOptions {
            missing_asset_is_error: false,
            ..BuildOptions::new(&input)
        };
        let build = build_deck(&opts).unwrap();
        let warnings: Vec<_> = build.diagnostics.iter().collect();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].code, "missing-asset");
        assert_eq!(
            warnings[0].path.as_deref(),
            Some(build.deck.source.path.as_path())
        );
        assert!(warnings[0].message.starts_with("markdown filter: "));
    }

    #[test]
    fn invalid_theme_plugins_are_theme_errors_naming_the_theme() {
        let dir = project(&[
            (
                "bt/theme.toml",
                "name = \"bt\"\nextends = \"default\"\n[reveal]\nplugins = [\"highlight\", \"nope\"]\n",
            ),
            ("slides.md", "---\ntheme: bt\n---\n# One\n"),
        ]);
        let err = build_deck(&BuildOptions::new(dir.path().join("slides.md"))).unwrap_err();
        let Error::Theme { name, message } = &err else {
            panic!("expected a theme error, got {err:?}");
        };
        assert_eq!(name, "bt");
        assert!(message.contains("[reveal]"), "{message}");
        assert!(
            message.contains("unknown reveal plugin 'nope'"),
            "{message}"
        );
    }

    #[test]
    fn default_options_are_those_of_new() {
        let options = BuildOptions {
            input: PathBuf::from("x.md"),
            ..BuildOptions::default()
        };
        let new = BuildOptions::new("x.md");
        assert_eq!(format!("{options:?}"), format!("{new:?}"));
    }

    #[test]
    fn unreadable_inputs_name_the_file() {
        let dir = project(&[]);
        let missing = dir.path().join("missing.md");
        let err = build_deck(&BuildOptions::new(&missing)).unwrap_err();
        assert!(matches!(err, Error::Io(_)), "{err:?}");
        assert!(err.to_string().contains("missing.md"), "{err}");

        let latin1 = dir.path().join("latin1.md");
        std::fs::write(&latin1, b"# caf\xe9\n").unwrap();
        let err = build_deck(&BuildOptions::new(&latin1)).unwrap_err();
        assert!(matches!(err, Error::Parse { .. }), "{err:?}");
        assert!(err.to_string().contains("latin1.md"), "{err}");
    }

    #[test]
    fn format_overrides_the_extension() {
        let dir = project(&[("slides.txt", "# One\n")]);
        let mut opts = BuildOptions::new(dir.path().join("slides.txt"));
        assert!(matches!(build_deck(&opts), Err(Error::Other(_))));
        opts.format = Some(SourceKind::Markdown);
        assert_eq!(build_deck(&opts).unwrap().deck.slides.len(), 1);
    }

    #[test]
    fn writes_standalone_and_site_output() {
        let dir = project(&[("slides.md", "# One\n\n![x](plot.png)\n")]);
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/md/images/ok.png"),
            dir.path().join("plot.png"),
        )
        .unwrap();
        let mut opts = BuildOptions::new(dir.path().join("slides.md"));

        let out = dir.path().join("out/one.html");
        let build = build_deck(&opts).unwrap();
        let written = write_output(&build, &out, &OutputMode::Standalone).unwrap();
        assert_eq!(written, std::slice::from_ref(&out));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), build.document.html);

        let site = dir.path().join("site");
        opts.output = output_mode(false, &site);
        opts.build = BuildInfo::build(false);
        let build = build_deck(&opts).unwrap();
        let written = write_output(&build, &site, &opts.output).unwrap();
        assert_eq!(written[0], site.join("index.html"));
        assert!(written.len() > 2, "{written:?}");
        for path in &written[1..] {
            assert!(path.starts_with(site.join("assets")), "{path:?}");
            assert!(path.is_file(), "{path:?}");
        }
        let index = std::fs::read_to_string(&written[0]).unwrap();
        assert!(
            index.contains(r#"src="assets/plot-"#),
            "image is referenced relatively"
        );

        let err = write_output(&build, &site, &OutputMode::Serve).unwrap_err();
        assert!(matches!(err, Error::Other(_)));
    }
}
