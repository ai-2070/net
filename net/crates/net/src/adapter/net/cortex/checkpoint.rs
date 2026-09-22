//! Local typed-adapter restore checkpoints. The external snapshot encoding
//! stays unchanged; this envelope binds its replay position to one origin.
//! Offline restore only: no cross-process writer coordination is supplied.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::super::behavior::enrollment_storage::StorageError;
use super::super::behavior::org_revocation::{write_atomic_phased, WritePhase};
use super::super::channel::ChannelName;
use super::super::redex::{Redex, RedexError, RedexFileConfig};
use super::CortexAdapterError;

const MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Test-only, one-shot: force the next [`publish`] to report a
/// POST-rename durability failure AFTER the real phased publish has
/// landed — the checkpoint is published, only its durability proof is
/// missing. This is the seam shape `StoreCore::force_post_rename`
/// gives the revocation store, and the only way to exercise the
/// post-rename phase on this host: a parent-directory fsync failure
/// cannot be forced from a test, and `MOVEFILE_WRITE_THROUGH` is
/// indistinguishable from `std::fs::rename` in-process. Process-wide
/// because [`store`] is a free function; the tests arm it immediately
/// before their single forced call and [`publish`] consumes it
/// one-shot.
#[cfg(test)]
static FORCE_POST_RENAME: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[derive(Serialize, Deserialize)]
pub(super) struct Checkpoint {
    version: u8,
    origin: u64,
    pub state: Vec<u8>,
    pub last_seq: Option<u64>,
}

fn path(
    redex: &Redex,
    name: &ChannelName,
    config: &RedexFileConfig,
) -> Result<Option<PathBuf>, CortexAdapterError> {
    if !config.persistent {
        return Ok(None);
    }
    #[cfg(feature = "redex-disk")]
    {
        Ok(Some(redex.cortex_checkpoint_path(name, config.clone())?))
    }
    #[cfg(not(feature = "redex-disk"))]
    {
        let _ = (redex, name);
        Ok(None)
    }
}

pub(super) fn load(
    redex: &Redex,
    name: &ChannelName,
    config: &RedexFileConfig,
    origin: u64,
) -> Result<Option<Checkpoint>, CortexAdapterError> {
    let Some(path) = path(redex, name, config)? else {
        return Ok(None);
    };
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(RedexError::io(e).into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(RedexError::io)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(RedexError::Decode("CortEX checkpoint exceeds size ceiling".into()).into());
    }
    let checkpoint: Checkpoint = postcard::from_bytes(&bytes)
        .map_err(|e| RedexError::Decode(format!("CortEX checkpoint: {e}")))?;
    if checkpoint.version != 1 || checkpoint.origin != origin {
        return Err(
            RedexError::Decode("CortEX checkpoint version or origin mismatch".into()).into(),
        );
    }
    Ok(Some(checkpoint))
}

/// Persist the incoming state with its position in the DESTINATION log.
/// A fresh/shorter log cannot skip to the source's higher sequence: its
/// next append starts at its own tail. Existing post-snapshot events remain
/// available for replay, preserving the established merge behavior.
pub(super) fn store(
    redex: &Redex,
    name: &ChannelName,
    config: &RedexFileConfig,
    origin: u64,
    state: &[u8],
    last_seq: Option<u64>,
) -> Result<Option<u64>, CortexAdapterError> {
    let Some(path) = path(redex, name, config)? else {
        return Ok(last_seq);
    };
    // Do not silently overwrite a corrupt checkpoint or change its origin.
    let _ = load(redex, name, config, origin)?;
    let tail = redex
        .open_file(name, config.clone())?
        .next_seq()
        .checked_sub(1);
    let last_seq = last_seq.zip(tail).map(|(source, dest)| source.min(dest));
    let bytes = postcard::to_allocvec(&Checkpoint {
        version: 1,
        origin,
        state: state.to_vec(),
        last_seq,
    })
    .map_err(|e| RedexError::Encode(format!("CortEX checkpoint: {e}")))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(RedexError::Encode("CortEX checkpoint exceeds size ceiling".into()).into());
    }
    // Publish through the crate's phased durable writer (review
    // finding 2). The old hand-rolled temp + `std::fs::rename` +
    // `#[cfg(unix)]`-only parent `sync_all` left NO durability step on
    // Windows (`std::fs::rename` is `MoveFileExW` with
    // `MOVEFILE_REPLACE_EXISTING` and no write-through): a power loss
    // after `store` returned `Ok` could resurrect a stale earlier
    // checkpoint or drop the restored entities the log alone cannot
    // rebuild. `write_atomic_phased` supplies the fresh `create_new`
    // temp, the `MOVEFILE_WRITE_THROUGH` publish
    // (`rename_write_through`), the parent-directory fsync on BOTH
    // platforms, and the pre/post-rename phase split.
    publish(&path, &bytes).map_err(wall_uncertain)?;
    Ok(last_seq)
}

/// Durable publish of the checkpoint envelope, with the fail-closed
/// phase modeling `EnrollmentStorage::replace_using` applies (review
/// finding 2): a PRE-rename failure is an ordinary IO error — nothing
/// was published — while a POST-rename failure is
/// [`StorageError::Uncertain`]: the rename LANDED (the checkpoint is
/// published) but its durability is unproven, and the caller must fail
/// closed rather than read it as a retryable IO error on an
/// unpublished file.
fn publish(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    #[cfg(test)]
    let forced = FORCE_POST_RENAME.swap(false, std::sync::atomic::Ordering::AcqRel);
    let outcome = write_atomic_phased(path, bytes);
    #[cfg(test)]
    let outcome = match outcome {
        // The phased publish above has already replaced the file;
        // report only the missing durability proof — exactly what a
        // post-rename parent-fsync failure looks like on the return
        // path ("durability-uncertain … on a published file").
        Ok(()) if forced => Err(WritePhase::PostRename(
            "forced post-rename failure (test seam)".to_string(),
        )),
        other => other,
    };
    match outcome {
        Ok(()) => Ok(()),
        Err(WritePhase::PreRename(reason)) => Err(StorageError::Io(std::io::Error::other(reason))),
        Err(WritePhase::PostRename(_)) => Err(StorageError::Uncertain),
    }
}

/// Wall [`publish`]'s phased outcome into [`CortexAdapterError`] for
/// the checkpoint call sites.
///
/// The durability-uncertain outcome deserves its own
/// `CortexAdapterError::DurabilityUncertain { path, reason }`
/// variant (mirroring `OrgRevocationError::DurabilityUncertain`);
/// until `cortex/error.rs` grows one, `RedexError::Encode` — this
/// module's store-side error class, which `store` already reports its
/// encode and size-ceiling failures through — carries the fail-closed
/// text. Never `RedexError::io`: that reads as a retryable IO error on
/// an unpublished file while the checkpoint is already published.
fn wall_uncertain(e: StorageError) -> CortexAdapterError {
    match e {
        StorageError::Io(io) => RedexError::io(io).into(),
        StorageError::Uncertain => RedexError::Encode(
            "CortEX checkpoint publish durability uncertain: the rename landed but the \
             durability step failed; the published checkpoint's durability is unproven — \
             close and reopen before use"
                .to_string(),
        )
        .into(),
        other => RedexError::Encode(format!("CortEX checkpoint publish failed: {other}")).into(),
    }
}

#[cfg(all(test, feature = "redex-disk"))]
mod tests {
    use super::*;

    const ORIGIN: u64 = 0xC0FF_EE00_D00D_F00D;

    fn fixture(channel: &str) -> (tempfile::TempDir, Redex, ChannelName, RedexFileConfig) {
        let dir = tempfile::tempdir().unwrap();
        let redex = Redex::new().with_persistent_dir(dir.path());
        let name = ChannelName::new(channel).unwrap();
        let config = RedexFileConfig::new().with_persistent(true);
        (dir, redex, name, config)
    }

    /// Review finding 2 witness. A POST-rename publish failure — the
    /// rename LANDED, only the durability proof is missing — must
    /// surface as `StorageError::Uncertain` (the
    /// `EnrollmentStorage::replace_using` fail-closed modeling) and,
    /// one wall up, as a durability-uncertain error on a PUBLISHED
    /// checkpoint — never a plain IO error that reads as "nothing
    /// happened; retry".
    ///
    /// Inverse: reverting [`publish`] to the old plain
    /// `std::fs::rename` sequence with flattened IO errors removes the
    /// post-rename phase (and its seam), the forced call returns `Ok`,
    /// and this witness goes red at the `StorageError::Uncertain`
    /// assertion. What this witness cannot see is the
    /// `MOVEFILE_WRITE_THROUGH` flag itself: `rename` and
    /// `rename_write_through` are identical in-process — the
    /// `MOVEFILE_WRITE_THROUGH` distinction is CI/Windows-kernel-level
    /// and was not executed here.
    #[test]
    fn post_rename_publish_failure_is_durability_uncertain_on_a_published_checkpoint() {
        let (_dir, redex, name, config) = fixture("cortex/ckpt-post-rename");
        let path = path(&redex, &name, &config).unwrap().unwrap();

        // Positive control: the unforced publish path succeeds and
        // lands, so a red row below cannot be a dead publish path.
        // `publish` takes raw bytes (the envelope is `store`'s job),
        // so observe the published file directly.
        publish(&path, b"first").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");

        // Forced post-rename failure at the production publish site.
        FORCE_POST_RENAME.store(true, std::sync::atomic::Ordering::Release);
        let result = publish(&path, b"second");
        assert!(
            matches!(result, Err(StorageError::Uncertain)),
            "a post-rename publish failure must surface as StorageError::Uncertain \
             (durability unproven on a published file), got: {result:?}"
        );
        // "on a published file": the rename really landed.
        assert_eq!(std::fs::read(&path).unwrap(), b"second");

        // End-to-end through `store`: the same uncertainty must be
        // walled as durability-uncertain, never plain IO. Reset the
        // fixture first: the raw `publish` bytes above are not an
        // envelope, and `store` refuses to overwrite a corrupt
        // checkpoint by design (its pre-check errors on them). Then
        // an unarmed control run establishes a valid envelope.
        std::fs::remove_file(&path).unwrap();
        store(&redex, &name, &config, ORIGIN, b"third", None).unwrap();
        FORCE_POST_RENAME.store(true, std::sync::atomic::Ordering::Release);
        let result = store(&redex, &name, &config, ORIGIN, b"fourth", None);
        let msg = match &result {
            Err(CortexAdapterError::Redex(RedexError::Encode(m))) => m.clone(),
            other => panic!(
                "store must surface a post-rename publish failure as a durability-uncertain \
                 error (not plain IO on a published checkpoint), got: {other:?}"
            ),
        };
        assert!(
            msg.contains("durability uncertain"),
            "the walled error must carry the fail-closed durability-uncertain text, got: {msg}"
        );
        assert_eq!(
            load(&redex, &name, &config, ORIGIN).unwrap().unwrap().state,
            b"fourth"
        );
    }

    /// Phase-reporting companion (real filesystem fault, no seam):
    /// with the destination occupied by a directory no rename can
    /// land — the PRE-rename phase — and the outcome must be plain IO
    /// with nothing published. Together with the post-rename witness
    /// this pins the phase split the plain `std::fs::rename` publish
    /// flattened into one undifferentiated IO error.
    #[test]
    fn pre_rename_publish_failure_is_plain_io_and_publishes_nothing() {
        let (_dir, redex, name, config) = fixture("cortex/ckpt-pre-rename");
        let path = path(&redex, &name, &config).unwrap().unwrap();

        // Real pre-rename fault: the destination is a directory, so
        // no rename can replace it.
        std::fs::create_dir(&path).unwrap();
        let result = publish(&path, b"state");
        assert!(
            matches!(result, Err(StorageError::Io(_))),
            "a pre-rename failure must surface as plain IO (nothing was published), got: {result:?}"
        );
        // Nothing published: the destination is still the planted
        // directory.
        assert!(path.is_dir());
    }
}
