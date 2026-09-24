//! OSDK-L X1 + S4Vectors — generate the `tests/cross_lang_org/` fixtures.
//!
//! The `org:` error vocabulary and the streaming OPENING envelope are the
//! contracts four language runtimes parse. Both fixtures are derived from the
//! ONE Rust source of those contracts (`OrgSdkError::to_wire` /
//! `parse_org_wire`, and the `org_call` codec that mints and verifies
//! `OrgStreamCallProof`), so neither can drift from the code by being
//! hand-edited:
//!
//! ```text
//! # write both fixtures
//! cargo run -p net-mesh-sdk --features net,cortex,fixtures --example gen_org_error_fixtures
//! # drift guard + the Rust-side conformance rows (the fixture authority)
//! cargo run -p net-mesh-sdk --features net,cortex,fixtures --example gen_org_error_fixtures -- --check
//! ```
//!
//! `error_vectors.json`'s drift guard also lives as a `cargo test`
//! (`src/org/tests_fixture.rs`). `streaming_opening_vectors.json`'s drift
//! guard IS the `--check` mode here: generator and checker share this ONE
//! render implementation, so a fixture whose generator and checker could
//! disagree cannot exist. Adding or renaming a kind fails every binding's
//! suite until each is updated; a codec layout change fails `--check` until
//! the fixture is regenerated — and then fails every runtime's byte pins.
//! That chain is the point: a change cannot be silent in any language.
//!
//! `--mint-frozen` exists ONCE-PER-FIXTURE-SCHEMA: credential issuance is
//! nondeterministic by design (RNG nonces + wall clock; the deterministic
//! `issue_at` seams are `pub(crate)`), so the fixture's credential blobs are
//! FROZEN constants minted by this mode and pasted below. Re-minting changes
//! every vector and is never part of regeneration.

use net::adapter::net::behavior::org::{OrgError, OrgKeypair, OrgMembershipCert};
use net::adapter::net::behavior::org_call::{
    OrgCallProof, OrgStreamCallProof, MAX_ORG_CALL_PROOF_BYTES, ORG_CALL_BINDING_CONTEXT,
    ORG_STREAM_CALL_BINDING_CONTEXT, STREAM_CALL_KIND_CLIENT_STREAMING, STREAM_CALL_KIND_DUPLEX,
    STREAM_CALL_KIND_SERVER_STREAMING,
};
use net::adapter::net::behavior::org_grant::{
    CapabilityAuthorityId, DispatcherScope, GrantRights, GrantTargetScope, OrgCapabilityGrant,
    OrgDispatcherGrant,
};
use net::adapter::net::identity::EntityKeypair;
use net_sdk::org::parse_org_wire;

use base64::Engine as _;
use serde_json::{json, Map, Value};

// ===========================================================================
// The frozen identity seeds — the SAME four-party shape as the live X2
// scenario (`sdk/src/org/fixtures.rs`): org A owns the caller entity, org B
// owns the provider entity, and the cross-org grant is B -> A over
// `nrpc:customer.read`. Keeping the seeds identical means the fixture's
// derived ids match every live harness's manifest expectations.
// ===========================================================================

/// Caller entity seed (org A's member; signs both transcript domains).
const CALLER_SEED: [u8; 32] = [0x22; 32];
/// Provider (callee) entity seed.
const PROVIDER_SEED: [u8; 32] = [0x11; 32];
/// Org A root seed — the acting org.
const ORG_A_SEED: [u8; 32] = [0xA1; 32];
/// Org B root seed — the provider org / grant issuer.
const ORG_B_SEED: [u8; 32] = [0xB2; 32];
/// The capability every vector binds.
const CAPABILITY_TAG: &str = "nrpc:customer.read";

/// Fixed proof expiry (2030-03-17T16:26:40Z) — time is never read at
/// generation; `check_expiry` is deliberately out of the fixture's scope.
const PROOF_EXPIRES_AT_UNIX_NS: u64 = 1_900_000_000_000_000_000;
/// Fixed call id (postcard varint — exercises a 2-byte varint on the wire).
const CALL_ID: u64 = 42;
/// Fixed canonical-request digest.
const REQUEST_DIGEST: [u8; 32] = [0x11; 32];
/// Fixed session binding (the Noise handshake hash a real caller signs).
const SESSION_BINDING: [u8; 32] = [0x5A; 32];

// ===========================================================================
// The frozen issuance chain — `tests/cross_lang_org/streaming_opening_frozen_credentials.json`,
// the ONE-TIME output of `--mint-frozen` (see the module docs: issuance is
// nondeterministic, so these bytes are schema input, not regeneration
// output). `--check` verifies their signatures and canonical round-trip; the
// opening proofs bind their digests into the signed transcripts.
// ===========================================================================

/// The five credential blobs an opening vector chain binds.
struct Frozen {
    membership_a: Vec<u8>,
    dispatcher_a: Vec<u8>,
    grant_b_to_a: Vec<u8>,
    membership_b: Vec<u8>,
    dispatcher_b: Vec<u8>,
}

fn frozen_path() -> std::path::PathBuf {
    fixture_path("streaming_opening_frozen_credentials.json")
}

fn frozen() -> Frozen {
    let text = std::fs::read_to_string(frozen_path()).unwrap_or_else(|e| {
        panic!(
            "read streaming_opening_frozen_credentials.json ({e}) — mint it ONCE with \
             `--mint-frozen` (a schema change, never maintenance)"
        )
    });
    let doc: Value = serde_json::from_str(&text).expect("parse frozen credentials");
    let blob = |key: &str| unhex(doc[key].as_str().expect(key));
    Frozen {
        membership_a: blob("membership_a_hex"),
        dispatcher_a: blob("dispatcher_a_hex"),
        grant_b_to_a: blob("grant_b_to_a_hex"),
        membership_b: blob("membership_b_hex"),
        dispatcher_b: blob("dispatcher_b_hex"),
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// ONE-TIME mint for the frozen chain. Never part of regeneration — re-run
/// only when the fixture SCHEMA changes, and expect every opening vector to
/// change with it.
fn mint_frozen() {
    let org_a = OrgKeypair::from_bytes(ORG_A_SEED);
    let org_b = OrgKeypair::from_bytes(ORG_B_SEED);
    let caller = EntityKeypair::from_bytes(CALLER_SEED);

    let membership_a =
        OrgMembershipCert::try_issue(&org_a, caller.entity_id().clone(), 1, 3600).expect("mint A");
    let dispatcher_a = OrgDispatcherGrant::try_issue(
        &org_a,
        caller.entity_id().clone(),
        DispatcherScope::Exact(CapabilityAuthorityId::for_tag(CAPABILITY_TAG)),
        3600,
    )
    .expect("mint A dispatcher");
    let (grant_b_to_a, _audience) = OrgCapabilityGrant::try_issue(
        &org_b,
        org_a.org_id(),
        CapabilityAuthorityId::for_tag(CAPABILITY_TAG),
        GrantRights::INVOKE,
        GrantTargetScope::ExactNode(provider_entity()),
        3600,
    )
    .expect("mint grant");
    let membership_b =
        OrgMembershipCert::try_issue(&org_b, caller.entity_id().clone(), 2, 3600).expect("mint B");
    let dispatcher_b = OrgDispatcherGrant::try_issue(
        &org_b,
        caller.entity_id().clone(),
        DispatcherScope::Exact(CapabilityAuthorityId::for_tag(CAPABILITY_TAG)),
        3600,
    )
    .expect("mint B dispatcher");

    let doc = json!({
        "description": "S4Vectors — the frozen issuance chain behind `streaming_opening_vectors.json`'s opening vectors. ONE-TIME output of `cargo run -p net-mesh-sdk --features net,cortex,fixtures --example gen_org_error_fixtures -- --mint-frozen`; NEVER part of regeneration (issuance is nondeterministic by design — RNG nonces + wall clock). The blobs are codec INPUTS: `--check` verifies their signatures and canonical round-trip but never their expiry windows. Re-minting changes every opening vector and is a schema change, not maintenance.",
        "membership_a_hex": hex(&membership_a.to_bytes()),
        "dispatcher_a_hex": hex(&dispatcher_a.to_bytes()),
        "grant_b_to_a_hex": hex(&grant_b_to_a.to_bytes()),
        "membership_b_hex": hex(&membership_b.to_bytes()),
        "dispatcher_b_hex": hex(&dispatcher_b.to_bytes()),
    });
    let path = frozen_path();
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create fixture dir");
    let mut out = serde_json::to_string_pretty(&doc).expect("render frozen");
    out.push('\n');
    std::fs::write(&path, out).expect("write frozen credentials");
    println!("wrote {}", path.display());
}

fn caller_key() -> EntityKeypair {
    EntityKeypair::from_bytes(CALLER_SEED)
}

fn caller_entity() -> net::adapter::net::identity::EntityId {
    caller_key().entity_id().clone()
}

fn provider_entity() -> net::adapter::net::identity::EntityId {
    EntityKeypair::from_bytes(PROVIDER_SEED).entity_id().clone()
}

fn org_a() -> OrgKeypair {
    OrgKeypair::from_bytes(ORG_A_SEED)
}

fn org_b() -> OrgKeypair {
    OrgKeypair::from_bytes(ORG_B_SEED)
}

/// Which issuance chain an opening vector binds.
#[derive(Clone, Copy, PartialEq)]
enum Access {
    /// Cross-org: org A membership + dispatcher + org B's capability grant.
    Granted,
    /// Owner-delegated: org B membership + dispatcher, no capability grant.
    OwnerDelegated,
}

impl Access {
    fn name(self) -> &'static str {
        match self {
            Access::Granted => "granted",
            Access::OwnerDelegated => "owner_delegated",
        }
    }
}

/// Build the unary + streaming proofs for one vector's fixed inputs.
fn build_proofs(frozen: &Frozen, access: Access, kind: u8) -> (OrgCallProof, OrgStreamCallProof) {
    let caller = caller_key();
    let (acting_org, provider_org, membership, dispatcher, grant) = match access {
        Access::Granted => (
            org_a().org_id(),
            org_b().org_id(),
            OrgMembershipCert::from_bytes(&frozen.membership_a).expect("membership A"),
            OrgDispatcherGrant::from_bytes(&frozen.dispatcher_a).expect("dispatcher A"),
            Some(OrgCapabilityGrant::from_bytes(&frozen.grant_b_to_a).expect("grant")),
        ),
        Access::OwnerDelegated => (
            org_b().org_id(),
            org_b().org_id(),
            OrgMembershipCert::from_bytes(&frozen.membership_b).expect("membership B"),
            OrgDispatcherGrant::from_bytes(&frozen.dispatcher_b).expect("dispatcher B"),
            None,
        ),
    };
    let cap = CapabilityAuthorityId::for_tag(CAPABILITY_TAG);
    let unary = OrgCallProof::sign_for_call(
        &caller,
        membership.clone(),
        dispatcher.clone(),
        grant.clone(),
        acting_org,
        provider_org,
        provider_entity(),
        CALL_ID,
        CapabilityAuthorityId::for_tag(CAPABILITY_TAG),
        PROOF_EXPIRES_AT_UNIX_NS,
        REQUEST_DIGEST,
    );
    let stream = OrgStreamCallProof::sign_for_stream_call(
        &caller,
        membership,
        dispatcher,
        grant,
        acting_org,
        provider_org,
        provider_entity(),
        CALL_ID,
        cap,
        PROOF_EXPIRES_AT_UNIX_NS,
        REQUEST_DIGEST,
        kind,
        SESSION_BINDING,
    );
    (unary, stream)
}

/// Postcard serializes `call_binding_sig` as bytes-with-length: 1 length byte
/// + 64 signature bytes (the in-core `SIG_WIRE` arithmetic).
const SIG_WIRE: usize = 65;
/// The streaming suffix: `kind: u8` + `session_binding: [u8; 32]`.
const STREAM_SUFFIX: usize = 33;

fn opening_vector(frozen: &Frozen, kind: u8, kind_name: &str, access: Access) -> Value {
    let (unary, stream) = build_proofs(frozen, access, kind);
    let unary_wire = unary.encode().expect("encode unary");
    let wire = stream.encode().expect("encode stream");
    assert_eq!(
        wire.len(),
        unary_wire.len() + STREAM_SUFFIX,
        "suffix arithmetic"
    );
    let pre_sig_len = wire.len() - STREAM_SUFFIX - SIG_WIRE;
    assert_eq!(pre_sig_len, unary_wire.len() - SIG_WIRE);
    let pre_sig = wire[..pre_sig_len].to_vec();
    assert_eq!(
        pre_sig[..],
        unary_wire[..pre_sig_len],
        "the pre-signature prefix is shared"
    );

    json!({
        "id": format!("opening.{kind_name}.{}", access.name()),
        "roles": ["caller_emits", "provider_accepts"],
        "kind": kind,
        "kind_name": kind_name,
        "access": access.name(),
        "call_id": CALL_ID.to_string(),
        "proof_expires_at_unix_ns": PROOF_EXPIRES_AT_UNIX_NS.to_string(),
        "request_digest_hex": hex(&REQUEST_DIGEST),
        "session_binding_hex": hex(&SESSION_BINDING),
        "call_binding_sig_hex": hex(&stream.call_binding_sig),
        "unary_call_binding_sig_hex": hex(&unary.call_binding_sig),
        "wire_hex": hex(&wire),
        "wire_base64": b64(&wire),
        "wire_len": wire.len(),
        "unary_wire_hex": hex(&unary_wire),
        "unary_wire_base64": b64(&unary_wire),
        "unary_wire_len": unary_wire.len(),
        "pre_sig_prefix_hex": hex(&pre_sig),
        "expect": {
            "decode": "ok",
            "verify": "ok",
            "kind": kind,
            "session_binding_hex": hex(&SESSION_BINDING),
            "sig_domain_separated": stream.call_binding_sig != unary.call_binding_sig,
        },
    })
}

fn reject_vector(id: &str, base_id: &str, mutation: &str, base: &[u8], wire: Vec<u8>) -> Value {
    json!({
        "id": id,
        "roles": ["provider_accepts"],
        "base": base_id,
        "mutation": mutation,
        "wire_hex": hex(&wire),
        "wire_base64": b64(&wire),
        "wire_len": wire.len(),
        "base_wire_len": base.len(),
        "expect_error_display": OrgError::InvalidFormat.to_string(),
    })
}

/// The vocabulary + unclassified cases, with the byte-pin fields the other
/// sections carry (`wire_base64` / `wire_len` over the `wire` string's UTF-8
/// bytes). The content itself comes from the ONE vocabulary source.
fn pinned_vocabulary() -> Value {
    let parsed: Value =
        serde_json::from_str(&net_sdk::org::render_error_vectors()).expect("parse vocabulary");
    let mut out = Map::new();
    for (key, value) in parsed.as_object().expect("object") {
        match key.as_str() {
            "vectors" | "unclassified_cases" => {
                let pinned: Vec<Value> = value
                    .as_array()
                    .expect("array")
                    .iter()
                    .map(|v| {
                        let mut item = v.as_object().expect("object").clone();
                        let wire: Vec<u8> =
                            item["wire"].as_str().expect("wire").as_bytes().to_vec();
                        item.insert("wire_base64".into(), json!(b64(&wire)));
                        item.insert("wire_len".into(), json!(wire.len()));
                        Value::Object(item)
                    })
                    .collect();
                out.insert(key.clone(), Value::Array(pinned));
            }
            _ => {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(out)
}

/// Render the canonical `streaming_opening_vectors.json` content — the ONE
/// implementation the writer and the `--check` drift guard share.
fn render_streaming_opening_vectors() -> String {
    let frozen = frozen();
    let mut opening = Vec::new();
    for kind in [
        STREAM_CALL_KIND_SERVER_STREAMING,
        STREAM_CALL_KIND_CLIENT_STREAMING,
        STREAM_CALL_KIND_DUPLEX,
    ] {
        let kind_name = match kind {
            STREAM_CALL_KIND_SERVER_STREAMING => "server_streaming",
            STREAM_CALL_KIND_CLIENT_STREAMING => "client_streaming",
            _ => "duplex",
        };
        for access in [Access::Granted, Access::OwnerDelegated] {
            opening.push(opening_vector(&frozen, kind, kind_name, access));
        }
    }

    // Decoder rejections — §1.1's strict full-consumption decode. Every one
    // must be refused with the one `invalid wire format` error.
    let base_id = "opening.server_streaming.granted";
    let (_, base_stream) =
        build_proofs(&frozen, Access::Granted, STREAM_CALL_KIND_SERVER_STREAMING);
    let base = base_stream.encode().expect("encode base");
    let mut rejects = Vec::new();

    let mut trailing = base.clone();
    trailing.push(0x00);
    rejects.push(reject_vector(
        "reject.trailing_byte",
        base_id,
        "append one 0x00 byte after the suffix",
        &base,
        trailing,
    ));
    for (id, cut, mutation) in [
        (
            "reject.truncated_len_minus_1",
            1usize,
            "cut the final byte (mid suffix)",
        ),
        (
            "reject.truncated_len_minus_17",
            17,
            "cut 17 bytes (mid suffix)",
        ),
        (
            "reject.truncated_len_minus_33",
            33,
            "cut the whole 33-byte suffix",
        ),
    ] {
        rejects.push(reject_vector(
            id,
            base_id,
            mutation,
            &base,
            base[..base.len() - cut].to_vec(),
        ));
    }
    for (id, bad_kind) in [
        ("reject.kind_0", 0u8),
        ("reject.kind_4", 4u8),
        ("reject.kind_255", 255u8),
    ] {
        let mut wire = base.clone();
        let at = wire.len() - STREAM_SUFFIX;
        wire[at] = bad_kind;
        rejects.push(reject_vector(
            id,
            base_id,
            &format!("overwrite the kind byte with {bad_kind}"),
            &base,
            wire,
        ));
    }
    let mut oversize = base.clone();
    oversize.resize(MAX_ORG_CALL_PROOF_BYTES + 1, 0);
    rejects.push(reject_vector(
        "reject.over_cap",
        base_id,
        "pad with zeros past MAX_ORG_CALL_PROOF_BYTES (1024)",
        &base,
        oversize,
    ));

    // A signature-level rejection: the envelope decodes, but the call binding
    // refuses it. Decode success must never become verify success.
    let mut sig_flipped = base.clone();
    let sig_at = sig_flipped.len() - STREAM_SUFFIX - SIG_WIRE + 1; // past the 0x41 length byte
    sig_flipped[sig_at] ^= 0xFF;
    let (_, flipped_proof) = (
        (),
        OrgStreamCallProof::decode(&sig_flipped).expect("decodes"),
    );
    let verify_err = flipped_proof
        .binding_for_stream_verify(
            org_b().org_id(),
            provider_entity(),
            CALL_ID,
            CapabilityAuthorityId::for_tag(CAPABILITY_TAG),
            REQUEST_DIGEST,
        )
        .verify(&flipped_proof.call_binding_sig)
        .expect_err("a flipped signature must not verify")
        .to_string();
    let sig_reject = json!({
        "id": "reject.signature_flipped",
        "roles": ["provider_accepts"],
        "base": base_id,
        "mutation": "xor 0xFF over the first signature byte",
        "wire_hex": hex(&sig_flipped),
        "wire_base64": b64(&sig_flipped),
        "wire_len": sig_flipped.len(),
        "base_wire_len": base.len(),
        "expect_decode": "ok",
        "expect_verify_error_display": verify_err,
    });

    let doc = json!({
        "description": "S4Vectors — the org streaming OPENING envelope + the frozen `org:` error vocabulary, byte-exact, for cross-language conformance. Each `wire_*` pin carries the SAME bytes in two encodings (`wire_hex`/`wire_base64` over the envelope bytes, or over the `wire` string's UTF-8 bytes) plus `wire_len`: a runtime MUST recover identical bytes from both encodings before any other handling of a vector (the byte-for-byte pin — a decoder that mangles either side disagrees with the fixture and must not become success). u64 values travel as decimal STRINGS (`call_id`, `proof_expires_at_unix_ns`) and MUST round-trip exactly. GENERATED — do not hand-edit; run `cargo run -p net-mesh-sdk --features net,cortex,fixtures --example gen_org_error_fixtures`.",
        "version": 1,
        "prefix": "org:",
        "layout": {
            "unary_transcript_context": ORG_CALL_BINDING_CONTEXT,
            "stream_transcript_context": ORG_STREAM_CALL_BINDING_CONTEXT,
            "unary_transcript_width": 304,
            "stream_transcript_width": 337,
            "admission_header": "net-org-admission",
            "stream_suffix_len": STREAM_SUFFIX,
            "session_binding_len": 32,
            "signature_wire_len": SIG_WIRE,
            "max_proof_bytes": MAX_ORG_CALL_PROOF_BYTES,
            "membership_wire_size": 156,
            "dispatcher_grant_wire_size": 185,
            "capability_grant_wire_size": 318,
            "kind_values": {
                "never_emitted": 0,
                "server_streaming": STREAM_CALL_KIND_SERVER_STREAMING,
                "client_streaming": STREAM_CALL_KIND_CLIENT_STREAMING,
                "duplex": STREAM_CALL_KIND_DUPLEX,
            },
        },
        "ids": {
            "acting_org_granted_hex": hex(org_a().org_id().as_bytes()),
            "acting_org_owner_delegated_hex": hex(org_b().org_id().as_bytes()),
            "provider_org_hex": hex(org_b().org_id().as_bytes()),
            "caller_entity_hex": hex(caller_entity().as_bytes()),
            "callee_entity_hex": hex(provider_entity().as_bytes()),
            "capability_tag": CAPABILITY_TAG,
        },
        "credentials": {
            "note": "The frozen issuance chain for the opening vectors (streaming_opening_frozen_credentials.json — minted once via --mint-frozen; issuance is nondeterministic by design). The live mixed pair uses the fresh gen_org_scenario chain under the same seeds.",
            "membership_a_hex": hex(&frozen.membership_a),
            "dispatcher_a_hex": hex(&frozen.dispatcher_a),
            "grant_b_to_a_hex": hex(&frozen.grant_b_to_a),
            "membership_b_hex": hex(&frozen.membership_b),
            "dispatcher_b_hex": hex(&frozen.dispatcher_b),
        },
        "opening_vectors": Value::Array(opening),
        "decoder_rejects": Value::Array(rejects),
        "signature_rejects": Value::Array(vec![sig_reject]),
        "error_vocabulary": pinned_vocabulary(),
        "scenarios": {
            "mixed_pair": {
                "id": "mixed_pair.go_caller_python_provider.server_streaming",
                "roles": ["go_caller", "python_provider"],
                "chain": "gen_org_scenario (the X2 cross-org granted chain, same seeds as `ids`)",
                "service": "customer.read",
                "granted_capability_tag": CAPABILITY_TAG,
                "shape": "server_streaming",
                "request_hex": "5334562d73747265616d696e6d00ff01",
                "chunks_hex": ["010203f00d0a", "7365636f6e64", "00ff544849524421"],
                "expect_handler": {
                    "entity_hex": hex(caller_entity().as_bytes()),
                    "acting_org_hex": hex(org_a().org_id().as_bytes()),
                    "provider_org_hex": hex(org_b().org_id().as_bytes()),
                    "capability_hex": hex(CapabilityAuthorityId::for_tag(CAPABILITY_TAG).as_bytes()),
                    "is_same_org": false,
                },
                "expect_terminal": "eof",
                "chunk_count": 3,
            },
        },
        "notes": [
            "Four harness-integrity rules, non-negotiable in every runtime: malformed/unknown errors, narrowing IDs, callback loss, or decoder disagreement MUST NOT become success.",
            "Malformed/unknown errors: `unclassified_cases` MUST classify as `unknown` (never a canonical domain — that would assert a request reached a provider and its admission engine evaluated it).",
            "Narrowing IDs: the vocabulary's detail strings carry NARROWED id displays (16 hex chars + \"...\"). A narrowed display MUST NOT compare equal to, or be upgraded into, a full 32-byte id.",
            "Callback loss: a handler, sink or stream callback that never fires is a FAILURE (timeout), never an empty success. Assert exact chunk counts.",
            "Decoder disagreement: any runtime whose decode of the byte pins, the u64 strings, or the `org:` grammar disagrees with these vectors FAILS that vector's row.",
            "`admission_denied` vectors carry the coarse bucket and NOTHING else — a precise remote reason would be a credential oracle (OA2-E2).",
            "`org:rpc:` reuses the frozen nRPC kind vocabulary rather than minting second names for the same conditions.",
        ],
    });

    let mut out = serde_json::to_string_pretty(&doc).expect("render");
    out.push('\n');
    out
}

// ===========================================================================
// `--check`: the drift guard + the Rust-side conformance rows. Rust is the
// fixture authority; these rows are what that authority claims.
// ===========================================================================

fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("tests")
        .join("cross_lang_org")
        .join(name)
}

/// One named conformance row: `Ok(name)` passes, `Err(name + detail)` fails.
type Row = Result<String, String>;

fn row(name: &str, detail: Result<(), String>) -> Row {
    match detail {
        Ok(()) => Ok(name.to_string()),
        Err(d) => Err(format!("{name}: {d}")),
    }
}

fn check_streaming_opening_vectors() -> Vec<Row> {
    let path = fixture_path("streaming_opening_vectors.json");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut rows = Vec::new();

    rows.push(row(
        "drift.streaming_opening_vectors",
        if text == render_streaming_opening_vectors() {
            Ok(())
        } else {
            Err("the checked-in fixture is stale — regenerate with `cargo run -p net-mesh-sdk --features net,cortex,fixtures --example gen_org_error_fixtures`".into())
        },
    ));

    // The frozen chain is REAL: every blob round-trips canonically and its
    // signature verifies (`from_bytes` alone does not verify signatures), so a
    // corrupt or counterfeited blob fails here before any vector depends on it.
    {
        let f = frozen();
        macro_rules! credential_row {
            ($name:literal, $parse:path, $bytes:expr) => {{
                rows.push(row(
                    concat!("credentials.", $name),
                    (|| {
                        let bytes: &[u8] = $bytes;
                        let cred = $parse(bytes).map_err(|e| format!("from_bytes: {e}"))?;
                        cred.verify().map_err(|e| format!("signature: {e}"))?;
                        if cred.to_bytes() != bytes {
                            return Err("bytes are not canonical (to_bytes != stored)".into());
                        }
                        Ok(())
                    })(),
                ));
            }};
        }
        credential_row!(
            "membership_a",
            OrgMembershipCert::from_bytes,
            &f.membership_a
        );
        credential_row!(
            "dispatcher_a",
            OrgDispatcherGrant::from_bytes,
            &f.dispatcher_a
        );
        credential_row!(
            "grant_b_to_a",
            OrgCapabilityGrant::from_bytes,
            &f.grant_b_to_a
        );
        credential_row!(
            "membership_b",
            OrgMembershipCert::from_bytes,
            &f.membership_b
        );
        credential_row!(
            "dispatcher_b",
            OrgDispatcherGrant::from_bytes,
            &f.dispatcher_b
        );
    }

    let doc: Value = serde_json::from_str(&text).expect("fixture parses");

    // The byte-pin fields are self-consistent in the fixture itself.
    for section in ["opening_vectors", "decoder_rejects", "signature_rejects"] {
        for v in doc[section].as_array().expect(section) {
            let id = v["id"].as_str().expect("id");
            rows.push(row(
                &format!("bytes.{id}"),
                (|| {
                    let wire = unhex(v["wire_hex"].as_str().expect("wire_hex"));
                    if wire.len() != v["wire_len"].as_u64().expect("wire_len") as usize {
                        return Err("wire_len disagrees with wire_hex".into());
                    }
                    if b64(&wire) != v["wire_base64"].as_str().expect("wire_base64") {
                        return Err("wire_base64 disagrees with wire_hex".into());
                    }
                    Ok(())
                })(),
            ));
        }
    }
    for v in doc["error_vocabulary"]["vectors"]
        .as_array()
        .expect("vocab")
    {
        let wire = v["wire"].as_str().expect("wire");
        let name = format!(
            "{}.{}",
            v["domain"].as_str().expect("domain"),
            v["kind"].as_str().expect("kind")
        );
        rows.push(row(
            &format!("bytes.vocab.{name}"),
            (|| {
                let bytes = wire.as_bytes();
                if bytes.len() != v["wire_len"].as_u64().expect("wire_len") as usize {
                    return Err("wire_len disagrees with wire".into());
                }
                if b64(bytes) != v["wire_base64"].as_str().expect("wire_base64") {
                    return Err("wire_base64 disagrees with wire".into());
                }
                Ok(())
            })(),
        ));
    }
    for (i, v) in doc["error_vocabulary"]["unclassified_cases"]
        .as_array()
        .expect("unclassified")
        .iter()
        .enumerate()
    {
        let wire = v["wire"].as_str().expect("wire");
        rows.push(row(
            &format!("bytes.unclassified[{i}]"),
            (|| {
                let bytes = wire.as_bytes();
                if bytes.len() != v["wire_len"].as_u64().expect("wire_len") as usize {
                    return Err("wire_len disagrees with wire".into());
                }
                if b64(bytes) != v["wire_base64"].as_str().expect("wire_base64") {
                    return Err("wire_base64 disagrees with wire".into());
                }
                Ok(())
            })(),
        ));
    }

    // Per opening vector: full codec round-trip, declared facts, the binding
    // signature, §1.1 prefix compatibility, and transcript-domain separation.
    for v in doc["opening_vectors"].as_array().expect("opening") {
        let id = v["id"].as_str().expect("id").to_string();
        let wire = unhex(v["wire_hex"].as_str().expect("wire_hex"));
        let unary_wire = unhex(v["unary_wire_hex"].as_str().expect("unary_wire_hex"));
        let proof = match OrgStreamCallProof::decode(&wire) {
            Ok(p) => p,
            Err(e) => {
                rows.push(Err(format!("decode.{id}: decode failed: {e}")));
                continue;
            }
        };
        rows.push(row(
            &format!("round_trip.{id}"),
            match proof.encode() {
                Ok(re) if re == wire => Ok(()),
                Ok(re) => Err(format!(
                    "re-encode differs ({} vs {} bytes)",
                    re.len(),
                    wire.len()
                )),
                Err(e) => Err(format!("re-encode failed: {e}")),
            },
        ));
        rows.push(row(
            &format!("facts.{id}"),
            (|| {
                let want_kind = v["expect"]["kind"].as_u64().expect("kind") as u8;
                if proof.kind != want_kind {
                    return Err(format!("kind {} != {want_kind}", proof.kind));
                }
                if hex(&proof.session_binding)
                    != v["expect"]["session_binding_hex"].as_str().expect("sb")
                {
                    return Err("session_binding differs".into());
                }
                if proof.proof_expires_at_unix_ns.to_string()
                    != v["proof_expires_at_unix_ns"].as_str().expect("expiry")
                {
                    return Err("proof_expires_at_unix_ns differs".into());
                }
                if hex(&proof.call_binding_sig) != v["call_binding_sig_hex"].as_str().expect("sig")
                {
                    return Err("call_binding_sig differs".into());
                }
                let access = v["access"].as_str().expect("access");
                let grant_expected = access == "granted";
                if proof.capability_grant.is_some() != grant_expected {
                    return Err(format!("capability_grant presence != {grant_expected}"));
                }
                Ok(())
            })(),
        ));
        rows.push(row(&format!("verify.{id}"), {
            let access = v["access"].as_str().expect("access");
            let provider_org = org_b().org_id();
            proof
                .binding_for_stream_verify(
                    provider_org,
                    provider_entity(),
                    CALL_ID,
                    CapabilityAuthorityId::for_tag(CAPABILITY_TAG),
                    REQUEST_DIGEST,
                )
                .verify(&proof.call_binding_sig)
                .map_err(|e| format!("{access} chain refused its own binding: {e}"))
        }));
        rows.push(row(
            &format!("prefix.{id}"),
            (|| {
                if wire.len() != unary_wire.len() + STREAM_SUFFIX {
                    return Err("stream wire is not unary wire + the 33-byte suffix".into());
                }
                let pre_sig_len = wire.len() - STREAM_SUFFIX - SIG_WIRE;
                let want_pre = unhex(v["pre_sig_prefix_hex"].as_str().expect("pre_sig"));
                if wire[..pre_sig_len] != want_pre[..] || unary_wire[..pre_sig_len] != want_pre[..]
                {
                    return Err(
                        "the pre-signature prefix is not shared with the unary encoding".into(),
                    );
                }
                if wire[wire.len() - STREAM_SUFFIX] != proof.kind {
                    return Err("the suffix kind byte is not at len-33".into());
                }
                if wire[wire.len() - STREAM_SUFFIX + 1..] != proof.session_binding[..] {
                    return Err("the suffix session binding is not at len-32".into());
                }
                // §1.1 structural prefix compatibility: the trailing-tolerant
                // unary decoder recovers the five leading fields EXACTLY.
                let decoded = OrgCallProof::decode(&wire)
                    .map_err(|e| format!("the unary decoder refused the stream prefix: {e}"))?;
                if decoded.caller_membership != proof.caller_membership
                    || decoded.dispatcher_grant != proof.dispatcher_grant
                    || decoded.capability_grant != proof.capability_grant
                    || decoded.proof_expires_at_unix_ns != proof.proof_expires_at_unix_ns
                    || decoded.call_binding_sig != proof.call_binding_sig
                {
                    return Err("the unary decoder's recovered prefix differs".into());
                }
                Ok(())
            })(),
        ));
        rows.push(row(
            &format!("domain_separation.{id}"),
            (|| {
                if proof.call_binding_sig
                    == unhex(v["unary_call_binding_sig_hex"].as_str().expect("usig"))[..]
                {
                    return Err(
                        "the stream transcript signed the same bytes as the unary transcript"
                            .into(),
                    );
                }
                let unary = OrgCallProof::decode(&unary_wire).map_err(|e| e.to_string())?;
                unary
                    .binding_for_verify(
                        org_b().org_id(),
                        provider_entity(),
                        CALL_ID,
                        CapabilityAuthorityId::for_tag(CAPABILITY_TAG),
                        REQUEST_DIGEST,
                    )
                    .verify(&unary.call_binding_sig)
                    .map_err(|e| format!("the unary transcript refused its own signature: {e}"))
            })(),
        ));
    }

    // Decoder rejections: strict full-consumption decode must refuse every one.
    for v in doc["decoder_rejects"].as_array().expect("rejects") {
        let id = v["id"].as_str().expect("id").to_string();
        let wire = unhex(v["wire_hex"].as_str().expect("wire_hex"));
        rows.push(row(&format!("reject.{id}"), {
            let want = v["expect_error_display"].as_str().expect("expect");
            match OrgStreamCallProof::decode(&wire) {
                Err(e) if e.to_string() == want => Ok(()),
                Err(e) => Err(format!("refused with {e:?}, want {want}")),
                Ok(_) => Err(
                    "decoded successfully — decoder disagreement must not become success".into(),
                ),
            }
        }));
    }

    // Signature rejection: decode OK, binding verify refuses.
    for v in doc["signature_rejects"].as_array().expect("sig rejects") {
        let id = v["id"].as_str().expect("id").to_string();
        let wire = unhex(v["wire_hex"].as_str().expect("wire_hex"));
        rows.push(row(
            &format!("sig_reject.{id}"),
            (|| {
                let proof = OrgStreamCallProof::decode(&wire)
                    .map_err(|e| format!("decode must succeed before verify refuses (got {e})"))?;
                let want = v["expect_verify_error_display"].as_str().expect("expect");
                match proof
                    .binding_for_stream_verify(
                        org_b().org_id(),
                        provider_entity(),
                        CALL_ID,
                        CapabilityAuthorityId::for_tag(CAPABILITY_TAG),
                        REQUEST_DIGEST,
                    )
                    .verify(&proof.call_binding_sig)
                {
                    Err(e) if e.to_string() == want => Ok(()),
                    Err(e) => Err(format!("verify refused with {e:?}, want {want}")),
                    Ok(()) => Err(
                        "verified successfully — a flipped signature must not become success"
                            .into(),
                    ),
                }
            })(),
        ));
    }

    // The frozen vocabulary parses back to exactly what it declares.
    for v in doc["error_vocabulary"]["vectors"]
        .as_array()
        .expect("vocab")
    {
        let wire = v["wire"].as_str().expect("wire");
        let name = format!(
            "{}.{}",
            v["domain"].as_str().expect("domain"),
            v["kind"].as_str().expect("kind")
        );
        rows.push(row(
            &format!("vocab.{name}"),
            (|| {
                let (domain, kind) = parse_org_wire(wire);
                if domain.as_wire() != v["domain"].as_str().expect("domain") {
                    return Err(format!("domain {} differs", domain.as_wire()));
                }
                if kind != Some(v["kind"].as_str().expect("kind")) {
                    return Err(format!("kind {kind:?} differs"));
                }
                if domain.is_local() != v["is_local"].as_bool().expect("is_local") {
                    return Err("is_local differs".into());
                }
                Ok(())
            })(),
        ));
    }
    for (i, v) in doc["error_vocabulary"]["unclassified_cases"]
        .as_array()
        .expect("unclassified")
        .iter()
        .enumerate()
    {
        rows.push(row(
            &format!("unclassified[{i}]"),
            (|| {
                let (domain, kind) = parse_org_wire(v["wire"].as_str().expect("wire"));
                if domain.as_wire() != "unknown" {
                    return Err(format!(
                        "classified as {} — must never become success",
                        domain.as_wire()
                    ));
                }
                if domain.as_wire() != v["expect_domain"].as_str().expect("expect") {
                    return Err("expect_domain disagrees with the parser".into());
                }
                if kind.is_some() {
                    return Err("an unclassifiable wire exposed a kind".into());
                }
                if domain.is_local() {
                    return Err("an unclassifiable wire claimed locality".into());
                }
                Ok(())
            })(),
        ));
    }

    // Layout constants come from the codec itself — a rename or resize is loud.
    rows.push(row(
        "layout.max_proof_bytes",
        if MAX_ORG_CALL_PROOF_BYTES == 1024 {
            Ok(())
        } else {
            Err(format!(
                "MAX_ORG_CALL_PROOF_BYTES is {MAX_ORG_CALL_PROOF_BYTES}, fixture pins 1024"
            ))
        },
    ));
    rows.push(row("layout.kind_values", {
        let l = &doc["layout"]["kind_values"];
        if l["server_streaming"].as_u64() == Some(STREAM_CALL_KIND_SERVER_STREAMING.into())
            && l["client_streaming"].as_u64() == Some(STREAM_CALL_KIND_CLIENT_STREAMING.into())
            && l["duplex"].as_u64() == Some(STREAM_CALL_KIND_DUPLEX.into())
            && l["never_emitted"].as_u64() == Some(0)
        {
            Ok(())
        } else {
            Err("the pinned kind values differ from the codec constants".into())
        }
    }));
    rows.push(row("layout.transcript_contexts", {
        let l = &doc["layout"];
        if l["unary_transcript_context"].as_str() == Some(ORG_CALL_BINDING_CONTEXT)
            && l["stream_transcript_context"].as_str() == Some(ORG_STREAM_CALL_BINDING_CONTEXT)
        {
            Ok(())
        } else {
            Err("the pinned transcript contexts differ from the codec constants".into())
        }
    }));

    rows
}

fn fixture_text(name: &str) -> String {
    std::fs::read_to_string(fixture_path(name)).unwrap_or_else(|e| panic!("read {name}: {e}"))
}

fn run_check() -> i32 {
    let mut rows = check_streaming_opening_vectors();
    rows.push(row(
        "drift.error_vectors",
        if fixture_text("error_vectors.json") == net_sdk::org::render_error_vectors() {
            Ok(())
        } else {
            Err("error_vectors.json is stale — regenerate".into())
        },
    ));
    let mut failed = 0;
    for r in &rows {
        match r {
            Ok(name) => println!("PASS {name}"),
            Err(detail) => {
                println!("FAIL {detail}");
                failed += 1;
            }
        }
    }
    println!("== {} rows, {} failed", rows.len(), failed);
    if failed > 0 {
        1
    } else {
        0
    }
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        None => {
            let error_path = fixture_path("error_vectors.json");
            std::fs::create_dir_all(error_path.parent().expect("parent"))
                .expect("create fixture dir");
            std::fs::write(&error_path, net_sdk::org::render_error_vectors())
                .expect("write error fixture");
            println!("wrote {}", error_path.display());
            let streaming_path = fixture_path("streaming_opening_vectors.json");
            std::fs::write(&streaming_path, render_streaming_opening_vectors())
                .expect("write streaming fixture");
            println!("wrote {}", streaming_path.display());
        }
        Some("--check") => std::process::exit(run_check()),
        Some("--mint-frozen") => mint_frozen(),
        Some(other) => {
            eprintln!("unknown mode {other:?} — use (write), --check or --mint-frozen");
            std::process::exit(2);
        }
    }
}
