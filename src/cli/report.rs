//! How failures and diagnostics are printed on stderr (cli.md): a failed command as
//! `error: outer: inner`, each diagnostic as `path:line: level: message` with paths relative
//! to the current directory, the level in color when stderr is a terminal and `NO_COLOR` is
//! not set.
//!
//! This module is the hook for T44 (diagnostics quality), which owns it: `cli/mod.rs` and
//! the commands print only through [`report_error`] (a failed command, from `cli::run_from`),
//! [`report_diagnostics`] (re-exported as `cli::report_diagnostics`) and
//! [`install_panic_hook`] (called once by `cli::run`), so T44 can add source excerpts and a
//! panic message by replacing the bodies here, without touching `cli/mod.rs` or `main.rs`.
//! The signatures of those three functions are the contract; [`stderr_color`] is also used
//! by the logging setup.

use std::io::{IsTerminal, Write};
use std::path::Path;

use super::{GlobalArgs, relative_to};
use crate::error::{Diagnostic, Diagnostics, Level};

/// Prints a failed command's error chain as `error: outer: inner` on stderr.
pub fn report_error(err: &anyhow::Error) {
    let label = paint(Level::Error, "error", stderr_color());
    let _ = writeln!(std::io::stderr().lock(), "{label}: {err:#}");
}

/// Prints `diagnostics` on stderr, one `path:line: level: message` line each (cli.md),
/// with paths relative to the current directory when they are below it. With `-q` only
/// error-level diagnostics are printed.
pub fn report_diagnostics(diagnostics: &Diagnostics, global: &GlobalArgs) {
    let cwd = std::env::current_dir().ok();
    let color = stderr_color();
    let mut err = std::io::stderr().lock();
    for diagnostic in diagnostics {
        if global.quiet && diagnostic.level != Level::Error {
            continue;
        }
        let line = diagnostic_line(diagnostic, cwd.as_deref(), color);
        let _ = writeln!(err, "{line}");
    }
}

/// Installs presenter's panic hook; `cli::run` calls it once, before parsing arguments.
/// Until T44 replaces this body (a one-line "this is a bug, please report it" message with
/// the location, T44 step 4), the standard hook stays in place.
pub fn install_panic_hook() {}

/// Whether stderr gets ANSI colors: a terminal, and `NO_COLOR` unset or empty.
pub fn stderr_color() -> bool {
    std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
}

/// One diagnostic as printed by [`report_diagnostics`].
fn diagnostic_line(diagnostic: &Diagnostic, cwd: Option<&Path>, color: bool) -> String {
    let mut diagnostic = diagnostic.clone();
    diagnostic.path = diagnostic.path.map(|path| relative_to(&path, cwd));
    let text = diagnostic.to_string();
    if !color {
        return text;
    }
    // Display is `<location>: <level>: <message>`; color the level.
    let tail = format!("{}: {}", diagnostic.level, diagnostic.message);
    match text.strip_suffix(&tail) {
        Some(location) => format!(
            "{location}{}: {}",
            paint(diagnostic.level, &diagnostic.level.to_string(), true),
            diagnostic.message
        ),
        None => text,
    }
}

/// `text` in the color of `level` (bold red / bold yellow) when `color`.
fn paint(level: Level, text: &str, color: bool) -> String {
    if !color {
        return text.to_string();
    }
    let code = match level {
        Level::Error => "1;31",
        Level::Warning => "1;33",
    };
    format!("\x1b[{code}m{text}\x1b[0m")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::SourceSpan;

    #[test]
    fn diagnostics_print_relative_paths_and_color_only_the_level() {
        let diagnostic = Diagnostic::warning("missing-asset", "image `x.png` not found")
            .with_path("/work/talk/slides.md")
            .with_span(SourceSpan {
                start_line: 7,
                end_line: 7,
                cell: None,
            });
        let cwd = Path::new("/work");
        assert_eq!(
            diagnostic_line(&diagnostic, Some(cwd), false),
            "talk/slides.md:7: warning: image `x.png` not found"
        );
        assert_eq!(
            diagnostic_line(&diagnostic, Some(Path::new("/elsewhere")), false),
            "/work/talk/slides.md:7: warning: image `x.png` not found"
        );
        let plain = Diagnostic::error("e", "boom");
        assert_eq!(diagnostic_line(&plain, None, false), "error: boom");
        assert_eq!(
            diagnostic_line(&diagnostic, Some(cwd), true),
            "talk/slides.md:7: \x1b[1;33mwarning\x1b[0m: image `x.png` not found"
        );
        assert_eq!(
            diagnostic_line(&plain, None, true),
            "\x1b[1;31merror\x1b[0m: boom"
        );
    }

    #[test]
    fn notebook_diagnostics_name_the_cell() {
        let diagnostic = Diagnostic::warning("missing-asset", "image `x.png` not found")
            .with_path("/work/talk.ipynb")
            .with_span(SourceSpan {
                start_line: 2,
                end_line: 2,
                cell: Some(5),
            });
        assert_eq!(
            diagnostic_line(&diagnostic, Some(Path::new("/work")), false),
            "talk.ipynb: cell 5, line 2: warning: image `x.png` not found"
        );
    }
}
