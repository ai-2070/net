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
//! # Remote-attach
//!
//! Subcommands that need to call a remote daemon (today: every
//! `net aggregator` write/query verb) build through
//! [`CliContext::build_with_remote`] and pass a
//! [`RemoteAttach`]. The context boots a local ephemeral
//! `MeshNode` on `127.0.0.1:0`, completes the Noise handshake
//! with the remote, starts the receive loop, and exposes the
//! `Arc<MeshNode>` via [`CliContext::mesh_node`] for typed
//! clients (`RegistryClient` / `FoldQueryClient`) to route
//! through.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use net_sdk::deck::{DeckClient, OperatorIdentity};
use net_sdk::meshos::{EntityKeypair, LoggingDispatcher, MeshOsConfig, MeshOsDaemonSdk};

use crate::config::Profile;
use crate::error::{connection_failure, generic, invalid_args, sdk, CliError};
use crate::parsers::{hex_decode_32, parse_u64_flexible};

/// Resolved remote-attach target. Built from subcommand flags +
/// profile fallbacks via [`resolve_remote_attach`]. Carrying it
/// as a typed struct (rather than three optional strings) makes
/// the bootstrap path infallible — every field is validated at
/// resolve time.
#[derive(Debug, Clone)]
pub struct RemoteAttach {
    pub addr: SocketAddr,
    pub public_key: [u8; 32],
    pub node_id: u64,
    pub psk: [u8; 32],
}

/// Live substrate context wrapping the SDK + DeckClient.
pub struct CliContext {
    _sdk: MeshOsDaemonSdk,
    deck: Arc<DeckClient>,
    identity: OperatorIdentity,
    /// Connected `MeshNode` when the context was built via
    /// [`CliContext::build_with_remote`]. `None` for in-process
    /// (read-local) subcommands. A-2 onward wires call sites.
    #[allow(dead_code)]
    mesh_node: Option<Arc<net_sdk::MeshNode>>,
    /// Held so the underlying UDP socket + receive loop keep
    /// running for the lifetime of the context. Dropped at end-
    /// of-command alongside `_sdk`.
    _mesh: Option<net_sdk::Mesh>,
}

impl CliContext {
    pub fn deck(&self) -> Arc<DeckClient> {
        Arc::clone(&self.deck)
    }

    pub fn identity(&self) -> &OperatorIdentity {
        &self.identity
    }

    /// Connected `MeshNode` for typed RPC clients. `None` when
    /// the context was built without a remote-attach target.
    /// A-2 onward consumes this from the aggregator subcommands.
    #[allow(dead_code)]
    pub fn mesh_node(&self) -> Option<Arc<net_sdk::MeshNode>> {
        self.mesh_node.clone()
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
        Self::build_inner(profile, identity_override, node_id, require_identity, None).await
    }

    /// Like [`Self::build`] but also handshakes with a remote
    /// daemon and exposes the connected `Arc<MeshNode>` via
    /// [`Self::mesh_node`]. Used by subcommands that route typed
    /// RPC clients (`RegistryClient` / `FoldQueryClient`) to a
    /// target node. A-2 onward consumes this from the
    /// aggregator subcommands.
    #[allow(dead_code)]
    pub async fn build_with_remote(
        profile: &Profile,
        identity_override: Option<&Path>,
        node_id: u64,
        require_identity: bool,
        remote: RemoteAttach,
    ) -> Result<Self, CliError> {
        Self::build_inner(
            profile,
            identity_override,
            node_id,
            require_identity,
            Some(remote),
        )
        .await
    }

    async fn build_inner(
        profile: &Profile,
        identity_override: Option<&Path>,
        node_id: u64,
        require_identity: bool,
        remote: Option<RemoteAttach>,
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

        // Remote-attach branch: stand up a local ephemeral mesh
        // on 127.0.0.1:0, handshake with the target, start the
        // receive loop, hand back the Arc<MeshNode>.
        let (mesh_node, _mesh) = match remote {
            Some(remote) => {
                let mesh = build_remote_mesh(remote).await?;
                let node = mesh.node_arc();
                (Some(node), Some(mesh))
            }
            None => (None, None),
        };

        Ok(Self {
            _sdk: sdk,
            deck,
            identity,
            mesh_node,
            _mesh,
        })
    }
}

/// Stand up a local ephemeral mesh that has handshaked with the
/// supplied remote target. Internal — used by
/// [`CliContext::build_with_remote`].
async fn build_remote_mesh(remote: RemoteAttach) -> Result<net_sdk::Mesh, CliError> {
    let mesh = net_sdk::MeshBuilder::new("127.0.0.1:0", &remote.psk)
        .map_err(|e| connection_failure(format!("mesh builder rejected bind address: {e}")))?
        .build()
        .await
        .map_err(|e| connection_failure(format!("mesh build failed: {e}")))?;
    mesh.connect(&remote.addr.to_string(), &remote.public_key, remote.node_id)
        .await
        .map_err(|e| {
            connection_failure(format!(
                "handshake with {} (node_id={}) failed: {e}",
                remote.addr, remote.node_id
            ))
        })?;
    mesh.start();
    Ok(mesh)
}

/// Resolve a [`RemoteAttach`] from subcommand-level flags +
/// profile fallbacks. Returns `Ok(Some(_))` when the operator
/// supplied (or has configured) at least one remote-attach
/// signal; `Ok(None)` when the call is in-process.
///
/// Validation is up-front: a partially-specified remote (e.g.
/// `--node-addr` without `--node-pubkey`) is a typed error
/// rather than a runtime handshake failure. A-2 onward consumes
/// this from the aggregator subcommands.
#[allow(dead_code)]
pub fn resolve_remote_attach(
    profile: &Profile,
    addr: Option<&str>,
    pubkey: Option<&str>,
    node_id: Option<&str>,
    psk_hex: Option<&str>,
) -> Result<Option<RemoteAttach>, CliError> {
    let addr_str = addr.or(profile.node_addr.as_deref());
    let pubkey_str = pubkey.or(profile.node_pubkey.as_deref());
    let node_id_str = node_id.or(profile.node_id.as_deref());
    let psk_str = psk_hex.or(profile.psk_hex.as_deref());

    let any_set = addr_str.is_some()
        || pubkey_str.is_some()
        || node_id_str.is_some()
        || psk_str.is_some();
    if !any_set {
        return Ok(None);
    }

    let addr_str = addr_str.ok_or_else(|| {
        invalid_args(
            "remote-attach requires --node-addr (or `node_addr` in the profile)",
        )
    })?;
    let pubkey_str = pubkey_str.ok_or_else(|| {
        invalid_args(
            "remote-attach requires --node-pubkey (or `node_pubkey` in the profile)",
        )
    })?;
    let node_id_str = node_id_str.ok_or_else(|| {
        invalid_args(
            "remote-attach requires --node-id (or `node_id` in the profile)",
        )
    })?;
    let psk_str = psk_str.ok_or_else(|| {
        invalid_args(
            "remote-attach requires --psk-hex (or `psk_hex` in the profile)",
        )
    })?;

    let addr: SocketAddr = addr_str.parse().map_err(|e| {
        invalid_args(format!(
            "--node-addr `{addr_str}` is not a valid IP:port: {e}"
        ))
    })?;
    let public_key = hex_decode_32(pubkey_str)
        .map_err(|e| invalid_args(format!("--node-pubkey: {e}")))?;
    let node_id =
        parse_u64_flexible(node_id_str).map_err(|e| invalid_args(format!("--node-id: {e}")))?;
    let psk = hex_decode_32(psk_str).map_err(|e| invalid_args(format!("--psk-hex: {e}")))?;

    Ok(Some(RemoteAttach {
        addr,
        public_key,
        node_id,
        psk,
    }))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ExitCodeKind;

    fn valid_pubkey() -> &'static str {
        "0101010101010101010101010101010101010101010101010101010101010101"
    }

    fn valid_psk() -> &'static str {
        "0x4242424242424242424242424242424242424242424242424242424242424242"
    }

    #[test]
    fn resolve_returns_none_when_nothing_set() {
        let profile = Profile::default();
        let out = resolve_remote_attach(&profile, None, None, None, None).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn resolve_pulls_full_target_from_flags() {
        let profile = Profile::default();
        let out = resolve_remote_attach(
            &profile,
            Some("127.0.0.1:51820"),
            Some(valid_pubkey()),
            Some("0x42"),
            Some(valid_psk()),
        )
        .unwrap()
        .expect("remote-attach resolved");
        assert_eq!(out.addr.port(), 51820);
        assert_eq!(out.node_id, 0x42);
        assert_eq!(out.public_key[0], 0x01);
        assert_eq!(out.psk[0], 0x42);
    }

    #[test]
    fn flags_override_profile_defaults() {
        let profile = Profile {
            node_addr: Some("10.0.0.1:1".into()),
            node_pubkey: Some(valid_pubkey().into()),
            node_id: Some("1".into()),
            psk_hex: Some(valid_psk().into()),
            ..Profile::default()
        };
        let out = resolve_remote_attach(&profile, Some("127.0.0.1:9999"), None, None, None)
            .unwrap()
            .expect("resolved");
        assert_eq!(out.addr.port(), 9999);
    }

    #[test]
    fn missing_pubkey_is_typed_invalid_args() {
        let profile = Profile::default();
        let err = resolve_remote_attach(
            &profile,
            Some("127.0.0.1:1"),
            None,
            Some("1"),
            Some(valid_psk()),
        )
        .expect_err("should reject partial remote spec");
        assert_eq!(err.kind(), ExitCodeKind::InvalidArgs);
    }

    #[test]
    fn bad_pubkey_length_is_typed_invalid_args() {
        let profile = Profile::default();
        let err = resolve_remote_attach(
            &profile,
            Some("127.0.0.1:1"),
            Some("0xaa"),
            Some("1"),
            Some(valid_psk()),
        )
        .expect_err("should reject short pubkey");
        assert_eq!(err.kind(), ExitCodeKind::InvalidArgs);
    }

    #[test]
    fn bad_addr_is_typed_invalid_args() {
        let profile = Profile::default();
        let err = resolve_remote_attach(
            &profile,
            Some("not-an-addr"),
            Some(valid_pubkey()),
            Some("1"),
            Some(valid_psk()),
        )
        .expect_err("should reject garbage addr");
        assert_eq!(err.kind(), ExitCodeKind::InvalidArgs);
    }
}
