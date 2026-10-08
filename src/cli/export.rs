//! `presenter export pdf` (cli.md `export`): builds the deck as one standalone HTML document
//! and prints it to PDF with headless Chrome ([`crate::pdf`]).
//!
//! - The browser is `--chrome PATH`, else `$CHROME_PATH`, else a well-known location
//!   ([`crate::pdf::find_chrome`]). None found: an error with install hints on stderr and
//!   exit [`EXIT_NO_CHROME`] (3), before anything is built.
//! - The deck is built like `presenter build --standalone` (`--theme`, `--config`, missing
//!   assets are errors, diagnostics on stderr), whatever `[build] standalone` says. The
//!   `[reveal]` options reach reveal.js unchanged, so `pdfSeparateFragments`,
//!   `pdfMaxPagesPerSlide`, `showNotes` and the slide size shape the PDF.
//! - The PDF goes to `-o FILE`, else next to where `presenter build` would write the HTML:
//!   [`crate::pipeline::default_output_path`] with `.pdf` (`dist/slides.pdf` for
//!   `slides.md`). On success stdout gets one line, `Wrote <path> (<n> pages, <size>)`
//!   (nothing with `-q`).
//! - A Chrome failure (it exits, times out after [`crate::pdf::DEFAULT_TIMEOUT`], or the
//!   page fails) is an error that includes Chrome's stderr: exit 1.
//!
//! `presenter export pdf --help` ends with [`PDF_LIMITATIONS`] (cli/mod.rs attaches it with
//! `after_help`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;

use super::build::format_size;
use super::report::report_error;
use super::{
    ExportArgs, ExportCommand, ExportPdfArgs, GlobalArgs, display_path, input_kind, locate,
    print_stdout, report_diagnostics,
};
use crate::config::{CliOverrides, Config};
use crate::pdf;
use crate::pipeline::{self, BuildFailure, BuildOptions};

/// Exit code of `export pdf` when no Chrome or Chromium is found (cli.md `export`).
pub const EXIT_NO_CHROME: u8 = 3;

/// The known limitations of `export pdf`, shown at the end of `presenter export pdf --help`
/// (T45 acceptance criterion).
pub const PDF_LIMITATIONS: &str = "\
Limitations (reveal.js print mode, ?print-pdf):
  - Fragments: with reveal.js' default ([reveal] pdfSeparateFragments = true) every
    fragment step is printed on its own page; with pdfSeparateFragments = false the
    fragments collapse and each slide is printed once, fully revealed.
  - Animations, transitions and auto-animate are not exported: every page is a static
    snapshot. Videos and iframes show whatever they display when the page is printed.
  - A slide taller than the page continues on extra pages, up to [reveal]
    pdfMaxPagesPerSlide.
  - Needs Chrome or Chromium (Edge and Brave work too); without one the command
    exits with code 3. Chrome gets 120 s for the whole export.

Tips: [reveal] options shape the PDF: width/height set the page size, showNotes = true
prints the speaker notes, and pdfSeparateFragments = false gives one page per slide.";

/// What `export pdf` prints when no browser is found (after `error: `).
const INSTALL_HINTS: &str = "\
presenter export pdf prints the deck with headless Chrome or Chromium (Microsoft Edge and
  Brave work too). Install one, for example:
    macOS:          brew install --cask google-chrome   (or: brew install --cask chromium)
    Debian, Ubuntu: sudo apt install chromium            (Ubuntu: sudo snap install chromium)
    Fedora:         sudo dnf install chromium
  or name an installed browser with --chrome PATH or the CHROME_PATH environment variable.";

/// Runs `presenter export`.
pub fn run(args: &ExportArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    match &args.command {
        ExportCommand::Pdf(pdf) => export_pdf(pdf, global),
    }
}

/// `presenter export pdf` (module docs).
fn export_pdf(args: &ExportPdfArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let kind = input_kind(&args.input)?;
    let Some(chrome) = pdf::find_chrome(args.chrome.as_deref()) else {
        report_error(&anyhow::anyhow!(no_chrome_message(args.chrome.as_deref())));
        return Ok(ExitCode::from(EXIT_NO_CHROME));
    };
    tracing::info!("using {}", chrome.display());
    let mut opts = BuildOptions::new(&args.input);
    opts.format = Some(kind);
    opts.config_path = global.config.clone();
    opts.cli = CliOverrides {
        theme: args.theme.clone(),
        standalone: Some(true),
        ..CliOverrides::default()
    };
    let build = match pipeline::try_build_deck(&opts) {
        Ok(build) => build,
        Err(BuildFailure { error, diagnostics }) => {
            report_diagnostics(&diagnostics, global);
            return Err(locate(error, &args.input));
        }
    };
    report_diagnostics(&build.diagnostics, global);
    let out = args
        .out
        .clone()
        .unwrap_or_else(|| default_pdf_path(&args.input, &build.config));
    let bytes = pdf::html_to_pdf(&chrome, &build.document.html, pdf::DEFAULT_TIMEOUT)?;
    write_pdf(&out, &bytes)?;
    if !global.quiet {
        let pages = pdf::page_count(&bytes);
        print_stdout(&format!(
            "Wrote {} ({pages} {}, {})",
            display_path(&out).display(),
            if pages == 1 { "page" } else { "pages" },
            format_size(bytes.len())
        ))?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `error: …` text when no browser is found: what was looked for, then [`INSTALL_HINTS`].
fn no_chrome_message(explicit: Option<&Path>) -> String {
    let what = match explicit {
        Some(path) if pdf::is_bare_name(path) => {
            format!("--chrome {}: not found on PATH", path.display())
        }
        Some(path) => format!("--chrome {}: not an executable file", path.display()),
        None => format!(
            "no Chrome or Chromium found (searched $CHROME_PATH, the usual application \
             folders and {} on PATH)",
            pdf::PATH_NAMES.join(", ")
        ),
    };
    format!("{what}\n  {INSTALL_HINTS}")
}

/// Where the PDF goes without `-o`: the standalone HTML output path with `.pdf`.
fn default_pdf_path(input: &Path, config: &Config) -> PathBuf {
    pipeline::default_output_path(input, config, true).with_extension("pdf")
}

/// Writes the PDF, creating parent directories.
fn write_pdf(out: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::write(out, bytes).with_context(|| format!("cannot write {}", out.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, EXIT_USAGE, exit_code_for};
    use clap::CommandFactory;

    fn pdf_args(input: &str, chrome: Option<&str>) -> ExportArgs {
        ExportArgs {
            command: ExportCommand::Pdf(ExportPdfArgs {
                input: PathBuf::from(input),
                out: None,
                chrome: chrome.map(PathBuf::from),
                theme: None,
            }),
        }
    }

    #[test]
    fn export_pdf_help_documents_the_limitations() {
        let mut command = Cli::command();
        let pdf = command
            .find_subcommand_mut("export")
            .and_then(|export| export.find_subcommand_mut("pdf"))
            .expect("export pdf");
        let help = pdf.render_long_help().to_string();
        assert!(help.contains("pdfSeparateFragments = false"), "{help}");
        assert!(help.contains("fragments collapse"), "{help}");
        assert!(help.contains("static"), "{help}");
        assert!(help.contains("exits with code 3"), "{help}");
    }

    #[test]
    fn a_missing_explicit_chrome_exits_3() {
        let args = pdf_args("slides.md", Some("/nonexistent/chrome"));
        let code = run(&args, &GlobalArgs::default()).unwrap();
        assert_eq!(code, ExitCode::from(EXIT_NO_CHROME));
    }

    #[test]
    fn unknown_inputs_are_usage_errors_before_chrome_is_looked_for() {
        let args = pdf_args("slides.txt", Some("/nonexistent/chrome"));
        let err = run(&args, &GlobalArgs::default()).unwrap_err();
        assert_eq!(exit_code_for(&err), EXIT_USAGE);
    }

    #[test]
    fn no_chrome_messages_name_the_search_and_how_to_install() {
        let message = no_chrome_message(None);
        assert!(message.starts_with("no Chrome or Chromium found (searched $CHROME_PATH"));
        assert!(message.contains("google-chrome, google-chrome-stable, chromium"));
        assert!(message.contains("brew install --cask google-chrome"));
        assert!(message.contains("sudo apt install chromium"));
        assert!(message.contains("--chrome PATH or the CHROME_PATH environment variable"));
        let message = no_chrome_message(Some(Path::new("/opt/chrome")));
        assert!(message.starts_with("--chrome /opt/chrome: not an executable file\n"));
        let message = no_chrome_message(Some(Path::new("./chromium")));
        assert!(message.starts_with("--chrome ./chromium: not an executable file\n"));
        // A bare name was looked up on PATH.
        let message = no_chrome_message(Some(Path::new("chromium")));
        assert!(message.starts_with("--chrome chromium: not found on PATH\n"));
    }

    #[test]
    fn the_pdf_goes_next_to_the_html_output() {
        let config = Config::default();
        assert_eq!(
            default_pdf_path(Path::new("talk/slides.md"), &config),
            Path::new("talk/dist/slides.pdf")
        );
        let mut config = Config::default();
        config.build.out_dir = PathBuf::from("/abs/out");
        assert_eq!(
            default_pdf_path(Path::new("deck.ipynb"), &config),
            Path::new("/abs/out/deck.pdf")
        );
    }
}
