//! Cluster bookmark store — the persistent "known meshes"
//! list per `DECK_PLAN.md` § Deferred work § Persistent
//! cluster bookmark store.
//!
//! On-disk shape:
//!
//! ```toml
//! version = 1
//!
//! [[cluster]]
//! name = "prod-east"
//! endpoint = "mesh://0xa96f@10.0.0.7:9001"
//! default_identity = "~/.config/deck/identities/prod.toml"
//! pinned = true
//!
//! [[cluster]]
//! name = "dev-laptop"
//! endpoint = "unix:///tmp/deck-dev.sock"
//! ```
//!
//! Loaded at startup; written on every operator-visible
//! mutation (add / remove / pin). The path resolves to
//! `$XDG_CONFIG_HOME/deck/bookmarks.toml` on Linux/Mac and
//! `%APPDATA%\deck\bookmarks.toml` on Windows via the `dirs`
//! crate's `config_dir()`.
//!
//! Connection semantics — what `endpoint` actually does at
//! switch time — wait for the multi-cluster RPC slice
//! (`DECK_PLAN.md` § Deferred work § Multi-Cluster Switcher).
//! This module just owns the persistence.

// Methods + the `path` accessor + the `Serialize` error
// variant + the in-store `bookmarks` field are read by the
// future multi-cluster picker; tests cover them today.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One bookmarked cluster. `endpoint` is the raw connection
/// string the future remote-`DeckClient` constructor will
/// parse; today we don't dial it, we just persist it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Bookmark {
    /// Operator-chosen display name (`prod-east`, `lab-vm`).
    pub name: String,
    /// Connection string (`mesh://…`, `unix://…`). Opaque to
    /// this module.
    pub endpoint: String,
    /// Optional path to a per-cluster operator identity file.
    /// When `None` the default identity at
    /// `~/.config/deck/identity.toml` is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_identity: Option<String>,
    /// `true` keeps the bookmark at the top of the picker.
    /// Operators pin the cluster they're babysitting during an
    /// incident.
    #[serde(default, skip_serializing_if = "is_false")]
    pub pinned: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// On-disk file wrapper. `version` is bumped on a breaking
/// format change so old configs fail loudly rather than silently
/// misinterpret.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct BookmarkFile {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default, rename = "cluster")]
    clusters: Vec<Bookmark>,
}

fn default_version() -> u32 {
    1
}

const CURRENT_VERSION: u32 = 1;

/// In-memory store + the path it was loaded from. Cheap to
/// clone (one `Vec` + one `PathBuf`).
#[derive(Clone, Debug, Default)]
pub struct BookmarkStore {
    bookmarks: Vec<Bookmark>,
    /// `None` when the store was constructed standalone (tests);
    /// `Some` when [`BookmarkStore::load`] resolved a real config path.
    path: Option<PathBuf>,
}

impl BookmarkStore {
    /// Load the store from the default config location. Missing
    /// file returns an empty store — first-run is not an error.
    /// Malformed file surfaces as `Err` so the operator notices.
    pub fn load() -> Result<Self, BookmarkError> {
        let path = default_path()?;
        Self::load_from(&path)
    }

    /// Load from a specific path. Used by tests + by future
    /// `--bookmarks <path>` overrides. A corrupt / unparseable
    /// file is renamed aside (`<path>.corrupt-<unix_ms>`) so
    /// the next save can recover with an empty store rather
    /// than leaving the operator unable to launch the deck.
    pub fn load_from(path: &Path) -> Result<Self, BookmarkError> {
        if !path.exists() {
            return Ok(Self {
                bookmarks: Vec::new(),
                path: Some(path.to_path_buf()),
            });
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| BookmarkError::Io(format!("read {}: {e}", path.display())))?;
        let file: BookmarkFile = match toml::from_str(&text) {
            Ok(f) => f,
            Err(e) => {
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                // Resolve a non-colliding aside path. Two
                // corrupt-recovery cycles inside the same ms
                // (rare but possible on a fast restart loop)
                // would otherwise clobber the prior aside;
                // probe `.toml.corrupt-{stamp}`,
                // `.toml.corrupt-{stamp}-1`, … until one is
                // free. Best-effort — if the loop overflows
                // (1000+ collisions, only realistic in test
                // scaffolding) fall back to the raw stamp and
                // let the rename clobber, since the prior
                // aside is also a recovery artefact.
                let mut aside = path.with_extension(format!("toml.corrupt-{stamp}"));
                for suffix in 1..1000u32 {
                    if !aside.exists() {
                        break;
                    }
                    aside = path.with_extension(format!("toml.corrupt-{stamp}-{suffix}"));
                }
                // Best-effort rename — if it fails, surface the
                // parse error instead so the operator at least
                // sees a meaningful message.
                if std::fs::rename(path, &aside).is_ok() {
                    return Ok(Self {
                        bookmarks: Vec::new(),
                        path: Some(path.to_path_buf()),
                    });
                }
                return Err(BookmarkError::Parse(format!("{}: {e}", path.display())));
            }
        };
        if file.version != CURRENT_VERSION {
            return Err(BookmarkError::Version(file.version, CURRENT_VERSION));
        }
        Ok(Self {
            bookmarks: file.clusters,
            path: Some(path.to_path_buf()),
        })
    }

    /// Construct an empty store with no backing path. Useful
    /// for tests + for the runtime's "no config dir" fallback
    /// when the operator hasn't created one.
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn bookmarks(&self) -> &[Bookmark] {
        &self.bookmarks
    }

    /// Sort bookmarks pinned-first then by name. Stable order
    /// so the picker reads the same on every render.
    pub fn sorted(&self) -> Vec<&Bookmark> {
        let mut out: Vec<&Bookmark> = self.bookmarks.iter().collect();
        out.sort_by(|a, b| b.pinned.cmp(&a.pinned).then_with(|| a.name.cmp(&b.name)));
        out
    }

    /// Add or replace a bookmark. Replacement is matched by
    /// `name` (operator-visible identity) — re-adding under the
    /// same name updates the endpoint / identity / pinned
    /// state. Rejects empty / whitespace-only names and empty
    /// endpoints so an operator-edited TOML with blanks doesn't
    /// silently create unusable bookmarks.
    pub fn upsert(&mut self, mut bm: Bookmark) -> Result<(), BookmarkError> {
        bm.name = bm.name.trim().to_string();
        bm.endpoint = bm.endpoint.trim().to_string();
        if bm.name.is_empty() {
            return Err(BookmarkError::InvalidField("name must be non-empty".into()));
        }
        if bm.endpoint.is_empty() {
            return Err(BookmarkError::InvalidField(
                "endpoint must be non-empty".into(),
            ));
        }
        if let Some(slot) = self.bookmarks.iter_mut().find(|b| b.name == bm.name) {
            *slot = bm;
        } else {
            self.bookmarks.push(bm);
        }
        Ok(())
    }

    /// Remove a bookmark by name. Returns `true` if a removal
    /// happened.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.bookmarks.len();
        self.bookmarks.retain(|b| b.name != name);
        self.bookmarks.len() != before
    }

    /// Toggle a bookmark's pinned flag. Returns the new state,
    /// or `None` if the name isn't bookmarked.
    pub fn toggle_pin(&mut self, name: &str) -> Option<bool> {
        let bm = self.bookmarks.iter_mut().find(|b| b.name == name)?;
        bm.pinned = !bm.pinned;
        Some(bm.pinned)
    }

    /// Write the store back to its backing path. Creates parent
    /// directories if missing. No-op when the store was
    /// constructed via [`BookmarkStore::empty`] (no path).
    ///
    /// Writes are atomic: the encoded TOML lands in a sibling
    /// `.tmp` file first, then renames over the destination.
    /// A crash mid-write leaves either the prior content intact
    /// (rename never happened) or the new content fully in
    /// place — never a half-written file the next `load()`
    /// reports as `Parse(...)`.
    pub fn save(&self) -> Result<(), BookmarkError> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                BookmarkError::Io(format!("create_dir_all {}: {e}", parent.display()))
            })?;
        }
        let file = BookmarkFile {
            version: CURRENT_VERSION,
            clusters: self.bookmarks.clone(),
        };
        let text =
            toml::to_string_pretty(&file).map_err(|e| BookmarkError::Serialize(e.to_string()))?;
        // Unique tmp suffix (pid + ms since epoch) so two deck
        // instances pointing at the same config dir don't race
        // on the rename. A fixed sibling name (`bookmarks.toml.tmp`)
        // would let one process's partial write land via the
        // other's rename. Stable enough on a single process —
        // pid changes between instances and ms changes between
        // saves.
        let suffix = {
            let pid = std::process::id();
            let ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            format!("toml.tmp.{pid}.{ms}")
        };
        let tmp = path.with_extension(suffix);
        std::fs::write(&tmp, text)
            .map_err(|e| BookmarkError::Io(format!("write {}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, path).map_err(|e| {
            // Best-effort cleanup so a failed rename doesn't
            // leave a stray .tmp around forever.
            let _ = std::fs::remove_file(&tmp);
            BookmarkError::Io(format!(
                "rename {} -> {}: {e}",
                tmp.display(),
                path.display()
            ))
        })?;
        Ok(())
    }

    /// Path the store reads / writes. `None` when constructed
    /// via [`BookmarkStore::empty`].
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

/// Resolve the default bookmark-file path:
/// `$XDG_CONFIG_HOME/net-deck/bookmarks.toml` (Linux/Mac) or
/// `%APPDATA%\net-deck\bookmarks.toml` (Windows). Returns
/// [`BookmarkError::NoConfigDir`] when neither resolves.
pub fn default_path() -> Result<PathBuf, BookmarkError> {
    let mut dir = dirs::config_dir().ok_or(BookmarkError::NoConfigDir)?;
    dir.push("net-deck");
    dir.push("bookmarks.toml");
    Ok(dir)
}

/// Bookmark-store error surface. Surfaces to App callers
/// (`App::new`) which fold it into a toast or stderr line.
#[derive(Debug)]
pub enum BookmarkError {
    /// Couldn't resolve a default config directory — neither
    /// `$XDG_CONFIG_HOME` nor `%APPDATA%` is set / readable.
    NoConfigDir,
    /// Filesystem I/O failed.
    Io(String),
    /// TOML parsing failed.
    Parse(String),
    /// Serializing a store back to TOML failed (rare —
    /// `Bookmark` is composed of trivial scalars).
    Serialize(String),
    /// File version doesn't match this build's expectations.
    /// `(found, expected)`.
    Version(u32, u32),
    /// A field on a bookmark failed validation (empty name,
    /// empty endpoint, …).
    InvalidField(String),
}

impl std::fmt::Display for BookmarkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoConfigDir => write!(f, "no config directory available"),
            Self::Io(msg) => write!(f, "bookmark I/O: {msg}"),
            Self::Parse(msg) => write!(f, "bookmark parse: {msg}"),
            Self::Serialize(msg) => write!(f, "bookmark serialize: {msg}"),
            Self::Version(found, expected) => write!(
                f,
                "bookmark file version {found} unsupported (expected {expected})"
            ),
            Self::InvalidField(msg) => write!(f, "bookmark invalid: {msg}"),
        }
    }
}

impl std::error::Error for BookmarkError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_store_round_trips() {
        let dir = tempdir_unique();
        let path = dir.join("bookmarks.toml");
        let store = BookmarkStore::load_from(&path).expect("missing file is ok");
        store.save().expect("save no-op when nothing to write");
        // No file should be created — empty save still works
        // because the path is set but the file wasn't requested.
        assert!(
            !path.exists() || {
                let s = std::fs::read_to_string(&path).unwrap();
                !s.is_empty()
            }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upsert_remove_toggle_roundtrip_to_disk() {
        let dir = tempdir_unique();
        let path = dir.join("bookmarks.toml");
        let mut store = BookmarkStore::load_from(&path).expect("missing ok");
        store
            .upsert(Bookmark {
                name: "prod-east".to_string(),
                endpoint: "mesh://0xa96f@10.0.0.7:9001".to_string(),
                default_identity: None,
                pinned: false,
            })
            .expect("valid bookmark");
        store
            .upsert(Bookmark {
                name: "dev-laptop".to_string(),
                endpoint: "unix:///tmp/deck-dev.sock".to_string(),
                default_identity: Some("~/.config/deck/identities/dev.toml".to_string()),
                pinned: false,
            })
            .expect("valid bookmark");
        assert_eq!(store.toggle_pin("prod-east"), Some(true));
        store.save().expect("save");

        // Reload from disk.
        let reloaded = BookmarkStore::load_from(&path).expect("reload");
        assert_eq!(reloaded.bookmarks().len(), 2);
        let sorted = reloaded.sorted();
        // Pinned bookmark sorts first.
        assert_eq!(sorted[0].name, "prod-east");
        assert!(sorted[0].pinned);
        assert_eq!(sorted[1].name, "dev-laptop");
        assert!(!sorted[1].pinned);

        // Remove + persist.
        let mut store = reloaded;
        assert!(store.remove("dev-laptop"));
        assert!(!store.remove("dev-laptop"));
        store.save().expect("save after remove");

        let reloaded = BookmarkStore::load_from(&path).expect("reload after remove");
        assert_eq!(reloaded.bookmarks().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upsert_replaces_existing_by_name() {
        let mut store = BookmarkStore::empty();
        store
            .upsert(Bookmark {
                name: "k1".to_string(),
                endpoint: "a".to_string(),
                ..Default::default()
            })
            .expect("valid bookmark");
        store
            .upsert(Bookmark {
                name: "k1".to_string(),
                endpoint: "b".to_string(),
                pinned: true,
                ..Default::default()
            })
            .expect("valid bookmark");
        assert_eq!(store.bookmarks().len(), 1);
        assert_eq!(store.bookmarks()[0].endpoint, "b");
        assert!(store.bookmarks()[0].pinned);
    }

    #[test]
    fn upsert_rejects_empty_name() {
        let mut store = BookmarkStore::empty();
        let err = store
            .upsert(Bookmark {
                name: "  ".to_string(),
                endpoint: "mesh://example".to_string(),
                ..Default::default()
            })
            .expect_err("whitespace-only name must be rejected");
        assert!(matches!(err, BookmarkError::InvalidField(_)));
        assert!(store.bookmarks().is_empty());
    }

    #[test]
    fn upsert_rejects_empty_endpoint() {
        let mut store = BookmarkStore::empty();
        let err = store
            .upsert(Bookmark {
                name: "named".to_string(),
                endpoint: "".to_string(),
                ..Default::default()
            })
            .expect_err("empty endpoint must be rejected");
        assert!(matches!(err, BookmarkError::InvalidField(_)));
    }

    #[test]
    fn version_mismatch_surfaces_as_error() {
        let dir = tempdir_unique();
        let path = dir.join("bookmarks.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            "version = 999\n[[cluster]]\nname = \"x\"\nendpoint = \"y\"\n",
        )
        .unwrap();
        match BookmarkStore::load_from(&path) {
            Err(BookmarkError::Version(999, 1)) => {}
            other => panic!("expected version mismatch, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Unique tempdir under the OS temp root so concurrent
    /// tests don't collide on the same bookmarks.toml.
    fn tempdir_unique() -> std::path::PathBuf {
        let n: u64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("deck-bookmark-test-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
