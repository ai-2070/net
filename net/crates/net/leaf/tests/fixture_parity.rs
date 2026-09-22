//! The leaf side of the `cross_lang_wire` pins.
//!
//! Three jobs:
//!
//! 1. **No drift between the two copies.** Every constant in
//!    [`net_leaf::test_vectors`] must be byte-identical to the
//!    repository original under
//!    `net/crates/net/tests/cross_lang_wire/`. The package copy
//!    exists so the wasm test target can `include_str!` it (a
//!    boundary-crossing `include_str!` breaks an unpacked
//!    `cargo package` tarball); the repository copy is what a Go /
//!    TypeScript / Python consumer reads. Two copies that cannot
//!    diverge.
//! 2. **The leaf's announcement encoder is pinned.** Stage 5 carries
//!    a deliberate second *writer* of the capability announcement —
//!    see `announce`'s module docs for why moving
//!    `CapabilityAnnouncement` into the wire crate was the wrong
//!    trade. This asserts the encoder reproduces
//!    `capability_announcement_leaf.json` byte for byte from the
//!    same deterministic identity, and that the signature in the
//!    fixture verifies.
//! 3. **The leaf's nRPC client codec is pinned**, the same way,
//!    against `nrpc_frame.json`.
//!
//! The other direction — the *core's* production decoders asserting
//! the same two fixtures — lives in
//! `net/crates/net/tests/cross_lang_wire.rs`. A one-directional pin
//! is exactly the gap that makes a second copy dangerous.

use std::path::PathBuf;

use net_leaf::announce;
use net_leaf::identity::{hex_lower, EntityKeypair, LeafIdentity};
use net_leaf::rpc_wire::{
    decode_route, encode_request_frame, EventMeta, RpcRequestPayload, DISPATCH_RPC_REQUEST,
};
use net_leaf::test_vectors;

/// The deterministic identity `capability_announcement_leaf.json`
/// was generated from. Named in the fixture itself.
fn fixture_identity() -> LeafIdentity {
    LeafIdentity::from_secrets(EntityKeypair::from_secret([0x21; 32]), [0x22; 32])
}

fn repository_copy(name: &str) -> Option<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/cross_lang_wire")
        .join(name);
    std::fs::read_to_string(path).ok()
}

fn field<'a>(document: &'a serde_json::Value, pointer: &str) -> &'a str {
    document
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("fixture has no {pointer}"))
}

/// The package copies and the repository originals are the same
/// bytes.
#[test]
fn every_package_vector_mirrors_its_repository_original() {
    let mut checked = 0;
    for (name, packaged) in test_vectors::ALL {
        let Some(original) = repository_copy(name) else {
            // An unpacked `.crate` has no repository tree. The
            // constant is still compiled in, which is the property
            // that matters there.
            continue;
        };
        assert_eq!(
            packaged, original,
            "{name} has drifted between the package copy and the repository copy"
        );
        checked += 1;
    }
    assert_eq!(
        checked,
        test_vectors::ALL.len(),
        "this is a repository checkout, so every vector must have been compared"
    );

    // The parity above walks the ALL registry — a hand-maintained
    // list. A vector added to either tree but never registered is
    // invisible to it (the defect this leg exists for: the walk
    // followed the list rather than the directories), so the two
    // directories are walked too and their contents must be exactly
    // the registry's names.
    let registered: std::collections::BTreeSet<&str> =
        test_vectors::ALL.iter().map(|(name, _)| *name).collect();
    let names: Vec<String> = registered.iter().map(|name| name.to_string()).collect();
    let mut packaged =
        json_names(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/test_vectors"));
    packaged.sort();
    assert_eq!(
        packaged, names,
        "src/test_vectors/ and the ALL registry disagree — a vector added but \
         unregistered is invisible to the parity above"
    );
    let mut repository: Vec<String> =
        json_names(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/cross_lang_wire"))
            .into_iter()
            .filter(|name| !REPOSITORY_ONLY.contains(&name.as_str()))
            .collect();
    repository.sort();
    if !repository.is_empty() {
        assert_eq!(
            repository, names,
            "../tests/cross_lang_wire/ and the ALL registry disagree (minus the \
             files another crate's registry owns)"
        );
    }
}

/// Files in the shared `cross_lang_wire/` tree that another crate's
/// registry owns — the wire crate's shared AEAD vector, packaged by
/// [`net_wire::test_vectors`], not by the leaf's constants. Nothing
/// else may live in either tree unregistered.
const REPOSITORY_ONLY: [&str; 1] = ["aead_vector.json"];

/// The `.json` file names in a directory; empty when it is absent
/// (an unpacked `.crate` has no repository tree).
fn json_names(dir: &PathBuf) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".json"))
        .collect()
}

/// The leaf's announcement encoder, pinned.
#[test]
fn the_leaf_announcement_encoder_reproduces_the_pinned_bytes() {
    let document: serde_json::Value =
        serde_json::from_str(test_vectors::CAPABILITY_ANNOUNCEMENT_LEAF)
            .expect("the fixture parses");
    let pinned = field(&document, "/bytes_utf8");

    let identity = fixture_identity();
    // The fixture names the identity it was generated from; assert
    // the derivation rather than trusting the comment.
    assert_eq!(
        identity.node_id().to_string(),
        field(&document, "/identity/node_id"),
        "the fixture's node_id must be this identity's derivation"
    );
    assert_eq!(
        hex_lower(identity.entity().entity_id()),
        field(&document, "/json/entity_id")
    );

    let produced = announce::build_announcement(
        &identity,
        &["stage5.browser".to_string()],
        7,
        1_700_000_000_000_000_000,
        300,
    )
    .expect("build");
    assert_eq!(
        String::from_utf8(produced).expect("UTF-8 JSON"),
        pinned,
        "the leaf's announcement encoder drifted from the pinned bytes — \
         if this is deliberate, the fixture and the core-side pin move with it"
    );

    // And the pinned document verifies through the real verifier.
    let verified =
        announce::verify_announcement(pinned.as_bytes()).expect("the pinned signature verifies");
    assert_eq!(verified.node_id, identity.node_id());
    assert_eq!(
        verified.capabilities,
        vec![
            "leaf".to_string(),
            "net.stream.fragment_reassembly@1".to_string(),
            "stage5.browser".to_string(),
            "transport:rtc".to_string()
        ],
        "the pinned bytes carry the fragment-reassembly negotiation tag \
         as well as the two role tags; a native sender reads it to decide \
         whether a payload above its per-event cap may be fragmented"
    );
    assert_eq!(
        verified.noise_pubkey.as_ref(),
        Some(identity.noise().public_key())
    );
    assert_eq!(verified.rtc_addr, None, "a leaf advertises no RTC socket");
    assert!(
        !pinned.contains("reflex_addr") && !pinned.contains("hop_count"),
        "§7: a leaf omits reflex_addr and originates at hop 0"
    );
}

/// The leaf's nRPC client codec, pinned.
#[test]
fn the_leaf_nrpc_encoder_reproduces_the_pinned_frame() {
    let document: serde_json::Value =
        serde_json::from_str(test_vectors::NRPC_FRAME).expect("the fixture parses");
    let pinned = field(&document, "/hex");
    let route: u64 = field(&document, "/fields/route_canonical_hash")
        .parse()
        .expect("route hash");
    let origin_hash: u64 = field(&document, "/fields/origin_hash")
        .parse()
        .expect("origin hash");
    let call_id: u64 = field(&document, "/fields/call_id")
        .parse()
        .expect("call id");

    // The route must be the canonical hash of the named channel, not
    // an arbitrary number in the fixture.
    assert_eq!(
        route,
        net_wire::channel::name::channel_hash(field(&document, "/fields/route_channel")),
        "the route discriminator must be the canonical channel hash"
    );

    let mut payload = RpcRequestPayload::unary(
        "net.mesh.enroll",
        1_700_000_000_000_000_000,
        bytes::Bytes::from_static(b"join"),
    );
    payload.headers = vec![(
        "content-type".to_string(),
        b"application/octet-stream".to_vec(),
    )];
    let frame = encode_request_frame(origin_hash, call_id, route, &payload).expect("encode");
    assert_eq!(
        hex(&frame),
        pinned,
        "the leaf's nRPC request encoder drifted from the pinned frame"
    );

    // And the pinned bytes decode back through this crate's reader.
    let bytes = unhex(pinned);
    let meta = EventMeta::from_bytes(&bytes).expect("meta");
    assert_eq!(meta.dispatch, DISPATCH_RPC_REQUEST);
    assert_eq!(meta.origin_hash, origin_hash);
    assert_eq!(meta.seq_or_ts, call_id, "the call_id rides seq_or_ts");
    assert_eq!(meta.checksum, 0, "a mesh frame's checksum is zero");
    assert_eq!(decode_route(&bytes), Some(route));
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex digit"))
        .collect()
}

/// The enrollment exchange's device-side objects, pinned.
///
/// This is the third and last of Stage 5's named second copies
/// (`InviteToken` / `JoinRequest` / `JoinOutcome` live in
/// `net-mesh-sdk`, which depends on the core and cannot be linked
/// from wasm). The pin here is one-directional — see the fixture's
/// own `description` for the SDK-side test that closes it — but the
/// signature check below is not cosmetic: it reconstructs the join
/// challenge from the decoded request, which is exactly what the
/// anchor's provider does before it admits anyone.
#[test]
fn the_leaf_enrollment_encoder_reproduces_the_pinned_objects() {
    use net_leaf::enroll::{build_join_request, join_challenge, Invite, JoinOutcome};
    use net_leaf::identity::verify_entity_signature;

    let document: serde_json::Value =
        serde_json::from_str(test_vectors::ENROLL_EXCHANGE).expect("the fixture parses");

    // The invite decodes to the root and nonce the fixture names.
    let invite = Invite::decode(&unhex(field(&document, "/invite/invite_hex"))).expect("invite");
    assert_eq!(hex(&invite.root), field(&document, "/invite/root"));
    assert_eq!(hex(&invite.nonce), field(&document, "/invite/nonce"));
    assert_eq!(
        invite.rendezvous,
        field(&document, "/invite/rendezvous"),
        "the rendezvous must survive the length-prefixed round trip"
    );

    // The signed request reproduces the pinned bytes exactly.
    let identity = fixture_identity();
    let tags = vec!["browser".to_string(), "leaf".to_string()];
    let body = build_join_request(&identity, "chrome-tab", &tags, &invite).expect("build");
    assert_eq!(
        hex(&body),
        field(&document, "/join_request/join_request_hex"),
        "the leaf's enrollment encoder drifted from the pinned request"
    );
    assert!(
        body.len() <= 16 * 1024,
        "the request must stay under §12's body bound"
    );

    // The device's self-signature verifies against a challenge
    // rebuilt from the pinned bytes, not from the builder's inputs.
    let device: [u8; 32] = body[4..36].try_into().expect("device id");
    assert_eq!(hex(&device), field(&document, "/identity/device_entity_id"));
    // magic(4) device(32) nonce(16) root(32) signature(64)
    let signature: [u8; 64] = body[84..148].try_into().expect("signature");
    let challenge = join_challenge(&device, "chrome-tab", &tags, &invite.nonce, &invite.root);
    verify_entity_signature(&device, &challenge, &signature)
        .expect("the pinned request's self-signature must verify");

    // Both outcome shapes decode to what the fixture says they mean.
    let admitted = JoinOutcome::decode(&unhex(field(&document, "/join_outcome/admitted_hex")))
        .expect("admitted decodes");
    assert_eq!(
        admitted.into_chain().expect("tag 0 promotes"),
        field(&document, "/join_outcome/admitted_chain_utf8")
            .as_bytes()
            .to_vec()
    );
    let rejected = JoinOutcome::decode(&unhex(field(&document, "/join_outcome/rejected_hex")))
        .expect("rejected decodes");
    let err = rejected
        .into_chain()
        .expect_err("tag 1 must leave the session provisional");
    let text = format!("{err}");
    assert!(
        text.contains(field(&document, "/join_outcome/rejected_code_name")),
        "the refusal must name its stable code: {text}"
    );
    assert!(
        text.contains(field(&document, "/join_outcome/rejected_message_utf8")),
        "the operator's message must survive: {text}"
    );
}
