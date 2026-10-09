//! `presenter init` (cli.md `init`): scaffolds a new presentation directory.
//!
//! The files come from the embedded `assets/scaffold/` tree ([`crate::vendor`]):
//! `presenter.toml` (every setting listed with its default), `slides.md` (or `slides.ipynb`
//! with `--notebook`), `theme/` (a child of the built-in `default` theme that overrides the
//! `title` layout and sets a few CSS custom properties), `.gitignore` (`dist/`) and
//! `README.md`. With `--full-theme`, `theme/` is a copy of the built-in `themes/default/`
//! instead: its `theme.toml` gets a new header comment, `name` and `description`, and its
//! `README.md` a note on top saying that it documents the built-in theme, with its links
//! into presenter's repository unlinked (audit F48).
//!
//! # The target directory
//!
//! `DIR` must be empty or not exist; anything else is a [`UsageError`] (exit 2) and nothing
//! is written. `DIR` is checked where the file system will resolve it: symbolic links are
//! followed (a broken one, whose target is missing, runs through a file or loops, is not a
//! directory; only a target that cannot be read is an I/O error, exit 1), a component that
//! is a file is an error, and
//! so is a `..` after a component that does not exist yet (`new/..` would create `new` and
//! then write next to it). After creating `DIR`, `init` checks once more that it is empty
//! before writing anything, and it never overwrites a file. On success stdout lists the
//! files and the next steps (not with `-q`).
//!
//! # Placeholders
//!
//! Text files of the scaffold contain placeholders, replaced by plain string replacement in
//! one pass over each file. No template engine runs on them, and the theme's templates
//! (`theme/templates/`) are copied unchanged, so their `{{ … }}` expressions stay as they are:
//!
//! | placeholder | value |
//! |---|---|
//! | `{{name}}` | the presentation name as Markdown text: characters Markdown would interpret are backslash-escaped, and so is whatever would start an autolink (`:` before `//`, `@`, `.` after `www`) |
//! | `{{name_yaml}}` | the name as a YAML double-quoted string, quotes included (for front matter) |
//! | `{{theme_name}}` | the theme's `name`: the presentation name as a lower-case slug |
//! | `{{deck}}` | the deck's file name, `slides.md` or `slides.ipynb` |
//!
//! A line containing the marker `{{child_theme}}` is about the child theme: with
//! `--full-theme` the whole line is left out, otherwise only the marker is removed.
//!
//! In `.ipynb` files the placeholders sit inside JSON strings, so there each value is also
//! escaped for a JSON string. The presentation name is `--name`, else the name of `DIR`
//! (made absolute); it must not contain control characters, line or paragraph separators,
//! or the non-characters U+FFFE and U+FFFF (YAML does not allow them).

use std::borrow::Cow;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, anyhow};
use serde_json::Value;

use super::theme::{BUILTIN_THEME_README, copied_readme};
use super::{GlobalArgs, InitArgs, UsageError, print_stdout};
use crate::assets::registry::normalize_lexically;
use crate::theme::resolve::MANIFEST_FILE;
use crate::vendor;

/// Where the scaffold lives in the embedded asset tree.
const SCAFFOLD_PREFIX: &str = "scaffold/";
/// The built-in theme `--full-theme` copies.
const DEFAULT_THEME_PREFIX: &str = "themes/default/";
/// The name of the built-in theme under [`DEFAULT_THEME_PREFIX`].
const DEFAULT_THEME_NAME: &str = "default";
/// The theme directory inside the new project (`[deck] theme` in the scaffold's
/// `presenter.toml` names it).
const THEME_DIR: &str = "theme";
/// Scaffold files copied without placeholder replacement: the theme's minijinja templates.
const VERBATIM_PREFIX: &str = "theme/templates/";
/// The marker of a scaffold line that only the child theme variant keeps.
const CHILD_THEME_MARKER: &str = "{{child_theme}}";
/// The Markdown deck of the scaffold.
const MARKDOWN_DECK: &str = "slides.md";
/// The notebook deck of the scaffold (`--notebook`).
const NOTEBOOK_DECK: &str = "slides.ipynb";
/// The name when neither `--name` nor the directory gives one (`presenter init /`).
const FALLBACK_NAME: &str = "Presentation";
/// The theme name when the presentation name has no letters or digits.
const FALLBACK_THEME_NAME: &str = "theme";
/// ASCII characters that can change the meaning of Markdown inline text (or of a heading
/// line: `#` closes an ATX heading, `{` starts heading attributes, `$` math, `@` an e-mail
/// autolink).
const MARKDOWN_SPECIAL: &str = "\\`*_[]<>!&$~{}#|@";
/// The header of the `--full-theme` copy's `theme.toml`, instead of the built-in one's.
const COPIED_MANIFEST_HEADER: &str = "\
# This presentation's theme, copied from the built-in \"default\" theme by
# `presenter init --full-theme`. It extends no other theme: every template and stylesheet
# it uses is in this directory, ready to change.

";
/// The `description` line of the `--full-theme` copy's `theme.toml`.
const COPIED_DESCRIPTION: &str =
    "description = \"The theme of this presentation, copied from the built-in default theme\"";
/// How many entries of a non-empty target directory the error message names.
const LISTED_ENTRIES: usize = 3;

/// Runs `presenter init`.
pub fn run(args: &InitArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    // An empty DIR means the current directory, which `std::fs` would not check otherwise.
    // (clap already rejects `presenter init ""`; this guards in-process callers.)
    let dir = if args.dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        args.dir.as_path()
    };
    check_target(dir)?;
    let name = presentation_name(args.name.as_deref(), dir)?;
    let scaffold = Scaffold::new(&name, args.notebook, args.full_theme);
    let files = scaffold.files()?;
    write_files(dir, &files)?;
    tracing::info!(
        dir = %dir.display(),
        files = files.len(),
        "scaffolded a presentation"
    );
    if !global.quiet {
        print_stdout(&scaffold.summary(dir))?;
    }
    Ok(ExitCode::SUCCESS)
}

/// What to scaffold: the deck variant, the theme variant and the placeholder values.
struct Scaffold {
    /// `slides.md` or `slides.ipynb`.
    deck: &'static str,
    /// `--full-theme`: copy the built-in default theme instead of the child theme.
    full_theme: bool,
    /// The theme's `name`.
    theme_name: String,
    /// Placeholder token (`{{name}}`) → value, before any per-file escaping.
    values: Vec<(&'static str, String)>,
}

impl Scaffold {
    /// The scaffold for presentation `name` (already validated).
    fn new(name: &str, notebook: bool, full_theme: bool) -> Scaffold {
        let deck = if notebook {
            NOTEBOOK_DECK
        } else {
            MARKDOWN_DECK
        };
        let theme_name = theme_name(name);
        let values = vec![
            ("{{name}}", markdown_text(name)),
            ("{{name_yaml}}", Value::String(name.to_string()).to_string()),
            ("{{theme_name}}", theme_name.clone()),
            ("{{deck}}", deck.to_string()),
        ];
        Scaffold {
            deck,
            full_theme,
            theme_name,
            values,
        }
    }

    /// Every file to write: (path relative to the project, `/`-separated; contents), sorted
    /// by path.
    fn files(&self) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
        let mut files = Vec::new();
        for key in vendor::list(SCAFFOLD_PREFIX) {
            let Some(rel) = key.strip_prefix(SCAFFOLD_PREFIX) else {
                continue;
            };
            if !self.wants(rel) {
                continue;
            }
            let data = embedded(&key)?;
            let data = match String::from_utf8(data) {
                Ok(text) => self.render(rel, &text).into_bytes(),
                Err(binary) => binary.into_bytes(),
            };
            files.push((rel.to_string(), data));
        }
        if !files.iter().any(|(rel, _)| rel == self.deck) {
            return Err(anyhow!(
                "the embedded scaffold has no {} (assets/{SCAFFOLD_PREFIX})",
                self.deck
            ));
        }
        if self.full_theme {
            files.extend(self.default_theme_files()?);
        }
        files.sort();
        Ok(files)
    }

    /// Whether the scaffold file `rel` is part of this variant: the other deck is not, nor
    /// the child theme with `--full-theme`.
    fn wants(&self, rel: &str) -> bool {
        let other_deck = if self.deck == MARKDOWN_DECK {
            NOTEBOOK_DECK
        } else {
            MARKDOWN_DECK
        };
        let child_theme = rel
            .strip_prefix(THEME_DIR)
            .is_some_and(|rest| rest.starts_with('/'));
        rel != other_deck && !(self.full_theme && child_theme)
    }

    /// The text of scaffold file `rel` for this variant: the theme's templates unchanged,
    /// any other file with its [`CHILD_THEME_MARKER`] lines selected and its placeholders
    /// filled in.
    fn render(&self, rel: &str, text: &str) -> String {
        if rel.starts_with(VERBATIM_PREFIX) {
            return text.to_string();
        }
        self.fill(&self.select_lines(text), rel.ends_with(".ipynb"))
    }

    /// `text` without the lines of the other theme variant: a line containing
    /// [`CHILD_THEME_MARKER`] is left out with `--full-theme` and kept without the marker
    /// otherwise.
    fn select_lines(&self, text: &str) -> String {
        text.split_inclusive('\n')
            .filter_map(|line| {
                if !line.contains(CHILD_THEME_MARKER) {
                    Some(Cow::Borrowed(line))
                } else if self.full_theme {
                    None
                } else {
                    Some(Cow::Owned(line.replace(CHILD_THEME_MARKER, "")))
                }
            })
            .collect()
    }

    /// The built-in default theme's files below `theme/`, with a manifest made for this
    /// project ([`copied_manifest`]) and its [`BUILTIN_THEME_README`] marked as the
    /// built-in theme's documentation ([`copied_readme`]).
    fn default_theme_files(&self) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
        let keys = vendor::list(DEFAULT_THEME_PREFIX);
        if keys.is_empty() {
            return Err(anyhow!("the built-in default theme is not embedded"));
        }
        keys.iter()
            .map(|key| {
                let rel = key.strip_prefix(DEFAULT_THEME_PREFIX).unwrap_or(key);
                let mut data = embedded(key)?;
                if rel == MANIFEST_FILE {
                    let text = String::from_utf8(data)
                        .with_context(|| format!("embedded {key} is not UTF-8"))?;
                    data = copied_manifest(&text, &self.theme_name).into_bytes();
                } else if rel == BUILTIN_THEME_README {
                    data = copied_readme(
                        data,
                        DEFAULT_THEME_NAME,
                        &self.theme_name,
                        "presenter init --full-theme",
                    );
                }
                Ok((format!("{THEME_DIR}/{rel}"), data))
            })
            .collect()
    }

    /// `text` with every placeholder replaced in one left-to-right pass (a value is never
    /// scanned again, so a name containing `{{deck}}` stays as it is). `json`: escape the
    /// values for a JSON string.
    fn fill(&self, text: &str, json: bool) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(start) = rest.find("{{") {
            out.push_str(&rest[..start]);
            rest = &rest[start..];
            match self
                .values
                .iter()
                .find(|(token, _)| rest.starts_with(token))
            {
                Some((token, value)) => {
                    if json {
                        out.push_str(&json_string_content(value));
                    } else {
                        out.push_str(value);
                    }
                    rest = &rest[token.len()..];
                }
                None => {
                    out.push_str("{{");
                    rest = &rest[2..];
                }
            }
        }
        out.push_str(rest);
        out
    }

    /// What `init` prints on success: the files it wrote and the next steps.
    fn summary(&self, dir: &Path) -> String {
        let here = is_current_dir(dir);
        let mut text = if here {
            "Created a presentation in the current directory:\n".to_string()
        } else {
            format!("Created a presentation in {}:\n", dir.display())
        };
        let deck = if self.deck == NOTEBOOK_DECK {
            "the deck: a notebook with three cells (open it in Jupyter)"
        } else {
            "the deck: six example slides"
        };
        let theme = if self.full_theme {
            "the look: a copy of the built-in \"default\" theme"
        } else {
            "the look: a theme that extends the built-in \"default\" theme"
        };
        let rows = [
            ("presenter.toml", "settings: every option, with its default"),
            (self.deck, deck),
            ("theme/", theme),
            (
                "README.md",
                "how to preview, build and restyle the presentation",
            ),
            (".gitignore", "keeps the build output (dist/) out of git"),
        ];
        for (file, what) in rows {
            text.push_str(&format!("  {file:<15} {what}\n"));
        }
        text.push_str("\nNext steps:\n");
        if !here {
            text.push_str(&format!("  cd {}\n", cd_argument(dir)));
        }
        let serve = format!("presenter serve {}", self.deck);
        let build = format!("presenter build {}", self.deck);
        // `{:<width$}` pads to a number of characters, not bytes.
        let width = serve.chars().count().max(build.chars().count());
        text.push_str(&format!(
            "  {serve:<width$}    # live preview; every save updates it\n"
        ));
        text.push_str(&format!("  {build:<width$}    # writes dist/slides.html"));
        text
    }
}

/// Fails with a [`UsageError`] unless `dir` is an empty directory or can be created (see
/// [`resolve_target`]).
fn check_target(dir: &Path) -> anyhow::Result<()> {
    let shown = described(dir);
    let (real, found) = resolve_target(dir)?;
    match found {
        Found::Missing => Ok(()),
        Found::NotDir => Err(UsageError(format!("{shown} exists and is not a directory")).into()),
        Found::Dir => check_empty(&real, &shown),
    }
}

/// What the path resolved so far is on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Found {
    /// An existing directory.
    Dir,
    /// An existing file, or anything else that is not a directory.
    NotDir,
    /// It does not exist (yet): `init` creates it.
    Missing,
}

/// Resolves `dir` the way the file system will once `init` has created what is missing:
/// returns the real path (absolute, symbolic links followed) and what it is now. A
/// [`UsageError`] when no directory can be created there: a component before the last one
/// is not a directory, a symbolic link is broken, or a `..` follows a missing component.
fn resolve_target(dir: &Path) -> anyhow::Result<(PathBuf, Found)> {
    let mut real = if dir.is_absolute() {
        PathBuf::new()
    } else {
        std::fs::canonicalize(".").context("cannot read the current directory")?
    };
    let mut found = Found::Dir;
    // The components of `dir` resolved so far, for messages.
    let mut given = PathBuf::new();
    for component in dir.components() {
        if found == Found::NotDir {
            return Err(UsageError(format!("{} is not a directory", given.display())).into());
        }
        let previous = given.clone();
        given.push(component);
        match component {
            Component::Prefix(_) | Component::RootDir => real.push(component),
            Component::CurDir => {}
            // `real` has no symbolic links, so its parent is what `..` names.
            Component::ParentDir if found == Found::Dir => {
                real.pop();
            }
            Component::ParentDir => {
                return Err(UsageError(format!(
                    "invalid directory {}: `..` follows {}, which does not exist",
                    dir.display(),
                    previous.display()
                ))
                .into());
            }
            Component::Normal(name) => {
                real.push(name);
                if found == Found::Dir {
                    (real, found) = inspect(real, &given)?;
                }
            }
        }
    }
    Ok((real, found))
}

/// What `path` (an existing directory's path plus one component, `given` for messages) is:
/// a symbolic link is replaced by its target.
fn inspect(path: PathBuf, given: &Path) -> anyhow::Result<(PathBuf, Found)> {
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok((path, Found::Missing));
        }
        Err(err) => {
            let message = format!("cannot read {}", given.display());
            return Err(anyhow::Error::new(err).context(message));
        }
    };
    let (path, metadata) = if metadata.file_type().is_symlink() {
        let target = std::fs::canonicalize(&path).map_err(|err| broken_link(err, given))?;
        let metadata = std::fs::metadata(&target)
            .with_context(|| format!("cannot read {}", given.display()))?;
        (target, metadata)
    } else {
        (path, metadata)
    };
    let found = if metadata.is_dir() {
        Found::Dir
    } else {
        Found::NotDir
    };
    Ok((path, found))
}

/// The error for symbolic link `given` whose target cannot be resolved (`err`, from
/// `canonicalize`): a [`UsageError`] when the link is broken (its target is missing, runs
/// through a file, or loops), so it is reported like the other target problems; a plain
/// I/O error (exit 1) when the target may be fine but cannot be read (permission denied).
fn broken_link(err: std::io::Error, given: &Path) -> anyhow::Error {
    let given = given.display();
    match err.kind() {
        std::io::ErrorKind::PermissionDenied => {
            anyhow::Error::new(err).context(format!("cannot read {given}"))
        }
        std::io::ErrorKind::NotFound => anyhow::Error::new(UsageError(format!(
            "{given} exists and is not a directory (a broken symbolic link)"
        ))),
        // ENOTDIR (a file on the way), ELOOP (a loop: `FilesystemLoop` is unstable), …
        _ => anyhow::Error::new(UsageError(format!(
            "{given} exists and is not a directory (a broken symbolic link: {err})"
        ))),
    }
}

/// Fails with a [`UsageError`] naming the first few entries unless directory `dir` (shown
/// as `shown`) is empty.
fn check_empty(dir: &Path, shown: &str) -> anyhow::Result<()> {
    let mut names = std::fs::read_dir(dir)
        .and_then(|entries| {
            entries
                .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
                .collect::<std::io::Result<Vec<String>>>()
        })
        .with_context(|| format!("cannot list {shown}"))?;
    if names.is_empty() {
        return Ok(());
    }
    names.sort();
    let mut listed = names[..names.len().min(LISTED_ENTRIES)].join(", ");
    if names.len() > LISTED_ENTRIES {
        listed.push_str(", …");
    }
    Err(UsageError(format!(
        "{shown} is not empty (it contains {listed}); presenter init needs an empty or new directory"
    ))
    .into())
}

/// The presentation name: `--name` (trimmed), else the name of `dir`. A [`UsageError`]
/// when it is empty or contains a character the deck's front matter cannot hold.
fn presentation_name(name: Option<&str>, dir: &Path) -> anyhow::Result<String> {
    let name = match name {
        Some(name) => name.trim().to_string(),
        None => default_name(dir),
    };
    if name.is_empty() {
        return Err(UsageError("the presentation name (--name) must not be empty".into()).into());
    }
    if let Some(bad) = name.chars().find(|&c| !allowed_in_name(c)) {
        return Err(UsageError(format!(
            "invalid presentation name {name:?}: it contains {bad:?}, a control character, \
             line break or non-character; choose another name with --name"
        ))
        .into());
    }
    Ok(name)
}

/// Whether presentation names may contain `c`. Not: control characters (line breaks and
/// tabs included), U+2028/U+2029 (YAML line breaks) and U+FFFE/U+FFFF (YAML does not allow
/// them in a document).
fn allowed_in_name(c: char) -> bool {
    !c.is_control() && !matches!(c, '\u{2028}' | '\u{2029}' | '\u{fffe}' | '\u{ffff}')
}

/// The last component of `dir` made absolute (`.` is the current directory's name), or
/// [`FALLBACK_NAME`] when there is none (the root directory).
fn default_name(dir: &Path) -> String {
    // `absolute` only fails for an empty path or an unavailable CWD.
    let absolute = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    normalize_lexically(&absolute)
        .file_name()
        .map(|name| name.to_string_lossy().trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| FALLBACK_NAME.to_string())
}

/// The theme name for presentation `name`: its letters and digits in lower case, every
/// other run of characters turned into one `-` (so always a valid theme name, see
/// [`crate::theme::manifest::check_theme_name`]); `-theme` is appended when that is the
/// name of a built-in theme, which the new theme could then not extend.
fn theme_name(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = match slug.trim_end_matches('-') {
        "" => FALLBACK_THEME_NAME,
        slug => slug,
    };
    if crate::theme::builtin_names()
        .iter()
        .any(|name| name == slug)
    {
        format!("{slug}-theme")
    } else {
        slug.to_string()
    }
}

/// `text` as Markdown that renders exactly `text`, as plain text: every
/// [`MARKDOWN_SPECIAL`] character is backslash-escaped (CommonMark allows escaping any
/// ASCII punctuation), and so are the characters that would start an autolink, inside
/// which escapes do not work: the `:` of `://` and the `.` of `www.`.
fn markdown_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (at, c) in text.char_indices() {
        let autolink = match c {
            ':' => text[at + 1..].starts_with("//"),
            '.' => text[..at].ends_with("www"),
            _ => false,
        };
        if autolink || MARKDOWN_SPECIAL.contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `text` escaped for the inside of a JSON string (without the quotes).
fn json_string_content(text: &str) -> String {
    let quoted = Value::String(text.to_string()).to_string();
    // A serialized JSON string always starts and ends with `"`.
    quoted[1..quoted.len() - 1].to_string()
}

/// The manifest of the `--full-theme` copy: the built-in theme's `manifest` with
/// [`COPIED_MANIFEST_HEADER`] instead of its leading comment lines, `name = "<name>"` and
/// [`COPIED_DESCRIPTION`] (`name` is a slug from [`theme_name`], so it needs no TOML
/// escaping).
fn copied_manifest(manifest: &str, name: &str) -> String {
    let mut lines = manifest.split_inclusive('\n').peekable();
    while lines
        .next_if(|line| {
            let line = line.trim();
            line.is_empty() || line.starts_with('#')
        })
        .is_some()
    {}
    let body: String = lines.collect();
    let body = set_top_level_key(&body, "description", COPIED_DESCRIPTION);
    let body = set_top_level_key(&body, "name", &format!("name = \"{name}\""));
    format!("{COPIED_MANIFEST_HEADER}{body}")
}

/// A TOML document with its first top-level `key = …` line replaced by `new_line`. When
/// there is no such line, `new_line` is added at the top.
fn set_top_level_key(manifest: &str, key: &str, new_line: &str) -> String {
    let mut out = String::with_capacity(manifest.len() + new_line.len());
    let mut replaced = false;
    let mut in_table = false;
    for line in manifest.split_inclusive('\n') {
        let trimmed = line.trim_start();
        in_table |= trimmed.starts_with('[');
        let is_key = trimmed
            .strip_prefix(key)
            .is_some_and(|rest| rest.trim_start().starts_with('='));
        if is_key && !replaced && !in_table {
            replaced = true;
            out.push_str(new_line);
            if line.ends_with('\n') {
                out.push('\n');
            }
        } else {
            out.push_str(line);
        }
    }
    if replaced {
        out
    } else {
        format!("{new_line}\n{out}")
    }
}

/// The bytes of the embedded file `key`.
fn embedded(key: &str) -> anyhow::Result<Vec<u8>> {
    vendor::get(key)
        .map(Cow::into_owned)
        .ok_or_else(|| anyhow!("embedded file {key} is missing"))
}

/// Creates `dir`, checks that it is (still) empty and writes `files` below it, creating
/// directories as needed. Never overwrites a file.
fn write_files(dir: &Path, files: &[(String, Vec<u8>)]) -> anyhow::Result<()> {
    // Without its `.` components after the first: `create_dir_all("new/.")` fails while
    // `new` does not exist, although `new/.` names the same directory.
    let dir: PathBuf = dir.components().collect();
    let dir = dir.as_path();
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    check_empty(dir, &described(dir))?;
    for (rel, data) in files {
        let path = rel
            .split('/')
            .fold(dir.to_path_buf(), |path, part| path.join(part));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut file| file.write_all(data))
            .with_context(|| format!("cannot write {}", path.display()))?;
    }
    Ok(())
}

/// Whether `dir` names the current directory (`.`, `./.`, or empty).
fn is_current_dir(dir: &Path) -> bool {
    dir.components().all(|c| c == Component::CurDir)
}

/// `dir` for messages: "the current directory" or the path as given.
fn described(dir: &Path) -> String {
    if is_current_dir(dir) {
        "the current directory".to_string()
    } else {
        dir.display().to_string()
    }
}

/// `dir` as the argument of a `cd` command: one shell word ([`shell_word`]), with `./` in
/// front of a relative path that starts with `-` (which `cd` would take for an option).
fn cd_argument(dir: &Path) -> String {
    let text = dir.display().to_string();
    if text.starts_with('-') {
        shell_word(&Path::new(".").join(dir))
    } else {
        shell_word(dir)
    }
}

/// `path` as one shell word: as is when it only has characters no shell treats specially,
/// else single-quoted.
fn shell_word(path: &Path) -> String {
    let text = path.display().to_string();
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+,:@%".contains(c));
    if plain {
        text
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scaffold(name: &str) -> Scaffold {
        Scaffold::new(name, false, false)
    }

    #[test]
    fn placeholders_are_filled_in_one_pass() {
        let s = scaffold("{{deck}} & co");
        assert_eq!(
            s.fill(
                "# {{name}}\ntitle: {{name_yaml}}\n{{deck}} {{ x }} {{",
                false
            ),
            "# \\{\\{deck\\}\\} \\& co\ntitle: \"{{deck}} & co\"\nslides.md {{ x }} {{"
        );
        assert_eq!(s.fill("no placeholders", false), "no placeholders");
        assert_eq!(s.fill("{{theme_name}}", false), "deck-co");
    }

    #[test]
    fn json_files_get_json_escaped_values() {
        let s = scaffold(r#"Say "hi" \o/"#);
        assert_eq!(
            s.fill(r##"["title: {{name_yaml}}\n", "# {{name}}"]"##, true),
            r##"["title: \"Say \\\"hi\\\" \\\\o/\"\n", "# Say \"hi\" \\\\o/"]"##
        );
        let filled = s.fill(r#"{"a": "{{name_yaml}} {{name}}"}"#, true);
        let value: Value = serde_json::from_str(&filled).unwrap();
        assert_eq!(value["a"], r#""Say \"hi\" \\o/" Say "hi" \\o/"#);
    }

    #[test]
    fn templates_are_copied_verbatim() {
        let s = scaffold("talk");
        let template = "{{deck}} {{name}} {{child_theme}}{{ meta.title }}\n";
        assert_eq!(
            s.render("theme/templates/layouts/title.html", template),
            template
        );
        assert_eq!(
            s.render("README.md", template),
            "slides.md talk {{ meta.title }}\n"
        );
        assert_eq!(
            s.render("theme/styles/theme.css", "/* {{deck}} */"),
            "/* slides.md */"
        );
    }

    #[test]
    fn child_theme_lines_depend_on_the_variant() {
        let text = "a\n{{child_theme}}b: {{deck}}\n  \"{{child_theme}}c\\n\",\nd";
        let child = Scaffold::new("t", false, false);
        assert_eq!(
            child.render("x.md", text),
            "a\nb: slides.md\n  \"c\\n\",\nd"
        );
        let full = Scaffold::new("t", true, true);
        assert_eq!(full.render("x.ipynb", text), "a\nd");
        // The last line has no line break.
        assert_eq!(full.select_lines("a\n{{child_theme}}b"), "a\n");
        // A name is filled in after the lines are chosen.
        let named = Scaffold::new("{{child_theme}}", false, true);
        assert_eq!(
            named.render("x.md", "{{name_yaml}}\n"),
            "\"{{child_theme}}\"\n"
        );
    }

    #[test]
    fn markdown_specials_are_escaped() {
        assert_eq!(markdown_text("talk"), "talk");
        assert_eq!(markdown_text("rust-intro 2.0: why"), "rust-intro 2.0: why");
        assert_eq!(
            markdown_text("*a* _b_ `c` [d] <e> {f} #1 $5 a&b ~x~ !|\\ @"),
            "\\*a\\* \\_b\\_ \\`c\\` \\[d\\] \\<e\\> \\{f\\} \\#1 \\$5 a\\&b \\~x\\~ \\!\\|\\\\ \\@"
        );
        // Escapes do not work inside autolinks, so none may start.
        assert_eq!(
            markdown_text("https://x.org/a_b www.x.org a@b.com ftp://y C:/x"),
            "https\\://x.org/a\\_b www\\.x.org a\\@b.com ftp\\://y C:/x"
        );
        assert_eq!(markdown_text("www"), "www");
    }

    #[test]
    fn theme_names_are_slugs() {
        assert_eq!(theme_name("talk"), "talk");
        assert_eq!(theme_name("Rust for Data People!"), "rust-for-data-people");
        assert_eq!(theme_name("  --A__b--  "), "a-b");
        assert_eq!(theme_name("Größe 2026"), "größe-2026");
        assert_eq!(theme_name("!!!"), FALLBACK_THEME_NAME);
        assert_eq!(theme_name("Default"), "default-theme");
        for name in ["talk", "x@y/z\\w", "Größe", "!!!", "default"] {
            let slug = theme_name(name);
            assert!(
                crate::theme::manifest::check_theme_name(&slug).is_ok(),
                "{slug}"
            );
        }
    }

    #[test]
    fn set_top_level_key_replaces_only_the_top_level_key() {
        let manifest =
            "# comment\nname = \"default\"\ndescription = \"x\"\n\n[layouts]\nname = \"keep\"\n";
        assert_eq!(
            set_top_level_key(manifest, "name", "name = \"talk\""),
            "# comment\nname = \"talk\"\ndescription = \"x\"\n\n[layouts]\nname = \"keep\"\n"
        );
        assert_eq!(
            set_top_level_key("[vars]\n", "name", "name = \"t\""),
            "name = \"t\"\n[vars]\n"
        );
        assert_eq!(
            set_top_level_key("  name=\"a\"", "name", "name = \"b\""),
            "name = \"b\""
        );
        // `names = …` is another key.
        assert_eq!(
            set_top_level_key("names = 1\nname = \"a\"\n", "name", "name = \"b\""),
            "names = 1\nname = \"b\"\n"
        );
    }

    #[test]
    fn copied_manifests_get_a_new_header_name_and_description() {
        let manifest = "# The built-in theme.\n# More.\n\nname = \"default\"\n\
                        description = \"Built-in\"\n# kept\n[styles]\nentries = []\n";
        assert_eq!(
            copied_manifest(manifest, "talk"),
            format!(
                "{COPIED_MANIFEST_HEADER}name = \"talk\"\n{COPIED_DESCRIPTION}\n\
                 # kept\n[styles]\nentries = []\n"
            )
        );
        let bare = copied_manifest("[styles]\n", "t");
        assert_eq!(
            bare,
            format!("{COPIED_MANIFEST_HEADER}name = \"t\"\n{COPIED_DESCRIPTION}\n[styles]\n")
        );
        let table: toml::Table = bare.parse().unwrap();
        assert!(table["description"].as_str().unwrap().contains("copied"));
    }

    #[test]
    fn default_names_come_from_the_directory() {
        assert_eq!(default_name(Path::new("/work/talk")), "talk");
        assert_eq!(default_name(Path::new("/work/talk/")), "talk");
        assert_eq!(default_name(Path::new("/work/talk/./x/..")), "talk");
        assert_eq!(default_name(Path::new("/")), FALLBACK_NAME);
        let cwd = std::env::current_dir().unwrap();
        let expected = cwd.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(default_name(Path::new(".")), expected);
    }

    #[test]
    fn names_must_be_printable_and_non_empty() {
        let dir = Path::new("/work/talk");
        let name = |name: &str| presentation_name(Some(name), dir);
        assert_eq!(name("  My talk ").unwrap(), "My talk");
        assert_eq!(
            name("Größe \u{fffd} \u{feff} 🦀").unwrap(),
            "Größe \u{fffd} \u{feff} 🦀"
        );
        assert_eq!(presentation_name(None, dir).unwrap(), "talk");
        for bad in [
            "",
            "   ",
            "a\nb",
            "tab\there",
            "a\u{85}b",
            "a\u{2028}b",
            "a\u{2029}b",
            "x\u{fffe}y",
            "x\u{ffff}y",
        ] {
            let err = name(bad).unwrap_err();
            assert!(err.is::<UsageError>(), "{bad:?}: {err}");
        }
        let err = presentation_name(None, Path::new("/work/a\nb")).unwrap_err();
        assert!(err.to_string().contains("--name"), "{err}");
    }

    #[test]
    fn variants_pick_their_files() {
        let markdown = Scaffold::new("t", false, false);
        assert!(markdown.wants("slides.md") && !markdown.wants("slides.ipynb"));
        assert!(markdown.wants("theme/theme.toml") && markdown.wants("themes.txt"));
        let notebook = Scaffold::new("t", true, true);
        assert!(notebook.wants("slides.ipynb") && !notebook.wants("slides.md"));
        assert!(!notebook.wants("theme/theme.toml") && notebook.wants("theme.txt"));
        assert!(notebook.wants(".gitignore") && notebook.wants("README.md"));
    }

    #[test]
    fn shell_words_are_quoted_when_needed() {
        assert_eq!(shell_word(Path::new("talk")), "talk");
        assert_eq!(shell_word(Path::new("../a/b-c_d.e")), "../a/b-c_d.e");
        assert_eq!(shell_word(Path::new("my talk")), "'my talk'");
        assert_eq!(shell_word(Path::new("it's")), r"'it'\''s'");
    }

    #[test]
    fn cd_arguments_are_never_options() {
        assert_eq!(cd_argument(Path::new("talk")), "talk");
        assert_eq!(cd_argument(Path::new("-dash")), "./-dash");
        assert_eq!(cd_argument(Path::new("-my talk")), "'./-my talk'");
        assert_eq!(cd_argument(Path::new("a/-b")), "a/-b");
        assert_eq!(cd_argument(Path::new("/abs/-x")), "/abs/-x");
        let summary = scaffold("t").summary(Path::new("-dash"));
        assert!(summary.contains("\n  cd ./-dash\n"), "{summary}");
    }

    #[test]
    fn next_steps_change_directory_first() {
        let summary = scaffold("t").summary(Path::new("my talk"));
        let steps = summary.split("Next steps:\n").nth(1).unwrap();
        let lines: Vec<&str> = steps.lines().collect();
        assert_eq!(lines[0], "  cd 'my talk'");
        assert!(lines[1].starts_with("  presenter serve slides.md    # "));
        assert!(lines[2].starts_with("  presenter build slides.md    # "));
        let here = scaffold("t").summary(Path::new("."));
        assert!(!here.contains("cd "), "{here}");
    }

    #[test]
    fn current_dir_detection() {
        assert!(is_current_dir(Path::new(".")));
        assert!(is_current_dir(Path::new("./.")));
        assert!(is_current_dir(Path::new("")));
        assert!(!is_current_dir(Path::new("talk")));
        assert!(!is_current_dir(Path::new("..")));
    }

    /// `resolve_target` of `rel` below `root`: Ok(what it is) or Err(the usage message).
    fn resolve(root: &Path, rel: &str) -> Result<(PathBuf, Found), String> {
        resolve_target(&root.join(rel)).map_err(|err| {
            assert!(err.is::<UsageError>(), "{rel}: {err:#}");
            err.to_string()
        })
    }

    #[test]
    fn targets_resolve_like_the_file_system() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir(root.join("dir")).unwrap();
        std::fs::write(root.join("file.txt"), "x").unwrap();

        assert_eq!(
            resolve(&root, "dir").unwrap(),
            (root.join("dir"), Found::Dir)
        );
        assert_eq!(
            resolve(&root, "dir/./").unwrap(),
            (root.join("dir"), Found::Dir)
        );
        assert_eq!(
            resolve(&root, "dir/..").unwrap(),
            (root.clone(), Found::Dir)
        );
        assert_eq!(
            resolve(&root, "new/a/b").unwrap(),
            (root.join("new/a/b"), Found::Missing)
        );
        assert_eq!(
            resolve(&root, "dir/../new").unwrap(),
            (root.join("new"), Found::Missing)
        );
        assert_eq!(
            resolve(&root, "file.txt").unwrap(),
            (root.join("file.txt"), Found::NotDir)
        );
        // `..` after a missing directory would write next to it.
        let err = resolve(&root, "new/..").unwrap_err();
        assert!(err.contains("`..` follows") && err.ends_with("new, which does not exist"));
        assert!(resolve(&root, "new/a/../b").is_err());
        // A file cannot hold a directory.
        let err = resolve(&root, "file.txt/sub").unwrap_err();
        assert!(err.ends_with("file.txt is not a directory"), "{err}");
        assert!(resolve(&root, "file.txt/..").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_are_followed() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("file.txt"), "x").unwrap();
        symlink(root.join("a/b"), root.join("to-b")).unwrap();
        symlink(root.join("file.txt"), root.join("to-file")).unwrap();
        symlink(root.join("nowhere"), root.join("dangling")).unwrap();

        assert_eq!(
            resolve(&root, "to-b").unwrap(),
            (root.join("a/b"), Found::Dir)
        );
        // `..` leaves the link's target, not the link.
        assert_eq!(
            resolve(&root, "to-b/../new").unwrap(),
            (root.join("a/new"), Found::Missing)
        );
        assert_eq!(resolve(&root, "to-file").unwrap().1, Found::NotDir);
        for rel in ["dangling", "dangling/", "dangling/sub"] {
            let err = resolve(&root, rel).unwrap_err();
            assert!(err.ends_with("(a broken symbolic link)"), "{rel}: {err}");
        }
        let err = check_target(&root.join("dangling")).unwrap_err();
        assert!(err.is::<UsageError>(), "{err}");

        // A loop, and a link through a file, are broken links too (usage errors).
        symlink("loop", root.join("loop")).unwrap();
        symlink("file.txt/x", root.join("via-file")).unwrap();
        for rel in ["loop", "loop/sub", "via-file", "via-file/sub"] {
            let err = resolve(&root, rel).unwrap_err();
            let name = rel.split('/').next().unwrap();
            assert!(
                err.starts_with(&format!(
                    "{} exists and is not a directory (a broken symbolic link: ",
                    root.join(name).display()
                )),
                "{rel}: {err}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_link_targets_are_io_errors() {
        let err = broken_link(
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            Path::new("talk"),
        );
        assert!(!err.is::<UsageError>(), "{err:#}");
        assert!(err.to_string().starts_with("cannot read talk"), "{err:#}");
        let err = broken_link(
            std::io::Error::from(std::io::ErrorKind::NotADirectory),
            Path::new("talk"),
        );
        assert!(err.is::<UsageError>(), "{err:#}");
    }
}
