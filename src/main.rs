//! `presenter` command-line entry point.
//!
//! T00 stub: parses `--version` and placeholder `build`/`serve` subcommands. T16 replaces
//! this file with a thin `fn main() -> ExitCode` that calls `presenter::cli::run()`.

use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

/// Exit code for unimplemented commands (cli.md: 2 = usage/config error).
const EXIT_NOT_IMPLEMENTED: u8 = 2;

/// Turn Markdown files and Jupyter notebooks into reveal.js presentations.
#[derive(Debug, Parser)]
#[command(name = "presenter", version, about)]
struct Cli {
    /// The command to run.
    #[command(subcommand)]
    command: Command,
}

/// Placeholder subcommands until T16/T23 implement them.
#[derive(Debug, Subcommand)]
enum Command {
    /// Render a deck to HTML (not implemented yet).
    Build(PlaceholderArgs),
    /// Render, watch and live-reload in the browser (not implemented yet).
    Serve(PlaceholderArgs),
}

/// Accepts any arguments so that the placeholder reports "not implemented" consistently.
#[derive(Debug, Args)]
struct PlaceholderArgs {
    /// Arguments (ignored until the command is implemented).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

/// Runs the parsed command and returns the process exit code.
fn run(cli: &Cli) -> u8 {
    let name = match cli.command {
        Command::Build(_) => "build",
        Command::Serve(_) => "serve",
    };
    eprintln!("presenter {name}: not implemented");
    EXIT_NOT_IMPLEMENTED
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    ExitCode::from(run(&cli))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn version_string() {
        assert_eq!(Cli::command().render_version(), "presenter 0.1.0\n");
    }

    #[test]
    fn placeholder_commands_exit_2() {
        for args in [
            &["presenter", "build", "slides.md", "-o", "out.html"][..],
            &["presenter", "serve", "slides.md"][..],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert_eq!(run(&cli), EXIT_NOT_IMPLEMENTED);
        }
    }
}
