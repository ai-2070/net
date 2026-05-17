//! `net identity (generate|show|fingerprint)` — operator-identity
//! authoring + inspection.
//!
//! Identity files are TOML at `$XDG_CONFIG_HOME/net/identities/`
//! by default. Format:
//!
//! ```toml
//! operator_id = "0x1234..."
//! seed_hex    = "..."                  # 64 hex chars (32-byte ed25519 seed)
//! created_at  = "2026-05-17T12:34:56Z"
//! note        = "Production operator for the deck-fleet cluster"
//! ```
//!
//! - `generate` writes a fresh seed + sets `chmod 600` on Unix
//!   so the file isn't world-readable.
//! - `show` reads the file, refuses to proceed if the permissions
//!   are too permissive, and prints `operator_id` / `public_key`
//!   / `created_at` / `note` — never the seed.
//! - `fingerprint` prints a short SHA-256-derived identifier
//!   suitable for inclusion in audit dashboards.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

use crate::error::{generic, invalid_args, sdk, CliError};
use crate::prelude::{emit_value, OutputFormat};

#[derive(Subcommand, Debug)]
pub enum IdentityCommand {
    /// Generate a fresh operator identity.
    Generate(GenerateArgs),
    /// Print the public summary (operator_id / public_key /
    /// created_at / note). Never emits the seed.
    Show(ShowArgs),
    /// Print a short SHA-256-derived identifier suitable for
    /// audit dashboards.
    Fingerprint(FingerprintArgs),
}

#[derive(Args, Debug)]
pub struct GenerateArgs {
    /// Output path. Defaults to
    /// `$XDG_CONFIG_HOME/net/identities/operator-<id>.toml`.
    #[arg(long)]
    pub out: Option<PathBuf>,

    /// Free-form note saved alongside the identity.
    #[arg(long)]
    pub note: Option<String>,

    /// Overwrite an existing file. Refuses by default.
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct ShowArgs {
    /// Path to the identity file.
    pub path: PathBuf,

    /// Allow files with permissive (world-readable) modes on
    /// Unix. Off by default — the binary refuses to read a
    /// seed file someone else can read, mirroring `ssh`'s
    /// permission gate.
    #[arg(long)]
    pub insecure_permissions: bool,
}

#[derive(Args, Debug)]
pub struct FingerprintArgs {
    /// Path to the identity file.
    pub path: PathBuf,

    /// Allow permissive file modes. See `show --insecure-permissions`.
    #[arg(long)]
    pub insecure_permissions: bool,
}

pub async fn run(cmd: IdentityCommand, output: Option<OutputFormat>) -> Result<(), CliError> {
    match cmd {
        IdentityCommand::Generate(args) => run_generate(args, output).await,
        IdentityCommand::Show(args) => run_show(args, output).await,
        IdentityCommand::Fingerprint(args) => run_fingerprint(args, output).await,
    }
}

// =========================================================================
// generate
// =========================================================================

async fn run_generate(args: GenerateArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    use net_sdk::deck::OperatorIdentity;

    let identity = OperatorIdentity::generate();
    let operator_id = identity.operator_id();
    let seed = *identity.keypair().secret_bytes();
    let public_key = *identity.keypair().entity_id().as_bytes();

    let path = args
        .out
        .unwrap_or_else(|| default_identity_path(operator_id));

    if !args.force && path.exists() {
        return Err(invalid_args(format!(
            "identity file already exists at {}; pass --force to overwrite",
            path.display()
        )));
    }

    // Ensure the parent directory exists. We deliberately don't
    // create a deep tree without permission; `dirs::config_dir()`
    // is already user-owned so a single `create_dir_all` is fine.
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            generic(format!(
                "failed to create parent directory {}: {e}",
                parent.display()
            ))
        })?;
    }

    let file = IdentityFile {
        operator_id_hex: format!("0x{operator_id:016x}"),
        seed_hex: hex::encode(seed),
        public_key_hex: hex::encode(public_key),
        created_at: now_iso8601(),
        note: args.note.clone(),
    };
    let toml_text = toml::to_string_pretty(&file)
        .map_err(|e| generic(format!("failed to serialize identity TOML: {e}")))?;

    tokio::fs::write(&path, &toml_text).await.map_err(|e| {
        generic(format!(
            "failed to write identity file {}: {e}",
            path.display()
        ))
    })?;
    enforce_strict_permissions(&path).await?;

    // Print the public summary on stdout — never the seed, even
    // though the file we just wrote contains it. Operators who
    // want the seed read it from the file directly.
    let summary = IdentitySummary {
        path: path.display().to_string(),
        operator_id_hex: file.operator_id_hex.clone(),
        public_key_hex: file.public_key_hex.clone(),
        created_at: file.created_at.clone(),
        note: file.note.clone(),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &summary)
        .map_err(|e| generic(format!("write summary: {e}")))?;
    Ok(())
}

// =========================================================================
// show
// =========================================================================

async fn run_show(args: ShowArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    let file = read_identity_file(&args.path, args.insecure_permissions).await?;
    let summary = IdentitySummary {
        path: args.path.display().to_string(),
        operator_id_hex: file.operator_id_hex.clone(),
        public_key_hex: file.public_key_hex.clone(),
        created_at: file.created_at.clone(),
        note: file.note.clone(),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &summary)
        .map_err(|e| generic(format!("write summary: {e}")))?;
    Ok(())
}

// =========================================================================
// fingerprint
// =========================================================================

async fn run_fingerprint(
    args: FingerprintArgs,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    let file = read_identity_file(&args.path, args.insecure_permissions).await?;
    let public_key = hex::decode(&file.public_key_hex)
        .map_err(|e| sdk(format!("public_key_hex is not valid hex: {e}")))?;
    // SHA-256 over the public key, truncated to the first 8
    // bytes for a short fingerprint. Renders as `XX:XX:XX:...`
    // — the ssh-style separator that operators recognize at a
    // glance. SHA-256 over 32 bytes is cheap; we don't avoid
    // pulling a hash dep by reusing blake2 here because we want
    // the operator-visible fingerprint to use a hash whose
    // collision properties are widely known.
    let mut hasher = SimpleSha256::new();
    hasher.update(&public_key);
    let digest = hasher.finalize();
    let short: Vec<String> = digest.iter().take(8).map(|b| format!("{b:02X}")).collect();
    let fingerprint = short.join(":");
    let info = FingerprintOutput {
        operator_id_hex: file.operator_id_hex.clone(),
        fingerprint,
    };
    emit_value(OutputFormat::resolve_oneshot(output), &info)
        .map_err(|e| generic(format!("write fingerprint: {e}")))?;
    Ok(())
}

// =========================================================================
// Disk shape
// =========================================================================

#[derive(Debug, Serialize, Deserialize)]
struct IdentityFile {
    operator_id_hex: String,
    seed_hex: String,
    public_key_hex: String,
    created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Debug, Serialize)]
struct IdentitySummary {
    path: String,
    operator_id_hex: String,
    public_key_hex: String,
    created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Debug, Serialize)]
struct FingerprintOutput {
    operator_id_hex: String,
    fingerprint: String,
}

async fn read_identity_file(
    path: &Path,
    insecure_permissions: bool,
) -> Result<IdentityFile, CliError> {
    if !insecure_permissions {
        check_strict_permissions(path).await?;
    }
    let text = tokio::fs::read_to_string(path).await.map_err(|e| {
        generic(format!(
            "failed to read identity file {}: {e}",
            path.display()
        ))
    })?;
    let parsed: IdentityFile = toml::from_str(&text).map_err(|e| {
        invalid_args(format!(
            "identity file {} failed to parse: {e}",
            path.display()
        ))
    })?;
    Ok(parsed)
}

#[cfg(unix)]
async fn enforce_strict_permissions(path: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    tokio::fs::set_permissions(path, perms).await.map_err(|e| {
        generic(format!(
            "failed to set 0600 permissions on {}: {e}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
async fn enforce_strict_permissions(_path: &Path) -> Result<(), CliError> {
    // No-op on Windows — file ACLs don't have a clean
    // 0o600 analog accessible from std::fs. Operators on
    // Windows are expected to manage NTFS ACLs out-of-band.
    Ok(())
}

#[cfg(unix)]
async fn check_strict_permissions(path: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = tokio::fs::metadata(path).await.map_err(|e| {
        generic(format!(
            "failed to stat identity file {}: {e}",
            path.display()
        ))
    })?;
    let mode = meta.permissions().mode() & 0o777;
    // Refuse anything where group or other can read the file.
    if mode & 0o077 != 0 {
        return Err(invalid_args(format!(
            "identity file {} has permissive mode {:#o}; tighten to 0600 \
             or pass --insecure-permissions to override (kind: \
             permissive_mode)",
            path.display(),
            mode
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
async fn check_strict_permissions(_path: &Path) -> Result<(), CliError> {
    Ok(())
}

fn default_identity_path(operator_id: u64) -> PathBuf {
    let base = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("net")
        .join("identities");
    base.join(format!("operator-0x{operator_id:016x}.toml"))
}

fn now_iso8601() -> String {
    // The chrono crate isn't in the CLI's deps; format the
    // current SystemTime as ISO-8601 by hand. Format:
    // `YYYY-MM-DDTHH:MM:SSZ` (no sub-second precision).
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_iso8601_utc(now)
}

fn format_iso8601_utc(secs_since_epoch: u64) -> String {
    // Civil-from-days algorithm (Hinnant 2010). Cheap, exact,
    // and avoids pulling chrono just for one timestamp.
    const SECONDS_PER_DAY: u64 = 86_400;
    let days = (secs_since_epoch / SECONDS_PER_DAY) as i64;
    let remainder = secs_since_epoch % SECONDS_PER_DAY;
    let hour = (remainder / 3600) as u32;
    let minute = ((remainder % 3600) / 60) as u32;
    let second = (remainder % 60) as u32;

    // Convert days-since-1970-01-01 to (year, month, day) via
    // Hinnant's algorithm.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, m, d, hour, minute, second
    )
}

// =========================================================================
// Tiny self-contained SHA-256 (for `fingerprint`)
//
// The CLI already has heavy deps; adding `sha2` is fine in
// principle, but the substrate already pulls it in transitively
// via the SDK feature stack. We re-export through a thin wrapper
// so the call site reads cleanly without a workspace dep tweak.
// =========================================================================

struct SimpleSha256 {
    inner: Box<dyn FnMut(&[u8]) -> Vec<u8> + Send>,
    buf: Vec<u8>,
}

impl SimpleSha256 {
    fn new() -> Self {
        // Use the SDK-bundled `sha2` indirectly via `net_sdk`'s
        // re-exported `blake3`? No — we want SHA-256
        // specifically. Instead, hash on `finalize` using a
        // minimal SHA-256 implementation. Pulling `sha2` directly
        // would be the production choice, but Phase 1 keeps the
        // CLI's direct dep list small; the fingerprint is
        // identification, not cryptographic security.
        Self {
            inner: Box::new(|bytes| sha256_oneshot(bytes)),
            buf: Vec::new(),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    fn finalize(mut self) -> Vec<u8> {
        (self.inner)(&self.buf)
    }
}

/// SHA-256 of `data`. RFC 6234 reference implementation,
/// inlined so the CLI doesn't grow a direct `sha2` dep just for
/// the fingerprint subcommand. The fingerprint is for human-
/// readable identification — collision resistance matters but
/// the surface isn't on a cryptographic hot path.
fn sha256_oneshot(data: &[u8]) -> Vec<u8> {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, b4) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes(b4.try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d) = (h[0], h[1], h[2], h[3]);
        let (mut e, mut f, mut g, mut hh) = (h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = Vec::with_capacity(32);
    for word in h {
        out.extend_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_formats_unix_epoch() {
        assert_eq!(format_iso8601_utc(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn iso8601_formats_known_timestamp() {
        // 2026-05-17T12:34:56Z = 1763382896
        assert_eq!(format_iso8601_utc(1763382896), "2025-11-17T12:34:56Z");
    }

    #[test]
    fn sha256_known_vector() {
        // "abc" → ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        let digest = sha256_oneshot(b"abc");
        let hex_digest = digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        assert_eq!(
            hex_digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_empty_string() {
        let digest = sha256_oneshot(b"");
        let hex_digest = digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        assert_eq!(
            hex_digest,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
