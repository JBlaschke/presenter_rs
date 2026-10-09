//! `presenter check` (cli.md `check`): lints each input without building it
//! ([`crate::check::run`]) and reports what it found.
//!
//! - `--format text` (the default): one `path:line: level: message` line per diagnostic on
//!   stderr ([`report_diagnostics`]: paths as [`super::display_path`] shows them, the level
//!   in color when stderr is a terminal). With `-q` only errors, unless `--deny-warnings`
//!   makes the warnings fail the run: then they are printed too, so a failing `-q check
//!   --deny-warnings` always says why (audit F24). Nothing on stdout.
//! - `--format json`: one JSON array on stdout, `[{level, code, message, path, line, cell}]`
//!   (`path` as in the text format; `line`/`cell` are `null` when unknown), every diagnostic
//!   of every input, also with `-q`.
//!
//! Every input is checked, in the order given, even after one fails. An input that cannot be
//! checked (it cannot be read or parsed, invalid front matter or `presenter.toml`, an unknown
//! extension) is reported as an error-level diagnostic of that input
//! ([`crate::check::error_diagnostic`]); the paths in its message are shown like those of
//! `build`'s error report ([`report::with_display_paths`]).
//!
//! Exit codes (cli.md `check`): 1 when there is an error of any kind (also an unknown
//! extension or an invalid `presenter.toml`, which other commands exit 2 for), or a warning
//! with `--deny-warnings`; otherwise 0. Only clap's own argument errors exit 2.

use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;

use super::{
    CheckArgs, CheckFormat, EXIT_BUILD_ERROR, EXIT_OK, GlobalArgs, display_path, input_kind,
    print_stdout, report, report_diagnostics,
};
use crate::check;
use crate::error::{Diagnostic, Diagnostics, Error, Level};
use crate::pipeline::BuildOptions;

/// Runs `presenter check`.
pub fn run(args: &CheckArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let mut all = Diagnostics::new();
    for input in &args.inputs {
        match check_input(input, args, global) {
            Ok(diagnostics) => all.extend(diagnostics),
            Err(diagnostic) => all.push(diagnostic),
        }
    }
    match args.format {
        CheckFormat::Text => report_diagnostics(&all, &text_report_flags(global, args)),
        CheckFormat::Json => print_stdout(&json_report(&all)?)?,
    }
    Ok(ExitCode::from(exit_code(&all, args.deny_warnings)))
}

/// The flags the text report honours: `global`, except that `-q` does not hide warnings
/// when `--deny-warnings` makes them decide the exit code (module docs).
fn text_report_flags(global: &GlobalArgs, args: &CheckArgs) -> GlobalArgs {
    GlobalArgs {
        quiet: global.quiet && !args.deny_warnings,
        ..global.clone()
    }
}

/// The diagnostics of one input, or the failure that stopped its check as an error
/// diagnostic.
fn check_input(
    input: &Path,
    args: &CheckArgs,
    global: &GlobalArgs,
) -> Result<Diagnostics, Diagnostic> {
    let kind = input_kind(input).map_err(|err| {
        // The message starts with the input's name, which the diagnostic's path already gives.
        let message = err.to_string();
        let prefix = format!("{}: ", input.display());
        let message = message
            .strip_prefix(&prefix)
            .unwrap_or(&message)
            .to_string();
        check::error_diagnostic(&Error::Other(message), input)
    })?;
    let mut opts = BuildOptions {
        config_path: global.config.clone(),
        missing_asset_is_error: false,
        format: Some(kind),
        ..BuildOptions::new(input)
    };
    opts.cli.theme = args.theme.clone();
    check::run(&opts)
        .map_err(|err| check::error_diagnostic(&report::with_display_paths(err), input))
}

/// The exit code for `diagnostics` (see the module docs).
fn exit_code(diagnostics: &Diagnostics, deny_warnings: bool) -> u8 {
    if diagnostics.has_errors() || (deny_warnings && !diagnostics.is_empty()) {
        EXIT_BUILD_ERROR
    } else {
        EXIT_OK
    }
}

/// One entry of the `--format json` array.
#[derive(Debug, Serialize)]
struct JsonDiagnostic<'a> {
    level: Level,
    code: &'a str,
    message: &'a str,
    path: Option<String>,
    line: Option<usize>,
    cell: Option<usize>,
}

impl<'a> From<&'a Diagnostic> for JsonDiagnostic<'a> {
    fn from(diagnostic: &'a Diagnostic) -> Self {
        let span = diagnostic.span.as_ref();
        JsonDiagnostic {
            level: diagnostic.level,
            code: diagnostic.code,
            message: &diagnostic.message,
            path: diagnostic
                .path
                .as_deref()
                .map(|path| display_path(path).display().to_string()),
            line: span.map(|span| span.start_line),
            cell: span.and_then(|span| span.cell),
        }
    }
}

/// `diagnostics` as the pretty-printed `--format json` array.
fn json_report(diagnostics: &Diagnostics) -> anyhow::Result<String> {
    let entries: Vec<JsonDiagnostic<'_>> = diagnostics.iter().map(JsonDiagnostic::from).collect();
    Ok(serde_json::to_string_pretty(&entries)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::SourceSpan;
    use serde_json::json;

    #[test]
    fn exit_codes_follow_cli_md() {
        let none = Diagnostics::new();
        let warned = Diagnostics::from(vec![Diagnostic::warning("empty-slide", "x")]);
        let failed = Diagnostics::from(vec![
            Diagnostic::warning("empty-slide", "x"),
            Diagnostic::error("io-error", "y"),
        ]);
        assert_eq!(exit_code(&none, false), EXIT_OK);
        assert_eq!(exit_code(&none, true), EXIT_OK);
        assert_eq!(exit_code(&warned, false), EXIT_OK);
        assert_eq!(exit_code(&warned, true), EXIT_BUILD_ERROR);
        assert_eq!(exit_code(&failed, false), EXIT_BUILD_ERROR);
        assert_eq!(exit_code(&failed, true), EXIT_BUILD_ERROR);
    }

    #[test]
    fn quiet_hides_warnings_only_when_they_cannot_fail_the_run() {
        let args = |deny_warnings| CheckArgs {
            inputs: vec![],
            format: CheckFormat::Text,
            deny_warnings,
            theme: None,
        };
        let quiet = GlobalArgs {
            quiet: true,
            ..GlobalArgs::default()
        };
        assert!(text_report_flags(&quiet, &args(false)).quiet);
        assert!(!text_report_flags(&quiet, &args(true)).quiet);
        assert!(!text_report_flags(&GlobalArgs::default(), &args(true)).quiet);
        assert!(!text_report_flags(&GlobalArgs::default(), &args(false)).quiet);
    }

    #[test]
    fn config_errors_name_the_file_like_build_does() {
        // tests/cli_build.rs checks the relative form from a chosen directory.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("presenter.toml"), "[deck]\ntheme = 3\n").unwrap();
        std::fs::write(dir.path().join("s.md"), "# A\n").unwrap();
        let args = CheckArgs {
            inputs: vec![],
            format: CheckFormat::Json,
            deny_warnings: false,
            theme: None,
        };
        let deck = dir.path().join("s.md");
        let diagnostic = check_input(&deck, &args, &GlobalArgs::default()).unwrap_err();
        let shown = display_path(&dir.path().join("presenter.toml"));
        assert!(
            diagnostic
                .message
                .starts_with(&format!("config: {}: ", shown.display())),
            "{diagnostic}"
        );
    }

    #[test]
    fn json_entries_flatten_the_span() {
        let cwd = std::env::current_dir().unwrap();
        let diagnostics = Diagnostics::from(vec![
            Diagnostic::warning("missing-asset", "image `x.png` not found")
                .with_path(cwd.join("talk.ipynb"))
                .with_span(SourceSpan {
                    start_line: 2,
                    end_line: 3,
                    cell: Some(5),
                }),
            Diagnostic::error("theme-error", "theme 'x': not found").with_path("/elsewhere/a.md"),
        ]);
        let value: serde_json::Value =
            serde_json::from_str(&json_report(&diagnostics).unwrap()).expect("valid JSON");
        assert_eq!(
            value,
            json!([
                {
                    "level": "warning",
                    "code": "missing-asset",
                    "message": "image `x.png` not found",
                    "path": "talk.ipynb",
                    "line": 2,
                    "cell": 5
                },
                {
                    "level": "error",
                    "code": "theme-error",
                    "message": "theme 'x': not found",
                    "path": "/elsewhere/a.md",
                    "line": null,
                    "cell": null
                }
            ])
        );
        assert_eq!(json_report(&Diagnostics::new()).unwrap(), "[]");
    }

    #[test]
    fn unknown_extensions_are_errors_and_other_inputs_are_still_checked() {
        let dir = tempfile::tempdir().unwrap();
        let deck = dir.path().join("deck.md");
        std::fs::write(&deck, "# One\n\nSome text\n--\n").unwrap();
        let notes = dir.path().join("notes.txt");
        std::fs::write(&notes, "text").unwrap();
        let args = CheckArgs {
            inputs: vec![notes.clone(), deck.clone()],
            format: CheckFormat::Json,
            deny_warnings: false,
            theme: None,
        };
        let global = GlobalArgs::default();
        let diagnostic = check_input(&notes, &args, &global).unwrap_err();
        assert_eq!(diagnostic.level, Level::Error);
        assert_eq!(diagnostic.code, check::codes::ERROR);
        assert!(
            diagnostic
                .message
                .starts_with("cannot tell the input format from the file name"),
            "{diagnostic}"
        );
        assert!(diagnostic.path.as_ref().unwrap().ends_with("notes.txt"));
        let found = check_input(&deck, &args, &global).unwrap();
        let codes: Vec<&str> = found.iter().map(|d| d.code).collect();
        assert_eq!(codes, ["separator-not-recognized"]);
        let missing = dir.path().join("missing.md");
        let diagnostic = check_input(&missing, &args, &global).unwrap_err();
        assert_eq!(diagnostic.code, check::codes::IO_ERROR);
    }
}
