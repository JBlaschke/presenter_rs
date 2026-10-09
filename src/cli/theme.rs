//! `presenter theme list/eject/new/check` (cli.md `theme`, theme-format.md §1–§3): the
//! theme-iteration workflow.
//!
//! - `list` prints the built-in themes with their `description`, then, when a
//!   `presenter.toml` is found from the current directory (or given with `--config`), the
//!   theme `build` would use there: [`pipeline::resolve_theme`] for a deck without front
//!   matter in the current directory, so `[deck] theme` (or the default) searched in that
//!   file's directory, then the current directory. It prints the chain child → root and
//!   where each theme lives.
//! - `eject NAME DIR` copies every embedded file of the built-in theme `NAME` into `DIR`,
//!   which must not exist. The copy is named after the directory: `name` in `theme.toml`
//!   (with a comment line on top saying where it came from) and the `@NAME/` template names
//!   in its templates change, and its top-level `README.md` gets a note on top saying that
//!   it documents the built-in theme, with its links into presenter's repository unlinked
//!   (audit F48); everything else, `extends` included, is copied as is, so ejecting
//!   `default` gives a root theme that builds exactly like the built-in one.
//! - `new DIR [--extends NAME]` writes a minimal child theme into `DIR` (which must be
//!   empty or not exist): `theme.toml` (`name` from the directory, `extends`, `[styles]
//!   entries`), `styles/theme.css` listing the parent's tokens (the CSS custom properties
//!   that its reveal theme and its stylesheets set on `:root`) as comments, and
//!   `templates/.gitkeep`. `--extends` is a theme directory relative to the current
//!   directory or a built-in theme name, as theme-format.md §3 resolves it; a directory is
//!   written to `theme.toml` relative to the new theme, as §2 requires. Without
//!   `--extends` the parent is the built-in `default`, even next to a `default/` directory.
//!   Stylesheets of the parent that cannot be read are left out of the token list (with a
//!   warning); `check` reports them.
//! - `check DIR` resolves the theme in `DIR` ([`theme::resolve()`], as an absolute path, so
//!   it is always that directory, never a built-in theme of the same name) and prints its
//!   chain, then reports every problem on stderr as `path:line: error: message`: style and
//!   script entries and a `highlight_style` file that do not exist, `[layouts]` entries
//!   without a template file in the chain, templates of the chain's directories that do
//!   not compile, and an invalid `[reveal] plugins` list. A `DIR` that does not exist, is
//!   not a directory or has no `theme.toml` is a usage error; what resolution of the theme
//!   in it rejects (unknown manifest keys, an invalid theme name, a missing parent, a
//!   cycle, required templates missing from the root theme) fails the command with that
//!   error.
//!
//! `eject` and `new` take `DIR` lexically (`a/..` is the current directory) for both the
//! checks and the writes. They validate everything before they write; if writing or the
//! final resolution of the new theme still fails, they remove the files and directories
//! they created, so a retry is not refused.
//!
//! Exit codes, one rule for every subcommand (audit F42): 2 when a name or path argument
//! names nothing usable, 1 when what it names is broken. So 0 on success; 1 when `check`
//! finds problems, for errors of a theme that exists (an invalid `theme.toml`, a missing
//! parent named by its `extends`, …) and I/O failures; 2 for usage errors: `eject` of a
//! theme that is not built in, an existing `eject` directory, a non-empty `new` directory,
//! a `new --extends` theme that is neither a theme directory nor a built-in theme, a
//! `check` directory that does not exist, is not a directory or has no `theme.toml`, a
//! directory name that cannot be a theme name or that a theme of the parent chain already
//! has (`theme new default`), a `new` parent whose chain already has the most themes a
//! chain may have, and an invalid `presenter.toml` or `--config` file in `list`. With
//! `-q`, `eject`, `new` and `check` print nothing on stdout.

use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::sync::LazyLock;

use anyhow::{Context, anyhow};
use regex::Regex;

use super::{
    EXIT_BUILD_ERROR, GlobalArgs, ThemeArgs, ThemeCheckArgs, ThemeCommand, ThemeEjectArgs,
    ThemeNewArgs, UsageError, display_path, print_stdout, report_diagnostics,
};
use crate::assets::registry::normalize_lexically;
use crate::config::Config;
use crate::error::{Diagnostic, Diagnostics, Error};
use crate::ir::{Deck, DeckMeta, Features, SourceInfo, SourceKind, SourceSpan};
use crate::pipeline::{self, BuildOptions};
use crate::theme::manifest::check_theme_name;
use crate::theme::resolve::{MANIFEST_FILE, MAX_CHAIN_LEN};
use crate::theme::{
    self, Theme, ThemeDir, ThemeManifest, ThemeSource, is_layout_name, normalize_rel,
};

/// The parent of `presenter theme new` without `--extends` (always the built-in theme).
const DEFAULT_PARENT: &str = "default";

/// The top-level file of a built-in theme that documents it (its tokens, layouts and
/// classes; the theme's CSS and templates refer to it). `eject` and `init --full-theme`
/// copy it as [`copied_readme`] (audit F48).
pub(crate) const BUILTIN_THEME_README: &str = "README.md";

/// `highlight_style` values that name a bundled style rather than a file.
const BUNDLED_HIGHLIGHT_STYLES: [&str; 3] = ["monokai", "zenburn", "none"];

/// Runs `presenter theme`.
pub fn run(args: &ThemeArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    match &args.command {
        ThemeCommand::List => list(global),
        ThemeCommand::Eject(args) => eject(args, global),
        ThemeCommand::New(args) => new(args, global),
        ThemeCommand::Check(args) => check(args, global),
    }
}

// ---------------------------------------------------------------------------------------
// list

/// `theme list`: the built-in themes, then the project's theme chain (if any).
fn list(global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    print_stdout(&builtin_listing()?)?;
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let found = match &global.config {
        Some(path) => {
            let path = std::path::absolute(path)?;
            let config = Config::load(&path)?;
            Some((path, config))
        }
        None => Config::discover(&cwd)?,
    };
    let Some((config_path, config)) = found else {
        print_stdout("\nNo presenter.toml in the current directory or its parents.")?;
        return Ok(ExitCode::SUCCESS);
    };
    let shown_path = display_path(&config_path);
    let theme = project_theme(&cwd, &config, &config_path)
        .with_context(|| format!("{}: [deck] theme", shown_path.display()))?;
    let origin = if sets_deck_theme(&config_path) {
        format!(
            "[deck] theme = {} in {}",
            toml_string(&config.deck.theme),
            shown_path.display()
        )
    } else {
        format!("the default; {} has no [deck] theme", shown_path.display())
    };
    print_stdout(&format!(
        "\nTheme '{}' for this directory ({origin}):\n{}",
        theme.name(),
        chain_lines(&theme)
    ))?;
    Ok(ExitCode::SUCCESS)
}

/// The theme `build` would use in `cwd` with `config` (from `config_path`):
/// [`pipeline::resolve_theme`], so the same precedence and search roots, for a deck
/// without front matter whose source directory is `cwd` (`theme list` has no deck).
fn project_theme(cwd: &Path, config: &Config, config_path: &Path) -> crate::error::Result<Theme> {
    let deck = Deck {
        meta: DeckMeta::default(),
        slides: Vec::new(),
        features: Features::default(),
        source: SourceInfo {
            path: cwd.to_path_buf(),
            kind: SourceKind::Markdown,
            base_dir: cwd.to_path_buf(),
        },
        assets: Vec::new(),
    };
    pipeline::resolve_theme(&BuildOptions::new(cwd), &deck, config, Some(config_path))
}

/// True if the TOML file at `path` sets `[deck] theme` (rather than leaving the default).
fn sets_deck_theme(path: &Path) -> bool {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .is_some_and(|table| {
            table
                .get("deck")
                .and_then(|deck| deck.get("theme"))
                .is_some()
        })
}

/// `Built-in themes:` and one `  <name>  <description>` line per built-in theme.
fn builtin_listing() -> anyhow::Result<String> {
    let names = theme::builtin_names();
    let width = names
        .iter()
        .map(|name| name.chars().count())
        .max()
        .unwrap_or(0);
    let mut text = String::from("Built-in themes:");
    for name in &names {
        let builtin = theme::resolve(name, &[])?;
        let description = builtin.chain[0].manifest.description.as_deref();
        let line = format!("  {name:<width$}  {}", description.unwrap_or(""));
        let _ = write!(text, "\n{}", line.trim_end());
    }
    Ok(text)
}

/// `  chain: a → b` and one `    <name>  <location>` line per theme of the chain.
fn chain_lines(theme: &Theme) -> String {
    let names: Vec<&str> = theme.chain.iter().map(|dir| dir.name.as_str()).collect();
    let width = names
        .iter()
        .map(|name| name.chars().count())
        .max()
        .unwrap_or(0);
    let mut text = format!("  chain: {}", names.join(" → "));
    for dir in &theme.chain {
        let location = match &dir.source {
            ThemeSource::Disk { dir } => display_path(dir).display().to_string(),
            ThemeSource::Embedded { .. } => "built-in".to_string(),
        };
        let _ = write!(text, "\n    {:<width$}  {location}", dir.name);
    }
    text
}

// ---------------------------------------------------------------------------------------
// eject

/// `theme eject NAME DIR`.
fn eject(args: &ThemeEjectArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let ejected = eject_builtin(&args.name, &args.dir)?;
    if !global.quiet {
        print_stdout(&eject_message(&args.name, &args.dir, &ejected))?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Copies the built-in theme `name` into the new directory `dir` as a theme named after
/// the directory, and returns the copy, resolved (`theme eject`; `init --full-theme` can
/// use it too). The copy builds exactly like the built-in theme (see the module docs).
/// Nothing is left behind when it fails.
///
/// Errors: [`UsageError`] when `name` is not a built-in theme, `dir` exists, or the
/// directory's name cannot be a theme name or is the name of a theme the copy extends;
/// theme and I/O errors otherwise.
pub(crate) fn eject_builtin(name: &str, dir: &Path) -> anyhow::Result<Theme> {
    let builtins = theme::builtin_names();
    if !builtins.iter().any(|builtin| builtin == name) {
        return Err(UsageError(format!(
            "theme eject: '{name}' is not a built-in theme (built-in themes: {})",
            builtins.join(", ")
        ))
        .into());
    }
    let target = absolute_target(dir)?;
    if fs::symlink_metadata(&target).is_ok() {
        return Err(UsageError(format!(
            "theme eject: {} already exists; name a directory that does not exist",
            describe(dir, &target)
        ))
        .into());
    }
    let (new_name, spec) = name_and_spec(&target, dir)?;
    let source = theme::resolve(name, &[])?;
    check_name_is_free(&new_name, &source.chain[1..], dir)?;
    let files = ejected_files(&source.chain[0], &new_name)?;
    let ejected = create_theme(&target, &files, &spec)?;
    tracing::info!(theme = name, dir = %dir.display(), files = files.len(), "ejected theme");
    Ok(ejected)
}

/// Every file of the built-in theme `builtin` as the files of its copy named `name`:
/// `theme.toml` renamed ([`renamed_manifest`]) under a provenance comment, templates
/// retargeted ([`retarget_template`]), the [`BUILTIN_THEME_README`] as [`copied_readme`],
/// everything else as is.
fn ejected_files(builtin: &ThemeDir, name: &str) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
    let mut files = Vec::new();
    for rel in builtin.list("") {
        let bytes = builtin
            .read(&rel)?
            .ok_or_else(|| anyhow!("built-in theme '{}': cannot read {rel}", builtin.name))?;
        let bytes = if rel == MANIFEST_FILE {
            let renamed = renamed_manifest(&String::from_utf8(bytes)?, name)?;
            format!(
                "# Ejected from the built-in theme '{}' by `presenter theme eject`.\n{renamed}",
                builtin.name
            )
            .into_bytes()
        } else if rel.starts_with("templates/") {
            retarget_template(bytes, &builtin.name, name)
        } else if rel == BUILTIN_THEME_README {
            copied_readme(bytes, &builtin.name, name, "presenter theme eject")
        } else {
            bytes
        };
        files.push((rel, bytes));
    }
    Ok(files)
}

/// The [`BUILTIN_THEME_README`] of the built-in theme `builtin` for its copy, the theme
/// `name` that the command `made_by` wrote (audit F48). A note on top says that the copy
/// is the theme `name` and that the text documents the built-in theme, whose tokens,
/// layouts and classes the copy keeps until it is changed. Links that leave the theme
/// directory (`[text](../../../docs/spec/theme-format.md)`: files of presenter's
/// repository, which a copy does not have) keep their text and lose the link; the note
/// says where those paths are. Non-UTF-8 bytes are copied unchanged.
pub(crate) fn copied_readme(bytes: Vec<u8>, builtin: &str, name: &str, made_by: &str) -> Vec<u8> {
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => return err.into_bytes(),
    };
    // A Markdown link `[text](target)` whose target starts with `../`.
    static OUTSIDE_LINK: LazyLock<Regex> = LazyLock::new(|| {
        // A constant pattern covered by the unit tests: compiling it cannot fail.
        Regex::new(r"\[([^\[\]\n]*)\]\(\.\./[^()\s]*\)").expect("link regex is valid")
    });
    let body = OUTSIDE_LINK.replace_all(&text, "${1}");
    format!(
        "> **A copy.** `{made_by}` copied presenter's built-in theme `{builtin}` here as the \
         theme `{name}`.\n> The text below documents the built-in theme: its tokens, layouts \
         and classes are this copy's too, until you change them. Paths outside this \
         directory, such as `docs/spec/theme-format.md`, are in presenter's source \
         repository.\n\n\
         {body}"
    )
    .into_bytes()
}

/// `manifest` (the text of a `theme.toml`) with its top-level `name = …` line replaced by
/// `name = "<name>"`; every other line (comments included) is kept. Fails if there is no
/// such line or the result does not parse with that name.
fn renamed_manifest(manifest: &str, name: &str) -> anyhow::Result<String> {
    let mut text = String::with_capacity(manifest.len() + name.len());
    let mut renamed = false;
    let mut in_table = false;
    for line in manifest.split_inclusive('\n') {
        let trimmed = line.trim_start();
        in_table |= trimmed.starts_with('[');
        let is_name = !in_table
            && !renamed
            && trimmed
                .strip_prefix("name")
                .is_some_and(|rest| rest.trim_start().starts_with('='));
        if is_name {
            let ending = &line[line.trim_end_matches(['\r', '\n']).len()..];
            let _ = write!(text, "name = {}{ending}", toml_string(name));
            renamed = true;
        } else {
            text.push_str(line);
        }
    }
    let parsed = renamed
        .then(|| ThemeManifest::from_toml_str(&text, name, MANIFEST_FILE))
        .transpose()?;
    match parsed {
        Some(manifest) if manifest.name == name => Ok(text),
        _ => Err(anyhow!(
            "cannot rename the theme in {MANIFEST_FILE}: no top-level `name = …` line"
        )),
    }
}

/// A template of the theme `old` as the template of the renamed copy `new`: the
/// `@<old>/` template names it uses for its own files (theme-format.md §3) become
/// `@<new>/`, which they must be once the theme is renamed. Non-UTF-8 files are unchanged.
fn retarget_template(bytes: Vec<u8>, old: &str, new: &str) -> Vec<u8> {
    match String::from_utf8(bytes) {
        Ok(text) => text
            .replace(&format!("@{old}/"), &format!("@{new}/"))
            .into_bytes(),
        Err(err) => err.into_bytes(),
    }
}

/// What `eject` prints: what was copied and how to use it.
fn eject_message(builtin: &str, dir: &Path, ejected: &Theme) -> String {
    let root = &ejected.chain[0];
    let files = root.list("").len();
    let shown = dir.display().to_string();
    let word = shell_word(&shown);
    let kind = match &root.manifest.extends {
        None => "It is a root theme (no `extends`) with every template: edit any file.".to_string(),
        Some(parent) => format!("It extends '{parent}', like the built-in theme."),
    };
    format!(
        "Ejected the built-in theme '{builtin}' to {shown} as theme '{}' ({files} files).\n\
         {kind}\n\
         Use it with:\n  presenter build slides.md --theme {word}\n\
         or in presenter.toml (a path relative to that file):\n  [deck]\n  theme = {}\n\
         Check it after editing: presenter theme check {word}",
        ejected.name(),
        toml_string(&shown)
    )
}

// ---------------------------------------------------------------------------------------
// new

/// `theme new DIR [--extends NAME]`.
fn new(args: &ThemeNewArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let target = absolute_target(&args.dir)?;
    check_empty_or_absent(&target, &args.dir)?;
    let (name, spec) = name_and_spec(&target, &args.dir)?;
    let (parent_spec, parent) = new_parent(args.extends.as_deref())?;
    check_name_is_free(&name, &parent.chain, &args.dir)?;
    check_chain_room(&parent, parent_spec)?;
    let extends = extends_value(&target, parent_spec, &parent.chain[0])?;
    let files = [
        (
            MANIFEST_FILE.to_string(),
            new_manifest(&name, &extends).into_bytes(),
        ),
        (
            "styles/theme.css".to_string(),
            new_stylesheet(&name, &parent).into_bytes(),
        ),
        ("templates/.gitkeep".to_string(), Vec::new()),
    ];
    let created = create_theme(&target, &files, &spec)?;
    tracing::info!(theme = %name, dir = %args.dir.display(), "created theme");
    if !global.quiet {
        print_stdout(&new_message(&args.dir, &created, &parent))?;
    }
    Ok(ExitCode::SUCCESS)
}

/// The parent of a new theme and the spec it was resolved from: `--extends` (a theme
/// directory relative to the current directory, else a built-in name), or without it the
/// built-in [`DEFAULT_PARENT`], never a `default/` directory that happens to be here.
///
/// A `--extends` that names no theme (neither a theme directory nor a built-in theme) is a
/// [`UsageError`]; a theme that exists but does not resolve (an invalid `theme.toml`, a
/// missing ancestor) is its theme error.
fn new_parent(extends: Option<&str>) -> anyhow::Result<(&str, Theme)> {
    let Some(spec) = extends else {
        let parent = theme::resolve(DEFAULT_PARENT, &[])
            .with_context(|| format!("theme new: the built-in theme '{DEFAULT_PARENT}'"))?;
        return Ok((DEFAULT_PARENT, parent));
    };
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    match theme::resolve(spec, &[&cwd]) {
        Ok(parent) => Ok((spec, parent)),
        Err(err @ Error::Theme { .. }) if names_no_theme(&err, spec) => {
            Err(UsageError(format!("theme new: --extends {spec}: {err}")).into())
        }
        Err(err) => Err(anyhow::Error::new(err).context(format!("theme new: --extends {spec}"))),
    }
}

/// Whether `err`, from resolving `spec`, says that `spec` itself names no theme (not found,
/// or empty) rather than that the theme it names is broken.
fn names_no_theme(err: &Error, spec: &str) -> bool {
    matches!(err, Error::Theme { name, message }
        if name == spec && (message.starts_with("not found") || message == "empty theme name"))
}

/// `target` (where `dir` points) must not exist or be an empty directory.
fn check_empty_or_absent(target: &Path, dir: &Path) -> anyhow::Result<()> {
    if fs::symlink_metadata(target).is_err() {
        return Ok(());
    }
    let empty = target.is_dir()
        && fs::read_dir(target)
            .with_context(|| format!("cannot read {}", dir.display()))?
            .next()
            .is_none();
    if empty {
        return Ok(());
    }
    Err(UsageError(format!(
        "theme new: {} exists and is not an empty directory",
        describe(dir, target)
    ))
    .into())
}

/// A child of `parent` (resolved from `spec`) must not make the chain longer than
/// [`MAX_CHAIN_LEN`], or it would never resolve.
fn check_chain_room(parent: &Theme, spec: &str) -> anyhow::Result<()> {
    if parent.chain.len() < MAX_CHAIN_LEN {
        return Ok(());
    }
    let names: Vec<&str> = parent.chain.iter().map(|dir| dir.name.as_str()).collect();
    Err(UsageError(format!(
        "theme new: --extends {spec}: its chain already has {} themes ({}), the most a chain \
         may have, so a child of it would not resolve",
        parent.chain.len(),
        names.join(" → ")
    ))
    .into())
}

/// The `extends` value for a new theme at `target` (absolute; it may not exist yet) whose
/// parent `spec` resolved to `parent`: the built-in name, or the parent's directory
/// relative to the new theme's (`/`-separated).
fn extends_value(target: &Path, spec: &str, parent: &ThemeDir) -> anyhow::Result<String> {
    let ThemeSource::Disk { dir: parent_dir } = &parent.source else {
        return Ok(spec.to_string());
    };
    let dir = canonical_destination(target)?;
    match relative_path(&dir, parent_dir) {
        Some(relative) => Ok(relative),
        None => parent_dir
            .to_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("{} is not a valid UTF-8 path", parent_dir.display())),
    }
}

/// `path` (absolute and normalized; it or some of its last components may not exist yet)
/// with its deepest existing ancestor canonicalized: the canonical path the directory
/// will have, as [`theme::resolve()`] names theme directories (symbolic links resolved).
fn canonical_destination(path: &Path) -> anyhow::Result<PathBuf> {
    for ancestor in path.ancestors() {
        if ancestor.exists() {
            let base = fs::canonicalize(ancestor)
                .with_context(|| format!("cannot open {}", ancestor.display()))?;
            let rest = path.strip_prefix(ancestor)?;
            return Ok(base.join(rest));
        }
    }
    Ok(path.to_path_buf())
}

/// `to` relative to `from` (both absolute and normalized), `/`-separated, or `None` when
/// they share no root (different Windows drives) or a component is not UTF-8.
fn relative_path(from: &Path, to: &Path) -> Option<String> {
    let from: Vec<Component<'_>> = from.components().collect();
    let to: Vec<Component<'_>> = to.components().collect();
    if from.first() != to.first() {
        return None;
    }
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<&str> = vec![".."; from.len() - common];
    for component in &to[common..] {
        parts.push(component.as_os_str().to_str()?);
    }
    Some(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}

/// The `theme.toml` of a new theme.
fn new_manifest(name: &str, extends: &str) -> String {
    format!(
        r##"# Theme manifest, created by `presenter theme new`.
name = {name}
extends = {extends}  # a built-in theme name, or a theme directory relative to this file
# description = "One line for `presenter theme list`"

[styles]
# Stylesheets of this theme, compiled after the parent's, so a plain rule here wins.
entries = ["styles/theme.css"]
# reveal_theme = "white"       # a stock reveal theme (black, white, league, …) or "none"
# highlight_style = "monokai"  # "monokai", "zenburn", "none" or a CSS file relative to this file

# [scripts]
# entries = ["scripts/extra.js"]  # included after the reveal plugins, before Reveal.initialize

# [reveal]                     # Reveal.initialize defaults of this theme
# transition = "fade"

# [layouts]                    # documents the layouts this theme adds: templates/layouts/<name>.html
# quote = "One big quotation"

# [vars]                       # free-form values for templates (theme.vars.*)
# accent = "#2a76dd"
"##,
        name = toml_string(name),
        extends = toml_string(extends),
    )
}

/// The `styles/theme.css` of a new theme: the parent's tokens as comments.
fn new_stylesheet(name: &str, parent: &Theme) -> String {
    let mut css = format!(
        "/*\n * Stylesheet of the theme '{name}', loaded after the stylesheets of '{}', so a\n \
         * plain rule here wins. Prefer overriding tokens (CSS custom properties) to overriding\n \
         * rules: uncomment a line below and change its value. The values shown are the\n \
         * parent's current ones.\n */\n:root {{",
        parent.name()
    );
    // Nothing may end the comments early.
    let safe = |text: &str| text.replace("*/", "* /");
    for (index, (label, tokens)) in token_groups(parent).iter().enumerate() {
        if index > 0 {
            css.push('\n');
        }
        let _ = write!(css, "\n  /* {} */", safe(label));
        for (token, value) in tokens {
            let _ = write!(css, "\n  /* {token}: {}; */", safe(value));
        }
    }
    css.push_str("\n}\n");
    css
}

/// A CSS custom property and its value: `("--p-accent", "#2a76dd")`.
type Token = (String, String);

/// A label (where the tokens come from) and its tokens.
type TokenGroup = (String, Vec<Token>);

/// The tokens a theme's documents get: those its reveal theme sets on `:root`, then those
/// of each stylesheet entry (parent-first), each group labelled. A token appears once,
/// in the last group that sets it (that value wins in the cascade); empty groups are left
/// out. A stylesheet that cannot be read (a parent still being edited) is skipped with a
/// warning: the listing is a convenience, and `theme check` reports the problem.
fn token_groups(theme: &Theme) -> Vec<TokenGroup> {
    let mut groups = Vec::new();
    let reveal_theme = theme.reveal_theme();
    if reveal_theme != "none" {
        match crate::vendor::get_str(&format!("vendor/reveal/dist/theme/{reveal_theme}.css")) {
            Ok(css) => groups.push((
                format!("reveal.js theme '{reveal_theme}'"),
                root_tokens(&css),
            )),
            Err(err) => tracing::warn!(
                "the tokens of the reveal.js theme '{reveal_theme}' are not listed: {err}"
            ),
        }
    }
    for dir in theme.chain.iter().rev() {
        for entry in &dir.manifest.styles.entries {
            let bytes = match path_problem(dir, entry) {
                Some(_) => None,
                None => dir.read(entry).ok().flatten(),
            };
            let Some(bytes) = bytes else {
                tracing::warn!(
                    "theme '{}': the style entry '{entry}' cannot be read, so its tokens are \
                     not listed (`presenter theme check` says why)",
                    dir.name
                );
                continue;
            };
            let label = format!("theme '{}': {entry}", dir.name);
            groups.push((label, root_tokens(&String::from_utf8_lossy(&bytes))));
        }
    }
    let later: Vec<Vec<String>> = (0..groups.len())
        .map(|index| {
            groups[index + 1..]
                .iter()
                .flat_map(|(_, tokens)| tokens.iter().map(|(token, _)| token.clone()))
                .collect()
        })
        .collect();
    for ((_, tokens), later) in groups.iter_mut().zip(&later) {
        tokens.retain(|(token, _)| !later.contains(token));
    }
    groups.retain(|(_, tokens)| !tokens.is_empty());
    groups
}

/// The custom properties (`--name: value`) declared in the `:root { … }` rules of `css`,
/// in order, each once (the first declaration), with comments removed and whitespace in
/// values collapsed.
fn root_tokens(css: &str) -> Vec<Token> {
    let css = strip_css_comments(css);
    let mut tokens: Vec<Token> = Vec::new();
    let mut rest = css.as_str();
    while let Some(at) = rest.find(":root") {
        rest = &rest[at + ":root".len()..];
        let Some(body) = rest.trim_start().strip_prefix('{') else {
            continue;
        };
        let Some(end) = body.find('}') else {
            break;
        };
        for declaration in body[..end].split(';') {
            let Some((token, value)) = declaration.split_once(':') else {
                continue;
            };
            let token = token.trim();
            if token.starts_with("--") && !tokens.iter().any(|(known, _)| known == token) {
                let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
                tokens.push((token.to_string(), value));
            }
        }
        rest = &body[end + 1..];
    }
    tokens
}

/// `css` without `/* … */` comments (an unterminated comment runs to the end).
fn strip_css_comments(css: &str) -> String {
    let mut text = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        text.push_str(&rest[..start]);
        rest = match rest[start + 2..].find("*/") {
            Some(end) => &rest[start + 2 + end + 2..],
            None => "",
        };
    }
    text.push_str(rest);
    text
}

/// What `new` prints: the files, the chain and the next steps.
fn new_message(dir: &Path, created: &Theme, parent: &Theme) -> String {
    let shown = dir.display().to_string();
    let word = shell_word(&shown);
    let parent_name = parent.name();
    let files = [
        (
            dir.join(MANIFEST_FILE).display().to_string(),
            "name, parent and stylesheets",
        ),
        (
            dir.join("styles").join("theme.css").display().to_string(),
            "the parent's tokens, commented out: uncomment one to override it",
        ),
        (
            format!("{}/", dir.join("templates").display()),
            "a template here replaces the parent's template of the same name",
        ),
    ];
    let width = files
        .iter()
        .map(|(path, _)| path.chars().count())
        .max()
        .unwrap_or(0);
    let mut text = format!(
        "Created the theme '{}' in {shown}, a child of '{parent_name}':",
        created.name()
    );
    for (path, purpose) in &files {
        let _ = write!(text, "\n  {path:<width$}  {purpose}");
    }
    let _ = write!(text, "\n{}\nNext steps:", chain_lines(created));
    if matches!(parent.chain[0].source, ThemeSource::Embedded { .. }) {
        let _ = write!(
            text,
            "\n  presenter theme eject {} <dir>  # a full copy of the parent to look at",
            shell_word(parent_name)
        );
    }
    let _ = write!(
        text,
        "\n  presenter build slides.md --theme {word}  # or [deck] theme = {} in presenter.toml\
         \n  presenter theme check {word}",
        toml_string(&shown)
    );
    text
}

// ---------------------------------------------------------------------------------------
// check

/// `theme check DIR`.
fn check(args: &ThemeCheckArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let dir = &args.dir;
    // Absolute, so that resolution finds this directory or fails: a relative spec that is
    // not a theme directory would fall back to the built-in theme of that name.
    let absolute = std::path::absolute(dir)
        .map_err(|err| UsageError(format!("theme check: {}: {err}", dir.display())))?;
    if let Some(why) = manifest_problem(&absolute)
        .map_err(|err| anyhow!("theme check: cannot read {}: {err}", dir.display()))?
    {
        // The argument names no theme: a usage error, like an unknown `eject` name.
        return Err(UsageError(format!(
            "theme check: {} {why}; a theme is a directory with a {MANIFEST_FILE} \
             (`presenter theme new <dir>` creates one)",
            dir.display()
        ))
        .into());
    }
    let spec = absolute
        .to_str()
        .ok_or_else(|| anyhow!("theme check: {} is not a valid UTF-8 path", dir.display()))?;
    // Errors name the theme by its spec; report the directory as the user wrote it rather
    // than the absolute path used for resolution.
    let theme = theme::resolve(spec, &[]).map_err(|err| match err {
        crate::error::Error::Theme { name, message } if name == spec => {
            crate::error::Error::Theme {
                name: dir.display().to_string(),
                message,
            }
        }
        other => other,
    })?;
    if !matches!(theme.chain[0].source, ThemeSource::Disk { .. }) {
        // Cannot happen: an absolute spec is never a built-in theme name.
        return Err(anyhow!(
            "theme check: {} resolved to a built-in theme",
            dir.display()
        ));
    }
    if !global.quiet {
        let header = format!("Theme '{}' in {}:", theme.name(), dir.display());
        print_stdout(&format!("{header}\n{}", chain_lines(&theme)))?;
    }
    let problems = problems(&theme)?;
    report_diagnostics(&problems, global);
    let summary = match problems.len() {
        0 => "No problems found.".to_string(),
        1 => "1 problem found.".to_string(),
        n => format!("{n} problems found."),
    };
    if !global.quiet {
        print_stdout(&summary)?;
    }
    Ok(if problems.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_BUILD_ERROR)
    })
}

/// Why `dir` is not a theme directory (an `is …`/`has …` clause), if it is not: it does
/// not exist, is not a directory, or has no file named exactly `theme.toml` (the rule
/// resolution applies on every platform; a `Theme.toml` is named in the message). Errors:
/// the directory cannot be read.
fn manifest_problem(dir: &Path) -> std::io::Result<Option<String>> {
    if !dir.is_dir() {
        let why = if fs::symlink_metadata(dir).is_ok() {
            "is not a directory"
        } else {
            "does not exist"
        };
        return Ok(Some(why.to_string()));
    }
    let names: Vec<String> = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    if names.iter().any(|name| name == MANIFEST_FILE) && dir.join(MANIFEST_FILE).is_file() {
        return Ok(None);
    }
    let mut why = format!("has no {MANIFEST_FILE}");
    let variant = names
        .iter()
        .filter(|name| *name != MANIFEST_FILE && name.eq_ignore_ascii_case(MANIFEST_FILE))
        .min();
    if let Some(variant) = variant {
        let _ = write!(
            why,
            " (it has {variant}, but the name must be {MANIFEST_FILE} exactly)"
        );
    }
    Ok(Some(why))
}

/// Every problem of a resolved theme (see the module docs), chain order.
fn problems(theme: &Theme) -> anyhow::Result<Diagnostics> {
    let mut found = Diagnostics::new();
    let env = theme.environment(false)?;
    for (index, dir) in theme.chain.iter().enumerate() {
        let manifest = ManifestFile::read(dir);
        entry_problems(&manifest, &mut found);
        layout_problems(&manifest, &theme.chain[index..], &mut found);
        template_problems(&env, dir, &mut found);
    }
    plugin_problems(theme, &mut found);
    if found.is_empty() {
        // The checks above cover every way these fail except an unreadable file.
        for result in [theme.style_entries(), theme.script_entries()] {
            if let Err(err) = result {
                found.push(Diagnostic::error("theme-entry", err.to_string()));
            }
        }
    }
    Ok(found)
}

/// A theme's `theme.toml` for reporting: its path (disk themes only) and text.
struct ManifestFile<'a> {
    dir: &'a ThemeDir,
    path: Option<PathBuf>,
    text: String,
}

impl<'a> ManifestFile<'a> {
    fn read(dir: &'a ThemeDir) -> ManifestFile<'a> {
        let path = match &dir.source {
            ThemeSource::Disk { dir } => Some(dir.join(MANIFEST_FILE)),
            ThemeSource::Embedded { .. } => None,
        };
        let text = dir
            .read(MANIFEST_FILE)
            .ok()
            .flatten()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        ManifestFile { dir, path, text }
    }

    /// An error about this manifest at the line `line` finds (disk themes); a built-in
    /// theme's message is prefixed with its name instead.
    fn problem(&self, code: &'static str, message: String, line: Option<usize>) -> Diagnostic {
        let Some(path) = &self.path else {
            let message = format!("built-in theme '{}': {message}", self.dir.name);
            return Diagnostic::error(code, message);
        };
        let diagnostic = Diagnostic::error(code, message).with_path(path);
        match line {
            Some(line) => diagnostic.with_span(line_span(line)),
            None => diagnostic,
        }
    }

    /// The line where `value` is written as a quoted TOML string in the value of `key` in
    /// `[table]` (an array may span several lines), comments ignored. When it is not
    /// written that way (a dotted key, an inline table), the first line that has it
    /// outside a comment.
    fn line_of_value(&self, table: &str, key: &str, value: &str) -> Option<usize> {
        let quoted = [format!("\"{value}\""), format!("'{value}'")];
        let has_value = |code: &str| quoted.iter().any(|q| code.contains(q.as_str()));
        let mut current: Option<String> = None;
        let mut in_value = false;
        let scoped = line_where(&self.text, |line| {
            let code = toml_code(line).trim();
            if let Some(header) = table_header(code) {
                current = Some(header);
                in_value = false;
                return false;
            }
            if let Some((name, rest)) = split_key(code) {
                in_value = current.as_deref() == Some(table) && is_key(name, key);
                return in_value && has_value(rest);
            }
            in_value && has_value(code)
        });
        scoped.or_else(|| line_where(&self.text, |line| has_value(toml_code(line))))
    }

    /// The line of `key = …` (bare or quoted key) in the table `[table]`.
    fn line_of_key(&self, table: &str, key: &str) -> Option<usize> {
        let mut current: Option<String> = None;
        line_where(&self.text, |line| {
            let code = toml_code(line).trim();
            if let Some(header) = table_header(code) {
                current = Some(header);
                return false;
            }
            current.as_deref() == Some(table)
                && split_key(code).is_some_and(|(name, _)| is_key(name, key))
        })
    }
}

/// The 1-based number of the first line of `text` for which `matches` is true.
fn line_where(text: &str, matches: impl FnMut(&str) -> bool) -> Option<usize> {
    text.lines().position(matches).map(|index| index + 1)
}

/// `line` without its comment: the text before the first `#` outside a TOML string.
fn toml_code(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (index, c) in line.char_indices() {
        match quote {
            Some('"') if escaped => escaped = false,
            Some('"') if c == '\\' => escaped = true,
            Some(open) if c == open => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' => return &line[..index],
            None => {}
        }
    }
    line
}

/// The table a header line (comment removed, trimmed) opens, without brackets or spaces:
/// `[ styles ]` → `styles`. `None` for any other line.
fn table_header(code: &str) -> Option<String> {
    let inner = code.strip_prefix('[')?.strip_suffix(']')?;
    let inner = inner.trim_start_matches('[').trim_end_matches(']');
    Some(inner.chars().filter(|c| !c.is_whitespace()).collect())
}

/// A `key = value` line (comment removed) split at its first `=` outside a string: the
/// trimmed key and the rest. `None` for a line without one (an array continuation line).
fn split_key(code: &str) -> Option<(&str, &str)> {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (index, c) in code.char_indices() {
        match quote {
            Some('"') if escaped => escaped = false,
            Some('"') if c == '\\' => escaped = true,
            Some(open) if c == open => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '=' => return Some((code[..index].trim(), &code[index + 1..])),
            None => {}
        }
    }
    None
}

/// True if the written key `name` (bare, `"quoted"` or `'quoted'`) is `key`.
fn is_key(name: &str, key: &str) -> bool {
    name == key
        || [('"', '"'), ('\'', '\'')].iter().any(|(open, close)| {
            name.strip_prefix(*open)
                .and_then(|rest| rest.strip_suffix(*close))
                .is_some_and(|inner| inner == key)
        })
}

/// A one-line span.
fn line_span(line: usize) -> SourceSpan {
    SourceSpan {
        start_line: line,
        end_line: line,
        cell: None,
    }
}

/// `[styles]`/`[scripts]` entries and a `highlight_style` file that cannot be read.
fn entry_problems(manifest: &ManifestFile<'_>, found: &mut Diagnostics) {
    let theme = &manifest.dir.manifest;
    let lists = [
        ("styles", "theme-style-entry", &theme.styles.entries),
        ("scripts", "theme-script-entry", &theme.scripts.entries),
    ];
    for (table, code, entries) in lists {
        for entry in entries {
            if let Some(problem) = path_problem(manifest.dir, entry) {
                let message = format!("[{table}] entry '{entry}' {problem}");
                let line = manifest.line_of_value(table, "entries", entry);
                found.push(manifest.problem(code, message, line));
            }
        }
    }
    if let Some(style) = &theme.styles.highlight_style
        && !BUNDLED_HIGHLIGHT_STYLES.contains(&style.as_str())
        && let Some(problem) = path_problem(manifest.dir, style)
    {
        let message = format!(
            "[styles] highlight_style '{style}' (not monokai, zenburn or none, so a CSS file) {problem}"
        );
        let line = manifest.line_of_value("styles", "highlight_style", style);
        found.push(manifest.problem("theme-highlight-style", message, line));
    }
}

/// Why `path` (from `theme.toml`) is not a file of `dir`, if it is not, as an `is …`
/// clause. The renderer's rules: relative, inside the theme directory, an existing file
/// (exact name).
fn path_problem(dir: &ThemeDir, path: &str) -> Option<&'static str> {
    if path.starts_with(['/', '\\']) || Path::new(path).has_root() {
        return Some("is not a relative path (paths are relative to the theme directory)");
    }
    match normalize_rel(path) {
        None => Some("is outside the theme directory"),
        Some(rel) if rel.is_empty() => Some("is not a file"),
        Some(_) if !dir.exists(path) => {
            Some("is not found (paths are relative to the theme directory)")
        }
        Some(_) => None,
    }
}

/// `[layouts]` entries of `manifest` without `templates/layouts/<name>.html` in `chain`
/// (the manifest's theme and its parents).
fn layout_problems(manifest: &ManifestFile<'_>, chain: &[ThemeDir], found: &mut Diagnostics) {
    let lineage = Theme {
        chain: chain.to_vec(),
    };
    for layout in manifest.dir.manifest.layouts.keys() {
        if lineage.has_layout(layout) {
            continue;
        }
        let message = if is_layout_name(layout) {
            format!(
                "[layouts] documents the layout '{layout}', but neither this theme nor a \
                 parent has templates/layouts/{layout}.html"
            )
        } else {
            format!(
                "[layouts] '{layout}' is not a layout name (one file name, without '/', '\\' \
                 or a leading '.')"
            )
        };
        let line = manifest.line_of_key("layouts", layout);
        found.push(manifest.problem("theme-layout", message, line));
    }
}

/// Templates of a disk theme that do not compile (syntax errors, unreadable files).
/// Hidden files (`.gitkeep`) are skipped.
fn template_problems(env: &minijinja::Environment<'_>, dir: &ThemeDir, found: &mut Diagnostics) {
    let ThemeSource::Disk { dir: root } = &dir.source else {
        return;
    };
    for path in dir.list("templates") {
        let Some(rel) = path.strip_prefix("templates/") else {
            continue;
        };
        if rel.split('/').any(|segment| segment.starts_with('.')) {
            continue;
        }
        let Err(err) = env.get_template(&format!("@{}/{rel}", dir.name)) else {
            continue;
        };
        let message = match err.detail() {
            Some(detail) => format!("{}: {detail}", err.kind()),
            None => err.kind().to_string(),
        };
        let mut diagnostic =
            Diagnostic::error("theme-template", message).with_path(root.join(&path));
        if let Some(line) = err.line() {
            diagnostic = diagnostic.with_span(line_span(line));
        }
        found.push(diagnostic);
    }
}

/// An invalid merged `[reveal] plugins` list, reported on the theme that sets it.
///
/// This is the check `build` makes in `pipeline::check_theme_plugins` (private to
/// src/pipeline.rs), which returns the first failure as an error instead of a located
/// diagnostic: keep the two in step. Follow-up: one `pub(crate)` probe in the pipeline
/// for both (pipeline.rs is not this module's file).
fn plugin_problems(theme: &Theme, found: &mut Diagnostics) {
    let probe = Config {
        reveal: theme.reveal_defaults(),
        ..Config::default()
    };
    let Err(err) = probe.validate_plugins() else {
        return;
    };
    let message = match err {
        Error::Config(message) => message,
        other => other.to_string(),
    };
    let Some(dir) = theme
        .chain
        .iter()
        .find(|dir| dir.manifest.reveal.contains_key("plugins"))
    else {
        return;
    };
    let manifest = ManifestFile::read(dir);
    let line = manifest.line_of_key("reveal", "plugins");
    found.push(manifest.problem("theme-plugins", format!("[reveal] {message}"), line));
}

// ---------------------------------------------------------------------------------------
// shared

/// `dir` as the directory `new` and `eject` check and write: absolute and lexically
/// normalized (`a/..` is the current directory, whether or not `a` exists), so that the
/// checks and the writes see the same directory.
fn absolute_target(dir: &Path) -> anyhow::Result<PathBuf> {
    let absolute =
        std::path::absolute(dir).map_err(|err| UsageError(format!("{}: {err}", dir.display())))?;
    Ok(normalize_lexically(&absolute))
}

/// `dir` (as given) for a message about `target`, the directory it names, with the
/// directory added when they differ in more than being relative (`a/..`).
fn describe(dir: &Path, target: &Path) -> String {
    match std::path::absolute(dir) {
        Ok(absolute) if absolute == target => dir.display().to_string(),
        _ => format!("{} (that is {})", dir.display(), target.display()),
    }
}

/// The theme name for a new theme directory `target` ([`absolute_target`] of `dir`): its
/// last component; and `target` as a string (the spec to resolve it with). Usage errors
/// when the name cannot be a theme name ([`check_theme_name`]) or the path is not UTF-8.
fn name_and_spec(target: &Path, dir: &Path) -> anyhow::Result<(String, String)> {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            UsageError(format!(
                "{}: the theme is named after its directory, which needs a UTF-8 name",
                dir.display()
            ))
        })?;
    check_theme_name(name).map_err(|problem| {
        UsageError(format!(
            "{}: the theme is named after its directory: {problem}",
            dir.display()
        ))
    })?;
    let spec = target
        .to_str()
        .ok_or_else(|| UsageError(format!("{} is not a valid UTF-8 path", dir.display())))?;
    Ok((name.to_string(), spec.to_string()))
}

/// A new theme called `name` (in `dir`) must not share its name with a theme of its parent
/// chain `parents` (`@<name>/` template names would be ambiguous).
fn check_name_is_free(name: &str, parents: &[ThemeDir], dir: &Path) -> anyhow::Result<()> {
    if !parents.iter().any(|parent| parent.name == name) {
        return Ok(());
    }
    Err(UsageError(format!(
        "{}: the theme would be named '{name}' after its directory, like a theme it extends; \
         names in a chain must differ, so choose another directory name",
        dir.display()
    ))
    .into())
}

/// Writes `files` (`/`-separated paths relative to `dir`, with their contents) into the
/// new theme directory `dir` (absolute; absent, or an empty directory), then resolves the
/// theme there (`spec` is `dir` as a string). On any failure it removes what it wrote
/// ([`Written::undo`]) before returning the error, so `dir` is as it was and a retry is
/// not refused. It never replaces an existing file.
fn create_theme(dir: &Path, files: &[(String, Vec<u8>)], spec: &str) -> anyhow::Result<Theme> {
    let mut written = Written::default();
    let result =
        write_theme_files(dir, files, &mut written).and_then(|()| Ok(theme::resolve(spec, &[])?));
    if result.is_err() {
        written.undo();
    }
    result
}

/// The writing part of [`create_theme`], recording what it creates in `written`.
fn write_theme_files(
    dir: &Path,
    files: &[(String, Vec<u8>)],
    written: &mut Written,
) -> anyhow::Result<()> {
    written.create_dirs(dir)?;
    for (rel, bytes) in files {
        let path = rel
            .split('/')
            .fold(dir.to_path_buf(), |path, part| path.join(part));
        if let Some(parent) = path.parent() {
            written.create_dirs(parent)?;
        }
        written.create_file(&path, bytes)?;
    }
    Ok(())
}

/// The files and directories [`create_theme`] created, to remove them if it fails.
#[derive(Debug, Default)]
struct Written {
    /// Files it created.
    files: Vec<PathBuf>,
    /// Directories it created (or was about to create).
    dirs: Vec<PathBuf>,
}

impl Written {
    /// Creates the directory `dir` and its missing ancestors, recording them first.
    fn create_dirs(&mut self, dir: &Path) -> anyhow::Result<()> {
        let missing = dir
            .ancestors()
            .take_while(|ancestor| fs::symlink_metadata(ancestor).is_err());
        self.dirs.extend(missing.map(Path::to_path_buf));
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))
    }

    /// Creates the file `path`, which must not exist, with `bytes`.
    fn create_file(&mut self, path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .with_context(|| format!("cannot create {}", path.display()))?;
        self.files.push(path.to_path_buf());
        file.write_all(bytes)
            .with_context(|| format!("cannot write {}", path.display()))
    }

    /// Removes the recorded files, then the recorded directories that are empty, deepest
    /// first. Best effort: what cannot be removed is left (and logged).
    fn undo(mut self) {
        for file in &self.files {
            if let Err(err) = fs::remove_file(file) {
                tracing::warn!("cannot remove {}: {err}", file.display());
            }
        }
        self.dirs
            .sort_by_key(|dir| std::cmp::Reverse(dir.components().count()));
        for dir in &self.dirs {
            if let Err(err) = fs::remove_dir(dir) {
                tracing::debug!("not removing {}: {err}", dir.display());
            }
        }
    }
}

/// `text` as one word of a printed shell command: unchanged when it only has characters
/// that common shells do not treat specially, else in single quotes (a `'` written as
/// `'\''`, the POSIX way).
fn shell_word(text: &str) -> String {
    let plain = !text.is_empty()
        && text.chars().all(|c| {
            c.is_ascii_alphanumeric() || "-_./,:@%+".contains(c) || (cfg!(windows) && c == '\\')
        });
    if plain {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

/// `value` as a TOML string literal (quoted and escaped).
fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_string()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renamed_manifest_replaces_only_the_top_level_name() {
        let manifest = "# name = \"comment\"\nname = \"default\"  # x\r\ndescription = \"d\"\n\n\
                        [layouts]\nname = \"a layout called name\"\n";
        let renamed = renamed_manifest(manifest, "my theme").unwrap();
        assert_eq!(
            renamed,
            "# name = \"comment\"\nname = \"my theme\"\r\ndescription = \"d\"\n\n\
             [layouts]\nname = \"a layout called name\"\n"
        );
        let quoted = renamed_manifest("name = 'x'\n", "it's \"q\"").unwrap();
        let parsed = ThemeManifest::from_toml_str(&quoted, "t", "t").unwrap();
        assert_eq!(parsed.name, "it's \"q\"");
        assert!(renamed_manifest("[styles]\nname = \"x\"\n", "y").is_err());
    }

    #[test]
    fn templates_follow_the_rename() {
        let source = b"{% extends \"@default/deck.html\" %}{% include \"@defaults/x\" %}".to_vec();
        assert_eq!(
            retarget_template(source, "default", "mine"),
            b"{% extends \"@mine/deck.html\" %}{% include \"@defaults/x\" %}"
        );
        assert_eq!(
            retarget_template(vec![0xff, b'@'], "default", "x"),
            [0xff, b'@']
        );
    }

    #[test]
    fn the_builtin_manifests_can_be_renamed() {
        for name in theme::builtin_names() {
            let builtin = theme::resolve(&name, &[]).unwrap();
            let text =
                String::from_utf8(builtin.chain[0].read(MANIFEST_FILE).unwrap().unwrap()).unwrap();
            let renamed = renamed_manifest(&text, "ejected").unwrap();
            let parsed = ThemeManifest::from_toml_str(&renamed, "t", "t").unwrap();
            assert_eq!(
                parsed,
                ThemeManifest {
                    name: "ejected".into(),
                    ..builtin.chain[0].manifest.clone()
                }
            );
        }
    }

    #[test]
    fn eject_builtin_writes_a_resolvable_copy_and_refuses_existing_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nested").join("copy");
        let ejected = eject_builtin("default", &dir).unwrap();
        assert_eq!(ejected.name(), "copy");
        assert!(matches!(ejected.chain[0].source, ThemeSource::Disk { .. }));
        let builtin = theme::resolve("default", &[]).unwrap();
        let expected = builtin.chain[0].list("");
        assert!(expected.iter().any(|rel| rel == BUILTIN_THEME_README));
        assert_eq!(ejected.chain[0].list(""), expected, "every file");
        let readme = fs::read_to_string(dir.join(BUILTIN_THEME_README)).unwrap();
        assert!(
            readme.starts_with(
                "> **A copy.** `presenter theme eject` copied presenter's built-in theme \
                 `default` here as the theme `copy`.\n"
            ),
            "{readme}"
        );
        let err = eject_builtin("default", &dir).unwrap_err();
        assert!(err.is::<UsageError>(), "{err:#}");
        let err = eject_builtin("nope", &tmp.path().join("other")).unwrap_err();
        assert!(err.is::<UsageError>(), "{err:#}");
    }

    #[test]
    fn copied_readmes_say_what_they_document_and_unlink_repository_paths() {
        let original = "# The `default` theme\n\nThe root ([`docs/spec/theme-format.md`](../../../docs/spec/theme-format.md)).\n\
                        See [Tokens](#tokens), [grass](https://crates.io/crates/grass), \
                        [`styles/theme.css`](styles/theme.css) and [`examples/notebook/`](../../../examples/notebook/).\n";
        let copied = copied_readme(original.as_bytes().to_vec(), "default", "mine", "cmd");
        let copied = String::from_utf8(copied).unwrap();
        let (note, body) = copied.split_once("\n\n").unwrap();
        assert_eq!(
            note,
            "> **A copy.** `cmd` copied presenter's built-in theme `default` here as the theme \
             `mine`.\n> The text below documents the built-in theme: its tokens, layouts and \
             classes are this copy's too, until you change them. Paths outside this directory, \
             such as `docs/spec/theme-format.md`, are in presenter's source repository."
        );
        assert_eq!(
            body,
            "# The `default` theme\n\nThe root (`docs/spec/theme-format.md`).\n\
             See [Tokens](#tokens), [grass](https://crates.io/crates/grass), \
             [`styles/theme.css`](styles/theme.css) and `examples/notebook/`.\n"
        );
        // The real README: no link leaves the copy.
        let builtin = theme::resolve("default", &[]).unwrap();
        let readme = builtin.chain[0]
            .read(BUILTIN_THEME_README)
            .unwrap()
            .unwrap();
        let copied = String::from_utf8(copied_readme(readme, "default", "x", "cmd")).unwrap();
        assert!(!copied.contains("](../"), "{copied}");
        // Bytes that are not UTF-8 are copied as they are.
        assert_eq!(copied_readme(vec![0xff, b'x'], "a", "b", "c"), [0xff, b'x']);
    }

    #[test]
    fn create_theme_removes_what_it_wrote_when_it_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let file = |rel: &str, text: &str| (rel.to_string(), text.as_bytes().to_vec());
        // A write fails (a file where a directory must go): the new directory and its
        // missing parent are gone again.
        let dir = tmp.path().join("parent").join("theme");
        let spec = dir.to_str().unwrap().to_string();
        let files = [file("a.txt", "x"), file("a.txt/b.txt", "y")];
        assert!(create_theme(&dir, &files, &spec).is_err());
        assert!(!tmp.path().join("parent").exists());
        // The theme does not resolve (a root theme without templates): an empty directory
        // that was there stays, empty.
        let dir = tmp.path().join("empty");
        fs::create_dir(&dir).unwrap();
        let spec = dir.to_str().unwrap().to_string();
        let files = [
            file(MANIFEST_FILE, "name = \"empty\"\n"),
            file("styles/x.css", ""),
        ];
        let err = create_theme(&dir, &files, &spec).unwrap_err();
        assert!(
            format!("{err:#}").contains("missing required files"),
            "{err:#}"
        );
        assert!(dir.is_dir());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        // It never replaces a file.
        let dir = tmp.path().join("taken");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("a.txt"), "mine").unwrap();
        let spec = dir.to_str().unwrap().to_string();
        assert!(create_theme(&dir, &[file("a.txt", "theirs")], &spec).is_err());
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "mine");
    }

    #[test]
    fn root_tokens_reads_custom_properties_of_root_rules() {
        let css = "/* :root { --commented: 1; } */\n\
                   :root {\n  --a: 1em; /* note: x */\n  --b:  \"Source Sans\",\n    serif;\n  color: red;\n}\n\
                   :root.dark { --dark: 1; }\n\
                   .reveal { --not-root: 2; }\n\
                   :root{--a:2em;--c:#fff}";
        assert_eq!(
            root_tokens(css),
            [
                ("--a".to_string(), "1em".to_string()),
                ("--b".to_string(), "\"Source Sans\", serif".to_string()),
                ("--c".to_string(), "#fff".to_string()),
            ]
        );
        assert_eq!(strip_css_comments("a/* b */c/* d"), "ac");
        assert!(root_tokens(":root { --a: 1").is_empty());
    }

    #[test]
    fn token_groups_list_each_token_once_where_it_wins() {
        let default = theme::resolve("default", &[]).unwrap();
        let groups = token_groups(&default);
        let reveal_theme = default.reveal_theme();
        assert_eq!(groups[0].0, format!("reveal.js theme '{reveal_theme}'"));
        let all: Vec<&str> = groups
            .iter()
            .flat_map(|(_, tokens)| tokens.iter().map(|(t, _)| t.as_str()))
            .collect();
        // Every stock reveal theme sets these; the default theme may override them.
        for token in ["--r-main-color", "--r-background-color", "--r-link-color"] {
            assert!(all.contains(&token), "{token} in {all:?}");
        }
        for token in &all {
            assert_eq!(all.iter().filter(|t| t == &token).count(), 1, "{token}");
        }
    }

    #[test]
    fn relative_paths_climb_to_the_common_ancestor() {
        let rel = |from: &str, to: &str| relative_path(Path::new(from), Path::new(to));
        assert_eq!(rel("/a/b/new", "/a/b/base").as_deref(), Some("../base"));
        assert_eq!(rel("/a/b/new", "/a/c/d").as_deref(), Some("../../c/d"));
        assert_eq!(rel("/a/b", "/a/b").as_deref(), Some("."));
        assert_eq!(rel("/a/b", "/a/b/c").as_deref(), Some("c"));
        assert_eq!(rel("a", "/a").as_deref(), None);
    }

    #[test]
    fn canonical_destination_keeps_the_missing_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = fs::canonicalize(tmp.path()).unwrap();
        let path = tmp.path().join("new").join("theme");
        assert_eq!(
            canonical_destination(&path).unwrap(),
            canonical.join("new").join("theme")
        );
        assert_eq!(canonical_destination(tmp.path()).unwrap(), canonical);
    }

    fn manifest_file<'a>(dir: &'a ThemeDir, text: &str) -> ManifestFile<'a> {
        ManifestFile {
            dir,
            path: None,
            text: text.to_string(),
        }
    }

    #[test]
    fn manifest_lines_are_found_by_value_and_by_key() {
        let dir = theme::resolve("default", &[]).unwrap().chain[0].clone();
        let manifest = manifest_file(
            &dir,
            "name = \"t\"\n[styles]\nentries = ['a.css', \"b.css\"]\n[layouts]\n\
             \"quote\" = \"q\"\ntitle= \"t\"\n[vars]\ntitle = 1\n",
        );
        assert_eq!(
            manifest.line_of_value("styles", "entries", "b.css"),
            Some(3)
        );
        assert_eq!(
            manifest.line_of_value("styles", "entries", "a.css"),
            Some(3)
        );
        assert_eq!(manifest.line_of_value("styles", "entries", "c.css"), None);
        assert_eq!(manifest.line_of_key("layouts", "quote"), Some(5));
        assert_eq!(manifest.line_of_key("layouts", "title"), Some(6));
        assert_eq!(manifest.line_of_key("vars", "title"), Some(8));
        assert_eq!(manifest.line_of_key("layouts", "missing"), None);
        // Built-in themes have no path: the message names the theme, no line.
        let problem = manifest.problem("x", "boom".into(), Some(3));
        assert_eq!(problem.to_string(), "error: built-in theme 'default': boom");
    }

    #[test]
    fn manifest_lines_skip_comments_and_follow_tables_and_keys() {
        let dir = theme::resolve("default", &[]).unwrap().chain[0].clone();
        let manifest = manifest_file(
            &dir,
            "# entries = [\"x.js\"]\n\
             [styles]\n\
             entries = [\"a.css\"]  # not \"x.js\"\n\
             highlight_style = \"x.js\"\n\
             [ scripts ]  # the scripts\n\
             # entries = [\"x.js\"]\n\
             other = \"x.js\"\n\
             entries = [\n\
             \x20 \"y.js\",  # \"x.js\" = 1\n\
             \x20 'x.js',\n\
             ]\n\
             [vars]\n\
             \"a#b\" = \"x.js\"\n",
        );
        assert_eq!(
            manifest.line_of_value("scripts", "entries", "x.js"),
            Some(10)
        );
        assert_eq!(
            manifest.line_of_value("scripts", "entries", "y.js"),
            Some(9)
        );
        assert_eq!(
            manifest.line_of_value("styles", "highlight_style", "x.js"),
            Some(4)
        );
        // Not in the table (a dotted key, say): the first line that has it as code.
        assert_eq!(manifest.line_of_value("reveal", "plugins", "x.js"), Some(4));
        assert_eq!(manifest.line_of_key("scripts", "other"), Some(7));
        assert_eq!(manifest.line_of_key("vars", "a#b"), Some(13));
        assert_eq!(toml_code("a = \"#\" # c"), "a = \"#\" ");
        assert_eq!(toml_code("a = 'x\\' # c"), "a = 'x\\' ");
        assert_eq!(toml_code("a = \"x\\\"#\" # c"), "a = \"x\\\"#\" ");
        assert_eq!(table_header("[[ a.b ]]").as_deref(), Some("a.b"));
        assert_eq!(table_header("\"x\","), None);
        assert_eq!(split_key("\"a = b\","), None);
        assert_eq!(split_key("'k' = 1"), Some(("'k'", " 1")));
    }

    #[test]
    fn shell_words_are_quoted_when_needed() {
        assert_eq!(shell_word("themes/my-theme_2.0"), "themes/my-theme_2.0");
        assert_eq!(shell_word("full copy"), "'full copy'");
        assert_eq!(shell_word("it's"), r"'it'\''s'");
        assert_eq!(shell_word("$HOME"), "'$HOME'");
        assert_eq!(shell_word("~/x"), "'~/x'");
        assert_eq!(shell_word(""), "''");
    }
}
