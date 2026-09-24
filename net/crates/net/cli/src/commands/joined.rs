//! Run a normal consumer (`wrap`, `mcp serve`) as an ENROLLED device
//! (NET_CLI_PLAN_V3 V3-5, E8/E16): `--joined <state-dir>` loads what `join`
//! installed there — the device identity, the mesh PSK and the contact of
//! the node it enrolled with — and attaches to that contact (direct first,
//! then the relay it named). No invite, root key or PSK is handed over on
//! the command line.
//!
//! By default the consumer attaches to the node it enrolled with. An
//! explicit `--node-addr/--node-pubkey/--node-id` names another peer of the
//! same mesh to attach to instead (for example a provider device), still as
//! the enrolled device, still with the enrolled PSK.
//!
//! The join store has one owner at a time: while `up` runs on that state
//! it holds the store, and `--joined` is refused (stop it with `down`
//! first). The consumer keeps the store for its whole lifetime, so `up`
//! cannot start on the same identity underneath it either.

use std::path::Path;
use std::time::Duration;

use net::adapter::net::behavior::enrollment_storage::StorageError;
use net_sdk::enrollment::bundle::MeshContact;
use net_sdk::enrollment::device::{DeviceJoin, DeviceJoinError};

use crate::error::{connection_failure, generic, invalid_args, CliError};

/// How long the first attach may take (direct, then the relay).
const ATTACH_WAIT: Duration = Duration::from_secs(20);

/// The join store held for the consumer's lifetime.
pub(crate) struct JoinedGuard {
    _join: DeviceJoin,
}

/// Validate the peer flags next to `--joined`: the PSK always comes from
/// the join (`--psk-hex` is refused), and a peer is either the enrolled
/// contact (no peer flags) or named completely. Returns the named peer.
pub(crate) fn peer_override(
    args: &crate::commands::aggregator::RemoteAttachArgs,
) -> Result<Option<MeshContact>, CliError> {
    if args.psk_hex.is_some() {
        return Err(invalid_args(
            "--joined supplies the PSK from the join; drop --psk-hex",
        ));
    }
    if args.inspect_target {
        return Err(invalid_args(
            "--inspect-target is not available with --joined",
        ));
    }
    match (&args.node_addr, &args.node_pubkey, &args.remote_node_id) {
        (None, None, None) => Ok(None),
        (Some(addr), Some(pubkey), Some(node_id)) => {
            let addr: std::net::SocketAddr = addr
                .parse()
                .map_err(|e| invalid_args(format!("--node-addr `{addr}`: {e}")))?;
            let noise_pubkey = crate::parsers::hex_decode_32(pubkey)
                .map_err(|e| invalid_args(format!("--node-pubkey: {e}")))?;
            let node_id = crate::parsers::parse_u64_flexible(node_id)
                .map_err(|e| invalid_args(format!("--node-id: {e}")))?;
            Ok(Some(MeshContact {
                addr: Some(addr),
                noise_pubkey,
                node_id,
                relay: None,
            }))
        }
        _ => Err(invalid_args(
            "a peer named next to --joined needs --node-addr, --node-pubkey and --node-id",
        )),
    }
}

/// Open the join at `state_root`, build a mesh bound to `bind` under the
/// enrolled identity and PSK, and attach it to the enrolled contact.
/// Returns the attached mesh, the path it attached over and the held store.
pub(crate) async fn attach(
    state_root: &Path,
    bind: &str,
    peer: Option<MeshContact>,
) -> Result<(net_sdk::Mesh, &'static str, JoinedGuard), CliError> {
    let join_dir = state_root.join(super::enrollment::JOIN_SUBDIR);
    if !join_dir.exists() {
        return Err(invalid_args(format!(
            "{} has not joined a mesh (no join state)",
            state_root.display()
        )));
    }
    let jd = join_dir.clone();
    let join = tokio::task::spawn_blocking(move || DeviceJoin::open(&jd))
        .await
        .map_err(|e| generic(format!("join state task failed: {e}")))?
        .map_err(|e| match e {
            DeviceJoinError::Storage(StorageError::Busy) => invalid_args(format!(
                "the join at {} is in use (is `net-mesh up` running on it? stop it with \
                 `net-mesh down` first)",
                state_root.display()
            )),
            other => generic(format!("join state {}: {other}", join_dir.display())),
        })?;
    if let Some(at) = join.left_at() {
        return Err(invalid_args(format!(
            "this device left the mesh at unix {at}; nothing to run as"
        )));
    }
    let bundle = join.bundle().ok_or_else(|| {
        invalid_args("the join is not complete (no delivered bundle); finish `net-mesh join` first")
    })?;
    let bind = crate::context::parse_bind_literal(bind)?;
    let mut psk = *bundle.psk().expose_bytes();
    let built = net_sdk::MeshBuilder::new(&bind.to_string(), &psk)
        .map_err(|e| invalid_args(format!("mesh bind {bind}: {e}")));
    crate::secret::zeroize_slice(&mut psk);
    let mesh = built?
        .identity(join.identity().clone())
        .build()
        .await
        .map_err(|e| connection_failure(format!("mesh start on {bind}: {e}")))?;
    mesh.start();
    let contact = peer.unwrap_or_else(|| bundle.contact().clone());
    let path = super::enrollment::attach_contact(mesh.node(), &contact, ATTACH_WAIT)
        .await
        .map_err(|e| connection_failure(format!("attach to the enrolled node failed: {e}")))?;
    Ok((mesh, path, JoinedGuard { _join: join }))
}
