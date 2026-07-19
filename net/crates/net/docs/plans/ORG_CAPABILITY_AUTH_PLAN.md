# Organization Capability Auth Plan (OA)

**Version:** v1.3 — applies review-7: the legacy-gate integration
correction for OA-2 (`may_execute` aggregates allow-lists
TARGET-WIDE across all of a node's entries, so an empty allow-list
on the protected service is not a dependable pass-through —
protected services now use policy-directed gate selection with a
narrow `has_local_capability` check, §2.4a), the stronger seam
witness matrix (§OA-4), and three OA-3 carry-forwards: the zero
grant_id is reserved (grant issuance/decode rejects it), envelope
dedup identity includes `grant_id`, and secret-bearing runtime
types are structurally non-serializable.
**Status (2026-07-19, branch `org-capability-auth`):**

- **OA-1** — IMPLEMENTED and iterated through reviews 8–11 plus the
  amended/consolidated closure rounds (revocation store, adopt ceremony,
  fold projection, MeshNode install lifecycle, send seqlock). OA-1's
  revocation-store hardening is Gate 1 of the OA-2 admission gates; its
  latest closures (R3-2 poison survives sidecar recreation, R3-3
  existing-handle sidecar-identity check, R3-4 externally-owned
  subscription + safe self-unsubscription) are landed and **Gate 1 —
  SIGNED OFF (Kyra)**.
- **OA-2** — internal + cross-org admission is IMPLEMENTED end to end. The
  nRPC **E0 substrate** (registration, channel/service equality, RpcRouteV1
  discriminator, direct-session identity, one clock sample) is landed (Gate 3);
  the **E1 provider-admission primitives** (`RegisteredRpcService`,
  `verify_org_admission`, replay guard, canonical digest, admission stamp) are
  landed (Gate 2 SIGNED OFF). The atomic **E1 live wiring + E2 caller seam**
  (`serve_rpc_protected`, the live gate, `RpcContext.org_admission`,
  `RpcStatus 0x0009`, the proof-intent builder — internally `#47`) is landed and
  **SIGNED OFF / CLOSED** (2026-07-19, `512cd1588`), with live two-node
  transport, provider-state, and mixed-version witnesses in
  `tests/integration_nrpc_protected.rs`. **OA2-F** — CLI/SDK grant management
  (`net org grant-dispatcher` / `grant-capability` incl. `--discover`, the
  `net_sdk::org` grant re-exports) plus the §2.6 exit-gate closure witnesses
  (no `discovery_key` in the proof/header; installed-secret commitment
  mismatch → local reject; `tests/org_admission_wire.rs`) — is landed. See
  `docs/plans/OA2E_INTEGRATION_DESIGN.md` for the E0/E1/E2 decomposition and the
  live-gate ledger.
- **OA-3 / OA-4** — held behind their staged reviews (next in sequence).

`may_execute` (`capability_bridge.rs`) is byte-for-byte unchanged across
the whole OA-2 series; every fix is red-witnessed.

**Scope — the compact core:**

```
membership proves who you belong to
dispatcher grant proves who you act for
provider grant proves where you may look and what you may invoke
scoped announcement privately carries the capability descriptor
provider verifies the exact call
```

**Invariants (pinned):** membership is never invocation authority;
decrypting an announcement is never invocation authority; the
audience credential grants only knowledge; visibility is never
admission; provider-local policy is final; authentication is never
replay prevention; ownership is singular; **the OrgAdmission gate —
not a legacy allow-list — supplies the authority for protected
services (red-witnessed, §OA-4)**; revocation monotonicity survives
restart.

---

## Model

```
IDENTITY
    OrgId = ed25519 verifying key. Self-certifying; root offline;
    issuance occasional, never per-call.

AUTHORITY OBJECTS

1. OrgMembershipCert                 S ∈ A     (scaffolded; one
     { org_id, member, not_before,             node one owner;
       not_after, generation, nonce, sig }     ~1y silent renewal)

2. OrgDispatcherGrant                A → S     (fixed one-hop,
     { org_id, dispatcher,                     org-root-signed,
       capability_scope: Exact | Any,          days–weeks TTL)
       not_before, not_after, nonce, sig }

3. OrgCapabilityGrant                B → A
     { grant_id: [u8;32],
       issuer_org: B, grantee_org: A,
       capability: CapabilityAuthorityId,
       rights: GrantRights,                    // DISCOVER | INVOKE
       target_scope: ExactNode(EntityId) | AnyNodeOwnedBy(OrgId),  // EntityId, not the short NodeId (collision-safe; OA2-F)
       discovery: Option<GrantedDiscoveryBinding>,
       not_before, not_after, nonce, sig }

   GrantedDiscoveryBinding {                   // in the SIGNED grant
       audience_handle: [u8; 32],              // random, per grant
       key_commitment:  [u8; 32],              // blake3 derive_key(
   }                                           //  "net-org-audience-
                                               //   commit-v1", key)

   OrgAudienceSecret {                         // LOCAL FILE, out of
       grant_id, audience_handle,              // band, never on the
       discovery_key: [u8; 32],                // wire, never in a
   }                                           // proof

   STRUCTURAL RULE (v1):
       rights ⊇ DISCOVER  ⇔  discovery == Some(..)
       one DISCOVER grant ⇔ one unique handle ⇔ one unique key.
       No audience reuse across grants.

4. ScopedCapabilityAnnouncement      P ⇒ audience   (§OA-3)

5. OrgCallProof                      S → P, one call
     { caller_membership, dispatcher_grant,
       capability_grant: Option<OrgCapabilityGrant>,
       proof_expires_at_unix_ns, call_binding_sig }
     Carries the SIGNED grant (with commitment) only — the raw
     discovery key never rides a call (witnessed).

CAPABILITY REGISTRATION (provider-local; the §2.4a seam)
    RegisteredCapability { descriptor, visibility, admission }
    Local registry/fold ALWAYS receives the capability;
    EMISSION projects by visibility:
        Public          → plaintext CAP-ANN
        OwnerScoped     → plaintext scoped form, excluded from
                          unscoped query projection (fanout
                          control, not confidentiality)
        GrantedAudience → encrypted ScopedCapabilityAnnouncement
                          ONLY

AUDIENCE SCOPES (fold partitioning; mutually invisible)
    enum CapabilityAudienceScope {
        Public,
        Owner { org_id, audience_handle },
        Grant { grant_id, audience_handle },
    }

ADMISSION (per registered service)
    enum OrgAdmission { PublicAuthenticated, OwnerDelegated,
                        CrossOrgGranted }

REVOCATION (v1) — local, operator-distributed, RESTART-DURABLE
    OrgRevocationBundle (signed floors) merged into a persisted
    OrgRevocationState maxima file; atomically written BEFORE the
    live view updates; a lower bundle never rolls back, including
    across restart. Grants: expiry + non-renewal; provider-local
    deny is immediate. Audience: per-grant rotation.

AUDIT IDENTITY: actor S, acting for org A, under grant from
provider org B, invoked capability C, on exact provider P — never
"A invoked B."
```

---

## OA-1 — scaffolded ownership (stop and review)

### 1.1–1.4 — as v1.1

`behavior/org.rs` types (`OrgId` with derived non-ct `PartialEq` —
public value, documented contrast with bearer-secret ids;
`OrgKeypair`; `OrgMembershipCert`; `OrgRevocationBundle`), canonical
signed layouts, strict decode, `verify_strict`, domains
(`net-org-cert-v1`, `net-org-floors-v1`), ~1y/2y TTLs, token-module
skew. `NodeAuthorityConfig` at adopt; loud startup
self-verification; one node one owner. Scaffolding installs the
owner audience credential as its own versioned file (see file
layout below). Announcement field
`owner_cert: Option<OrgMembershipCert>` with
`SignedPayloadCanonical` lockstep; fold projection
`owner_org: Option<OrgId>` from ingest-verified certs only
(this projection also implements `OwnerScoped` query filtering and
the caller-side `AnyNodeOwnedBy` precheck). **`may_execute`
untouched; no org execute axis; no `org:` tag.** Uncached
verification + bench. CLI: `net org keygen`, `issue-cert`,
`issue-floors`, `net node adopt`.

Config file layout (separately versioned; visibility key material
is not membership and does not ride certificate-renewal
semantics):

```
owner-membership.json      // NodeAuthorityConfig + owner_cert
owner-audience.key         // owner audience handle + key
revocation-state.json      // persisted floor maxima (§1.5)
```

### 1.5 Restart-persistent revocation maxima (review-6 §4)

In-memory monotone merge is insufficient: if config management
replaces the bundle with an OLDER valid signed bundle and the node
restarts, there is no prior maximum to compare against. The minimum
fix — NOT the deferred WAL/replication system — is one small atomic
local file:

```rust
OrgRevocationState {
    floors: BTreeMap<(OrgId, EntityId), u32>,
}
```

Reload path (locked order):

```
verify incoming bundle signature
→ merge maxima with PERSISTED state (monotone; lower never wins)
→ atomically write merged maxima
     (write temp → fsync temp → atomic rename → fsync parent dir)
→ ONLY THEN publish the new live view
```

Failure handling: corrupt incoming bundle → keep persisted
last-good, log loudly (Q1-prior resolved). Corrupt persisted maxima
file → LOUD startup failure — protected verification never starts
against silently weaker floors.

### 1.6 OA-1 exit gate

As v1.1 (byte identity + canonical proptest; golden vectors;
cert/floor matrix; ingest drops bad certs not announcements;
authority-dark `may_execute` pin; adopt loud failure; mixed-fleet
fail-closed pins) PLUS the restart witness, verbatim:

```
load floor generation 5 → persist
replace operator bundle with VALID generation 3
restart
→ generation 5 remains authoritative
```

and: corrupt persisted maxima → loud startup failure; reload of a
corrupt bundle → last-good retained. Stop. Review.

---

## OA-2 — internal and cross-org admission (stop and review)

> **Implementation note (2026-07-19):** §§2.1–2.5 landed as
> `behavior/org_grant.rs` (§2.1–2.2), `behavior/org_call.rs` (§2.3),
> `behavior/org_admission_replay.rs` (§2.5), `behavior/org_admission.rs`
> (§2.4 `verify_org_admission`), and `adapter/net/org_admission_gate.rs` (E1
> provider self-verify + canonical digest + admission stamp); their audit is
> **Gate 2 — SIGNED OFF**. The §2.4a registration/gate seam is LIVE-WIRED
> through `serve_rpc_protected` + the unary bridge (internally `#47`, **SIGNED
> OFF / CLOSED** 2026-07-19, `512cd1588`), with live two-node witnesses in
> `tests/integration_nrpc_protected.rs`. §2.6 is EXERCISED: golden vectors,
> grant matrices (incl. `rights ⊇ DISCOVER ⇔ binding` + the `AnyNodeOwnedBy`
> owner rule), binding transplant, replay, header/streaming/reason mapping, the
> no-`discovery_key` byte-scan, and the installed-secret commitment mismatch
> (`tests/org_admission_wire.rs` plus the `org_grant` / `org_call` units).
> **OA2-F** (CLI/SDK grant management) is landed. See the top-of-file status and
> `OA2E_INTEGRATION_DESIGN.md`.

### 2.1 `CapabilityAuthorityId` — as v1.1

Deterministic 32-byte `blake3::derive_key("net-org-capability-v1",
canonical identity)`; authorization scope only; never a locator or
secret.

### 2.2 Grants — commitments in, keys out (review-6 §1–2)

`OrgDispatcherGrant` as v1.1. `OrgCapabilityGrant` per the model:
the SIGNED grant carries `GrantedDiscoveryBinding { audience_handle,
key_commitment }`; the raw key lives only in the local
`OrgAudienceSecret` file, delivered out of band to B's publishing
nodes and A's consuming nodes, validated against the commitment
(`key_commitment == blake3 derive_key("net-org-audience-commit-v1",
discovery_key)`). The key therefore never transits RPC headers,
tracing/debug paths, denial logs, or provider surfaces
(witnessed in OA-2's gate).

Structural rule enforced at issue AND decode:
`rights ⊇ DISCOVER ⇔ discovery.is_some()`. One DISCOVER grant, one
unique handle, one unique key — `net org grant-capability
--discover` always mints fresh audience material; there is NO
`--audience <file>` reuse flag in v1 (the reuse failure modes:
a shared key lets an INVOKE-only grantee decrypt, and an expired
grant's holder retains a still-live shared key; both are structural
non-events under per-grant audiences). Shared "disclosure groups"
are a future explicit feature.

Grant lifetimes days–weeks; renewal is grant revocation in v1;
`AnyNodeOwnedBy(B)` reusable across discovered B-owned providers,
call always names exact P.

### 2.3 `OrgCallProof` + binding — as v1.1

Exactly one `net-org-admission` header; postcard proof (grant now
carries a 32-byte commitment instead of the key — size unchanged);
static asserts vs 4096 B. Transcript `"net-org-call-v1"` binding
acting org, caller + origin, provider org, exact callee, call_id,
capability, whole canonical request minus the proof header, expiry
(unit-explicit ns), and all credential digests. Wall-clock verify,
monotonic retention.

### 2.4 Provider-local admission — as v1.1

Same 10-step verification order (local policy resolution →
exactly-one-header → TOFU member binding → mode checks
(`OwnerDelegated` rejects an unexpected capability grant as
malformed; `CrossOrgGranted` requires issuer == my owner, grantee
== caller's org, rights ⊇ INVOKE, capability match, target covers
exact me) → dispatcher grant checks → floors/windows/freshness →
binding → replay guard → provider-local policy LAST). Typed
`AdmissionDenied` (0x0009) with distinguishable reasons. Fold
state, decrypted announcements, and discovery responses are never
admission evidence. Unary only; streaming rejected with a distinct
reason.

### 2.4a Registration/projection seam (review-6 §3)

`may_execute` requires the provider's LOCAL fold to carry the
service's capability tag, and today the local self-announcement
feeds both the self-fold and the peer broadcast. Private services
therefore need an explicit seam:

```rust
RegisteredCapability {
    descriptor: CapabilityDescriptor,
    visibility: CapabilityVisibility,
    admission:  OrgAdmission,
}
```

- The local registry/self-fold ALWAYS receives the capability
  (with empty v0.4 allow-lists), so the exact service is locally
  registered/capable.
- **Gate selection is policy-directed (review-7).** The legacy
  `may_execute` aggregates allow-lists TARGET-WIDE: it unions
  `allowed_nodes/subnets/groups` from EVERY capability entry the
  target carries, so an unrelated restricted capability (e.g. an
  admin service with a tight `allowed_nodes`) on the same provider
  would block a protected service's callers before `OrgAdmission`
  ever ran. Therefore the callee resolves the registered
  `OrgAdmission` FIRST and only then picks the gate:

  ```rust
  match registered.admission {
      OrgAdmission::PublicAuthenticated => {
          // Preserve existing v0.4 behavior exactly.
          require(may_execute(fold, self_id, tag, caller));
      }
      OrgAdmission::OwnerDelegated | OrgAdmission::CrossOrgGranted => {
          // Exact service must be locally registered/capable, but
          // the unrelated v0.4 allow-list union is NOT authority.
          require(has_local_capability(fold, self_id, tag));
          verify_org_admission(...)?;
      }
  }

  /// Narrow helper in capability_bridge: does this target carry
  /// this exact tag? Evaluates NO legacy allow-lists.
  pub fn has_local_capability(fold, target, capability_tag) -> bool;
  ```

  `may_execute` itself stays byte-for-byte untouched for existing
  public/v0.4 services; protected services simply never route
  authority through it. **`OrgAdmission` is the load-bearing
  authority** (red-witnessed without depending on the legacy gate
  returning any particular value).
- Emission projects by visibility: `Public` → plaintext CAP-ANN;
  `OwnerScoped` → plaintext scoped form excluded from unscoped
  query projection; `GrantedAudience` → encrypted envelope only.
  A `GrantedAudience` descriptor never appears in plaintext
  broadcast bytes (witnessed).

**Migration implication, explicit:** an OLD provider with a
permissive local entry would serve any authenticated caller.
Therefore per protected service the order is: upgrade provider
code → enable org admission at registration → only then
emit/enable the protected service. Step 5 of §Migration enforces
this.

### 2.5 Replay guard — as v1.1

`(caller, call_id)` → `{binding_digest, expires_at}`; atomic
insert-or-deny before handler; replay vs collision reasons;
retention to proof expiry on a monotonic clock; unexpired keys
never evicted; `AdmissionReplayConfig` ceilings, deny+metric on
exhaustion, constants frozen after measurement; volatile guard,
cross-restart idempotency is the application's.

### 2.6 OA-2 exit gate

As v1.1 (golden vectors — grants now with commitments; grant
matrices incl. the structural DISCOVER⇔binding rule at issue and
decode; binding transplant matrix; replay witnesses; header
discipline; streaming rejection; reason mapping) PLUS:

```
call proof / header bytes contain no discovery_key   → witnessed
  (byte-scan of the encoded header against the known key, plus a
   type-level assertion that OrgAudienceSecret is not a member of
   any wire object)
key_commitment mismatch with installed secret        → credential
                                                        rejected
                                                        locally
```

Stop. Review.

---

## OA-3 — grant-scoped private discovery (stop and review)

### 3.1 `ScopedCapabilityAnnouncement` — as v1.1, per-grant audiences

Envelope as v1.1 (provider, owner_org, owner_cert,
audience_handle, grant_id, generation, expires_at, 24-byte nonce,
bounded ciphertext, outer signature by P under
`"net-org-scoped-ann-v1"`). AEAD XChaCha20-Poly1305 under the
per-grant `discovery_key`; associated data = `(provider ‖
owner_org ‖ audience_handle ‖ grant_id ‖ generation ‖ expires_at)`.
For the owner audience (below), `grant_id` is the fixed
all-zero sentinel — committed in the golden vectors — so owner and
granted envelopes can never be confused under one AD.
**Consequently `[0u8; 32]` is a RESERVED grant id:** ordinary
`OrgCapabilityGrant` issuance AND decode reject it (pinned in the
grant matrix).

Size bounds (review-6 Q1 — dual, no silent trimming):

```rust
MAX_SCOPED_ANN_CIPHERTEXT_BYTES: usize;   // plaintext-side cap
MAX_SCOPED_ANN_ENCODED_BYTES:    usize =  // whole-envelope cap
    8192 − transport/event framing − outer fields − signature
         − AEAD tag − safety headroom;
// builder AND decoder enforce both; oversized descriptors return
ScopedAnnouncementError::DescriptorTooLarge { encoded, maximum }
// — never trimmed: trimming changes capability semantics.
```

### 3.2 Propagation — as v1.1

Own broadcast id from the subprotocol registry; CAP-ANN hop-cap +
dedup discipline, dedup key `(provider, grant_id, audience_handle,
generation)` — the wire identity carries the actual audience scope
even though handles are unique by rule, so a handle collision
(accidental or malicious) cannot let one envelope suppress another
before ingest validation; v1 floods opaque envelopes (observers learn
existence, provider, owner, random handle, size — nothing
matchable); selective forwarding by handle is a later fanout
optimization.

### 3.3 Audience authority — owner vs granted (review-6 §2 fix)

The v1.1 "internal capabilities use the owner audience" claim
contradicted the grant-dependent ingest path (no grant, no
grant_id, nothing for step 2 to verify). Explicit split:

```rust
enum AudienceAuthority {
    Owner   { owner_org: OrgId, audience_handle: [u8; 32],
              discovery_key: [u8; 32] },          // scaffolded
    Granted { grant: OrgCapabilityGrant,          // signed authz
              secret: OrgAudienceSecret },        // local material
}

enum CapabilityAudienceScope {                    // fold partition
    Public,
    Owner { org_id: OrgId,     audience_handle: [u8; 32] },
    Grant { grant_id: [u8;32], audience_handle: [u8; 32] },
}
```

**Owner ingest** (internal private capabilities): local node's
owner == envelope `owner_org`; local owner audience handle
matches; owner key decrypts (AD with the zero grant_id sentinel);
provider's `owner_cert` proves P ∈ same org (incl. floors);
generation/expiry valid → fold entry under
`Owner { org_id, handle }`. The owner audience credential grants
ONLY knowledge — internal invocation still requires
`OwnerDelegated`.

**Granted ingest** (cross-org): handle matches an installed
grant+secret pair; `key_commitment` matches the secret; B signed
the grant; grant names A (local owner); grant ⊇ DISCOVER; S's own
membership currently valid; P's outer signature; P's `owner_cert`
proves P ∈ grant.issuer_org; generation/expiry → fold entry under
`Grant { grant_id, handle }`.

Both scopes are mutually invisible and invisible to unscoped
queries. Query surface: `find_capabilities_for_grant(grant_id,
predicate)` and `find_owner_private_capabilities(predicate)`.

### 3.4 Rotation — as v1.1, plus owner-audience procedure (Q4)

Granted: per-grant expiry → new grant mints fresh handle+key; a
former grantee keeps cached knowledge (stated) but cannot decrypt
new envelopes, invoke after expiry, or bypass admission. Owner:
config-management rotation of `owner-audience.key` — install new
credential → providers briefly dual-publish old+new envelopes →
consumers accept both → retire old after announcement TTL. An
operational tooling concern, not a mesh key-epoch protocol.

### 3.5 OA-3 exit gate

As v1.1 (golden vectors incl. the zero-sentinel owner AD;
AD-transplant matrix; generation/expiry/dedup; scoped-fold
isolation — Owner↔Grant↔Public all mutually invisible;
owner-audience internal case) PLUS: dual size bounds enforced at
builder and decoder with the typed error; INVOKE-only grant holds
no audience material by construction (structural rule) and cannot
ingest; expired-grant holder cannot decrypt a NEW audience's
envelopes (per-grant key independence witnessed). Stop. Review.

---

## OA-4 — end-to-end witnesses (then STOP)

Nodes: dispatcher **S** ∈ A; providers **P₁** ∈ A, **P₂** ∈ B;
unrelated **X**; org **C** as wrong-grantee foil.

**Internal (P₁, `OwnerDelegated`)** — as v1.1: valid dispatcher
grant accepted; membership-only denied; copied proof denied; wrong
target/capability/body/expiry/replay denied; floored-after-reload
denied; public capabilities unchanged; `may_execute` pinned
unchanged. Plus the restart witness chain from OA-1 run end-to-end
(floor 5 → old bundle 3 → restart → S with generation 4 cert still
denied).

**Cross-org invocation (P₂, `CrossOrgGranted`)** — as v1.1: the
full accept + eleven-denial matrix; `AnyNodeOwnedBy(B)` reuse
accepted against a second B-owned node, denied against a non-B
node; four-party audit attribution asserted.

**Private discovery (`GrantedAudience` on P₂)** — as v1.1
(Kyra's matrix verbatim: absent from global CAP-ANN; no grant ⇒ no
enumeration; DISCOVER-only resolves but cannot invoke; INVOKE-only
cannot ingest; DISCOVER|INVOKE resolves and calls exact P₂; copied
grant by C, wrong dispatcher, wrong provider owner, wrong
handle/capability, stale registration, decrypt-without-invoke all
denied/ignored; provider policy final) plus observer-recovers-
nothing, AD-transplant, and post-rotation decryption failure.

**Seam red witnesses (review-7 matrix — robust to target-wide
allow-list aggregation):**

```
P registers protected capability C:
    OrgAdmission::CrossOrgGranted, empty legacy allow-lists
P also registers unrelated capability D:
    legacy allow-list EXCLUDES caller S

may_execute(P, C, S) may be false (target-wide aggregation pulls
    in D's restrictions) — witnessed, not assumed either way
has_local_capability(P, C) is true
valid org proof for C → ACCEPTED through the OrgAdmission path
    (D's restrictions cannot block C)
invalid org proof for C → denied
missing local C tag + otherwise-valid proof → denied as
    unregistered
public capability D → still governed by unchanged may_execute
    behavior

private service C absent from plaintext broadcast bytes

RED: disable verify_org_admission in the protected-service branch
    (test rig only)
→ unauthorized C invocation SUCCEEDS
    — proving OrgAdmission is load-bearing, independent of any
      legacy-gate verdict
```

Then STOP. No further org work without a named consumer or
measured failure.

---

## Migration

1. Upgrade every participant; emission of all new signed
   fields/objects OFF (old readers reject nonempty new signed
   announcement fields; old forwarders strip-then-break; all
   fail-closed; upgrade-all-then-emit mandatory).
2. Provision ownership (`net node adopt`): membership, owner
   audience credential, and the persisted revocation-state file —
   three separately versioned files.
3. Enable `owner_cert` emission fleet-wide.
4. Issue dispatcher grants; wire caller proof plumbing; distribute
   `OrgAudienceSecret` files out of band alongside their grants.
5. **Per protected service:** upgrade provider code → register with
   `OrgAdmission` enabled → only then emit/enable the service
   (an old provider's permissive local entry would otherwise serve
   any authenticated caller — the §2.4a implication).
6. Flip private capabilities to `GrantedAudience` (plaintext
   CAP-ANN presence stops at the same moment — OA-4 absence
   witness); rotate owner audience via config management as needed.

No data migration; fold state rebuilds; bundles, audience
credentials, and revocation maxima are plain local files.

---

## Deliberately NOT in v1

As v1.1 (floor replication/WAL/sync/anti-entropy → deferred plan;
live private sensing → deferred plan; resolver nodes and
request/response discovery objects; deterministic locators;
org-wide discovery keys; key epochs; delegation chains;
`OrgMembership` invocation mode; advertised policy vectors;
extension bodies; foreign co-ownership REJECTED; streaming
admission; caches before benchmarks; language parity; any change
to `PermissionToken` or `may_execute`; selective forwarding by
handle) plus, newly explicit:

- **No audience sharing across grants** — disclosure groups are a
  future explicit feature with their own review; v1 is structurally
  one-secret-per-DISCOVER-grant.
- **No raw key material in any wire object** — enforced
  structurally (review-7): `OrgAudienceSecret` and
  `AudienceAuthority::Owner` do NOT derive `Serialize` /
  `Deserialize` / any postcard wire trait; config-file encoding,
  where needed, is a separate explicit codec; wire objects carry
  only `GrantedDiscoveryBinding` and its commitment. The type-level
  witness covers both granted and owner secret types.

---

## Locked design points

1. `OrgId` IS the org key; no registry.
2. Ownership singular + scaffolded; loud failure; cross-org access
   is a B→A grant, never co-membership.
3. Membership never enters `may_execute`; decryption never
   authorizes; fold state is never admission evidence; **the
   OrgAdmission gate is load-bearing and the legacy permissive
   self-entry is only a pass-through seam — red-witnessed.**
4. Grants are fixed one-hop, org-root-signed; DISCOVER and INVOKE
   independent; `rights ⊇ DISCOVER ⇔ discovery binding present`;
   one DISCOVER grant = one audience secret; the signed grant
   carries a key COMMITMENT and the raw key never rides the wire;
   grant renewal is grant revocation in v1.
5. Membership revocation is the local monotone bundle whose merged
   maxima are atomically persisted BEFORE the live view updates —
   "lower bundle never rolls back" holds across restart; corrupt
   incoming bundle → last-good; corrupt persisted maxima → loud
   startup failure.
6. Admission per-service, provider-local, bound at registration,
   always last; three modes only.
7. Call binding as specified; exactly one admission header or
   deny.
8. Replay identity (caller, call_id); unexpired keys never evict;
   volatile by contract.
9. Unary only; streaming rejected distinctly.
10. Private discovery is a visibility mode of the one announcement
    substrate; audience authority is explicitly Owner or Granted
    with mutually invisible fold scopes; owner-audience envelopes
    use the zero grant_id sentinel in AD; per-grant audiences make
    rotation surgical; `OwnerScoped` plaintext is labeled fanout
    control, not confidentiality.
11. Audit identity is the full four-party attribution.
12. Wire evolution honest; unit-explicit timestamps; dual size
    bounds with typed errors, never silent trimming; constants
    freeze only after measurement.
13. **The authority directory is a trusted local security boundary.**
    Concurrent mutation by another process running with write access
    to it — replacing directory entries or the stable `.lock` sidecar
    mid-transaction — is explicitly OUT OF SCOPE: a same-account
    attacker who can write there can already attack the surrounding
    configuration and process state, so hardening one sidecar protocol
    against it while the rest of the local boundary trusts the account
    would be incoherent. Supported Net writers never unlink or replace
    the sidecar. R3-3 detects sidecar replacement occurring BETWEEN
    legitimate transactions and common operator/startup mistakes; it
    does NOT claim protection against an actor concurrently mutating
    directory entries DURING a transaction. The supplied path is first
    normalized ONCE (`normalize_authority_dir`, applied by both `adopt`
    and `open`): a relative path is resolved against the current directory
    (a bare relative name has an empty parent, so its ancestor chain would
    otherwise go unchecked) and a trailing separator is stripped (so
    `symlink_metadata` on a final symlink reports the link, not its
    followed target). The boundary is then enforced at its edges
    (`org_authority::ensure_secure_authority_dir`): on Unix
    the resolved ancestor chain is validated (no group/other-writable,
    non-sticky parent through which another account could rename the
    owned entry — sticky writable parents like `/tmp` are fine), a new
    authority directory is created no broader than 0700 (the `DirBuilder`
    mode is filtered through umask) and then tightened to exactly 0700 —
    a restrictive-umask create + owner chmod(0700) is safe; the dangerous
    pattern was permissive-create-then-tighten — an existing one must be
    owned by the current user and not group/other-writable, and
    state/lock/audience files are 0600; the generic path-agnostic store
    API never chmods a supplied parent. On Windows every missing component
    (missing intermediate parents AND the final directory) is created
    ATOMICALLY with a protected, owner-only DACL (`CreateDirectoryW` with a
    `SECURITY_ATTRIBUTES` whose DACL grants only the process token SID full
    control, `OI|CI`-inheritable and `SE_DACL_PROTECTED`) — there is no
    post-creation window under inherited permissions, and a failure leaves
    no directory behind, so a retry cannot adopt an insecure residue. A
    pre-existing directory is re-validated against its BINARY DACL and fails
    closed unless every write-capable ACE grants only a trusted principal.
    A custom path's PRE-EXISTING ancestor chain is not walked on Windows (a
    writable ancestor's owner could replace the entry, which a child DACL
    cannot prevent — the CLI warns). The user account, SYSTEM, and local
    administrators are trusted local principals.

---

## Open questions

- **Q1 — encoded-bounds fixture.** Concrete values for the two
  scoped-envelope bounds from a worst-case encoded fixture
  (realistic `CapabilitySet` fragment + envelope overhead) at OA-3
  review.
- **Q2 — `OrgAudienceSecret` at-rest protection.** Plain file
  0600 under the config dir (matching existing key-material
  handling in the repo) vs OS keychain integration — follow
  whatever `EntityKeypair` storage does today; confirm at OA-2
  review.
- **Q3 — `default_proof_ttl`.** 30 s provisional; freeze after
  OA-2 measurement; callee `AdmissionReplayConfig` authoritative.
