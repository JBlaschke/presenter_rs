//! Theme lookup and inheritance-chain resolution (`docs/spec/theme-format.md` §3).
//!
//! A theme spec (from `--theme`, front matter `theme:` or `deck.theme`) is first tried as a
//! directory containing `theme.toml`, relative to each search root in order (the pipeline
//! passes config file dir, source dir, CWD) or as an absolute path; otherwise it must name
//! a built-in theme. `extends` is resolved the same way with the extending theme's own
//! directory as the only search root (built-in themes can only extend built-in themes by
//! name). The chain must not repeat a theme (a cycle), must not contain two themes with
//! the same name (`@<name>/` template names would be ambiguous) and has at most
//! [`MAX_CHAIN_LEN`] themes. Its root must provide every template in
//! [`REQUIRED_TEMPLATES`].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

use super::{REQUIRED_TEMPLATES, Theme, ThemeDir, ThemeManifest, ThemeSource, disk_file};

/// The most themes one chain may contain (the requested theme plus seven ancestors).
pub const MAX_CHAIN_LEN: usize = 8;

/// Name of the manifest file every theme directory contains.
pub const MANIFEST_FILE: &str = "theme.toml";

/// Resolves `spec` (a theme name or directory path) and its `extends` chain.
///
/// Fails with [`Error::Theme`] (`name` = `spec`, or the `extends` value for a parent that
/// cannot be found or parsed) when the theme is not found (the message lists every
/// directory that was tried and the built-in theme names), a manifest is invalid, the
/// chain has a cycle or more than [`MAX_CHAIN_LEN`] themes, two themes share a name, or the
/// root lacks required templates (the message lists the missing files).
pub fn resolve(spec: &str, search_roots: &[&Path]) -> Result<Theme> {
    let source = locate(spec, search_roots).map_err(|message| theme_error(spec, message))?;
    let mut chain: Vec<ThemeDir> = Vec::new();
    let mut next = Some((spec.to_string(), source));
    while let Some((requested, source)) = next.take() {
        check_not_in_chain(spec, &chain, &source)?;
        if chain.len() == MAX_CHAIN_LEN {
            return Err(theme_error(
                spec,
                format!(
                    "inheritance chain is longer than {MAX_CHAIN_LEN} themes ({} → …)",
                    chain_text(&chain)
                ),
            ));
        }
        let dir = load(&requested, source)?;
        check_unique_name(spec, &chain, &dir)?;
        next = match &dir.manifest.extends {
            Some(parent) => Some((parent.clone(), locate_parent(parent, &dir)?)),
            None => None,
        };
        chain.push(dir);
    }
    check_root_templates(spec, &chain)?;
    tracing::debug!(theme = spec, chain = %chain_text(&chain), "resolved theme");
    Ok(Theme { chain })
}

/// Names of the built-in themes (directories `themes/<name>/` of the embedded asset tree
/// that contain a `theme.toml`), sorted.
pub fn builtin_names() -> Vec<String> {
    let names: BTreeSet<String> = crate::vendor::list("themes/")
        .iter()
        .filter_map(|path| {
            let rest = path.strip_prefix("themes/")?;
            let (name, file) = rest.split_once('/')?;
            (file == MANIFEST_FILE).then(|| name.to_string())
        })
        .collect();
    names.into_iter().collect()
}

/// Finds the theme `spec`: a directory with a manifest below one of `roots` (or at an
/// absolute path), else a built-in theme. The error message says where it looked.
///
/// The manifest is detected with the exact-name rule [`ThemeDir::read`] uses, so a
/// `Theme.toml` counts as no manifest on every platform, as on a case-sensitive file
/// system (an `is_file` check would accept it on macOS and the read would then fail). The
/// directory `spec` itself is a user path and follows the file system's own rules.
fn locate(spec: &str, roots: &[&Path]) -> std::result::Result<ThemeSource, String> {
    if spec.trim().is_empty() {
        return Err("empty theme name".to_string());
    }
    let candidates = candidate_dirs(spec, roots);
    for candidate in &candidates {
        if disk_file(candidate, MANIFEST_FILE).is_some() {
            let dir = std::fs::canonicalize(candidate)
                .map_err(|e| format!("cannot open {}: {e}", candidate.display()))?;
            return Ok(ThemeSource::Disk { dir });
        }
    }
    let builtins = builtin_names();
    if builtins.iter().any(|name| name == spec) {
        return Ok(ThemeSource::Embedded {
            prefix: format!("themes/{spec}/"),
        });
    }
    Err(not_found_message(&candidates, &builtins))
}

/// The directories `spec` may be: `spec` itself if absolute, else `spec` below each root,
/// made absolute (against the CWD) and without duplicates, in order. The pipeline's roots
/// (config dir, source dir, CWD) often coincide; each directory is tried and reported once.
fn candidate_dirs(spec: &str, roots: &[&Path]) -> Vec<PathBuf> {
    let joined: Vec<PathBuf> = if Path::new(spec).is_absolute() {
        vec![PathBuf::from(spec)]
    } else {
        roots.iter().map(|root| root.join(spec)).collect()
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    for path in joined {
        // `absolute` only fails for an empty path or an unavailable CWD; keep it as is then.
        let path = std::path::absolute(&path).unwrap_or(path);
        if !candidates.contains(&path) {
            candidates.push(path);
        }
    }
    candidates
}

/// "not found" with every directory tried and the built-in names.
fn not_found_message(candidates: &[PathBuf], builtins: &[String]) -> String {
    let tried = if candidates.is_empty() {
        "no search directories".to_string()
    } else {
        candidates
            .iter()
            .map(|c| c.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut message = format!(
        "not found; looked for {MANIFEST_FILE} in: {tried}; built-in themes: {}",
        builtins.join(", ")
    );
    for candidate in candidates.iter().filter(|c| c.is_dir()) {
        message.push_str(&format!(
            " ({} exists but has no {MANIFEST_FILE}",
            candidate.display()
        ));
        if let Some(found) = case_variant(candidate, MANIFEST_FILE) {
            message.push_str(&format!(
                "; it has {found}, but the name must be {MANIFEST_FILE} exactly"
            ));
        }
        message.push(')');
    }
    message
}

/// The name of an entry of `dir` that equals `file` up to ASCII case but not exactly, if
/// any (the smallest such name, for a deterministic message).
fn case_variant(dir: &Path, file: &str) -> Option<String> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name != file && name.eq_ignore_ascii_case(file))
        .min()
}

/// Locates the parent `parent` named by `child`'s `extends`.
fn locate_parent(parent: &str, child: &ThemeDir) -> Result<ThemeSource> {
    let roots: Vec<&Path> = match &child.source {
        ThemeSource::Disk { dir } => vec![dir.as_path()],
        ThemeSource::Embedded { .. } => Vec::new(),
    };
    locate(parent, &roots).map_err(|message| {
        theme_error(
            parent,
            format!(
                "{message} (extended by theme '{}' at {})",
                child.name,
                child.location()
            ),
        )
    })
}

/// Reads and parses the manifest of the theme at `source`.
fn load(requested: &str, source: ThemeSource) -> Result<ThemeDir> {
    let mut dir = ThemeDir {
        name: requested.to_string(),
        source,
        manifest: ThemeManifest::default(),
    };
    let origin = match &dir.source {
        ThemeSource::Disk { dir } => dir.join(MANIFEST_FILE).display().to_string(),
        ThemeSource::Embedded { prefix } => format!("built-in {prefix}{MANIFEST_FILE}"),
    };
    let bytes = dir
        .read(MANIFEST_FILE)?
        .ok_or_else(|| theme_error(requested, format!("{origin} disappeared")))?;
    let text = String::from_utf8(bytes)
        .map_err(|e| theme_error(requested, format!("{origin}: not valid UTF-8: {e}")))?;
    dir.manifest = ThemeManifest::from_toml_str(&text, requested, &origin)?;
    dir.name = dir.manifest.name.clone();
    Ok(dir)
}

/// A theme whose source is already in the chain closes a cycle.
fn check_not_in_chain(spec: &str, chain: &[ThemeDir], source: &ThemeSource) -> Result<()> {
    let Some(start) = chain.iter().position(|dir| &dir.source == source) else {
        return Ok(());
    };
    let cycle: Vec<String> = chain[start..]
        .iter()
        .chain(std::iter::once(&chain[start]))
        .map(|dir| format!("'{}' ({})", dir.name, dir.location()))
        .collect();
    Err(theme_error(
        spec,
        format!("inheritance cycle: {}", cycle.join(" extends ")),
    ))
}

/// Two different themes with the same name would make `@<name>/` lookups ambiguous.
fn check_unique_name(spec: &str, chain: &[ThemeDir], dir: &ThemeDir) -> Result<()> {
    match chain.iter().find(|other| other.name == dir.name) {
        None => Ok(()),
        Some(other) => Err(theme_error(
            spec,
            format!(
                "two themes in the chain are named '{}' ({} and {}); rename one",
                dir.name,
                other.location(),
                dir.location()
            ),
        )),
    }
}

/// The root theme must provide every template in [`REQUIRED_TEMPLATES`].
fn check_root_templates(spec: &str, chain: &[ThemeDir]) -> Result<()> {
    let Some(root) = chain.last() else {
        return Err(theme_error(spec, "empty theme chain".to_string()));
    };
    let missing: Vec<String> = REQUIRED_TEMPLATES
        .iter()
        .map(|name| format!("templates/{name}"))
        .filter(|path| !root.exists(path))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(theme_error(
        spec,
        format!(
            "root theme '{}' ({}) is missing required files: {}",
            root.name,
            root.location(),
            missing.join(", ")
        ),
    ))
}

/// `child → parent → root` names.
fn chain_text(chain: &[ThemeDir]) -> String {
    chain
        .iter()
        .map(|dir| dir.name.as_str())
        .collect::<Vec<_>>()
        .join(" → ")
}

fn theme_error(name: &str, message: String) -> Error {
    Error::Theme {
        name: name.to_string(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_reports_where_it_looked() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("bare")).unwrap();
        let message = locate("bare", &[tmp.path()]).unwrap_err();
        assert!(
            message.contains(&tmp.path().join("bare").display().to_string()),
            "{message}"
        );
        assert!(
            message.contains("exists but has no theme.toml"),
            "{message}"
        );
        assert!(
            !message.contains("but the name must be"),
            "no case hint without a near match: {message}"
        );
        assert!(locate("", &[tmp.path()]).unwrap_err().contains("empty"));
        assert!(
            locate("x", &[])
                .unwrap_err()
                .contains("no search directories")
        );
    }

    #[test]
    fn locate_requires_the_exact_manifest_name() {
        // `is_file` accepts `theme.toml` for `Theme.toml` on macOS; the exact-name rule
        // makes this "no manifest" everywhere, with a hint naming the near miss.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("t")).unwrap();
        std::fs::write(tmp.path().join("t").join("Theme.toml"), "name = \"t\"").unwrap();
        let message = locate("t", &[tmp.path()]).unwrap_err();
        assert!(
            message.contains("exists but has no theme.toml; it has Theme.toml"),
            "{message}"
        );
        assert_eq!(
            case_variant(&tmp.path().join("t"), MANIFEST_FILE).as_deref(),
            Some("Theme.toml")
        );
        assert_eq!(case_variant(&tmp.path().join("t"), "Theme.toml"), None);
        assert_eq!(
            case_variant(&tmp.path().join("missing"), MANIFEST_FILE),
            None
        );
    }

    #[test]
    fn locate_tries_roots_in_order_and_absolute_paths() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for root in [first.path(), second.path()] {
            std::fs::create_dir(root.join("t")).unwrap();
            std::fs::write(root.join("t").join(MANIFEST_FILE), "name = \"t\"").unwrap();
        }
        let canonical = |p: &Path| std::fs::canonicalize(p).unwrap();
        assert_eq!(
            locate("t", &[first.path(), second.path()]).unwrap(),
            ThemeSource::Disk {
                dir: canonical(&first.path().join("t"))
            }
        );
        assert_eq!(
            locate("t", &[Path::new("/nonexistent-root"), second.path()]).unwrap(),
            ThemeSource::Disk {
                dir: canonical(&second.path().join("t"))
            }
        );
        let absolute = second.path().join("t").display().to_string();
        assert_eq!(
            locate(&absolute, &[]).unwrap(),
            ThemeSource::Disk {
                dir: canonical(&second.path().join("t"))
            }
        );
    }
}
