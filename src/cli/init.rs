//! `presenter init` (cli.md `init`): scaffolds a new presentation directory.
//!
//! The files come from the embedded `assets/scaffold/` tree ([`crate::vendor`]):
//! `presenter.toml` (every setting listed with its default), `slides.md` (or `slides.ipynb`
//! with `--notebook`), `theme/` (a child of the built-in `default` theme that overrides the
//! `title` layout and sets a few CSS custom properties), `.gitignore` (`dist/`) and
//! `README.md`. With `--full-theme`, `theme/` is a copy of the built-in `themes/default/`
//! instead, renamed like the child theme would be (its `theme.toml` `name` line is
//! rewritten). `DIR` must be empty or not exist; anything else is a [`UsageError`] (exit 2)
//! and nothing is written. On success stdout lists the files and the next steps (not with
//! `-q`).
//!
//! # Placeholders
//!
//! Text files of the scaffold contain placeholders, replaced by plain string replacement in
//! one pass over each file (no template engine runs on them, so the `{{ … }}` expressions
//! of the theme's templates stay as they are):
//!
//! | placeholder | value |
//! |---|---|
//! | `{{name}}` | the presentation name as Markdown text: characters Markdown would interpret are backslash-escaped |
//! | `{{name_yaml}}` | the name as a YAML double-quoted string, quotes included (for front matter) |
//! | `{{theme_name}}` | the theme's `name`: the presentation name as a lower-case slug |
//! | `{{deck}}` | the deck's file name, `slides.md` or `slides.ipynb` |
//!
//! In `.ipynb` files the placeholders sit inside JSON strings, so there each value is also
//! escaped for a JSON string. The presentation name is `--name`, else the name of `DIR`
//! (made absolute); it must not contain control characters.

use std::borrow::Cow;
use std::io::Write as _;
use std::path::{Component, Path};
use std::process::ExitCode;

use anyhow::{Context as _, anyhow};
use serde_json::Value;

use super::{GlobalArgs, InitArgs, UsageError, print_stdout};
use crate::assets::registry::normalize_lexically;
use crate::theme::resolve::MANIFEST_FILE;
use crate::vendor;

/// Where the scaffold lives in the embedded asset tree.
const SCAFFOLD_PREFIX: &str = "scaffold/";
/// The built-in theme `--full-theme` copies.
const DEFAULT_THEME_PREFIX: &str = "themes/default/";
/// The theme directory inside the new project (`[deck] theme` in the scaffold's
/// `presenter.toml` names it).
const THEME_DIR: &str = "theme";
/// The Markdown deck of the scaffold.
const MARKDOWN_DECK: &str = "slides.md";
/// The notebook deck of the scaffold (`--notebook`).
const NOTEBOOK_DECK: &str = "slides.ipynb";
/// The name when neither `--name` nor the directory gives one (`presenter init /`).
const FALLBACK_NAME: &str = "Presentation";
/// The theme name when the presentation name has no letters or digits.
const FALLBACK_THEME_NAME: &str = "theme";
/// ASCII characters that can change the meaning of Markdown inline text (or of a heading
/// line: `#` closes an ATX heading, `{` starts heading attributes, `$` math).
const MARKDOWN_SPECIAL: &str = "\\`*_[]<>!&$~{}#|";
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
                Ok(text) => self.fill(&text, rel.ends_with(".ipynb")).into_bytes(),
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

    /// The built-in default theme's files below `theme/`, with the manifest's `name`
    /// replaced by this project's theme name.
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
                    data = rename_theme(&text, &self.theme_name).into_bytes();
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
        let serve = format!("presenter serve {}", self.deck);
        let serve = if here {
            serve
        } else {
            format!("cd {} && {serve}", shell_word(dir))
        };
        let build = format!("presenter build {}", self.deck);
        // `{:<width$}` pads to a number of characters, not bytes.
        let width = serve.chars().count().max(build.chars().count());
        text.push_str("\nNext steps:\n");
        text.push_str(&format!(
            "  {serve:<width$}    # live preview; every save updates it\n"
        ));
        text.push_str(&format!("  {build:<width$}    # writes dist/slides.html"));
        text
    }
}

/// Fails with a [`UsageError`] unless `dir` is an empty directory or does not exist.
fn check_target(dir: &Path) -> anyhow::Result<()> {
    let shown = described(dir);
    let metadata = match std::fs::metadata(dir) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(anyhow::Error::new(err).context(format!("cannot read {shown}"))),
    };
    if !metadata.is_dir() {
        return Err(UsageError(format!("{shown} exists and is not a directory")).into());
    }
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
/// when it is empty or contains a control character.
fn presentation_name(name: Option<&str>, dir: &Path) -> anyhow::Result<String> {
    let name = match name {
        Some(name) => name.trim().to_string(),
        None => default_name(dir),
    };
    if name.is_empty() {
        return Err(UsageError("the presentation name (--name) must not be empty".into()).into());
    }
    if name.chars().any(char::is_control) {
        return Err(UsageError(format!(
            "invalid presentation name {name:?}: it contains a control character \
             (line break, tab, …); choose another one with --name"
        ))
        .into());
    }
    Ok(name)
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

/// `text` with every [`MARKDOWN_SPECIAL`] character backslash-escaped, so that Markdown
/// renders it literally (CommonMark allows escaping any ASCII punctuation).
fn markdown_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if MARKDOWN_SPECIAL.contains(c) {
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

/// A theme manifest with its top-level `name = …` line replaced by `name = "<name>"`
/// (`name` is a slug from [`theme_name`], so it needs no TOML escaping). When there is no
/// such line, one is added at the top.
fn rename_theme(manifest: &str, name: &str) -> String {
    let new_line = format!("name = \"{name}\"");
    let mut out = String::with_capacity(manifest.len());
    let mut renamed = false;
    let mut in_table = false;
    for line in manifest.split_inclusive('\n') {
        let trimmed = line.trim_start();
        in_table |= trimmed.starts_with('[');
        let is_name = trimmed
            .strip_prefix("name")
            .is_some_and(|rest| rest.trim_start().starts_with('='));
        if is_name && !renamed && !in_table {
            renamed = true;
            out.push_str(&new_line);
            if line.ends_with('\n') {
                out.push('\n');
            }
        } else {
            out.push_str(line);
        }
    }
    if renamed {
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

/// Writes `files` below `dir`, creating directories as needed. Never overwrites a file.
fn write_files(dir: &Path, files: &[(String, Vec<u8>)]) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
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
    fn markdown_specials_are_escaped() {
        assert_eq!(markdown_text("talk"), "talk");
        assert_eq!(markdown_text("rust-intro 2.0"), "rust-intro 2.0");
        assert_eq!(
            markdown_text("*a* _b_ `c` [d] <e> {f} #1 $5 a&b ~x~ !|\\"),
            "\\*a\\* \\_b\\_ \\`c\\` \\[d\\] \\<e\\> \\{f\\} \\#1 \\$5 a\\&b \\~x\\~ \\!\\|\\\\"
        );
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
    fn rename_theme_replaces_only_the_top_level_name() {
        let manifest =
            "# comment\nname = \"default\"\ndescription = \"x\"\n\n[layouts]\nname = \"keep\"\n";
        assert_eq!(
            rename_theme(manifest, "talk"),
            "# comment\nname = \"talk\"\ndescription = \"x\"\n\n[layouts]\nname = \"keep\"\n"
        );
        assert_eq!(rename_theme("[vars]\n", "t"), "name = \"t\"\n[vars]\n");
        assert_eq!(rename_theme("  name=\"a\"", "b"), "name = \"b\"");
        // `names = …` is another key.
        assert_eq!(
            rename_theme("names = 1\nname = \"a\"\n", "b"),
            "names = 1\nname = \"b\"\n"
        );
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
        assert_eq!(presentation_name(None, dir).unwrap(), "talk");
        for bad in ["", "   ", "a\nb", "tab\there"] {
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
    fn current_dir_detection() {
        assert!(is_current_dir(Path::new(".")));
        assert!(is_current_dir(Path::new("./.")));
        assert!(is_current_dir(Path::new("")));
        assert!(!is_current_dir(Path::new("talk")));
        assert!(!is_current_dir(Path::new("..")));
    }
}
