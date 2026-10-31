# Post-quantum encryption implementation plan

Status: proposed; implementation has not started. Written 2026-10-31.

## Objective

Give Net hybrid post-quantum session encryption, then migrate identity,
authorization and sealed secrets so a mesh can enforce post-quantum protection
throughout its own trust boundary. Keep the established packet AEAD fast path.
Deliver this as small, reviewable changes with explicit compatibility gates.

Paths below are relative to `net/crates/net/` unless they start with `go/`,
`web/`, `.github/` or `docs/internal/`, which are relative to the repository root.
Names marked **new** are proposed, not existing APIs or files.

## Scope and security claims

| Milestone | Delivered property | Remaining limitation |
|---|---|---|
| M0: key schedule | Secret session keys separated from channel-binding values | Classical public-key algorithms remain |
| M1: hybrid transport | Ephemeral X25519 + ML-KEM-768 session establishment; protection against passive recording for later quantum decryption | Ed25519-only authentication still permits future active quantum impersonation; legacy signed objects and sealed boxes remain |
| M2: hybrid identity and authority | Both Ed25519 and ML-DSA-65 required for node authentication and every in-scope authorization object | Legacy sealed secrets and unconverted external trust systems remain |
| M3: mesh enforcement | Hybrid sealing, enrollment, durable credentials, bindings and migration complete; strict policy refuses weaker peers/objects | External TLS, payment chains and application protocols retain their own security properties |

Do not advertise M1 as a fully post-quantum mesh. Transport security does not
upgrade a stored grant, an Ed25519-only root or an X25519 sealed credential.
Previously recorded traffic and previously copied credentials cannot be
retroactively protected by this rollout.

Threat model: a network attacker records, drops, reorders, replays and alters
traffic; later acquires a cryptographically relevant quantum computer; or is a
malicious mesh member with the domain PSK. Assume OS entropy and uncompromised
endpoints. Test later compromise of long-term keys separately from compromise
of an active session. No protection against an endpoint already reading plaintext.

## Verified starting point

| Surface | Current implementation | Work required |
|---|---|---|
| Handshake | `wire/src/crypto.rs::NoiseHandshake`, `Noise_NKpsk0_25519_ChaChaPoly_BLAKE2s` | Add a separately versioned hybrid handshake |
| Packet encryption | `wire/src/aead.rs`, `wire/src/crypto.rs::PacketCipher` | Retain ChaCha20-Poly1305; change the source of keys |
| Session key derivation | `NoiseHandshake::into_session_keys_with_binding` derives packet/hop keys from `get_handshake_hash()` and discards the Snow transport state | M0 prerequisite: use secret key material; preserve the hash solely for channel binding |
| Routing/session integration | `src/adapter/net/mod.rs` re-exports wire crypto/session; `mesh.rs`, `wire/src/session.rs`, `wire/src/route_hop.rs` | Carry suite, authentication level and generation through all install/replacement paths |
| Browser leaf | `leaf/src/session.rs`, `establish.rs`, `identity.rs`, `bootstrap.rs`, `storage.rs` | Same protocol and cryptographic backend on wasm; leaf has its own Cargo workspace |
| Identity | `src/adapter/net/identity/entity.rs::EntityId(pub [u8; 32])` is an Ed25519 public key; routing ids are derived from it | Add authenticated hybrid key bundles without reinterpreting existing 32-byte ids |
| Authority | `identity/token.rs`, `identity/proof.rs`, `behavior/org*.rs`, `subnet/*.rs`, `behavior/capability.rs` | Version fixed 64-byte signature encodings and verification policy |
| Migrated identity | `identity/envelope.rs`, envelope version 1, 209-byte encoding, X25519 + XChaCha20-Poly1305 | Versioned hybrid recipient sealing and attestation |
| Forwarded credentials | `adapters/mcp/src/forward/aead.rs`, `keys.rs`, `seal.rs` | Hybrid sealing exposed through the SDK; preserve the MCP dependency boundary |
| Bootstrap/enrollment | `sdk/src/enrollment/`, `mesh_enroll.rs`, `bootstrap_credential.rs`; `leaf/src/enroll.rs` | Upgrade the trust bootstrap as well as ordinary sessions |
| Packet limits | `wire/src/protocol.rs`: version 1, maximum packet 8192 bytes | Explicit handshake framing and path-MTU handling; 8192 is not a safe Internet UDP datagram size |

The key-schedule observation is established by source inspection, not an
exploit result. Noise defines `h` for channel binding and derives transport
keys from the chaining key through `Split()` [R3]. The PSK influences `h`, so
do not describe every handshake hash as publicly computable. Nevertheless,
channel-binding disclosure and knowledge available to another PSK holder must
not expose traffic keys. Investigate those cases in M0. Handle any confirmed
security disclosure through the repository's private reporting process.

## Design decisions

### D1. Retain symmetric encryption; replace public-key establishment

Keep ChaCha20-Poly1305, 256-bit keys, directional counters, replay protection
and authenticated headers. Post-quantum primitives execute during handshake,
rotation and signed-control operations, not once per data packet. Existing
XChaCha20-Poly1305 sealed payloads can also retain their AEAD.

Use ML-KEM-768 [R1] in a hybrid with X25519. Prefer an implementation of the
published **X-Wing** construction rather than inventing a concatenation KDF.
Pin the reviewed specification revision and dependency version. The checked
reference is draft-connolly-cfrg-xwing-kem-11, an Internet-Draft, not a final
RFC [R4]. A later construction or incompatible draft revision gets a new suite
identifier and vectors, never a silent change under the same identifier.

X-Wing is a KEM, not an authenticated handshake. The Net handshake around it
needs its own protocol review. Stock Snow 0.10.0 and the existing Noise name
must not be presented as providing this hybrid protocol.

### D2. Add a v2 handshake alongside an explicit legacy profile

Recommended architecture: a versioned hybrid authenticated key exchange in
`net-mesh-wire`, alongside the legacy Noise adapter. Generate a fresh ephemeral
X-Wing encapsulation/decapsulation key pair for each attempt; encapsulate to
that ephemeral key. Do not use a persistent recipient KEM key as the sole
source of session secrecy: later compromise would decrypt recorded sessions.

The v2 design document must freeze these flights before production code:

1. Initiator hello: version, offered suites, policy, fresh attempt nonce,
   ordered peer identities and ephemeral hybrid encapsulation key.
2. Responder response: selected suite, fresh responder nonce, KEM ciphertext,
   identity/key-bundle references and signature over the canonical exchange.
3. Initiator authentication: signature over the exchange and responder proof,
   plus initiator key confirmation under a dedicated derived key.
4. Responder key confirmation and establishment completion.

These are logical flights, independent of fragmentation/retransmission. An
optional stateless retry cookie precedes expensive processing. Exact bytes,
transcript checkpoints, signature inputs, key-confirmation MACs, PSK mixing
and retransmission semantics belong in the reviewed specification from S1.
This outline is not a claim that a new AKE is already proven secure. Prefer an
existing reviewed AKE with these properties if S1 finds a suitable implementation.

For M1, classical signatures authenticate this hybrid exchange; for M2 the
same roles require both signature algorithms. Assign different authentication
profiles and bind the chosen one into the transcript. No application data,
peer admission or `Connected` event before the required proofs and key
confirmation. Do not permit early application data/0-RTT in this version.

The reviewed schedule must use the hybrid KEM's **secret** output and retain
the domain PSK as a separately identified secret input where that deployment
requires one. Specify a standard extract/expand construction, unambiguous
input encoding and labels for TX/RX packet keys, route-hop MACs, confirmation
keys and any exporter. The final transcript is context, never the sole secret
input. Keep exported bindings separate from traffic and exporter secrets.

### D3. Make runtime policy independent of compile-time availability

Proposed SDK policies (**new**, names to finalize in S1):

| Policy | Behavior |
|---|---|
| `LegacyOnly` | Explicit legacy interoperability; never reports PQ protection |
| `PreferHybrid` | Uses hybrid when authenticated peer policy supports it; legacy requires an explicit per-peer/domain allowance |
| `RequireHybridTransport` | Requires hybrid session establishment; accepts the documented M1 authentication limitation |
| `RequirePostQuantum` | Requires hybrid transport, hybrid authentication and upgraded authority/sealing for enabled operations |

A timeout or unauthenticated "unsupported" reply is never permission to retry
with weaker crypto. Before first contact, an M1 Ed25519-only capability record
cannot defend against an active quantum downgrade; document this limitation.
Strict first contact requires a PQ-authenticated root/bundle or an out-of-band
pin. Persist the minimum accepted profile per peer/domain, including key epoch,
and reject older records after restart. In strict mode, an old peer fails with
a typed compatibility error. Administrative rollback must not lower a stored
security floor automatically.

Cargo features only control available implementations. A build without PQ
support must reject PQ-required configuration at construction, not substitute
legacy. Introduce a coherent `post-quantum` feature across wire/core/SDK/leaf
and bindings, then add that graph to CI and local aliases deliberately.

### D4. Use hybrid signatures and explicit trust migration

Use Ed25519 **and** ML-DSA-65 [R2] over the same domain-separated, canonical,
versioned statement. Both must verify for a hybrid object. Do not implement
"either signature is sufficient" or accept a missing PQ signature as legacy
when the object claims the hybrid version. Specify pure ML-DSA/context handling
or a standardized prehash variant explicitly; do not invent a digest convention.

Keep existing `EntityId` values as stable compatibility identifiers. Add a
versioned `IdentityKeyBundle` (**new**) containing that id, algorithm ids,
Ed25519 and ML-DSA public keys, key purpose, epoch and validity. Use a separate
full-length canonical bundle fingerprint for cryptographic pins. Routing's
u64/u32 identifiers are indexes, not authentication credentials.

An Ed25519 cross-signature alone is not a quantum-safe way to install a new
PQ root. Provision initial PQ trust through an already trusted migration
window with explicit recorded pins, or an out-of-band root fingerprint.
Cross-signing helps continuity but does not repair an already compromised
classical root. After migration, rotation/revocation is authorized under the
current required hybrid root and epoch policy. Prevent a newly signed bundle
from resetting a revocation floor or replacing a stronger pin.

Use independent random PQ signing and persistent sealing keys. Do not derive
new PQ private keys from an existing Ed25519 seed: an attacker who later
recovers that seed must not recover the replacement keys. A legacy 32-byte
identity seed remains sufficient for the legacy identity only; recovery of a
hybrid identity requires the new versioned backup/key store as well.

### D5. Bound larger control messages

ML-KEM-768 public keys/ciphertexts are 1184/1088 bytes; ML-DSA-65 public keys
and signatures are 1952/3309 bytes [R5]. X-Wing public keys/ciphertexts are
1216/1120 bytes [R4]. Multiple bundles, grants and signatures can exceed both
the path MTU and Net's current packet bound.

Add bounded handshake/control-object fragmentation at the Net layer. Start
with a 1200-byte total UDP payload target, reduced for Net/routing headers;
measure actual path limits. Do not raise `MAX_PACKET_SIZE` and rely on IP
fragmentation. RTC uses the same logical message bounds and state machine,
with a transport-appropriate fragment size.

Freeze total-message, fragment-count, nesting/delegation-depth, per-peer and
global incomplete-byte limits and expiry in S1. Bind attempt id, message type,
suite, fragment index/count and completed message into the protocol. Duplicate
fragments are idempotent; conflicting duplicates fail. Before authentication,
use bounded parsing, address validation, rate limits and anti-amplification
budgets; a fragment identifier or cookie is not cryptographic peer identity.
Avoid allocating from attacker-supplied lengths or verifying unlimited signatures.

## Execution slices

Each slice updates this document with actual tests/results and leaves its
unfinished dependencies explicit. A checkbox means verified completion, not
"code written". Do not implement later slices by weakening an earlier invariant.

### S0 — Correct and verify the key-schedule boundary (M0)

Files: `wire/src/crypto.rs`, `wire/src/session.rs`, `wire/Cargo.toml`,
`src/adapter/net/mesh.rs`, `leaf/src/session.rs`, `leaf/src/establish.rs`.

- [ ] Trace every consumer of the handshake hash/session binding, packet keys,
  route-hop keys and rekey paths. Include public accessors and debug output.
- [ ] Reproduce the current schedule with two honest peers and a passive
  observer holding only the domain PSK; separately test binding disclosure.
  Record conclusions privately if they demonstrate a security vulnerability.
- [ ] Extract genuine Noise split secrets through a small internal adapter, or
  retain Snow's stateless transport state. Local Snow 0.10.0 exposes
  `dangerously_get_raw_split` behind `risky-raw-split`; if selected, isolate it,
  require a completed handshake and zeroize temporary material. Do not infer
  secret export support from default docs: that feature is currently off.
- [ ] Define directional, purpose-separated derivation from those secrets;
  retain a separate opaque binding value for existing establishment proofs.
- [ ] Version the corrected schedule explicitly. It is a wire incompatibility
  even if packet layout stays identical. No decrypt-with-both-schedules fallback.
- [ ] Witness: learning the binding or the PSK plus public transcript does not
  reconstruct the new traffic keys; initiator TX equals responder RX, and all
  direction/purpose labels produce distinct keys.

Exit: reviewed secret/binding separation, passing native and wasm witnesses,
and a compatibility strategy that does not hide this change inside a PQ flag.
An unchanged legacy profile, if retained temporarily, is labeled separately
and excluded from the corrected security claim.

### S1 — Freeze protocol, provider and wire decisions

Deliverable: **new** `docs/POST_QUANTUM.md`, with a linked decision record for
the chosen AKE and provider. Depends on S0 findings.

- [ ] Freeze suite/authentication identifiers, wire discriminator, exact
  flights, canonical transcript, signature checkpoints, PSK schedule, key
  confirmation, retry cookies, state transitions and failure behavior.
- [ ] Specify how new hellos traverse direct UDP, routed handshakes and RTC
  without an old parser treating them as a valid legacy handshake. Decide
  whether an envelope version is sufficient or packet version must change.
- [ ] Select and pin native/wasm providers against FIPS final ML-KEM/ML-DSA,
  applicable errata, audit evidence, entropy, zeroization and constant-time
  handling. Test target support and stack/heap use. No hand-written lattice math.
- [ ] Evaluate RustCrypto providers as portability candidates, not audited
  defaults: their checked documentation explicitly reports no independent
  audit [R6, R7]. Record alternatives and outstanding review work.
- [ ] Specify initial trust, migration epochs, persisted minimum profiles,
  object size limits and all compatibility behavior. Use D1-D5 as the baseline.
- [ ] Obtain protocol/security review before production-enabling the new AKE.
  A feature-gated prototype and vectors may proceed while review is pending.

Exit: a byte-level design an independent implementation can reproduce.
No production release based only on this high-level plan.

### S2 — Implement isolated primitives and codecs

Files (**new**): `wire/src/crypto/{suite,hybrid,transcript,handshake}.rs` as an
internal layout if useful; update `wire/src/crypto.rs`, `wire/src/lib.rs` and
manifests without changing public re-export paths accidentally.

- [ ] Implement typed, length-checked suite/key/ciphertext wrappers and the
  selected provider adapter. Keep deterministic randomness test-only.
- [ ] Implement the exact pinned hybrid KEM construction and vectors; do not
  apply X-Wing's combiner to arbitrary independently arranged DH exchanges.
- [ ] Implement bounded canonical codecs, secret-owning types with redacted
  formatting, zeroization, and error types that reveal no secret data.
- [ ] Treat malformed encodings separately from KEM implicit rejection.
  Well-formed altered ciphertext must fail confirmation/authentication without
  exposing an invalid-ciphertext oracle. Follow the selected construction's
  validation rules rather than adding incompatible component checks.
- [ ] Verify final FIPS vectors, pinned hybrid vectors, cross-provider
  agreement, malformed lengths, RNG failure and native/wasm parity.

Exit: primitives/codecs pass independently; no network behavior changes yet.

### S3 — Add bounded handshake framing

Files: `wire/src/protocol.rs`, **new** handshake framing module,
`src/adapter/net/transport.rs`, routed handling in `mesh.rs`, `leaf/src/rtc.rs`.

- [ ] Implement S1's fragment/reassembly limits, cookies and per-source/global
  budgets before exposing expensive PQ verification to arbitrary datagrams.
- [ ] Cover loss, reorder, duplicate/conflicting fragments, truncation,
  integer overflow, expiration, repeated attempts and cancellation/shutdown.
- [ ] Retransmit cached bytes from the same logical attempt; restart with
  fresh ephemeral secrets and nonce. Never reuse an attempt across peers.
- [ ] Test full credential chains larger than one packet and prove routed
  forwarding does not require relays to assemble endpoint secrets.

Exit: bounded memory/CPU and successful exchange at the chosen UDP budget,
including a constrained-MTU test. No dependency on IP fragmentation.

### S4 — Integrate hybrid sessions and rotation (M1)

Files: `wire/src/crypto.rs`, `wire/src/session.rs`, `wire/src/route_hop.rs`,
`src/adapter/net/mod.rs`, `mesh.rs`, `leaf/src/session.rs`, `node.rs`, `establish.rs`.

- [ ] Implement the reviewed flights, classical M1 authentication, secret
  schedule and both confirmations. Preserve the provisional-session boundary.
- [ ] Thread authenticated suite/profile/epoch through direct, relayed and RTC
  establishment, admission and session installation. Bind both peer identities,
  roles, all offers/selection, key material and attempt nonces.
- [ ] Apply runtime policy and stored floors before installation. Unknown
  suites, stripped offers, stale credentials and failed proofs fail closed.
- [ ] Integrate full fresh hybrid re-handshake for rotation, expiry/revocation
  and restart. Refresh dependent subscription state using existing generation
  fencing; stale completion must not replace a newer session.
- [ ] Keep nonce ownership, counter exhaustion rejection, replay windows and
  hop-key separation. Cap any old-generation drain window; packets cannot
  fall through to a weaker suite after an authentication failure.
- [ ] Exercise native/native, native/leaf and leaf/leaf, plus a relay that only
  forwards endpoint handshake bytes. Pin deliberate legacy compatibility tests.

Exit: M1 conformance and no early data/admission. Erased ephemeral KEM secrets
cannot be regenerated from long-term identity, PSK or persisted configuration.

### S5 — Persist hybrid identities and trust anchors

Files: `identity/entity.rs`, `identity/state.rs`, `sdk/src/identity.rs`,
`leaf/src/identity.rs`, `leaf/src/storage.rs`, CLI identity/authority storage.

- [ ] Add versioned bundles, hybrid signing/verification, full fingerprints,
  epochs, key-purpose separation and atomic key-store upgrades.
- [ ] Preserve legacy ids and routing-id derivation; bind them to the full
  bundle when authenticating. Do not replace `EntityId` with a PQ key array.
- [ ] Implement independent key generation, backup/import, explicit PQ-root
  pinning and migration. Preserve Unix secret permissions and Windows DACL gates.
- [ ] Test crash recovery, partial writes, backup restore, corrupt records,
  rollback, key replacement and lost PQ keys. A legacy seed restore must not
  claim to have restored the hybrid identity.

Exit: stable peer identity across restart, independently provisioned PQ trust,
and no silent identity regeneration or trust-floor reset.

### S6 — Upgrade authentication and authorization (M2)

Files: `identity/{token,proof,envelope}.rs`, `behavior/capability.rs`,
`behavior/org*.rs`, `subnet/*.rs`, `traversal/blind_relay.rs`, corresponding
leaf `org/`, announcement and establishment code, SDK credential codecs.

- [ ] Complete an issuer/verifier inventory, including capability forwarding,
  org descriptors/grants/revocations/admission, RPC invocation statements,
  permission delegation, subnet floors, relay authorizations and durable logs.
  Mark every signature surface converted, explicitly excluded, or blocked.
- [ ] Add versioned hybrid object encodings and both-signatures-required
  verification. Keep legacy readers explicit; reject unknown or inconsistent
  version/algorithm/length combinations, trailing data and noncanonical objects.
- [ ] Bind epoch, issuer, subject, audience, scope and validity to signatures;
  maintain existing authority, replay, delegation and generation checks.
- [ ] Extend S3 to large signed control objects and nested chains. Cache only
  verified bundles with bounded lifetime and epoch-aware invalidation.
- [ ] Require hybrid authentication in v2's M2 profile. Prevent admission via
  an unconverted classical side path even when the session is hybrid.
- [ ] Migrate persisted authority/revocation state and define retained-history
  verification. Old signatures remain legacy evidence, not upgraded proofs.

Exit: strict mode rejects every Ed25519-only authorization path in the inventory.
Maintain the existing named routing/admission witnesses and CI minimum floors.

### S7 — Upgrade sealed identities and forwarded secrets

Files: `identity/envelope.rs`, `subprotocol/migration_handler.rs`, SDK sealing
APIs, `adapters/mcp/src/forward/{aead,keys,seal}.rs`.

- [ ] Add a versioned recipient hybrid sealing-key bundle with purpose/epoch
  authenticated under the required identity policy. Persistent KEM keys are
  acceptable for these sealed objects, with their documented compromise limit.
- [ ] Use a reviewed KEM/DEM envelope schedule, fresh encapsulation, existing
  XChaCha20-Poly1305 and authenticated suite/version, recipient fingerprint,
  purpose, object id and migration/causal context. Upgrade sender attestation.
- [ ] Migrate the complete versioned identity secret bundle, not just the old
  32-byte Ed25519 seed. Strict mode refuses sealing to a legacy-only recipient.
- [ ] Put sealing/key discovery on `net-mesh-sdk` first. MCP must use the SDK,
  preserving `adapters/mcp/tests/dependency_boundary.rs`.
- [ ] Test wrong recipient, retargeting, stale epoch, corrupted encapsulation,
  replay, substituted bundle, relay observation and secret redaction.

Exit: no in-scope long-lived secret relies solely on X25519 wrapping. Document
that later compromise of a persistent recipient sealing key can expose old
objects; rotation alone does not provide session-style forward secrecy.

### S8 — Upgrade enrollment, bootstrap and discovery

Files: `sdk/src/enrollment/`, `mesh_enroll.rs`, `bootstrap_credential.rs`,
`leaf/src/{bootstrap,enroll,announce}.rs`, `behavior/enrollment_storage.rs`,
`cli/src/commands/enrollment.rs`, capability schema and discovery codecs.

- [ ] Upgrade invite, redeem, join, renew, rendezvous and first-contact flows;
  inventory actual enrollment crypto separately from ordinary mesh Noise.
- [ ] Carry bounded authenticated bundle references, required profiles and
  trust-root fingerprints. Large bundles need a bounded authenticated fetch
  mechanism or fragmentation; never place private keys in discovery records.
- [ ] Give strict invitations a PQ-authenticated root or securely delivered
  pin. HTTPS/DTLS alone is not evidence that Net's PQ bootstrap is complete.
- [ ] Persist minimum policy/epoch in enrollment state and backups. An old
  invitation must not lower an already enrolled peer's floor on renewal.
- [ ] Test direct/relay/RTC joins, offline root provisioning, restart, expired
  invites, single-use redemption, lost responses and downgrade attempts.

Exit: a new strict mesh can bootstrap without an Ed25519-only trust decision.

### S9 — Expose policy and status across SDKs and bindings

Files: Rust SDK first; `bindings/node/`, `sdk-ts/`, `bindings/python/`,
`sdk-py/`, `src/ffi/`, `bindings/go/`, `include/`, `go/`, CLI and deck surfaces.

- [ ] Expose policy, bundle import/export, trusted fingerprints and typed
  compatibility failures. Expose negotiated KEM/authentication/object policy
  separately; do not reduce everything to a misleading `isQuantumSafe` bool.
- [ ] Preserve existing constructors with documented migration behavior;
  explicit PQ requirements never degrade because a binding omitted an option.
- [ ] Use versioned C config structs or additive symbols. Update Rust C ABI,
  headers, Go mirrors and ABI tests together; keep the single `net-ffi` cdylib.
- [ ] Implement shared serialization/verification in pure-language mirrors
  where applicable. Route live transport through existing native engines;
  explicitly reject unsupported crypto rather than implementing a second AKE.
- [ ] Add cross-language vectors for bundles, transcripts, credentials,
  errors and envelopes, with consuming Rust/Go/TS/Python tests. Include wasm
  execution, not just wasm compile checks.

Exit: all supported clients can request and inspect the same security policy,
and mixed-language live peers obey it.

### S10 — Adversarial validation, CI and performance

Files: **new** `tests/pq_handshake.rs`, `pq_policy.rs`, `pq_migration.rs`,
`tests/cross_lang_pq/`; wire/leaf unit tests, fuzz targets, benches,
`.github/workflows/ci.yml`, `.config/nextest.toml` and `.cargo/config.toml`.

- [ ] Pin every new root integration binary in CI. Set retries to zero for
  PQ security suites; add required witness names/count guards where useful.
- [ ] Add the PQ feature to shared test aliases/CI families coherently; cover
  builds with/without it, wasm/browser, Windows secret storage and Unix gates.
- [ ] Fuzz codecs, fragmented messages, credential chains and state-machine
  transitions under resource budgets. Test cancellation and no leaked secret
  state on every error/timeout path.
- [ ] Use deterministic test seams to neutralize one hybrid component at a
  time and verify the other secret still affects keys. These are wiring
  witnesses, not proofs of the combiner's cryptographic security.
- [ ] Mutation witnesses: omit the PQ KEM secret, omit either required
  signature, skip confirmation, remove peer/role/policy/epoch binding, accept
  stripped offers, reset a persisted floor, permit early delivery. Each
  mutation must make its named witness fail.
- [ ] Benchmark handshake CPU/p50/p95/p99, flight count, bytes/fragments,
  retransmits at fixed loss, concurrent-handshake memory/CPU, rekey churn,
  signature verification/cache behavior and wasm bundle/startup cost.
- [ ] Record before/after established-session throughput and latency; target
  no material packet-path regression (investigate >5% under the same setup).
  Set numeric handshake/load budgets from measured baselines before rollout,
  rather than inventing a latency target here.

Exit: external review findings resolved or explicitly block release; all
affected CI families green; measured limits and compatibility evidence recorded.

### S11 — Roll out and document (M3)

- [ ] Ship experimental opt-in hybrid transport with an accurate M1 claim.
  Keep full `RequirePostQuantum` unavailable until S5-S9 are complete.
- [ ] Exercise a mixed-version test mesh, then provision PQ roots/bundles,
  reissue authority objects, upgrade sealed-secret recipients and retire
  classical-only credentials under a recorded migration epoch.
- [ ] Enable preferred hybrid for upgraded domains with explicit legacy
  allowances; raise strict floors only after coverage evidence is complete.
  Rollback preserves floors or fails closed with an administrative diagnostic.
- [ ] Expose nonsecret suite/authentication, compatibility failures and
  migration status. Do not log PSKs, seeds, raw split/exporter keys or secret
  state; avoid treating a key fingerprint as permission by itself.
- [ ] Update `docs/{TRANSPORT,IDENTITY,ORGANIZATIONS,CAPABILITIES_SCHEMA}.md`,
  SDK/binding examples, recovery docs and web docs. Mirror release notes with
  `bun run sync:releases`; run `bun run check` from `web/`.
- [ ] State exclusions precisely: external TLS/DTLS and payment/wallet
  signatures are not upgraded by this feature. Audit nonsecret content hashes
  by required security property separately; retaining them is not a blanket
  claim about all hash-based identifiers under quantum attack.

Exit: strict deployment passes the full security matrix below and operational
recovery/rollback rehearsal. Only then describe the in-scope mesh as enforcing
post-quantum protection.

## Dependency order and suggested PR boundaries

```text
S0 -> S1 -> S2 -> S3 -> S4                         M1
      S1 -> S5 -> S6 (also needs S3/S4)            M2
                  S7 (needs S2/S5/S6)
                  S8 (needs S4/S5/S6)
S4..S8 -> S9 -> S10 -> S11                        M3
```

S10's witnesses and CI pins land with each implementing slice; its final step
aggregates evidence. Do not defer security tests until the end. Split S6 by
credential family and S9 by binding when needed, while leaving strict mode
disabled until all required paths are complete. Keep S0's compatibility change
separate from provider import, framing, transport and identity migrations.

## Required security/compatibility matrix

| Scenario | Required result |
|---|---|
| PQ-enabled peers, direct UDP / routed / native RTC / browser RTC | Same authenticated suite, keys, binding and policy; application delivery only after confirmation |
| Hybrid transport with classical authentication | Report M1 explicitly; full strict policy refuses it |
| Old peer with explicit legacy allowance | Legacy only; never report PQ protection |
| Old peer, absent PQ build or unknown suite under strict policy | Typed failure, no weaker retry and no application data |
| Altered offer/selection, peer id, role, bundle, nonce or key epoch | Authentication/confirmation fails |
| Missing/invalid classical or PQ signature on hybrid object | Reject; both are required |
| Wrong PSK / outside-domain peer / malicious PSK-holding member | Reject unauthorized admission; domain membership is not individual identity |
| Replayed proof, stale grant, rotated key, reset profile floor | Reject under persisted epoch/revocation policy, including after restart |
| Loss/reorder/duplicate/timeout/fragment flood | Bounded work/state and deterministic cleanup; no early delivery |
| Session rotation races and process restart | Fresh secrets/counters; stale attempts cannot install or erase newer state |
| Later compromise of identity keys and domain PSK | Does not recover erased ephemeral hybrid session secrets; distinguish persistent sealed-object limits |
| Corrupt/retargeted identity migration or forwarded credential | No plaintext released and no fallback envelope decoding |
| Cross-language vectors and live sessions | Identical canonical bytes/policy decisions; both positive and negative vectors consumed |

## Validation commands and repository constraints

Run Rust commands from `net/crates/net/`. Use `cargo check --lib` for the
implementation loop, then the narrowest meaningful nextest selection. After
S1 adds PQ support to the established alias graphs, focused examples are:

```powershell
cargo check --lib
cargo check -p net-mesh-wire --features post-quantum
cargo tf --test pq_handshake --retries 0
cargo tf --test pq_policy --retries 0
cargo tf --test pq_migration --retries 0
cargo tfl org_routing_wiring_tests --retries 0
cargo test -p net-mesh-wire --features post-quantum
```

The PQ names above are planned additions; they do not exist at plan time.
The wire crate's units are a separate target from root `--lib` units. Doctests
are separate as well. Use `--no-tests=fail` (already in the aliases) and do not
paste changing CI feature lists into local mutation loops. Run the applicable
full family once after focused checks pass, as described in `TESTS.md`.

Before calling an implementation branch ready, run the complete Rust pre-push
checklist in `AGENTS.md`: Windows-aware formatting, workspace/all-target check,
strict production clippy variants, permissive test-target clippy and rustdoc
with warnings denied. Include touched workspace members with CI's actual
features; add PQ to the relevant doc jobs. Format/build/test the leaf separately
through its own workspace (invoke Cargo from the Rust root with
`--manifest-path leaf/Cargo.toml` where appropriate). Preserve its wasm boundary.

For bindings, use CI's actual native build feature sets: cgo enabled with the
single cdylib, napi plus TS build/tests, maturin with real CPython plus pytest,
pure TS/Python mirror tests and real browser/wasm execution. Public doc comments
may not link private items. Do not lower existing security witness floors to
make a refactor green; preserve or replace witnesses deliberately in CI.

## Instructions for the implementing coding agent

1. Read `AGENTS.md`, `TESTS.md`, this plan and current crypto/session code.
   Use CodeGraph for structural exploration and literal search for formats,
   feature wiring and protocol constants. Re-check facts against the current
   branch; line numbers in past reviews are not stable interfaces.
2. Begin with S0. Produce a failing meaningful witness before changing the
   secret/binding boundary, and document the corrected profile's compatibility.
3. Complete S1 before treating any proposed v2 flight or API name as frozen.
   This plan proposes the architecture; the reviewed byte-level design governs
   implementation. Never improvise a KEM combiner or signature convention.
4. Work slice by slice. For each PR, state the problem, resulting behavior,
   exact compatibility impact, tests actually run and remaining claim limits.
   Update checkboxes only when the slice's exit criteria have evidence.
5. If provider portability, review findings or first-contact trust make a slice
   infeasible, record the concrete blocker and continue independent authorized
   work. Do not replace a required invariant with a permissive fallback.
6. Never change security claims, algorithm identifiers or persisted floors
   merely to complete a rollout. M1, M2 and M3 have different exit criteria.

## Primary references checked for this plan

- **R1:** [NIST FIPS 203: ML-KEM](https://csrc.nist.gov/pubs/fips/203/final).
  Use the final algorithm and applicable errata, not pre-standard Kyber formats.
- **R2:** [NIST FIPS 204: ML-DSA](https://csrc.nist.gov/pubs/fips/204/final).
  Check errata when freezing the implementation profile.
- **R3:** [Noise Protocol Framework, symmetric state and Split](https://noiseprotocol.org/noise.html#the-symmetricstate-object).
  Channel binding and transport key derivation are separate purposes.
- **R4:** [X-Wing draft-11](https://www.ietf.org/archive/id/draft-connolly-cfrg-xwing-kem-11.html)
  and [document status](https://datatracker.ietf.org/doc/draft-connolly-cfrg-xwing-kem/).
  Hybrid KEM construction and encoding; work in progress, not a complete AKE.
- **R5:** [RFC 9958: Post-Quantum Cryptography for Engineers](https://www.rfc-editor.org/rfc/rfc9958.html).
  Parameter sizes and deployment considerations.
- **R6:** [RustCrypto ml-kem documentation](https://docs.rs/ml-kem/0.3.3/ml_kem/).
  Checked provider candidate; zeroization feature and audit limitations need review.
- **R7:** [RustCrypto ml-dsa documentation](https://docs.rs/ml-dsa/0.1.1/ml_dsa/).
  Checked provider candidate; documentation reports no independent audit.
- **R8:** [Snow 0.10.0](https://docs.rs/snow/0.10.0/snow/).
  Confirm feature-gated APIs against the pinned local source before use.

References were checked on 2026-10-31. Revalidate versions, errata and draft
status during S1; this plan does not imply a FIPS-validated implementation.
