//! A file-backed [`IncarnationPersistence`] (SENSING_OLB_COMPLETION_PLAN B1).
//!
//! A provider must sign under a monotonic, durably persisted boot counter,
//! and the core leaves the storage to the caller. A Rust application can
//! implement the trait over whatever it already persists. A language binding
//! cannot ask its JavaScript or Python caller for a crash-safe trait
//! implementation, so the SDK ships one: a single decimal counter in one
//! file, beside the identity key material.
//!
//! Crash safety follows the trait's contract ("MUST NOT return `Ok` before
//! the value would survive a crash"): the new value goes to a sibling
//! temporary file, which is synced, then renamed over the old one. On Unix the
//! directory is synced afterwards too. A crash therefore leaves the old value
//! or the new one, never a torn file. An unreadable or malformed file is a
//! [`PersistenceFault`], never a fresh start, because defaulting to zero would
//! manufacture exactly the rollback incarnations exist to prevent.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::{IncarnationPersistence, PersistenceFault};

/// A boot counter kept in one file. See the module docs for the crash-safety
/// argument.
#[derive(Debug, Clone)]
pub struct FileIncarnation {
    path: PathBuf,
}

impl FileIncarnation {
    /// A counter stored at `path`. The file need not exist yet: absence is a
    /// fresh install, and the first boot is incarnation 1. Its directory must
    /// exist.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Where the counter lives.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn temporary(&self) -> PathBuf {
        let mut name = self
            .path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();
        name.push(".tmp");
        self.path.with_file_name(name)
    }
}

impl IncarnationPersistence for FileIncarnation {
    fn load(&mut self) -> Result<Option<u64>, PersistenceFault> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => text
                .trim()
                .parse::<u64>()
                .map(Some)
                .map_err(|_| PersistenceFault),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(PersistenceFault),
        }
    }

    fn store(&mut self, value: u64) -> Result<(), PersistenceFault> {
        let temporary = self.temporary();
        let write = || -> std::io::Result<()> {
            let mut file = std::fs::File::create(&temporary)?;
            writeln!(file, "{value}")?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, &self.path)?;
            #[cfg(unix)]
            if let Some(dir) = self.path.parent() {
                std::fs::File::open(dir)?.sync_all()?;
            }
            Ok(())
        };
        write().map_err(|_| {
            let _ = std::fs::remove_file(&temporary);
            PersistenceFault
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensing::next_incarnation;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "net-sdk-incarnation-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("incarnation")
    }

    /// Fresh install starts at 1, and each boot is strictly greater than the
    /// last, across separate handles on the same file (a restart).
    #[test]
    fn boots_are_monotonic_across_restarts() {
        let path = scratch("monotonic");
        assert_eq!(
            next_incarnation(&mut FileIncarnation::new(&path))
                .unwrap()
                .get(),
            1
        );
        assert_eq!(
            next_incarnation(&mut FileIncarnation::new(&path))
                .unwrap()
                .get(),
            2
        );
        assert_eq!(
            next_incarnation(&mut FileIncarnation::new(&path))
                .unwrap()
                .get(),
            3
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "3");
        assert!(
            !FileIncarnation::new(&path).temporary().exists(),
            "no temp file left behind"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A corrupt counter is a fault, never a fresh start. RED against a load
    /// that maps a parse failure to `None` (that would reset to 1 and reuse
    /// incarnations).
    #[test]
    fn a_corrupt_counter_fails_closed() {
        let path = scratch("corrupt");
        std::fs::write(&path, "not a number").unwrap();
        assert!(next_incarnation(&mut FileIncarnation::new(&path)).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "not a number",
            "nothing overwritten"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A store into a directory that does not exist fails rather than
    /// pretending the value is durable.
    #[test]
    fn an_unwritable_location_is_a_fault() {
        let path = scratch("missing")
            .parent()
            .unwrap()
            .join("absent-dir")
            .join("incarnation");
        assert!(next_incarnation(&mut FileIncarnation::new(&path)).is_err());
    }
}
