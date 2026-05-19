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
    /// authored by `net identity generate`).
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

    /// Advisory ICE signature threshold for `net ice` previews.
    /// The substrate-side `AdminVerifier` is the source of truth;
    /// this is only a UI hint for the confirm gate.
    #[serde(default)]
    pub ice_signature_threshold: Option<usize>,
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
            Ok(s) => toml::from_str(&s).map_err(|e| ConfigError::Parse {
                path: path.clone(),
                source: e,
            }),
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

    #[error("config file at {path} failed to parse: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}
