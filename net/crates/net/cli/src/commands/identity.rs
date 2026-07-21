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

use crate::commands::org::{refuse_replacing_foreign_seed, SeedArtifact};
use crate::error::{generic, invalid_args, sdk, CliError};
use crate::prelude::{emit_value, OutputFormat};
use crate::secret::{zeroize_string, ScrubbedBytes, ScrubbedString};

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
    // §10 — the seed is copied out of the keypair here, so this local is a
    // second live copy of the private key and must be scrubbed on EVERY exit,
    // not just the success tail. `org.rs:run_keygen` avoids the copy entirely
    // by consuming `secret_bytes()` inline; this path needs it twice (the file
    // body and nothing else), so it gets an RAII guard instead.
    let seed = ScrubbedBytes::new(identity.keypair().secret_bytes().to_vec());
    let public_key = *identity.keypair().entity_id().as_bytes();

    let path = match args.out {
        Some(explicit) => explicit,
        None => default_identity_path(operator_id).ok_or_else(|| {
            invalid_args(
                "cannot resolve the platform config directory, and refusing to fall back to \
                 the working directory — this file holds the operator's private seed. Pass \
                 an explicit --out."
                    .to_string(),
            )
        })?,
    };

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
            // No `--force` advice: it does not override a stat failure, it
            // skips the check and overwrites whatever is there (§17).
            Err(e) => {
                return Err(generic(format!("failed to stat {}: {e}", path.display())));
            }
        }
    } else {
        // §2. `--force` here means "replace the identity at this path". Because
        // the publish below ends in an unconditional `rename`, and because the
        // block above is skipped entirely when forcing, this guard is the only
        // thing standing between a drifted `--out` and the org root key:
        //
        //   net identity generate --out "$KEY" --force
        //
        // with `$KEY` pointing at an org key file used to replace the org root
        // with an operator identity and exit 0 — root unrecoverable, no floor
        // ever issuable again, every outstanding membership cert live until
        // natural expiry. The original §2 fix guarded `issue-cert` /
        // `issue-floors` and never reached the two verbs that write seed files.
        //
        // Replacing an identity with an identity stays allowed: that is the
        // rotation `--force` exists for.
        refuse_replacing_foreign_seed(&path, SeedArtifact::Identity).await?;
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
        seed_hex: hex::encode(seed.as_slice()),
        public_key_hex: hex::encode(public_key),
        created_at: now_iso8601(),
        note: args.note.clone(),
    };
    // The serialized TOML carries the seed too — wrap it so a failed write or
    // permission step scrubs it as well, not only the success tail. `file`
    // scrubs its own `seed_hex` on Drop.
    let toml_text = ScrubbedString::new(
        toml::to_string_pretty(&file)
            .map_err(|e| generic(format!("failed to serialize identity TOML: {e}")))?,
    );

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

/// §10 — deliberately NO `Debug` derive, and a `Drop` that scrubs `seed_hex`.
///
/// `OrgKeyFile` has had both since OA2-F for exactly this reason; this struct
/// is the same shape holding the same class of secret (a 32-byte ed25519 seed
/// as 64 hex chars) and had neither. The `Debug` derive was the sharper half:
/// a single `{file:?}` in a diagnostic — or a `#[derive(Debug)]` on any struct
/// that came to contain one — renders the operator's private key into a log
/// line.
#[derive(Serialize, Deserialize)]
pub(crate) struct IdentityFile {
    pub(crate) operator_id_hex: String,
    pub(crate) seed_hex: String,
    pub(crate) public_key_hex: String,
    pub(crate) created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

impl Drop for IdentityFile {
    fn drop(&mut self) {
        // The operator identity seed rides in `seed_hex`; scrub it on drop so
        // no copy is left in freed memory (mirrors `OrgKeyFile`).
        zeroize_string(&mut self.seed_hex);
    }
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

/// Write `bytes` into the already-created `f` and fsync it, REMOVING `tmp` if
/// any step fails.
///
/// §11, second half. The first pass cleaned up only after a failed RENAME, but
/// `OpenOptions::open` creates the file before a single byte is written — so a
/// disk-full, quota, read-only-remount or fsync failure stranded a partial
/// org-root or node-identity seed on disk. `write_all` can fail mid-buffer, so
/// the residue may be a PREFIX of the seed file rather than nothing at all.
///
/// Split out from the `spawn_blocking` closure so the failure can be driven
/// deterministically in a test: hand it a read-only handle and `write_all`
/// fails on every platform. There is no portable way to force that from
/// inside the closure.
fn write_sync_or_remove(mut f: std::fs::File, tmp: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let written = std::io::Write::write_all(&mut f, bytes).and_then(|()| {
        // Without the fsync a crash after this returns could still lose the
        // bytes; a sync failure means they are not durable.
        f.sync_all()
    });
    let Err(e) = written else {
        return Ok(());
    };
    // Close before unlinking — Windows refuses to remove an open file.
    drop(f);
    match std::fs::remove_file(tmp) {
        Ok(()) => {}
        Err(rm) if rm.kind() == std::io::ErrorKind::NotFound => {}
        // Loud, with the exact path — the same discipline the rename-failure
        // path uses. A silent best-effort removal would be the original defect
        // in a new place.
        Err(rm) => eprintln!(
            "warning: failed to remove partially-written seed temp {}: {rm};              REMOVE IT MANUALLY — it may contain key material.",
            tmp.display()
        ),
    }
    Err(e)
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
    // §10: the copy MUST scrub. Callers wrap the serialized seed in
    // `ScrubbedString` precisely so every exit path zeroes it — and a plain
    // `bytes.to_vec()` here defeated that ceremony entirely, leaving the org
    // root seed (or a node identity seed) in freed heap for a core dump, a
    // swapped page, or heap reuse to disclose. `ScrubbedBytes` zeroes on drop,
    // including when the blocking task unwinds.
    let bytes_owned = ScrubbedBytes::new(bytes.to_vec());

    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let f = opts.open(&tmp_owned)?;
        write_sync_or_remove(f, &tmp_owned, bytes_owned.as_slice())
    })
    .await
    .map_err(|e| generic(format!("seed-write task panicked: {e}")))?
    .map_err(|e| {
        generic(format!(
            "failed to write identity tmp {}: {e}",
            tmp.display()
        ))
    })?;

    // §11: the temp holds SEED MATERIAL, so a failed rename must not orphan it
    // silently. The previous cleanup was a detached `tokio::spawn` — in a
    // one-shot CLI the error propagates straight out of `dispatch` and the
    // process exits, so that task was almost never scheduled and the seed was
    // left on disk with only a "rename failed" message that never mentioned
    // it. Await the removal, and if it fails say so LOUDLY with the exact path
    // so the operator can remove it by hand.
    if let Err(e) = tokio::fs::rename(tmp, final_path).await {
        match tokio::fs::remove_file(tmp).await {
            Ok(()) => {}
            Err(rm) if rm.kind() == std::io::ErrorKind::NotFound => {}
            Err(rm) => eprintln!(
                "warning: failed to remove seed-bearing temp file {}: {rm};                  REMOVE IT MANUALLY — it contains key material.",
                tmp.display()
            ),
        }
        return Err(generic(format!(
            "rename identity tmp {} -> {}: {e}",
            tmp.display(),
            final_path.display()
        )));
    }
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

/// The default identity path, or `None` when the platform config directory
/// cannot be resolved (§19).
///
/// Deliberately NOT falling back to `PathBuf::from(".")`. This file holds the
/// operator's ed25519 SEED; a CWD fallback silently writes it wherever the
/// operator happened to be standing — a git checkout, an archived CI
/// workspace, a shared build directory. On Windows the file then inherits that
/// directory's DACL and `enforce_strict_permissions` is a no-op, so a
/// world-readable CWD yields a world-readable private key with no warning.
///
/// `node.rs::default_authority_dir` was hardened this way for `owner-audience.key`;
/// the same argument applies at least as strongly here, and `config.rs` already
/// used the `Option` pattern — it simply was not propagated.
fn default_identity_path(operator_id: u64) -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join("net-mesh")
            .join("identities")
            .join(format!("operator-0x{operator_id:016x}.toml")),
    )
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

    /// §11 — a failed rename must not orphan the seed-bearing temp file.
    ///
    /// The temp holds raw key material (an org root seed via `net org keygen`,
    /// or a node identity seed). The previous cleanup was a detached
    /// `tokio::spawn`, and this is a one-shot CLI: the error propagates
    /// straight out of `dispatch` and the process exits, so that task was
    /// almost never scheduled. The operator saw only "rename failed" — nothing
    /// said a seed file had been left behind.
    ///
    /// The rename is forced to fail by pointing `final_path` at an existing
    /// DIRECTORY, which is portable (EISDIR on Unix, ERROR_ACCESS_DENIED on
    /// Windows) and needs no permission games.
    ///
    /// Red-witness: restoring the detached `tokio::spawn` cleanup leaves the
    /// temp on disk and fails the assertion.
    #[tokio::test]
    async fn a_failed_rename_removes_the_seed_bearing_temp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tmp = dir.path().join("identity.tmp");
        // An existing directory can never be replaced by a file rename.
        let occupied = dir.path().join("occupied");
        std::fs::create_dir(&occupied).expect("mkdir");

        let err = write_identity_atomically(&tmp, &occupied, b"seed-material")
            .await
            .expect_err("renaming onto a directory must fail");
        assert!(
            format!("{err}").contains("rename identity tmp"),
            "the error must name the failed step; got: {err}",
        );
        assert!(
            !tmp.exists(),
            "the seed-bearing temp {} must not be left on disk",
            tmp.display(),
        );
    }

    /// §11 (second half) — a failed WRITE or SYNC must not orphan the temp.
    ///
    /// The first §11 fix cleaned up only after a failed RENAME. But
    /// `OpenOptions::open` creates the file before a byte is written, so
    /// disk-full, quota, read-only-remount and fsync failures still stranded
    /// seed material — and because `write_all` can fail mid-buffer, the
    /// residue may be a PREFIX of the seed file rather than nothing.
    ///
    /// Driven deterministically by handing the writer a READ-ONLY handle to an
    /// existing file: `write_all` then fails on every platform, though the
    /// error differs — POSIX `write(2)` on an `O_RDONLY` fd is EBADF (no
    /// `ErrorKind` mapping, so `Uncategorized`), while Windows `WriteFile`
    /// without `GENERIC_WRITE` is `ERROR_ACCESS_DENIED` (`PermissionDenied`).
    /// Forcing a genuine ENOSPC/EIO from inside the blocking closure is not
    /// portable, which is why the cleanup was extracted into
    /// `write_sync_or_remove`.
    ///
    /// Red-witness: deleting the removal block leaves the file on disk and
    /// fails the second assertion.
    #[test]
    fn a_failed_write_removes_the_partial_seed_temp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tmp = dir.path().join("identity.tmp");

        // Stand in for a temp the writer had already created and partially
        // filled before the failure.
        std::fs::write(&tmp, b"partial-seed-material").expect("seed the temp");
        let read_only = std::fs::File::open(&tmp).expect("open read-only");

        let err = write_sync_or_remove(read_only, &tmp, b"the-real-seed")
            .expect_err("writing through a read-only handle must fail");
        #[cfg(unix)]
        assert_eq!(
            err.raw_os_error(),
            Some(9), // EBADF — write(2) on an O_RDONLY fd; same value on Linux/macOS/BSDs
            "precondition: the failure is the write itself, not something else",
        );
        #[cfg(windows)]
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::PermissionDenied,
            "precondition: the failure is the write itself, not something else",
        );
        assert!(
            !tmp.exists(),
            "a partially-written seed temp must be removed on write failure",
        );
    }

    /// The success path leaves the file in place and reports Ok — the cleanup
    /// must not fire on a healthy write.
    #[test]
    fn a_successful_write_keeps_the_temp_for_the_rename() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tmp = dir.path().join("identity.tmp");
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .expect("create");
        write_sync_or_remove(f, &tmp, b"seed-material").expect("healthy write succeeds");
        assert_eq!(
            std::fs::read(&tmp).expect("read back"),
            b"seed-material",
            "the payload is intact and the temp survives for the rename",
        );
    }

    /// The happy path still writes, renames, and leaves no temp behind.
    #[tokio::test]
    async fn a_successful_write_renames_and_leaves_no_temp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tmp = dir.path().join("identity.tmp");
        let final_path = dir.path().join("identity.toml");

        write_identity_atomically(&tmp, &final_path, b"seed-material")
            .await
            .expect("write succeeds");
        assert_eq!(
            std::fs::read(&final_path).expect("read final"),
            b"seed-material",
            "the payload landed intact",
        );
        assert!(!tmp.exists(), "no temp is left behind on success");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&final_path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o077,
                0,
                "seed file must be owner-only, got {mode:o}"
            );
        }
    }

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
