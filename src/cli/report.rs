//! How failures, diagnostics and panics are printed on stderr (cli.md "Global behaviour";
//! T44, diagnostics quality).
//!
//! # Errors
//!
//! A failed command ([`report_error`]) says what went wrong, where, and, when the error has a
//! location and its file can be read, shows the offending line (rustc's layout):
//!
//! ```text
//! error: unknown directive 'include'
//!   --> slides.md:41
//!    |
//! 41 | :::include other.md
//!    | ^^^^^^^^^^^^^^^^^^^
//! ```
//!
//! | error | location (`-->`) | excerpt |
//! |---|---|---|
//! | parse, Markdown | `slides.md:41` | the line |
//! | parse, notebook cell | `talk.ipynb: cell 3, line 2` | the line of the cell's source |
//! | parse, notebook JSON | `talk.ipynb:7:12` | the JSON line, `^` at the column |
//! | missing asset | `slides.md:12` (or the cell) | the line, the reference underlined |
//! | front matter (YAML) | `slides.md:2:9` (or the cell) | the line, `^` at the column |
//! | template | in the message: `template 'layouts/title.html' line 4: …` | minijinja's own excerpt when it captured the source (see below) |
//! | `theme.toml`, `presenter.toml`, SCSS | in the message | the `toml`/`grass` excerpt in the message |
//!
//! The location comes from the error ([`Error::path`], [`Error::span`]); for a missing
//! asset or a front matter error, which do not name the deck source, from the context that
//! [`super::locate`] adds. Template errors carry minijinja's debug info (the source around
//! the line, the referenced variables) in serve mode and for syntax errors converted with
//! `?` ([`crate::error::template_error`]); `build` renders without it, so a template error
//! there names the template and line only. Paths are relative to the current directory when
//! they are below it. Errors that are not library errors print `error: outer: inner`.
//!
//! [`print_error`] prints one library error the same way (`presenter serve` can pass a
//! source cache that reads templates through the theme chain), [`format_error`] returns the
//! text.
//!
//! # Colors
//!
//! Labels, the message, the gutter and the underline are colored when stderr is a terminal,
//! or `CLICOLOR_FORCE` is set (non-empty and not `0`); `NO_COLOR` (non-empty) turns colors
//! off in every case ([`stderr_color`], also used for log lines). cli.md has no `--no-color`
//! flag, so there is none: use `NO_COLOR=1`.
//!
//! # Panics
//!
//! [`install_panic_hook`] replaces the standard panic message with a bug-report line that
//! names the location; `RUST_BACKTRACE` still gives the standard message and backtrace.
//!
//! # Contract with `cli/mod.rs`
//!
//! `cli/mod.rs` and the commands print only through [`report_error`] (a failed command,
//! from `cli::run_from`), [`report_diagnostics`] (re-exported as `cli::report_diagnostics`)
//! and [`install_panic_hook`] (called once by `cli::run`). The signatures of those three
//! functions are the contract; [`stderr_color`] is also used by the logging setup.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Once;

use super::{GlobalArgs, relative_to};
use crate::error::{Diagnostic, Diagnostics, Error, Level};
use crate::ir::SourceSpan;

/// A source cache for [`print_error`] and [`format_error`]: the text of a file, or `None`.
pub type SourceCache<'a> = &'a dyn Fn(&Path) -> Option<String>;

/// SGR codes: the message (bold), the gutter and `-->` (bold blue), the underline (bold red).
const BOLD: &str = "1";
const GUTTER: &str = "1;34";
const UNDERLINE: &str = "1;31";

/// Prints a failed command's error on stderr: a library error ([`Error`], found anywhere in
/// the chain) with its location and source excerpt (module docs), anything else as
/// `error: outer: inner`.
pub fn report_error(err: &anyhow::Error) {
    write_stderr(&format_error_chain(err, &read_source, stderr_color()));
}

/// Prints `err` on stderr with its location and the offending line from `source_cache`
/// (module docs), in color per [`stderr_color`].
pub fn print_error(err: &Error, source_cache: SourceCache<'_>) {
    write_stderr(&format_error(err, None, source_cache, stderr_color()));
}

/// The text [`print_error`] prints (without the final line break), with ANSI colors when
/// `color`. `input` is the deck source, for the errors that do not name it themselves (a
/// missing asset, front matter); `source_cache` gives the text of a file to quote.
pub fn format_error(
    err: &Error,
    input: Option<&Path>,
    source_cache: SourceCache<'_>,
    color: bool,
) -> String {
    let cwd = std::env::current_dir().ok();
    Report::new(err, input, cwd.as_deref()).render(source_cache, color)
}

/// The CLI's source cache: the file's text, `None` when it cannot be read as UTF-8.
pub fn read_source(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
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

/// Installs presenter's panic hook; `cli::run` calls it before parsing arguments (a second
/// call does nothing). A panic prints
///
/// ```text
/// error: internal error at src/render/html.rs:42:9: <panic message>
/// note: this is a bug in presenter 0.1.0; please report it with the command line you ran (RUST_BACKTRACE=1 shows a backtrace)
/// ```
///
/// instead of the standard message. With `RUST_BACKTRACE` set (and not `0`) the standard
/// hook runs after these lines, printing its message and the backtrace as usual.
pub fn install_panic_hook() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let standard = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let backtrace = backtrace_requested(std::env::var_os("RUST_BACKTRACE").as_deref());
            let location = info
                .location()
                .map(|at| format!("{}:{}:{}", at.file(), at.line(), at.column()));
            let payload = info.payload();
            let what = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str));
            let text = panic_message(location.as_deref(), what, backtrace, stderr_color());
            write_stderr(&text);
            if backtrace {
                standard(info);
            }
        }));
    });
}

/// Whether stderr gets ANSI colors: `NO_COLOR` unset or empty, and stderr a terminal or
/// `CLICOLOR_FORCE` set (non-empty, not `0`).
pub fn stderr_color() -> bool {
    use_color(
        std::env::var_os("NO_COLOR").as_deref(),
        std::env::var_os("CLICOLOR_FORCE").as_deref(),
        || std::io::stderr().is_terminal(),
    )
}

/// [`stderr_color`] for the given variables: `NO_COLOR` wins over `CLICOLOR_FORCE`, which
/// wins over the terminal test.
fn use_color(
    no_color: Option<&OsStr>,
    force: Option<&OsStr>,
    terminal: impl FnOnce() -> bool,
) -> bool {
    if no_color.is_some_and(|value| !value.is_empty()) {
        return false;
    }
    force.is_some_and(|value| !value.is_empty() && value != "0") || terminal()
}

/// Whether `RUST_BACKTRACE` (its value) asks for a backtrace, as the standard hook reads it.
fn backtrace_requested(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| !value.is_empty() && value != "0")
}

/// The two lines the panic hook prints for a panic at `location` with message `payload`.
fn panic_message(
    location: Option<&str>,
    payload: Option<&str>,
    backtrace: bool,
    color: bool,
) -> String {
    let at = location.map(|at| format!(" at {at}")).unwrap_or_default();
    // The standard hook's text for a payload that is not a string.
    let what = payload.unwrap_or("Box<dyn Any>");
    let hint = if backtrace {
        ""
    } else {
        " (RUST_BACKTRACE=1 shows a backtrace)"
    };
    format!(
        "{}: {}\n{}: this is a bug in presenter {}; please report it with the command line \
         you ran{hint}",
        paint(Level::Error, "error", color),
        style(BOLD, &format!("internal error{at}: {what}"), color),
        style(BOLD, "note", color),
        super::version(),
    )
}

/// Writes `text` and a line break on stderr. Nothing sensible is left to do when that fails.
fn write_stderr(text: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{text}");
}

/// The report of `err` (see [`report_error`]): the first library [`Error`] in the chain
/// with its location; the contexts outside it in front of its message, except the
/// location [`super::locate`] added (it names the deck source, which the excerpt reads);
/// its causes after it.
fn format_error_chain(err: &anyhow::Error, source_cache: SourceCache<'_>, color: bool) -> String {
    let chain: Vec<&(dyn std::error::Error + 'static)> = err.chain().collect();
    let found = chain
        .iter()
        .enumerate()
        .find_map(|(at, cause)| Some((at, cause.downcast_ref::<Error>()?)));
    let Some((at, library)) = found else {
        return Report::message(format!("{err:#}")).render(source_cache, color);
    };
    let mut outer: Vec<String> = chain[..at].iter().map(ToString::to_string).collect();
    let input = outer
        .last()
        .and_then(|context| located_input(library, context));
    if input.is_some() {
        outer.pop();
    }
    let cwd = std::env::current_dir().ok();
    let mut report = Report::new(library, input.as_deref(), cwd.as_deref());
    report.outer = outer;
    report.inner = chain[at + 1..].iter().map(ToString::to_string).collect();
    report.render(source_cache, color)
}

/// The deck source named by `context` when it is the location [`super::locate`] added to
/// `err` (`input:line`, `input: cell C, line L`, or `input`) and names a file.
fn located_input(err: &Error, context: &str) -> Option<PathBuf> {
    let input = match err {
        Error::MissingAsset {
            span: Some(span), ..
        } => context.strip_suffix(&match span.cell {
            Some(cell) => format!(": cell {cell}, line {}", span.start_line),
            None => format!(":{}", span.start_line),
        })?,
        Error::MissingAsset { span: None, .. } | Error::FrontMatter(_) => context,
        _ => return None,
    };
    Some(PathBuf::from(input)).filter(|path| path.is_file())
}

/// One error as printed: `error: <outer>: <message>: <inner>`, its location and excerpt,
/// then the further lines of the message.
struct Report<'a> {
    /// Contexts around the error, outermost first.
    outer: Vec<String>,
    /// The error's message; lines after the first are printed after the excerpt as they are.
    message: String,
    /// The error's causes, outermost first.
    inner: Vec<String>,
    /// Where the error is, when it says.
    location: Option<Location<'a>>,
}

/// The `-->` line of a [`Report`] and what its excerpt needs.
struct Location<'a> {
    /// The error, for [`Error::excerpt`].
    err: &'a Error,
    /// The text after `-->`.
    shown: String,
    /// The file the error's span is in (to quote), as given.
    file: Option<PathBuf>,
    /// The span's line, for the gutter width when there is no excerpt.
    line: Option<usize>,
}

impl<'a> Report<'a> {
    /// A report of a message without a location.
    fn message(message: String) -> Self {
        Report {
            outer: Vec::new(),
            message,
            inner: Vec::new(),
            location: None,
        }
    }

    /// The report of `err`; `input` is the deck source (for a missing asset or front
    /// matter), paths are shown relative to `cwd`.
    fn new(err: &'a Error, input: Option<&Path>, cwd: Option<&Path>) -> Self {
        let message = match err {
            Error::MissingAsset { path, .. } => {
                format!("asset not found: {}", relative_to(path, cwd).display())
            }
            Error::Theme { name, message } => {
                format!("theme '{name}': {}", relative_lead(message, cwd))
            }
            Error::Config(message) => format!("config: {}", relative_lead(message, cwd)),
            _ => err.message().into_owned(),
        };
        let file = match err {
            Error::Parse { .. } => err.path().map(Path::to_path_buf),
            Error::MissingAsset { .. } | Error::FrontMatter(_) => input.map(Path::to_path_buf),
            _ => None,
        };
        // A template's name and line are in its message; the other variants have no span.
        let span = match err {
            Error::Template { .. } => None,
            _ => err.span(),
        };
        let location = (file.is_some() || span.is_some()).then(|| Location {
            shown: location_text(
                file.as_deref()
                    .map(|file| relative_to(file, cwd))
                    .as_deref(),
                span.as_ref(),
                err.column(),
            ),
            err,
            file,
            line: span.as_ref().map(|span| span.start_line),
        });
        Report {
            location,
            ..Report::message(message)
        }
    }

    /// The report's text, without the final line break.
    fn render(&self, source_cache: SourceCache<'_>, color: bool) -> String {
        let mut lines = self.message.lines();
        let mut headline = String::new();
        for context in &self.outer {
            headline.push_str(context);
            headline.push_str(": ");
        }
        headline.push_str(lines.next().unwrap_or_default());
        for cause in &self.inner {
            headline.push_str(": ");
            headline.push_str(cause);
        }
        let mut out = format!(
            "{}: {}\n",
            paint(Level::Error, "error", color),
            style(BOLD, &headline, color)
        );
        if let Some(location) = &self.location {
            location.render(&mut out, source_cache, color);
        }
        for line in lines {
            out.push_str(line);
            out.push('\n');
        }
        out.pop();
        out
    }
}

impl Location<'_> {
    /// Appends ` --> location` and, when the file has the line, the excerpt.
    fn render(&self, out: &mut String, source_cache: SourceCache<'_>, color: bool) {
        let excerpt = self
            .file
            .as_deref()
            .and_then(|file| self.err.excerpt(file, source_cache));
        let number = excerpt
            .as_ref()
            .map(|excerpt| excerpt.line)
            .or(self.line)
            .map_or_else(String::new, |line| line.to_string());
        let pad = " ".repeat(number.len().max(1));
        // Writing to a String cannot fail.
        let _ = writeln!(out, "{pad}{} {}", style(GUTTER, "-->", color), self.shown);
        let Some(excerpt) = excerpt else {
            return;
        };
        let bar = style(GUTTER, "|", color);
        let _ = writeln!(out, "{pad} {bar}");
        let number = style(GUTTER, &number, color);
        let _ = match excerpt.text.as_str() {
            "" => writeln!(out, "{number} {bar}"),
            text => writeln!(out, "{number} {bar} {text}"),
        };
        if let Some((start, width)) = excerpt.marker {
            let marker = style(UNDERLINE, &"^".repeat(width), color);
            let _ = writeln!(out, "{pad} {bar} {}{marker}", " ".repeat(start));
        }
    }
}

/// `message` with the absolute path it starts with (a theme or config error names its
/// `theme.toml` or `presenter.toml` first) relative to `cwd` when it is below it.
fn relative_lead<'m>(message: &'m str, cwd: Option<&Path>) -> &'m str {
    let Some(cwd) = cwd else {
        return message;
    };
    let prefix = format!("{}{}", cwd.display(), std::path::MAIN_SEPARATOR);
    message.strip_prefix(&prefix).unwrap_or(message)
}

/// The text after `-->`: `file:line[:column]` for a file, `file: cell C, line L[, column N]`
/// for a notebook cell, without a file `line L[, column N]` (or the cell form), without a
/// span the file alone.
fn location_text(file: Option<&Path>, span: Option<&SourceSpan>, column: Option<usize>) -> String {
    let Some(span) = span else {
        return file
            .map(|file| file.display().to_string())
            .unwrap_or_default();
    };
    let line = span.start_line;
    if let (Some(file), None) = (file, span.cell) {
        return match column {
            Some(column) => format!("{}:{line}:{column}", file.display()),
            None => format!("{}:{line}", file.display()),
        };
    }
    let mut parts = Vec::new();
    if let Some(cell) = span.cell {
        parts.push(format!("cell {cell}"));
    }
    parts.push(format!("line {line}"));
    if let Some(column) = column {
        parts.push(format!("column {column}"));
    }
    match file {
        Some(file) => format!("{}: {}", file.display(), parts.join(", ")),
        None => parts.join(", "),
    }
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
    let code = match level {
        Level::Error => "1;31",
        Level::Warning => "1;33",
    };
    style(code, text, color)
}

/// `text` with the SGR `code` when `color` (unchanged otherwise, and when empty).
fn style(code: &str, text: &str, color: bool) -> String {
    if !color || text.is_empty() {
        return text.to_string();
    }
    format!("\x1b[{code}m{text}\x1b[0m")
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A source cache with one file.
    fn one_file<'a>(name: &'a str, text: &'a str) -> impl Fn(&Path) -> Option<String> + 'a {
        move |path: &Path| (path == Path::new(name)).then(|| text.to_string())
    }

    fn span(line: usize, cell: Option<usize>) -> SourceSpan {
        SourceSpan {
            start_line: line,
            end_line: line,
            cell,
        }
    }

    #[test]
    fn the_layout_follows_rustc() {
        let err = Error::Parse {
            path: "slides.md".into(),
            line: Some(41),
            message: "unknown directive 'include'".into(),
        };
        let mut text = "x\n".repeat(40);
        text.push_str(":::include other.md\n");
        let cache = one_file("slides.md", &text);
        assert_eq!(
            format_error(&err, None, &cache, false),
            "error: unknown directive 'include'\n  --> slides.md:41\n   |\n\
             41 | :::include other.md\n   | ^^^^^^^^^^^^^^^^^^^"
        );
        // Without the file: the location only.
        assert_eq!(
            format_error(&err, None, &|_| None, false),
            "error: unknown directive 'include'\n  --> slides.md:41"
        );
        // In color: the label, the message, the gutter and the underline.
        let colored = format_error(&err, None, &cache, true);
        assert!(
            colored.starts_with(
                "\x1b[1;31merror\x1b[0m: \x1b[1munknown directive 'include'\x1b[0m\n  \
                 \x1b[1;34m-->\x1b[0m slides.md:41\n"
            ),
            "{colored:?}"
        );
        assert!(colored.ends_with("\x1b[1;31m^^^^^^^^^^^^^^^^^^^\x1b[0m"));
    }

    #[test]
    fn located_errors_without_a_line_or_a_file() {
        let no_line = Error::Parse {
            path: "talk.ipynb".into(),
            line: None,
            message: "nbformat 4 required".into(),
        };
        assert_eq!(
            format_error(&no_line, None, &|_| None, false),
            "error: nbformat 4 required\n --> talk.ipynb"
        );
        let missing = Error::MissingAsset {
            path: "x.png".into(),
            span: Some(span(3, Some(4))),
        };
        assert_eq!(
            format_error(&missing, None, &|_| None, false),
            "error: asset not found: x.png\n --> cell 4, line 3"
        );
        let front = Error::FrontMatter("bad at line 2 column 9".into());
        assert_eq!(
            format_error(&front, None, &|_| None, false),
            "error: front matter: bad at line 2 column 9\n --> line 2, column 9"
        );
        let other = Error::Other("plain".into());
        assert_eq!(format_error(&other, None, &|_| None, false), "error: plain");
    }

    #[test]
    fn chains_keep_their_contexts_and_drop_locate_s() {
        let tmp = tempfile::tempdir().unwrap();
        let deck = tmp.path().join("deck.md");
        std::fs::write(&deck, "# One\n\n![x](img/nope.png)\n").unwrap();
        let missing = || Error::MissingAsset {
            path: tmp.path().join("img/nope.png"),
            span: Some(span(3, None)),
        };
        // `locate`'s context names the deck: it becomes the location.
        let located = super::super::locate(missing(), &deck);
        let text = format_error_chain(&located, &read_source, false);
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines[0].starts_with("error: asset not found: ") && lines[0].ends_with("nope.png"),
            "{text}"
        );
        assert_eq!(lines[1], format!(" --> {}:3", deck.display()));
        assert_eq!(
            &lines[2..],
            ["  |", "3 | ![x](img/nope.png)", "  |      ^^^^^^^^^^^^"]
        );
        // Any other context stays in front of the message.
        let wrapped = anyhow::Error::new(missing()).context("loading slides");
        let text = format_error_chain(&wrapped, &read_source, false);
        assert!(text.starts_with("error: loading slides: asset not found: "));
        assert!(text.ends_with(" --> line 3"), "{text}");
        // A context that only looks like a location of a file that does not exist.
        let fake = anyhow::Error::new(missing()).context("nowhere.md:3");
        let text = format_error_chain(&fake, &read_source, false);
        assert!(
            text.starts_with("error: nowhere.md:3: asset not found: "),
            "{text}"
        );
        // Front matter: `locate` names the deck alone.
        std::fs::write(&deck, "---\ntitle: a: b\n---\n").unwrap();
        let front = Error::FrontMatter("mapping values are not allowed at line 2 column 9".into());
        let text = format_error_chain(&super::super::locate(front, &deck), &read_source, false);
        assert!(
            text.ends_with(":2:9\n  |\n2 | title: a: b\n  |         ^"),
            "{text}"
        );
        // Causes follow; errors that are not library errors print as before.
        let io = anyhow::Error::new(Error::Io(std::io::Error::other("disk on fire")));
        assert_eq!(
            format_error_chain(&io.context("writing out.html"), &read_source, false),
            "error: writing out.html: disk on fire"
        );
        let plain = anyhow::anyhow!("inner").context("outer");
        assert_eq!(
            format_error_chain(&plain, &read_source, false),
            "error: outer: inner"
        );
    }

    #[test]
    fn multi_line_messages_follow_the_location() {
        let err = Error::Theme {
            name: "t".into(),
            message: "t/theme.toml: TOML parse error at line 1, column 1\n  |\n1 | x\n  | ^\nboom"
                .into(),
        };
        assert_eq!(
            format_error(&err, None, &|_| None, false),
            "error: theme 't': t/theme.toml: TOML parse error at line 1, column 1\n  |\n\
             1 | x\n  | ^\nboom"
        );
    }

    #[test]
    fn theme_and_config_errors_name_their_file_relative_to_the_current_directory() {
        let err = Error::Theme {
            name: "./t".into(),
            message: "/work/talk/t/theme.toml: missing required key 'name'".into(),
        };
        let report = Report::new(&err, None, Some(Path::new("/work")));
        assert_eq!(
            report.message,
            "theme './t': talk/t/theme.toml: missing required key 'name'"
        );
        let elsewhere = Report::new(&err, None, Some(Path::new("/else")));
        assert_eq!(elsewhere.message, err.to_string());
        let config = Error::Config("/work/presenter.toml: TOML parse error".into());
        let report = Report::new(&config, None, Some(Path::new("/work")));
        assert_eq!(report.message, "config: presenter.toml: TOML parse error");
    }

    #[test]
    fn colors_follow_no_color_clicolor_force_and_the_terminal() {
        let os = |value: &'static str| Some(OsStr::new(value));
        assert!(use_color(None, None, || true));
        assert!(!use_color(None, None, || false));
        assert!(!use_color(os("1"), None, || true));
        assert!(
            use_color(os(""), None, || true),
            "an empty NO_COLOR is unset"
        );
        assert!(use_color(None, os("1"), || false));
        assert!(!use_color(None, os("0"), || false));
        assert!(!use_color(None, os(""), || false));
        assert!(!use_color(os("1"), os("1"), || true), "NO_COLOR wins");
    }

    #[test]
    fn panic_messages_name_the_location_and_ask_for_a_report() {
        let version = super::super::version();
        assert_eq!(
            panic_message(Some("src/x.rs:3:5"), Some("boom"), false, false),
            format!(
                "error: internal error at src/x.rs:3:5: boom\nnote: this is a bug in presenter \
                 {version}; please report it with the command line you ran \
                 (RUST_BACKTRACE=1 shows a backtrace)"
            )
        );
        let with_backtrace = panic_message(None, None, true, false);
        assert!(
            with_backtrace.starts_with("error: internal error: Box<dyn Any>\nnote: "),
            "{with_backtrace}"
        );
        assert!(with_backtrace.ends_with("with the command line you ran"));
        assert!(panic_message(None, Some("x"), false, true).starts_with("\x1b[1;31merror"));
        let os = |value: &'static str| Some(OsStr::new(value));
        assert!(backtrace_requested(os("1")));
        assert!(backtrace_requested(os("full")));
        assert!(!backtrace_requested(os("0")));
        assert!(!backtrace_requested(os("")));
        assert!(!backtrace_requested(None));
    }
}
