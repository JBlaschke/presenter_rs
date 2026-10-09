//! Themes: discovery, manifest, inheritance chain, template environment, styles.
//!
//! Contract: `docs/spec/theme-format.md`. Implemented by T13 (styles: T42).
//!
//! A [`Theme`] is the resolved inheritance chain (child first, root last) of [`ThemeDir`]s.
//! Each `ThemeDir` reads its files either from the embedded `assets/themes/<name>/` tree
//! (built-in themes, through [`crate::vendor`]) or from a directory on disk, behind one
//! interface ([`ThemeDir::read`], [`ThemeDir::exists`], [`ThemeDir::list`]). Disk themes are
//! read on every call, never cached, so `serve` always sees the latest edits. Lookups walk
//! the chain child → root: templates under `templates/`, assets under `assets/`.
//! Style and script entries are concatenated parent-first; `[reveal]` and `[vars]`
//! deep-merge with the child winning; `reveal_theme`/`highlight_style` are plain overrides.
//!
//! File names match exactly, including case, for both sources: the embedded tree has exact
//! keys, and disk lookups check the real directory entries, so `layouts/TITLE.html` does not
//! find `layouts/title.html` on a case-insensitive file system (macOS) either.
//!
//! **A disk theme never reaches outside its directory** (theme-format.md §3, audit F08). A
//! path that climbs out lexically (`../x`) finds nothing, and so does a symbolic link inside
//! the theme whose target is outside it (`assets/logo.png -> /etc/hosts`, a linked
//! `assets/shared/` directory elsewhere): every file is opened only when its canonical path
//! (links resolved) is below the canonical theme directory. Links that stay inside the theme
//! work. So a downloaded theme cannot embed other local files into a deck's output, and the
//! dev server cannot serve them. [`ThemeDir::list`] lists exactly the files
//! [`ThemeDir::read`] accepts.

pub mod manifest;
pub mod resolve;
pub mod styles;
pub mod templates;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use serde_json::{Map, Value};

use crate::config::Config;
use crate::error::{Diagnostic, Error, Result};
use crate::ir::SourceSpan;

pub use manifest::{ScriptsSection, StylesSection, ThemeManifest};
pub use resolve::{builtin_names, resolve};
use templates::TemplateFile;

/// The codes of the diagnostics [`Theme::problems`] and [`Theme::deck_problems`] return.
pub mod codes {
    /// A `[styles] entries` file that is missing or outside its theme (error).
    pub const THEME_STYLE_ENTRY: &str = "theme-style-entry";
    /// A `[scripts] entries` file that is missing or outside its theme (error).
    pub const THEME_SCRIPT_ENTRY: &str = "theme-script-entry";
    /// A path-valued `highlight_style` that is missing or outside its theme (error).
    pub const THEME_HIGHLIGHT_STYLE: &str = "theme-highlight-style";
    /// A template that does not compile (error, or warning when the deck does not use it).
    pub const THEME_TEMPLATE: &str = "theme-template";
    /// An invalid merged `[reveal] plugins` list (error).
    pub const THEME_PLUGINS: &str = "theme-plugins";
    /// A symbolic link whose target is outside its theme, which lookups ignore (warning).
    pub const THEME_LINK_OUTSIDE: &str = "theme-link-outside";
}

/// Templates (paths under `templates/`) the root theme of every chain must provide.
pub const REQUIRED_TEMPLATES: [&str; 5] = [
    "deck.html",
    "slide.html",
    "block.html",
    "cell.html",
    "layouts/default.html",
];

/// `reveal_theme` when no theme in the chain sets one.
pub const DEFAULT_REVEAL_THEME: &str = "white";

/// `highlight_style` when no theme in the chain sets one.
pub const DEFAULT_HIGHLIGHT_STYLE: &str = "monokai";

/// `highlight_style` values that name a built-in style rather than a file.
pub const BUILTIN_HIGHLIGHT_STYLES: [&str; 3] = ["monokai", "zenburn", "none"];

/// Where a theme's files live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeSource {
    /// A built-in theme in the embedded asset tree; `prefix` is its directory relative to
    /// `assets/`, with a trailing slash (`"themes/default/"`).
    Embedded {
        /// Path prefix of the theme's files in [`crate::vendor`].
        prefix: String,
    },
    /// A theme directory on disk (canonicalized by [`resolve()`]).
    Disk {
        /// The directory containing `theme.toml`.
        dir: PathBuf,
    },
}

/// One theme of a chain: its name, where its files are and its manifest.
#[derive(Debug, Clone)]
pub struct ThemeDir {
    /// The manifest's `name`.
    pub name: String,
    /// Where the files are read from.
    pub source: ThemeSource,
    /// The parsed `theme.toml`.
    pub manifest: ThemeManifest,
}

/// A file read from one theme of the chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeFile {
    /// Name of the theme the file came from.
    pub theme: String,
    /// Path relative to that theme's directory, as written in `theme.toml`
    /// (e.g. `styles/theme.css`).
    pub rel_path: String,
    /// File contents.
    pub bytes: Vec<u8>,
}

/// A resolved theme: the inheritance chain, child first, root last.
#[derive(Debug, Clone)]
pub struct Theme {
    /// The chain, ordered child → … → root. [`resolve()`] never returns an empty chain.
    pub chain: Vec<ThemeDir>,
}

impl ThemeDir {
    /// Reads the file at `rel` (relative to the theme directory, `/`-separated).
    ///
    /// `rel` is normalized lexically (`.` and empty segments dropped, `..` removes the
    /// previous segment); a path that is empty or climbs out of the theme directory yields
    /// `Ok(None)`, as does a missing file, a directory and a symbolic link (or a path through
    /// one) whose target is outside the theme directory. The file name must match exactly,
    /// including case (see the module docs). Disk themes are read from disk on every call.
    /// Fails with [`Error::Theme`] only when an existing file cannot be read.
    pub fn read(&self, rel: &str) -> Result<Option<Vec<u8>>> {
        let Some(rel) = normalize_rel(rel).filter(|r| !r.is_empty()) else {
            return Ok(None);
        };
        match &self.source {
            ThemeSource::Embedded { prefix } => {
                Ok(crate::vendor::get(&format!("{prefix}{rel}")).map(|data| data.into_owned()))
            }
            ThemeSource::Disk { dir } => {
                let Some(path) = disk_file(dir, &rel) else {
                    return Ok(None);
                };
                std::fs::read(&path).map(Some).map_err(|e| Error::Theme {
                    name: self.name.clone(),
                    message: format!("cannot read {}: {e}", path.display()),
                })
            }
        }
    }

    /// True if the file at `rel` exists (same path rules as [`ThemeDir::read`]).
    pub fn exists(&self, rel: &str) -> bool {
        let Some(rel) = normalize_rel(rel).filter(|r| !r.is_empty()) else {
            return false;
        };
        match &self.source {
            ThemeSource::Embedded { prefix } => {
                crate::vendor::get(&format!("{prefix}{rel}")).is_some()
            }
            ThemeSource::Disk { dir } => disk_file(dir, &rel).is_some(),
        }
    }

    /// Every file below the directory `rel_dir` (recursively; `""` for the whole theme),
    /// as `/`-separated paths relative to the theme directory, sorted. Every listed path is
    /// accepted by [`ThemeDir::read`]. A missing directory lists nothing.
    pub fn list(&self, rel_dir: &str) -> Vec<String> {
        let Some(rel_dir) = normalize_rel(rel_dir) else {
            return Vec::new();
        };
        let mut files = match &self.source {
            ThemeSource::Embedded { prefix } => {
                let start = if rel_dir.is_empty() {
                    prefix.clone()
                } else {
                    format!("{prefix}{rel_dir}/")
                };
                crate::vendor::list(&start)
                    .into_iter()
                    .filter_map(|path| path.strip_prefix(prefix.as_str()).map(str::to_string))
                    .collect()
            }
            ThemeSource::Disk { dir } => list_disk(dir, &rel_dir),
        };
        files.sort();
        files
    }

    /// True if `rel` names an existing file of this disk theme that [`ThemeDir::read`]
    /// refuses only because a symbolic link takes it out of the theme directory (module
    /// docs), so that messages can say why the file counts as missing.
    pub fn linked_outside(&self, rel: &str) -> bool {
        let Some(rel) = normalize_rel(rel).filter(|r| !r.is_empty()) else {
            return false;
        };
        match &self.source {
            ThemeSource::Disk { dir } => linked_outside(dir, &rel),
            ThemeSource::Embedded { .. } => false,
        }
    }

    /// Why the file at `rel` cannot be read: `missing`, or that a symbolic link takes it out
    /// of the theme ([`ThemeDir::linked_outside`]).
    fn missing_reason(&self, rel: &str, missing: &'static str) -> &'static str {
        if self.linked_outside(rel) {
            LINKED_OUTSIDE
        } else {
            missing
        }
    }

    /// Where the theme lives, for messages: the directory, or `built-in (themes/<name>/)`.
    pub fn location(&self) -> String {
        match &self.source {
            ThemeSource::Embedded { prefix } => format!("built-in ({prefix})"),
            ThemeSource::Disk { dir } => dir.display().to_string(),
        }
    }
}

/// `dir/rel` (`rel` normalized and non-empty) if it is a file whose name matches exactly and
/// that is inside `dir` once symbolic links are resolved ([`inside`]). A file that is there
/// but, through a symbolic link, outside `dir` is refused with a logged warning
/// ([`warn_linked_outside`]), so that a build that then fails on a missing file says why.
fn disk_file(dir: &Path, rel: &str) -> Option<PathBuf> {
    let path = dir.join(rel);
    if !(path.is_file() && exact_names(dir, rel)) {
        return None;
    }
    if inside(dir, &path) {
        return Some(path);
    }
    warn_linked_outside(dir, rel);
    None
}

/// What a file reached through a symbolic link out of its theme is, after its path in
/// messages (theme-format.md §3, audit F08).
const LINKED_OUTSIDE: &str = "is a symbolic link (or below one) to a file outside the theme \
                              directory, which presenter ignores: theme files must stay \
                              inside the theme";

/// The symbolic links already reported as leading out of their theme, by
/// [`warn_linked_outside`] or as `theme-link-outside` findings ([`link_problems`]).
static REPORTED_LINKS: LazyLock<Mutex<BTreeSet<PathBuf>>> = LazyLock::new(Mutex::default);

/// True the first time `link` is passed (in this process): it has not been reported yet.
fn first_report(link: &Path) -> bool {
    // A poisoned lock only costs the deduplication.
    REPORTED_LINKS
        .lock()
        .map_or(true, |mut reported| reported.insert(link.to_path_buf()))
}

/// Logs, once per link and process, the symbolic link that takes the file `dir/rel` out of
/// the theme directory `dir`, which [`disk_file`] therefore refuses: so that a build that
/// fails on the "missing" file says why.
fn warn_linked_outside(dir: &Path, rel: &str) {
    if let Some((link, target)) = outside_link(dir, rel)
        && first_report(&link)
    {
        tracing::warn!("{}", link_message(&link.display().to_string(), &target));
    }
}

/// The symbolic link that takes `dir/rel` out of the directory `dir`: the first of its
/// ancestors below `dir`, or itself, that is a link whose target is outside `dir`, with the
/// canonical target.
fn outside_link(dir: &Path, rel: &str) -> Option<(PathBuf, PathBuf)> {
    let root = std::fs::canonicalize(dir).ok()?;
    let mut path = dir.to_path_buf();
    for segment in rel.split('/') {
        path.push(segment);
        if path.symlink_metadata().is_ok_and(|meta| meta.is_symlink()) {
            let target = std::fs::canonicalize(&path).ok()?;
            if !target.starts_with(&root) {
                return Some((path, target));
            }
        }
    }
    None
}

/// `<link> is a symbolic link to <target>, outside the theme directory: …`, for a log
/// line and a `theme-link-outside` finding.
fn link_message(link: &str, target: &Path) -> String {
    let below = if target.is_dir() {
        " and everything below it"
    } else {
        ""
    };
    format!(
        "{link} is a symbolic link to {}, outside the theme directory: presenter ignores \
         it{below}, so it counts as missing (theme files must stay inside the theme; copy the \
         file into it, or share it through `extends`)",
        target.display()
    )
}

/// True if the existing file `dir/rel` (exact name) is refused only because a symbolic link
/// takes it out of `dir` ([`disk_file`]).
fn linked_outside(dir: &Path, rel: &str) -> bool {
    let path = dir.join(rel);
    path.is_file() && exact_names(dir, rel) && !inside(dir, &path)
}

/// True if the existing `path` is below the directory `dir` when both are canonicalized
/// (symbolic links resolved). False when either cannot be canonicalized.
fn inside(dir: &Path, path: &Path) -> bool {
    match (std::fs::canonicalize(dir), std::fs::canonicalize(path)) {
        (Ok(dir), Ok(path)) => path.starts_with(&dir),
        _ => false,
    }
}

/// True if every segment of the normalized `rel` is the exact name (case included) of an
/// entry of its parent directory below `dir`. A plain `is_file`/`is_dir` would accept
/// `layouts/TITLE.html` for `layouts/title.html` on a case-insensitive file system, which
/// the embedded tree (exact keys) and a case-sensitive file system (Linux CI) reject.
fn exact_names(dir: &Path, rel: &str) -> bool {
    let mut parent = dir.to_path_buf();
    for segment in rel.split('/').filter(|s| !s.is_empty()) {
        let Ok(entries) = std::fs::read_dir(&parent) else {
            return false;
        };
        if !entries
            .filter_map(|entry| entry.ok())
            .any(|entry| entry.file_name() == segment)
        {
            return false;
        }
        parent.push(segment);
    }
    true
}

/// Files below `dir/rel_dir` on disk, relative to `dir`, following symlinks that stay inside
/// `dir` (the files [`disk_file`] accepts). A directory a link takes out of `dir` is not
/// descended into, so a link to `/usr` costs nothing.
fn list_disk(dir: &Path, rel_dir: &str) -> Vec<String> {
    let start = dir.join(rel_dir);
    if !start.is_dir() || !exact_names(dir, rel_dir) {
        return Vec::new();
    }
    let Ok(root) = std::fs::canonicalize(dir) else {
        return Vec::new();
    };
    walkdir::WalkDir::new(&start)
        .follow_links(true)
        .into_iter()
        .filter_entry(|entry| {
            std::fs::canonicalize(entry.path()).is_ok_and(|path| path.starts_with(&root))
        })
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            let rel = entry.path().strip_prefix(dir).ok()?;
            let parts: Option<Vec<&str>> =
                rel.components().map(|c| c.as_os_str().to_str()).collect();
            Some(parts?.join("/"))
        })
        .collect()
}

/// Normalizes a theme-relative path: `/` and `\` separate segments, empty and `.` segments
/// are dropped, `..` removes the previous segment. `None` if the path climbs out of the
/// theme directory; `Some("")` for the theme directory itself.
pub(crate) fn normalize_rel(rel: &str) -> Option<String> {
    let mut segments: Vec<&str> = Vec::new();
    for segment in rel.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            name => segments.push(name),
        }
    }
    Some(segments.join("/"))
}

impl Theme {
    /// The child theme's name (the theme that was requested).
    pub fn name(&self) -> &str {
        self.chain.first().map_or("", |dir| dir.name.as_str())
    }

    /// The theme of the chain called `name`, if any.
    pub fn dir(&self, name: &str) -> Option<&ThemeDir> {
        self.chain.iter().find(|dir| dir.name == name)
    }

    /// `[styles] reveal_theme` of the first theme (child → root) that sets it, else
    /// [`DEFAULT_REVEAL_THEME`].
    pub fn reveal_theme(&self) -> String {
        self.chain
            .iter()
            .find_map(|dir| dir.manifest.styles.reveal_theme.clone())
            .unwrap_or_else(|| DEFAULT_REVEAL_THEME.to_string())
    }

    /// `[styles] highlight_style` of the first theme (child → root) that sets it, else
    /// [`DEFAULT_HIGHLIGHT_STYLE`]. A value other than `monokai`/`zenburn`/`none` is a path
    /// relative to the theme that set it (find it with `chain` and read it with
    /// [`ThemeDir::read`]).
    pub fn highlight_style(&self) -> String {
        self.chain
            .iter()
            .find_map(|dir| dir.manifest.styles.highlight_style.clone())
            .unwrap_or_else(|| DEFAULT_HIGHLIGHT_STYLE.to_string())
    }

    /// The chain's `[reveal]` tables deep-merged root → child (the child wins; nested
    /// tables merge key by key).
    pub fn reveal_defaults(&self) -> Map<String, Value> {
        self.merged(|manifest| &manifest.reveal)
    }

    /// The chain's `[vars]` tables deep-merged root → child (the child wins).
    pub fn vars(&self) -> Map<String, Value> {
        self.merged(|manifest| &manifest.vars)
    }

    fn merged(&self, table: impl Fn(&ThemeManifest) -> &Map<String, Value>) -> Map<String, Value> {
        let mut merged = Map::new();
        for dir in self.chain.iter().rev() {
            deep_merge(&mut merged, table(&dir.manifest));
        }
        merged
    }

    /// The `[styles] entries` of the whole chain, parent-first (root's entries first, in
    /// manifest order), each read from the theme that lists it. An entry must be a relative
    /// path that stays inside its theme's directory (so `serve` never exposes files outside
    /// a theme); such an entry, or a missing file, is an [`Error::Theme`].
    pub fn style_entries(&self) -> Result<Vec<ThemeFile>> {
        self.entries("style", |manifest| &manifest.styles.entries)
    }

    /// The `[scripts] entries` of the whole chain, parent-first, like
    /// [`Theme::style_entries`].
    pub fn script_entries(&self) -> Result<Vec<ThemeFile>> {
        self.entries("script", |manifest| &manifest.scripts.entries)
    }

    fn entries(
        &self,
        kind: &str,
        list: impl Fn(&ThemeManifest) -> &Vec<String>,
    ) -> Result<Vec<ThemeFile>> {
        let mut files = Vec::new();
        for dir in self.chain.iter().rev() {
            for entry in list(&dir.manifest) {
                let found = match entry_path_problem(entry) {
                    Some(problem) => Err(problem),
                    None => dir
                        .read(entry)?
                        .ok_or_else(|| dir.missing_reason(entry, "not found")),
                };
                let bytes = found.map_err(|problem| Error::Theme {
                    name: dir.name.clone(),
                    message: format!(
                        "{kind} entry '{entry}' {problem} ({}; entries are relative to the theme directory)",
                        dir.location()
                    ),
                })?;
                files.push(ThemeFile {
                    theme: dir.name.clone(),
                    rel_path: entry.clone(),
                    bytes,
                });
            }
        }
        Ok(files)
    }

    /// The file `[styles] highlight_style` names (theme-format.md §2), read from the theme
    /// that sets it (the first, child → root), or `None` for a built-in style
    /// ([`BUILTIN_HIGHLIGHT_STYLES`]). Like a style entry, the path must be relative and stay
    /// inside that theme's directory (the `@<theme>/<path>` form allows no `..` escapes,
    /// theme-format.md §4), and the file must exist; otherwise [`Error::Theme`] naming the
    /// theme, the value and the theme's location (audit F10).
    pub fn highlight_style_file(&self) -> Result<Option<ThemeFile>> {
        let Some((dir, style, found)) = self.highlight_style_lookup()? else {
            return Ok(None);
        };
        let bytes = found.map_err(|problem| Error::Theme {
            name: dir.name.clone(),
            message: format!(
                "{} ({}; paths are relative to the theme directory)",
                highlight_style_problem(style, problem),
                dir.location()
            ),
        })?;
        Ok(Some(ThemeFile {
            theme: dir.name.clone(),
            rel_path: style.to_string(),
            bytes,
        }))
    }

    /// A path-valued `highlight_style`: the theme that sets it, the value, and the file's
    /// bytes or why it cannot be used. `None` for a built-in style (or none set).
    fn highlight_style_lookup(&self) -> Result<Option<HighlightLookup<'_>>> {
        let Some((dir, style)) = self.chain.iter().find_map(|dir| {
            let style = dir.manifest.styles.highlight_style.as_deref()?;
            Some((dir, style))
        }) else {
            return Ok(None);
        };
        if BUILTIN_HIGHLIGHT_STYLES.contains(&style) {
            return Ok(None);
        }
        let found = match entry_path_problem(style) {
            Some(problem) => Err(problem),
            None => dir
                .read(style)?
                .ok_or_else(|| dir.missing_reason(style, "is not found")),
        };
        Ok(Some((dir, style, found)))
    }

    /// What a build can fail on in this theme, found without rendering (audit F10), as
    /// error diagnostics: `[styles]` and `[scripts]` entries and a `highlight_style` file
    /// that are missing or outside their theme's directory (`theme-style-entry`,
    /// `theme-script-entry`, `theme-highlight-style`), templates of the chain's disk themes
    /// that do not compile (`theme-template`, path and line of the template; a build fails
    /// on one when it loads it) and an invalid merged `[reveal] plugins` list
    /// (`theme-plugins`). Also, as warnings, every symbolic link in a disk theme whose
    /// target is outside the theme (`theme-link-outside`): lookups ignore it, so the file
    /// counts as missing. Manifest problems and missing required templates already fail
    /// [`resolve()`]. A finding about a manifest has the path of the disk theme's
    /// `theme.toml` (the line of the value, found with the rules of
    /// `presenter theme check`) and names the theme. Stylesheets are not compiled here
    /// (`presenter check` compiles them itself). [`Theme::deck_problems`] is the variant for
    /// one deck (`presenter check`).
    pub fn problems(&self) -> Vec<Diagnostic> {
        self.problems_with(None)
    }

    /// [`Theme::problems`] for a deck whose slides use the layouts `layouts`: a template
    /// that does not compile is an error only when a build of that deck loads it (the
    /// required templates, `deck.html` first, the deck's layouts and the templates these
    /// name, `templates::reached_templates`). Any other (a layout no slide uses, a partial
    /// nothing includes, a parent's template the child overrides) is a warning: the build of
    /// this deck does not fail on it, a deck that uses it does.
    pub fn deck_problems(&self, layouts: &BTreeSet<String>) -> Vec<Diagnostic> {
        self.problems_with(Some(layouts))
    }

    /// The templates a build of a deck whose slides use `layouts` loads
    /// ([`Theme::deck_problems`]).
    fn deck_templates(&self, layouts: &BTreeSet<String>) -> BTreeSet<TemplateFile> {
        let mut roots: Vec<String> = REQUIRED_TEMPLATES.iter().map(|t| t.to_string()).collect();
        roots.extend(
            layouts
                .iter()
                .filter(|layout| self.has_layout(layout))
                .map(|layout| format!("layouts/{layout}.html")),
        );
        templates::reached_templates(self, roots)
    }

    /// [`Theme::problems`], or [`Theme::deck_problems`] when `layouts` is given.
    fn problems_with(&self, layouts: Option<&BTreeSet<String>>) -> Vec<Diagnostic> {
        // First, so that the lookups below do not log the links they report.
        let links: Vec<Diagnostic> = self.chain.iter().flat_map(link_problems).collect();
        let reached = layouts.map(|layouts| self.deck_templates(layouts));
        let mut found = Vec::new();
        for dir in self.chain.iter().rev() {
            found.extend(entry_problems(dir));
        }
        match self.highlight_style_lookup() {
            Ok(Some((dir, style, Err(problem)))) => {
                let message = format!(
                    "theme '{}': {} (paths are relative to the theme directory)",
                    dir.name,
                    highlight_style_problem(style, problem)
                );
                found.push(manifest_problem(
                    dir,
                    codes::THEME_HIGHLIGHT_STYLE,
                    message,
                    |text| manifest::line_of_value(text, "styles", "highlight_style", style),
                ));
            }
            Ok(_) => {}
            Err(err) => found.push(Diagnostic::error(
                codes::THEME_HIGHLIGHT_STYLE,
                err.to_string(),
            )),
        }
        found.extend(self.template_problems(reached.as_ref()));
        found.extend(self.plugin_problem());
        found.extend(links);
        found
    }

    /// `theme-template`: every template of the chain's disk themes that does not compile,
    /// an error when `reached` is `None` or has it, else a warning ([`Theme::deck_problems`]).
    /// Hidden files (`.gitkeep`) are skipped; built-in themes always compile.
    fn template_problems(&self, reached: Option<&BTreeSet<TemplateFile>>) -> Vec<Diagnostic> {
        let env = match self.environment(false) {
            Ok(env) => env,
            Err(err) => return vec![Diagnostic::error(codes::THEME_TEMPLATE, err.to_string())],
        };
        let mut found = Vec::new();
        for dir in &self.chain {
            let ThemeSource::Disk { dir: root } = &dir.source else {
                continue;
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
                let detail = match err.detail() {
                    Some(detail) => format!("{}: {detail}", err.kind()),
                    None => err.kind().to_string(),
                };
                let message = format!("theme '{}': template {rel}: {detail}", dir.name);
                let used = reached
                    .is_none_or(|reached| reached.contains(&(dir.name.clone(), rel.to_string())));
                let diagnostic = if used {
                    Diagnostic::error(codes::THEME_TEMPLATE, message)
                } else {
                    let message = format!(
                        "{message} (this deck does not use the template, so its build does not \
                         fail on it; a deck that uses it does)"
                    );
                    Diagnostic::warning(codes::THEME_TEMPLATE, message)
                };
                let mut diagnostic = diagnostic.with_path(root.join(&path));
                if let Some(line) = err.line() {
                    diagnostic = diagnostic.with_span(line_span(line));
                }
                found.push(diagnostic);
            }
        }
        found
    }

    /// `theme-plugins`: the chain's merged `[reveal] plugins` list is invalid (the check the
    /// pipeline makes before rendering), reported on the theme that sets it.
    fn plugin_problem(&self) -> Option<Diagnostic> {
        let probe = Config {
            reveal: self.reveal_defaults(),
            ..Config::default()
        };
        let err = probe.validate_plugins().err()?;
        let message = match err {
            Error::Config(message) => message,
            other => other.to_string(),
        };
        let dir = self
            .chain
            .iter()
            .find(|dir| dir.manifest.reveal.contains_key("plugins"))?;
        let message = format!("theme '{}': theme.toml [reveal]: {message}", dir.name);
        Some(manifest_problem(
            dir,
            codes::THEME_PLUGINS,
            message,
            |text| manifest::line_of_key(text, "reveal", "plugins"),
        ))
    }

    /// The `unknown-layout` warning text for a slide labelled `label` (`slide 3`) whose
    /// layout this theme does not provide: the layouts it has and, for a near miss, a
    /// suggestion (`… (theme 'talk' has: default, title; did you mean 'title'?); using the
    /// default layout`). The renderer and `presenter check` both use it (audit F40).
    pub fn unknown_layout_message(&self, label: &str, layout: &str) -> String {
        let layouts = self.layouts();
        let hint = closest_name(layout, layouts.iter().map(String::as_str))
            .map(|name| format!("; did you mean '{name}'?"))
            .unwrap_or_default();
        format!(
            "{label}: unknown layout '{layout}' (theme '{}' has: {}{hint}); using the default \
             layout",
            self.name(),
            layouts.join(", ")
        )
    }

    /// The template `name` as `(theme name, source)`: `templates/<name>` of the first
    /// theme (child → root) that has it. `@<theme>/<path>` reads `templates/<path>` of that
    /// theme only.
    ///
    /// The path is taken as written, like minijinja's own `path_loader`: `/`-separated
    /// segments, none of them empty or starting with `.`, no `\`. Any other name
    /// (`../theme.toml`, `layouts/../slide.html`, `./deck.html`) finds nothing, so a
    /// template can never be reached under a second name. File names match exactly,
    /// including case. `Ok(None)` if no theme has it; [`Error::Theme`] for an `@` name that
    /// does not name a theme of the chain, [`Error::Template`] for a file that is not UTF-8.
    pub fn read_template(&self, name: &str) -> Result<Option<(String, String)>> {
        let (dirs, path) = self.template_dirs(name)?;
        let Some(file) = template_file(path) else {
            return Ok(None);
        };
        for dir in dirs {
            if let Some(bytes) = dir.read(&file)? {
                let source = String::from_utf8(bytes).map_err(|e| Error::Template {
                    template: name.to_string(),
                    line: None,
                    message: format!(
                        "template '{name}' is not valid UTF-8 (theme '{}'): {e}",
                        dir.name
                    ),
                })?;
                return Ok(Some((dir.name.clone(), source)));
            }
        }
        Ok(None)
    }

    /// True if [`Theme::read_template`] would find `name` (without reading it).
    pub fn has_template(&self, name: &str) -> bool {
        self.template_origin(name).is_some()
    }

    /// Where [`Theme::read_template`] finds `name` (without reading it): the theme that has
    /// it and its path below `templates/`.
    pub(crate) fn template_origin(&self, name: &str) -> Option<(&ThemeDir, String)> {
        let (dirs, path) = self.template_dirs(name).ok()?;
        let file = template_file(path)?;
        let dir = dirs.into_iter().find(|dir| dir.exists(&file))?;
        Some((dir, path.to_string()))
    }

    /// True if `layout` is one of [`Theme::layouts`], i.e. a single path segment (see
    /// [`is_layout_name`]) for which `templates/layouts/<layout>.html` exists in the chain.
    ///
    /// The renderer (choosing `layout_template`) and `check` (`unknown-layout`) should both
    /// use this, so they agree on every name: `../slide`, `sub/x` or `TITLE` (for a file
    /// `title.html`) are never layouts.
    pub fn has_layout(&self, layout: &str) -> bool {
        is_layout_name(layout) && self.has_template(&format!("layouts/{layout}.html"))
    }

    /// The themes to search for template `name` and the path below `templates/`.
    fn template_dirs<'a, 'n>(&'a self, name: &'n str) -> Result<(Vec<&'a ThemeDir>, &'n str)> {
        let Some(qualified) = name.strip_prefix('@') else {
            return Ok((self.chain.iter().collect(), name));
        };
        let chain_names = || {
            let names: Vec<&str> = self.chain.iter().map(|d| d.name.as_str()).collect();
            names.join(" → ")
        };
        let Some((theme, path)) = qualified.split_once('/') else {
            return Err(Error::Theme {
                name: self.name().to_string(),
                message: format!("template name '{name}' must have the form '@<theme>/<path>'"),
            });
        };
        let dir = self.dir(theme).ok_or_else(|| Error::Theme {
            name: self.name().to_string(),
            message: format!(
                "template '{name}': no theme named '{theme}' in the chain ({})",
                chain_names()
            ),
        })?;
        Ok((vec![dir], path))
    }

    /// Layout names: the union of `templates/layouts/*.html` file stems across the chain
    /// that are valid layout names ([`is_layout_name`]; files in subdirectories are not
    /// layouts), sorted. `layouts().contains(x)` equals [`Theme::has_layout`]`(x)`.
    pub fn layouts(&self) -> Vec<String> {
        let mut names = BTreeSet::new();
        for dir in &self.chain {
            for path in dir.list("templates/layouts") {
                let file = &path["templates/layouts/".len()..];
                if let Some(stem) = file.strip_suffix(".html")
                    && is_layout_name(stem)
                {
                    names.insert(stem.to_string());
                }
            }
        }
        names.into_iter().collect()
    }

    /// `assets/<rel>` of the first theme (child → root) that has it. `rel` is normalized
    /// like in [`ThemeDir::read`]; a path that climbs out of `assets/` yields `Ok(None)`.
    pub fn read_asset(&self, rel: &str) -> Result<Option<Vec<u8>>> {
        let Some(rel) = normalize_rel(rel).filter(|r| !r.is_empty()) else {
            return Ok(None);
        };
        for dir in &self.chain {
            if let Some(bytes) = dir.read(&format!("assets/{rel}"))? {
                return Ok(Some(bytes));
            }
        }
        Ok(None)
    }

    /// The directories of the chain's disk themes, child first, to watch recursively.
    /// Built-in themes contribute nothing.
    pub fn watch_dirs(&self) -> Vec<PathBuf> {
        self.chain
            .iter()
            .filter_map(|theme_dir| match &theme_dir.source {
                ThemeSource::Disk { dir } => Some(dir.clone()),
                ThemeSource::Embedded { .. } => None,
            })
            .collect()
    }

    /// A minijinja environment whose loader searches this chain; see
    /// [`templates::environment`].
    pub fn environment(&self, debug: bool) -> Result<minijinja::Environment<'static>> {
        templates::environment(self, debug)
    }
}

/// True if `segment` may appear in a template name: non-empty, not starting with `.` (which
/// also excludes `.` and `..`) and without `\`; the rule of minijinja's `path_loader`.
fn is_name_segment(segment: &str) -> bool {
    !segment.is_empty() && !segment.starts_with('.') && !segment.contains('\\')
}

/// True if `name` can be a layout name: one template-name segment ([`Theme::read_template`]),
/// so no `/`, `\`, leading `.` or empty name.
pub fn is_layout_name(name: &str) -> bool {
    !name.contains('/') && is_name_segment(name)
}

/// `templates/<path>` for a template path made of valid segments ([`is_name_segment`]),
/// else `None` (so `{% include "../theme.toml" %}` and `layouts/../slide.html` find
/// nothing).
fn template_file(path: &str) -> Option<String> {
    path.split('/')
        .all(is_name_segment)
        .then(|| format!("templates/{path}"))
}

/// Why a `[styles]`/`[scripts]` entry is not a usable path inside the theme directory, if
/// it is not: absolute, climbing out of the directory, or naming the directory itself.
fn entry_path_problem(entry: &str) -> Option<&'static str> {
    if entry.starts_with(['/', '\\']) || Path::new(entry).has_root() {
        return Some("must be a relative path");
    }
    match normalize_rel(entry) {
        None => Some("must stay inside the theme directory"),
        Some(rel) if rel.is_empty() => Some("does not name a file"),
        Some(_) => None,
    }
}

/// What [`Theme::highlight_style_lookup`] finds: the theme that sets a path-valued
/// `highlight_style`, the value, and the file's bytes or why it cannot be used.
type HighlightLookup<'a> = (
    &'a ThemeDir,
    &'a str,
    std::result::Result<Vec<u8>, &'static str>,
);

/// `[styles] highlight_style '<style>' (…) <problem>`, the text of a bad `highlight_style`.
fn highlight_style_problem(style: &str, problem: &str) -> String {
    format!(
        "[styles] highlight_style '{style}' (not monokai, zenburn or none, so a CSS file) \
         {problem}"
    )
}

/// `theme-style-entry` / `theme-script-entry` for every `[styles]`/`[scripts]` entry of
/// `dir` that is not a file inside its directory.
fn entry_problems(dir: &ThemeDir) -> Vec<Diagnostic> {
    let lists = [
        (
            "styles",
            codes::THEME_STYLE_ENTRY,
            &dir.manifest.styles.entries,
        ),
        (
            "scripts",
            codes::THEME_SCRIPT_ENTRY,
            &dir.manifest.scripts.entries,
        ),
    ];
    let mut found = Vec::new();
    for (table, code, entries) in lists {
        for entry in entries {
            let problem = match entry_path_problem(entry) {
                Some(problem) => problem,
                None if dir.exists(entry) => continue,
                None => dir.missing_reason(entry, "is not found"),
            };
            let message = format!(
                "theme '{}': [{table}] entry '{entry}' {problem} (paths are relative to the \
                 theme directory)",
                dir.name
            );
            found.push(manifest_problem(dir, code, message, |text| {
                manifest::line_of_value(text, table, "entries", entry)
            }));
        }
    }
    found
}

/// `theme-link-outside` (warning) for every symbolic link in the disk theme `dir` whose
/// target is outside the theme directory: lookups ignore it, and every file below it
/// ([`disk_file`]), so it counts as missing (audit F08). Each is marked as reported, so
/// lookups do not log it again ([`warn_linked_outside`]). Links are not followed, so the
/// walk stays inside the theme; dangling links are not reported.
fn link_problems(dir: &ThemeDir) -> Vec<Diagnostic> {
    let ThemeSource::Disk { dir: root } = &dir.source else {
        return Vec::new();
    };
    let Ok(canonical) = std::fs::canonicalize(root) else {
        return Vec::new();
    };
    walkdir::WalkDir::new(root)
        .sort_by_file_name()
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path_is_symlink())
        .filter_map(|entry| {
            let target = std::fs::canonicalize(entry.path()).ok()?;
            if target.starts_with(&canonical) {
                return None;
            }
            let rel = entry.path().strip_prefix(root).ok()?;
            first_report(entry.path());
            let message = format!(
                "theme '{}': {}",
                dir.name,
                link_message(&rel.display().to_string(), &target)
            );
            Some(Diagnostic::warning(codes::THEME_LINK_OUTSIDE, message).with_path(entry.path()))
        })
        .collect()
}

/// An error about `dir`'s manifest: with the path of a disk theme's `theme.toml` and the
/// line `line_of` finds in its text, when it finds one.
fn manifest_problem(
    dir: &ThemeDir,
    code: &'static str,
    message: String,
    line_of: impl FnOnce(&str) -> Option<usize>,
) -> Diagnostic {
    let diagnostic = Diagnostic::error(code, message);
    let ThemeSource::Disk { dir: root } = &dir.source else {
        return diagnostic;
    };
    let path = root.join(resolve::MANIFEST_FILE);
    let line = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| line_of(&text));
    let diagnostic = diagnostic.with_path(path);
    match line {
        Some(line) => diagnostic.with_span(line_span(line)),
        None => diagnostic,
    }
}

/// A one-line span (of a `theme.toml` or a template).
fn line_span(line: usize) -> SourceSpan {
    SourceSpan {
        start_line: line,
        end_line: line,
        cell: None,
    }
}

/// The name among `candidates` closest to `word` (edit distance 1 or 2), for a "did you
/// mean" hint; the same rule as the config's unknown-key hints
/// ([`crate::config::unknown_key_hint`], which this asks). Candidates with a backtick or a
/// line break are skipped.
pub(crate) fn closest_name<'c>(
    word: &str,
    candidates: impl IntoIterator<Item = &'c str>,
) -> Option<String> {
    if word.contains('`') {
        return None;
    }
    let expected: Vec<String> = candidates
        .into_iter()
        .filter(|candidate| !candidate.contains(['`', '\n']) && *candidate != word)
        .map(|candidate| format!("`{candidate}`"))
        .collect();
    if expected.is_empty() {
        return None;
    }
    let message = format!("unknown variant `{word}`, expected {}", expected.join(", "));
    crate::config::unknown_key_hint(&message)
}

/// Merges `over` into `base`: objects present on both sides merge recursively, anything
/// else in `over` replaces the value in `base`.
fn deep_merge(base: &mut Map<String, Value>, over: &Map<String, Value>) {
    for (key, value) in over {
        match (base.get_mut(key), value) {
            (Some(Value::Object(base_map)), Value::Object(over_map)) => {
                deep_merge(base_map, over_map)
            }
            _ => {
                base.insert(key.clone(), value.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("not an object: {other}"),
        }
    }

    #[test]
    fn normalize_rel_stays_inside_the_theme() {
        assert_eq!(
            normalize_rel("styles/theme.css").as_deref(),
            Some("styles/theme.css")
        );
        assert_eq!(
            normalize_rel("./styles//./a\\b.css").as_deref(),
            Some("styles/a/b.css")
        );
        assert_eq!(
            normalize_rel("styles/../assets/x.png").as_deref(),
            Some("assets/x.png")
        );
        assert_eq!(
            normalize_rel("/templates/deck.html").as_deref(),
            Some("templates/deck.html")
        );
        assert_eq!(normalize_rel("").as_deref(), Some(""));
        assert_eq!(normalize_rel("a/..").as_deref(), Some(""));
        assert_eq!(normalize_rel(".."), None);
        assert_eq!(normalize_rel("assets/../../theme.toml"), None);
    }

    #[test]
    fn deep_merge_lets_the_child_win_key_by_key() {
        let mut base = object(json!({"a": 1, "nested": {"x": 1, "y": 2}, "list": [1, 2]}));
        deep_merge(
            &mut base,
            &object(json!({"b": 2, "nested": {"y": 3, "z": 4}, "list": [3]})),
        );
        assert_eq!(
            Value::Object(base),
            json!({"a": 1, "b": 2, "nested": {"x": 1, "y": 3, "z": 4}, "list": [3]})
        );
        let mut base = object(json!({"a": {"x": 1}}));
        deep_merge(&mut base, &object(json!({"a": "flat"})));
        assert_eq!(Value::Object(base), json!({"a": "flat"}));
    }

    #[test]
    fn disk_theme_dir_reads_lists_and_refuses_to_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("t");
        std::fs::create_dir_all(root.join("templates/layouts")).unwrap();
        std::fs::write(root.join("theme.toml"), "name = \"t\"").unwrap();
        std::fs::write(root.join("templates/layouts/a.html"), "A").unwrap();
        std::fs::write(tmp.path().join("secret.txt"), "s").unwrap();
        let dir = ThemeDir {
            name: "t".into(),
            source: ThemeSource::Disk { dir: root.clone() },
            manifest: ThemeManifest::default(),
        };
        assert_eq!(
            dir.read("templates/layouts/a.html").unwrap(),
            Some(b"A".to_vec())
        );
        assert_eq!(dir.read("templates/missing.html").unwrap(), None);
        assert_eq!(
            dir.read("templates").unwrap(),
            None,
            "directories are not files"
        );
        assert_eq!(dir.read("../secret.txt").unwrap(), None);
        assert!(!dir.exists("../secret.txt"));
        assert!(dir.exists("./theme.toml"));
        assert_eq!(dir.list("templates"), ["templates/layouts/a.html"]);
        assert_eq!(dir.list(""), ["templates/layouts/a.html", "theme.toml"]);
        assert!(dir.list("nope").is_empty());
        assert!(dir.list("..").is_empty());
    }
}
