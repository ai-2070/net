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
//! directory is synced afterwards too. Elsewhere there is no portable
//! directory sync, so the renamed file itself is flushed (on NTFS the rename
//! is journaled metadata) and a directory flush is attempted best-effort. A
//! crash therefore leaves the old value or the new one, never a torn file. An
//! unreadable or malformed file is a [`PersistenceFault`], never a fresh
//! start, because defaulting to zero would manufacture exactly the rollback
//! incarnations exist to prevent.
//!
//! Concurrency: `load` takes an exclusive lock on a sibling `<name>.lock`
//! file and `store` releases it, so two handles on one counter (two
//! processes or two threads) cannot both read N and both publish N+1. The
//! lock is advisory; every SDK and binding writer goes through this type.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::{IncarnationPersistence, PersistenceFault};

/// A boot counter kept in one file. See the module docs for the crash-safety
/// and concurrency arguments.
#[derive(Debug)]
pub struct FileIncarnation {
    path: PathBuf,
    /// The exclusive lock held from `load` until `store` (or drop).
    held: Option<std::fs::File>,
}

impl Clone for FileIncarnation {
    /// A clone names the same counter but never shares a held lock.
    fn clone(&self) -> Self {
        Self::new(self.path.clone())
    }
}

impl FileIncarnation {
    /// A counter stored at `path`. The file need not exist yet: absence is a
    /// fresh install, and the first boot is incarnation 1. Its directory must
    /// exist.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            held: None,
        }
    }

    /// Where the counter lives.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn sibling(&self, suffix: &str) -> PathBuf {
        let mut name = self
            .path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();
        name.push(suffix);
        self.path.with_file_name(name)
    }

    fn temporary(&self) -> PathBuf {
        self.sibling(".tmp")
    }

    /// The counter's directory. A bare file name has an EMPTY parent, which
    /// cannot be opened; it means the current directory.
    fn directory(&self) -> &Path {
        self.path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
    }

    /// Take the exclusive transaction lock, once per load/store pair.
    fn acquire(&mut self) -> std::io::Result<()> {
        if self.held.is_none() {
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(self.sibling(".lock"))?;
            lock.lock()?;
            self.held = Some(lock);
        }
        Ok(())
    }
}

impl IncarnationPersistence for FileIncarnation {
    fn load(&mut self) -> Result<Option<u64>, PersistenceFault> {
        self.acquire().map_err(|_| PersistenceFault)?;
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
        self.acquire().map_err(|_| PersistenceFault)?;
        let temporary = self.temporary();
        let write = || -> std::io::Result<()> {
            let mut file = std::fs::File::create(&temporary)?;
            writeln!(file, "{value}")?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, &self.path)?;
            #[cfg(unix)]
            std::fs::File::open(self.directory())?.sync_all()?;
            #[cfg(not(unix))]
            {
                // No portable directory sync here: flush the renamed file,
                // then try the directory, whose failure is not fatal.
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&self.path)?
                    .sync_all()?;
                #[cfg(windows)]
                {
                    use std::os::windows::fs::OpenOptionsExt;
                    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
                    if let Ok(dir) = std::fs::OpenOptions::new()
                        .write(true)
                        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                        .open(self.directory())
                    {
                        let _ = dir.sync_all();
                    }
                }
            }
            Ok(())
        };
        let outcome = write().map_err(|_| {
            let _ = std::fs::remove_file(&temporary);
            PersistenceFault
        });
        // The transaction is over, published or not.
        self.held = None;
        outcome
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

    /// Concurrent handles on ONE counter never hand out the same incarnation:
    /// the load-to-store transaction is serialized by the lock. RED against a
    /// `load` that takes no lock (two handles read N and both publish N+1).
    #[test]
    fn concurrent_handles_never_share_an_incarnation() {
        let path = scratch("concurrent");
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || {
                    (0..10)
                        .map(|_| {
                            next_incarnation(&mut FileIncarnation::new(&path))
                                .expect("boot")
                                .get()
                        })
                        .collect::<Vec<u64>>()
                })
            })
            .collect();
        let mut all: Vec<u64> = threads
            .into_iter()
            .flat_map(|t| t.join().expect("thread"))
            .collect();
        all.sort_unstable();
        assert_eq!(all, (1..=80).collect::<Vec<u64>>(), "every boot distinct");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A bare file name lives in the current directory: its directory is `.`,
    /// never the empty path (which cannot be opened, so the Unix directory
    /// sync used to fail after publishing).
    #[test]
    fn a_bare_file_name_resolves_its_directory_to_the_current_one() {
        assert_eq!(
            FileIncarnation::new("incarnation").directory(),
            Path::new(".")
        );
        assert_eq!(
            FileIncarnation::new("state/incarnation").directory(),
            Path::new("state")
        );
    }
}
