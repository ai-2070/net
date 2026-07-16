# Organization Capability Auth Plan

**Version:** v0.5 — closes review-4's floor-replication admission
blocker (tracked-org authority boundary + authenticated owner-scoped
synchronization, §1.5–§1.6), adopts the review-4 answers to Q11
(dedicated WAL, §1.7), Q12 (bounded sync pages, §1.6), Q13 (explicit
proof timestamp semantics, §2.4), and corrects the foreign-access
wording throughout. All review-4 approved portions (see §Locked) are
retained verbatim and are not reopened.
**Status:** Awaiting review-5 — expected to be the implementation-
authorization pass for Phases 1–2; Phase 3 remains correctly gated
on the sensing amendment.
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
replay key never dies before the proof it guards. **A signature
proves what an organization said — never that this node has any
reason to track that organization (§1.5).**

---

## Context

Unchanged from v0.2–v0.4. One addition from review-4: once floor
replication became concrete, self-certifying `OrgId`s + permanent
fsynced retention + unauthenticated all-maxima sync composed into
an amplification and disclosure surface — any peer could mint
unlimited valid (org, member) floors and force permanent durable
state, rebroadcast, unbounded join sync, and cross-tenant
disclosure of membership/revocation events. v0.5 adds the missing
authority boundary: cryptographic validity admits nothing; local
provisioning does.

---

## Model

```
ORG IDENTITY / OWNERSHIP / SCAFFOLDING — as v0.4
    OrgId = self-certifying ed25519 pubkey; one node, one owner org;
    OrgMembershipCert (~1y, silent renewal, generation floor-checked);
    NodeAuthorityConfig with loud startup self-verification;
    fold projection owner_org: Option<OrgId> — never execution.

REVOCATION (Phase 1) — OrgAuthorityFloorStore, TRACKED ORGS ONLY
    tracked organizations (v1) = local node's owner_org only
    ingest: tracked-check BEFORE durable allocation; unknown orgs
    are never persisted, never rebroadcast, never synced, never
    allocate per-org state.
    sync: org-scoped, member-authenticated, policy-gated
    (OwnerMembersOnly in v1), bounded pages.
    storage: dedicated WAL + snapshot owned by org_floor.rs.
    Write-through before admission visibility; indefinite retention;
    carrier-independent inner authority — all as v0.4.

ADMISSION (Phase 2) — as v0.4, timestamps made explicit
    AdmissionProof { membership_cert, delegation_chain,
                     proof_expires_at_unix_ns, call_binding_sig }
    (caller, call_id) replay identity; retention to proof expiry +
    skew on a monotonic clock; unary only.
    v1 OwnerDelegation semantics, stated plainly:
        valid owner member + valid owner-rooted dispatcher
        delegation → may request invocation.
    Foreign access = FUTURE explicit grants; NOT implemented by v1
    OwnerDelegation. membership_cert stays mandatory.

VISIBILITY (Phase 3) — as v0.4
    Public | PublicLocatorOwnerDetail | OwnerOnly; epoch-keyed with
    two-key overlap; plaintext-interest boundary gated in the
    sensing amendment.
```

---

## What ships — Phase 1: scaffolded ownership, dark authority facts

### 1.1–1.4 — as v0.4, unchanged

(`org.rs` types; cert discipline; `NodeAuthorityConfig` + adopt path
with loud failure incl. floor-store health; single `owner_cert`
announcement field with canonical lockstep; verified-only fold
projection; `may_execute` untouched; no cache before benchmark.)

### 1.5 Floor authority boundary — tracked organizations
(closes review-4 critical)

The inner org signature proves authorship. It does not create a
tracking relation. Durable floor state exists only for locally
provisioned roots:

```rust
pub struct TrackedOrgAuthority {
    pub org_id:     OrgId,
    pub floor_sync: FloorSyncPolicy,
}

pub enum FloorSyncPolicy {
    /// v1 default and only shipped mode: floor pages are served
    /// exclusively to verified current members of this same org.
    OwnerMembersOnly,
    /// Reserved for future foreign relying parties (providers that
    /// accept a foreign org's grants pin that org explicitly).
    ExplicitRelyingParties,
}
```

**v1 rule:** `tracked organizations = { local owner_org }` —
populated from `NodeAuthorityConfig` at adopt time. A future
relying-party provider pins additional roots explicitly and
locally. **Receiving a signed record from the network never creates
the trust relation** (T-floor-18).

Ingest pipeline (locked order):

```
decode bounded frame (size-checked before allocation)
→ record.org_id locally tracked?  — NO → drop; bounded counter
                                     metric only, no per-org state,
                                     no high-cardinality label
→ verify inner org signature
→ monotone compare against durable maximum
→ write-through durable commit (§1.7)
→ live admission view update
→ owner-scoped rebroadcast of the exact inner record
```

Unknown organizations: not persisted, not rebroadcast, not included
in anti-entropy, never allocate per-org state (T-floor-14). This
removes all four review-4 amplification/disclosure vectors at the
first pipeline step.

**Operator injection path:** the offline root signs a new floor;
the operator injects it into ONE owner node via the CLI / local
administrative path (`net org inject-floor --record <path>`, local
transport only); that node durably commits and propagates it
through the owner domain. Origination never requires the org root
on the mesh.

### 1.6 Floor synchronization — org-scoped, member-authenticated
(closes review-4 critical, part 2; answers Q12)

The v0.4 unauthenticated `OrgFloorSyncRequest {}` is replaced:

```rust
OrgFloorSyncRequest {
    org_id:           OrgId,
    requester_cert:   OrgMembershipCert,
    nonce:            u64,
    expires_at:       u64,
    member_signature: Signature64,
}
// transcript: "net-org-floor-sync-v1" ‖ org_id
//   ‖ requester EntityId ‖ responding peer NodeId
//   ‖ nonce ‖ expires_at
```

Responder verifies, in order: `org_id` locally tracked;
`requester_cert.org_id == org_id`; `requester_cert.member` == the
authenticated session EntityId; the cert valid under the
responder's CURRENT local floor; freshness (`expires_at`, nonce)
and signature; the tracked org's `FloorSyncPolicy` admits the
requester. v1 `OwnerMembersOnly`: only valid current members of
that same organization receive pages — floor synchronization no
longer discloses membership identities or revocation events to
outsiders (T-floor-15/16/17).

Page shape (Q12 — 256/page was impossible: ~140 B/record × 256 ≈
35,840 B against the 8,192 B packet ceiling):

```rust
/// Provisional; frozen only after a worst-case encoded fixture.
/// 32 × ~140 B ≈ 4,480 B before framing — fits with headroom.
pub const MAX_FLOOR_SYNC_RECORDS_PER_PAGE: usize = 32;
pub const MAX_FLOOR_SYNC_PAGE_BYTES: usize =
    MAX_PACKET_SIZE - frame_overhead - transport_headroom;

OrgFloorSyncPage {
    records: Vec<OrgMembershipFloor>,   // both bounds enforced at
                                        // decoder BEFORE allocation
                                        // and at builder
    /// Last key of this page under lexicographic
    /// (org_id, member) ordering; None on the final page.
    cursor: Option<MembershipFloorKey>,
    /// Final-page marker + snapshot digest/version so one sync run
    /// can prove completeness; records added mid-paging also arrive
    /// via live announce / periodic anti-entropy (state is
    /// monotone, so overlap is harmless).
    complete: Option<FloorSnapshotDigest>,
}
```

### 1.7 Storage — dedicated WAL + snapshot owned by `org_floor.rs`
(Q11 answered)

Not Mikoshi/datafort machinery — authority state is tiny and
specialized; the guarantees are clearer in a dedicated store.

Commit path: `append canonical signed record → fsync log → update
in-memory maximum`. Compaction: `write canonical maxima to temp
snapshot → fsync snapshot → atomic rename → fsync parent directory
→ rotate/truncate log`.

Recovery rules (locked): incomplete tail record → truncate safely;
invalid checksum/signature in the committed region → LOUD
corruption failure — never silently skip and continue with weaker
floors; single-writer lock enforced; snapshot and log carry format
version + domain; recovery replay passes through the same monotone
merge; untrusted frame sizes checked before allocation. Acceptance
bar: the crash-injection harness (commit-path and compaction-path
fault points).

All v0.4 floor semantics stand: write-through before admission
visibility; unhealthy-store fail-closed; indefinite retention;
carrier-independent inner authority; thirteen witnesses — now
nineteen (§Tests).

---

## What ships — Phase 2: capability-specific delegated admission

### 2.1–2.3 — as v0.4, unchanged

(`CapabilityAuthorityId`; `OrgDelegation` sibling type with
reserved-unenforced generation; provider-local per-capability
`CapabilityPolicy` with dual bounds, measured fixture, fail-closed
`Extension`.)

### 2.4 `AdmissionProof` — explicit timestamp semantics (Q13)

Field renamed for unit-explicitness in a signed transcript:

```rust
AdmissionProof {
    membership_cert:           OrgMembershipCert,
    delegation_chain:          Vec<OrgDelegation>,
    /// Unix NANOSECONDS (matches nRPC's deadline_ns convention).
    proof_expires_at_unix_ns:  u64,
    call_binding_sig:          [u8; 64],
}
```

Caller-side selection rule (the proof-provider hook's default):

```
if rpc.deadline_ns == 0:
    proof_expires_at_unix_ns = now + default_proof_ttl   // 30 s
                                                         // provisional
else:
    proof_expires_at_unix_ns = min(rpc.deadline_ns,
                                   now + default_proof_ttl)
// and, where practical:
proof_expires_at_unix_ns <= effective chain not_after
// (a proof should not claim admission life beyond its own
//  delegation's validity)
```

30 s stays provisional until Phase-2 latency/RPS measurement. The
provider-advertised maximum is caller guidance only; the callee's
local `AdmissionReplayConfig` remains authoritative. Callee checks
and the `"net-org-call-v3"` transcript binding are as v0.4 (the
transcript field is the renamed ns value; golden vectors name the
unit).

**Clock discipline (locked):** expiry is VERIFIED against
wall-clock protocol time (with skew allowance); replay-map
retention is then derived onto a monotonic local `Instant`, so a
local wall-clock rollback cannot evict a key prematurely.

### 2.5–2.6 — as v0.4, unchanged

(Callee gate order; unary-only scope with distinct streaming
rejection; `(caller, call_id)` replay identity with
replay/collision denial; never-evict-unexpired; capacity
deny+metric; volatile-guard residual with idempotency guidance.)

### 2.7 Scope statement — foreign access (wording corrected)

v1 `OwnerDelegation` semantics, stated plainly:

```
valid owner member
+ valid owner-rooted dispatcher delegation
→ may request invocation
```

Every `AdmissionProof` carries a mandatory owner membership
certificate; a foreign actor without owner membership cannot use
v1 `OwnerDelegation`. That is intended for this plan's
internal-organization objective. Everywhere earlier revisions said
"foreign access = grants," read:

```
foreign access = FUTURE explicit grants;
not implemented by v1 OwnerDelegation
```

When foreign invocation ships, it is a separate policy/proof arm
(under the reserved `Extension` discriminant or a new reviewed
variant) in which delegation authenticates the foreign subject
WITHOUT issuing it a false ownership membership certificate.
`membership_cert` is not made optional globally; the internal
default is not weakened.

---

## What ships — Phase 3: owner-scoped discovery

As v0.4 in full (three visibility modes; epoch-keyed locators with
two-key overlap and the retirement inequality; locator trust
binding; `DiscoveryAuthority` v2 with epoch; `CapabilityDetailBody`
wire enum with inline bound; plaintext-interest boundary settled by
the sensing amendment — recommended option (2), option (1) iff
proven, option (3) as documented interim). Review-4 approved the
architecture; implementation remains gated on that amendment. One
alignment: the amendment's owner-scoped registration/serving paths
adopt the same tracked-org principle — a leader/provider serves
owner-domain state only for orgs it is provisioned to serve, never
because a frame named one.

---

## What this plan deliberately does NOT include

As v0.4 (no ACL engine; no multi-org ownership; no enrollment
protocol; no caches before measurement; no parity in this plan; no
delegation floors; no cross-restart exactly-once; no streaming org
admission; no floor GC/org retirement; no compact anti-entropy
summaries), plus newly explicit:

- **No foreign-org invocation in v1** (§2.7) — future explicit
  grant arm, separately reviewed.
- **No network auto-enrollment of tracked orgs** — tracking is
  local provisioning, only (§1.5); `ExplicitRelyingParties` is
  reserved shape, not shipped behavior.

---

## Risks

1–3 as v0.4. 4. floor propagation lag — unchanged, witnessed.
5. **Floor-store durability** — now concretely the §1.7 WAL
   contract; crash-injection harness is the acceptance bar.
6. **Replay-guard memory** — unchanged (proof-lifetime exact,
   ceilings, deny+metric).
7. root-key ceremony — unchanged (inject-floor is carrier-side
   only; the root never automates).
8. size coupling — unchanged, now also covering floor sync pages
   (dual bounds + fixture).
9. discovery-key/epoch handling — unchanged.
10. denial-surface triage — unchanged.
11. **Tracked-set misconfiguration** — new: a relying party that
    forgets to pin a foreign root simply ignores its floors
    (fail-closed toward not-tracking); the dangerous direction —
    auto-enrollment — is structurally absent (T-floor-18). Unknown-
    floor counter metrics are bounded and unlabeled by org to keep
    the metrics plane from becoming the amplification surface
    (T-floor-19).

---

## Phases

### Phase 1 — scaffolded ownership, dark authority facts
As v0.4 plus: tracked-org ingest boundary, authenticated org-scoped
sync with bounded pages, `inject-floor` local admin path, dedicated
WAL/snapshot store per §1.7. Exit: T1–T6 + T-floor(1–19) green
incl. crash injection; bench recorded; adopt-path loud failure
demonstrated.

### Phase 2 — capability-specific delegated admission (unary)
As v0.4 plus: unit-explicit proof timestamp + selection rule,
monotonic retention clocks, foreign-access scope statement.
Emission stays dark until deployable fleet-wide.

### Phase 3 — owner-scoped discovery
Unchanged from v0.4; gated on the sensing amendment (which now also
carries the tracked-org alignment note).

---

## Test plan

T1–T5, T7–T18 as v0.4 (with golden vectors naming
`proof_expires_at_unix_ns` and covering floor-sync
request/page/digest forms; T8/T9 use the renamed field; T-floor
1–13 unchanged). T6 as v0.4. New floor witnesses (review-4,
verbatim):

- **T-floor-14** — a peer mints many self-signed unknown
  organizations and floods valid-signature floors → no durable
  rows, no rebroadcast, store size unchanged.
- **T-floor-15** — unauthenticated / foreign sync request → no
  floor pages returned.
- **T-floor-16** — valid owner-member sync request → bounded pages
  returned (count AND byte limits enforced at decoder before
  allocation; fixture-verified worst case).
- **T-floor-17** — requester whose membership is below the current
  floor → sync refused.
- **T-floor-18** — a trusted root must be locally provisioned;
  receiving its signed floor cannot auto-enroll it (record from an
  untracked-but-later-pinned org is only accepted after local
  pinning, via re-announce/sync — never retroactively from cache).
- **T-floor-19** — no unknown-org identifier becomes a persistent
  or high-cardinality metrics label; the unknown-floor counter is
  bounded.

Plus, from §2.4's clock discipline: a wall-clock rollback on the
callee does not evict unexpired replay keys (added to T9).

---

## Migration

As v0.4, with tracked-set provisioning folded into step 2 (adopt
writes `TrackedOrgAuthority { owner_org, OwnerMembersOnly }`
alongside `NodeAuthorityConfig`) and floor-store WAL paths
provisioned/health-checked at the same step. No other changes.

---

## Locked design points

1–3 as v0.4 (OrgId IS the key; singular scaffolded ownership;
membership never enters `may_execute`).
4. Revocation floor store as v0.4 (dedicated, write-through,
   indefinite, carrier-independent) **plus the authority boundary:
   floors are ingested, stored, served, and rebroadcast only for
   locally provisioned tracked orgs; network receipt never enrolls;
   sync is org-scoped and member-authenticated under
   `OwnerMembersOnly` in v1; storage is the §1.7 dedicated
   WAL+snapshot with loud-corruption recovery.**
5–8 as v0.4.
9. Replay guard as v0.4, **plus explicit clock discipline:
   wall-clock verification, monotonic retention.** Proof expiry is
   unit-explicit (`_unix_ns`) inside the signed transcript, with
   the deadline-less selection rule and chain-window cap stated.
10–13 as v0.4 (visibility modes; sensing proofs + amendment gate;
    no cache/constants before measurement; unary-only admission).
14. **Foreign access is future explicit grants** — v1
    `OwnerDelegation` is owner-member + owner-rooted delegation by
    construction; no global optional `membership_cert`; the
    internal default is never weakened to accommodate a feature
    that isn't shipping.

---

## Open questions (v0.5)

- **Q14 — `FloorSnapshotDigest` shape.** Digest over the sorted
  canonical maxima + store format version (proposal:
  blake3-of-concatenated-records + u32 version); confirm at
  Phase-1 implementation review alongside the paging fixture.
- **Q15 — unknown-floor counter surfacing.** Single bounded counter
  vs a small fixed histogram (by frame size class) for the dropped
  unknown-org floors — observability wants some signal for abuse
  detection without per-org labels; pick at Phase-1 review.

Everything else previously open is answered: Q11 (§1.7 WAL),
Q12 (§1.6 bounds), Q13 (§2.4 rule + units).
