//! Persistent producer identity for cross-process dedup.
//!
//! Adapters that rely on backend-side dedup keyed on
//! `(producer_nonce, shard, sequence_start, i)` (today: JetStream's
//! `Nats-Msg-Id` header) need a `producer_nonce` that survives
//! process restart. Without that, a producer that crashes mid-batch
//! and restarts gets a fresh nonce, the post-restart retry writes
//! new msg-ids, and JetStream's dedup window can't recognize them
//! as duplicates of the pre-crash partial — the accepted half ends
//! up persisted twice.
//!
//! `PersistentProducerNonce` provides exactly that: a u64 sampled
//! once and stored on disk. On startup, callers `load_or_create` it
//! against a known path; the second + Nth process loads the same
//! nonce, so retries' msg-ids match the pre-crash incarnation's.
//! Atomic write (`tempfile + rename`) so a crash between the
//! random-sample and the final rename leaves either no file (next
//! load creates fresh) or the complete file — never a partial
//! write.
//!
//! When the bus is configured WITHOUT a path
//! (`EventBusConfig::producer_nonce_path = None`), the existing
//! per-process nonce is used. That keeps the behavior of every
//! pre-fix caller unchanged and is documented as
//! "at-most-once-across-restarts."

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Wire format: `[VERSION:u8 = 1][nonce:u64 LE]` = 9 bytes.
///
/// The version prefix lets a future format change (e.g.
/// HMAC-keyed nonce, extended to 16 bytes for `(epoch, nonce)`)
/// deploy without an out-of-band file migration — loaders just
/// match on `data[0]` and dispatch to the matching parser.
///
/// A raw 8-byte format with no version prefix is **not
/// supported**: a loader against such a file will surface
/// `InvalidData`. Operators with legacy unversioned files can
/// simply delete the existing nonce file — the next start will
/// create a fresh v1, with a one-time loss of cross-restart
/// dedup that's bounded by the JetStream / Redis dedup window.
const NONCE_FILE_LEN_V1: usize = 1 + 8;

/// Version byte for the current wire format.
const NONCE_FORMAT_V1: u8 = 1;

/// Persistent u64 nonce loaded from (or created at) a stable path.
///
/// Callers construct via [`Self::load_or_create`] and read the value
/// via [`Self::nonce`]. The struct itself is cheap to clone — the
/// nonce is a `u64` and the path is a `PathBuf` retained for
/// debugging / logging.
#[derive(Debug, Clone)]
pub struct PersistentProducerNonce {
    nonce: u64,
    #[allow(dead_code)] // retained for diagnostic output
    path: PathBuf,
}

impl PersistentProducerNonce {
    /// Load (or create) the persistent nonce at `path`.
    ///
    /// On first call: samples a fresh u64 from `getrandom`, writes
    /// it to `path` atomically (write to `<path>.tmp`, fsync, rename
    /// to `path`), and returns the value.
    ///
    /// On subsequent calls (post-restart, same path): reads the
    /// existing 8-byte file and returns its little-endian u64.
    ///
    /// Errors:
    /// - `io::ErrorKind::NotFound` if the parent directory doesn't
    ///   exist. We don't auto-create the parent — that's a
    ///   configuration decision the caller should make explicitly.
    /// - `io::ErrorKind::InvalidData` if the file exists but has
    ///   length other than 8 bytes (corrupt or someone else's file
    ///   at this path).
    /// - Other `io::Error` from filesystem operations.
    pub fn load_or_create(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();

        // Fast path: file exists.
        match fs::read(&path) {
            Ok(bytes) => {
                if bytes.len() != NONCE_FILE_LEN_V1 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "producer-nonce file at {} has length {} (expected {} for v1)",
                            path.display(),
                            bytes.len(),
                            NONCE_FILE_LEN_V1,
                        ),
                    ));
                }
                if bytes[0] != NONCE_FORMAT_V1 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "producer-nonce file at {} has unknown version byte 0x{:02x} \
                             (expected 0x{:02x} for v1)",
                            path.display(),
                            bytes[0],
                            NONCE_FORMAT_V1,
                        ),
                    ));
                }
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&bytes[1..]);
                let nonce = u64::from_le_bytes(buf);
                Ok(Self { nonce, path })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // First-load path: sample, write atomically, return.
                Self::create_new(path)
            }
            Err(e) => Err(e),
        }
    }

    fn create_new(path: PathBuf) -> io::Result<Self> {
        // Sample a fresh nonce. We can't depend on `getrandom` here
        // — it's gated behind the `net` feature, but this module is
        // unconditional (the bus uses it whether `net` is on or
        // off, e.g. for JetStream/Redis-only deployments). Mix the
        // same set of entropy sources `event::batch_process_nonce`
        // uses, but DON'T share its `OnceLock` cache — distinct
        // create_new calls in the same process must produce distinct
        // nonces (e.g. two buses configured against different
        // nonce paths should not silently collide). The OnceLock
        // semantic is right for the per-process fallback nonce; it
        // would be wrong here.
        //
        // The mix is identical in spirit to `batch_process_nonce`:
        // wall-clock nanos + monotonic-clock marker + pid +
        // ASLR-derived stack address + thread id, all hashed
        // through xxh3. Adequate for a startup-time nonce — the
        // collision risk we care about is two-processes-on-the-
        // same-machine within a single nanosecond tick, which the
        // pid + stack marker covers.
        //
        // Refuse `0` to keep parity with `batch_process_nonce` —
        // some downstream consumers use 0 as a sentinel.
        use std::hash::{Hash, Hasher};
        use std::time::Instant;

        let wall_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let mono_marker = format!("{:?}", Instant::now());
        let pid = std::process::id() as u64;
        let stack_local: u64 = wall_nanos;
        let stack_marker = (&stack_local as *const u64) as usize;
        let mut tid_hasher = std::collections::hash_map::DefaultHasher::new();
        std::thread::current().id().hash(&mut tid_hasher);
        let tid = tid_hasher.finish();

        // Pull 16 bytes of OS-random entropy via the standard
        // library's `RandomState`, which is itself seeded from
        // platform-secure RNG (getrandom on Linux/macOS, BCrypt on
        // Windows). Each `RandomState::new()` call draws a fresh
        // SipHash key (16 bytes of OS entropy), and finishing a
        // hasher built from that key against a fixed byte yields
        // a 64-bit value derived from those 16 bytes — i.e. 64
        // bits of OS-randomness folded into 64. Two independent
        // samples gives us a full 128 bits of OS-derived entropy
        // mixed into the nonce, on top of the existing
        // pid/tid/wall/stack/mono inputs that are mostly
        // predictable.
        //
        // Pre-fix the mix relied entirely on `(pid, tid, wall,
        // stack_marker as usize, mono)`. On 32-bit targets
        // `stack_marker as u64` is zero-extended from 32 bits,
        // halving its entropy contribution; on 64-bit targets
        // ASLR gives ~30 bits. Combined with predictable pid /
        // wall-time, the total OS-independent entropy was
        // ~50-60 bits — below the 64-bit nonce's stated promise.
        // The OS-random samples below dominate the predictable
        // sources and restore the security margin.
        use std::hash::BuildHasher;
        let os_entropy_a = std::collections::hash_map::RandomState::new().hash_one(0u64);
        let os_entropy_b = std::collections::hash_map::RandomState::new().hash_one(0u64);

        let mut hash_input = [0u8; 64];
        hash_input[..8].copy_from_slice(&wall_nanos.to_le_bytes());
        hash_input[8..16].copy_from_slice(&pid.to_le_bytes());
        hash_input[16..24].copy_from_slice(&(stack_marker as u64).to_le_bytes());
        hash_input[24..32].copy_from_slice(&tid.to_le_bytes());
        // Trim the mono_marker slot to 16 bytes (was 32) and
        // claim the trailing 16 bytes for the two OS-random
        // samples. The mono marker's first 16 bytes still tie-
        // break two same-instant calls within the same process;
        // its longer tail was largely wall-time-correlated text
        // that didn't add meaningful entropy.
        let mono_bytes = mono_marker.as_bytes();
        let n = mono_bytes.len().min(16);
        hash_input[32..32 + n].copy_from_slice(&mono_bytes[..n]);
        hash_input[48..56].copy_from_slice(&os_entropy_a.to_le_bytes());
        hash_input[56..64].copy_from_slice(&os_entropy_b.to_le_bytes());

        let mut nonce = xxhash_rust::xxh3::xxh3_64(&hash_input);
        if nonce == 0 {
            nonce = 1;
        }
        // v1 wire format — `[VERSION:u8 = 1][nonce:u64 LE]`.
        // Versioning lets a future format change (HMAC-keyed nonce,
        // 16-byte epoch+nonce, etc.) deploy without an out-of-band
        // migration — the loader matches on length + version byte.
        let mut buf = [0u8; NONCE_FILE_LEN_V1];
        buf[0] = NONCE_FORMAT_V1;
        buf[1..].copy_from_slice(&nonce.to_le_bytes());

        // Atomic write: create a per-call-unique sibling tempfile,
        // fsync it, rename over the target.
        //
        // Stamp the tempfile name with `pid + tid + nanos` so each
        // caller writes to its own file. A fixed sibling like
        // `<path>.tmp` would let concurrent first-loaders racing on
        // the same path (two threads in one process, OR two
        // daemons misconfigured to point at the same nonce file)
        // interleave their writes at the OS layer and produce a
        // corrupted 8-byte sequence, or one rename would `ENOENT`
        // because the other already moved the tempfile, surfacing
        // as a load_or_create failure. Last rename still wins
        // (intended semantic — the first-loader race is rare and
        // the cap on nonce divergence is "different per call"
        // anyway, since each call samples fresh entropy), but each
        // renamed file is now a complete, valid 8-byte nonce — no
        // interleaved-write corruption.
        let tmp_path = {
            use std::hash::{Hash, Hasher};
            let mut p = path.clone();
            let mut name = p.file_name().map(|n| n.to_os_string()).unwrap_or_default();
            let pid = std::process::id();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let mut tid_hasher = std::collections::hash_map::DefaultHasher::new();
            std::thread::current().id().hash(&mut tid_hasher);
            let tid = tid_hasher.finish();
            // Mix in the freshly-sampled nonce too so the tempfile
            // name is unique even if the wall clock tick is shared
            // and the same thread retries (e.g., after a stale
            // tempfile cleanup). The nonce is the load-bearing
            // entropy source; this just borrows it for naming.
            name.push(format!(".{pid}.{tid:x}.{nanos}.{nonce:x}.tmp"));
            p.set_file_name(name);
            p
        };
        // Pre-fix, the write/sync split was
        //   fs::write(&tmp_path, buf)?;        // (a) write + close
        //   if let Ok(f) = fs::File::open(&tmp_path) {
        //       let _ = f.sync_all();          // (b) sync_all on
        //                                      //     a read-only handle
        //   }
        // Two distinct hazards:
        //   #40 — `let _ = f.sync_all()` swallowed disk-full / I/O
        //         errors; the producer-nonce file was reported as
        //         "saved" while still being only in the kernel
        //         page cache. A power loss between rename and the
        //         OS's own background flush left the nonce file
        //         partial / undurable, breaking cross-restart
        //         dedup on next start.
        //   #68 — On Windows, `fs::File::open(&path)` opens
        //         read-only. `File::sync_all` calls
        //         `FlushFileBuffers`, which returns
        //         `ERROR_ACCESS_DENIED` on a read-only handle —
        //         the entire fsync was a silent no-op on every
        //         Windows install.
        //
        // Post-fix uses a single writable handle for write+sync
        // and propagates both errors. `OpenOptions` with
        // `create_new(true)` matches the per-call-unique tmp_path
        // contract.
        //
        // Pre-emptively remove any zombie tempfile at this exact
        // path. The path hash mixes pid + tid + nanos + freshly-
        // sampled nonce, so a same-named file can only be a
        // crashed prior run of the SAME process+thread that
        // happened to land on the identical nanos+nonce — vanishingly
        // unlikely, but observable in practice if a system clock
        // rewinds across a crash. Without this, `create_new` fails
        // with `AlreadyExists` and there is no retry path; every
        // subsequent save then errors out and the producer nonce
        // never persists. `remove_file().ok()` is safe because no
        // concurrent caller can be holding this exact path (the
        // hash is unique per-call by construction).
        let _ = fs::remove_file(&tmp_path);
        {
            use std::io::Write;
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp_path)?;
            f.write_all(&buf)?;
            f.sync_all()?;
        }
        // `fs::rename` is `MoveFileEx(MOVEFILE_REPLACE_EXISTING)` on
        // Windows / `rename(2)` on Unix — atomic replace on POSIX,
        // best-effort on Windows. Per-call-unique source means the
        // rename can't race against a sibling's rename (each
        // `tmp_path` is its own file).
        fs::rename(&tmp_path, &path)?;

        Ok(Self { nonce, path })
    }

    /// The loaded (or freshly created) nonce.
    #[inline]
    pub fn nonce(&self) -> u64 {
        self.nonce
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(suffix: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        // Combine pid + nanos + suffix so concurrent test runs don't
        // collide on a shared `temp_dir()`.
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!("net-test-nonce-{pid}-{nanos}-{suffix}"));
        p
    }

    #[test]
    fn first_load_creates_a_random_nonzero_nonce() {
        let path = temp_path("first");
        let nonce = PersistentProducerNonce::load_or_create(&path)
            .unwrap()
            .nonce();
        assert_ne!(nonce, 0, "first-load must sample a nonzero nonce");
        // Cleanup.
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn second_load_returns_the_same_nonce() {
        let path = temp_path("second");
        let first = PersistentProducerNonce::load_or_create(&path)
            .unwrap()
            .nonce();
        let second = PersistentProducerNonce::load_or_create(&path)
            .unwrap()
            .nonce();
        assert_eq!(
            first, second,
            "second load against same path must return the same nonce — \
             this is the load-bearing cross-restart property",
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn corrupt_file_surfaces_invalid_data_error() {
        let path = temp_path("corrupt");
        // Write 7 bytes (one short of NONCE_FILE_LEN).
        fs::write(&path, b"shorty!").unwrap();

        let err = PersistentProducerNonce::load_or_create(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("length 7"),
            "error message should pin the actual length; got: {err}",
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn missing_parent_directory_surfaces_not_found_error() {
        let mut path = temp_path("missing-parent");
        path.push("subdir-that-does-not-exist");
        path.push("nonce");

        let err = PersistentProducerNonce::load_or_create(&path).unwrap_err();
        // Either NotFound (Unix-y) or other kinds depending on platform;
        // we just need a clear failure rather than silent success.
        assert!(
            err.kind() == io::ErrorKind::NotFound
                || err.kind() == io::ErrorKind::PermissionDenied
                || err.kind() == io::ErrorKind::Other,
            "expected a clear filesystem error; got {err:?}",
        );
    }

    /// Regression: the startup nonce mix must include OS-derived
    /// entropy (via `RandomState`-keyed hashing) on top of the
    /// pid/tid/wall/stack/mono inputs. Pre-fix the mix relied
    /// entirely on those predictable sources (~50-60 bits of
    /// effective entropy on 64-bit, ~30-40 bits on 32-bit due
    /// to `as usize` zero-extending the stack address). Two
    /// co-located pods restarting from the same checkpoint at
    /// the same wall-clock instant carried tighter collision
    /// margins than the 64-bit nonce promise implied.
    ///
    /// The strict "two co-located pods at the same wall-clock
    /// instant produce different nonces" property is hard to
    /// pin in a unit test (we'd need to fake all the system
    /// inputs identically). Instead this test pins a weaker but
    /// observable property: rapid back-to-back `create_new`
    /// calls in the same process — where wall_nanos is nearly
    /// identical, pid is the same, mono_marker is nearly
    /// identical, and tid is identical for sequential calls —
    /// must still produce distinct nonces. Without OS entropy,
    /// the SipHash randomization of `tid_hasher` is the only
    /// remaining variation, and that's per-process not per-call.
    /// With OS entropy mixed in, every call samples fresh
    /// `RandomState` keys.
    #[test]
    fn back_to_back_nonces_in_same_thread_differ_via_os_entropy() {
        // Hammer 32 nonces from one thread; with OS entropy
        // mixed in, every one should be unique. Pre-fix this
        // would fail because pid/tid/wall_nanos/stack_marker
        // were nearly identical across rapid calls and the
        // hash output collided.
        let mut nonces = std::collections::HashSet::new();
        for i in 0..32 {
            let path = temp_path(&format!("os_entropy_{i}"));
            let nonce = PersistentProducerNonce::load_or_create(&path)
                .unwrap()
                .nonce();
            assert!(
                nonces.insert(nonce),
                "regression: back-to-back nonces must differ — same-thread \
                 same-instant calls have identical predictable inputs, so \
                 OS-random entropy is the only thing that varies. \
                 collision at i={i}, nonce={nonce}",
            );
            let _ = fs::remove_file(&path);
        }
    }

    #[test]
    fn two_distinct_paths_produce_two_distinct_nonces() {
        let a = temp_path("a");
        let b = temp_path("b");
        let n_a = PersistentProducerNonce::load_or_create(&a).unwrap().nonce();
        let n_b = PersistentProducerNonce::load_or_create(&b).unwrap().nonce();
        assert_ne!(
            n_a, n_b,
            "two distinct nonce paths must produce distinct nonces (collision \
             probability is ~2^-63 — if this fires twice, suspect getrandom)",
        );
        let _ = fs::remove_file(&a);
        let _ = fs::remove_file(&b);
    }

    /// Cubic-ai P1: concurrent first-loaders against the SAME path
    /// must not corrupt the on-disk nonce or fail startup. Pre-fix
    /// every caller wrote to `<path>.tmp`, so two threads racing
    /// the first-create could either:
    ///   - interleave writes at the OS layer (resulting in a
    ///     corrupted 8-byte sequence — our `from_le_bytes` would
    ///     decode garbage, or a future length check would reject),
    ///   - or one's `fs::rename` would ENOENT because the other
    ///     already moved the tempfile (surfacing as
    ///     `load_or_create` failure → `EventBus::new` failure).
    ///
    /// The test races N threads on a single path. Each MUST return
    /// successfully; the resulting on-disk file MUST be exactly 8
    /// bytes (no corruption); and a subsequent `load_or_create`
    /// MUST decode a non-zero u64 (cross-thread last-rename-wins
    /// stable state). Any pre-fix interleave or ENOENT would surface
    /// as a panic in one of the threads.
    #[test]
    fn concurrent_first_load_does_not_corrupt_or_fail() {
        use std::sync::Arc;
        use std::thread;

        const N: usize = 16;
        let path = Arc::new(temp_path("concurrent-first-load"));

        let barrier = Arc::new(std::sync::Barrier::new(N));
        let mut handles = Vec::with_capacity(N);
        for _ in 0..N {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                // Pre-fix this could panic on `fs::rename` ENOENT
                // when another thread already moved the shared
                // tempfile. Post-fix every thread owns its own
                // tempfile, so every load_or_create returns Ok.
                PersistentProducerNonce::load_or_create(&*path)
                    .expect("concurrent first-load must succeed")
                    .nonce()
            }));
        }
        let nonces: Vec<u64> = handles
            .into_iter()
            .map(|h| h.join().expect("worker must not panic"))
            .collect();

        // Every thread got a non-zero nonce.
        assert!(
            nonces.iter().all(|&n| n != 0),
            "every concurrent first-loader must observe a non-zero nonce, \
             got: {nonces:?}",
        );

        // CR-28: the on-disk file is exactly NONCE_FILE_LEN_V1 = 9
        // bytes (1 version byte + 8 LE nonce bytes) — no
        // interleaved-write corruption. (Pre-fix two threads could
        // write to the same tempfile and the OS could split their
        // writes mid-byte; the resulting file might be 4 + 4 bytes
        // from different nonces.)
        let on_disk = fs::read(&*path).expect("path must exist after concurrent first-load");
        assert_eq!(
            on_disk.len(),
            NONCE_FILE_LEN_V1,
            "on-disk nonce must be exactly {} bytes (no interleaved-write corruption)",
            NONCE_FILE_LEN_V1,
        );

        // A subsequent load returns the nonce of whichever thread
        // won the last rename — and it MUST equal one of the
        // observed nonces. (If we got a value none of the threads
        // produced, the file is corrupt.)
        let post_load = PersistentProducerNonce::load_or_create(&*path)
            .unwrap()
            .nonce();
        assert!(
            nonces.contains(&post_load),
            "post-load nonce {post_load:#x} must match one of the in-race \
             samples {nonces:?} — anything else implies corruption",
        );

        let _ = fs::remove_file(&*path);
    }

    /// CR-28: legacy 8-byte (pre-versioning) files are NOT
    /// supported. The feature shipped along with CR-28 itself, so
    /// no production deployments of the legacy format exist;
    /// loaders surface `InvalidData` and operators delete the
    /// stale file to recover (next start writes a fresh v1, with
    /// a one-time loss of cross-restart dedup bounded by the
    /// JetStream/Redis dedup window). Pin the rejection so a
    /// future refactor can't silently re-introduce the legacy
    /// path.
    #[test]
    fn cr28_legacy_8_byte_file_is_rejected() {
        let path = temp_path("legacy-8byte");
        // Write 8 raw LE bytes — the pre-CR-28 wire format.
        let stale: u64 = 0xDEAD_BEEF_CAFE_F00D;
        fs::write(&path, stale.to_le_bytes()).unwrap();

        let err = PersistentProducerNonce::load_or_create(&path).unwrap_err();
        assert_eq!(
            err.kind(),
            io::ErrorKind::InvalidData,
            "legacy 8-byte file must surface InvalidData (CR-28 dropped v0 support)"
        );
        assert!(
            err.to_string().contains("length 8"),
            "error message should pin the rejected length; got: {err}"
        );
        let _ = fs::remove_file(&path);
    }

    /// CR-28 v1 round-trip: the new versioned file format is
    /// `[VERSION = 1][8 LE bytes]`. Pin the wire shape so a future
    /// refactor can't silently break it.
    #[test]
    fn cr28_v1_versioned_9_byte_file_round_trip() {
        let path = temp_path("v1-roundtrip");
        let expected: u64 = 0xDEAD_BEEF_CAFE_F00D;
        // Write [VERSION=1][8 LE bytes] by hand — the CR-28 wire
        // format.
        let mut bytes = Vec::with_capacity(9);
        bytes.push(NONCE_FORMAT_V1);
        bytes.extend_from_slice(&expected.to_le_bytes());
        fs::write(&path, &bytes).unwrap();

        let loaded = PersistentProducerNonce::load_or_create(&path)
            .unwrap()
            .nonce();
        assert_eq!(
            loaded, expected,
            "CR-28: v1 file format is [VERSION=1][8 LE bytes]. Pin so a \
             future refactor that flips byte order or drops the version \
             byte doesn't silently produce a different nonce."
        );
        let _ = fs::remove_file(&path);
    }

    /// CR-28: a 9-byte file with an UNKNOWN version byte must
    /// surface InvalidData. This is the forward-compat tripwire —
    /// when v2 is introduced, a v2-aware reader will accept it,
    /// but until then we refuse to silently misinterpret a v2
    /// file as v1.
    #[test]
    fn cr28_unknown_version_byte_surfaces_invalid_data() {
        let path = temp_path("v-unknown");
        // 9 bytes with version byte = 0xFF (reserved future).
        let mut bytes = Vec::with_capacity(9);
        bytes.push(0xFF);
        bytes.extend_from_slice(&0xDEAD_BEEFu64.to_le_bytes());
        fs::write(&path, &bytes).unwrap();

        let err = PersistentProducerNonce::load_or_create(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("0xff") || err.to_string().contains("0xFF"),
            "error message must name the unknown version byte; got: {err}"
        );
        let _ = fs::remove_file(&path);
    }

    /// CR-28: a freshly-created nonce file MUST be the v1 shape
    /// (9 bytes, version byte = 1). Pin so a regression that
    /// reverts to a legacy unversioned write is caught.
    #[test]
    fn cr28_create_new_writes_v1_format() {
        let path = temp_path("v1-fresh");
        let _ = PersistentProducerNonce::load_or_create(&path).unwrap();

        let on_disk = fs::read(&path).unwrap();
        assert_eq!(
            on_disk.len(),
            NONCE_FILE_LEN_V1,
            "CR-28: freshly-created nonce file must be v1 (9 bytes); got {} bytes",
            on_disk.len()
        );
        assert_eq!(
            on_disk[0], NONCE_FORMAT_V1,
            "CR-28: freshly-created nonce file must carry version byte 0x{:02x}; got 0x{:02x}",
            NONCE_FORMAT_V1, on_disk[0]
        );
        let _ = fs::remove_file(&path);
    }
}
