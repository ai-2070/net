//! A joined device's channel credential at runtime (NET_CLI_PLAN_V3 V3-2A
//! C3).
//!
//! The join delivered a chain `root → issuer → device` for one canonical
//! channel. Two distinct real paths use it, and status keeps them apart:
//!
//! - **subscribe**: the chain is presented to the issuing node (the
//!   publisher the signed offer names) on every new session and the
//!   publisher's ACK is what `subscribed: true` reports. The ACK is a
//!   routing fact about that node, not a proof of its full identity.
//! - **publish**: the chain is installed as this node's managed publish
//!   chain. It is *credential-ready* only while this node's own channel
//!   config trusts the chain's root (`channel serve` here); no root is ever
//!   installed implicitly. Whether a publish actually clears the local gate
//!   is the publisher application's observation, not this report's.
//!
//! `channel leave` records the departure durably first, then unsubscribes
//! (acknowledged) and removes exactly the installed chain incarnation,
//! evicting its tokens from the cache. If a publish credential for the
//! channel is still visible afterwards (another source this runtime does not
//! control), the stop is reported unconfirmed.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::identity::{TokenChain, TokenScope};
use net::adapter::net::MeshNode;
use net_sdk::enrollment::device::DeviceJoin;
use net_sdk::enrollment::invite::ChannelOffer;
use serde::Serialize;
use serde_json::{json, Value};

/// The durable record that this device left its channel relation (under the
/// state root). Kept apart from the join so the mesh membership is untouched.
pub(crate) const CHANNEL_LEFT_FILE: &str = "channel.left";

/// Retry pause after a refused or failed subscribe.
const SUBSCRIBE_RETRY_SECS: u64 = 5;

/// The channel credential a join delivered.
#[derive(Clone)]
pub(crate) struct ChannelCred {
    pub offer: ChannelOffer,
    pub chain: TokenChain,
}

impl ChannelCred {
    /// The join's channel credential, if it carries one.
    pub(crate) fn of(join: &DeviceJoin) -> Option<Self> {
        let offer = join.invite().channel()?.clone();
        let chain = join.bundle()?.channel_chain()?;
        Some(Self { offer, chain })
    }

    fn expires_at(&self) -> u64 {
        self.chain.tokens.last().map_or(0, |leaf| leaf.not_after)
    }

    fn subscribes(&self) -> bool {
        self.offer.rights.contains(TokenScope::SUBSCRIBE)
    }

    fn publishes(&self) -> bool {
        self.offer.rights.contains(TokenScope::PUBLISH)
    }
}

/// Live state of the channel relation, as `up`, `node status` and
/// `channel status` report it.
#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct ChannelLink {
    pub channel: String,
    pub rights: String,
    pub root: String,
    /// `active`, `left` or `expired`.
    pub state: String,
    pub expires_at: u64,
    /// The publisher's ACK of the full-chain subscribe on the current
    /// session (subscribe rights only; `None` while there is no session).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscribed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscribe_detail: Option<String>,
    /// ACKed subscribes after the start one (each new session).
    pub resubscribes: u64,
    /// The managed publish chain is installed (publish rights only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish_installed: Option<bool>,
    /// Installed AND this node's config trusts the root: the local publish
    /// gate can pass. Not an observation of a publish.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish_ready: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish_detail: Option<String>,
    /// When a caller-requested publish last cleared this node's local gate
    /// with this credential installed (`channel publish`): live-active
    /// evidence, as opposed to readiness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_at: Option<u64>,
}

pub(crate) type SharedChannel = Arc<parking_lot::Mutex<ChannelLink>>;

/// Per-runtime bookkeeping for [`keep`].
pub(crate) struct ChannelTrack {
    /// The session the chain was last ACKed on.
    pub on: Option<u64>,
    retry_at: u64,
    /// Set by `channel leave`; checked on every pass.
    pub left: Arc<AtomicBool>,
}

impl ChannelTrack {
    pub(crate) fn new(on: Option<u64>, left: Arc<AtomicBool>) -> Self {
        Self {
            on,
            retry_at: 0,
            left,
        }
    }
}

/// The recorded channel departure, if this device left its channel.
pub(crate) fn read_left(state_root: &Path) -> Option<Value> {
    let bytes = std::fs::read(state_root.join(CHANNEL_LEFT_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn record_left(state_root: &Path, channel: &str, at: u64) -> std::io::Result<()> {
    use std::io::Write as _;
    let path = state_root.join(CHANNEL_LEFT_FILE);
    let tmp = path.with_extension("left-tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(
            json!({ "channel": channel, "left_at": at })
                .to_string()
                .as_bytes(),
        )?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)
}

/// Whether this node's own config for exactly `offer.channel` trusts
/// `offer.root` (a prefix entry or another name never counts).
pub(crate) fn locally_trusted(node: &MeshNode, offer: &ChannelOffer) -> bool {
    node.channel_configs().is_some_and(|registry| {
        registry
            .get_by_name(offer.channel.as_str())
            .is_some_and(|config| {
                config.channel_id.name() == &offer.channel
                    && config.token_roots.contains(&offer.root)
            })
    })
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// First use at `up`: install the publish chain, and subscribe once when
/// attached. A left or expired credential is reported, never used.
pub(crate) async fn start(
    node: &MeshNode,
    cred: &ChannelCred,
    left: bool,
    publisher: Option<u64>,
    wait: Duration,
) -> ChannelLink {
    let mut link = ChannelLink {
        channel: cred.offer.channel.as_str().to_string(),
        rights: super::channel::format_channel_rights(cred.offer.rights),
        root: hex::encode(cred.offer.root.as_bytes()),
        state: "active".to_string(),
        expires_at: cred.expires_at(),
        ..Default::default()
    };
    if left {
        link.state = "left".to_string();
        return link;
    }
    if now_unix() >= cred.expires_at() {
        link.state = "expired".to_string();
        return link;
    }
    if cred.publishes() {
        match node.install_publish_chain(&cred.offer.channel, cred.chain.clone()) {
            Ok(_) => {
                link.publish_installed = Some(true);
                link.publish_ready = Some(locally_trusted(node, &cred.offer));
                if link.publish_ready == Some(false) {
                    link.publish_detail = Some(format!(
                        "this node does not trust root {} for {}; run `net-mesh channel serve {} \
                         --token-root {}`",
                        link.root, link.channel, link.channel, link.root
                    ));
                }
            }
            Err(e) => {
                link.publish_installed = Some(false);
                link.publish_ready = Some(false);
                link.publish_detail = Some(e.to_string());
            }
        }
    }
    if cred.subscribes() {
        match publisher {
            Some(publisher) => {
                let acked = subscribe(node, publisher, cred, wait).await;
                link.subscribed = Some(acked.is_ok());
                link.subscribe_detail = acked.err();
            }
            None => {
                link.subscribed = Some(false);
                link.subscribe_detail = Some("not attached".to_string());
            }
        }
    }
    link
}

async fn subscribe(
    node: &MeshNode,
    publisher: u64,
    cred: &ChannelCred,
    wait: Duration,
) -> Result<(), String> {
    match tokio::time::timeout(
        wait,
        node.subscribe_channel_with_chain(
            publisher,
            cred.offer.channel.clone(),
            cred.chain.clone(),
        ),
    )
    .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("the publisher did not answer".to_string()),
    }
}

/// One supervisor pass on `session` with the publisher: keep the full-chain
/// subscribe ACKed on this session, and publish readiness current.
pub(crate) async fn keep(
    node: &MeshNode,
    publisher: u64,
    session: u64,
    cred: &ChannelCred,
    track: &mut ChannelTrack,
    link: &SharedChannel,
    wait: Duration,
) {
    if track.left.load(Ordering::SeqCst) || link.lock().state != "active" {
        return;
    }
    if now_unix() >= cred.expires_at() {
        let mut l = link.lock();
        l.state = "expired".to_string();
        l.subscribed = None;
        l.publish_ready = Some(false);
        return;
    }
    if cred.publishes() && link.lock().publish_installed == Some(true) {
        let ready = locally_trusted(node, &cred.offer);
        let mut l = link.lock();
        l.publish_ready = Some(ready);
        if ready {
            l.publish_detail = None;
        }
    }
    let now = now_unix();
    if cred.subscribes() && track.on != Some(session) && now >= track.retry_at {
        let acked = subscribe(node, publisher, cred, wait).await;
        // A leave that raced this subscribe wins.
        if track.left.load(Ordering::SeqCst) {
            return;
        }
        let mut l = link.lock();
        l.subscribed = Some(acked.is_ok());
        match acked {
            Ok(()) => {
                track.on = Some(session);
                l.subscribe_detail = None;
                l.resubscribes += 1;
            }
            Err(e) => {
                track.retry_at = now + SUBSCRIBE_RETRY_SECS;
                l.subscribe_detail = Some(e);
            }
        }
    }
}

/// A caller-requested publish on `channel` cleared the local gate.
pub(crate) fn published(link: &SharedChannel, channel: &str) {
    let mut l = link.lock();
    if l.channel == channel && l.state == "active" && l.publish_installed == Some(true) {
        l.published_at = Some(now_unix());
    }
}

/// There is no live session: no subscription holds.
pub(crate) fn lost_session(track: &mut ChannelTrack, link: &SharedChannel) {
    track.on = None;
    track.retry_at = 0;
    let mut l = link.lock();
    if l.state == "active" && l.subscribed.is_some() {
        l.subscribed = None;
    }
}

/// Control op `channel_leave`: record the departure durably, then stop both
/// uses of the credential and report what was confirmed.
pub(crate) async fn leave(
    node: &MeshNode,
    state_root: &Path,
    cred: &ChannelCred,
    publisher: u64,
    left: &AtomicBool,
    link: &SharedChannel,
    wait: Duration,
) -> Value {
    if let Some(prior) = read_left(state_root) {
        return json!({ "left": true, "newly_left": false, "left_at": prior["left_at"] });
    }
    let at = now_unix();
    if let Err(e) = record_left(state_root, cred.offer.channel.as_str(), at) {
        return json!({ "error": format!("the departure was not recorded: {e}") });
    }
    left.store(true, Ordering::SeqCst);
    let mut reply = json!({
        "left": true,
        "newly_left": true,
        "left_at": at,
        "channel": cred.offer.channel.as_str(),
    });
    if cred.subscribes() {
        let unsubscribed = match tokio::time::timeout(
            wait,
            node.unsubscribe_channel(publisher, cred.offer.channel.clone()),
        )
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("the publisher did not answer".to_string()),
        };
        reply["unsubscribed"] = json!(unsubscribed.is_ok());
        if let Err(e) = unsubscribed {
            reply["unsubscribe_detail"] = json!(e);
        }
    }
    if cred.publishes() {
        let removed = node.remove_publish_chain_if(&cred.offer.channel, &cred.chain.fingerprint());
        let still_held = node
            .publish_chain_fingerprint(&cred.offer.channel)
            .is_some()
            || node.token_cache().is_some_and(|cache| {
                cache
                    .get_for_action(
                        node.entity_id(),
                        TokenScope::PUBLISH,
                        cred.offer.channel.hash(),
                    )
                    .is_some()
            });
        reply["publish_removed"] = json!(removed);
        reply["publish_stop"] = json!(if still_held {
            "unconfirmed"
        } else {
            "confirmed"
        });
    }
    let mut l = link.lock();
    l.state = "left".to_string();
    l.subscribed = None;
    l.subscribe_detail = None;
    if cred.publishes() {
        l.publish_installed = Some(false);
        l.publish_ready = Some(false);
        l.publish_detail = None;
    }
    reply
}
