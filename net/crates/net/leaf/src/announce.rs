//! The capability announcement: built, signed, verified, queried.
//!
//! # The honest part first
//!
//! `CapabilityAnnouncement` lives in the **core** crate
//! (`adapter/net/behavior/capability.rs`), is ~2 900 lines, and its
//! fields reach into `CapabilitySet`, `EntityId`, `SubnetId`,
//! `GroupId` and `OrgMembershipCert`. A leaf compiled to wasm cannot
//! link the core, and moving that type into `net-mesh-wire` — even
//! feature-gated — would drag `serde_json`, `postcard`,
//! `ed25519-dalek` and four more core modules into the crate whose
//! whole premise is that it is minimal and tokio-free. Stage 5 chose
//! not to.
//!
//! So this module is a **deliberate, named second writer** of one
//! document's byte form. That is a cost, and these are the things
//! that keep it from rotting:
//!
//! 1. **Verification is codec-free.** [`verify_announcement`] never
//!    reconstructs a struct. It parses the inbound JSON into an
//!    order-preserving `Value`, removes exactly `signature` and
//!    `hop_count`, and re-serializes — which *is* the core's
//!    `SignedPayloadCanonical` (it emits every other field in
//!    declaration order with the same `skip_serializing_if`
//!    predicates). No field of an announcement from a native node can
//!    be dropped, reordered or misread by this leaf, because nothing
//!    here enumerates the fields.
//! 2. **Emission is pinned from both directions.** The leaf's own
//!    announcement is the only document this module *writes*, and it
//!    writes seven fields. `tests/announcement_parity.rs` pins its
//!    bytes against the repository fixtures, and
//!    `tests/cross_lang_wire/capability_announcement_leaf.json`
//!    carries a leaf-generated announcement that the core's
//!    `cross_lang_wire` test decodes, re-encodes byte-identically and
//!    verifies — so a drift on either side reddens the other side's
//!    suite.
//!
//! # What a leaf announces (plan §7, "Non-forwarding is a role")
//!
//! Tagged `leaf` and `transport:rtc`, `reflex_addr` **omitted** (a
//! browser has no observer-visible socket), `rtc_addr` omitted (no
//! server-reflexive socket to advertise), `noise_pubkey` **set** —
//! that field is what makes first contact possible for a browser
//! (§5 Layer 1: key discovery precedes signalling).
//!
//! Reachability needs nothing new: the leaf sends its signed
//! announcement to each anchor it holds a session with, anchors
//! flood it, and receivers install `route(leaf) = via sender`. A
//! leaf never re-floods what it receives.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::error::{LeafError, Result};
use crate::identity::{hex_lower, unhex, verify_entity_signature, LeafIdentity};

/// Subprotocol id for capability announcements.
///
/// `behavior::broadcast::SUBPROTOCOL_CAPABILITY_ANN` in the core.
pub const SUBPROTOCOL_CAPABILITY_ANN: u16 = 0x0C00;

/// Subprotocol id for fold frames, which is how a signed
/// announcement reaches a peer over a session.
///
/// `behavior::fold::dispatch::SUBPROTOCOL_FOLD` in the core.
pub const SUBPROTOCOL_FOLD: u16 = 0x1000;

/// The role tag §7 requires on a leaf's announcement.
pub const TAG_LEAF: &str = "leaf";

/// The transport tag §7 requires on a leaf's announcement.
pub const TAG_TRANSPORT_RTC: &str = "transport:rtc";

/// The transport-negotiation tag that tells a native sender this leaf
/// reassembles fragment groups, so a stream payload above the native
/// per-event cap may be cut into pieces instead of refused
/// (`§14.4`, owner ruling 2026-09-16). It mirrors the core's
/// `FRAGMENT_REASSEMBLY_TAG` and must keep the same string.
///
/// Announced unconditionally, and that is not a shortcut: `frame::
/// Reassembler` is always on the leaf's receive path, so this is
/// never a claim the leaf could fail to honour. A conditional tag
/// would be a capability that depends on state a peer cannot see.
pub const TAG_FRAGMENT_REASSEMBLY: &str = "net.stream.fragment_reassembly@1";

/// Default announcement TTL, in seconds. The same 300 the core's
/// fixtures pin.
pub const DEFAULT_TTL_SECS: u32 = 300;

/// Nanoseconds in a second.
///
/// The unit `timestamp_ns` is stamped in, and — since the owner's
/// 2026-09-16 ruling — the unit the freshness comparison is made in.
const NANOS_PER_SEC: u64 = 1_000_000_000;

/// The two fields that sit **outside** the signed transcript.
///
/// `signature` for the obvious reason; `hop_count` because a
/// forwarder increments it and must not need the origin's secret key
/// to do so. The core's canonical serializer omits both
/// unconditionally, so removing both from a parsed document
/// reproduces it.
const UNSIGNED_FIELDS: [&str; 2] = ["signature", "hop_count"];

/// A verified inbound announcement, reduced to what a leaf uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAnnouncement {
    /// The announcing node id.
    pub node_id: u64,
    /// The announcing entity's Ed25519 public key, hex.
    pub entity_id: String,
    /// Its capability tags, in the sorted wire order.
    pub capabilities: Vec<String>,
    /// Its Noise static public key, when it published one — the
    /// §5 Layer 1 datum a browser needs to build a session.
    pub noise_pubkey: Option<[u8; 32]>,
    /// Its public RTC socket, when it published one. A leaf's own
    /// announcement never carries this.
    pub rtc_addr: Option<String>,
    /// Its bootstrap URL, when it published one.
    pub rtc_bootstrap: Option<String>,
    /// Monotonic version, for the newest-wins rule.
    pub version: u64,
    /// Wall-clock stamp in nanoseconds.
    pub timestamp_ns: u64,
    /// Announcement TTL in seconds.
    pub ttl_secs: u32,
}

impl VerifiedAnnouncement {
    /// How long ago this announcement was stamped, in nanoseconds,
    /// as of `now_unix_nanos`.
    ///
    /// `saturating_sub` is what makes a stamp in the **future** safe.
    /// A peer whose wall clock runs ahead of ours — or our own clock
    /// stepping backwards between the stamp and the read — would
    /// otherwise wrap `u64` and report an age of ~584 years, which
    /// expires every record in the store at once. Saturating yields
    /// age zero instead, the youngest an announcement can be, so a
    /// clock disagreement can only ever grant a record lifetime it
    /// has not yet earned and can never invent expiry. There is no
    /// negative age and no wrap. The native side saturates in exactly
    /// the same place (`behavior/capability.rs`'s `is_expired`).
    pub fn age_nanos(&self, now_unix_nanos: u64) -> u64 {
        now_unix_nanos.saturating_sub(self.timestamp_ns)
    }

    /// Is this announcement still inside the lifetime it declared,
    /// as of `now_unix_nanos`?
    ///
    /// **This is the native rule, deliberately, and there is now
    /// exactly one of them.** `behavior/capability.rs`'s
    /// `CapabilityAnnouncement::is_expired` computes
    /// `age_secs = (now_ns - timestamp_ns) / 1e9` and expires at
    /// `age_secs >= ttl_secs`. For an integer `ttl_secs` that is
    /// *identically* the comparison made here, because
    /// `floor(age_ns / 1e9) >= ttl  ⟺  age_ns >= ttl * 1e9` — so
    /// this spelling loses no precision and invents none. The owner
    /// ruled on 2026-09-16 that the leaf adopts it: two expiry rules
    /// for one announcement type is a defect whichever one is nicer,
    /// and the native one is the documented match for
    /// `PermissionToken::is_valid`, which makes the effective
    /// lifetime exactly `ttl_secs` seconds.
    ///
    /// What follows from that arithmetic, all of it load-bearing:
    ///
    /// 1. **The stamp's sub-second remainder counts.** The issue
    ///    instant is not truncated, so a record stamped at
    ///    `S + 0.999 s` is authoritative until `S + ttl + 0.999 s`.
    ///    The previous leaf rule truncated the stamp down to `S` and
    ///    then compensated with an inclusive end; this needs no
    ///    compensation, which is why the inclusive end is gone.
    /// 2. **Exclusive at `age == ttl`.** A TTL of `n` covers the
    ///    half-open interval `[stamp, stamp + n)` and nothing more.
    /// 3. **`ttl_secs == 0` is expired at age zero.** Not "forever",
    ///    and not "the rest of the issuing second" either: a record
    ///    that declares no lifetime has declared an exhausted one,
    ///    from the instant it was stamped. A peer that wants to be
    ///    discoverable says for how long — which is what the leaf's
    ///    own writer does ([`DEFAULT_TTL_SECS`]).
    ///
    /// **No overflow on the bound.** `ttl_secs` is a `u32`, so the
    /// widened product `u64::from(u32::MAX) * NANOS_PER_SEC` is
    /// `4.294_967_295e18` — comfortably under `u64::MAX`
    /// (`≈1.845e19`). A hostile `ttl_secs` cannot wrap the bound
    /// into the past and make an expired record look fresh, and no
    /// saturation is needed on this side of the comparison: the
    /// widening does it. The other side is [`Self::age_nanos`],
    /// which saturates.
    pub fn is_fresh_at_nanos(&self, now_unix_nanos: u64) -> bool {
        self.age_nanos(now_unix_nanos) < u64::from(self.ttl_secs) * NANOS_PER_SEC
    }
}

impl VerifiedAnnouncement {
    /// JSON for the event stream and for `query`.
    ///
    /// `node_id` rides as a decimal **string**: it exceeds 2^53 and
    /// `JSON.parse` would round it, so a page filtering on it would
    /// match the wrong node.
    pub fn to_json(&self) -> String {
        let caps = self
            .capabilities
            .iter()
            .map(|c| Value::String(c.clone()))
            .collect::<Vec<_>>();
        let mut obj = Map::new();
        obj.insert("node_id".into(), Value::String(self.node_id.to_string()));
        obj.insert("entity_id".into(), Value::String(self.entity_id.clone()));
        obj.insert("capabilities".into(), Value::Array(caps));
        obj.insert(
            "rtc_addr".into(),
            self.rtc_addr.clone().map_or(Value::Null, Value::String),
        );
        obj.insert(
            "noise_pubkey".into(),
            self.noise_pubkey
                .map_or(Value::Null, |k| Value::String(hex_lower(&k))),
        );
        obj.insert("version".into(), Value::String(self.version.to_string()));
        Value::Object(obj).to_string()
    }
}

/// Build and sign this leaf's announcement.
///
/// `capabilities` are the application's tags; [`TAG_LEAF`] and
/// [`TAG_TRANSPORT_RTC`] are added unconditionally — they are the
/// role, not an option. Tags are emitted in sorted order, which is
/// what makes the signature reproducible across processes (the
/// core's `CapabilitySet` holds them in a `HashSet` and sorts on the
/// way out for exactly this reason).
pub fn build_announcement(
    identity: &LeafIdentity,
    capabilities: &[String],
    version: u64,
    timestamp_ns: u64,
    ttl_secs: u32,
) -> Result<Vec<u8>> {
    let mut tags: Vec<String> = capabilities.to_vec();
    tags.push(TAG_LEAF.to_string());
    tags.push(TAG_TRANSPORT_RTC.to_string());
    tags.push(TAG_FRAGMENT_REASSEMBLY.to_string());
    tags.sort_unstable();
    tags.dedup();

    let body = announcement_body(
        identity.node_id(),
        &identity.entity().entity_id_hex(),
        version,
        timestamp_ns,
        ttl_secs,
        &tags,
        Some(*identity.noise().public_key()),
    );

    // Sign the transcript, then insert `signature` at its
    // declaration position: after `capabilities`, before every
    // optional field. `Map` preserves insertion order, so the
    // signed document is rebuilt rather than patched.
    let transcript = serde_json::to_vec(&Value::Object(body.clone()))
        .map_err(|e| LeafError::Identity(format!("announcement transcript: {e}")))?;
    let signature = identity.entity().sign(&transcript);

    let mut signed = Map::new();
    for (key, value) in body {
        signed.insert(key.clone(), value);
        if key == "capabilities" {
            signed.insert("signature".into(), Value::String(hex_lower(&signature)));
        }
    }
    serde_json::to_vec(&Value::Object(signed))
        .map_err(|e| LeafError::Identity(format!("announcement encode: {e}")))
}

/// The signed fields of a leaf's announcement, in the core's
/// declaration order.
///
/// Seven entries, and every one of them is load-bearing:
/// `capabilities` carries the two role tags, `noise_pubkey` is the
/// key-discovery datum, and the four scalars are what the fold's
/// newest-wins rule and TTL expiry read. Every other field of
/// `CapabilityAnnouncement` is `skip_serializing_if`-omitted when a
/// leaf leaves it unset, which is why a leaf's announcement is a
/// strict prefix-shape of a native node's.
fn announcement_body(
    node_id: u64,
    entity_id_hex: &str,
    version: u64,
    timestamp_ns: u64,
    ttl_secs: u32,
    sorted_tags: &[String],
    noise_pubkey: Option<[u8; 32]>,
) -> Map<String, Value> {
    let mut capabilities = Map::new();
    capabilities.insert(
        "tags".into(),
        Value::Array(
            sorted_tags
                .iter()
                .map(|t| Value::String(t.clone()))
                .collect(),
        ),
    );
    // `metadata` is a `BTreeMap` with `#[serde(default)]` and no
    // skip predicate: it is emitted even when empty, as `{}`.
    capabilities.insert(
        "metadata".into(),
        Value::Object(
            BTreeMap::<String, String>::new()
                .into_iter()
                .map(|(k, v)| (k, Value::String(v)))
                .collect(),
        ),
    );

    let mut body = Map::new();
    body.insert("node_id".into(), Value::from(node_id));
    body.insert("entity_id".into(), Value::String(entity_id_hex.to_string()));
    body.insert("version".into(), Value::from(version));
    body.insert("timestamp_ns".into(), Value::from(timestamp_ns));
    body.insert("ttl_secs".into(), Value::from(ttl_secs));
    body.insert("capabilities".into(), Value::Object(capabilities));
    if let Some(key) = noise_pubkey {
        // A 32-element byte ARRAY, not hex: that is what the derived
        // `Serialize` on `Option<[u8; 32]>` produces, and what
        // `capability_announcement_rtc.json` pins.
        body.insert(
            "noise_pubkey".into(),
            Value::Array(key.iter().map(|b| Value::from(*b)).collect()),
        );
    }
    body
}

/// Rebuild the signed transcript of an announcement document.
///
/// This is the whole verification codec: drop `UNSIGNED_FIELDS`,
/// keep everything else exactly as it arrived, re-serialize. Nothing
/// here enumerates the announcement's fields, so a field this leaf
/// has never heard of still enters the transcript and still
/// authenticates.
pub fn signed_transcript(document: &Value) -> Result<Vec<u8>> {
    let Value::Object(fields) = document else {
        return Err(LeafError::Wire("announcement is not a JSON object".into()));
    };
    let mut canonical = Map::new();
    for (key, value) in fields {
        if UNSIGNED_FIELDS.contains(&key.as_str()) {
            continue;
        }
        canonical.insert(key.clone(), value.clone());
    }
    serde_json::to_vec(&Value::Object(canonical))
        .map_err(|e| LeafError::Wire(format!("announcement transcript: {e}")))
}

/// Verify an inbound announcement and reduce it to what a leaf uses.
///
/// An announcement without a signature is **refused**. The plan's
/// signed mode is what makes `query` meaningful: an unverified
/// announcement would let any peer claim any capability for any node
/// id, and this leaf answers `query` from ingested announcements.
pub fn verify_announcement(bytes: &[u8]) -> Result<VerifiedAnnouncement> {
    let document: Value = serde_json::from_slice(bytes)
        .map_err(|e| LeafError::Wire(format!("announcement did not parse: {e}")))?;

    let signature_hex = document
        .get("signature")
        .and_then(Value::as_str)
        .ok_or_else(|| LeafError::Wire("announcement carries no signature".into()))?;
    let signature: [u8; 64] = unhex(signature_hex)?
        .try_into()
        .map_err(|_| LeafError::Wire("signature is not 64 bytes".into()))?;

    let entity_hex = document
        .get("entity_id")
        .and_then(Value::as_str)
        .ok_or_else(|| LeafError::Wire("announcement carries no entity_id".into()))?;
    let entity_id: [u8; 32] = unhex(entity_hex)?
        .try_into()
        .map_err(|_| LeafError::Wire("entity_id is not 32 bytes".into()))?;

    let transcript = signed_transcript(&document)?;
    verify_entity_signature(&entity_id, &transcript, &signature)
        .map_err(|_| LeafError::Wire("announcement signature did not verify".into()))?;

    let node_id = document
        .get("node_id")
        .and_then(Value::as_u64)
        .ok_or_else(|| LeafError::Wire("announcement carries no node_id".into()))?;
    // **The signature covers `entity_id`, not `node_id`** (R1). An
    // attacker signs with its OWN entity key, claims the VICTIM's
    // node id and a later version, and the store replaces the honest
    // record — including the verification key every later signal
    // attributed to that node is checked against. The core refuses
    // this at its capability arm; the leaf now refuses it here,
    // BEFORE the record can replace anything or authorise anything.
    let derived = crate::identity::node_id_for_entity(&entity_id);
    if derived != node_id {
        return Err(LeafError::Wire(format!(
            "announcement claims node {node_id:#018x} but its signing entity derives to \
             {derived:#018x} — a signature over someone else's node id authorises nothing"
        )));
    }
    let capabilities = document
        .pointer("/capabilities/tags")
        .and_then(Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(|t| t.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let noise_pubkey = document
        .get("noise_pubkey")
        .and_then(Value::as_array)
        .and_then(|bytes| {
            let raw: Vec<u8> = bytes
                .iter()
                .filter_map(|b| b.as_u64().and_then(|v| u8::try_from(v).ok()))
                .collect();
            <[u8; 32]>::try_from(raw.as_slice()).ok()
        });

    Ok(VerifiedAnnouncement {
        node_id,
        entity_id: entity_hex.to_string(),
        capabilities,
        noise_pubkey,
        rtc_addr: document
            .get("rtc_addr")
            .and_then(Value::as_str)
            .map(str::to_string),
        rtc_bootstrap: document
            .get("rtc_bootstrap")
            .and_then(Value::as_str)
            .map(str::to_string),
        version: document.get("version").and_then(Value::as_u64).unwrap_or(0),
        timestamp_ns: document
            .get("timestamp_ns")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        ttl_secs: document
            .get("ttl_secs")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0),
    })
}

/// What the leaf learned from announcements, newest-wins per node.
///
/// This is the leaf's half of `query`: the control plane answers
/// from the mesh, and this answers from what the dispatcher actually
/// ingested and verified. A leaf never re-floods these.
#[derive(Debug, Default)]
pub struct AnnouncementStore {
    by_node: BTreeMap<u64, VerifiedAnnouncement>,
}

impl AnnouncementStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest a verified announcement. Older versions for a node are
    /// refused, matching the fold's generation rule.
    ///
    /// Returns whether the store changed.
    pub fn ingest(&mut self, announcement: VerifiedAnnouncement) -> bool {
        match self.by_node.get(&announcement.node_id) {
            Some(existing) if existing.version >= announcement.version => false,
            _ => {
                self.by_node.insert(announcement.node_id, announcement);
                true
            }
        }
    }

    /// Every node whose announcement carries `capability` as a tag
    /// **and is still fresh** (R8).
    ///
    /// An expired announcement is not discoverable. It used to be:
    /// `ttl_secs` was parsed, stored and never consulted, so a peer
    /// that went away stayed in every answer `query` gave and stayed
    /// the key authority for every signal attributed to it. Freshness
    /// is evaluated at READ time rather than by a sweep, so a
    /// re-announce restores discoverability immediately.
    pub fn query(&self, capability: &str) -> Vec<&VerifiedAnnouncement> {
        self.query_at_nanos(capability, crate::clock::now_unix_nanos())
    }

    /// [`Self::query`] evaluated against an explicit wall-clock
    /// nanosecond reading.
    ///
    /// One clock reading for the whole scan — the same shape as
    /// `clock::Deadline::expired_at`, and the same reason: a sweep
    /// over many records must judge them all against one instant, or
    /// an instant that advances mid-scan makes the answer depend on
    /// iteration order. It is also the entrypoint that lets the
    /// expiry boundary be exercised at an exact nanosecond instead of
    /// whenever the host clock happens to be; see
    /// [`VerifiedAnnouncement::is_fresh_at_nanos`] for the boundary.
    pub fn query_at_nanos(
        &self,
        capability: &str,
        now_unix_nanos: u64,
    ) -> Vec<&VerifiedAnnouncement> {
        self.by_node
            .values()
            .filter(|a| {
                a.is_fresh_at_nanos(now_unix_nanos)
                    && a.capabilities.iter().any(|c| c == capability)
            })
            .collect()
    }

    /// The announcement held for `node`, if it is still fresh.
    ///
    /// This is the accessor the signal verifier reads its key from,
    /// so the same expiry that removes a peer from discovery removes
    /// its authority to authorise a signal.
    pub fn get(&self, node: u64) -> Option<&VerifiedAnnouncement> {
        self.get_at_nanos(node, crate::clock::now_unix_nanos())
    }

    /// [`Self::get`] evaluated against an explicit wall-clock
    /// nanosecond reading — the authority lookup with its expiry
    /// boundary made addressable.
    pub fn get_at_nanos(&self, node: u64, now_unix_nanos: u64) -> Option<&VerifiedAnnouncement> {
        self.by_node
            .get(&node)
            .filter(|a| a.is_fresh_at_nanos(now_unix_nanos))
    }

    /// The record for `node` whether or not it is fresh — for the
    /// version comparison `ingest` makes, which must not let an
    /// expired record be replaced by an OLDER one.
    pub fn get_including_expired(&self, node: u64) -> Option<&VerifiedAnnouncement> {
        self.by_node.get(&node)
    }

    /// The full node id whose low 32 bits are `routing_id`, from a
    /// still-fresh announcement.
    ///
    /// **Why this exists.** `RoutingHeader::src_id` is a `u32` — the
    /// low 32 bits of a node id — so a relayed packet names its
    /// sender only by that projection, and the native side handles it
    /// the same way (`adapter/net/mesh.rs`'s `handle_routed_handshake`
    /// binds its prologue to `routing_header.src_id as u64` for
    /// exactly this reason). A leaf's NKpsk0 message 1 carries an
    /// empty payload, so the projection is all a relayed handshake
    /// arrives with, and the session table is keyed on the FULL id.
    ///
    /// Resolving it here rather than anywhere else is deliberate:
    /// the answer can only be a node whose **signed announcement**
    /// this leaf verified, which is §5 Layer 1's rule ("key discovery
    /// precedes signalling") applied to the session seam as well as
    /// to the envelope. An expired announcement resolves to nothing,
    /// so the same expiry that removes a peer from discovery removes
    /// it from here.
    ///
    /// **Ambiguity is refused, not guessed.** A 32-bit projection is
    /// not unique; two fresh announcements sharing one is `None`,
    /// because choosing between them would be this function
    /// inventing a sender.
    pub fn resolve_routing_id(&self, routing_id: u32) -> Option<u64> {
        self.resolve_routing_id_at_nanos(routing_id, crate::clock::now_unix_nanos())
    }

    /// [`Self::resolve_routing_id`] against an explicit wall-clock
    /// nanosecond reading, so the expiry boundary is testable
    /// natively.
    pub fn resolve_routing_id_at_nanos(&self, routing_id: u32, now_unix_nanos: u64) -> Option<u64> {
        let mut found = None;
        for node in self.by_node.values().filter_map(|a| {
            a.is_fresh_at_nanos(now_unix_nanos)
                .then_some(a.node_id)
                .filter(|id| *id as u32 == routing_id)
        }) {
            if found.replace(node).is_some() {
                return None;
            }
        }
        found
    }

    /// How many announcements are held.
    pub fn len(&self) -> usize {
        self.by_node.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.by_node.is_empty()
    }

    /// A JSON array of the matches, the shape `query` resolves to.
    pub fn query_json(&self, capability: &str) -> String {
        let mut out = String::from("[");
        for (i, a) in self.query(capability).into_iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&a.to_json());
        }
        out.push(']');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::EntityKeypair;

    fn identity() -> LeafIdentity {
        LeafIdentity::from_secrets(EntityKeypair::from_secret([0x21; 32]), [0x22; 32])
    }

    /// Round trip through the real signer and the real verifier.
    #[test]
    fn a_leaf_announcement_verifies_and_carries_the_two_role_tags() {
        let id = identity();
        let bytes = build_announcement(
            &id,
            &["stage5.browser".to_string()],
            7,
            1_700_000_000_000_000_000,
            DEFAULT_TTL_SECS,
        )
        .expect("build");

        let verified = verify_announcement(&bytes).expect("its own signature must verify");
        assert_eq!(verified.node_id, id.node_id());
        assert_eq!(
            verified.capabilities,
            vec![
                "leaf".to_string(),
                "net.stream.fragment_reassembly@1".to_string(),
                "stage5.browser".to_string(),
                "transport:rtc".to_string()
            ],
            "tags must be sorted, and neither the two role tags nor the \
             fragment-reassembly negotiation tag is optional: a native \
             sender reads the last one to decide whether a payload above \
             its per-event cap may be fragmented instead of refused"
        );
        assert_eq!(
            verified.noise_pubkey.as_ref(),
            Some(id.noise().public_key()),
            "the leaf must publish its Noise static — that is §5 Layer 1"
        );
        assert_eq!(
            verified.rtc_addr, None,
            "a browser has no server-reflexive socket to advertise"
        );
        let text = String::from_utf8(bytes).expect("UTF-8 JSON");
        assert!(
            !text.contains("reflex_addr"),
            "§7: a leaf omits reflex_addr — got {text}"
        );
        assert!(!text.contains("hop_count"), "a leaf originates at hop 0");
    }

    /// A record with a chosen node id, stamped at the exact start of
    /// second `issued_unix_secs`.
    ///
    /// Hand-built rather than signed: the only field under test is
    /// `node_id`, and two node ids that collide in their low 32 bits
    /// cannot be reached by choosing entity secrets — the id is a
    /// hash of the key.
    fn record(node_id: u64, issued_unix_secs: u64) -> VerifiedAnnouncement {
        VerifiedAnnouncement {
            node_id,
            entity_id: format!("{node_id:016x}"),
            capabilities: vec!["leaf".to_string()],
            noise_pubkey: Some([0x33; 32]),
            rtc_addr: None,
            rtc_bootstrap: None,
            version: 1,
            timestamp_ns: issued_unix_secs * NANOS_PER_SEC,
            ttl_secs: DEFAULT_TTL_SECS,
        }
    }

    /// The 32-bit projection a routing header carries resolves to the
    /// full node id, and expiry takes that authority away with
    /// discovery.
    #[test]
    fn a_routing_id_resolves_to_the_announced_node_and_expires_with_it() {
        const NOW: u64 = 1_700_000_000;
        let now_ns = NOW * NANOS_PER_SEC;
        let mut store = AnnouncementStore::new();
        store.ingest(record(0xAAAA_BBBB_1234_5678, NOW));
        store.ingest(record(0xCCCC_DDDD_9999_0000, NOW));

        assert_eq!(
            store.resolve_routing_id_at_nanos(0x1234_5678, now_ns),
            Some(0xAAAA_BBBB_1234_5678)
        );
        assert_eq!(
            store.resolve_routing_id_at_nanos(0x9999_0000, now_ns),
            Some(0xCCCC_DDDD_9999_0000)
        );
        assert_eq!(
            store.resolve_routing_id_at_nanos(0xDEAD_BEEF, now_ns),
            None,
            "a projection nobody announced resolves to nobody"
        );
        let ttl_ns = u64::from(DEFAULT_TTL_SECS) * NANOS_PER_SEC;
        assert_eq!(
            store.resolve_routing_id_at_nanos(0x1234_5678, now_ns + ttl_ns - 1),
            Some(0xAAAA_BBBB_1234_5678),
            "one nanosecond of declared lifetime is still lifetime"
        );
        assert_eq!(
            store.resolve_routing_id_at_nanos(0x1234_5678, now_ns + ttl_ns),
            None,
            "the expiry that removes a peer from discovery removes it \
             from here, and it lands at age == ttl exactly"
        );
    }

    /// **Fail closed on a collision.** Two fresh announcements
    /// sharing a 32-bit projection resolve to neither: a leaf that
    /// picked one would be inventing the sender of a relayed packet,
    /// and the whole point of resolving through discovery is that
    /// nothing else gets to decide who sent something.
    #[test]
    fn two_announcements_sharing_a_routing_id_resolve_to_neither() {
        const NOW: u64 = 1_700_000_000;
        let now_ns = NOW * NANOS_PER_SEC;
        let mut store = AnnouncementStore::new();
        store.ingest(record(0x1111_1111_4444_4444, NOW));
        store.ingest(record(0x2222_2222_4444_4444, NOW));

        assert_eq!(store.len(), 2, "both are held: they are different nodes");
        assert_eq!(
            store.resolve_routing_id_at_nanos(0x4444_4444, now_ns),
            None,
            "ambiguity is refused, not guessed"
        );
        // And the ambiguity is confined to the projection: each is
        // still reachable by its full id, so a collision costs those
        // two peers the relayed path and nothing else.
        assert!(store.get_at_nanos(0x1111_1111_4444_4444, now_ns).is_some());
        assert!(store.get_at_nanos(0x2222_2222_4444_4444, now_ns).is_some());
    }

    /// One flipped byte anywhere in the signed transcript must fail.
    #[test]
    fn tampering_with_any_signed_field_fails_verification() {
        let id = identity();
        let bytes = build_announcement(&id, &[], 1, 1_700_000_000_000_000_000, 300).expect("build");
        let text = String::from_utf8(bytes).expect("UTF-8");

        for (from, to) in [
            ("\"version\":1", "\"version\":2"),
            ("\"ttl_secs\":300", "\"ttl_secs\":301"),
            ("\"leaf\"", "\"anchor\""),
            ("\"transport:rtc\"", "\"transport:udp\""),
        ] {
            let tampered = text.replace(from, to);
            assert_ne!(tampered, text, "the test's own substitution must apply");
            assert!(
                verify_announcement(tampered.as_bytes()).is_err(),
                "tampering {from} -> {to} must fail verification"
            );
        }
    }

    #[test]
    fn an_unsigned_announcement_is_refused() {
        let id = identity();
        let bytes = build_announcement(&id, &[], 1, 1, 300).expect("build");
        let document: Value = serde_json::from_slice(&bytes).expect("parses");
        let stripped = signed_transcript(&document).expect("transcript");
        assert!(
            verify_announcement(&stripped).is_err(),
            "an announcement with no signature must never be accepted"
        );
    }

    /// The transcript rule: exactly `signature` and `hop_count` are
    /// removed, nothing else, and the order of what remains is the
    /// order it arrived in.
    #[test]
    fn the_transcript_removes_only_the_two_unsigned_fields() {
        let document: Value = serde_json::from_str(
            r#"{"node_id":1,"entity_id":"ab","version":2,"signature":"ff","hop_count":3,"rtc_addr":"1.2.3.4:5"}"#,
        )
        .expect("parses");
        let transcript =
            String::from_utf8(signed_transcript(&document).expect("transcript")).expect("UTF-8");
        assert_eq!(
            transcript, r#"{"node_id":1,"entity_id":"ab","version":2,"rtc_addr":"1.2.3.4:5"}"#,
            "field order must survive, and only signature + hop_count leave"
        );
    }

    /// A forwarded announcement (hop_count > 0) must still verify —
    /// that is the entire reason hop_count sits outside the
    /// transcript.
    #[test]
    fn a_forwarded_announcement_still_verifies() {
        let id = identity();
        let bytes = build_announcement(&id, &[], 3, 1, 300).expect("build");
        let mut document: Value = serde_json::from_slice(&bytes).expect("parses");
        if let Value::Object(fields) = &mut document {
            fields.insert("hop_count".into(), Value::from(4u8));
        }
        let forwarded = serde_json::to_vec(&document).expect("re-encode");
        verify_announcement(&forwarded)
            .expect("incrementing hop_count must not invalidate the signature");
    }

    /// A `timestamp_ns` inside the announcement's own lifetime.
    ///
    /// These two tests pin STORE semantics — newest-wins, tag
    /// matching, the JSON shape — and used `1` as a stand-in
    /// timestamp. Since R8 made freshness load-bearing (an expired
    /// announcement is neither discoverable nor a key authority), a
    /// 1970 stamp means expired, so the stand-in has to be a real
    /// one. It is read in NANOSECONDS, not a second truncated and
    /// re-scaled: since the owner's 2026-09-16 ruling the stamp's
    /// sub-second remainder is part of the issue instant, and a
    /// truncated stand-in would silently donate up to a second of
    /// the record's lifetime to the past. `nudge` keeps the two
    /// versions distinguishable.
    fn fresh_stamp(nudge: u64) -> u64 {
        crate::clock::now_unix_nanos() + nudge
    }

    #[test]
    fn the_store_is_newest_wins_per_node_and_query_matches_on_tags() {
        let id = identity();
        let mut store = AnnouncementStore::new();

        let v1 = verify_announcement(
            &build_announcement(&id, &["gpu".to_string()], 1, fresh_stamp(1), 300).expect("build"),
        )
        .expect("verify");
        assert!(store.ingest(v1));
        assert_eq!(store.query("gpu").len(), 1);

        let v3 = verify_announcement(
            &build_announcement(&id, &["tpu".to_string()], 3, fresh_stamp(2), 300).expect("build"),
        )
        .expect("verify");
        assert!(store.ingest(v3));
        assert_eq!(store.len(), 1, "one entry per node");
        assert!(
            store.query("gpu").is_empty(),
            "the superseded tag must not answer a query"
        );
        assert_eq!(store.query("tpu").len(), 1);

        let v2 = verify_announcement(
            &build_announcement(&id, &["old".to_string()], 2, fresh_stamp(3), 300).expect("build"),
        )
        .expect("verify");
        assert!(!store.ingest(v2), "an older version must be refused");
        assert!(store.query("old").is_empty());

        // Both role tags are queryable, which is what makes a native
        // `find_best_node` able to select a browser.
        assert_eq!(store.query(TAG_LEAF).len(), 1);
        assert_eq!(store.query(TAG_TRANSPORT_RTC).len(), 1);
    }

    #[test]
    fn query_json_carries_node_ids_as_strings() {
        let id = identity();
        let mut store = AnnouncementStore::new();
        store.ingest(
            verify_announcement(
                &build_announcement(&id, &[], 1, fresh_stamp(1), 300).expect("build"),
            )
            .expect("verify"),
        );
        let json = store.query_json(TAG_LEAF);
        assert!(
            json.contains(&format!("\"node_id\":\"{}\"", id.node_id())),
            "node_id must be a decimal string — JSON.parse rounds \
             integers above 2^53: {json}"
        );
        assert_eq!(store.query_json("absent"), "[]");
    }

    /// The expiry boundary, pinned at the exact nanosecond on the
    /// PRODUCTION authority lookup (`get_at_nanos`) and the
    /// production discovery scan (`query_at_nanos`) — not on
    /// `is_fresh_at_nanos` alone.
    ///
    /// `get` is what the signal verifier reads a peer's key from, so
    /// one nanosecond either side of expiry is the difference between
    /// a peer being able to authorise a signal and not. The
    /// announcement goes through the real `build_announcement` →
    /// `verify_announcement` → `ingest` path so the stored
    /// `timestamp_ns`/`ttl_secs` are the signed ones.
    ///
    /// **This test MOVED, by the owner's 2026-09-16 ruling.** It used
    /// to assert that the whole second `issued + ttl` was still
    /// authoritative — the leaf's inclusive, second-granular rule —
    /// and to be named for expiring "one second after issue plus
    /// ttl". The instant asserted `None` below (`stamp + ttl`, which
    /// falls inside that second) was asserted `Some` before. The
    /// ruling is that one announcement type gets one expiry rule, and
    /// the rule is the native one
    /// (`behavior/capability.rs::is_expired`, `age >= ttl`, which is
    /// documented as matching `PermissionToken::is_valid`): a leaf
    /// and a native node must not disagree about whether a peer is
    /// still an authority. This asserts the opposite outcome at the
    /// boundary, not a vaguer one.
    #[test]
    fn the_authority_lookup_expires_exactly_ttl_nanoseconds_after_the_stamp() {
        let id = identity();
        let issued: u64 = 1_700_000_000;
        let ttl: u32 = 300;
        // Stamp deliberately mid-second: the sub-second remainder is
        // part of the issue instant now, so it must move the boundary
        // rather than be truncated away.
        let stamp = issued * NANOS_PER_SEC + 999_999_999;
        let ttl_ns = u64::from(ttl) * NANOS_PER_SEC;

        let mut store = AnnouncementStore::new();
        store.ingest(
            verify_announcement(
                &build_announcement(&id, &["gpu".to_string()], 1, stamp, ttl).expect("build"),
            )
            .expect("verify"),
        );
        let node = id.node_id();

        assert!(
            store.get_at_nanos(node, stamp + ttl_ns - 1).is_some(),
            "one nanosecond of declared lifetime is still lifetime"
        );
        assert!(
            store.get_at_nanos(node, stamp + ttl_ns).is_none(),
            "at age == ttl the peer has no authority left: the \
             interval a ttl of {ttl} covers is [stamp, stamp + {ttl}), \
             half-open. The old inclusive rule called this instant \
             fresh because it fell inside second {}",
            issued + u64::from(ttl)
        );
        assert!(
            store
                .get_at_nanos(node, (issued + u64::from(ttl)) * NANOS_PER_SEC)
                .is_some(),
            "the stamp's remainder is NOT truncated to its second: a \
             record issued at {issued}.999999999 has only spent \
             {ttl}s − 0.999999999s of its lifetime here"
        );

        // Discovery uses the same boundary, and the record is still
        // held either way — expiry is a read-time filter, not a
        // deletion, so a re-announce restores it.
        assert_eq!(store.query_at_nanos("gpu", stamp + ttl_ns - 1).len(), 1);
        assert!(store.query_at_nanos("gpu", stamp + ttl_ns).is_empty());
        assert!(
            store.get_including_expired(node).is_some(),
            "the expired record is retained for the version comparison"
        );
    }

    /// `ttl_secs == 0` is a lifetime of zero nanoseconds: expired
    /// from the instant it was stamped. Not "forever", and not "the
    /// rest of the issuing second" either.
    ///
    /// **This test MOVED, by the owner's 2026-09-16 ruling.** It was
    /// named `a_zero_ttl_announcement_is_authoritative_only_within_
    /// its_issuing_second` and asserted that a zero-TTL record was a
    /// key authority and discoverable for the remainder of the second
    /// it was issued in. Natively the same record is already dead at
    /// age zero (`age >= ttl` with `ttl == 0` is true immediately),
    /// so the leaf and a native node disagreed about whether a peer
    /// could authorise a signal — one announcement type with two
    /// expiry rules, which is a defect whichever rule is nicer. The
    /// owner ruled the leaf adopts the native one. Every assertion
    /// below is the exact negation of the one it replaces.
    ///
    /// The pre-existing ancient-zero-TTL probe
    /// (`tests/kyra_review.rs`'s
    /// `kyra_expired_announcement_is_not_discovery_or_signal_authority`)
    /// only covers a 1970 stamp, where both readings agree.
    #[test]
    fn a_zero_ttl_announcement_is_expired_from_the_instant_it_was_stamped() {
        let id = identity();
        let issued: u64 = 1_700_000_000;
        let stamp = issued * NANOS_PER_SEC + 1;
        let mut store = AnnouncementStore::new();
        store.ingest(
            verify_announcement(
                &build_announcement(&id, &["gpu".to_string()], 1, stamp, 0).expect("build"),
            )
            .expect("verify"),
        );
        let node = id.node_id();

        assert!(
            store.get_at_nanos(node, stamp).is_none(),
            "read at its own stamp, a zero-TTL record has already \
             spent every nanosecond it declared"
        );
        assert!(store.query_at_nanos("gpu", stamp).is_empty());
        assert!(
            store.get_at_nanos(node, issued * NANOS_PER_SEC).is_none(),
            "read one nanosecond BEFORE its stamp — a peer clock ahead \
             of ours, or ours stepped back — the age saturates to zero \
             and zero is still spent; a clock disagreement must not \
             resurrect it"
        );
        assert!(
            store.get_at_nanos(node, stamp + NANOS_PER_SEC).is_none(),
            "and it stays expired a second later"
        );
        assert!(store
            .query_at_nanos("gpu", stamp + NANOS_PER_SEC)
            .is_empty());
        assert!(
            store.get_including_expired(node).is_some(),
            "expiry is a read-time filter: the record is retained for \
             the version comparison `ingest` makes"
        );
    }
}
