//! `presenter export pdf`. Implemented by T45.
//!
//! Until then `run` is the T16 stub: it prints `export: not implemented yet` on stderr and
//! exits 2. T45 replaces the body of `run`; the arguments are defined in `cli/mod.rs`
//! ([`ExportArgs`]). `presenter export pdf --help` ends with [`PDF_LIMITATIONS`] (cli/mod.rs
//! attaches it with `after_help`), so T45 keeps the documented limitations here.

use std::process::ExitCode;

use super::{ExportArgs, GlobalArgs, not_implemented};

/// The known limitations of `export pdf`, shown at the end of `presenter export pdf --help`
/// (T45 acceptance criterion; T45 owns the wording).
pub const PDF_LIMITATIONS: &str = "\
Limitations (reveal.js print mode, ?print-pdf):
  - Fragments: with reveal.js' default ([reveal] pdfSeparateFragments = true) every
    fragment step is printed on its own page; with pdfSeparateFragments = false the
    fragments collapse and each slide is printed once, fully revealed.
  - Animations, transitions and auto-animate are not exported: every page is a static
    snapshot. Videos and iframes show whatever they display when the page is printed.
  - A slide taller than the page continues on extra pages, up to [reveal]
    pdfMaxPagesPerSlide.
  - Needs Chrome or Chromium; without one the command exits with code 3.";

/// Runs `presenter export` (stub until T45).
pub fn run(args: &ExportArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let _ = (args, global);
    not_implemented("export")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::cli::{EXIT_USAGE, ExportCommand, ExportPdfArgs};

    /// The stub's contract (`export: not implemented yet` on stderr, exit 2); T45 deletes
    /// this test together with the stub.
    #[test]
    fn stub_exits_2() {
        let args = ExportArgs {
            command: ExportCommand::Pdf(ExportPdfArgs {
                input: PathBuf::from("slides.md"),
                out: None,
                chrome: None,
                theme: None,
            }),
        };
        let code = run(&args, &GlobalArgs::default()).unwrap();
        assert_eq!(code, ExitCode::from(EXIT_USAGE));
    }
}
