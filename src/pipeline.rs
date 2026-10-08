//! Build orchestration: parse → resolve theme and config → render → emit (PLAN.md §3.1).
//!
//! Every command that turns a deck into HTML goes through this module: `build` and `dump`
//! (T16), `serve` (T20/T21), `check` (T34) and `export pdf` (T45). The stages:
//!
//! 1. [`load_config`]: `--config PATH`, else [`Config::discover`] from the input's
//!    directory, else the defaults; the command-line overrides go on top
//!    ([`Config::apply_cli`]).
//! 2. [`parse_only`]: reads the input (UTF-8; errors name the file), picks the frontend by
//!    [`BuildOptions::format`] or the extension ([`detect_kind`]) and parses it into a
//!    [`Parsed`] deck. The returned config also has the deck's front matter merged.
//! 3. [`prepare`]: escalates the first `missing-asset` warning to [`Error::MissingAsset`]
//!    when [`BuildOptions::missing_asset_is_error`] (ir.md §4 contract change), resolves the
//!    theme (`cli.theme` → front matter `theme` → `[deck] theme`, searched in the config
//!    file's directory, the input's directory and the current directory) and builds the
//!    **effective** config: [`render::context::effective_config`] (theme `[reveal]` under
//!    the project config, front matter over it), then the command line again, then a strict
//!    check of `reveal.plugins` ([`Config::validate_plugins`]).
//! 4. [`build_deck`]: renders the prepared deck for [`BuildOptions::output`]
//!    ([`render::render`]) and collects the frontend's and the renderer's diagnostics.
//! 5. [`write_output`]: writes the document (one file, or `index.html` plus the site's
//!    `assets/`); nothing before this stage touches the disk.
//!
//! Output locations: [`default_output_path`] is `<input dir>/<build.out_dir>/<stem>.html`
//! (standalone) or `<input dir>/<build.out_dir>/<stem>/` (site; `out_dir` is relative to
//! the input's directory unless absolute), and [`output_mode`] gives the matching
//! [`OutputMode`] for an output path.

use std::path::{Path, PathBuf};

use crate::assets::emit::OutputMode;
use crate::assets::registry::{AssetRegistry, normalize_lexically};
use crate::config::{CliOverrides, Config};
use crate::error::{Diagnostics, Error, Result};
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

/// Parses, resolves and renders `opts.input` (module docs, stages 1–4). Nothing is written.
///
/// Errors: the input cannot be read ([`Error::Io`] naming the file) or is not UTF-8
/// ([`Error::Parse`]), an unknown extension ([`Error::Other`]), `presenter.toml`
/// ([`Error::Config`]), the frontend's errors, a missing asset (with
/// `missing_asset_is_error`), the theme ([`Error::Theme`]) and the renderer's errors.
pub fn build_deck(opts: &BuildOptions) -> Result<Build> {
    let prepared = prepare(opts)?;
    let document = render::render(prepared.render_input(opts.output.clone(), opts.build.clone()))?;
    let Prepared {
        deck,
        assets,
        theme,
        config,
        config_path,
        mut diagnostics,
    } = prepared;
    diagnostics.extend(document.diagnostics.iter().cloned());
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
    let (parsed, config, config_path) = parse_only(opts)?;
    let Parsed {
        deck,
        assets,
        diagnostics,
    } = parsed;
    if opts.missing_asset_is_error
        && let Some(err) = missing_asset_error(&diagnostics)
    {
        return Err(err);
    }
    match &config_path {
        Some(path) => tracing::info!("config: {}", path.display()),
        None => tracing::info!("config: no presenter.toml found; using the defaults"),
    }
    let theme = resolve_theme(opts, &deck, &config, config_path.as_deref())?;
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
    let mut config = render::context::effective_config(&config, &deck.meta, &theme)?;
    config.apply_cli(&opts.cli);
    config.validate_plugins()?;
    Ok(Prepared {
        deck,
        assets,
        theme,
        config,
        config_path,
        diagnostics,
    })
}

/// Stages 1–2 of the module docs: the parsed deck, the config and the `presenter.toml`
/// that was used (absolute). The config is the project config with the deck's front
/// matter merged ([`Config::merge_front_matter`]) and the command line applied; the theme's
/// `[reveal]` defaults are not in it (the theme is not resolved here; see [`prepare`]).
/// Missing assets are never errors here: they are `missing-asset` diagnostics in
/// [`Parsed::diagnostics`]. For `dump` and `check`.
pub fn parse_only(opts: &BuildOptions) -> Result<(Parsed, Config, Option<PathBuf>)> {
    let kind = match opts.format {
        Some(kind) => kind,
        None => detect_kind(&opts.input)?,
    };
    let text = read_source(&opts.input)?;
    let source = source_info(&opts.input, kind)?;
    let (mut config, config_path) = load_config(opts)?;
    let parsed = match kind {
        SourceKind::Markdown => crate::markdown::parse(&source, &text, &config)?,
        SourceKind::Notebook => crate::notebook::parse(&source, &text, &config)?,
    };
    config.merge_front_matter(&parsed.deck.meta)?;
    config.apply_cli(&opts.cli);
    tracing::debug!(
        diagnostics = parsed.diagnostics.len(),
        "parsed {} ({kind:?})",
        source.path.display()
    );
    Ok((parsed, config, config_path))
}

/// Stage 1 of the module docs: the project config (`opts.config_path`, else the first
/// `presenter.toml` found walking up from the input's directory, else the defaults) with
/// `opts.cli` applied, and the absolute path of the file that was used.
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
    config.apply_cli(&opts.cli);
    Ok((config, path))
}

/// The frontend for `path`, by extension (ASCII case-insensitive): `.md` and `.markdown`
/// are Markdown, `.ipynb` is a notebook. Anything else is an [`Error::Other`] that suggests
/// `--format`.
pub fn detect_kind(path: &Path) -> Result<SourceKind> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("md" | "markdown") => Ok(SourceKind::Markdown),
        Some("ipynb") => Ok(SourceKind::Notebook),
        _ => Err(Error::Other(format!(
            "{}: cannot tell the input format from the file name (expected a .md or .ipynb \
             file; use --format md|ipynb)",
            path.display()
        ))),
    }
}

/// Where `presenter build` writes when `-o` is not given (cli.md `build`):
/// `<input dir>/<config.build.out_dir>/<input stem>.html` when `standalone`, else the
/// directory `<input dir>/<config.build.out_dir>/<input stem>` (which receives
/// [`SITE_INDEX`] and [`SITE_ASSETS_DIR`]). An absolute `out_dir` is used as is. The result
/// is relative when `input` is.
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

/// Resolves the theme: `cli.theme`, else the front matter's `theme`, else `[deck] theme`
/// (which [`Config::apply_cli`] already replaced by `cli.theme` when given), searched in
/// the config file's directory, the input's directory and the current directory.
fn resolve_theme(
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
            let err = detect_kind(Path::new(bad)).unwrap_err();
            assert!(err.to_string().contains("--format"), "{err}");
            assert!(err.to_string().contains(bad), "{err}");
        }
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
            prepare(&opts).unwrap().theme.name().to_string()
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
