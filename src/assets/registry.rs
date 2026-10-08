//! `AssetRegistry`: registered images/binary outputs and `asset:<id>` placeholders.
//!
//! Contract: `docs/spec/ir.md` §3. Frontends never read image bytes or decide URLs: they
//! register an asset and write the placeholder `asset:<id>` (exactly 12 `[0-9a-f]`) into
//! their HTML; the emitter (`assets::emit`, T14) later rewrites every placeholder into a
//! `data:` URI, a site-mode file or a dev-server route. Implemented by T01; T14 adds
//! `merge`, `len`, `is_empty` and `read`.
//!
//! Ids are content-derived and therefore stable across runs: the first 12 hex chars of the
//! BLAKE3 hash of the absolute path (files) or of the bytes (in-memory assets). Registering
//! the same path or bytes again returns the same id and keeps the first registration.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

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
    /// The path is hashed as given, so callers pass the resolved absolute path (relative
    /// paths would make ids depend on the working directory).
    pub fn register_file(&mut self, abs_path: &Path) -> String {
        let id = hash_id(abs_path.to_string_lossy().as_bytes());
        self.assets.entry(id.clone()).or_insert_with(|| Asset {
            id: id.clone(),
            mime: mime_guess::from_path(abs_path)
                .first_or_octet_stream()
                .essence_str()
                .to_string(),
            data: AssetData::File(abs_path.to_path_buf()),
            file_name_hint: abs_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "asset".to_string()),
        });
        id
    }

    /// Registers in-memory bytes. id = first 12 hex chars of blake3(bytes). Returns the id.
    pub fn register_bytes(&mut self, bytes: Vec<u8>, mime: &str, file_name_hint: &str) -> String {
        let id = hash_id(&bytes);
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
    /// base_dir>` (`/`-separated; the absolute path when the file is not under `base_dir`)
    /// or `inline:<byte length>`.
    pub fn infos(&self, base_dir: &Path) -> Vec<AssetInfo> {
        self.iter()
            .map(|asset| AssetInfo {
                id: asset.id.clone(),
                mime: asset.mime.clone(),
                origin: match &asset.data {
                    AssetData::File(path) => format!("file:{}", display_relative(path, base_dir)),
                    AssetData::Bytes(bytes) => format!("inline:{}", bytes.len()),
                },
            })
            .collect()
    }

    /// The placeholder URL for an id: `asset:<id>`.
    pub fn placeholder(id: &str) -> String {
        format!("asset:{id}")
    }
}

/// First [`ID_LEN`] lowercase hex chars of the BLAKE3 hash of `bytes`.
fn hash_id(bytes: &[u8]) -> String {
    let hex = blake3::hash(bytes).to_hex();
    hex[..ID_LEN].to_string()
}

/// `path` relative to `base`, joined with `/`; the full path when it is not under `base`.
fn display_relative(path: &Path, base: &Path) -> String {
    match path.strip_prefix(base) {
        Ok(rel) => rel
            .components()
            .map(|c| match c {
                Component::Normal(part) => part.to_string_lossy().into_owned(),
                other => other.as_os_str().to_string_lossy().into_owned(),
            })
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_id(id: &str) -> bool {
        id.len() == ID_LEN && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }

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
}
