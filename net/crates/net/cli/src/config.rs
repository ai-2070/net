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

use serde::{Deserialize, Serialize};

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
    pub async fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => match default_path() {
                Some(p) => p,
                None => return Ok(Self::default()),
            },
        };
        match tokio::fs::read_to_string(&path).await {
            // SEC-06 / LINUX-03. The `toml::de::Error` is deliberately
            // dropped rather than carried: its `Display` embeds the
            // offending source line, and this file holds `psk_hex` —
            // the mesh-membership root secret. A malformed PSK line
            // (operator typo, truncated write, templating failure)
            // would otherwise be reproduced verbatim wherever this
            // error is printed: stderr, shell scrollback, CI logs,
            // journald. Those readers are a far wider set than the
            // config file's.
            //
            // Same sanitized shape the org and subnet key loaders
            // already use. The line/column is the cost; a profile is
            // small enough to eyeball, and no diagnostic is worth
            // leaking the mesh PSK.
            Ok(s) => toml::from_str(&s).map_err(|_| ConfigError::Parse { path: path.clone() }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ConfigError::Io {
                path: path.clone(),
                source: e,
            }),
        }
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

        let cfg = ConfigFile::load(Some(&path)).await.expect("valid TOML loads");
        assert_eq!(cfg.profile("default").psk_hex.as_deref(), Some("abcd"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
