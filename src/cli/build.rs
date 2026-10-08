//! `presenter build` (cli.md `build`): renders a deck to one self-contained HTML file or a
//! site directory (`index.html` + `assets/`).
//!
//! The output mode is `--standalone`/`--site`, else `[build] standalone` of the
//! `presenter.toml` that applies; the output path is `-o`, else
//! [`pipeline::default_output_path`]. Missing local assets are errors. On success stdout
//! gets exactly one line, `Wrote <path> (<n> slides, <size>)` (for a site, `<path>` is its
//! `index.html` and `<size>` the total of all files written; `<path>` is relative to the
//! current directory when below it); diagnostics go to stderr. A failed build prints the
//! diagnostics found before the error (warnings, and every other missing asset as an
//! error), then the error. `--watch` is [`watch`] (T20).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::{
    BuildArgs, GlobalArgs, UsageError, display_path, input_format, locate, print_stdout,
    report_diagnostics,
};
use crate::config::CliOverrides;
use crate::pipeline::{self, BuildFailure, BuildOptions};
use crate::render::BuildInfo;

/// Runs `presenter build`.
pub fn run(args: &BuildArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    if args.watch {
        return watch(args, global);
    }
    let (opts, out) = build_options(args, global)?;
    build_once(&opts, &out, global)?;
    Ok(ExitCode::SUCCESS)
}

/// `presenter build --watch`: rebuild on every change without serving. T20 replaces this
/// body (it can call [`build_options`] once and [`build_once`] per change).
pub fn watch(args: &BuildArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let _ = (args, global);
    Err(UsageError("build --watch: watch arrives in T20".into()).into())
}

/// The pipeline options for `args` and the output path (file or site directory).
///
/// Errors: an input whose format cannot be told ([`UsageError`]) and `presenter.toml`
/// problems ([`crate::error::Error::Config`]).
pub fn build_options(
    args: &BuildArgs,
    global: &GlobalArgs,
) -> anyhow::Result<(BuildOptions, PathBuf)> {
    let mut opts = BuildOptions::new(&args.input);
    opts.format = Some(input_format(&args.input, args.format)?);
    opts.config_path = global.config.clone();
    opts.cli = CliOverrides {
        theme: args.theme.clone(),
        out: args.out.clone(),
        standalone: args.standalone_override(),
        ..CliOverrides::default()
    };
    let (config, _) = pipeline::load_config(&opts)?;
    let standalone = config.build.standalone;
    let out = args
        .out
        .clone()
        .unwrap_or_else(|| pipeline::default_output_path(&args.input, &config, standalone));
    opts.output = pipeline::output_mode(standalone, &out);
    opts.build = BuildInfo::build(standalone);
    opts.missing_asset_is_error = true;
    Ok((opts, out))
}

/// Builds `opts`, prints its diagnostics, writes the output to `out` and prints the
/// `Wrote …` line (not with `-q`). On failure the diagnostics found before the error are
/// printed and the error is returned with the input's location ([`locate`]).
pub fn build_once(opts: &BuildOptions, out: &Path, global: &GlobalArgs) -> anyhow::Result<()> {
    let build = match pipeline::try_build_deck(opts) {
        Ok(build) => build,
        Err(BuildFailure { error, diagnostics }) => {
            report_diagnostics(&diagnostics, global);
            return Err(locate(error, &opts.input));
        }
    };
    report_diagnostics(&build.diagnostics, global);
    let written = pipeline::write_output(&build, out, &opts.output)?;
    if global.quiet {
        return Ok(());
    }
    let bytes = build.document.html.len()
        + build
            .document
            .files
            .iter()
            .map(|file| file.bytes.len())
            .sum::<usize>();
    let slides = build.document.slide_count;
    let document = written.first().map_or(out, PathBuf::as_path);
    print_stdout(&format!(
        "Wrote {} ({slides} {}, {})",
        display_path(document).display(),
        if slides == 1 { "slide" } else { "slides" },
        format_size(bytes)
    ))
}

/// `bytes` in decimal units with one decimal place: `512 B`, `12.3 kB`, `1.8 MB`. The unit
/// is chosen after rounding, so 999 950 bytes are `1.0 MB`, not `1000.0 kB`.
pub fn format_size(bytes: usize) -> String {
    if bytes < 1_000 {
        return format!("{bytes} B");
    }
    // Sizes of written files are far below 2^52, so the conversion is exact enough.
    let kilo_tenths = (bytes as f64 / 100.0).round();
    if kilo_tenths < 10_000.0 {
        format!("{:.1} kB", kilo_tenths / 10.0)
    } else {
        format!("{:.1} MB", bytes as f64 / 1e6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_use_decimal_units() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1_000), "1.0 kB");
        assert_eq!(format_size(1_049), "1.0 kB");
        assert_eq!(format_size(1_050), "1.1 kB");
        assert_eq!(format_size(12_345), "12.3 kB");
        assert_eq!(format_size(999_949), "999.9 kB");
        assert_eq!(format_size(999_950), "1.0 MB");
        assert_eq!(format_size(999_999), "1.0 MB");
        assert_eq!(format_size(1_000_000), "1.0 MB");
        assert_eq!(format_size(1_849_000), "1.8 MB");
        assert_eq!(format_size(1_250_000_000), "1250.0 MB");
    }

    /// The T20 stub (T20 replaces `watch` and deletes this test).
    #[test]
    fn watch_is_a_usage_error_until_t20() {
        let args = BuildArgs {
            input: PathBuf::from("slides.md"),
            out: None,
            theme: None,
            standalone: false,
            site: false,
            watch: true,
            format: None,
        };
        let err = run(&args, &GlobalArgs::default()).unwrap_err();
        assert_eq!(err.to_string(), "build --watch: watch arrives in T20");
        assert_eq!(super::super::exit_code_for(&err), super::super::EXIT_USAGE);
    }
}
