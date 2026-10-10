//! `presenter export pdf` and `presenter export pptx` (cli.md `export`): build the deck as
//! one standalone HTML document, then print it to PDF with headless Chrome
//! ([`crate::pdf`]), or convert every page of its print view into shapes
//! ([`crate::pdf::capture`]) and write them as a PowerPoint file ([`crate::pptx`]), one
//! editable slide per page, with the speaker notes in the notes pane.
//!
//! Both commands run the same way (`export`), apart from the file they write:
//!
//! - The browser is `--chrome PATH`, else `$CHROME_PATH`, else a well-known location
//!   ([`crate::pdf::find_chrome`]). None found: an error with install hints on stderr and
//!   exit [`EXIT_NO_CHROME`] (3), before anything is built.
//! - The deck is built like `presenter build --standalone` (`--theme`, `--config`, missing
//!   assets are errors, diagnostics on stderr), whatever `[build] standalone` says. The
//!   `[reveal]` options reach reveal.js unchanged, so `pdfSeparateFragments`,
//!   `pdfMaxPagesPerSlide`, `showNotes` and the slide size shape the PDF and, since its
//!   slides are the PDF's pages, the PowerPoint file too.
//! - The file goes to `-o FILE`, else next to where `presenter build` would write the HTML:
//!   [`crate::pipeline::default_output_path`] with the format's extension (`dist/slides.pdf`
//!   or `dist/slides.pptx` for `slides.md`). On success stdout gets one line, `Wrote <path>
//!   (<n> pages, <size>)` for a PDF or `Wrote <path> (<n> slides, <size>)` for a PowerPoint
//!   file (nothing with `-q`).
//! - The file never overwrites an input (audit F01): before Chrome is looked for, the output
//!   is compared by file identity with the input and the `presenter.toml` in use, and after
//!   the build, before Chrome starts, with the deck's local assets and the theme's files
//!   too ([`crate::pipeline::input_files`]). A clash is a usage error (exit 2), and so is a
//!   `-o` that names a directory. A `-o` that does not end in the format's extension gets a
//!   warning (`pdf-extension`, `pptx-extension`) once every check has passed, just before
//!   Chrome starts.
//! - A Chrome failure (it exits, times out after [`crate::pdf::DEFAULT_TIMEOUT`], plus
//!   [`crate::pdf::capture::PER_PAGE`] per page for `pptx`, or the page fails) is an error
//!   that includes Chrome's stderr: exit 1.
//!
//! `presenter export pdf --help` ends with [`PDF_LIMITATIONS`] and `presenter export pptx
//! --help` with [`PPTX_LIMITATIONS`] (cli/mod.rs attaches them with `after_help`).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;

use super::build::format_size;
use super::report::{report_build_failure, report_error};
use super::{
    ExportArgs, ExportCommand, ExportPdfArgs, ExportPptxArgs, GlobalArgs, clash_error,
    display_path, input_kind, print_stdout, reject_directory, report_diagnostics,
};
use crate::config::{CliOverrides, Config};
use crate::error::{Diagnostic, Diagnostics};
use crate::ir::DeckMeta;
use crate::pdf::{self, capture};
use crate::pipeline::{self, BuildOptions, InputRole};
use crate::pptx;

/// Exit code of `export pdf` and `export pptx` when no Chrome or Chromium is found (cli.md
/// `export`).
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
    pdfMaxPagesPerSlide. The default theme prints the lines of code blocks and outputs
    slightly closer than on screen, to leave room for the line Chrome moves at each page
    break; reveal.js reserves the pages from the screen height, so such a slide that ends
    just past a page boundary on screen can end with a page that is empty but for its
    background (about one in ten to twenty of them). Nothing is lost.
  - Needs Chrome or Chromium (Edge and Brave work too); without one the command
    exits with code 3. Chrome gets 120 s for the whole export.
  - The PDF is not byte-for-byte reproducible, unlike `presenter build`: Chrome stamps
    the export time (CreationDate, ModDate) and its own version (Creator, Producer)
    into it, and another Chrome version lays the pages out differently.

Tips: [reveal] options shape the PDF: width/height set the page size, showNotes = true
prints the speaker notes, and pdfSeparateFragments = false gives one page per slide.";

/// The known limitations of `export pptx`, shown at the end of `presenter export pptx
/// --help`.
pub const PPTX_LIMITATIONS: &str = "\
What the file holds (from reveal.js' print mode, ?print-pdf):
  - Every page of the PDF export becomes one editable slide: headings, paragraphs,
    lists, code and quotes are text boxes with the fonts, sizes, colours and bullets
    the theme gave them (code keeps its highlighting colours, one paragraph per line),
    tables are tables, images are the deck's image files, and the slide background
    is a colour or the background picture. Everything sits where the browser laid it
    out. The speaker notes are text in the notes pane, one paragraph per line.
  - What PowerPoint cannot hold as native content becomes a picture with a transparent
    background: SVG, canvases, videos and iframes (as displayed), display math, and
    gradient backgrounds. Inline math becomes text (superscripts and subscripts kept,
    fractions as a/b). Themes' own decorations (rules under headings, logos) are
    kept when they are boxes or pictures; reveal.js' slide number is left out.
  - Fragments: with reveal.js' default ([reveal] pdfSeparateFragments = true) every
    fragment step is a slide of its own, so the slides build the deck's slide up step
    by step; with pdfSeparateFragments = false each slide is exported once, fully
    revealed.
  - Animations, transitions and auto-animate are not exported: every slide is static.
  - A slide taller than the page continues on extra slides, up to [reveal]
    pdfMaxPagesPerSlide, with the same pages as the PDF.
  - Fonts: PowerPoint substitutes a font the viewer's machine does not have, so text
    may wrap differently there; a text box grows to fit when it is edited.
  - Needs Chrome or Chromium (Edge and Brave work too); without one the command
    exits with code 3. Chrome gets 120 s to load the deck, plus 3 s per page.
  - The file is reproducible for one Chrome version: the same deck gives the same
    file.

Tips: [reveal] options shape the file: width/height set the slide size (16:9 gives
PowerPoint's Widescreen size, 4:3 its Standard size) and pdfSeparateFragments = false
gives one slide per slide; --scale sets the resolution of the pictures.";

/// What `export` prints when no browser is found (after `error: `).
const INSTALL_HINTS: &str = "\
presenter export needs headless Chrome or Chromium (Microsoft Edge and Brave work too).
  Install one, for example:
    macOS:          brew install --cask google-chrome   (or: brew install --cask chromium)
    Debian, Ubuntu: sudo apt install chromium            (Ubuntu: sudo snap install chromium)
    Fedora:         sudo dnf install chromium
  or name an installed browser with --chrome PATH or the CHROME_PATH environment variable.";

/// Runs `presenter export`.
pub fn run(args: &ExportArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    match &args.command {
        ExportCommand::Pdf(pdf) => export(&Export::pdf(pdf), global),
        ExportCommand::Pptx(pptx) => export(&Export::pptx(pptx), global),
    }
}

/// The file an export writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// A PDF, printed by Chrome.
    Pdf,
    /// A PowerPoint file of pictures taken at `scale` device pixels per CSS pixel.
    Pptx {
        /// The `--scale`.
        scale: u8,
    },
}

impl Format {
    /// The file extension the output should have.
    fn extension(self) -> &'static str {
        match self {
            Format::Pdf => "pdf",
            Format::Pptx { .. } => "pptx",
        }
    }

    /// The file, as the `-o` errors and warnings name it.
    fn file(self) -> &'static str {
        match self {
            Format::Pdf => "PDF",
            Format::Pptx { .. } => "PowerPoint",
        }
    }

    /// What the `Wrote` line counts: `page`/`pages`, `slide`/`slides`.
    fn unit(self, count: usize) -> &'static str {
        match (self, count) {
            (Format::Pdf, 1) => "page",
            (Format::Pdf, _) => "pages",
            (Format::Pptx { .. }, 1) => "slide",
            (Format::Pptx { .. }, _) => "slides",
        }
    }
}

/// One export: the arguments both commands share, and the format.
#[derive(Debug)]
struct Export<'a> {
    /// The deck.
    input: &'a Path,
    /// `-o`.
    out: Option<&'a Path>,
    /// `--chrome`.
    chrome: Option<&'a Path>,
    /// `--theme`.
    theme: Option<&'a str>,
    /// What to write.
    format: Format,
}

impl<'a> Export<'a> {
    /// `presenter export pdf`.
    fn pdf(args: &'a ExportPdfArgs) -> Export<'a> {
        Export {
            input: &args.input,
            out: args.out.as_deref(),
            chrome: args.chrome.as_deref(),
            theme: args.theme.as_deref(),
            format: Format::Pdf,
        }
    }

    /// `presenter export pptx`.
    fn pptx(args: &'a ExportPptxArgs) -> Export<'a> {
        Export {
            input: &args.input,
            out: args.out.as_deref(),
            chrome: args.chrome.as_deref(),
            theme: args.theme.as_deref(),
            format: Format::Pptx { scale: args.scale },
        }
    }
}

/// Runs one export (module docs).
fn export(export: &Export<'_>, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let kind = input_kind(export.input)?;
    let mut opts = BuildOptions::new(export.input);
    opts.format = Some(kind);
    opts.config_path = global.config.clone();
    opts.cli = CliOverrides {
        theme: export.theme.map(str::to_string),
        standalone: Some(true),
        ..CliOverrides::default()
    };
    let out = output_path(export, &opts)?;
    let Some(chrome) = pdf::find_chrome(export.chrome) else {
        report_error(&anyhow::anyhow!(no_chrome_message(export.chrome)));
        return Ok(ExitCode::from(EXIT_NO_CHROME));
    };
    tracing::info!("using {}", chrome.display());
    let build = match pipeline::try_build_deck(&opts) {
        Ok(build) => build,
        Err(failure) => return Err(report_build_failure(failure, export.input, global)),
    };
    report_diagnostics(&build.diagnostics, global);
    if let Some(clash) = pipeline::find_clash([out.as_path()], &pipeline::input_files(&build)) {
        return Err(clash_error(&clash, export.out.is_some()));
    }
    warn_unless_extension(export.out, export.format, global);
    let html = &build.document.html;
    let (bytes, count) = match export.format {
        Format::Pdf => {
            let bytes = pdf::html_to_pdf(&chrome, html, pdf::DEFAULT_TIMEOUT)?;
            let pages = pdf::page_count(&bytes);
            (bytes, pages)
        }
        Format::Pptx { scale } => {
            let pages = capture::html_to_pages(&chrome, html, pdf::DEFAULT_TIMEOUT, scale)?;
            let presentation = presentation(&build.deck.meta, pages);
            let bytes = pptx::write(&presentation)?;
            (bytes, presentation.slides.len())
        }
    };
    write_file(&out, &bytes)?;
    if !global.quiet {
        print_stdout(&format!(
            "Wrote {} ({count} {}, {})",
            display_path(&out).display(),
            export.format.unit(count),
            format_size(bytes.len())
        ))?;
    }
    Ok(ExitCode::SUCCESS)
}

/// The PowerPoint file's content: the deck's title and author from `meta`, and one slide
/// per page, named by [`describe`].
fn presentation(meta: &DeckMeta, pages: capture::Pages) -> pptx::Presentation {
    pptx::Presentation {
        title: meta.title.clone(),
        author: meta.author.clone(),
        width: pages.width,
        height: pages.height,
        slides: pages
            .pages
            .into_iter()
            .map(|page| pptx::Slide {
                name: describe(&page),
                background: page.background,
                shapes: page.shapes,
                notes: page.notes,
            })
            .collect(),
    }
}

/// What a page shows, for the slide's name: the slide's heading (or `Slide <n>`), then
/// `, step <k> of <n>` when the slide has fragment steps and `, part <k> of <n>` when it
/// takes more than one page.
fn describe(page: &capture::Page) -> String {
    let mut text = page
        .heading
        .clone()
        .unwrap_or_else(|| format!("Slide {}", page.slide));
    if page.steps > 1 {
        let _ = write!(text, ", step {} of {}", page.step, page.steps);
    }
    if page.parts > 1 {
        let _ = write!(text, ", part {} of {}", page.part, page.parts);
    }
    text
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

/// Where the file goes: `-o`, else [`default_output`] for the project config of `opts`.
/// Checked before Chrome is looked for: a `-o` that names a directory, and an output that
/// is the input or the `presenter.toml` in use, are usage errors.
fn output_path(export: &Export<'_>, opts: &BuildOptions) -> anyhow::Result<PathBuf> {
    let (config, config_path) = pipeline::load_config(opts)?;
    let out = match export.out {
        Some(out) => {
            reject_directory(
                out,
                &format!("give the {} file to write", export.format.file()),
            )?;
            out.to_path_buf()
        }
        None => default_output(export.input, &config, export.format),
    };
    let mut inputs = vec![(export.input.to_path_buf(), InputRole::Source)];
    inputs.extend(config_path.map(|path| (path, InputRole::Config)));
    if let Some(clash) = pipeline::find_clash([out.as_path()], &inputs) {
        return Err(clash_error(&clash, export.out.is_some()));
    }
    Ok(out)
}

/// The `pdf-extension` or `pptx-extension` warning (not with `-q`) when the `-o` file does
/// not end in the format's extension. Printed once every check has passed, just before
/// Chrome starts, so it never precedes a refusal to write that file.
fn warn_unless_extension(out: Option<&Path>, format: Format, global: &GlobalArgs) {
    let Some(out) = out else {
        return;
    };
    let extension = format.extension();
    if out
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
    {
        return;
    }
    let (code, holds) = match format {
        Format::Pdf => ("pdf-extension", "a PDF"),
        Format::Pptx { .. } => ("pptx-extension", "a PowerPoint presentation"),
    };
    let warning = Diagnostic::warning(
        code,
        format!(
            "-o {} does not end in .{extension}; the file will hold {holds}",
            display_path(out).display()
        ),
    );
    report_diagnostics(&Diagnostics::from(vec![warning]), global);
}

/// Where the file goes without `-o`: the standalone HTML output path with the format's
/// extension.
fn default_output(input: &Path, config: &Config, format: Format) -> PathBuf {
    pipeline::default_output_path(input, config, true).with_extension(format.extension())
}

/// Writes the file, creating parent directories.
fn write_file(out: &Path, bytes: &[u8]) -> anyhow::Result<()> {
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

    fn pptx_args(input: &str, chrome: Option<&str>) -> ExportArgs {
        ExportArgs {
            command: ExportCommand::Pptx(ExportPptxArgs {
                input: PathBuf::from(input),
                out: None,
                chrome: chrome.map(PathBuf::from),
                theme: None,
                scale: 2,
            }),
        }
    }

    /// `presenter export <format> --help`.
    fn help_of(format: &str) -> String {
        let mut command = Cli::command();
        let sub = command
            .find_subcommand_mut("export")
            .and_then(|export| export.find_subcommand_mut(format))
            .unwrap_or_else(|| panic!("export {format}"));
        sub.render_long_help().to_string()
    }

    #[test]
    fn export_pdf_help_documents_the_limitations() {
        let help = help_of("pdf");
        assert!(help.contains("pdfSeparateFragments = false"), "{help}");
        assert!(help.contains("fragments collapse"), "{help}");
        assert!(help.contains("static"), "{help}");
        assert!(help.contains("exits with code 3"), "{help}");
        // Audit F21: PDF export is the documented exception to reproducible output.
        assert!(
            help.contains("The PDF is not byte-for-byte reproducible"),
            "{help}"
        );
        assert!(help.contains("CreationDate, ModDate"), "{help}");
    }

    #[test]
    fn export_pptx_help_documents_the_limitations() {
        let help = help_of("pptx");
        for expected in [
            "Every page of the PDF export becomes one editable slide",
            "tables are tables",
            "becomes a picture with a transparent\n    background",
            "every\n    fragment step is a slide of its own",
            "pdfSeparateFragments = false",
            "pdfMaxPagesPerSlide",
            "exits with code 3",
            "plus 3 s per page",
            "--scale <N>",
            "[default: 2]",
        ] {
            assert!(help.contains(expected), "{expected:?} in {help}");
        }
    }

    #[test]
    fn a_missing_explicit_chrome_exits_3() {
        for args in [
            pdf_args("slides.md", Some("/nonexistent/chrome")),
            pptx_args("slides.md", Some("/nonexistent/chrome")),
        ] {
            let code = run(&args, &GlobalArgs::default()).unwrap();
            assert_eq!(code, ExitCode::from(EXIT_NO_CHROME));
        }
    }

    #[test]
    fn unknown_inputs_are_usage_errors_before_chrome_is_looked_for() {
        for args in [
            pdf_args("slides.txt", Some("/nonexistent/chrome")),
            pptx_args("slides.txt", Some("/nonexistent/chrome")),
        ] {
            let err = run(&args, &GlobalArgs::default()).unwrap_err();
            assert_eq!(exit_code_for(&err), EXIT_USAGE);
        }
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
    fn the_file_goes_next_to_the_html_output() {
        let config = Config::default();
        assert_eq!(
            default_output(Path::new("talk/slides.md"), &config, Format::Pdf),
            Path::new("talk/dist/slides.pdf")
        );
        assert_eq!(
            default_output(
                Path::new("talk/slides.md"),
                &config,
                Format::Pptx { scale: 2 }
            ),
            Path::new("talk/dist/slides.pptx")
        );
        let mut config = Config::default();
        config.build.out_dir = PathBuf::from("/abs/out");
        assert_eq!(
            default_output(Path::new("deck.ipynb"), &config, Format::Pdf),
            Path::new("/abs/out/deck.pdf")
        );
        assert_eq!(
            default_output(Path::new("deck.ipynb"), &config, Format::Pptx { scale: 1 }),
            Path::new("/abs/out/deck.pptx")
        );
    }

    #[test]
    fn pages_are_described_by_heading_step_and_part() {
        let picture = |heading: Option<&str>, step, steps, part, parts| capture::Page {
            slide: 4,
            step,
            steps,
            part,
            parts,
            heading: heading.map(str::to_string),
            notes: Vec::new(),
            background: pptx::Background::default(),
            shapes: Vec::new(),
        };
        assert_eq!(describe(&picture(Some("Results"), 1, 1, 1, 1)), "Results");
        assert_eq!(describe(&picture(None, 1, 1, 1, 1)), "Slide 4");
        assert_eq!(
            describe(&picture(Some("Results"), 2, 3, 1, 1)),
            "Results, step 2 of 3"
        );
        assert_eq!(describe(&picture(None, 1, 1, 2, 2)), "Slide 4, part 2 of 2");
        assert_eq!(
            describe(&picture(Some("Long"), 3, 3, 1, 2)),
            "Long, step 3 of 3, part 1 of 2"
        );
        assert_eq!(Format::Pdf.unit(1), "page");
        assert_eq!(Format::Pdf.unit(2), "pages");
        assert_eq!(Format::Pptx { scale: 2 }.unit(1), "slide");
        assert_eq!(Format::Pptx { scale: 2 }.unit(0), "slides");
    }
}
