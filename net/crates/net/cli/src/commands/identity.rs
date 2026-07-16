//! `net identity (generate|show|fingerprint)` — operator-identity
//! authoring + inspection.
//!
//! Identity files are TOML at `$XDG_CONFIG_HOME/net-mesh/identities/`
//! by default (see [`default_identity_path`]). Format:
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
use net_sdk::identity::EntityId;
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
    /// Revoke a delegated identity (Phase 3): raise an issuer's revocation
    /// floor in the machine-shared store so a running `net wrap --owner-root`
    /// provider stops admitting its delegations — without a restart.
    Revoke(RevokeArgs),
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

#[derive(Args, Debug)]
pub struct RevokeArgs {
    /// The issuer entity-id to revoke (32-byte ed25519 pubkey, 64 hex chars,
    /// optional `0x`). To revoke a machine's gateway (and its subagents), pass
    /// the *machine* identity's entity-id — the issuer of the machine→gateway
    /// link, so the floor bump kills the whole subtree.
    pub issuer: String,

    /// Raise the floor to this generation (default 1 — revokes all current
    /// generation-0 delegations). Monotonic: a value ≤ the current floor is a
    /// no-op (never un-revokes).
    #[arg(long, default_value_t = 1)]
    pub generation: u32,

    /// Revocation-store path (default: the per-user shared file a
    /// `net wrap --owner-root` provider honors).
    #[arg(long = "revocation-store", value_name = "PATH")]
    pub revocation_store: Option<PathBuf>,
}

pub async fn run(cmd: IdentityCommand, output: Option<OutputFormat>) -> Result<(), CliError> {
    match cmd {
        IdentityCommand::Generate(args) => run_generate(args, output).await,
        IdentityCommand::Show(args) => run_show(args, output).await,
        IdentityCommand::Fingerprint(args) => run_fingerprint(args, output).await,
        IdentityCommand::Revoke(args) => run_revoke(args, output).await,
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

    // `try_exists` distinguishes "file is absent" (Ok(false)) from
    // "I can't tell because of a permission error" (Err). Pre-fix
    // `.exists()` followed symlinks and returned false on
    // permission errors, so a symlink at `path` pointing to a
    // sensitive file (or a permission-denied stat) silently
    // skipped the safety gate and we overwrote the target.
    if !args.force {
        match tokio::fs::try_exists(&path).await {
            Ok(true) => {
                return Err(invalid_args(format!(
                    "identity file already exists at {}; pass --force to overwrite",
                    path.display()
                )));
            }
            Ok(false) => {}
            Err(e) => {
                return Err(generic(format!(
                    "failed to stat {}: {e}; pass --force to override",
                    path.display()
                )));
            }
        }
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

    // Atomic, mode-restricted publish. Pre-fix `tokio::fs::write`
    // created the file with the process umask (commonly 0o644 —
    // world-readable on most distros) and `enforce_strict_permissions`
    // only chmod'd it down to 0o600 *after* the bytes had already
    // landed on disk. A reader / backup agent / inotify-watcher in
    // that window could grab the seed. Open the temp file with
    // mode=0o600 in one syscall (Unix), write the seed into the
    // already-restricted handle, and atomic-rename onto the final
    // path so the visible file is either an intact prior identity
    // or the new one - never a half-written world-readable seed.
    let pid = std::process::id();
    let tmp = path.with_extension(format!("tmp.{pid}"));
    write_identity_atomically(&tmp, &path, toml_text.as_bytes()).await?;
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
    use sha2::{Digest, Sha256};

    let file = read_identity_file(&args.path, args.insecure_permissions).await?;
    let public_key = hex::decode(&file.public_key_hex)
        .map_err(|e| sdk(format!("public_key_hex is not valid hex: {e}")))?;
    // SHA-256 over the public key, truncated to the first 8
    // bytes for a short fingerprint. Renders as `XX:XX:XX:...`
    // — the ssh-style separator that operators recognize at a
    // glance.
    let digest = Sha256::digest(&public_key);
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
// revoke
// =========================================================================

async fn run_revoke(args: RevokeArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    let issuer = parse_entity_hex(&args.issuer)?;
    let path = args
        .revocation_store
        .or_else(net_sdk::revocation::default_revocation_store_path)
        .ok_or_else(|| {
            invalid_args(
                "no revocation-store path could be resolved; pass --revocation-store <PATH>",
            )
        })?;
    let floor = net_sdk::revocation::RevocationStore::revoke_below(&path, &issuer, args.generation)
        .map_err(|e| sdk(format!("revoke failed: {e}")))?;
    let info = RevokeOutput {
        issuer_hex: hex::encode(issuer.as_bytes()),
        generation: args.generation,
        floor,
        store: path.display().to_string(),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &info)
        .map_err(|e| generic(format!("write revoke: {e}")))?;
    Ok(())
}

/// Parse an issuer entity-id: 64 hex chars (optional `0x`) → 32-byte
/// [`EntityId`].
pub(crate) fn parse_entity_hex(raw: &str) -> Result<EntityId, CliError> {
    let trimmed = raw
        .strip_prefix("0x")
        .or_else(|| raw.strip_prefix("0X"))
        .unwrap_or(raw);
    let bytes =
        hex::decode(trimmed).map_err(|e| invalid_args(format!("issuer: invalid hex: {e}")))?;
    let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        invalid_args(format!(
            "issuer must be 32 bytes (64 hex chars), got {}",
            bytes.len()
        ))
    })?;
    Ok(EntityId::from_bytes(arr))
}

// =========================================================================
// Disk shape
// =========================================================================

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct IdentityFile {
    pub(crate) operator_id_hex: String,
    pub(crate) seed_hex: String,
    pub(crate) public_key_hex: String,
    pub(crate) created_at: String,
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

#[derive(Debug, Serialize)]
struct RevokeOutput {
    /// The issuer whose delegations were revoked (hex entity-id).
    issuer_hex: String,
    /// The generation the revocation raised the floor toward.
    generation: u32,
    /// The floor now in effect for this issuer (≥ `generation`).
    floor: u32,
    /// The store the revocation was written to.
    store: String,
}

pub(crate) async fn read_identity_file(
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

/// Write `bytes` to `tmp` with the tightest creation mode the
/// platform supports, then atomic-rename onto `final_path`. On Unix
/// the temp file is created with `O_CREAT | O_EXCL` and mode 0o600
/// in a single syscall so the seed is never reachable to a
/// concurrent reader at the default umask. On Windows the temp
/// file is created with default ACLs — managed out-of-band per the
/// module header — and `enforce_strict_permissions` is a no-op.
pub(crate) async fn write_identity_atomically(
    tmp: &Path,
    final_path: &Path,
    bytes: &[u8],
) -> Result<(), CliError> {
    let tmp_owned = tmp.to_path_buf();
    let bytes_owned = bytes.to_vec();

    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp_owned)?;
        std::io::Write::write_all(&mut f, &bytes_owned)?;
        f.sync_all()?;
        Ok(())
    })
    .await
    .map_err(|e| generic(format!("seed-write task panicked: {e}")))?
    .map_err(|e| {
        generic(format!(
            "failed to write identity tmp {}: {e}",
            tmp.display()
        ))
    })?;

    tokio::fs::rename(tmp, final_path).await.map_err(|e| {
        let tmp_for_cleanup = tmp.to_path_buf();
        tokio::spawn(async move {
            let _ = tokio::fs::remove_file(tmp_for_cleanup).await;
        });
        generic(format!(
            "rename identity tmp {} -> {}: {e}",
            tmp.display(),
            final_path.display()
        ))
    })?;
    Ok(())
}

#[cfg(unix)]
pub(crate) async fn enforce_strict_permissions(path: &Path) -> Result<(), CliError> {
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
pub(crate) async fn enforce_strict_permissions(_path: &Path) -> Result<(), CliError> {
    // No-op on Windows — file ACLs don't have a clean
    // 0o600 analog accessible from std::fs. Operators on
    // Windows are expected to manage NTFS ACLs out-of-band.
    Ok(())
}

#[cfg(unix)]
pub(crate) async fn check_strict_permissions(path: &Path) -> Result<(), CliError> {
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
pub(crate) async fn check_strict_permissions(path: &Path) -> Result<(), CliError> {
    // NTFS ACLs don't have a clean 0o600 analog reachable from
    // `std::fs`, so structurally the permission gate is a no-op
    // on Windows — but pre-fix that no-op was silent and every
    // doc on top of `read_identity_file` advertised a contract
    // that wasn't enforced. Operators reading the help text or
    // module header believed their identity files were guarded
    // the same way `ssh` guards `~/.ssh/id_*`; on Windows they
    // weren't, with no surfaced warning.
    //
    // Surface a stderr warning so a permissive ACL is at least
    // observable in operator logs. Pass `--insecure-permissions`
    // to suppress (matches the Unix gate's escape hatch). The
    // proper fix is a `GetFileSecurityW` DACL check; tracked as
    // a follow-up because it pulls in the `windows`-rs crate.
    eprintln!(
        "warning: identity-file permission gate is a no-op on Windows; \
         NTFS ACLs on {} are not validated. Pass --insecure-permissions \
         to silence, or manage the DACL out-of-band.",
        path.display()
    );
    Ok(())
}

fn default_identity_path(operator_id: u64) -> PathBuf {
    let base = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("net-mesh")
        .join("identities");
    base.join(format!("operator-0x{operator_id:016x}.toml"))
}

pub(crate) fn now_iso8601() -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_formats_unix_epoch() {
        assert_eq!(format_iso8601_utc(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn iso8601_formats_known_timestamp() {
        // 2025-11-17T12:34:56Z = 1763382896
        assert_eq!(format_iso8601_utc(1763382896), "2025-11-17T12:34:56Z");
    }

    #[test]
    fn parse_entity_hex_accepts_64_hex_and_rejects_bad() {
        let id = net_sdk::Identity::generate();
        let hexed = hex::encode(id.entity_id().as_bytes());
        assert_eq!(
            parse_entity_hex(&hexed).unwrap().as_bytes(),
            id.entity_id().as_bytes()
        );
        assert_eq!(
            parse_entity_hex(&format!("0x{hexed}")).unwrap().as_bytes(),
            id.entity_id().as_bytes()
        );
        assert_eq!(
            parse_entity_hex(&format!("0X{hexed}")).unwrap().as_bytes(),
            id.entity_id().as_bytes()
        );
        assert!(parse_entity_hex("deadbeef").is_err()); // wrong length
        assert!(parse_entity_hex(&"zz".repeat(32)).is_err()); // non-hex
    }

    #[test]
    fn revoke_writes_the_floor_to_the_store() {
        // `net identity revoke` writes a floor a running provider then honors.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rev.json");
        let issuer = net_sdk::Identity::generate();
        let floor =
            net_sdk::revocation::RevocationStore::revoke_below(&path, issuer.entity_id(), 1)
                .unwrap();
        assert_eq!(floor, 1);
        assert_eq!(
            net_sdk::revocation::RevocationStore::load(&path)
                .unwrap()
                .floor(issuer.entity_id()),
            1
        );
    }
}
