//! `presenter theme list/eject/new/check` (cli.md `theme`, theme-format.md §1–§3): the
//! theme-iteration workflow.
//!
//! - `list` prints the built-in themes with their `description`, then, when a
//!   `presenter.toml` is found from the current directory (or given with `--config`), the
//!   theme its `[deck] theme` resolves to: the chain child → root and where each theme
//!   lives. The theme is searched in that file's directory, then the current directory.
//! - `eject NAME DIR` copies every embedded file of the built-in theme `NAME` into `DIR`,
//!   which must not exist. The copy is named after the directory: `name` in `theme.toml`
//!   (with a comment line on top saying where it came from) and the `@NAME/` template names
//!   in its templates change; everything else, `extends` included, is copied as is, so
//!   ejecting `default` gives a root theme that builds exactly like the built-in one.
//! - `new DIR [--extends NAME]` writes a minimal child theme into `DIR` (which must be
//!   empty or not exist): `theme.toml` (`name` from the directory, `extends`, `[styles]
//!   entries`), `styles/theme.css` listing the parent's tokens (the CSS custom properties
//!   that its reveal theme and its stylesheets set on `:root`) as comments, and
//!   `templates/.gitkeep`. `--extends` (default `default`) is a built-in theme name or a
//!   theme directory relative to the current directory; a directory is written to
//!   `theme.toml` relative to the new theme, as theme-format.md §2 requires.
//! - `check DIR` resolves the theme in `DIR` ([`theme::resolve()`]) and prints its chain,
//!   then reports every problem on stderr as `path:line: error: message`: style and script
//!   entries and a `highlight_style` file that do not exist, `[layouts]` entries without a
//!   template file in the chain, templates of the chain's directories that do not compile,
//!   and an invalid `[reveal] plugins` list. What resolution itself rejects (no
//!   `theme.toml`, unknown manifest keys, an invalid theme name, a missing parent, a cycle,
//!   required templates missing from the root theme) fails the command with that error.
//!
//! Exit codes: 0 on success; 1 when `check` finds problems, for theme errors and I/O
//! failures; 2 for usage errors (`eject` of a theme that is not built in, an existing
//! `eject` directory, a non-empty `new` directory, a directory name that cannot be a theme
//! name). With `-q`, `eject`, `new` and `check` print nothing on stdout.

use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, anyhow};

use super::{
    EXIT_BUILD_ERROR, GlobalArgs, ThemeArgs, ThemeCheckArgs, ThemeCommand, ThemeEjectArgs,
    ThemeNewArgs, UsageError, display_path, print_stdout, report_diagnostics,
};
use crate::assets::registry::normalize_lexically;
use crate::config::Config;
use crate::error::{Diagnostic, Diagnostics, Error};
use crate::ir::SourceSpan;
use crate::theme::manifest::check_theme_name;
use crate::theme::resolve::MANIFEST_FILE;
use crate::theme::{
    self, Theme, ThemeDir, ThemeManifest, ThemeSource, is_layout_name, normalize_rel,
};

/// The parent of `presenter theme new` without `--extends`.
const DEFAULT_PARENT: &str = "default";

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
    let spec = &config.deck.theme;
    let shown_path = display_path(&config_path);
    let mut roots: Vec<&Path> = Vec::with_capacity(2);
    if let Some(dir) = config_path.parent() {
        roots.push(dir);
    }
    roots.push(&cwd);
    let theme = theme::resolve(spec, &roots)
        .with_context(|| format!("{}: [deck] theme", shown_path.display()))?;
    print_stdout(&format!(
        "\nTheme '{}' for this directory ([deck] theme = {} in {}):\n{}",
        theme.name(),
        toml_string(spec),
        shown_path.display(),
        chain_lines(&theme)
    ))?;
    Ok(ExitCode::SUCCESS)
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
    let builtins = theme::builtin_names();
    if !builtins.contains(&args.name) {
        return Err(UsageError(format!(
            "theme eject: '{}' is not a built-in theme (built-in themes: {})",
            args.name,
            builtins.join(", ")
        ))
        .into());
    }
    if std::fs::symlink_metadata(&args.dir).is_ok() {
        return Err(UsageError(format!(
            "theme eject: {} already exists; name a directory that does not exist",
            args.dir.display()
        ))
        .into());
    }
    let (name, spec) = name_and_spec(&args.dir)?;
    let source = theme::resolve(&args.name, &[])?;
    check_name_is_free(&name, &source.chain[1..], &args.dir)?;
    let builtin = &source.chain[0];
    let files = builtin.list("");
    for rel in &files {
        let bytes = builtin
            .read(rel)?
            .ok_or_else(|| anyhow!("built-in theme '{}': cannot read {rel}", builtin.name))?;
        let bytes = if rel == MANIFEST_FILE {
            let renamed = renamed_manifest(&String::from_utf8(bytes)?, &name)?;
            format!(
                "# Ejected from the built-in theme '{}' by `presenter theme eject`.\n{renamed}",
                args.name
            )
            .into_bytes()
        } else if rel.starts_with("templates/") {
            retarget_template(bytes, &args.name, &name)
        } else {
            bytes
        };
        write_file(&args.dir, rel, &bytes)?;
    }
    let ejected = theme::resolve(&spec, &[])?;
    tracing::info!(theme = %args.name, dir = %args.dir.display(), files = files.len(), "ejected theme");
    if !global.quiet {
        print_stdout(&eject_message(args, builtin, &ejected, files.len()))?;
    }
    Ok(ExitCode::SUCCESS)
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
fn eject_message(
    args: &ThemeEjectArgs,
    builtin: &ThemeDir,
    ejected: &Theme,
    files: usize,
) -> String {
    let dir = args.dir.display().to_string();
    let name = ejected.name();
    let kind = match &builtin.manifest.extends {
        None => "It is a root theme (no `extends`) with every template: edit any file.".to_string(),
        Some(parent) => format!("It extends '{parent}', like the built-in theme."),
    };
    format!(
        "Ejected the built-in theme '{}' to {dir} as theme '{name}' ({files} files).\n\
         {kind}\n\
         Use it with:\n  presenter build slides.md --theme {dir}\n\
         or in presenter.toml (a path relative to that file):\n  [deck]\n  theme = {}\n\
         Check it after editing: presenter theme check {dir}",
        builtin.name,
        toml_string(&dir)
    )
}

// ---------------------------------------------------------------------------------------
// new

/// `theme new DIR [--extends NAME]`.
fn new(args: &ThemeNewArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    check_empty_or_absent(&args.dir)?;
    let (name, spec) = name_and_spec(&args.dir)?;
    let parent_spec = args.extends.as_deref().unwrap_or(DEFAULT_PARENT);
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let parent = theme::resolve(parent_spec, &[&cwd])
        .with_context(|| format!("theme new: --extends {parent_spec}"))?;
    check_name_is_free(&name, &parent.chain, &args.dir)?;
    std::fs::create_dir_all(&args.dir)
        .with_context(|| format!("cannot create {}", args.dir.display()))?;
    let extends = extends_value(&args.dir, parent_spec, &parent.chain[0])?;
    let manifest = new_manifest(&name, &extends);
    let css = new_stylesheet(&name, &parent)?;
    write_file(&args.dir, MANIFEST_FILE, manifest.as_bytes())?;
    write_file(&args.dir, "styles/theme.css", css.as_bytes())?;
    write_file(&args.dir, "templates/.gitkeep", b"")?;
    let created = theme::resolve(&spec, &[])?;
    tracing::info!(theme = %name, dir = %args.dir.display(), "created theme");
    if !global.quiet {
        print_stdout(&new_message(&args.dir, &created, &parent))?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `dir` must not exist or be an empty directory.
fn check_empty_or_absent(dir: &Path) -> anyhow::Result<()> {
    if std::fs::symlink_metadata(dir).is_err() {
        return Ok(());
    }
    let empty = dir.is_dir()
        && std::fs::read_dir(dir)
            .with_context(|| format!("cannot read {}", dir.display()))?
            .next()
            .is_none();
    if empty {
        return Ok(());
    }
    Err(UsageError(format!(
        "theme new: {} exists and is not an empty directory",
        dir.display()
    ))
    .into())
}

/// The `extends` value for a new theme in `dir` whose parent `spec` resolved to `parent`:
/// the built-in name, or the parent's directory relative to `dir` (`/`-separated).
fn extends_value(dir: &Path, spec: &str, parent: &ThemeDir) -> anyhow::Result<String> {
    let ThemeSource::Disk { dir: parent_dir } = &parent.source else {
        return Ok(spec.to_string());
    };
    let dir =
        std::fs::canonicalize(dir).with_context(|| format!("cannot open {}", dir.display()))?;
    match relative_path(&dir, parent_dir) {
        Some(relative) => Ok(relative),
        None => parent_dir
            .to_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("{} is not a valid UTF-8 path", parent_dir.display())),
    }
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
        r##"# Theme manifest (docs/spec/theme-format.md §2), created by `presenter theme new`.
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
fn new_stylesheet(name: &str, parent: &Theme) -> anyhow::Result<String> {
    let mut css = format!(
        "/*\n * Stylesheet of the theme '{name}', loaded after the stylesheets of '{}', so a\n \
         * plain rule here wins. Prefer overriding tokens (CSS custom properties) to overriding\n \
         * rules: uncomment a line below and change its value. The values shown are the\n \
         * parent's current ones.\n */\n:root {{",
        parent.name()
    );
    // Nothing may end the comments early.
    let safe = |text: &str| text.replace("*/", "* /");
    let groups = token_groups(parent)?;
    for (index, (label, tokens)) in groups.iter().enumerate() {
        if index > 0 {
            css.push('\n');
        }
        let _ = write!(css, "\n  /* {} */", safe(label));
        for (token, value) in tokens {
            let _ = write!(css, "\n  /* {token}: {}; */", safe(value));
        }
    }
    css.push_str("\n}\n");
    Ok(css)
}

/// A CSS custom property and its value: `("--p-accent", "#2a76dd")`.
type Token = (String, String);

/// A label (where the tokens come from) and its tokens.
type TokenGroup = (String, Vec<Token>);

/// The tokens a theme's documents get: those its reveal theme sets on `:root`, then those
/// of each stylesheet entry (parent-first), each group labelled. A token appears once,
/// in the last group that sets it (that value wins in the cascade); empty groups are left
/// out.
fn token_groups(theme: &Theme) -> anyhow::Result<Vec<TokenGroup>> {
    let mut groups = Vec::new();
    let reveal_theme = theme.reveal_theme();
    if reveal_theme != "none" {
        let css = crate::vendor::get_str(&format!("vendor/reveal/dist/theme/{reveal_theme}.css"))?;
        groups.push((
            format!("reveal.js theme '{reveal_theme}'"),
            root_tokens(&css),
        ));
    }
    for file in theme.style_entries()? {
        let css = String::from_utf8_lossy(&file.bytes);
        let label = format!("theme '{}': {}", file.theme, file.rel_path);
        groups.push((label, root_tokens(&css)));
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
    Ok(groups)
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
            "\n  presenter theme eject {parent_name} <dir>  # a full copy of the parent to look at"
        );
    }
    let _ = write!(
        text,
        "\n  presenter build slides.md --theme {shown}  # or [deck] theme = {} in presenter.toml\
         \n  presenter theme check {shown}",
        toml_string(&shown)
    );
    text
}

// ---------------------------------------------------------------------------------------
// check

/// `theme check DIR`.
fn check(args: &ThemeCheckArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let dir = &args.dir;
    if !dir.join(MANIFEST_FILE).is_file() {
        let why = if dir.is_dir() {
            format!("has no {MANIFEST_FILE}")
        } else if std::fs::symlink_metadata(dir).is_ok() {
            "is not a directory".to_string()
        } else {
            "does not exist".to_string()
        };
        return Err(anyhow!(
            "theme check: {} {why}; a theme is a directory with a {MANIFEST_FILE} \
             (`presenter theme new <dir>` creates one)",
            dir.display()
        ));
    }
    let spec = dir
        .to_str()
        .ok_or_else(|| anyhow!("theme check: {} is not a valid UTF-8 path", dir.display()))?;
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let theme = theme::resolve(spec, &[&cwd])?;
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

    /// The first line that contains `value` as a quoted TOML string.
    fn line_of_value(&self, value: &str) -> Option<usize> {
        let quoted = [format!("\"{value}\""), format!("'{value}'")];
        line_where(&self.text, |line| {
            quoted.iter().any(|q| line.contains(q.as_str()))
        })
    }

    /// The line of `key = …` (bare or quoted key) in the table `[table]`.
    fn line_of_key(&self, table: &str, key: &str) -> Option<usize> {
        let header = format!("[{table}]");
        let mut current = String::new();
        let keys = [key.to_string(), format!("\"{key}\""), format!("'{key}'")];
        line_where(&self.text, |line| {
            let line = line.trim();
            if line.starts_with('[') {
                current = line.split('#').next().unwrap_or("").replace(' ', "");
                return false;
            }
            current == header
                && keys.iter().any(|k| {
                    line.strip_prefix(k.as_str())
                        .is_some_and(|rest| rest.trim_start().starts_with('='))
                })
        })
    }
}

/// The 1-based number of the first line of `text` for which `matches` is true.
fn line_where(text: &str, matches: impl FnMut(&str) -> bool) -> Option<usize> {
    text.lines().position(matches).map(|index| index + 1)
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
        ("[styles] entry", "theme-style-entry", &theme.styles.entries),
        (
            "[scripts] entry",
            "theme-script-entry",
            &theme.scripts.entries,
        ),
    ];
    for (label, code, entries) in lists {
        for entry in entries {
            if let Some(problem) = path_problem(manifest.dir, entry) {
                let message = format!("{label} '{entry}' {problem}");
                found.push(manifest.problem(code, message, manifest.line_of_value(entry)));
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
        let line = manifest.line_of_value(style);
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

/// An invalid merged `[reveal] plugins` list, reported on the theme that sets it (the
/// check `build` makes).
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

/// The theme name for a new theme directory (its last component) and the directory as an
/// absolute path string (the spec to resolve it with). Usage errors when the name cannot
/// be a theme name ([`check_theme_name`]) or the path is not UTF-8.
fn name_and_spec(dir: &Path) -> anyhow::Result<(String, String)> {
    let absolute = normalize_lexically(&std::path::absolute(dir)?);
    let name = absolute
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
    let spec = absolute
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

/// Writes `bytes` to `dir/rel` (`/`-separated), creating parent directories.
fn write_file(dir: &Path, rel: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let path = rel
        .split('/')
        .fold(dir.to_path_buf(), |path, part| path.join(part));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::write(&path, bytes).with_context(|| format!("cannot write {}", path.display()))
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
        let groups = token_groups(&default).unwrap();
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
    fn manifest_lines_are_found_by_value_and_by_key() {
        let dir = theme::resolve("default", &[]).unwrap().chain[0].clone();
        let manifest = ManifestFile {
            dir: &dir,
            path: None,
            text: "name = \"t\"\n[styles]\nentries = ['a.css', \"b.css\"]\n[layouts]\n\
                   \"quote\" = \"q\"\ntitle= \"t\"\n[vars]\ntitle = 1\n"
                .to_string(),
        };
        assert_eq!(manifest.line_of_value("b.css"), Some(3));
        assert_eq!(manifest.line_of_value("a.css"), Some(3));
        assert_eq!(manifest.line_of_value("c.css"), None);
        assert_eq!(manifest.line_of_key("layouts", "quote"), Some(5));
        assert_eq!(manifest.line_of_key("layouts", "title"), Some(6));
        assert_eq!(manifest.line_of_key("vars", "title"), Some(8));
        assert_eq!(manifest.line_of_key("layouts", "missing"), None);
        // Built-in themes have no path: the message names the theme, no line.
        let problem = manifest.problem("x", "boom".into(), Some(3));
        assert_eq!(problem.to_string(), "error: built-in theme 'default': boom");
    }
}
