//! `net-mesh up` / `net-mesh down` / `net-mesh node status` — one long-lived
//! production node per profile, owned by an explicit foreground process.
//!
//! # State directory
//!
//! Everything lives under one per-profile state directory (`--state-dir`,
//! default `<platform data dir>/net-mesh/nodes/<profile>`), in a protected
//! `node/` subdirectory created with the core's owner-only directory rules
//! (0700 / owner-only inheritable DACL):
//!
//! ```text
//! node/enrollment.snapshot   generated node identity seed + generated PSK
//! node/enrollment.lock       store lock, held by the running `up`
//! node/up.lock               lifetime lock, held by the running `up`; probed
//!                            by `down` / `node status` to decide liveness
//! node/control.json          per-run control endpoint: port, incarnation and
//!                            a fresh secret (removed on clean shutdown)
//! ```
//!
//! Liveness comes from the lifetime lock, never from a PID or a file's
//! presence: a control file without a held lock is reported as stale metadata.
//!
//! # Local control endpoint
//!
//! Loopback TCP on a random port. The per-run 32-byte secret is only in the
//! owner-only control file. Both sides prove knowledge of it with keyed BLAKE3
//! over fresh nonces from each side; the secret never crosses the socket, and a
//! process squatting the port cannot pass as the node. Each subsequent message
//! is encrypted with a keyed-BLAKE3 keystream and authenticated with a keyed
//! MAC over the ciphertext (direction and sequence bound), both under keys
//! derived from the secret and both nonces: `invite create` returns a bearer
//! join token over this channel.
//!
//! # Secrets
//!
//! The PSK comes from `--psk-from file:<path>` (32 raw bytes or 64 hex through
//! the secret-file gate), `--psk-from stdin` (piped only; a terminal is refused
//! because nothing here can disable echo), or — when omitted — a CSPRNG value
//! generated on first start and persisted before bind, then reused. It is never
//! accepted on argv or printed; output shows only its public trust-domain id.
//! `kms:` sources are not implemented in this build and are refused.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Args;
use net::adapter::net::behavior::enrollment_storage::{EnrollmentStorage, StorageError};
use net_sdk::bootstrap_credential::Psk;
use net_sdk::identity::Identity;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Semaphore};

use crate::error::{connection_failure, generic, invalid_args, timeout, CliError};
use crate::prelude::{emit_stream_row, emit_value, OutputFormat};
use crate::secret::{zeroize_slice, ScrubbedBytes};

pub(crate) const NODE_SUBDIR: &str = "node";
const LOCK_FILE: &str = "up.lock";
const CONTROL_FILE: &str = "control.json";
const SECRETS_MAGIC: [u8; 4] = *b"NMUP";
const SECRETS_VERSION: u16 = 1;
const SECRETS_CHECKSUM: &str = "net-mesh up node secrets v1";
const CONTROL_MAGIC: [u8; 4] = *b"NMCT";
const MAX_CONTROL_FRAME: usize = 16 * 1024;
const CONTROL_SESSION_TIMEOUT: Duration = Duration::from_secs(5);
const CONTROL_MAX_SESSIONS: usize = 8;
const MESH_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// `net-mesh up` arguments.
#[derive(Args, Debug)]
pub struct UpArgs {
    /// Node state directory: identity, generated PSK, lifetime lock and control
    /// endpoint. Defaults to `<platform data dir>/net-mesh/nodes/<profile>`.
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,

    /// Mesh bind address (IP:port). Defaults to the profile `bind`, else
    /// `0.0.0.0:0`.
    #[arg(long, value_name = "ADDR")]
    pub bind: Option<String>,

    /// PSK source: `file:<path>` (32 raw bytes or 64 hex characters) or
    /// `stdin` (piped, not a terminal). Omit to generate one on first start
    /// and reuse it. Never a literal PSK.
    #[arg(long, value_name = "SOURCE")]
    pub psk_from: Option<String>,

    /// Node identity file. Defaults to the profile `identity`, else an identity
    /// generated on first start and kept in the state directory.
    #[arg(long, value_name = "PATH")]
    pub identity: Option<PathBuf>,

    /// Make this node the enrollment owner: serve join-token redemption and
    /// accept `net-mesh invite` operations. Requires `--public-addr`,
    /// `--issuer-identity`, a fixed `--bind` port and an initialized ledger
    /// (`net-mesh enrollment init`); refuses before binding without them.
    #[arg(long)]
    pub enroll: bool,

    /// Address joiners reach this node at (`host:port`), signed into every
    /// join token. The same port number must reach the node over TCP
    /// (enrollment) and UDP (mesh).
    #[arg(long, value_name = "HOST:PORT", requires = "enroll")]
    pub public_addr: Option<String>,

    /// Issuer identity file that signs invitations and membership receipts.
    #[arg(long, value_name = "PATH", requires = "enroll")]
    pub issuer_identity: Option<PathBuf>,

    /// Enrollment ledger directory. Defaults to `<state-dir>/ledger`.
    #[arg(long, value_name = "DIR", requires = "enroll")]
    pub ledger: Option<PathBuf>,

    /// Trust-domain label shown to joiners (`[A-Za-z0-9._-]`, at most 64).
    /// Defaults to the profile name.
    #[arg(long, value_name = "NAME", requires = "enroll")]
    pub domain_name: Option<String>,
}

/// `net-mesh down` arguments.
#[derive(Args, Debug)]
pub struct DownArgs {
    /// Node state directory (as given to `net-mesh up`).
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,

    /// How long to wait for the node to release its lifetime lock after it
    /// accepts the shutdown request.
    #[arg(long, value_name = "DURATION", default_value = "15s", value_parser = crate::humantime::parse_duration)]
    pub wait: Duration,
}

/// `net-mesh node status` arguments.
#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Node state directory (as given to `net-mesh up`).
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,
}

pub(crate) fn state_dir(explicit: Option<PathBuf>, profile: &str) -> Result<PathBuf, CliError> {
    if let Some(dir) = explicit {
        return Ok(dir);
    }
    let usable = !profile.is_empty()
        && !profile.starts_with('.')
        && profile
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if !usable {
        return Err(invalid_args(
            "profile name is not usable as a directory name; pass --state-dir",
        ));
    }
    dirs::data_local_dir()
        .map(|d| d.join("net-mesh").join("nodes").join(profile))
        .ok_or_else(|| invalid_args("no platform data directory is available; pass --state-dir"))
}

pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn random<const N: usize>() -> Result<[u8; N], CliError> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|_| generic("operating-system CSPRNG unavailable"))?;
    Ok(b)
}

// ---- node secrets (generated identity seed + generated PSK) ----------------

struct NodeSecrets {
    seed: [u8; 32],
    psk: Option<[u8; 32]>,
}

impl Drop for NodeSecrets {
    fn drop(&mut self) {
        zeroize_slice(&mut self.seed);
        if let Some(psk) = self.psk.as_mut() {
            zeroize_slice(psk);
        }
    }
}

impl NodeSecrets {
    fn encode(&self) -> ScrubbedBytes {
        let mut out = Vec::with_capacity(4 + 2 + 32 + 1 + 32 + 32);
        out.extend_from_slice(&SECRETS_MAGIC);
        out.extend_from_slice(&SECRETS_VERSION.to_le_bytes());
        out.extend_from_slice(&self.seed);
        match &self.psk {
            Some(psk) => {
                out.push(1);
                out.extend_from_slice(psk);
            }
            None => out.push(0),
        }
        let sum = blake3::derive_key(SECRETS_CHECKSUM, &out);
        out.extend_from_slice(&sum);
        ScrubbedBytes::new(out)
    }

    fn decode(bytes: &[u8]) -> Result<Self, CliError> {
        let corrupt = || {
            generic("node state is corrupt or from an unsupported version; refusing to replace it")
        };
        let body_len = bytes.len().checked_sub(32).ok_or_else(corrupt)?;
        let (body, sum) = bytes.split_at(body_len);
        if blake3::derive_key(SECRETS_CHECKSUM, body) != sum
            || body.len() < 4 + 2 + 32 + 1
            || body[..4] != SECRETS_MAGIC
            || body[4..6] != SECRETS_VERSION.to_le_bytes()
        {
            return Err(corrupt());
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&body[6..38]);
        let psk = match (body[38], body.len()) {
            (0, 39) => None,
            (1, 71) => {
                let mut psk = [0u8; 32];
                psk.copy_from_slice(&body[39..71]);
                Some(psk)
            }
            _ => {
                zeroize_slice(&mut seed);
                return Err(corrupt());
            }
        };
        Ok(Self { seed, psk })
    }
}

fn storage_error(dir: &Path, e: StorageError) -> CliError {
    match e {
        StorageError::Busy => already_running(dir),
        StorageError::Security => invalid_args(format!(
            "node state directory {} failed ownership/permission validation",
            dir.display()
        )),
        other => generic(format!("node state {}: {other}", dir.display())),
    }
}

fn already_running(dir: &Path) -> CliError {
    let who = read_control_file(dir)
        .map(|c| format!(" (incarnation {}, pid {})", c.incarnation, c.pid))
        .unwrap_or_default();
    generic(format!(
        "a node is already running for {}{who}; see `net-mesh node status`",
        dir.display()
    ))
}

/// Open or initialize the node store, taking its exclusive lock. A generated
/// PSK is committed in the same initial snapshot, before any bind.
fn acquire(dir: &Path, generate_psk: bool) -> Result<(EnrollmentStorage, NodeSecrets), CliError> {
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let secrets = NodeSecrets {
                seed: random()?,
                psk: if generate_psk { Some(random()?) } else { None },
            };
            let storage = EnrollmentStorage::create(dir, secrets.encode().as_slice()).map_err(
                |e| match e {
                    StorageError::AlreadyExists => generic(format!(
                        "another `net-mesh up` is initializing {}",
                        dir.display()
                    )),
                    other => storage_error(dir, other),
                },
            )?;
            Ok((storage, secrets))
        }
        Err(e) => Err(generic(format!("node state {}: {e}", dir.display()))),
        Ok(_) => {
            let storage = EnrollmentStorage::open(dir).map_err(|e| storage_error(dir, e))?;
            let bytes = ScrubbedBytes::new(storage.read().map_err(|e| storage_error(dir, e))?);
            let secrets = NodeSecrets::decode(bytes.as_slice())?;
            Ok((storage, secrets))
        }
    }
}

/// Take the lifetime lock that `down` / `node status` probe. The store lock is
/// already held, so contention here is only a momentary status probe.
fn hold_lifetime_lock(dir: &Path) -> Result<File, CliError> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts
        .open(dir.join(LOCK_FILE))
        .map_err(|e| generic(format!("lifetime lock {}: {e}", dir.display())))?;
    for _ in 0..200 {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(10)),
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(generic(format!("lifetime lock {}: {e}", dir.display())))
            }
        }
    }
    Err(already_running(dir))
}

#[derive(Debug, PartialEq, Eq)]
enum Liveness {
    /// No state directory or lock file: never started here.
    Absent,
    /// Lock file present and free: no live owner.
    Free,
    /// Lock held: a live owner (starting, ready or draining).
    Held,
}

/// Probe the lifetime lock without creating anything.
fn probe(dir: &Path) -> Result<Liveness, CliError> {
    let file = match File::open(dir.join(LOCK_FILE)) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Liveness::Absent),
        Err(e) => return Err(generic(format!("lifetime lock {}: {e}", dir.display()))),
    };
    match file.try_lock() {
        // Dropping `file` releases the probe lock immediately.
        Ok(()) => Ok(Liveness::Free),
        Err(std::fs::TryLockError::WouldBlock) => Ok(Liveness::Held),
        Err(std::fs::TryLockError::Error(e)) => {
            Err(generic(format!("lifetime lock {}: {e}", dir.display())))
        }
    }
}

// ---- PSK sources -----------------------------------------------------------

enum PskSource {
    Generated,
    File(PathBuf),
    Stdin,
}

impl PskSource {
    fn parse(raw: Option<&str>) -> Result<Self, CliError> {
        match raw {
            None => Ok(Self::Generated),
            Some("stdin") => Ok(Self::Stdin),
            Some(s) if s.starts_with("file:") && s.len() > 5 => {
                Ok(Self::File(PathBuf::from(&s[5..])))
            }
            Some(s) if s.starts_with("kms:") => Err(invalid_args(
                "--psk-from kms: sources are not available in this build",
            )),
            Some(_) => Err(invalid_args(
                "--psk-from must be `file:<path>` or `stdin`; a literal PSK is never accepted",
            )),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Generated => "generated",
            Self::File(_) => "file",
            Self::Stdin => "stdin",
        }
    }
}

/// Parse 32 raw bytes or 64 hex characters (surrounding whitespace allowed for
/// hex). Refuses an all-zero PSK. Never echoes the input.
fn parse_psk(bytes: &[u8]) -> Result<[u8; 32], CliError> {
    let mut psk = [0u8; 32];
    if bytes.len() == 32 {
        psk.copy_from_slice(bytes);
    } else {
        let text = std::str::from_utf8(bytes).ok().map(str::trim);
        let decoded = text
            .filter(|t| t.len() == 64)
            .and_then(|t| hex::decode(t).ok());
        let Some(decoded) = decoded.map(ScrubbedBytes::new) else {
            return Err(invalid_args(
                "PSK source must contain exactly 32 raw bytes or 64 hex characters",
            ));
        };
        psk.copy_from_slice(decoded.as_slice());
    }
    if psk == [0u8; 32] {
        return Err(invalid_args("an all-zero PSK is refused"));
    }
    Ok(psk)
}

async fn read_psk_file(path: &Path) -> Result<[u8; 32], CliError> {
    let owned = path.to_path_buf();
    let read = tokio::task::spawn_blocking(move || -> Result<ScrubbedBytes, String> {
        use std::io::Read as _;
        let file = ::net::adapter::net::secret_file::open_secret_file(&owned, false)
            .map_err(|e| format!("{e}"))?;
        let mut buf = Vec::with_capacity(130);
        file.take(130)
            .read_to_end(&mut buf)
            .map_err(|e| format!("read failed: {e}"))?;
        Ok(ScrubbedBytes::new(buf))
    })
    .await
    .map_err(|e| generic(format!("PSK file read task failed: {e}")))?
    .map_err(|e| invalid_args(format!("--psk-from file:{}: {e}", path.display())))?;
    parse_psk(read.as_slice())
}

async fn read_psk_stdin() -> Result<[u8; 32], CliError> {
    use std::io::IsTerminal as _;
    if std::io::stdin().is_terminal() {
        return Err(invalid_args(
            "--psk-from stdin needs piped input; a terminal is refused because echo cannot be disabled",
        ));
    }
    let mut buf = Vec::with_capacity(130);
    tokio::io::stdin()
        .take(130)
        .read_to_end(&mut buf)
        .await
        .map_err(|e| generic(format!("--psk-from stdin: {e}")))?;
    let buf = ScrubbedBytes::new(buf);
    parse_psk(buf.as_slice())
}

// ---- control file ------------------------------------------------------------

#[derive(Serialize, Deserialize)]
pub(crate) struct ControlFile {
    version: u32,
    pub(crate) incarnation: String,
    port: u16,
    secret: String,
    pid: u32,
    started_at: u64,
}

fn read_control_file(dir: &Path) -> Option<ControlFile> {
    let text = ::net::adapter::net::secret_file::read_secret_file_to_string(
        &dir.join(CONTROL_FILE),
        false,
    )
    .ok()?;
    let text = crate::secret::ScrubbedString::new(text);
    let parsed = serde_json::from_str::<ControlFile>(text.as_str()).ok();
    parsed.filter(|c| c.version == 1)
}

// ---- control protocol ----------------------------------------------------------

fn keyed(key: &[u8; 32], label: &[u8], a: &[u8], b: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new_keyed(key);
    h.update(label);
    h.update(a);
    h.update(b);
    *h.finalize().as_bytes()
}

/// Constant-time tag comparison (`blake3::Hash` equality is constant-time).
fn tags_equal(a: [u8; 32], b: [u8; 32]) -> bool {
    blake3::Hash::from(a) == blake3::Hash::from(b)
}

/// Per-session keys: `mac` authenticates ciphertext, `enc` drives the keystream.
struct SessionKeys {
    mac: [u8; 32],
    enc: [u8; 32],
}

impl SessionKeys {
    fn derive(secret: &[u8; 32], node_nonce: &[u8], client_nonce: &[u8]) -> Self {
        Self {
            mac: keyed(secret, b"session", node_nonce, client_nonce),
            enc: keyed(secret, b"encrypt", node_nonce, client_nonce),
        }
    }

    /// XOR `data` with the keyed-BLAKE3 keystream for (direction, seq).
    fn apply_keystream(&self, direction: u8, seq: u64, data: &mut [u8]) {
        let mut h = blake3::Hasher::new_keyed(&self.enc);
        h.update(&[direction]);
        h.update(&seq.to_le_bytes());
        let mut stream = vec![0u8; data.len()];
        h.finalize_xof().fill(&mut stream);
        for (d, k) in data.iter_mut().zip(stream.iter()) {
            *d ^= k;
        }
        zeroize_slice(&mut stream);
    }
}

impl Drop for SessionKeys {
    fn drop(&mut self) {
        zeroize_slice(&mut self.mac);
        zeroize_slice(&mut self.enc);
    }
}

async fn write_msg(
    s: &mut TcpStream,
    keys: &SessionKeys,
    direction: u8,
    seq: u64,
    payload: &[u8],
) -> std::io::Result<()> {
    let mut sealed = payload.to_vec();
    keys.apply_keystream(direction, seq, &mut sealed);
    let tag = keyed(&keys.mac, &[direction], &seq.to_le_bytes(), &sealed);
    let len = u32::try_from(sealed.len() + 32).map_err(std::io::Error::other)?;
    s.write_all(&len.to_be_bytes()).await?;
    s.write_all(&sealed).await?;
    s.write_all(&tag).await?;
    s.flush().await
}

async fn read_msg(
    s: &mut TcpStream,
    keys: &SessionKeys,
    direction: u8,
    seq: u64,
) -> std::io::Result<Vec<u8>> {
    let bad = || std::io::Error::new(std::io::ErrorKind::InvalidData, "control message");
    let mut len = [0u8; 4];
    s.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if !(32..=MAX_CONTROL_FRAME).contains(&len) {
        return Err(bad());
    }
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).await?;
    let payload_len = len - 32;
    let mut tag = [0u8; 32];
    tag.copy_from_slice(&body[payload_len..]);
    body.truncate(payload_len);
    if !tags_equal(
        tag,
        keyed(&keys.mac, &[direction], &seq.to_le_bytes(), &body),
    ) {
        return Err(bad());
    }
    keys.apply_keystream(direction, seq, &mut body);
    Ok(body)
}

const TO_NODE: u8 = 0;
const FROM_NODE: u8 = 1;

/// What the running node reports about itself.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct NodeReport {
    incarnation: String,
    pid: u32,
    node_id: String,
    entity_id: String,
    public_key: String,
    bind: String,
    psk_source: String,
    trust_domain: String,
    started_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enrollment: Option<super::enrollment::EnrollmentReport>,
}

struct ControlState {
    report: NodeReport,
    draining: AtomicBool,
    enroll: Option<Arc<super::enrollment::EnrollContext>>,
}

async fn serve_control(
    listener: TcpListener,
    secret: [u8; 32],
    state: Arc<ControlState>,
    stop: mpsc::Sender<()>,
) {
    let permits = Arc::new(Semaphore::new(CONTROL_MAX_SESSIONS));
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            continue;
        };
        let (state, stop) = (state.clone(), stop.clone());
        tokio::spawn(async move {
            let _permit = permit;
            let _ = tokio::time::timeout(
                CONTROL_SESSION_TIMEOUT,
                control_session(stream, secret, &state, &stop),
            )
            .await;
        });
    }
}

async fn control_session(
    mut s: TcpStream,
    secret: [u8; 32],
    state: &ControlState,
    stop: &mpsc::Sender<()>,
) -> std::io::Result<()> {
    let bad = || std::io::Error::new(std::io::ErrorKind::PermissionDenied, "control auth");
    let server_nonce: [u8; 32] = random().map_err(|_| bad())?;
    let mut hello = CONTROL_MAGIC.to_vec();
    hello.extend_from_slice(&server_nonce);
    s.write_all(&hello).await?;
    let mut proof = [0u8; 64];
    s.read_exact(&mut proof).await?;
    let (client_nonce, client_tag) = proof.split_at(32);
    let mut tag = [0u8; 32];
    tag.copy_from_slice(client_tag);
    if !tags_equal(tag, keyed(&secret, b"client", &server_nonce, client_nonce)) {
        return Err(bad());
    }
    s.write_all(&keyed(&secret, b"server", &server_nonce, client_nonce))
        .await?;
    let keys = SessionKeys::derive(&secret, &server_nonce, client_nonce);

    let request = crate::secret::ScrubbedBytes::new(read_msg(&mut s, &keys, TO_NODE, 0).await?);
    let request: serde_json::Value = serde_json::from_slice(request.as_slice()).unwrap_or_default();
    let op = request["op"].as_str().unwrap_or_default().to_string();
    let draining = state.draining.load(Ordering::SeqCst);
    let reply = match op.as_str() {
        _ if op.starts_with("invite_") => match (&state.enroll, draining) {
            (_, true) => serde_json::json!({ "error": "node is draining" }),
            (None, false) => {
                serde_json::json!({ "error": "enrollment is not enabled on this node (start it with `up --enroll`)" })
            }
            (Some(ctx), false) => {
                let ctx = ctx.clone();
                tokio::task::spawn_blocking(move || ctx.handle(&op_owned(&request), &request))
                    .await
                    .unwrap_or_else(
                        |_| serde_json::json!({ "error": "enrollment operation failed" }),
                    )
            }
        },
        "status" => serde_json::json!({
            "state": if draining { "draining" } else { "ready" },
            "node": state.report,
        }),
        "shutdown" => {
            state.draining.store(true, Ordering::SeqCst);
            serde_json::json!({ "accepted": true, "incarnation": state.report.incarnation })
        }
        _ => serde_json::json!({ "error": "unknown control operation" }),
    };
    let bytes = crate::secret::ScrubbedBytes::new(
        serde_json::to_vec(&reply).map_err(std::io::Error::other)?,
    );
    write_msg(&mut s, &keys, FROM_NODE, 0, bytes.as_slice()).await?;
    if op == "shutdown" {
        let _ = stop.try_send(());
    }
    Ok(())
}

fn op_owned(request: &serde_json::Value) -> String {
    request["op"].as_str().unwrap_or_default().to_string()
}

#[derive(Debug)]
pub(crate) enum ControlError {
    /// No readable control file.
    NoControlFile,
    /// Could not connect to the recorded port.
    Unreachable,
    /// The endpoint did not prove the secret (or rejected ours).
    Unauthenticated,
    /// Malformed exchange.
    Protocol,
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoControlFile => "no running node found for this state directory",
            Self::Unreachable => "the node's control endpoint is unreachable",
            Self::Unauthenticated => "the node's control endpoint did not authenticate",
            Self::Protocol => "the control exchange failed",
        })
    }
}

/// Authenticate to the node recorded in `dir` and perform one request.
pub(crate) async fn control_call(
    dir: &Path,
    request: serde_json::Value,
) -> Result<(ControlFile, serde_json::Value), ControlError> {
    let control = read_control_file(dir).ok_or(ControlError::NoControlFile)?;
    let mut secret = [0u8; 32];
    let decoded = hex::decode(&control.secret)
        .ok()
        .filter(|d| d.len() == 32)
        .map(ScrubbedBytes::new)
        .ok_or(ControlError::NoControlFile)?;
    secret.copy_from_slice(decoded.as_slice());
    let result = tokio::time::timeout(CONTROL_SESSION_TIMEOUT, async {
        let mut s = TcpStream::connect(("127.0.0.1", control.port))
            .await
            .map_err(|_| ControlError::Unreachable)?;
        let mut hello = [0u8; 36];
        s.read_exact(&mut hello)
            .await
            .map_err(|_| ControlError::Unauthenticated)?;
        if hello[..4] != CONTROL_MAGIC {
            return Err(ControlError::Unauthenticated);
        }
        let server_nonce = &hello[4..];
        let client_nonce: [u8; 32] = random().map_err(|_| ControlError::Protocol)?;
        let mut proof = client_nonce.to_vec();
        proof.extend_from_slice(&keyed(&secret, b"client", server_nonce, &client_nonce));
        s.write_all(&proof)
            .await
            .map_err(|_| ControlError::Unauthenticated)?;
        let mut server_tag = [0u8; 32];
        s.read_exact(&mut server_tag)
            .await
            .map_err(|_| ControlError::Unauthenticated)?;
        if !tags_equal(
            server_tag,
            keyed(&secret, b"server", server_nonce, &client_nonce),
        ) {
            return Err(ControlError::Unauthenticated);
        }
        let keys = SessionKeys::derive(&secret, server_nonce, &client_nonce);
        let request = serde_json::to_vec(&request).map_err(|_| ControlError::Protocol)?;
        write_msg(&mut s, &keys, TO_NODE, 0, &request)
            .await
            .map_err(|_| ControlError::Protocol)?;
        let reply = crate::secret::ScrubbedBytes::new(
            read_msg(&mut s, &keys, FROM_NODE, 0)
                .await
                .map_err(|_| ControlError::Protocol)?,
        );
        serde_json::from_slice(reply.as_slice()).map_err(|_| ControlError::Protocol)
    })
    .await;
    zeroize_slice(&mut secret);
    let value = result.map_err(|_| ControlError::Unreachable)??;
    Ok((control, value))
}

// ---- up ------------------------------------------------------------------------

#[derive(Serialize)]
struct UpEvent<'a> {
    event: &'a str,
    #[serde(flatten)]
    node: &'a NodeReport,
    state_dir: String,
}

/// Run one production node in the foreground until Ctrl-C or `net-mesh down`.
pub async fn run_up(
    args: UpArgs,
    output: Option<OutputFormat>,
    config_path: Option<&Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    // Resolve and validate everything before any filesystem or network effect.
    let profile = crate::context::resolve_profile(config_path, profile_name).await?;
    let state = state_dir(args.state_dir, profile_name)?;
    let source = PskSource::parse(args.psk_from.as_deref())?;
    let bind_raw = args
        .bind
        .or_else(|| profile.bind.clone())
        .unwrap_or_else(|| "0.0.0.0:0".to_string());
    let bind = crate::context::parse_bind_literal(&bind_raw)?;
    let identity_path = args.identity.or_else(|| profile.identity.clone());
    let fmt = OutputFormat::resolve_stream(output);
    let enroll_plan = if args.enroll {
        Some(super::enrollment::EnrollPlan::validate(
            args.public_addr,
            args.issuer_identity,
            args.ledger,
            args.domain_name,
            bind,
            &state,
            profile_name,
        )?)
    } else {
        None
    };
    // A supplied PSK is read and validated before any filesystem effect, so a
    // refused source leaves no state behind.
    let supplied = match &source {
        PskSource::Generated => None,
        PskSource::File(path) => Some(read_psk_file(path).await?),
        PskSource::Stdin => Some(read_psk_stdin().await?),
    };

    std::fs::create_dir_all(&state)
        .map_err(|e| generic(format!("create state directory {}: {e}", state.display())))?;
    let dir = state.join(NODE_SUBDIR);
    let generate = matches!(source, PskSource::Generated);
    let (dir_owned, gen) = (dir.clone(), generate);
    let (mut storage, mut secrets) = tokio::task::spawn_blocking(move || acquire(&dir_owned, gen))
        .await
        .map_err(|e| generic(format!("node state task failed: {e}")))??;
    let lifetime_lock = hold_lifetime_lock(&dir)?;
    // Enrollment ownership (issuer + ledger lock) is taken before any bind.
    let enroll_owner = match enroll_plan {
        Some(plan) => Some(plan.open().await?),
        None => None,
    };

    let mut psk = match supplied {
        Some(psk) => psk,
        None => match secrets.psk {
            Some(psk) => psk,
            None => {
                // First generated start after earlier supplied-source runs:
                // commit the new value before bind.
                secrets.psk = Some(random()?);
                storage
                    .replace(secrets.encode().as_slice())
                    .map_err(|e| storage_error(&dir, e))?;
                secrets.psk.unwrap_or_default()
            }
        },
    };
    let psk_value = Psk::new(psk);
    let trust_domain = psk_value.trust_domain().to_string();
    let identity = match &identity_path {
        Some(path) => crate::context::load_operator_identity(path).await?,
        None => Identity::from_seed(secrets.seed),
    };
    drop(secrets);

    let built = net_sdk::MeshBuilder::new(&bind.to_string(), &psk)
        .map_err(|e| invalid_args(format!("mesh bind {bind}: {e}")));
    zeroize_slice(&mut psk);
    let mesh = built?
        .identity(identity.clone())
        .build()
        .await
        .map_err(|e| connection_failure(format!("mesh start on {bind}: {e}")))?;
    mesh.start();

    let enrollment = match enroll_owner {
        Some(owner) => Some(owner.start(&mesh, psk_value).await?),
        None => None,
    };

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| generic(format!("control endpoint bind: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| generic(format!("control endpoint: {e}")))?
        .port();
    let mut secret: [u8; 32] = random()?;
    let incarnation = hex::encode(random::<16>()?);
    let report = NodeReport {
        incarnation: incarnation.clone(),
        pid: std::process::id(),
        node_id: format!("0x{:016x}", mesh.node_id()),
        entity_id: hex::encode(identity.entity_id().as_bytes()),
        public_key: hex::encode(mesh.public_key()),
        bind: mesh.local_addr().to_string(),
        psk_source: source.kind().to_string(),
        trust_domain,
        started_at: now_unix(),
        enrollment: enrollment.as_ref().map(|e| e.report()),
    };
    let control = ControlFile {
        version: 1,
        incarnation: incarnation.clone(),
        port,
        secret: hex::encode(secret),
        pid: report.pid,
        started_at: report.started_at,
    };
    let control_text = crate::secret::ScrubbedString::new(
        serde_json::to_string(&control).map_err(|e| generic(format!("control file: {e}")))?,
    );
    let control_path = dir.join(CONTROL_FILE);
    crate::commands::identity::write_identity_atomically(
        &dir.join(format!("{CONTROL_FILE}.{incarnation}.tmp")),
        &control_path,
        control_text.as_bytes(),
    )
    .await?;
    drop(control_text);

    let state_ctl = Arc::new(ControlState {
        report: report.clone(),
        draining: AtomicBool::new(false),
        enroll: enrollment.as_ref().map(|e| e.context()),
    });
    let (stop_tx, mut stop_rx) = mpsc::channel(1);
    let server = tokio::spawn(serve_control(listener, secret, state_ctl.clone(), stop_tx));
    zeroize_slice(&mut secret);

    emit_stream_row(
        fmt,
        &UpEvent {
            event: "ready",
            node: &report,
            state_dir: state.display().to_string(),
        },
    )
    .map_err(|e| generic(format!("write readiness: {e}")))?;

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = stop_rx.recv() => {}
    }

    // Drain: stop taking control sessions, stop the node, then remove the
    // endpoint and release the lifetime lock. Identity, PSK and state persist.
    state_ctl.draining.store(true, Ordering::SeqCst);
    server.abort();
    if let Some(enrollment) = enrollment {
        enrollment.shutdown().await;
    }
    let stopped = tokio::time::timeout(MESH_SHUTDOWN_TIMEOUT, mesh.shutdown()).await;
    let _ = std::fs::remove_file(&control_path);
    drop(lifetime_lock);
    drop(storage);
    match stopped {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(generic(format!("mesh shutdown: {e}"))),
        Err(_) => return Err(timeout("mesh shutdown did not complete in time")),
    }
    emit_stream_row(
        fmt,
        &UpEvent {
            event: "stopped",
            node: &report,
            state_dir: state.display().to_string(),
        },
    )
    .map_err(|e| generic(format!("write stop event: {e}")))
}

// ---- node status -------------------------------------------------------------

#[derive(Serialize)]
struct StatusView {
    state: String,
    state_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    node: Option<NodeReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

async fn observe(state: &Path) -> Result<StatusView, CliError> {
    let dir = state.join(NODE_SUBDIR);
    let view = |s: &str, node: Option<NodeReport>, detail: Option<&str>| StatusView {
        state: s.to_string(),
        state_dir: state.display().to_string(),
        node,
        detail: detail.map(str::to_string),
    };
    Ok(match probe(&dir)? {
        Liveness::Absent => view(
            "stopped",
            None,
            Some("no node has run with this state directory"),
        ),
        Liveness::Free if dir.join(CONTROL_FILE).exists() => view(
            "stale_metadata",
            None,
            Some("a control file remains but no process holds the lifetime lock"),
        ),
        Liveness::Free => view("stopped", None, None),
        Liveness::Held => match control_call(&dir, serde_json::json!({ "op": "status" })).await {
            Ok((_, reply)) => {
                let node = serde_json::from_value::<NodeReport>(reply["node"].clone()).ok();
                let s = reply["state"].as_str().unwrap_or("unknown").to_string();
                view(&s, node, None)
            }
            Err(ControlError::NoControlFile) => view(
                "starting",
                None,
                Some("the lifetime lock is held but the control endpoint is not published yet"),
            ),
            Err(ControlError::Unauthenticated) => view(
                "unknown",
                None,
                Some("the control endpoint did not authenticate; not trusting it"),
            ),
            Err(e) => view("unknown", None, Some(&format!("control endpoint: {e:?}"))),
        },
    })
}

/// Report the selected node's verified state.
pub async fn run_status(
    args: StatusArgs,
    output: Option<OutputFormat>,
    profile_name: &str,
) -> Result<(), CliError> {
    let state = state_dir(args.state_dir, profile_name)?;
    let view = observe(&state).await?;
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write status: {e}")))
}

// ---- down ----------------------------------------------------------------------

#[derive(Serialize)]
struct DownView {
    state: &'static str,
    was_running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    incarnation: Option<String>,
    stale_metadata: bool,
}

/// Ask the selected node to drain and stop, then verify it released its
/// lifetime lock. Never uses a PID; never revokes or deletes anything.
pub async fn run_down(
    args: DownArgs,
    output: Option<OutputFormat>,
    profile_name: &str,
) -> Result<(), CliError> {
    let state = state_dir(args.state_dir, profile_name)?;
    let dir = state.join(NODE_SUBDIR);
    let fmt = OutputFormat::resolve_oneshot(output);
    let emit = |v: &DownView| emit_value(fmt, v).map_err(|e| generic(format!("write result: {e}")));
    if probe(&dir)? != Liveness::Held {
        return emit(&DownView {
            state: "stopped",
            was_running: false,
            incarnation: None,
            stale_metadata: dir.join(CONTROL_FILE).exists(),
        });
    }
    let (control, reply) = control_call(&dir, serde_json::json!({ "op": "shutdown" })).await.map_err(|e| {
        connection_failure(format!(
            "the node holds its lifetime lock but its control endpoint failed ({e:?}); it was not stopped"
        ))
    })?;
    if reply["accepted"] != serde_json::Value::Bool(true)
        || reply["incarnation"].as_str() != Some(control.incarnation.as_str())
    {
        return Err(generic(
            "the node did not accept the shutdown request for the recorded incarnation",
        ));
    }
    let deadline = tokio::time::Instant::now() + args.wait;
    while tokio::time::Instant::now() < deadline {
        if probe(&dir)? != Liveness::Held {
            return emit(&DownView {
                state: "stopped",
                was_running: true,
                incarnation: Some(control.incarnation),
                stale_metadata: false,
            });
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(timeout(format!(
        "node incarnation {} accepted shutdown but still holds its lifetime lock after {:?}; state is running or unknown",
        control.incarnation, args.wait
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn control_messages_are_encrypted_and_bound_to_direction_and_sequence() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (mut node, _) = listener.accept().await.unwrap();
        let keys = SessionKeys::derive(&[7; 32], &[1; 32], &[2; 32]);
        let secret = b"netmesh-join_SECRET-TOKEN-MARKER";

        // The bytes on the socket never contain the plaintext.
        write_msg(&mut client, &keys, TO_NODE, 0, secret)
            .await
            .unwrap();
        let mut len = [0u8; 4];
        node.read_exact(&mut len).await.unwrap();
        let mut raw = vec![0u8; u32::from_be_bytes(len) as usize];
        node.read_exact(&mut raw).await.unwrap();
        assert!(!raw.windows(secret.len()).any(|w| w == secret));

        // Round trip with the right keys, direction and sequence.
        write_msg(&mut client, &keys, TO_NODE, 1, secret)
            .await
            .unwrap();
        assert_eq!(
            read_msg(&mut node, &keys, TO_NODE, 1).await.unwrap(),
            secret
        );
        // A replayed or reordered frame fails authentication.
        write_msg(&mut client, &keys, TO_NODE, 2, secret)
            .await
            .unwrap();
        assert!(read_msg(&mut node, &keys, TO_NODE, 3).await.is_err());
    }
}
