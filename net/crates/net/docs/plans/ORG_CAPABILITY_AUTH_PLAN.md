# Organization Capability Auth Plan

**Version:** v0.4 — closes the three review-3 implementation
contracts: dedicated floor replication + write-through lifecycle
(§1.5), finite admission-proof validity + (caller, call_id) replay
identity (§2.4/§2.6), two-epoch owner discovery + plaintext-interest
boundary (§3.2/§3.5). Architecture, three-plane separation,
scaffolded ownership, delegated admission model, and the visibility
mode split are review-3 APPROVED and unchanged. All review-2 and
review-3 sign-offs (see §Locked) are retained verbatim.
**Status:** Draft, awaiting review-4 / implementation authorization.
**Scope:** cryptographic organization ownership for nodes,
capability-specific delegated admission of incoming work, and
owner-scoped discovery.

```
Belonging:   org-signed membership          (Phase 1 — scaffolded,
                                             authority-dark)
Admission:   per-call delegated authority   (Phase 2 — load-bearing,
                                             replay-guarded, unary)
Visibility:  owner-scoped discovery         (Phase 3 — two honest
                                             modes, epoch-keyed)
```

**The invariant sentence:** *Scaffolding establishes belonging.
Delegation authorizes work. Discovery limits knowledge. Provider
policy controls execution.* Membership is never invocation
authority. Visibility is never admission. Admission is never
confidentiality. Authentication is never replay prevention. A
replay key never dies before the proof it guards (§2.6).

---

## Context

Unchanged from v0.2/v0.3: v0.4-capability-auth's four structural
limits for an org boundary, with "membership conflates into
invocation authority" as the trap no phase may rebuild. No
execute-gate change ships in Phase 1; authority arrives only as
capability-specific, per-call, replay-guarded delegated admission
in Phase 2; Phase 3 ends the discovery deferral with
honestly-named visibility modes.

---

## Model

```
ORG IDENTITY
    OrgId = org's ed25519 public key (32 B, self-certifying).
    Root key offline.

OWNERSHIP (Phase 1) — one node, exactly one owner org
    OrgMembershipCert { org_id, member, not_before, not_after,
                        generation, nonce, --- signed ---, sig }
    ~1-year TTL, silent renewal, 156 B wire.
    VERIFY = verify_strict AND TOFU member binding AND window
             AND generation >= durable_floor(org, member)

REVOCATION (Phase 1) — OrgAuthorityFloorStore (§1.5)
    Dedicated durable authority store + explicit replication
    protocol. Inner org signature is authority; carriers are
    interchangeable; floors never expire; write-through durable
    commit BEFORE admission visibility; monotone generation-primary
    merge. NOT an ordinary fold.
    Delegations: short expiry only; no floors in v1.

SCAFFOLDING (Phase 1)
    NodeAuthorityConfig { owner_org, owner_cert, trusted_org_root,
                          revocation_state }
    Loud self-verification at startup (cert AND floor store health).
    Fold projection owner_org: Option<OrgId> — discovery/indexing/
    admission-input ONLY. may_execute untouched.

ADMISSION (Phase 2) — capability-specific, per-call, replay-guarded,
                       UNARY (v1)
    Provider-local HashMap<CapabilityAuthorityId, CapabilityPolicy>.
    AdmissionRequirement: PublicAuthenticated | OwnerDelegation(OrgId)
        | OrgMembership(OrgId) | Extension{kind,body} (unknown DENIES)
    Exactly one "net-org-admission" header:
        AdmissionProof { membership_cert, delegation_chain,
                         proof_expires_at, call_binding_sig }
    proof_expires_at is FINITE always (deadline-less RPC included);
    replay keys retain until proof_expires_at + skew — a key never
    dies before its proof.
    Replay identity is nRPC correlation identity: (caller, call_id).

VISIBILITY (Phase 3)
    Public | PublicLocatorOwnerDetail(OrgId) | OwnerOnly(OrgId)
    OwnerOnly: epoch-keyed locator ids + owner-domain registration;
    two-key rotation overlap; the sensing amendment must settle the
    plaintext-interest boundary before implementation (§3.5).
```

---

## What ships — Phase 1: scaffolded ownership, dark authority facts

### 1.1–1.4 — as v0.3, unchanged

`OrgId` (`behavior/org.rs`, derived non-ct PartialEq, documented
contrast with bearer-secret ids); `OrgKeypair` / `OrgMembershipCert`
(canonical layout, strict decode, verify_strict, ~1y recommended /
2y max TTL, token-module skew); `NodeAuthorityConfig` scaffolding
(`net node adopt`, loud startup self-verification, one owner org,
runtime-attached cert); single `owner_cert: Option<...>`
announcement field with `SignedPayloadCanonical` lockstep; fold
`owner_org` projection from ingest-verified certs only;
`may_execute` untouched (T5); no verification cache before its
benchmark.

Startup self-verification now explicitly includes floor-store
health per §1.5: cert valid AND durable store open and consistent,
or loud failure. CLI as v0.3 plus nothing — floor distribution is
runtime, not operator ceremony.

### 1.5 `OrgAuthorityFloorStore` — dedicated durable authority store
(closes review-3 Blocker 1)

The generic fold runtime assumes per-entry `expires_at`,
outer-publisher entry ownership, failure eviction of a publisher's
entries, outer-generation merge ordering, and no automatic
rebroadcast of inbound deliveries. Every one of those conflicts
with floor semantics. **Floors are therefore NOT a `FoldKind`** —
no "non-default lifecycle" fiction, no `Duration::MAX` abuse of an
expiry-structured sweeper. Dedicated component:

```rust
// behavior/org_floor.rs (new)
pub struct OrgAuthorityFloorStore {
    /// Authoritative local maximum per key. Durable, write-through.
    durable: PersistentMap<MembershipFloorKey, MembershipFloorValue>,
    /// Mesh transport/projection surface (frames below).
    replication: OrgFloorReplication,
}

MembershipFloorKey   { org_id: OrgId, member: EntityId }
MembershipFloorValue {
    minimum_generation: u32,
    issued_at:          u64,
    org_signature:      Signature64,  // over ("net-org-floor-v1" ‖
}                                     //  org_id ‖ member ‖ min_gen ‖
                                      //  issued_at)
```

**Authority + carriage.** The inner org signature is the sole
authority; the outer transporting node is an interchangeable
carrier. Any node may wrap and carry the byte-identical inner
record under its own transport identity. Carrier disconnect,
failure eviction, or carrier outer-generation NEVER affects stored
floors (witnesses 8–9). Records failing inner-signature
verification are dropped, never merged.

**Merge** (unchanged, restated): generation-primary monotone;
`issued_at` breaks ties only; lower generation never replaces
higher regardless of `issued_at`.

**Write-through lifecycle (locked; Q10 answered as mandatory):**

```
verify inner org signature
→ compare against durable maximum
→ write + fsync / atomic durable commit
→ ONLY THEN update the live admission view
→ notify / rebroadcast the exact inner record
```

Persistence failure: the new floor is NOT exposed from memory;
protected admission is marked unhealthy and fails closed (deny)
until durable state is trustworthy again. A node must never
enforce a floor it could forget on crash (witnesses 10–11).
Periodic checkpointing may optimize restart; it is never the
durability boundary.

**Retention: indefinite (locked).** No TTL, no age-based GC — the
v0.3 "retention ≥ MAX_ORG_CERT_TTL past issued_at" rule is removed
(unsafe without bounding future-dated `not_before`, and the table
is tiny). Compaction retains exactly one maximum record per key,
verbatim with signature, re-verifiable. Per-key removal arrives
only with a future reviewed org/key-retirement mechanism.

**Explicit replication protocol** (subprotocol ids allocated from
the registry at implementation, alongside 0x0C00/0x0C02/0x0C03):

```rust
OrgFloorAnnounce    { record: OrgMembershipFloor }
OrgFloorSyncRequest { }                         // v1: all maxima
OrgFloorSyncPage    { records: Vec<OrgMembershipFloor>,
                      cursor:  Option<...> }    // bounded pages
```

Behavior: an ACCEPTED higher floor (post-durable-commit) is
broadcast/rebroadcast as the exact inner record — inbound valid
maxima are re-propagated beyond the receiving peer, because
ordinary delivery does not do this automatically (witness 12). On
peer session establishment/recovery: `OrgFloorSyncRequest` → bounded
pages → verify + monotone-merge every record; converges across
non-full-mesh topologies (witness 13). Periodic anti-entropy replays
maxima (v1: full replay; compact summaries are a follow-up
optimization — the table is tiny).

**Witnesses (T-floor, thirteen total):** 1–7 from v0.3 (restart;
compaction; no later-lower rollback; reconnect sync; post-
propagation rejection; pre-propagation acceptance proven;
store-unavailable loud failure) plus review-3's:
(8) carrier failure/disconnect does not remove a floor;
(9) a carrier with a huge outer generation cannot block a higher
inner floor arriving via another carrier;
(10) an accepted floor is not visible to admission before durable
commit; (11) persistence failure ⇒ protected admission unhealthy /
fail-closed; (12) valid inbound floor re-propagates beyond the
receiving peer; (13) join-time full sync converges on non-full-mesh
topology.

Delegation floors: still out of v1; `OrgDelegation.generation`
reserved and unenforced (T7 pins).

---

## What ships — Phase 2: capability-specific delegated admission

### 2.1–2.3 — as v0.3, unchanged

`CapabilityAuthorityId` (32-byte blake3 derive_key, documented
enumerable, never a secrecy mechanism); `OrgDelegation` (sibling
type, actions vs `CapabilityScope::{Exact,Any}`, short-lived,
TokenChain-semantics chain rules, membership floor checked via the
proof's cert, delegation generation reserved-unenforced);
provider-local per-capability `CapabilityPolicy` with bounded
advertisement (dual count+byte limits, worst-case encoded fixture
before constants freeze, `MAX_ADMISSION_EXTENSION_BODY_BYTES`
independent cap, fail-closed `Extension`).

### 2.4 `AdmissionProof` — finite validity, exactly one header
(closes review-3 Blocker 2, part 1)

```rust
AdmissionProof {
    membership_cert:  OrgMembershipCert,
    delegation_chain: Vec<OrgDelegation>,
    /// FINITE, always. Bounds how long this signed invocation can
    /// FIRST be admitted — independent of the RPC deadline, which
    /// still bounds execution. Deadline-less RPC (deadline_ns == 0)
    /// changes nothing here.
    proof_expires_at: u64,
    call_binding_sig: [u8; 64],
}
```

Callee checks (in addition to v0.3's):

```
now <= proof_expires_at + skew
proof_expires_at <= now + MAX_ADMISSION_PROOF_TTL
```

`proof_expires_at` is bound into the call-binding transcript —
domain bumps to `"net-org-call-v3"`:

```
blake3("net-org-call-v3" ‖ caller EntityId ‖ caller origin_hash
       ‖ callee NodeId ‖ call_id
       ‖ canonical RpcRequestPayload bytes, net-org-admission
         header omitted
       ‖ proof_expires_at
       ‖ blake3(membership_cert) ‖ blake3(delegation_chain))
```

Header discipline unchanged: exactly one `net-org-admission`
header or deny; static size asserts include the new field.
Golden vectors regenerated for v3 transcripts.

### 2.5 Callee gate + plumbing — as v0.3

Local policy resolution → per-requirement verification (incl.
membership floor, proof expiry) → §2.6 → handler.
`RpcStatus::AdmissionDenied` (0x0009) + typed error; caller
`CallOptions.admission_proof` + proof-provider hook (the hook now
also picks `proof_expires_at`; sane default = min(deadline,
now + provider-advertised max) with `MAX_ADMISSION_PROOF_TTL`
ceiling); pre-flight advisory only; old callees can't enforce.

**Streaming scope (locked, v1):** organizational admission covers
UNARY requests / durable-job submission only. The call binding
covers the initial `RpcRequestPayload`; it does not bind later
`REQUEST_CHUNK` bodies, so no whole-work binding claim is made for
streaming. Client-streaming and duplex calls under org admission
are DEFERRED until chunks are hash-chained or independently
caller-signed (future amendment). Phase-2 code rejects
org-protected streaming calls with `AdmissionDenied` + a distinct
reason rather than admitting them under a false binding claim
(T18).

### 2.6 Admission replay guard — call-identity keyed, proof-lifetime
retained (closes review-3 Blocker 2, part 2)

nRPC's completed-call replay cache never landed
(`cortex/rpc.rs:108`); in-flight dedup alone is insufficient.
Corrected guard:

```rust
/// Key is nRPC correlation identity — NOT request content.
AdmissionReplayKey   { caller: EntityId, call_id: u64 }
AdmissionReplayValue { binding_digest: [u8; 32], expires_at: u64 }
                     // expires_at = proof_expires_at + skew
```

Keying on `(caller, call_id, binding_digest)` (v0.3) was wrong: the
same caller could reuse a call_id with a freshly signed different
binding and mint a new map key. Under correlation-identity keying,
ANY reuse of `(caller, call_id)` before expiry denies without a
second handler invocation:

```
same binding_digest      → AdmissionDenied (replay)
different binding_digest → AdmissionDenied (call-id collision)
```

Processing order unchanged and locked: verify proof + binding →
ATOMIC insert-or-deny (single entry-op; occupied → deny) → handler.
Retention: until `expires_at` — **an unexpired key is NEVER evicted,
for capacity or any other reason** (v0.3's 60-second default window
is removed; proof expiry is the retention clock, so the guard is
exact for deadline-less RPC too).

Capacity (Q9 answered): constants are not frozen now. Provider
configuration with hard global ceilings:

```rust
AdmissionReplayConfig {
    max_proof_ttl:          Duration,   // ceiling on accepted
                                        // proof_expires_at horizon
    max_entries_per_caller: usize,
    max_entries_total:      usize,
}
```

Sizing rule before freezing defaults:
`required ≈ peak per-caller admitted RPS × max_proof_ttl × safety
factor` (v0.3's 4096 × 60 s ≈ 68 calls/s/caller — fine for durable
job dispatch, too low for inference RPC; measure, don't guess).
Capacity exhaustion denies NEW protected calls and emits a metric;
it never evicts unexpired entries. Providers trade `max_proof_ttl`
against guard memory explicitly.

Honest residual unchanged: the guard is volatile; cross-restart
exactly-once is the application's idempotency key; call binding is
never called replay prevention.

---

## What ships — Phase 3: owner-scoped discovery

### 3.1 Visibility modes — as v0.3, review-3 APPROVED

`Public` / `PublicLocatorOwnerDetail(OrgId)` (deterministic global
locator, disclosure named honestly) / `OwnerOnly(OrgId)`. Locator
trust binding locked (publisher/owner-cert/generation cross-checks;
cross-org spoof witness T17). Org-scaffolded default
`OwnerOnly(owner_org)`.

### 3.2 Epoch-keyed owner discovery (closes review-3 Q8)

```rust
OrgDiscoveryKey { epoch: u32, key: [u8; 32] }

OwnerCapabilityLocatorId =
    blake3::keyed_hash(key[epoch], canonical capability identity)
```

The `epoch` is bound into: the keyed locator identity, the signed
locator registration, the `DiscoveryAuthority` transcript (domain
bumps to `"net-org-discovery-v2"`, epoch after org_id), provider
verification, and the private locator-table key. Distributed via
adopt-time config alongside membership; grants METADATA VISIBILITY
ONLY.

**Two-key rotation overlap (locked):** during rotation to epoch
E+1, providers register locators under BOTH E and E+1; consumers
may query both; new interests prefer E+1. Epoch E retires only
after

```
max locator soft-state TTL
+ max live interest TTL
+ anti-entropy / registration propagation allowance
```

has elapsed since all intended members received E+1. A revoked
member still fails membership/floor verification even while
holding an old discovery key — the key is metadata visibility, not
authority; where metadata confidentiality from a revoked member
matters, revocation SHOULD trigger discovery-key rotation
(operator guidance, stated in docs).

### 3.3 Member-signed `DiscoveryAuthority` — as v0.3 + epoch

Transcript: `("net-org-discovery-v2" ‖ org_id ‖ discovery_key_epoch
‖ member EntityId ‖ consumer NodeId ‖ interest_digest ‖ audience
scope ‖ expires_at ‖ nonce)`. Leader verifies (incl. floor),
refuses registration otherwise; leader→provider leg forwards the
complete authority; provider re-verifies independently; digest
alone never authorizes.

### 3.4 Capability-detail leg — wire enum + bounds

As v0.3 with review-3's corrections: the descriptor is a real wire
enum, not a conceptual union —

```rust
enum CapabilityDetailBody {
    Inline(CapabilitySet),      // independent size bound:
                                // MAX_INLINE_DETAIL_BYTES
    Datafort(DatafortRef),
}
```

`CapabilityDetailResponse { provider, owner_org, capability_id,
generation, request_nonce, descriptor_digest, body, signature }` —
nonce + generation bound, digest over encoded body bytes,
admission-gated retrieval, attestations stay compact (T16).

### 3.5 Plaintext-interest boundary (sensing-amendment gate, locked)

A keyed locator id is worthless if another field in the same
protocol carries the plaintext capability identity, constraints,
or selector/detail through a relay the threat model allows to
inspect payloads. The sensing amendment MUST resolve the boundary
by adopting exactly one of:

1. **Prove the session boundary** — routed logical sessions already
   provide end-to-end payload confidentiality across relays;
   document and test that boundary, then plaintext interest fields
   are relay-opaque by construction.
2. **Keyed + encrypted frames** — owner-only registration and
   interest frames use keyed capability ids AND encrypt sensitive
   constraints/selectors to the provider / owner leader.
3. **Trusted-path restriction** — `OwnerOnly` is supported only on
   owner-member relay paths, stated as a deployment constraint.

**This plan's recommended default is (2)**, with (1) acceptable
instead if the confidentiality boundary is proven and pinned by
tests, and (3) only as an explicitly documented interim. The
amendment states its choice; Phase-3 implementation does not start
before that. No configuration may claim OwnerOnly while any frame
on an inspectable path carries the plaintext capability name
(T15 extended: a relay-position observer of the full OwnerOnly
interest/registration exchange cannot recover the capability
identity under the chosen option).

---

## What this plan deliberately does NOT include

As v0.3 (no ACL engine; no multi-org ownership/hierarchy/
federation; no enrollment protocol; no caches before measurement;
no Go/TS/Python parity — golden vectors freeze the wire;
channel-auth untouched; no delegation floors; no cross-restart
exactly-once; no E2E-encrypted descriptors beyond §3.5's chosen
option), plus newly explicit:

- **No streaming/duplex org admission in v1** (§2.5) — deferred
  until chunk hash-chaining or per-chunk signatures exist.
- **No floor GC / org retirement** — per-key maxima are kept
  indefinitely until a future reviewed retirement mechanism.
- **No compact floor anti-entropy summaries in v1** — full replay
  of a tiny table; summaries are an optimization follow-up.

---

## Risks

1–3 as v0.3 (canonical-serializer drift; mixed-fleet emission
fail-closed + upgrade-all-then-emit; old callees can't enforce).
4. **Floor propagation lag** — unchanged, witnessed (T-floor 5/6).
5. **Floor-store durability** — now governed by the write-through
   contract; residual risk moves to `PersistentMap` implementation
   quality (fsync discipline, atomic rename); witnesses 10–11 +
   crash-injection in T-floor.
6. **Replay-guard memory** — retention is proof-lifetime exact and
   unexpired entries are never evicted, so memory is
   `O(admitted RPS × max_proof_ttl)` per provider; bounded by
   `AdmissionReplayConfig` ceilings with deny+metric on exhaustion;
   providers tune TTL vs memory explicitly.
7. **Root-key ceremony** — unchanged.
8. **Announcement/proof size coupling** — unchanged (dual bounds +
   measured fixture).
9. **Discovery-key handling** — as v0.3 (blast radius: metadata
   visibility only) plus epoch machinery: skewed epoch adoption
   during rotation is absorbed by the two-key overlap window;
   premature retirement of E is the failure mode — the retirement
   inequality in §3.2 is enforced by operator tooling, not vibes.
10. **Two denial surfaces** — unchanged; the replay and collision
    and streaming-rejection reasons are distinct within
    `AdmissionDenied` for triage.

---

## Phases

### Phase 1 — scaffolded ownership, dark authority facts
As v0.3 with §1.5 replaced by the dedicated
`OrgAuthorityFloorStore` + explicit replication protocol +
write-through lifecycle + indefinite retention. Exit: T1–T6 +
T-floor(1–13) green (incl. crash-injection on the durable commit
path); bench recorded; adopt-path loud failure demonstrated.

### Phase 2 — capability-specific delegated admission (unary)
As v0.3 plus: `proof_expires_at` (transcript v3), correlation-
identity replay keying with proof-lifetime retention and
never-evict-unexpired, `AdmissionReplayConfig` with measured
defaults, streaming rejection with distinct reason. Organizational
capability emission stays dark until Phase 2 is deployable
fleet-wide.

### Phase 3 — owner-scoped discovery
Sensing amendment FIRST — it must contain: the §3.5 plaintext-
boundary choice with its witnesses, epoch-keyed locator identity +
two-key rotation, `DiscoveryAuthority` v2 transcript, registration
frames, `CapabilityDetailResponse` with `CapabilityDetailBody` and
inline bound. Then implementation + T13–T18.

---

## Test plan

T1–T5 as v0.3 (byte identity; golden vectors regenerated for
`net-org-call-v3`, `net-org-discovery-v2`, floor frames, epoch-keyed
locator ids, `CapabilityDetailBody`; cert matrix incl. floors; fold
ingest; authority-dark pin).

- **T6 — mixed-fleet** as v0.3 (old readers reject nonempty new
  signed fields; fail-closed everywhere).
- **T-floor — thirteen witnesses** (§1.5), including crash injection
  between durable commit and admission-view update, carrier-failure
  independence, outer-generation independence, re-propagation, and
  non-full-mesh join sync.
- **T7 — chain matrix** as v0.3 (reserved generation ignored,
  pinned).
- **T8 — admission e2e** as v0.3, plus: expired `proof_expires_at`
  denied; `proof_expires_at` beyond `MAX_ADMISSION_PROOF_TTL`
  denied.
- **T9 — binding + replay** (review-3 matrix): transplant matrix as
  v0.3 (now incl. mutated `proof_expires_at` → binding fails);
  same caller + same call_id + identical binding → deny (replay);
  same caller + same call_id + DIFFERENT valid binding → deny
  (collision); different caller + same call_id → independent;
  same caller + different call_id → independent; two identical
  concurrent requests → exactly one handler invocation;
  deadline-less request replayed after v0.3's-would-be-window but
  before `proof_expires_at` → DENIED (the fixed defect, witnessed);
  replay after `proof_expires_at + skew` → proof itself expired →
  denied at verification, key may be gone, still no execution;
  restart residual witnessed honestly.
- **T10 — index-staleness** as v0.3.
- **T11 — bounds** as v0.3, plus: unexpired replay entries survive
  capacity pressure (new protected calls denied + metric instead);
  proof-size asserts include `proof_expires_at`.
- **T12 — status mapping** as v0.3; replay / collision / streaming
  reasons distinguishable.
- **T13 — publication modes** as v0.3; OwnerOnly floods nothing
  capability-identifying in ANY epoch.
- **T14 — signed-interest gate** as v0.3 + epoch binding: wrong-
  epoch authority refused; both-epoch queries admitted during
  overlap.
- **T15 — dictionary/relay insufficiency** as v0.3, extended per
  §3.5: relay-position observer of the complete OwnerOnly exchange
  cannot recover capability identity under the amendment's chosen
  option.
- **T16 — detail-leg binding** as v0.3 + inline body over
  `MAX_INLINE_DETAIL_BYTES` rejected.
- **T17 — locator trust binding** as v0.3, per epoch.
- **T18 — streaming rejection**: org-protected client-streaming /
  duplex call → `AdmissionDenied` with the streaming reason; no
  handler invocation; unary unaffected.

---

## Migration

As v0.3 (upgrade-all → provision → enable `owner_cert` emission →
delegations + caller plumbing → flip provider policies per
capability → Phase 3 last), with two additions: floor-store
provisioning (durable path + health check) happens at adopt time in
step 2, and `org_discovery_key` epoch-0 distribution rides the
adopt-config update in step 6 before any capability flips to
`OwnerOnly`. Epoch rotations thereafter follow §3.2's retirement
inequality. No data migration; the floor store migrates as its own
durable files.

---

## Locked design points

1–3 as v0.3 (OrgId IS the key; singular scaffolded ownership;
membership is never execution authority).
4. **Revocation is the §1.5 dedicated durable authority store** —
   not a FoldKind; inner-signature authority with interchangeable
   carriers; write-through durable commit before admission
   visibility; indefinite retention; explicit
   announce/sync/anti-entropy protocol; loud unhealthy-store
   fail-closed coupling. Delegations revoke by short expiry only.
5–8 as v0.3 (capability-specific provider-local admission; 32-byte
digest grant scopes, wildcard as scope variant; whole-initial-
request call binding + exactly-one-header; honest wire evolution,
fail-closed `Extension`).
9. **Replay guard is exact for the proof lifetime**: finite
   `proof_expires_at` on every proof (deadline-less RPC included),
   bound into the v3 transcript; `(caller, call_id)` correlation-
   identity keying; reuse denies as replay OR collision, never a
   second invocation; unexpired keys are never evicted; capacity
   exhaustion denies new calls; volatile by contract with the
   idempotency-key guidance.
10. **Visibility modes named by disclosure** as v0.3, now
    epoch-keyed with a two-key rotation overlap and the retirement
    inequality; discovery keys grant metadata visibility only.
11. **Sensing carries proofs, not assumptions** — detail leg binds
    nonce + generation with a real wire enum and inline bound; the
    plaintext-interest boundary is settled by the amendment (§3.5
    options, recommended default (2)) before any Phase-3 code.
12. **No cache before its benchmark**; replay/floor sizing
    constants freeze only after measurement.
13. **Org admission is unary in v1.** Streaming admission waits for
    chunk-level binding; org-protected streaming calls are rejected
    with a distinct reason, never admitted under a partial binding
    claim.

---

## Open questions (v0.4)

- **Q11 — `PersistentMap` backing.** Reuse an existing durable
  primitive in-repo (Mikoshi/datafort-adjacent) vs a minimal
  fsync'd log+snapshot local store owned by `org_floor.rs`. The
  contract (§1.5) is fixed either way; pick at Phase-1
  implementation review with a crash-injection harness as the
  acceptance bar.
- **Q12 — floor sync paging bounds.** Page size / cursor shape for
  `OrgFloorSyncPage` (table is tiny; propose 256 records/page,
  postcard-bounded) — confirm against frame-size conventions at
  implementation.
- **Q13 — proof-provider default TTL.** Default `proof_expires_at`
  horizon the caller hook picks when the app doesn't specify
  (proposal: min(RPC deadline, 30 s), never exceeding the
  provider-advertised max) — confirm against real dispatch
  latencies during Phase-2 review.
