//! `presenter` command-line entry point: everything happens in [`presenter::cli::run`],
//! except in a Sass worker, the process presenter starts to compile a theme stylesheet in
//! (`presenter::theme::styles`, "Isolation"; live-reload.md §4).

use std::process::ExitCode;

use presenter::theme::styles;

fn main() -> ExitCode {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == styles::WORKER_ARG)
    {
        return styles::run_worker();
    }
    // Without its own path, presenter compiles Sass in this process instead.
    if let Ok(exe) = std::env::current_exe() {
        styles::isolate_in_worker(exe);
    }
    presenter::cli::run()
}
