//! Profile-file parsing + env-var fallback.
//!
//! Plan §10: a TOML config at `$XDG_CONFIG_HOME/net/config.toml`
//! with named profiles. `--config` / `--profile` / `NET_*` env
//! vars resolve which profile applies; every individual subcommand
//! flag overrides the profile value at the call site.
//!
//! Phase 1 keeps this minimal — the file is optional and the
//! binary works without one. The struct is shaped so a future
//! `endpoint` / `ice_signature_threshold` knob slots in without
//! a breaking change.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

/// Set by `main` from `--insecure-config-permissions` before any
/// subcommand runs. See [`set_insecure_permissions`].
static INSECURE_PERMISSIONS: AtomicBool = AtomicBool::new(false);

/// Allow [`ConfigFile::load`] to read a profile that is
/// group/world-accessible or owned by another user.
///
/// A process-wide switch rather than an argument threaded through
/// [`ConfigFile::load`] because the profile is resolved from
/// ~35 call sites across the subcommands, all of which take the config
/// path from the same global argv. Adding a parameter to each would put
/// the decision in 35 places when it is made in one — and a security
/// control is worse, not better, for being restatable per call site.
/// `main` sets it once, before dispatch; nothing else writes it.
///
/// The name is deliberately distinct from the per-command
/// `--insecure-permissions` (which downgrades the gate on the *key
/// file* that command names). These are different files with different
/// consequences, and an operator should not be able to lower the guard
/// on the mesh PSK by reaching for a flag they meant to point at an
/// identity file.
pub fn set_insecure_permissions(allow: bool) {
    INSECURE_PERMISSIONS.store(allow, Ordering::Relaxed);
}

/// Whether the profile permission gate has been waived process-wide.
pub fn insecure_permissions() -> bool {
    INSECURE_PERMISSIONS.load(Ordering::Relaxed)
}

/// Top-level config file shape. The `default` table is the
/// implicit profile when `--profile` is omitted; `profiles.*`
/// adds named profiles selectable via `--profile` /
/// `$NET_MESH_PROFILE`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ConfigFile {
    #[serde(default)]
    pub default: Profile,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

/// Per-profile knobs. Every field is optional; the CLI fills
/// substrate defaults when absent.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Profile {
    /// Connection target. `in-process` is the only supported
    /// value today (Phase 1); `tcp://host:port` is a Phase 5
    /// addition gated on substrate remote-attach work.
    #[serde(default)]
    pub endpoint: Option<String>,

    /// Path to the operator identity file (the TOML format
    /// authored by `net-mesh identity generate`).
    #[serde(default)]
    pub identity: Option<PathBuf>,

    /// Path to the NetDB store. Defaults to
    /// `$XDG_DATA_HOME/net/netdb.redex` when absent.
    #[serde(default)]
    pub netdb: Option<PathBuf>,

    /// Default per-call timeout in milliseconds. The global
    /// `--timeout` flag overrides this; absent → 30s.
    #[serde(default)]
    pub default_timeout_ms: Option<u64>,

    /// Advisory ICE signature threshold for `net-mesh ice` previews.
    /// The substrate-side `AdminVerifier` is the source of truth;
    /// this is only a UI hint for the confirm gate.
    #[serde(default)]
    pub ice_signature_threshold: Option<usize>,

    /// Pre-shared key for handshaking with a remote daemon, as
    /// 64 hex chars (`0x`-prefix optional). Required by every
    /// subcommand that uses `--node-addr`. Profile-level so an
    /// operator who always talks to the same daemon can omit
    /// `--psk-hex` from the call site.
    #[serde(default)]
    pub psk_hex: Option<String>,

    /// Default remote daemon `IP:port`. Subcommand `--node-addr`
    /// overrides; omitted when only in-process supervisor work
    /// happens against this profile.
    #[serde(default)]
    pub node_addr: Option<String>,

    /// Default remote daemon's Noise public key (64 hex chars).
    /// Subcommand `--node-pubkey` overrides.
    #[serde(default)]
    pub node_pubkey: Option<String>,

    /// Default remote daemon's `node_id` (decimal or `0x`-prefixed
    /// hex). Subcommand `--node-id` overrides.
    #[serde(default)]
    pub node_id: Option<String>,
}

impl ConfigFile {
    /// Resolve the named profile (or `default` when none named).
    /// Returns an empty profile when the named one is absent —
    /// the CLI degrades gracefully when the file is partial.
    pub fn profile(&self, name: &str) -> Profile {
        if name == "default" {
            return self.default.clone();
        }
        self.profiles.get(name).cloned().unwrap_or_default()
    }

    /// Load from disk. Returns `Ok(default)` when the file is
    /// missing — the binary is usable without a config.
    ///
    /// Honours the process-wide override set by
    /// [`set_insecure_permissions`], i.e. the CLI's
    /// `--insecure-config-permissions`. Embedders that want to decide
    /// per call should use [`Self::load_with`] instead.
    pub async fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        Self::load_with(path, insecure_permissions()).await
    }

    /// [`Self::load`], with `allow_insecure` skipping the secret-file
    /// permission gate.
    ///
    /// SEC-05 / LINUX-02: this file can hold `psk_hex`, the
    /// mesh-membership root secret, and it had no permission check at
    /// all — the CLI would read a world-readable profile and attach to
    /// the mesh without a word. Another local user reading it gets the
    /// PSK plus the bootstrap address, pubkey and node id sitting
    /// beside it: everything needed to join.
    ///
    /// The gate validates the opened descriptor (regular file, owned
    /// by this user, no group/other access), which also closes the
    /// swap-between-check-and-read window a path-based check leaves.
    ///
    /// A profile with no `psk_hex` is not secret-bearing, but the gate
    /// necessarily runs before parsing and so cannot know that yet —
    /// and a profile that gains a PSK later should not silently lose
    /// the protection.
    pub async fn load_with(path: Option<&Path>, allow_insecure: bool) -> Result<Self, ConfigError> {
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => match default_path() {
                Some(p) => p,
                None => return Ok(Self::default()),
            },
        };
        let gate_path = path.clone();
        let gated = tokio::task::spawn_blocking(move || {
            ::net::adapter::net::secret_file::read_secret_file_to_string(&gate_path, allow_insecure)
        })
        .await
        .map_err(|e| ConfigError::Io {
            path: path.clone(),
            source: std::io::Error::other(e),
        })?;
        let text = match gated {
            Ok(t) => t,
            // A missing config is not an error — the binary is usable
            // without one.
            Err(::net::adapter::net::secret_file::SecretFileError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(Self::default())
            }
            Err(::net::adapter::net::secret_file::SecretFileError::Io { source, .. }) => {
                return Err(ConfigError::Io {
                    path: path.clone(),
                    source,
                })
            }
            Err(e) => {
                return Err(ConfigError::Permissions {
                    path: path.clone(),
                    source: e,
                })
            }
        };
        // SEC-06 / LINUX-03. The `toml::de::Error` is deliberately
        // dropped rather than carried: its `Display` embeds the
        // offending source line, and this file holds `psk_hex` — the
        // mesh-membership root secret. A malformed PSK line (operator
        // typo, truncated write, templating failure) would otherwise
        // be reproduced verbatim wherever this error is printed:
        // stderr, shell scrollback, CI logs, journald. Those readers
        // are a far wider set than the config file's.
        //
        // Same sanitized shape the org and subnet key loaders already
        // use. The line/column is the cost; a profile is small enough
        // to eyeball, and no diagnostic is worth leaking the mesh PSK.
        toml::from_str(&text).map_err(|_| ConfigError::Parse { path: path.clone() })
    }
}

/// `$XDG_CONFIG_HOME/net-mesh/config.toml` — used when `--config`
/// is absent. Returns `None` if dirs can't resolve a config home
/// (e.g. in restricted CI environments without `$HOME`).
pub fn default_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("net-mesh").join("config.toml"))
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The profile file was refused before being read: it is not a
    /// regular file, is owned by another user, or is group/world
    /// accessible. It can hold the mesh PSK.
    #[error("{source}")]
    Permissions {
        path: PathBuf,
        #[source]
        source: ::net::adapter::net::secret_file::SecretFileError,
    },

    /// The profile file is not valid TOML.
    ///
    /// Carries no `toml::de::Error` on purpose — see the call site in
    /// [`ConfigFile::load`]. The parser's message embeds the offending
    /// source line, and this file holds `psk_hex`, so the error is
    /// reported as a category and a path only.
    #[error("config file at {path} is not valid TOML (kind: parse_error)")]
    Parse { path: PathBuf },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Make a freshly-written config owner-only.
    ///
    /// These tests exercise parse-error redaction, not permissions, but
    /// `load` runs the SEC-05 secret-file gate first — the file holds
    /// `psk_hex`, so a group-readable one is refused before parsing.
    /// `std::fs::write` leaves the umask default (typically `0o644`),
    /// which the gate correctly rejects.
    ///
    /// A no-op off Unix, matching the gate: `std::fs` exposes no usable
    /// NTFS ACL view, so there the gate warns rather than enforcing.
    /// That asymmetry is why this defect passed on Windows and failed
    /// on CI.
    fn make_owner_only(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .expect("chmod 600");
        }
        #[cfg(not(unix))]
        let _ = path;
    }

    /// The SEC-05 gate has to be reachable from the tool operators
    /// actually run.
    ///
    /// `load_with(.., true)` existed but had no caller passing `true`,
    /// so the CLI's only documented escape hatch was an in-process Rust
    /// API. The aggregator daemon got `--insecure-permissions`; the CLI
    /// got nothing, and every pre-existing `0644` profile — the umask
    /// default, and the CLI never writes this file itself — became an
    /// unoverridable hard failure on upgrade.
    ///
    /// Unix-only: off Unix the gate warns rather than enforcing, so
    /// there is no refusal to override.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_world_readable_profile_is_refused_but_the_override_admits_it() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("net-sec05-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[default]\npsk_hex = \"abcd\"\n").expect("write config");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod 644");

        match ConfigFile::load(Some(&path)).await {
            Err(ConfigError::Permissions { source, .. }) => {
                assert_eq!(source.kind(), "permissive_mode", "got {source}");
            }
            Err(other) => panic!("expected a permission refusal, got {other:?}"),
            Ok(_) => panic!("a world-readable profile holding a PSK was read"),
        }

        // The flag `main` sets from `--insecure-config-permissions`.
        // Restored below; leaking it would only *relax* the gate for
        // the other tests in this binary, all of which write owner-only
        // files and would pass either way — but a test that leaves
        // process state behind is a test that makes the next failure
        // harder to read.
        let previously = insecure_permissions();
        set_insecure_permissions(true);
        let loaded = ConfigFile::load(Some(&path)).await;
        set_insecure_permissions(previously);

        let cfg = loaded.expect("--insecure-config-permissions must admit the file");
        assert_eq!(
            cfg.profile("default").psk_hex.as_deref(),
            Some("abcd"),
            "the override admitted the file but did not actually parse it"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SEC-06 / LINUX-03 witness. A malformed `psk_hex` line must not
    /// be reproduced in the error the CLI prints.
    ///
    /// `toml::de::Error`'s `Display` embeds the offending source line.
    /// This file holds the mesh-membership root secret, and the error
    /// is printed to stderr — which reaches shell scrollback, CI job
    /// logs and journald, all read by more people than the config file
    /// is.
    #[tokio::test]
    async fn a_malformed_psk_line_is_not_reproduced_in_the_parse_error() {
        const SENTINEL: &str = "PSK_SENTINEL_0123456789abcdef";

        let dir = std::env::temp_dir().join(format!("net-sec06-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");
        // Trailing garbage after a valid string: the parser reports
        // the line, which carries the secret.
        std::fs::write(
            &path,
            format!("listen = \"127.0.0.1:0\"\npsk_hex = \"{SENTINEL}\" trailing\n"),
        )
        .expect("write config");
        make_owner_only(&path);

        let err = ConfigFile::load(Some(&path))
            .await
            .expect_err("malformed TOML must fail to parse");

        // Both the Display and the Debug rendering — an operator may
        // see either, and `{:?}` on a `#[source]`-carrying error walks
        // the chain.
        let rendered = format!("{err} | {err:?}");
        assert!(
            !rendered.contains(SENTINEL),
            "the PSK was reproduced in the parse error: {rendered}"
        );
        // Still useful: names the file so the operator knows where to
        // look, and the category so they know what kind of failure it
        // was.
        assert!(
            rendered.contains("config.toml") && rendered.contains("parse_error"),
            "the sanitized error dropped the path or the category: {rendered}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A well-formed profile still loads — a redaction that broke
    /// parsing would pass the test above for the wrong reason.
    #[tokio::test]
    async fn a_well_formed_profile_still_parses() {
        let dir = std::env::temp_dir().join(format!("net-sec06-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[default]\npsk_hex = \"abcd\"\n").expect("write config");
        make_owner_only(&path);

        let cfg = ConfigFile::load(Some(&path))
            .await
            .expect("valid TOML loads");
        assert_eq!(cfg.profile("default").psk_hex.as_deref(), Some("abcd"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
