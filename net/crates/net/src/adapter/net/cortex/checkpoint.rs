//! Local typed-adapter restore checkpoints. The external snapshot encoding
//! stays unchanged; this envelope binds its replay position to one origin.
//! Offline restore only: no cross-process writer coordination is supplied.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use super::super::channel::ChannelName;
use super::super::redex::{Redex, RedexError, RedexFileConfig};
use super::CortexAdapterError;

const MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

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
    let temp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    // create_new prevents a stale temp or symlink from being overwritten.
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(RedexError::io)?;
    let written = file.write_all(&bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = written.and_then(|()| std::fs::rename(&temp, &path)) {
        let _ = std::fs::remove_file(&temp);
        return Err(RedexError::io(e).into());
    }
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(RedexError::io)?;
    }
    Ok(last_seq)
}
