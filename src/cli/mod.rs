//! Command-line interface (`docs/spec/cli.md`): the complete clap surface, logging setup,
//! error reporting and exit codes. `src/main.rs` only calls [`run`].
//!
//! # Per-command modules
//!
//! This file defines every command's arguments ([`BuildArgs`], [`ServeArgs`], [`InitArgs`],
//! [`ThemeArgs`], [`DumpArgs`], [`CheckArgs`], [`ExportArgs`]) and the global flags
//! ([`GlobalArgs`]). Each command lives in its own module with exactly this entry point:
//!
//! ```text
//! pub fn run(args: &<Cmd>Args, global: &GlobalArgs) -> anyhow::Result<std::process::ExitCode>
//! ```
//!
//! | command | module | task |
//! |---|---|---|
//! | `build` | [`build`] (`--watch`: `build::watch`) | T16 (watch: T20) |
//! | `serve` | [`serve`] | T23 |
//! | `init` | [`init`] | T40 |
//! | `theme list/eject/new/check` | [`theme`] | T41 |
//! | `dump` | [`dump`] | T16 |
//! | `check` | [`check`] | T34 |
//! | `export pdf` | [`export`] | T45 |
//!
//! A command that is not implemented yet prints `<command>: not implemented yet` on stderr
//! and exits 2 ([`not_implemented`]); its task replaces the body of `run` and never needs to
//! touch this file. `run` returns the exit code for the outcomes it handles itself (e.g.
//! `check --deny-warnings` → 1, `export` without Chrome → 3) and `Err` for failures, which
//! [`run`] prints as `error: …` and maps with [`exit_code_for`]: [`UsageError`] and
//! [`Error::Config`] → 2, everything else → 1. Helpers for commands:
//! [`report_diagnostics`] (prints `path:line: level: message` on stderr, paths relative to
//! the current directory, honouring `-q`), [`locate`] (adds the input's location to an
//! error that lacks it), [`print_stdout`], [`display_path`], [`input_kind`] (the input's
//! format by extension) and [`input_format`] (the same for a command with `--format md|ipynb`).
//!
//! Stub commands are tested in their own module (a `#[cfg(test)]` test that the owning task
//! deletes with the stub), never end to end.
//!
//! # Hooks for later tasks
//!
//! Besides the stub bodies, later tasks change behaviour defined here only through files
//! they own:
//!
//! - all printing on stderr goes through [`report`] (T44: source excerpts, panic hook);
//! - `--version` prints [`version`], which appends ` (<sha>)` when the build script sets
//!   `PRESENTER_GIT_SHA` (T47 adds the build script only);
//! - `presenter export pdf --help` ends with [`export::PDF_LIMITATIONS`] (T45).
//!
//! A new flag still means a new field in this file (e.g. T46's optional
//! `dump --default-config`, which cli.md does not list): that needs the coordinator.
//!
//! # Global behaviour
//!
//! Logging ([`log_directives`]): the base level is warn; `-v` raises presenter's own
//! modules to info and `-vv` to debug; `RUST_LOG` (when set) is applied on top of that, so it
//! can add targets or set any level, including presenter's. `-q` logs only errors and
//! ignores `RUST_LOG`. Logs and diagnostics go to stderr, in color when stderr is a terminal
//! and `NO_COLOR` is not set. Stdout carries only a command's result.

pub mod build;
pub mod check;
pub mod dump;
pub mod export;
pub mod init;
pub mod report;
pub mod serve;
pub mod theme;

use std::ffi::OsString;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

use crate::error::Error;
use crate::ir::SourceKind;

pub use report::report_diagnostics;

/// Exit code: success.
pub const EXIT_OK: u8 = 0;
/// Exit code: build error (parse, template, missing asset, …).
pub const EXIT_BUILD_ERROR: u8 = 1;
/// Exit code: usage or configuration error (also clap's code for bad arguments).
pub const EXIT_USAGE: u8 = 2;

/// Turn Markdown files and Jupyter notebooks into reveal.js presentations.
#[derive(Debug, Parser)]
#[command(name = "presenter", version = version(), about)]
pub struct Cli {
    /// Flags that apply to every command.
    #[command(flatten)]
    pub global: GlobalArgs,
    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Flags accepted before (or after) any command.
#[derive(Debug, Clone, Default, Args)]
pub struct GlobalArgs {
    /// Log more: -v info, -vv debug (RUST_LOG directives apply on top)
    #[arg(short, long, action = ArgAction::Count, global = true)]
    pub verbose: u8,
    /// Print only errors (no warnings, no summary line; RUST_LOG is ignored)
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,
    /// Use this presenter.toml instead of searching for one next to the input
    #[arg(long, value_name = "PATH", global = true)]
    pub config: Option<PathBuf>,
}

/// The commands of cli.md.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Render a deck to HTML
    Build(BuildArgs),
    /// Render, watch and live-reload in the browser
    Serve(ServeArgs),
    /// Scaffold a new presentation directory
    Init(InitArgs),
    /// Inspect, eject or create themes
    Theme(ThemeArgs),
    /// Print the IR or the template context as JSON
    Dump(DumpArgs),
    /// Lint a deck without building
    Check(CheckArgs),
    /// Export to other formats (pdf)
    Export(ExportArgs),
}

/// The input format named by `--format` (overrides the extension).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum InputFormat {
    /// Markdown (`.md`).
    Md,
    /// Jupyter notebook (`.ipynb`).
    Ipynb,
}

impl From<InputFormat> for SourceKind {
    fn from(format: InputFormat) -> SourceKind {
        match format {
            InputFormat::Md => SourceKind::Markdown,
            InputFormat::Ipynb => SourceKind::Notebook,
        }
    }
}

/// `presenter build`.
#[derive(Debug, Clone, Args)]
pub struct BuildArgs {
    /// The deck: a .md or .ipynb file
    pub input: PathBuf,
    /// Output file (standalone) or directory (site); the default is
    /// `pipeline::default_output_path`.
    #[arg(
        short = 'o',
        long = "out",
        value_name = "PATH",
        help = "Output file (standalone) or directory (site) [default: <build.out_dir>/<stem>.html, \
                or <build.out_dir>/<stem>/ with --site; out_dir is relative to presenter.toml, \
                or to the input's directory without one]"
    )]
    pub out: Option<PathBuf>,
    /// Theme name or directory (overrides front matter and presenter.toml)
    #[arg(long, value_name = "NAME|PATH")]
    pub theme: Option<String>,
    /// Write one self-contained HTML file (the default unless [build] standalone = false)
    #[arg(long, conflicts_with = "site")]
    pub standalone: bool,
    /// Write a directory: index.html plus assets/
    #[arg(long)]
    pub site: bool,
    /// Rebuild whenever the deck, its assets or the theme change
    #[arg(long)]
    pub watch: bool,
    /// Input format (default: from the file extension)
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub format: Option<InputFormat>,
}

impl BuildArgs {
    /// `--standalone` → `Some(true)`, `--site` → `Some(false)`, neither → `None`.
    pub fn standalone_override(&self) -> Option<bool> {
        match (self.standalone, self.site) {
            (true, _) => Some(true),
            (_, true) => Some(false),
            _ => None,
        }
    }
}

/// `presenter serve`.
#[derive(Debug, Clone, Args)]
pub struct ServeArgs {
    /// The deck: a .md or .ipynb file
    pub input: PathBuf,
    /// Theme name or directory (overrides front matter and presenter.toml)
    #[arg(long, value_name = "NAME|PATH")]
    pub theme: Option<String>,
    /// Port to listen on; 0 picks a free one [default: [serve] port, 3000]
    #[arg(long, value_name = "N")]
    pub port: Option<u16>,
    /// Address to bind [default: [serve] host, 127.0.0.1]
    #[arg(long, value_name = "H")]
    pub host: Option<String>,
    /// Open the deck in the default browser
    #[arg(long)]
    pub open: bool,
    /// Serve the deck the way a site build lays it out
    #[arg(long)]
    pub site_preview: bool,
}

/// `presenter init`.
#[derive(Debug, Clone, Args)]
pub struct InitArgs {
    /// Directory to create (must be empty or not exist)
    #[arg(default_value = ".")]
    pub dir: PathBuf,
    /// Presentation name [default: the directory name]
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    /// Start from a notebook (slides.ipynb) instead of slides.md
    #[arg(long)]
    pub notebook: bool,
    /// Eject the full default theme instead of a small child theme
    #[arg(long)]
    pub full_theme: bool,
}

/// `presenter theme`.
#[derive(Debug, Clone, Args)]
pub struct ThemeArgs {
    /// What to do.
    #[command(subcommand)]
    pub command: ThemeCommand,
}

/// The `theme` subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum ThemeCommand {
    /// List the built-in themes and the theme resolved for the current directory
    List,
    /// Copy a built-in theme's files to DIR (DIR must not exist)
    Eject(ThemeEjectArgs),
    /// Create a minimal child theme in DIR
    New(ThemeNewArgs),
    /// Validate a theme: theme.toml, its chain and the required templates
    Check(ThemeCheckArgs),
}

/// `presenter theme eject`.
#[derive(Debug, Clone, Args)]
pub struct ThemeEjectArgs {
    /// Built-in theme to copy
    pub name: String,
    /// Destination directory (must not exist)
    pub dir: PathBuf,
}

/// `presenter theme new`.
#[derive(Debug, Clone, Args)]
pub struct ThemeNewArgs {
    /// Directory of the new theme
    pub dir: PathBuf,
    /// Parent theme [default: default]
    #[arg(long, value_name = "NAME")]
    pub extends: Option<String>,
}

/// `presenter theme check`.
#[derive(Debug, Clone, Args)]
pub struct ThemeCheckArgs {
    /// Theme directory to validate
    pub dir: PathBuf,
}

/// `presenter dump`.
#[derive(Debug, Clone, Args)]
pub struct DumpArgs {
    /// The deck: a .md or .ipynb file
    pub input: PathBuf,
    /// Print the template context (theme resolved) instead of the deck IR
    #[arg(long)]
    pub context: bool,
    /// Theme name or directory (overrides front matter and presenter.toml)
    #[arg(long, value_name = "NAME|PATH")]
    pub theme: Option<String>,
    /// Pretty-print the JSON (the default)
    #[arg(long)]
    pub pretty: bool,
    /// Print the JSON on one line
    #[arg(long, conflicts_with = "pretty")]
    pub compact: bool,
}

/// `check --format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum CheckFormat {
    /// `path:line: level: message` lines.
    #[default]
    Text,
    /// A JSON array of diagnostics.
    Json,
}

/// `presenter check`.
#[derive(Debug, Clone, Args)]
pub struct CheckArgs {
    /// Decks to lint (.md or .ipynb)
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<PathBuf>,
    /// Output format
    #[arg(long, value_enum, default_value_t = CheckFormat::Text)]
    pub format: CheckFormat,
    /// Exit 1 when there are warnings
    #[arg(long)]
    pub deny_warnings: bool,
    /// Theme name or directory (overrides front matter and presenter.toml)
    #[arg(long, value_name = "NAME|PATH")]
    pub theme: Option<String>,
}

/// `presenter export`.
#[derive(Debug, Clone, Args)]
pub struct ExportArgs {
    /// The format to export to.
    #[command(subcommand)]
    pub command: ExportCommand,
}

/// The `export` formats.
#[derive(Debug, Clone, Subcommand)]
pub enum ExportCommand {
    /// Export a deck to PDF with headless Chrome
    #[command(after_help = export::PDF_LIMITATIONS)]
    Pdf(ExportPdfArgs),
}

/// `presenter export pdf`.
#[derive(Debug, Clone, Args)]
pub struct ExportPdfArgs {
    /// The deck: a .md or .ipynb file
    pub input: PathBuf,
    /// PDF file to write [default: next to the HTML output, with .pdf]
    #[arg(short = 'o', long = "out", value_name = "FILE")]
    pub out: Option<PathBuf>,
    /// Chrome or Chromium executable [default: $CHROME_PATH, then well-known locations]
    #[arg(long, value_name = "PATH")]
    pub chrome: Option<PathBuf>,
    /// Theme name or directory (overrides front matter and presenter.toml)
    #[arg(long, value_name = "NAME|PATH")]
    pub theme: Option<String>,
}

/// A usage error found after argument parsing (e.g. an input whose format cannot be told
/// from its name). [`exit_code_for`] maps it to [`EXIT_USAGE`].
#[derive(Debug)]
pub struct UsageError(pub String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

/// Runs the command line of this process and returns its exit code (`main` calls this).
/// Installs the panic hook first ([`report::install_panic_hook`]).
pub fn run() -> ExitCode {
    report::install_panic_hook();
    run_from(std::env::args_os())
}

/// What `presenter --version` prints after the program name: the crate version, followed by
/// ` (<build>)` when `PRESENTER_GIT_SHA` is set at compile time (by T47's build script, e.g.
/// `presenter 0.1.0 (abc1234)`); `tests/cli_version.rs` accepts both forms.
pub fn version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION
        .get_or_init(|| version_text(env!("CARGO_PKG_VERSION"), option_env!("PRESENTER_GIT_SHA")))
}

/// `version`, plus ` (<sha>)` when `sha` is given and not blank.
fn version_text(version: &str, sha: Option<&str>) -> String {
    match sha.map(str::trim).filter(|sha| !sha.is_empty()) {
        Some(sha) => format!("{version} ({sha})"),
        None => version.to_string(),
    }
}

/// Runs the command line `args` (the first item is the program name). Help and version
/// exit 0; argument errors print clap's message and exit 2.
pub fn run_from<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => {
            // Nothing sensible is left to do when the terminal is gone.
            let _ = err.print();
            return ExitCode::from(u8::try_from(err.exit_code()).unwrap_or(EXIT_USAGE));
        }
    };
    init_logging(&cli.global);
    match dispatch(&cli) {
        Ok(code) => code,
        Err(err) => {
            report::report_error(&err);
            ExitCode::from(exit_code_for(&err))
        }
    }
}

/// Calls the command's `run`.
fn dispatch(cli: &Cli) -> anyhow::Result<ExitCode> {
    let global = &cli.global;
    match &cli.command {
        Command::Build(args) => build::run(args, global),
        Command::Serve(args) => serve::run(args, global),
        Command::Init(args) => init::run(args, global),
        Command::Theme(args) => theme::run(args, global),
        Command::Dump(args) => dump::run(args, global),
        Command::Check(args) => check::run(args, global),
        Command::Export(args) => export::run(args, global),
    }
}

/// The exit code for a failed command: [`EXIT_USAGE`] for a [`UsageError`] or an
/// [`Error::Config`] anywhere in the chain, else [`EXIT_BUILD_ERROR`].
pub fn exit_code_for(err: &anyhow::Error) -> u8 {
    let usage = err.chain().any(|cause| {
        cause.is::<UsageError>() || matches!(cause.downcast_ref::<Error>(), Some(Error::Config(_)))
    });
    if usage { EXIT_USAGE } else { EXIT_BUILD_ERROR }
}

/// The stub body of a command that a later task implements: prints
/// `<command>: not implemented yet` on stderr and exits [`EXIT_USAGE`].
pub fn not_implemented(command: &str) -> anyhow::Result<ExitCode> {
    let _ = writeln!(std::io::stderr().lock(), "{command}: not implemented yet");
    Ok(ExitCode::from(EXIT_USAGE))
}

/// The input format for a command with `--format md|ipynb` (`build`): the flag when given,
/// else [`input_kind`], whose error then suggests the flag.
pub fn input_format(path: &Path, format: Option<InputFormat>) -> anyhow::Result<SourceKind> {
    match format {
        Some(format) => Ok(format.into()),
        None => crate::pipeline::detect_kind(path)
            .map_err(|err| UsageError(format!("{err}; use --format md|ipynb")).into()),
    }
}

/// The input format by extension, for commands without `--format` (`dump`, `serve`,
/// `check`, `export`): an unknown extension is a [`UsageError`] (exit 2) without a flag hint.
pub fn input_kind(path: &Path) -> anyhow::Result<SourceKind> {
    crate::pipeline::detect_kind(path).map_err(|err| UsageError(err.to_string()).into())
}

/// Adds the input's location to an error that does not name it itself: a missing asset
/// (`slides.md:12: asset not found: …`, or `talk.ipynb: cell 5, line 2: …` for a notebook,
/// like [`crate::error::Diagnostic`]'s Display) and front matter errors (`slides.md: front matter: …`).
/// Other errors are passed through.
pub fn locate(err: Error, input: &Path) -> anyhow::Error {
    let location = match &err {
        Error::MissingAsset {
            span: Some(span), ..
        } => match span.cell {
            Some(cell) => format!("{}: cell {cell}, line {}", input.display(), span.start_line),
            None => format!("{}:{}", input.display(), span.start_line),
        },
        Error::MissingAsset { .. } | Error::FrontMatter(_) => input.display().to_string(),
        _ => return err.into(),
    };
    anyhow::Error::new(err).context(location)
}

/// `path` for messages: relative to the current directory when it is an absolute path below
/// it (as diagnostics print paths), else unchanged.
pub fn display_path(path: &Path) -> PathBuf {
    let cwd = std::env::current_dir().ok();
    relative_to(path, cwd.as_deref())
}

/// `path` relative to `cwd` when it is below it, else `path`.
fn relative_to(path: &Path, cwd: Option<&Path>) -> PathBuf {
    match cwd.map(|cwd| path.strip_prefix(cwd)) {
        Some(Ok(relative)) if !relative.as_os_str().is_empty() => relative.to_path_buf(),
        _ => path.to_path_buf(),
    }
}

/// Writes `text` and a line break to stdout. A closed pipe (`presenter dump x.md | head`)
/// is not an error.
pub fn print_stdout(text: &str) -> anyhow::Result<()> {
    let mut out = std::io::stdout().lock();
    match writeln!(out, "{text}").and_then(|()| out.flush()) {
        Err(err) if err.kind() != std::io::ErrorKind::BrokenPipe => Err(err.into()),
        _ => Ok(()),
    }
}

/// The `EnvFilter` directives for `global` and the value of `RUST_LOG`: the flags' level
/// first, then `RUST_LOG` (a later directive for the same target replaces an earlier one, so
/// `RUST_LOG=presenter=debug` with `-v` logs presenter at debug). `-q` ignores `RUST_LOG`.
pub fn log_directives(global: &GlobalArgs, rust_log: Option<&str>) -> String {
    let base = match (global.quiet, global.verbose) {
        (true, _) => return "error".to_string(),
        (false, 0) => "warn",
        (false, 1) => "warn,presenter=info",
        (false, _) => "warn,presenter=debug",
    };
    match rust_log.map(str::trim).filter(|value| !value.is_empty()) {
        Some(rust_log) => format!("{base},{rust_log}"),
        None => base.to_string(),
    }
}

/// Installs the tracing subscriber (see the module docs). Does nothing when one is
/// already installed.
fn init_logging(global: &GlobalArgs) {
    use tracing_subscriber::EnvFilter;

    let rust_log = std::env::var("RUST_LOG").ok();
    // Invalid RUST_LOG directives are skipped with a note on stderr.
    let filter = EnvFilter::builder().parse_lossy(log_directives(global, rust_log.as_deref()));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(report::stderr_color())
        .with_target(false)
        .without_time()
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::SourceSpan;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{args:?}: {e}"))
    }

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn version_string() {
        let rendered = Cli::command().render_version().to_string();
        assert_eq!(rendered, format!("presenter {}\n", version()));
        assert!(rendered.starts_with("presenter 0.1.0"), "{rendered}");
        assert_eq!(version_text("0.1.0", None), "0.1.0");
        assert_eq!(version_text("0.1.0", Some(" ")), "0.1.0");
        assert_eq!(version_text("0.1.0", Some("abc1234")), "0.1.0 (abc1234)");
        if option_env!("PRESENTER_GIT_SHA").is_none() {
            assert_eq!(rendered, "presenter 0.1.0\n");
        }
    }

    #[test]
    fn every_cli_md_command_parses() {
        let cases: &[&[&str]] = &[
            &[
                "presenter",
                "build",
                "s.md",
                "-o",
                "out.html",
                "--theme",
                "./t",
            ],
            &[
                "presenter",
                "build",
                "s.md",
                "--site",
                "--watch",
                "--format",
                "md",
            ],
            &["presenter", "build", "s.ipynb", "--standalone"],
            &["presenter", "-vv", "--config", "p.toml", "build", "s.md"],
            &["presenter", "build", "s.md", "-q"],
            &[
                "presenter",
                "serve",
                "s.md",
                "--port",
                "0",
                "--host",
                "0.0.0.0",
            ],
            &[
                "presenter",
                "serve",
                "s.md",
                "--open",
                "--site-preview",
                "--theme",
                "x",
            ],
            &["presenter", "init"],
            &[
                "presenter",
                "init",
                "talk",
                "--name",
                "Talk",
                "--notebook",
                "--full-theme",
            ],
            &["presenter", "theme", "list"],
            &["presenter", "theme", "eject", "default", "./theme"],
            &["presenter", "theme", "new", "./t", "--extends", "default"],
            &["presenter", "theme", "check", "./t"],
            &[
                "presenter",
                "dump",
                "s.md",
                "--context",
                "--theme",
                "x",
                "--pretty",
            ],
            &["presenter", "dump", "s.md", "--compact"],
            &["presenter", "check", "a.md", "b.ipynb", "--format", "json"],
            &[
                "presenter",
                "check",
                "a.md",
                "--deny-warnings",
                "--theme",
                "x",
            ],
            &[
                "presenter",
                "export",
                "pdf",
                "s.md",
                "-o",
                "s.pdf",
                "--chrome",
                "/c",
            ],
        ];
        for args in cases {
            parse(args);
        }
        let cli = parse(&["presenter", "-v", "build", "s.md", "--site", "-o", "out"]);
        assert_eq!(cli.global.verbose, 1);
        let Command::Build(build) = &cli.command else {
            panic!("expected build");
        };
        assert_eq!(build.standalone_override(), Some(false));
        assert_eq!(build.out.as_deref(), Some(Path::new("out")));
        let cli = parse(&["presenter", "init"]);
        let Command::Init(init) = &cli.command else {
            panic!("expected init");
        };
        assert_eq!(init.dir, PathBuf::from("."));
        let cli = parse(&["presenter", "check", "a.md"]);
        let Command::Check(check) = &cli.command else {
            panic!("expected check");
        };
        assert_eq!(check.format, CheckFormat::Text);
    }

    #[test]
    fn conflicting_and_missing_arguments_are_usage_errors() {
        for args in [
            &["presenter", "build", "s.md", "--site", "--standalone"][..],
            &["presenter", "build"][..],
            &["presenter", "-q", "-v", "build", "s.md"][..],
            &["presenter", "dump", "s.md", "--pretty", "--compact"][..],
            &["presenter", "check"][..],
            &["presenter", "build", "s.md", "--format", "docx"][..],
            &["presenter", "export", "s.md"][..],
        ] {
            let err = Cli::try_parse_from(args).unwrap_err();
            assert_eq!(err.exit_code(), i32::from(EXIT_USAGE), "{args:?}");
        }
    }

    #[test]
    fn exit_codes_follow_the_error_kind() {
        let config = anyhow::Error::new(Error::Config("x".into()));
        assert_eq!(exit_code_for(&config), EXIT_USAGE);
        let usage = anyhow::Error::new(UsageError("x".into())).context("while building");
        assert_eq!(exit_code_for(&usage), EXIT_USAGE);
        let theme = anyhow::Error::new(Error::Theme {
            name: "nope".into(),
            message: "not found".into(),
        });
        assert_eq!(exit_code_for(&theme), EXIT_BUILD_ERROR);
        assert_eq!(exit_code_for(&anyhow::anyhow!("other")), EXIT_BUILD_ERROR);
    }

    #[test]
    fn input_format_prefers_the_flag() {
        let path = Path::new("deck.txt");
        assert_eq!(
            input_format(path, Some(InputFormat::Md)).unwrap(),
            SourceKind::Markdown
        );
        let err = input_format(path, None).unwrap_err();
        assert_eq!(exit_code_for(&err), EXIT_USAGE);
        assert!(
            err.to_string().ends_with("; use --format md|ipynb"),
            "{err}"
        );
        assert_eq!(
            input_format(Path::new("x.ipynb"), None).unwrap(),
            SourceKind::Notebook
        );
        // Commands without --format do not suggest it.
        let err = input_kind(path).unwrap_err();
        assert_eq!(exit_code_for(&err), EXIT_USAGE);
        assert!(err.to_string().contains("deck.txt"), "{err}");
        assert!(!err.to_string().contains("--format"), "{err}");
        assert_eq!(input_kind(Path::new("a.md")).unwrap(), SourceKind::Markdown);
    }

    #[test]
    fn stubs_print_not_implemented_and_exit_2() {
        assert_eq!(
            not_implemented("serve").unwrap(),
            ExitCode::from(EXIT_USAGE)
        );
    }

    #[test]
    fn rust_log_applies_on_top_of_the_verbosity_flags() {
        let flags = |verbose: u8, quiet: bool| GlobalArgs {
            verbose,
            quiet,
            config: None,
        };
        assert_eq!(log_directives(&flags(0, false), None), "warn");
        assert_eq!(
            log_directives(&flags(1, false), None),
            "warn,presenter=info"
        );
        assert_eq!(
            log_directives(&flags(3, false), Some("  ")),
            "warn,presenter=debug"
        );
        assert_eq!(
            log_directives(&flags(0, false), Some("presenter=trace")),
            "warn,presenter=trace"
        );
        assert_eq!(
            log_directives(&flags(1, false), Some("presenter=debug,hyper=info")),
            "warn,presenter=info,presenter=debug,hyper=info"
        );
        assert_eq!(log_directives(&flags(0, true), Some("debug")), "error");

        // EnvFilter keeps the last directive for a target: RUST_LOG wins over -v.
        let filter = tracing_subscriber::EnvFilter::builder()
            .parse_lossy(log_directives(&flags(1, false), Some("presenter=debug")));
        let shown = filter.to_string();
        assert!(shown.contains("presenter=debug"), "{shown}");
        assert!(!shown.contains("presenter=info"), "{shown}");
    }

    #[test]
    fn export_pdf_help_documents_the_limitations() {
        let mut command = Cli::command();
        let pdf = command
            .find_subcommand_mut("export")
            .and_then(|export| export.find_subcommand_mut("pdf"))
            .expect("export pdf");
        let help = pdf.render_long_help().to_string();
        assert!(help.contains("pdfSeparateFragments"), "{help}");
        assert!(help.contains("static"), "{help}");
        assert!(help.contains("exits with code 3"), "{help}");
    }

    #[test]
    fn paths_are_shown_relative_to_the_current_directory() {
        let cwd = Some(Path::new("/work"));
        assert_eq!(
            relative_to(Path::new("/work/talk/dist/s.html"), cwd),
            PathBuf::from("talk/dist/s.html")
        );
        assert_eq!(
            relative_to(Path::new("/elsewhere/s.html"), cwd),
            PathBuf::from("/elsewhere/s.html")
        );
        assert_eq!(
            relative_to(Path::new("out/s.html"), cwd),
            PathBuf::from("out/s.html")
        );
        assert_eq!(relative_to(Path::new("/work"), cwd), PathBuf::from("/work"));
        assert_eq!(
            relative_to(Path::new("/work/x"), None),
            PathBuf::from("/work/x")
        );
    }

    #[test]
    fn missing_assets_and_front_matter_errors_name_the_input() {
        let input = Path::new("talk/slides.md");
        let missing = Error::MissingAsset {
            path: PathBuf::from("/abs/talk/x.png"),
            span: Some(SourceSpan {
                start_line: 12,
                end_line: 12,
                cell: None,
            }),
        };
        assert_eq!(
            format!("{:#}", locate(missing, input)),
            "talk/slides.md:12: asset not found: /abs/talk/x.png"
        );
        let in_cell = Error::MissingAsset {
            path: PathBuf::from("/abs/fig.png"),
            span: Some(SourceSpan {
                start_line: 2,
                end_line: 2,
                cell: Some(5),
            }),
        };
        assert_eq!(
            format!("{:#}", locate(in_cell, Path::new("talk.ipynb"))),
            "talk.ipynb: cell 5, line 2: asset not found: /abs/fig.png"
        );
        let no_span = Error::MissingAsset {
            path: PathBuf::from("/abs/logo.png"),
            span: None,
        };
        assert_eq!(
            format!("{:#}", locate(no_span, input)),
            "talk/slides.md: asset not found: /abs/logo.png"
        );
        let front = Error::FrontMatter("expected a mapping".into());
        assert_eq!(
            format!("{:#}", locate(front, input)),
            "talk/slides.md: front matter: expected a mapping"
        );
        let theme = Error::Theme {
            name: "nope".into(),
            message: "not found".into(),
        };
        assert_eq!(
            format!("{:#}", locate(theme, input)),
            "theme 'nope': not found"
        );
    }
}
