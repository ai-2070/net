//! Lazy substrate context — config + identity + in-process SDK.
//!
//! Subcommands that touch the substrate call
//! [`CliContext::build`] to spin a `MeshOsDaemonSdk` + a
//! `DeckClient` configured against the resolved profile + the
//! operator identity. The build is one-shot per binary
//! invocation — every subcommand gets its own context (the CLI
//! is single-shot by design; long-running watches reuse the
//! same context for their lifetime).
//!
//! # Phase 1 scope
//!
//! Only the in-process supervisor pattern is supported. The
//! profile's `endpoint` field must be `in-process` (the default
//! when omitted); `tcp://host:port` is a Phase 5 addition gated
//! on substrate remote-attach work.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use net_sdk::deck::{DeckClient, OperatorIdentity};
use net_sdk::meshos::{EntityKeypair, LoggingDispatcher, MeshOsConfig, MeshOsDaemonSdk};

use crate::config::Profile;
use crate::error::{generic, invalid_args, sdk, CliError};

/// Live substrate context wrapping the SDK + DeckClient.
pub struct CliContext {
    _sdk: MeshOsDaemonSdk,
    deck: Arc<DeckClient>,
    identity: OperatorIdentity,
}

impl CliContext {
    pub fn deck(&self) -> Arc<DeckClient> {
        Arc::clone(&self.deck)
    }

    pub fn identity(&self) -> &OperatorIdentity {
        &self.identity
    }

    /// Build a fresh context. Resolves the operator identity
    /// from (in priority order): `identity_override`,
    /// `profile.identity`, an ephemeral random keypair (with a
    /// diagnostic warning on stderr).
    ///
    /// `require_identity = true` refuses the ephemeral-fallback
    /// path; the admin / ICE write surfaces pass this so a missing
    /// identity can't silently sign a commit with a throwaway key
    /// whose operator id no audit consumer will recognize.
    pub async fn build(
        profile: &Profile,
        identity_override: Option<&Path>,
        node_id: u64,
        require_identity: bool,
    ) -> Result<Self, CliError> {
        // Endpoint check — Phase 1 supports only in-process.
        if let Some(endpoint) = profile.endpoint.as_deref() {
            if endpoint != "in-process" {
                return Err(invalid_args(format!(
                    "endpoint `{endpoint}` is not supported in this build; \
                     only `in-process` is available until the substrate \
                     remote-attach surface lands (see NET_CLI_PLAN.md \
                     Phase 5)"
                )));
            }
        }

        // Identity resolution. Generates an ephemeral one as a
        // last resort so read-only subcommands work without
        // ceremony; writes pass `require_identity = true` so the
        // ephemeral branch becomes a typed error instead of a
        // silent warn-and-proceed.
        let keypair = match identity_override.or(profile.identity.as_deref()) {
            Some(path) => load_identity_keypair(path).await?,
            None => {
                if require_identity {
                    return Err(invalid_args(
                        "no operator identity configured; pass --identity <PATH> \
                         or set `identity = \"...\"` under your profile in the \
                         config file. Admin / ICE commits refuse to sign with \
                         an ephemeral keypair.",
                    ));
                }
                tracing::warn!(
                    "no operator identity configured; using an ephemeral \
                     keypair. Run `net identity generate --out <PATH>` and \
                     point your profile at the result for stable operator id."
                );
                EntityKeypair::generate()
            }
        };

        let mut cfg = MeshOsConfig::default();
        cfg.this_node = node_id;
        cfg.tick_interval = Duration::from_millis(250);
        let dispatcher = Arc::new(LoggingDispatcher::new());

        let sdk = MeshOsDaemonSdk::start(cfg, dispatcher);
        let identity = OperatorIdentity::from_keypair(keypair);
        let deck = Arc::new(DeckClient::from_runtime(sdk.runtime(), identity.clone()));

        Ok(Self {
            _sdk: sdk,
            deck,
            identity,
        })
    }
}

pub(crate) async fn load_identity_keypair(path: &Path) -> Result<EntityKeypair, CliError> {
    let text = tokio::fs::read_to_string(path).await.map_err(|e| {
        generic(format!(
            "failed to read identity file {}: {e}",
            path.display()
        ))
    })?;
    let parsed: PartialIdentityFile = toml::from_str(&text).map_err(|e| {
        invalid_args(format!(
            "identity file {} failed to parse: {e}",
            path.display()
        ))
    })?;
    let seed_bytes = hex::decode(&parsed.seed_hex).map_err(|e| {
        invalid_args(format!(
            "identity file {} `seed_hex` is not valid hex: {e}",
            path.display()
        ))
    })?;
    if seed_bytes.len() != 32 {
        return Err(invalid_args(format!(
            "identity file {} `seed_hex` decodes to {} bytes; expected 32",
            path.display(),
            seed_bytes.len()
        )));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&seed_bytes);
    Ok(EntityKeypair::from_bytes(arr))
}

#[derive(serde::Deserialize)]
struct PartialIdentityFile {
    seed_hex: String,
}

/// Resolve the active profile + identity path. Centralises the
/// `--identity` / `profile.identity` / env-var precedence so
/// subcommands don't reimplement it.
pub async fn resolve_profile(
    config_path: Option<&Path>,
    profile_name: &str,
) -> Result<Profile, CliError> {
    let file = crate::config::ConfigFile::load(config_path)
        .await
        .map_err(|e| sdk(format!("config load: {e}")))?;
    Ok(file.profile(profile_name))
}

#[allow(dead_code)]
pub fn resolved_identity_path(profile: &Profile, overide_: Option<&Path>) -> Option<PathBuf> {
    overide_
        .map(Path::to_path_buf)
        .or_else(|| profile.identity.clone())
}
