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

pub mod manifest;
pub mod resolve;
pub mod styles;
pub mod templates;

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::error::{Error, Result};

pub use manifest::{ScriptsSection, StylesSection, ThemeManifest};
pub use resolve::{builtin_names, resolve};

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

/// Where a theme's files live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeSource {
    /// A built-in theme in the embedded asset tree; `prefix` is its directory relative to
    /// `assets/`, with a trailing slash (`"themes/default/"`).
    Embedded {
        /// Path prefix of the theme's files in [`crate::vendor`].
        prefix: String,
    },
    /// A theme directory on disk (canonicalized by [`resolve`]).
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
    /// The chain, ordered child → … → root. [`resolve`] never returns an empty chain.
    pub chain: Vec<ThemeDir>,
}

impl ThemeDir {
    /// Reads the file at `rel` (relative to the theme directory, `/`-separated).
    ///
    /// `rel` is normalized lexically (`.` and empty segments dropped, `..` removes the
    /// previous segment); a path that is empty or climbs out of the theme directory yields
    /// `Ok(None)`, as does a missing file or a directory. Disk themes are read from disk on
    /// every call. Fails with [`Error::Theme`] only when an existing file cannot be read.
    pub fn read(&self, rel: &str) -> Result<Option<Vec<u8>>> {
        let Some(rel) = normalize_rel(rel).filter(|r| !r.is_empty()) else {
            return Ok(None);
        };
        match &self.source {
            ThemeSource::Embedded { prefix } => {
                Ok(crate::vendor::get(&format!("{prefix}{rel}")).map(|data| data.into_owned()))
            }
            ThemeSource::Disk { dir } => {
                let path = dir.join(&rel);
                if !path.is_file() {
                    return Ok(None);
                }
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
            ThemeSource::Disk { dir } => dir.join(rel).is_file(),
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

    /// Where the theme lives, for messages: the directory, or `built-in (themes/<name>/)`.
    pub fn location(&self) -> String {
        match &self.source {
            ThemeSource::Embedded { prefix } => format!("built-in ({prefix})"),
            ThemeSource::Disk { dir } => dir.display().to_string(),
        }
    }
}

/// Files below `dir/rel_dir` on disk, relative to `dir`, following symlinks.
fn list_disk(dir: &std::path::Path, rel_dir: &str) -> Vec<String> {
    let start = dir.join(rel_dir);
    if !start.is_dir() {
        return Vec::new();
    }
    walkdir::WalkDir::new(&start)
        .follow_links(true)
        .into_iter()
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
    /// manifest order), each read from the theme that lists it. A missing entry is an
    /// [`Error::Theme`].
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
                let bytes = dir.read(entry)?.ok_or_else(|| Error::Theme {
                    name: dir.name.clone(),
                    message: format!(
                        "{kind} entry '{entry}' not found in {} (entries are relative to the theme directory)",
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

    /// The template `name` as `(theme name, source)`: `templates/<name>` of the first
    /// theme (child → root) that has it. `@<theme>/<path>` reads `templates/<path>` of that
    /// theme only. The path is normalized like in [`ThemeDir::read`] and must stay inside
    /// `templates/`. `Ok(None)` if no theme has it; [`Error::Theme`] for an `@` name that does
    /// not name a theme of the chain, [`Error::Template`] for a file that is not UTF-8.
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
                    message: format!("not valid UTF-8 (theme '{}'): {e}", dir.name),
                })?;
                return Ok(Some((dir.name.clone(), source)));
            }
        }
        Ok(None)
    }

    /// True if [`Theme::read_template`] would find `name` (without reading it).
    pub fn has_template(&self, name: &str) -> bool {
        let Ok((dirs, path)) = self.template_dirs(name) else {
            return false;
        };
        template_file(path).is_some_and(|file| dirs.iter().any(|dir| dir.exists(&file)))
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

    /// Layout names: the union of `templates/layouts/*.html` file stems across the chain,
    /// sorted.
    pub fn layouts(&self) -> Vec<String> {
        let mut names = BTreeSet::new();
        for dir in &self.chain {
            for path in dir.list("templates/layouts") {
                let file = &path["templates/layouts/".len()..];
                if let Some(stem) = file.strip_suffix(".html")
                    && !stem.is_empty()
                    && !stem.contains('/')
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

/// `templates/<path>` for a template path, or `None` if `path` is empty or climbs out of
/// `templates/` (so `{% include "../theme.toml" %}` finds nothing).
fn template_file(path: &str) -> Option<String> {
    normalize_rel(path)
        .filter(|p| !p.is_empty())
        .map(|p| format!("templates/{p}"))
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
