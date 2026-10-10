//! `presenter build` (cli.md `build`): renders a deck to one self-contained HTML file or a
//! site directory (`index.html` + `assets/`).
//!
//! The output mode is `--standalone`/`--site`, else `[build] standalone` of the
//! `presenter.toml` that applies; the output path is `-o`, else
//! [`pipeline::default_output_path`]. Missing local assets are errors. On success stdout
//! gets exactly one line, `Wrote <path> (<n> slides, <size>)` (for a site, `<path>` is its
//! `index.html` and `<size>` the total of all files written; `<path>` as
//! [`super::display_path`] shows it); diagnostics go to stderr. A failed build prints the
//! diagnostics found before the error, then the error; a build that failed on missing
//! assets reports each of them as a located error, in source order
//! ([`report::report_build_failure`]).
//!
//! The output never overwrites an input (audit F01): before building, the document's path
//! (`-o`, the default path, or `<out>/index.html` for a site) is compared by file identity
//! with the input and the `presenter.toml` in use, and after building, everything to be
//! written with the deck's local assets and the theme's files too
//! ([`pipeline::output_clash`], [`pipeline::input_files`]). A clash is a usage error
//! (exit 2) and nothing is written. A standalone `-o` that names a directory (a trailing
//! `/`, a last component `.` or `..`, or an existing directory) is a usage error too.
//!
//! `--watch` ([`watch`], T20) builds once, then rebuilds on every change of the deck's
//! inputs ([`crate::serve::watch`]) until interrupted: each build prints exactly what a
//! single build prints (a failed one prints its error and the loop goes on), and stderr gets
//! `Watching for changes (Ctrl-C to stop)` after the first build and `Rebuilding (<kind>):
//! <paths>` before every rebuild (neither with `-q`). A `presenter.toml` that cannot be
//! loaded at startup is reported the same way and watched until it can (the output path
//! and mode depend on it); then the first build runs. An output that would overwrite the
//! input or the `presenter.toml` ends the command before watching starts, and so does a
//! first build whose output would overwrite one of its other inputs (an asset, a theme
//! file): exit 2 with the message of a single build. A rebuild that would overwrite an
//! input the deck starts to use later fails with that message and watching goes on.

use std::io::Write as _;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;

use super::{
    BuildArgs, GlobalArgs, clash_error, display_path, input_format, print_stdout, reject_directory,
    report, report_diagnostics,
};
use crate::config::CliOverrides;
use crate::error::Error;
use crate::pipeline::{self, Build, BuildOptions, InputRole};
use crate::render::BuildInfo;
use crate::serve::watch::{self as watcher, ChangeBatch, ChangeKind, WatchStatus, Watcher};

/// The status line `build --watch` prints once watching has started.
const WATCHING: &str = "Watching for changes (Ctrl-C to stop)";

/// Runs `presenter build`.
pub fn run(args: &BuildArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    if args.watch {
        return watch(args, global);
    }
    let (opts, out) = build_options(args, global)?;
    build_once(&opts, &out, args.out.is_some(), global)?;
    Ok(ExitCode::SUCCESS)
}

/// `presenter build --watch`: builds, then rebuilds on every change without serving
/// ([`watcher::run_build_watch_with`]); runs until interrupted. An input whose format
/// cannot be told ends the command like a single build's; a `presenter.toml` that cannot be
/// loaded is reported and watched until it can be, then the first build runs; build
/// errors are printed and watching goes on. A first build whose output would overwrite
/// one of its inputs ends the command like a single build (its diagnostics, then the
/// usage error, exit 2): `-o` cannot change while watching.
pub fn watch(args: &BuildArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let (opts, out, announced) = match build_options(args, global) {
        Ok((opts, out)) => (opts, out, false),
        Err(err) if is_config_error(&err) => {
            let (opts, out) = wait_for_config(args, global, &err)?;
            (opts, out, true)
        }
        Err(err) => return Err(err),
    };
    let explicit = args.out.is_some();
    let mut first = true;
    let mut refused = None;
    watcher::run_build_watch_with(&opts, &out, |status| {
        // The first status is the first build's (`Built` or `Failed`).
        if std::mem::take(&mut first)
            && let WatchStatus::Failed(failure) = &status
            && let Some(clash) = pipeline::refused_clash(&failure.error)
        {
            report_diagnostics(&failure.diagnostics, global);
            refused = Some(clash_error(clash, explicit));
            return ControlFlow::Break(());
        }
        // `Watching …` was printed while waiting for the config.
        if !(announced && matches!(status, WatchStatus::Watching(_))) {
            print_watch_status(status, &opts, explicit, global);
        }
        ControlFlow::Continue(())
    })?;
    match refused {
        Some(err) => Err(err),
        None => Ok(ExitCode::SUCCESS),
    }
}

/// `build --watch` with a `presenter.toml` that cannot be loaded before the first build:
/// reports `err`, watches the source and the config file
/// ([`watcher::watch_set_before_build`]) and, on every change, prints `Rebuilding …` and
/// tries again (reporting each new error) until [`build_options`] succeeds.
fn wait_for_config(
    args: &BuildArgs,
    global: &GlobalArgs,
    err: &anyhow::Error,
) -> anyhow::Result<(BuildOptions, PathBuf)> {
    report::report_error(err);
    let (tx, rx) = mpsc::channel();
    let _watcher = Watcher::start(
        &watcher::watch_set_before_build(&base_options(args, global)?),
        tx,
    )?;
    status_line(WATCHING, global);
    for batch in rx {
        status_line(&rebuilding_line(&batch), global);
        match build_options(args, global) {
            Ok(found) => return Ok(found),
            Err(err) if is_config_error(&err) => report::report_error(&err),
            Err(err) => return Err(err),
        }
    }
    Err(anyhow::anyhow!("file watcher stopped"))
}

/// Whether `err` is a `presenter.toml` problem ([`Error::Config`] anywhere in its chain).
fn is_config_error(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| matches!(cause.downcast_ref::<Error>(), Some(Error::Config(_))))
}

/// Prints one [`WatchStatus`] of `build --watch` (module docs). A rebuild whose output
/// would overwrite one of its inputs is reported like a single build's refusal
/// ([`clash_error`]; `explicit`: the output came from `-o`).
fn print_watch_status(
    status: WatchStatus<'_>,
    opts: &BuildOptions,
    explicit: bool,
    global: &GlobalArgs,
) {
    match status {
        WatchStatus::Built { build, written } => {
            report_diagnostics(&build.diagnostics, global);
            if let Some(document) = written.first().filter(|_| !global.quiet) {
                // stdout is gone (closed pipe ignored); nothing sensible is left to do.
                let _ = print_stdout(&wrote_line(build, document));
            }
        }
        WatchStatus::Failed(failure) => match pipeline::refused_clash(&failure.error) {
            Some(clash) => {
                report_diagnostics(&failure.diagnostics, global);
                report::report_error(&clash_error(clash, explicit));
            }
            None => {
                let error = report::report_build_failure(failure, &opts.input, global);
                report::report_error(&error);
            }
        },
        WatchStatus::Watching(_) => status_line(WATCHING, global),
        WatchStatus::Changed(batch) => status_line(&rebuilding_line(batch), global),
    }
}

/// `Rebuilding (<kind>): <paths>` for a change batch: at most three paths (as
/// [`display_path`] shows them), then `and N more`.
fn rebuilding_line(batch: &ChangeBatch) -> String {
    const SHOWN: usize = 3;
    let kind = match batch.kind {
        ChangeKind::Css => "css",
        ChangeKind::Content => "content",
    };
    let mut paths: Vec<String> = batch
        .paths
        .iter()
        .take(SHOWN)
        .map(|path| display_path(path).display().to_string())
        .collect();
    if batch.paths.len() > SHOWN {
        paths.push(format!("and {} more", batch.paths.len() - SHOWN));
    }
    format!("Rebuilding ({kind}): {}", paths.join(", "))
}

/// Writes `text` as a line on stderr, unless `-q`.
fn status_line(text: &str, global: &GlobalArgs) {
    if !global.quiet {
        let _ = writeln!(std::io::stderr().lock(), "{text}");
    }
}

/// The pipeline options for `args` and the output path (file or site directory).
///
/// Errors: an input whose format cannot be told, a standalone `-o` that names a directory,
/// an output document that is the input or the `presenter.toml` in use (each a
/// [`super::UsageError`]), and `presenter.toml` problems ([`crate::error::Error::Config`]).
pub fn build_options(
    args: &BuildArgs,
    global: &GlobalArgs,
) -> anyhow::Result<(BuildOptions, PathBuf)> {
    let mut opts = base_options(args, global)?;
    let (config, config_path) = pipeline::load_config(&opts)?;
    let standalone = config.build.standalone;
    if let Some(out) = args.out.as_deref().filter(|_| standalone) {
        reject_directory(
            out,
            "give the HTML file to write, or use --site to write a directory",
        )?;
    }
    let out = args
        .out
        .clone()
        .unwrap_or_else(|| pipeline::default_output_path(&args.input, &config, standalone));
    let mut inputs = vec![(args.input.clone(), InputRole::Source)];
    inputs.extend(config_path.map(|path| (path, InputRole::Config)));
    let document = pipeline::document_path(&out, standalone);
    if let Some(clash) = pipeline::find_clash([document.as_path()], &inputs) {
        return Err(clash_error(&clash, args.out.is_some()));
    }
    opts.output = pipeline::output_mode(standalone, &out);
    opts.build = BuildInfo::build(standalone);
    opts.missing_asset_is_error = true;
    Ok((opts, out))
}

/// The options of `args` that do not depend on `presenter.toml`: input and format,
/// `--config` and the command-line overrides. Errors: an input whose format cannot be told.
fn base_options(args: &BuildArgs, global: &GlobalArgs) -> anyhow::Result<BuildOptions> {
    let mut opts = BuildOptions::new(&args.input);
    opts.format = Some(input_format(&args.input, args.format)?);
    opts.config_path = global.config.clone();
    opts.cli = CliOverrides {
        theme: args.theme.clone(),
        out: args.out.clone(),
        standalone: args.standalone_override(),
        ..CliOverrides::default()
    };
    Ok(opts)
}

/// Builds `opts`, prints its diagnostics, writes the output to `out` and prints the
/// `Wrote …` line (not with `-q`). On failure the diagnostics found before the error are
/// printed and the error is returned with the input's location
/// ([`report::report_build_failure`]). An output that would overwrite one of the build's
/// inputs ([`pipeline::output_clash`]) is a usage error and nothing is written; `explicit`
/// says whether `out` came from `-o` (for the message).
pub fn build_once(
    opts: &BuildOptions,
    out: &Path,
    explicit: bool,
    global: &GlobalArgs,
) -> anyhow::Result<()> {
    let build = match pipeline::try_build_deck(opts) {
        Ok(build) => build,
        Err(failure) => return Err(report::report_build_failure(failure, &opts.input, global)),
    };
    report_diagnostics(&build.diagnostics, global);
    if let Some(clash) = pipeline::output_clash(&build, out, &opts.output) {
        return Err(clash_error(&clash, explicit));
    }
    let written = pipeline::write_output(&build, out, &opts.output)?;
    if global.quiet {
        return Ok(());
    }
    let document = written.first().map_or(out, PathBuf::as_path);
    print_stdout(&wrote_line(&build, document))
}

/// `Wrote <document> (<n> slides, <size>)`: `document` as [`display_path`] shows it,
/// `<size>` the document plus every file written next to it.
fn wrote_line(build: &Build, document: &Path) -> String {
    let bytes = build.document.html.len()
        + build
            .document
            .files
            .iter()
            .map(|file| file.bytes.len())
            .sum::<usize>();
    let slides = build.document.slide_count;
    format!(
        "Wrote {} ({slides} {}, {})",
        display_path(document).display(),
        if slides == 1 { "slide" } else { "slides" },
        format_size(bytes)
    )
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

    #[test]
    fn rebuilding_line_shows_three_paths() {
        let cwd = std::env::current_dir().unwrap();
        let batch = |names: &[&str], kind| ChangeBatch {
            paths: names.iter().map(|name| cwd.join(name)).collect(),
            kind,
        };
        assert_eq!(
            rebuilding_line(&batch(&["slides.md"], ChangeKind::Content)),
            "Rebuilding (content): slides.md"
        );
        // `/`-separated on every platform (cli::display_path).
        assert_eq!(
            rebuilding_line(&batch(&["t/a.css", "t/b.css"], ChangeKind::Css)),
            "Rebuilding (css): t/a.css, t/b.css"
        );
        let many = rebuilding_line(&batch(&["a", "b", "c", "d", "e"], ChangeKind::Content));
        assert_eq!(many, "Rebuilding (content): a, b, c, and 2 more");
    }
}
