//! `AssetRegistry`: registered images/binary outputs and `asset:<id>` placeholders.
//!
//! Contract: `docs/spec/ir.md` §3. Frontends never read image bytes or decide URLs: they
//! register an asset and write the placeholder `asset:<id>` (exactly 12 `[0-9a-f]`) into
//! their HTML; the emitter (`assets::emit`, T14) later rewrites every placeholder into a
//! `data:` URI, a site-mode file or a dev-server route. Implemented by T01; T14 added
//! `merge`, `len`, `is_empty` and `read` (the only place that reads a file asset's bytes).
//!
//! Ids are content-derived and therefore stable across runs: the first 12 hex chars of the
//! BLAKE3 hash of the absolute path (files) or of the bytes (in-memory assets). Registering
//! the same path or bytes again returns the same id and keeps the first registration.
//! File paths are resolved first ([`resolve_path`]: canonicalized when the absolute path
//! exists, so symlinks are followed exactly as the OS does; otherwise normalized lexically),
//! so `/talk/./img.png`, `/talk/sub/../img.png` and `/talk/img.png` are one asset.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};
use crate::ir::AssetInfo;

/// Number of hex characters in an asset id.
pub const ID_LEN: usize = 12;

/// Where an asset's bytes come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetData {
    /// A file on disk, by absolute path; read lazily by the emitter.
    File(PathBuf),
    /// Bytes held in memory (e.g. a decoded notebook PNG output).
    Bytes(Vec<u8>),
}

/// One registered asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// 12 lowercase hex chars.
    pub id: String,
    /// MIME type, e.g. `"image/png"`.
    pub mime: String,
    /// The bytes or the file they live in.
    pub data: AssetData,
    /// Preferred output file name in site mode, e.g. "plot.png" or "output.png".
    pub file_name_hint: String,
}

/// The assets of one deck, keyed (and iterated) by id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetRegistry {
    assets: BTreeMap<String, Asset>,
}

impl AssetRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a file by absolute path. id = first 12 hex chars of blake3(path as UTF-8)
    /// (lossy for non-UTF-8 paths). MIME from the extension via `mime_guess`, default
    /// "application/octet-stream". Does NOT read the file. Returns the id.
    ///
    /// The path is first resolved with [`resolve_path`] (canonicalized when it is absolute
    /// and exists — only metadata is touched — else normalized lexically), and the resolved
    /// path is what is hashed and stored in [`AssetData::File`]: `base_dir.join("./x.png")`,
    /// `base_dir.join("a/../x.png")` and `base_dir.join("x.png")` all get the same id, and a
    /// `..` that crosses a symlinked directory resolves to the file the OS would open.
    /// Callers pass the resolved absolute path; a relative path is accepted and hashed as
    /// given (after lexical normalization), which makes the id depend on the path spelling
    /// rather than on the working directory.
    pub fn register_file(&mut self, abs_path: &Path) -> String {
        let path = resolve_path(abs_path);
        let id = content_hash(path.to_string_lossy().as_bytes());
        self.assets.entry(id.clone()).or_insert_with(|| Asset {
            id: id.clone(),
            mime: mime_guess::from_path(&path)
                .first_or_octet_stream()
                .essence_str()
                .to_string(),
            file_name_hint: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "asset".to_string()),
            data: AssetData::File(path),
        });
        id
    }

    /// Registers in-memory bytes. id = first 12 hex chars of blake3(bytes). Returns the id.
    pub fn register_bytes(&mut self, bytes: Vec<u8>, mime: &str, file_name_hint: &str) -> String {
        let id = content_hash(&bytes);
        self.assets.entry(id.clone()).or_insert_with(|| Asset {
            id: id.clone(),
            mime: mime.to_string(),
            data: AssetData::Bytes(bytes),
            file_name_hint: file_name_hint.to_string(),
        });
        id
    }

    /// The asset with this id, if registered.
    pub fn get(&self, id: &str) -> Option<&Asset> {
        self.assets.get(id)
    }

    /// All assets, ordered by id.
    pub fn iter(&self) -> impl Iterator<Item = &Asset> {
        self.assets.values()
    }

    /// Summaries for `Deck.assets`, ordered by id. `origin` is `file:<path relative to
    /// base_dir>` (`/`-separated; the full stored path when the file is not under
    /// `base_dir`, e.g. for `base_dir.join("../shared/logo.png")`) or `inline:<byte length>`.
    /// `base_dir` is resolved with [`resolve_path`] before the comparison, like the files.
    pub fn infos(&self, base_dir: &Path) -> Vec<AssetInfo> {
        let base_dir = resolve_path(base_dir);
        self.iter()
            .map(|asset| AssetInfo {
                id: asset.id.clone(),
                mime: asset.mime.clone(),
                origin: match &asset.data {
                    AssetData::File(path) => format!("file:{}", display_relative(path, &base_dir)),
                    AssetData::Bytes(bytes) => format!("inline:{}", bytes.len()),
                },
            })
            .collect()
    }

    /// The placeholder URL for an id: `asset:<id>`.
    pub fn placeholder(id: &str) -> String {
        format!("asset:{id}")
    }

    /// Moves every asset of `other` into `self`. Ids are content-derived, so an id present
    /// in both registries names the same asset; the existing registration is kept, exactly
    /// as when the same path or bytes are registered twice.
    pub fn merge(&mut self, other: AssetRegistry) {
        for (id, asset) in other.assets {
            self.assets.entry(id).or_insert(asset);
        }
    }

    /// Number of registered assets.
    pub fn len(&self) -> usize {
        self.assets.len()
    }

    /// True if nothing is registered.
    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }

    /// The bytes of asset `id`: borrowed for [`AssetData::Bytes`], read from disk now for
    /// [`AssetData::File`] (registration never reads files).
    ///
    /// Errors: an unregistered id is [`Error::Other`] naming the id; a file that no longer
    /// exists (deleted between parsing and emitting) is
    /// [`Error::MissingAsset`]` { path, span: None }`; any other read failure is
    /// [`Error::Io`] with the path prepended to the message (same [`std::io::ErrorKind`]).
    pub fn read(&self, id: &str) -> Result<Cow<'_, [u8]>> {
        let asset = self.get(id).ok_or_else(|| unknown_id(id))?;
        match &asset.data {
            AssetData::Bytes(bytes) => Ok(Cow::Borrowed(bytes)),
            AssetData::File(path) => match std::fs::read(path) {
                Ok(bytes) => Ok(Cow::Owned(bytes)),
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) =>
                {
                    Err(Error::MissingAsset {
                        path: path.clone(),
                        span: None,
                    })
                }
                Err(err) => Err(Error::Io(io::Error::new(
                    err.kind(),
                    format!("{}: {err}", path.display()),
                ))),
            },
        }
    }
}

/// The error for an id that is not registered: [`Error::Other`] naming the id.
pub(crate) fn unknown_id(id: &str) -> Error {
    Error::Other(format!(
        "unknown asset id '{id}': the placeholder 'asset:{id}' does not name a registered asset"
    ))
}

/// First [`ID_LEN`] lowercase hex chars of the BLAKE3 hash of `bytes`: the id of an asset
/// registered by its bytes, and the hash in a site-mode file name (`assets::emit`), which
/// is always taken from the bytes, also for a file asset (whose id hashes its path).
pub fn content_hash(bytes: &[u8]) -> String {
    let hex = blake3::hash(bytes).to_hex();
    hex[..ID_LEN].to_string()
}

/// `path` relative to `base`, joined with `/`; the full path when it is not under `base`.
/// Both paths are already normalized, so `..` never appears in the relative form.
fn display_relative(path: &Path, base: &Path) -> String {
    match path.strip_prefix(base) {
        Ok(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

/// The path the registry stores for `path`: `std::fs::canonicalize(path)` when `path` is
/// absolute and exists (symlinks resolved exactly as the OS resolves them when opening the
/// file), otherwise [`normalize_lexically`]. Relative paths are never made absolute.
pub fn resolve_path(path: &Path) -> PathBuf {
    if path.is_absolute()
        && let Ok(canonical) = std::fs::canonicalize(path)
    {
        return canonical;
    }
    normalize_lexically(path)
}

/// Removes `.` components and resolves `..` against the preceding component, without
/// touching the filesystem (so symlinks are not followed: `a/link/../b` becomes `a/b`).
///
/// `..` directly under the root is dropped (`/..` is `/`); leading `..` components of a
/// relative path are kept (`../a/../b` becomes `../b`). An input that normalizes to nothing
/// (`""`, `"."`, `"a/.."`) becomes `"."`. Shared by the registry and config discovery.
pub fn normalize_lexically(path: &Path) -> PathBuf {
    let mut parts: Vec<Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match parts.last() {
                Some(Component::Normal(_)) => {
                    parts.pop();
                }
                Some(Component::RootDir) => {}
                // Empty, a drive prefix without root (Windows `C:..`) or a leading `..`.
                _ => parts.push(component),
            },
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return PathBuf::from(".");
    }
    parts.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_id(id: &str) -> bool {
        id.len() == ID_LEN && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }

    // The four tests on POSIX paths: `/abs/…` is absolute, `/` is the separator, and the
    // hashed and shown paths are the spellings below. On Windows `/abs` is relative to the
    // current drive and the normalized path uses `\`; the integration tests cover it with
    // real paths.
    #[cfg(unix)]
    #[test]
    fn file_ids_hash_the_path_and_are_idempotent() {
        let mut reg = AssetRegistry::new();
        let path = Path::new("/abs/talk/figures/plot.png");
        let id = reg.register_file(path);
        assert!(is_id(&id), "{id}");
        assert_eq!(
            id,
            blake3::hash(b"/abs/talk/figures/plot.png").to_hex()[..12]
        );
        assert_eq!(reg.register_file(path), id);
        assert_eq!(reg.iter().count(), 1);
        let asset = reg.get(&id).unwrap();
        assert_eq!(asset.mime, "image/png");
        assert_eq!(asset.file_name_hint, "plot.png");
        assert_eq!(asset.data, AssetData::File(path.to_path_buf()));
        // A different path is a different asset, even if the file content were identical.
        assert_ne!(
            reg.register_file(Path::new("/abs/talk/figures/other.png")),
            id
        );
        assert_eq!(reg.iter().count(), 2);
    }

    #[test]
    fn register_file_does_not_touch_the_disk() {
        let mut reg = AssetRegistry::new();
        let id = reg.register_file(Path::new("/definitely/not/here.svg"));
        assert_eq!(reg.get(&id).unwrap().mime, "image/svg+xml");
    }

    #[test]
    fn mime_comes_from_the_extension() {
        let mut reg = AssetRegistry::new();
        for (path, mime) in [
            ("/x/a.jpg", "image/jpeg"),
            ("/x/a.woff2", "font/woff2"),
            ("/x/a.css", "text/css"),
            ("/x/noext", "application/octet-stream"),
            ("/x/a.unknownext", "application/octet-stream"),
        ] {
            let id = reg.register_file(Path::new(path));
            assert_eq!(reg.get(&id).unwrap().mime, mime, "{path}");
        }
    }

    #[test]
    fn byte_ids_hash_the_content() {
        let mut reg = AssetRegistry::new();
        let a = reg.register_bytes(b"\x89PNG one".to_vec(), "image/png", "output.png");
        let b = reg.register_bytes(b"\x89PNG two".to_vec(), "image/png", "output.png");
        assert!(is_id(&a) && is_id(&b));
        assert_ne!(a, b);
        assert_eq!(a, blake3::hash(b"\x89PNG one").to_hex()[..12]);
        // Same bytes again: same id, first registration kept.
        assert_eq!(
            reg.register_bytes(b"\x89PNG one".to_vec(), "image/x-other", "again.png"),
            a
        );
        let asset = reg.get(&a).unwrap();
        assert_eq!(asset.mime, "image/png");
        assert_eq!(asset.file_name_hint, "output.png");
        assert_eq!(asset.data, AssetData::Bytes(b"\x89PNG one".to_vec()));
        assert_eq!(reg.iter().count(), 2);
        assert!(reg.get("000000000000").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn iteration_and_infos_are_ordered_by_id() {
        let mut reg = AssetRegistry::new();
        let base = Path::new("/abs/talk");
        let ids = [
            reg.register_file(&base.join("figures/plot.png")),
            reg.register_bytes(vec![1, 2, 3], "image/gif", "output.gif"),
            reg.register_file(Path::new("/elsewhere/logo.svg")),
        ];
        let mut sorted = ids.to_vec();
        sorted.sort();
        let iterated: Vec<&str> = reg.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(iterated, sorted);

        let infos = reg.infos(base);
        assert_eq!(
            infos.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            sorted
        );
        let origin = |id: &str| infos.iter().find(|i| i.id == id).unwrap().origin.clone();
        assert_eq!(origin(&ids[0]), "file:figures/plot.png");
        assert_eq!(origin(&ids[1]), "inline:3");
        assert_eq!(origin(&ids[2]), "file:/elsewhere/logo.svg");
        let info = infos.iter().find(|i| i.id == ids[0]).unwrap();
        assert_eq!(info.mime, "image/png");
    }

    #[cfg(unix)]
    #[test]
    fn equivalent_spellings_of_one_file_share_an_id() {
        let mut reg = AssetRegistry::new();
        let base = Path::new("/abs/talk");
        let id = reg.register_file(&base.join("images/ok.png"));
        for spelling in [
            "./images/ok.png",
            "images/./ok.png",
            "sub/../images/ok.png",
            "images/ok.png/",
        ] {
            assert_eq!(reg.register_file(&base.join(spelling)), id, "{spelling}");
        }
        assert_eq!(
            reg.register_file(Path::new("/abs/./talk/images/ok.png")),
            id
        );
        assert_eq!(reg.iter().count(), 1);
        // The normalized path is what is hashed and stored.
        assert_eq!(id, blake3::hash(b"/abs/talk/images/ok.png").to_hex()[..12]);
        let asset = reg.get(&id).unwrap();
        assert_eq!(
            asset.data,
            AssetData::File(PathBuf::from("/abs/talk/images/ok.png"))
        );
        assert_eq!(asset.file_name_hint, "ok.png");
        let infos = reg.infos(base);
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].origin, "file:images/ok.png");
    }

    #[cfg(unix)]
    #[test]
    fn parent_relative_paths_outside_base_get_the_absolute_origin() {
        let mut reg = AssetRegistry::new();
        let base = Path::new("/abs/talk");
        let outside = reg.register_file(&base.join("../shared/logo.png"));
        assert_eq!(
            reg.register_file(Path::new("/abs/shared/logo.png")),
            outside
        );
        assert_eq!(
            reg.get(&outside).unwrap().data,
            AssetData::File(PathBuf::from("/abs/shared/logo.png"))
        );
        // `..` that stays inside base_dir is still relative.
        let inside = reg.register_file(&base.join("../talk/figures/plot.png"));
        assert_eq!(reg.iter().count(), 2);
        let infos = reg.infos(base);
        let origin = |id: &str| infos.iter().find(|i| i.id == id).unwrap().origin.clone();
        assert_eq!(origin(&outside), "file:/abs/shared/logo.png");
        assert_eq!(origin(&inside), "file:figures/plot.png");
        // A base_dir spelled with `.`/`..` is normalized before the comparison.
        let infos = reg.infos(Path::new("/abs/./other/../talk/"));
        assert!(infos.iter().any(|i| i.origin == "file:figures/plot.png"));
        // A sibling directory whose name merely starts with base_dir's name is outside.
        let sibling = reg.register_file(Path::new("/abs/talk2/x.png"));
        let infos = reg.infos(base);
        let info = infos.iter().find(|i| i.id == sibling).unwrap();
        assert_eq!(info.origin, "file:/abs/talk2/x.png");
    }

    #[test]
    fn relative_paths_are_normalized_but_not_made_absolute() {
        let mut reg = AssetRegistry::new();
        let id = reg.register_file(Path::new("tests/fixtures/md/./images/../images/ok.png"));
        assert_eq!(
            reg.get(&id).unwrap().data,
            AssetData::File(PathBuf::from("tests/fixtures/md/images/ok.png"))
        );
        let infos = reg.infos(Path::new("tests/fixtures/md"));
        assert_eq!(infos[0].origin, "file:images/ok.png");
    }

    #[test]
    fn normalize_lexically_cases() {
        for (input, expected) in [
            ("/a/b/../c/./d", "/a/c/d"),
            ("/..", "/"),
            ("/../a", "/a"),
            ("/a/b/", "/a/b"),
            ("a/./b", "a/b"),
            ("./a", "a"),
            ("../a", "../a"),
            ("../a/../b", "../b"),
            ("a/../../b", "../b"),
            ("a/..", "."),
            (".", "."),
            ("", "."),
        ] {
            assert_eq!(
                normalize_lexically(Path::new(input)),
                PathBuf::from(expected),
                "{input:?}"
            );
        }
    }

    #[test]
    fn placeholder_format() {
        assert_eq!(
            AssetRegistry::placeholder("3f9a1c2b7e0d"),
            "asset:3f9a1c2b7e0d"
        );
    }

    #[test]
    fn default_equals_new() {
        assert_eq!(AssetRegistry::default(), AssetRegistry::new());
        assert_eq!(AssetRegistry::new().iter().count(), 0);
    }

    #[test]
    fn len_and_is_empty_count_distinct_ids() {
        let mut reg = AssetRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        reg.register_file(Path::new("/abs/a.png"));
        reg.register_file(Path::new("/abs/./a.png"));
        reg.register_bytes(vec![1], "image/png", "x.png");
        reg.register_bytes(vec![1], "image/png", "y.png");
        assert!(!reg.is_empty());
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn merge_adds_new_ids_and_keeps_existing_registrations() {
        let mut deck = AssetRegistry::new();
        let shared = deck.register_bytes(vec![7, 7], "image/png", "deck.png");
        let only_deck = deck.register_file(Path::new("/abs/deck.svg"));

        let mut theme = AssetRegistry::new();
        assert_eq!(
            theme.register_bytes(vec![7, 7], "image/x-other", "theme.png"),
            shared
        );
        let only_theme = theme.register_file(Path::new("/abs/theme/bg.jpg"));

        deck.merge(theme);
        assert_eq!(deck.len(), 3);
        let ids: Vec<&str> = deck.iter().map(|a| a.id.as_str()).collect();
        let mut expected = vec![shared.as_str(), only_deck.as_str(), only_theme.as_str()];
        expected.sort();
        assert_eq!(ids, expected);
        // The first registration of a shared id wins.
        assert_eq!(deck.get(&shared).unwrap().file_name_hint, "deck.png");
        assert_eq!(deck.get(&shared).unwrap().mime, "image/png");
        assert_eq!(deck.get(&only_theme).unwrap().mime, "image/jpeg");

        deck.merge(AssetRegistry::new());
        assert_eq!(deck.len(), 3);
    }

    #[test]
    fn read_borrows_bytes_and_reads_files_lazily() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("late.png");
        let mut reg = AssetRegistry::new();
        // Registered before it exists: registration never touches the file.
        let file_id = reg.register_file(&path);
        std::fs::write(&path, b"written after registration").unwrap();
        assert_eq!(&*reg.read(&file_id).unwrap(), b"written after registration");
        assert!(matches!(reg.read(&file_id).unwrap(), Cow::Owned(_)));

        let bytes_id = reg.register_bytes(b"inline".to_vec(), "image/png", "o.png");
        let bytes = reg.read(&bytes_id).unwrap();
        assert!(matches!(bytes, Cow::Borrowed(b"inline")));
    }

    #[test]
    fn read_of_a_deleted_file_is_missing_asset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gone.png");
        std::fs::write(&path, b"png").unwrap();
        let mut reg = AssetRegistry::new();
        let id = reg.register_file(&path);
        let AssetData::File(stored) = reg.get(&id).unwrap().data.clone() else {
            panic!("expected a file asset");
        };
        std::fs::remove_file(&path).unwrap();
        match reg.read(&id) {
            Err(Error::MissingAsset { path, span }) => {
                assert_eq!(path, stored);
                assert_eq!(span, None);
            }
            other => panic!("expected MissingAsset, got {other:?}"),
        }
        // A parent that became a regular file is missing too.
        let nested = dir.path().join("dir/img.png");
        let nested_id = reg.register_file(&nested);
        std::fs::write(dir.path().join("dir"), b"not a directory").unwrap();
        assert!(matches!(
            reg.read(&nested_id),
            Err(Error::MissingAsset { .. })
        ));
    }

    #[test]
    fn read_failures_other_than_missing_are_io_errors_naming_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = AssetRegistry::new();
        // Reading a directory fails with an I/O error that is not "not found".
        let id = reg.register_file(dir.path());
        let err = reg.read(&id).unwrap_err();
        assert!(matches!(err, Error::Io(_)), "{err:?}");
        let resolved = resolve_path(dir.path());
        assert!(
            err.to_string().contains(&*resolved.to_string_lossy()),
            "{err}"
        );
    }

    #[test]
    fn read_of_an_unknown_id_names_the_id() {
        let err = AssetRegistry::new().read("0123456789ab").unwrap_err();
        assert!(matches!(err, Error::Other(_)), "{err:?}");
        assert!(err.to_string().contains("0123456789ab"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn parent_dir_through_a_symlink_resolves_like_the_os() {
        // root/current -> root/2026/talk; the image lives at root/2026/shared/logo.png, so
        // `current/../shared/logo.png` exists for the OS but `root/shared/logo.png` does not.
        let tmp = std::env::temp_dir().join(format!("presenter-registry-{}", std::process::id()));
        let root = tmp.join("root");
        std::fs::create_dir_all(root.join("2026/talk")).unwrap();
        std::fs::create_dir_all(root.join("2026/shared")).unwrap();
        std::fs::write(root.join("2026/shared/logo.png"), b"png").unwrap();
        let link = root.join("current");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(root.join("2026/talk"), &link).unwrap();

        let joined = link.join("../shared/logo.png");
        assert!(joined.is_file());
        let mut registry = AssetRegistry::new();
        let id = registry.register_file(&joined);
        match &registry.get(&id).unwrap().data {
            AssetData::File(stored) => {
                assert!(stored.is_file(), "stored path must be readable: {stored:?}");
                assert_eq!(std::fs::read(stored).unwrap(), b"png");
            }
            other => panic!("expected a file asset, got {other:?}"),
        }
        // The same file reached through the real directory is the same asset.
        assert_eq!(
            registry.register_file(&root.join("2026/shared/logo.png")),
            id
        );
        let infos = registry.infos(&link);
        assert_eq!(infos.len(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
