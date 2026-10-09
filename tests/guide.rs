//! The user guide (`docs/guide/`) and `README.md` cannot drift from the binary:
//!
//! - `cli.md`: every ```` ```text ```` block whose first line is `$ presenter … --help` is
//!   exactly what that command prints (at 100 columns, without colour), and every command and
//!   subcommand has such a block.
//! - `configuration.md`: the ```` ```toml ```` block after the line
//!   `<!-- generated: presenter dump --default-config -->` is exactly what
//!   `presenter dump --default-config` prints, and the per-table key tables list every key of
//!   the config itself (`Config::default()` with the built-in `[reveal]` options, serialized,
//!   so also the keys without a default) with its default, or `unset` for a key without one,
//!   and nothing else.
//! - `presenter dump --default-config` names every key of the config too, and every setting
//!   in it is commented out: as it is, the text is the default config; with the `#` of its
//!   `#key = value` lines removed, it is the built-in defaults spelled out.
//! - Every relative link and image in `README.md` and `docs/guide/*.md` names a file that
//!   exists, and a `#fragment` into a Markdown file names one of its headings (GitHub's
//!   anchors).
//!
//! When the help texts or the defaults change, regenerate the blocks instead of editing them:
//! `PRESENTER_UPDATE_GUIDE=1 cargo test --test guide` rewrites them in place (then review the
//! diff, and the key tables, which stay hand-written).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use clap::CommandFactory;
use presenter::cli::Cli;
use presenter::config::Config;
use regex::Regex;

/// The marker line in `configuration.md` above the generated defaults block.
const DEFAULT_CONFIG_MARKER: &str = "<!-- generated: presenter dump --default-config -->";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn guide(name: &str) -> PathBuf {
    root().join("docs/guide").join(name)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

fn updating() -> bool {
    std::env::var_os("PRESENTER_UPDATE_GUIDE").is_some_and(|value| !value.is_empty())
}

/// Runs the binary with `args` and returns its stdout; it must succeed. Help is wrapped at
/// 100 columns (stdin, stdout and stderr are not terminals, so clap reads `COLUMNS`), and no
/// colour is forced.
fn presenter_stdout(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_presenter"))
        .args(args)
        .current_dir(root())
        .env("COLUMNS", "100")
        .env("NO_COLOR", "1")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .output()
        .expect("run presenter");
    assert!(
        output.status.success(),
        "presenter {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 stdout")
}

/// `text` with trailing white space removed from every line and trailing blank lines
/// dropped (editors strip both; clap writes some).
fn normalize(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let end = lines
        .iter()
        .rposition(|line| !line.is_empty())
        .map_or(0, |i| i + 1);
    lines[..end].join("\n")
}

/// A fenced code block of a Markdown file: its info string, the 0-based line indices of its
/// first content line and of its closing fence, and its content.
struct Fence {
    info: String,
    start: usize,
    end: usize,
    content: String,
}

/// The fence a line opens: its character (`` ` `` or `~`), its length (3 or more) and the
/// info string after it. Only fences at the start of a line count (the guide indents none).
fn fence_open(line: &str) -> Option<(char, usize, &str)> {
    let c = line.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let len = line.chars().take_while(|&x| x == c).count();
    (len >= 3).then(|| (c, len, &line[len..]))
}

/// Whether `line` closes a fence opened with `len` times `c`.
fn fence_closes(line: &str, c: char, len: usize) -> bool {
    let line = line.trim_end();
    line.chars().count() >= len && line.chars().all(|x| x == c)
}

/// The fenced code blocks of `text`, in order.
fn fences(text: &str) -> Vec<Fence> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some((c, len, info)) = fence_open(lines[i]) {
            let start = i + 1;
            let end = (start..lines.len())
                .find(|&j| fence_closes(lines[j], c, len))
                .unwrap_or_else(|| panic!("unclosed code fence on line {}", i + 1));
            found.push(Fence {
                info: info.trim().to_string(),
                start,
                end,
                content: lines[start..end].join("\n"),
            });
            i = end + 1;
        } else {
            i += 1;
        }
    }
    found
}

/// For each line of `text`: whether it belongs to a fenced code block (fences included).
fn in_code(text: &str) -> Vec<bool> {
    let mut code = vec![false; text.lines().count()];
    for fence in fences(text) {
        for flag in &mut code[fence.start - 1..=fence.end] {
            *flag = true;
        }
    }
    code
}

/// `text` with the lines `start..end` replaced by `content`.
fn replace_lines(text: &str, start: usize, end: usize, content: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<&str> = lines[..start].to_vec();
    out.extend(content.lines());
    out.extend(&lines[end..]);
    out.join("\n") + "\n"
}

/// The help blocks of `cli.md`: the command words after `$ presenter` (without `--help`),
/// the block, and the help the binary prints for them.
fn help_blocks(text: &str) -> Vec<(Vec<String>, Fence, String)> {
    fences(text)
        .into_iter()
        .filter(|fence| fence.info == "text")
        .filter_map(|fence| {
            let first = fence.content.lines().next()?.to_string();
            let words = first.strip_prefix("$ presenter")?.strip_suffix("--help")?;
            let words: Vec<String> = words.split_whitespace().map(str::to_string).collect();
            let mut args: Vec<&str> = words.iter().map(String::as_str).collect();
            args.push("--help");
            let help = presenter_stdout(&args);
            Some((words, fence, help))
        })
        .collect()
}

/// `presenter <words> --help`.
fn help_command(words: &[String]) -> String {
    let mut parts = vec!["presenter".to_string()];
    parts.extend(words.iter().cloned());
    parts.push("--help".to_string());
    parts.join(" ")
}

/// Every command path of the CLI (`[]`, `["build"]`, …, `["theme", "list"]`), without
/// clap's generated `help` subcommands.
fn command_paths(cmd: &clap::Command, prefix: Vec<String>, out: &mut Vec<Vec<String>>) {
    out.push(prefix.clone());
    for sub in cmd.get_subcommands().filter(|sub| sub.get_name() != "help") {
        let mut path = prefix.clone();
        path.push(sub.get_name().to_string());
        command_paths(sub, path, out);
    }
}

#[test]
fn cli_reference_shows_the_help_of_every_command_as_printed() {
    let path = guide("cli.md");
    let text = read(&path);
    let blocks = help_blocks(&text);

    let mut expected = Vec::new();
    command_paths(&Cli::command(), Vec::new(), &mut expected);
    let documented: BTreeSet<&Vec<String>> = blocks.iter().map(|(words, ..)| words).collect();
    let missing: Vec<String> = expected
        .iter()
        .filter(|words| !documented.contains(words))
        .map(|words| help_command(words))
        .collect();
    assert!(missing.is_empty(), "cli.md has no block for {missing:?}");

    if updating() {
        let mut updated = text.clone();
        for (words, fence, help) in blocks.iter().rev() {
            let content = format!("$ {}\n{}", help_command(words), normalize(help));
            updated = replace_lines(&updated, fence.start, fence.end, &content);
        }
        std::fs::write(&path, updated).expect("write cli.md");
        return;
    }
    for (words, fence, help) in &blocks {
        let shown: String = fence.content.lines().skip(1).collect::<Vec<_>>().join("\n");
        assert_eq!(
            normalize(&shown),
            normalize(help),
            "cli.md: the block of `{}` (line {}) is not what the binary prints; \
             run `PRESENTER_UPDATE_GUIDE=1 cargo test --test guide`",
            help_command(words),
            fence.start
        );
    }
}

/// The ```` ```toml ```` block right after [`DEFAULT_CONFIG_MARKER`] in `text`.
fn default_config_fence(text: &str) -> Fence {
    let marker = text
        .lines()
        .position(|line| line.trim() == DEFAULT_CONFIG_MARKER)
        .expect("configuration.md has the default-config marker");
    fences(text)
        .into_iter()
        .find(|fence| fence.start == marker + 2 && fence.info == "toml")
        .expect("a ```toml block directly after the default-config marker")
}

#[test]
fn configuration_page_shows_the_default_config_as_printed() {
    let path = guide("configuration.md");
    let text = read(&path);
    let fence = default_config_fence(&text);
    let printed = presenter_stdout(&["dump", "--default-config"]);
    if updating() {
        let updated = replace_lines(&text, fence.start, fence.end, &normalize(&printed));
        std::fs::write(&path, updated).expect("write configuration.md");
        return;
    }
    assert_eq!(
        normalize(&fence.content),
        normalize(&printed),
        "configuration.md: the default config block is not what `presenter dump \
         --default-config` prints; run `PRESENTER_UPDATE_GUIDE=1 cargo test --test guide`"
    );
}

/// The key tables of `configuration.md`: `(table, key)` → the default cell, from rows
/// `` | `key` | `default` | … | `` under a heading that names `` `[table]` ``.
fn documented_defaults(text: &str) -> BTreeMap<(String, String), String> {
    let heading = Regex::new(r"^#+ .*`\[([a-z]+)\]`").unwrap();
    let row = Regex::new(r"^\| `([A-Za-z_]+)` \| (.+?) \|").unwrap();
    let mut table = None;
    let mut rows = BTreeMap::new();
    for (line, code) in text.lines().zip(in_code(text)) {
        if code {
            continue;
        }
        if let Some(caps) = heading.captures(line) {
            table = Some(caps[1].to_string());
        } else if line.starts_with('#') {
            table = None;
        } else if let (Some(table), Some(caps)) = (&table, row.captures(line)) {
            let key = (table.clone(), caps[1].to_string());
            let previous = rows.insert(key.clone(), caps[2].trim().to_string());
            assert!(previous.is_none(), "configuration.md lists {key:?} twice");
        }
    }
    rows
}

/// Every setting with its built-in default: [`Config::default`] with the built-in `[reveal]`
/// options.
fn builtin_config() -> Config {
    Config {
        reveal: Config::builtin_reveal_defaults(),
        ..Config::default()
    }
}

/// Every key of [`builtin_config`], `(table, key)` in struct order, with its default as
/// TOML (inline), or `None` for a key without a default. From the JSON form, where a
/// `None` field is `null` (no field of `Config` skips serialization), so a new key cannot
/// be missed because it has no default.
fn config_keys() -> Vec<((String, String), Option<String>)> {
    let json = serde_json::to_value(builtin_config()).expect("the config serializes");
    let mut keys = Vec::new();
    for (table, values) in json.as_object().expect("the config is an object") {
        let values = values.as_object().expect("every config entry is a table");
        for (key, value) in values {
            let default = (!value.is_null()).then(|| {
                toml::Value::try_from(value)
                    .expect("a TOML value")
                    .to_string()
            });
            keys.push(((table.clone(), key.clone()), default));
        }
    }
    keys
}

#[test]
fn configuration_tables_list_every_key_with_its_default() {
    let text = read(&guide("configuration.md"));
    let documented = documented_defaults(&text);
    let keys = config_keys();
    for ((table, key), default) in &keys {
        let expected = match default {
            Some(value) => format!("`{value}`"),
            None => "unset".to_string(),
        };
        assert_eq!(
            documented.get(&(table.clone(), key.clone())),
            Some(&expected),
            "configuration.md: `{key}` under `[{table}]` must be listed with the default {expected}"
        );
    }
    let known: BTreeSet<&(String, String)> = keys.iter().map(|(key, _)| key).collect();
    for (table, key) in documented.keys() {
        assert!(
            known.contains(&(table.clone(), key.clone())),
            "configuration.md lists `{key}` under `[{table}]`, which is not a setting"
        );
    }
}

/// `text` with the `#` of its commented-out settings (`#key = value`) removed.
fn uncomment(text: &str) -> String {
    let setting = Regex::new(r"^#([A-Za-z0-9_]+ = .*)$").unwrap();
    text.lines()
        .map(|line| match setting.captures(line) {
            Some(caps) => caps[1].to_string(),
            None => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn default_config_names_every_key_and_sets_none() {
    let printed = presenter_stdout(&["dump", "--default-config"]);
    assert_eq!(
        Config::from_toml_str(&printed).expect("valid TOML"),
        Config::default(),
        "saved as presenter.toml, the default config must change nothing"
    );
    let table: toml::Table = printed.parse().expect("valid TOML");
    assert!(
        table
            .values()
            .all(|keys| keys.as_table().is_some_and(toml::Table::is_empty)),
        "the default config sets something: {table:?}"
    );
    assert_eq!(
        Config::from_toml_str(&uncomment(&printed)).expect("valid TOML when uncommented"),
        builtin_config(),
        "uncommented, the default config must be the built-in defaults"
    );

    let mut current = String::new();
    let mut lines = BTreeSet::new();
    for line in printed.lines() {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name.to_string();
        } else {
            lines.insert((current.clone(), line.to_string()));
        }
    }
    for ((table, key), default) in config_keys() {
        let line = match default {
            Some(value) => format!("#{key} = {value}"),
            None => format!("# {key}: unset"),
        };
        assert!(
            lines.contains(&(table.clone(), line.clone())),
            "`presenter dump --default-config` has no `{line}` under `[{table}]`"
        );
    }
}

#[test]
fn dump_default_config_takes_no_deck() {
    for args in [
        &["dump", "--default-config", "examples/basic/slides.md"][..],
        &["dump", "--default-config", "--context"][..],
        &["dump"][..],
    ] {
        let status = Command::new(env!("CARGO_BIN_EXE_presenter"))
            .args(args)
            .current_dir(root())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run presenter");
        assert_eq!(status.code(), Some(2), "presenter {args:?}");
    }
}

/// The Markdown files whose links are checked.
fn documents() -> Vec<PathBuf> {
    let mut files = vec![root().join("README.md")];
    let mut pages: Vec<PathBuf> = std::fs::read_dir(root().join("docs/guide"))
        .expect("docs/guide exists")
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    pages.sort();
    files.extend(pages);
    files
}

/// `line` without its inline code spans: a run of backticks up to the next run of the same
/// length (an unmatched run stays as it is).
fn strip_code_spans(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let run_at = |i: usize| chars[i..].iter().take_while(|&&c| c == '`').count();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '`' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let open = run_at(i);
        let mut j = i + open;
        let close = loop {
            if j >= chars.len() {
                break None;
            }
            if chars[j] == '`' {
                let run = run_at(j);
                if run == open {
                    break Some(j + run);
                }
                j += run;
            } else {
                j += 1;
            }
        };
        match close {
            Some(end) => i = end,
            None => {
                out.extend(&chars[i..i + open]);
                i += open;
            }
        }
    }
    out
}

/// `text` without fenced code blocks and inline code spans (whose `[x](y)` are examples).
fn prose(text: &str) -> String {
    let mut out = String::new();
    for (line, code) in text.lines().zip(in_code(text)) {
        if !code {
            out.push_str(&strip_code_spans(line));
        }
        out.push('\n');
    }
    out
}

/// The link targets of `text`'s prose: `[text](target)`, `![alt](target)` and
/// `[text](target "title")`.
fn link_targets(text: &str) -> Vec<String> {
    let link = Regex::new(r#"\]\(([^)\s]+)(?:\s+"[^"]*")?\)"#).unwrap();
    link.captures_iter(&prose(text))
        .map(|caps| caps[1].to_string())
        .collect()
}

/// GitHub's anchors for the headings of a Markdown file: lower case, punctuation other than
/// `-` and `_` removed, spaces as `-`, and `-1`, `-2`, … for repeats.
fn anchors(text: &str) -> BTreeSet<String> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut found = BTreeSet::new();
    for (line, code) in text.lines().zip(in_code(text)) {
        if code || !line.starts_with('#') {
            continue;
        }
        let title = line.trim_start_matches('#').trim();
        let slug: String = title
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
            .map(|c| if c == ' ' { '-' } else { c })
            .collect();
        let count = seen.entry(slug.clone()).or_insert(0);
        found.insert(if *count == 0 {
            slug.clone()
        } else {
            format!("{slug}-{count}")
        });
        *count += 1;
    }
    found
}

#[test]
fn relative_links_and_images_resolve() {
    let mut broken = Vec::new();
    let mut checked = 0;
    for document in documents() {
        let text = read(&document);
        let dir = document.parent().expect("a parent directory");
        for target in link_targets(&text) {
            if target.contains("://") || target.starts_with("mailto:") {
                continue;
            }
            checked += 1;
            let (path, fragment) = match target.split_once('#') {
                Some((path, fragment)) => (path, Some(fragment)),
                None => (target.as_str(), None),
            };
            let file = if path.is_empty() {
                document.clone()
            } else {
                dir.join(path)
            };
            let name = document.strip_prefix(root()).unwrap_or(&document).display();
            if !file.exists() {
                broken.push(format!("{name}: {target}: no such file"));
                continue;
            }
            let markdown = file.extension().is_some_and(|ext| ext == "md");
            if let (Some(fragment), true) = (fragment, markdown)
                && !anchors(&read(&file)).contains(fragment)
            {
                broken.push(format!("{name}: {target}: no heading with this anchor"));
            }
        }
    }
    assert!(broken.is_empty(), "broken links:\n{}", broken.join("\n"));
    assert!(
        checked > 50,
        "only {checked} relative links found; is the scan broken?"
    );
}
