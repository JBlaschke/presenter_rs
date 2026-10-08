//! `presenter check` (cli.md `check`): lints each input without building it
//! ([`crate::check::run`]) and reports what it found.
//!
//! - `--format text` (the default): one `path:line: level: message` line per diagnostic on
//!   stderr ([`report_diagnostics`]: paths relative to the current directory, the level in
//!   color when stderr is a terminal, only errors with `-q`). Nothing on stdout.
//! - `--format json`: one JSON array on stdout, `[{level, code, message, path, line, cell}]`
//!   (`path` as in the text format; `line`/`cell` are `null` when unknown), every diagnostic
//!   of every input, also with `-q`.
//!
//! Every input is checked, in the order given, even after one fails. An input that cannot be
//! checked (it cannot be read or parsed, invalid front matter or `presenter.toml`, an unknown
//! extension) is reported as an error-level diagnostic of that input
//! ([`crate::check::error_diagnostic`]).
//!
//! Exit codes: 2 when an input has an unknown extension or a configuration error (as for
//! every command, [`exit_code_for`]); otherwise 1 when there is an error, or a warning with
//! `--deny-warnings`; otherwise 0.

use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;

use super::{
    CheckArgs, CheckFormat, EXIT_BUILD_ERROR, EXIT_OK, EXIT_USAGE, GlobalArgs, display_path,
    exit_code_for, input_kind, print_stdout, report_diagnostics,
};
use crate::check;
use crate::error::{Diagnostic, Diagnostics, Error, Level};
use crate::pipeline::BuildOptions;

/// Runs `presenter check`.
pub fn run(args: &CheckArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let mut all = Diagnostics::new();
    let mut usage_error = false;
    for input in &args.inputs {
        match check_input(input, args, global) {
            Ok(diagnostics) => all.extend(diagnostics),
            Err((diagnostic, usage)) => {
                usage_error |= usage;
                all.push(diagnostic);
            }
        }
    }
    match args.format {
        CheckFormat::Text => report_diagnostics(&all, global),
        CheckFormat::Json => print_stdout(&json_report(&all)?)?,
    }
    Ok(ExitCode::from(exit_code(
        &all,
        usage_error,
        args.deny_warnings,
    )))
}

/// The diagnostics of one input, or the failure that stopped its check as an error
/// diagnostic plus whether that failure is a usage or configuration error (exit 2).
fn check_input(
    input: &Path,
    args: &CheckArgs,
    global: &GlobalArgs,
) -> Result<Diagnostics, (Diagnostic, bool)> {
    let kind = input_kind(input).map_err(|err| {
        // The message starts with the input's name, which the diagnostic's path already gives.
        let message = err.to_string();
        let prefix = format!("{}: ", input.display());
        let message = message
            .strip_prefix(&prefix)
            .unwrap_or(&message)
            .to_string();
        (check::error_diagnostic(&Error::Other(message), input), true)
    })?;
    let mut opts = BuildOptions {
        config_path: global.config.clone(),
        missing_asset_is_error: false,
        format: Some(kind),
        ..BuildOptions::new(input)
    };
    opts.cli.theme = args.theme.clone();
    check::run(&opts).map_err(|err| {
        let diagnostic = check::error_diagnostic(&err, input);
        let usage = exit_code_for(&anyhow::Error::new(err)) == EXIT_USAGE;
        (diagnostic, usage)
    })
}

/// The exit code for `diagnostics` (see the module docs).
fn exit_code(diagnostics: &Diagnostics, usage_error: bool, deny_warnings: bool) -> u8 {
    if usage_error {
        EXIT_USAGE
    } else if diagnostics.has_errors() || (deny_warnings && !diagnostics.is_empty()) {
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
        assert_eq!(exit_code(&none, false, false), EXIT_OK);
        assert_eq!(exit_code(&none, false, true), EXIT_OK);
        assert_eq!(exit_code(&warned, false, false), EXIT_OK);
        assert_eq!(exit_code(&warned, false, true), EXIT_BUILD_ERROR);
        assert_eq!(exit_code(&failed, false, false), EXIT_BUILD_ERROR);
        assert_eq!(exit_code(&failed, true, false), EXIT_USAGE);
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
    fn unknown_extensions_are_usage_errors_and_other_inputs_are_still_checked() {
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
        let (diagnostic, usage) = check_input(&notes, &args, &global).unwrap_err();
        assert!(usage);
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
        let (diagnostic, usage) = check_input(&missing, &args, &global).unwrap_err();
        assert!(!usage);
        assert_eq!(diagnostic.code, check::codes::IO_ERROR);
    }
}
