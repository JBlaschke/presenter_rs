//! The Sass worker process (module docs of [`super`], "Isolation"; live-reload.md §4, audit
//! F02/F06): presenter compiles a Sass file in a child process, the presenter binary itself
//! started with [`WORKER_ARG`], because grass can neither be stopped nor limited. A stack
//! overflow aborts only the worker, and a compilation that does not finish within its time
//! limit is ended by killing the worker, which frees the CPU and memory it used.
//!
//! The binary opts in ([`isolate_in_worker`], from `main`); a library user (a test binary
//! is no presenter) compiles on a thread of its own process unless it names a worker
//! ([`with_worker`]).
//!
//! **Protocol.** The caller writes one JSON request to the worker's stdin (the theme
//! chain's names and locations, the file's key and text, the time limit) and closes it.
//! The worker answers with JSON lines on stdout, as they happen: each file grass reads (its
//! key and BLAKE3 hash, for the caller's memory of failed compilations), grass's `@debug`
//! and `@warn` output (logged by the caller with `tracing`), and last the CSS or the
//! error. A worker that ends without that last line crashed: its stderr tells whether its
//! stack overflowed. A worker whose caller is gone ends itself, at the latest
//! [`WORKER_GRACE`] after the time limit.

use std::cell::RefCell;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{
    Isolation, Outcome, Report, Reporter, SASS_STACK_SIZE, VirtualFs, compile_sass, overflow_error,
    style_message, timeout_error, timeout_message,
};
use crate::error::Error;
use crate::theme::{Theme, ThemeDir, ThemeManifest, ThemeSource};

/// The first argument that makes `presenter` a Sass worker: `main` then calls
/// [`run_worker`] instead of the command line interface.
pub const WORKER_ARG: &str = "__presenter-sass-worker";

/// How long a worker outlives its time limit before it ends itself: its caller kills it at
/// the limit, so this only ends a worker whose caller is gone (killed, crashed).
pub const WORKER_GRACE: Duration = Duration::from_secs(5);

/// The exit status of a worker that could not read its request.
const EXIT_BAD_REQUEST: u8 = 2;
/// The exit status of a worker that outlived its time limit by [`WORKER_GRACE`].
const EXIT_EXPIRED: u8 = 3;
/// The exit status of a worker whose caller stopped reading its answer.
const EXIT_ORPHANED: u8 = 4;

/// The worker of this process ([`isolate_in_worker`]).
static WORKER: OnceLock<PathBuf> = OnceLock::new();

thread_local! {
    /// The worker of the compilations started on this thread ([`with_worker`]).
    static THREAD_WORKER: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Makes every Sass compilation of this process run in a worker process: `exe` (the
/// presenter binary, `std::env::current_exe()`) started with [`WORKER_ARG`] (module docs).
/// `main` calls it once at start-up; later calls change nothing.
pub fn isolate_in_worker(exe: PathBuf) {
    let _ = WORKER.set(exe);
}

/// Runs `f` with the Sass compilations started on this thread running in the worker `exe`
/// (a presenter binary; module docs), whatever [`isolate_in_worker`] said; for tests and
/// other library users, whose own executable is no presenter.
pub fn with_worker<R>(exe: &Path, f: impl FnOnce() -> R) -> R {
    /// Restores the previous worker, also when `f` panics.
    struct Restore(Option<PathBuf>);
    impl Drop for Restore {
        fn drop(&mut self) {
            THREAD_WORKER.set(self.0.take());
        }
    }
    let _restore = Restore(THREAD_WORKER.replace(Some(exe.to_path_buf())));
    f()
}

/// The worker for a compilation started on this thread, if there is one.
pub(super) fn configured() -> Option<PathBuf> {
    THREAD_WORKER
        .with_borrow(Clone::clone)
        .or_else(|| WORKER.get().cloned())
}

/// What the caller asks a worker to compile.
#[derive(Debug, Serialize, Deserialize)]
struct Request {
    /// The theme chain, child first.
    chain: Vec<ChainDir>,
    /// The canonical key of the file to compile.
    key: String,
    /// Its text.
    source: String,
    /// The caller's time limit, in milliseconds.
    limit_ms: u64,
}

/// One theme of the chain: its name and where its files are (one of `dir` and `prefix`).
#[derive(Debug, Serialize, Deserialize)]
struct ChainDir {
    name: String,
    /// A theme directory on disk.
    dir: Option<PathBuf>,
    /// A built-in theme's prefix in the embedded tree.
    prefix: Option<String>,
}

/// One line of a worker's answer (module docs, "Protocol").
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Message {
    /// grass read the file `key`, whose contents have the BLAKE3 hash `hash` (hex).
    Read { key: String, hash: String },
    /// A `@debug` line, as it is logged.
    Debug(String),
    /// A `@warn` line, as it is logged.
    Warn(String),
    /// The compiled CSS: the last line.
    Css(String),
    /// The compilation's [`Error::Style`]: the last line.
    Error { file: String, message: String },
}

impl Request {
    fn new(theme: &Theme, key: &str, source: &str, limit: Duration) -> Request {
        let chain = theme.chain.iter().map(|dir| {
            let (path, prefix) = match &dir.source {
                ThemeSource::Disk { dir } => (Some(dir.clone()), None),
                ThemeSource::Embedded { prefix } => (None, Some(prefix.clone())),
            };
            ChainDir {
                name: dir.name.clone(),
                dir: path,
                prefix,
            }
        });
        Request {
            chain: chain.collect(),
            key: key.to_string(),
            source: source.to_string(),
            limit_ms: u64::try_from(limit.as_millis()).unwrap_or(u64::MAX),
        }
    }

    /// The theme the request names: its chain's names and locations (the [`VirtualFs`]
    /// needs nothing else).
    fn theme(&self) -> Theme {
        let chain = self.chain.iter().map(|dir| ThemeDir {
            name: dir.name.clone(),
            source: match (&dir.dir, &dir.prefix) {
                (Some(path), _) => ThemeSource::Disk { dir: path.clone() },
                (None, prefix) => ThemeSource::Embedded {
                    prefix: prefix.clone().unwrap_or_default(),
                },
            },
            manifest: ThemeManifest::default(),
        });
        Theme {
            chain: chain.collect(),
        }
    }
}

/// The worker's `main` (module docs): reads the request from stdin, compiles it on a
/// thread with a [`super::SASS_STACK_SIZE`] stack and writes the answer to stdout.
pub fn run_worker() -> ExitCode {
    let mut input = String::new();
    let request = io::stdin()
        .read_to_string(&mut input)
        .ok()
        .and_then(|_| serde_json::from_str::<Request>(&input).ok());
    let Some(request) = request else {
        return ExitCode::from(EXIT_BAD_REQUEST);
    };
    // The caller kills this process at the limit; if the caller is gone, this ends it.
    let expires = Duration::from_millis(request.limit_ms).saturating_add(WORKER_GRACE);
    std::thread::spawn(move || {
        std::thread::sleep(expires);
        std::process::exit(i32::from(EXIT_EXPIRED));
    });
    let compiling = std::thread::Builder::new()
        .name("presenter-sass".into())
        .stack_size(SASS_STACK_SIZE)
        .spawn(move || compile_request(&request));
    // A compilation ends with its result line; a panic was caught as an error already.
    match compiling.map(JoinHandle::join) {
        Ok(Ok(())) => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

/// Compiles `request` in the worker, writing the answer's lines.
fn compile_request(request: &Request) {
    let theme = request.theme();
    let report = Reporter::new(|report| {
        send(&match report {
            Report::Read(key, hash) => Message::Read {
                key,
                hash: hash.to_hex().to_string(),
            },
            Report::Debug(text) => Message::Debug(text),
            Report::Warn(text) => Message::Warn(text),
        });
    });
    let fs = VirtualFs::new(&theme)
        .with_file(&request.key, request.source.as_bytes())
        .reporting(report);
    send(&match compile_sass(&fs, &request.key) {
        Ok(css) => Message::Css(css),
        Err(Error::Style { file, message }) => Message::Error { file, message },
        Err(other) => Message::Error {
            file: request.key.clone(),
            message: other.to_string(),
        },
    });
}

/// Writes `message` as one line to stdout; ends the worker when nobody reads it.
fn send(message: &Message) {
    let written = serde_json::to_string(message).map(|line| {
        let mut out = io::stdout().lock();
        writeln!(out, "{line}").and_then(|()| out.flush())
    });
    if !matches!(written, Ok(Ok(()))) {
        std::process::exit(i32::from(EXIT_ORPHANED));
    }
}

/// Compiles `source`, the Sass file `key` of `theme`, in the worker `exe` within `limit`
/// (module docs). `Err` when the worker cannot be used (it cannot be started, or a theme
/// path is not UTF-8): the caller then compiles without one.
pub(super) fn compile(
    exe: &Path,
    theme: &Theme,
    key: &str,
    source: &str,
    limit: Duration,
) -> io::Result<Outcome> {
    let request = serde_json::to_vec(&Request::new(theme, key, source, limit))
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let mut child = Command::new(exe)
        .arg(WORKER_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + limit;
    let pipes = Pipes::open(&mut child, request);
    let mut reads = Vec::new();
    let mut result = None;
    while result.is_none() {
        let left = deadline.saturating_duration_since(Instant::now());
        match pipes.messages.recv_timeout(left) {
            Ok(Message::Read { key, hash }) => {
                if let Ok(hash) = blake3::Hash::from_hex(&hash) {
                    reads.push((key, hash));
                }
            }
            Ok(Message::Debug(text)) => tracing::debug!("{text}"),
            Ok(Message::Warn(text)) => tracing::warn!("{text}"),
            Ok(Message::Css(css)) => result = Some(Ok(css)),
            Ok(Message::Error { file, message }) => {
                result = Some(Err(Error::Style { file, message }))
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                let _ = child.wait();
                pipes.close();
                return Ok(Outcome::Abandoned {
                    error: timeout_error(key, limit, Isolation::Worker),
                    reads,
                    again: Some(timeout_message(limit, Isolation::Worker, true)),
                });
            }
        }
    }
    // The worker has answered or closed its stdout: it is ending.
    let status = child.wait();
    let stderr = pipes.close();
    Ok(match (result, status) {
        (Some(result), _) => Outcome::Finished(result),
        (None, Ok(status)) => crashed(theme, key, source, reads, status, &stderr),
        (None, Err(err)) => Outcome::Finished(Err(Error::Style {
            file: key.to_string(),
            message: format!("the Sass worker process ended without an answer: {err}"),
        })),
    })
}

/// The pipes of a running worker: a thread writes the request to its stdin, one reads its
/// answer's lines into `messages`, one collects its stderr.
struct Pipes {
    messages: mpsc::Receiver<Message>,
    threads: [Option<JoinHandle<String>>; 3],
}

impl Pipes {
    fn open(child: &mut Child, request: Vec<u8>) -> Pipes {
        let (tx, messages) = mpsc::channel();
        let stdin = child.stdin.take().map(|mut stdin| {
            // An error means that the worker ended early: its status tells why.
            std::thread::spawn(move || {
                let _ = stdin.write_all(&request);
                String::new()
            })
        });
        let stdout = child.stdout.take().map(|stdout| {
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    // Anything else on stdout is not the worker's.
                    let Ok(message) = serde_json::from_str(&line) else {
                        continue;
                    };
                    if tx.send(message).is_err() {
                        break;
                    }
                }
                String::new()
            })
        });
        let stderr = child.stderr.take().map(|mut stderr| {
            std::thread::spawn(move || {
                let mut text = String::new();
                let _ = stderr.read_to_string(&mut text);
                text
            })
        });
        Pipes {
            messages,
            threads: [stdin, stdout, stderr],
        }
    }

    /// Waits for the threads (the worker has ended) and returns its stderr.
    fn close(self) -> String {
        let mut stderr = String::new();
        for thread in self.threads.into_iter().flatten() {
            stderr.push_str(&thread.join().unwrap_or_default());
        }
        stderr
    }
}

/// The outcome of a worker that ended without its result (`status`, `stderr`) after
/// reading `reads`: a stack overflow is explained by the recursion
/// ([`overflow_error`]) and remembered (it would overflow again); any other end is
/// reported as it is.
fn crashed(
    theme: &Theme,
    key: &str,
    source: &str,
    reads: Vec<(String, blake3::Hash)>,
    status: ExitStatus,
    stderr: &str,
) -> Outcome {
    let overflow = stderr.contains("has overflowed its stack");
    let error = match overflow {
        true => overflow_error(theme, key, source, &reads).unwrap_or_else(|| Error::Style {
            file: key.to_string(),
            message: "the Sass compiler ran out of stack, which a recursion that never ends \
                      does: a @mixin that includes itself or a @function that calls itself \
                      (maybe through others, in several files) whose end condition is never \
                      met"
            .into(),
        }),
        false => Error::Style {
            file: key.to_string(),
            message: format!(
                "the Sass compiler (grass) ended unexpectedly ({}){}",
                describe(status),
                last_line(stderr).map_or_else(String::new, |line| format!(": {line}"))
            ),
        },
    };
    let again = overflow.then(|| style_message(&error));
    Outcome::Abandoned {
        error,
        reads,
        again,
    }
}

/// How a process ended, in words.
fn describe(status: ExitStatus) -> String {
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
        return format!("stopped by signal {signal}");
    }
    match status.code() {
        Some(code) => format!("exit status {code}"),
        None => status.to_string(),
    }
}

/// The last line of `text` that is not blank.
fn last_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).rfind(|line| !line.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_name_the_chain_and_round_trip() {
        let theme = crate::theme::resolve("default", &[]).unwrap();
        let request = Request::new(
            &theme,
            "@default/styles/x.scss",
            "a{}",
            Duration::from_secs(10),
        );
        let json = serde_json::to_string(&request).unwrap();
        let back: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(back.limit_ms, 10_000);
        assert_eq!(
            (back.key.as_str(), back.source.as_str()),
            ("@default/styles/x.scss", "a{}")
        );
        let rebuilt = back.theme();
        assert_eq!(rebuilt.chain.len(), theme.chain.len());
        assert_eq!(rebuilt.chain[0].name, "default");
        assert_eq!(rebuilt.chain[0].source, theme.chain[0].source);
    }

    #[test]
    fn messages_are_one_json_line_each() {
        let message = Message::Error {
            file: "@t/a.scss".into(),
            message: "line 1\n 1 | x".into(),
        };
        let line = serde_json::to_string(&message).unwrap();
        assert!(!line.contains('\n'), "{line}");
        assert!(line.starts_with("{\"error\":"), "{line}");
        let read = serde_json::to_string(&Message::Css("a{}".into())).unwrap();
        assert_eq!(read, "{\"css\":\"a{}\"}");
        assert_eq!(
            last_line("a\nthread 'x' overflowed\n\n"),
            Some("thread 'x' overflowed")
        );
        assert_eq!(last_line(" \n"), None);
    }

    #[test]
    fn a_missing_worker_is_an_io_error() {
        let theme = crate::theme::resolve("default", &[]).unwrap();
        let missing = Path::new("/nonexistent/presenter-worker");
        let result = compile(
            missing,
            &theme,
            "@default/x.scss",
            "a{}",
            Duration::from_secs(1),
        );
        assert!(result.is_err());
    }
}
