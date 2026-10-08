//! `presenter build` (cli.md `build`): renders a deck to one self-contained HTML file or a
//! site directory (`index.html` + `assets/`).
//!
//! The output mode is `--standalone`/`--site`, else `[build] standalone` of the
//! `presenter.toml` that applies; the output path is `-o`, else
//! [`pipeline::default_output_path`]. Missing local assets are errors. On success stdout
//! gets exactly one line, `Wrote <path> (<n> slides, <size>)` (for a site, `<path>` is its
//! `index.html` and `<size>` the total of all files written); diagnostics go to stderr.
//! `--watch` is [`watch`] (T20).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::{BuildArgs, GlobalArgs, UsageError, input_format, print_stdout, report_diagnostics};
use crate::config::CliOverrides;
use crate::error::Error;
use crate::pipeline::{self, BuildOptions};
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
/// problems ([`Error::Config`]).
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
/// `Wrote …` line (not with `-q`).
pub fn build_once(opts: &BuildOptions, out: &Path, global: &GlobalArgs) -> anyhow::Result<()> {
    let build = pipeline::build_deck(opts).map_err(|err| locate(err, &opts.input))?;
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
        document.display(),
        if slides == 1 { "slide" } else { "slides" },
        format_size(bytes)
    ))
}

/// Adds the input's location to errors that do not name it themselves: a missing asset
/// (`slides.md:12: asset not found: …`) and front matter errors.
fn locate(err: Error, input: &Path) -> anyhow::Error {
    let location = match &err {
        Error::MissingAsset {
            span: Some(span), ..
        } => format!("{}:{}", input.display(), span.start_line),
        Error::MissingAsset { .. } | Error::FrontMatter(_) => input.display().to_string(),
        _ => return err.into(),
    };
    anyhow::Error::new(err).context(location)
}

/// `bytes` in decimal units with one decimal place: `512 B`, `12.3 kB`, `1.8 MB`.
pub fn format_size(bytes: usize) -> String {
    // Sizes of written files are far below 2^52, so the conversion is exact enough.
    let value = bytes as f64;
    if bytes < 1_000 {
        format!("{bytes} B")
    } else if bytes < 1_000_000 {
        format!("{:.1} kB", value / 1e3)
    } else {
        format!("{:.1} MB", value / 1e6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::SourceSpan;

    #[test]
    fn sizes_use_decimal_units() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1_000), "1.0 kB");
        assert_eq!(format_size(12_345), "12.3 kB");
        assert_eq!(format_size(1_849_000), "1.8 MB");
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
