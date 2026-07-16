# Organization Capability Auth Plan

**Status:** Draft — not started.
**Scope:** cryptographic organization membership for nodes, org-scoped
capability announcements, and org-verified admission of incoming work.
Three phases: (1) org identity + membership certs + org allow-list
axis, (2) invocation delegation carried per-request and verified at
the callee, (3) owner-scoped discovery over the sensing path.
**Supersedes:** the deferred "v3" of `SCOPED_CAPABILITIES_PLAN.md`
(§"Follow-ups") — "multiple organizations sharing one mesh under
strict cross-tenant requirements" is now the requirement.

---

## Context

Capability auth v0.4 (`CAPABILITY_AUTH_PLAN.md`, shipped) gates
execution with three allow-list axes on `CapabilityAnnouncement`:
`allowed_nodes`, `allowed_subnets`, `allowed_groups`. Membership on
the subnet/group axes is **self-declared** via `subnet:<hex>` /
`group:<hex>` tags on the caller's own announcement. That is safe
only in the bearer-secret sense: a random `GroupId` is unguessable,
so knowing the bytes IS membership.

Bearer-secret membership has four structural limits an organization
boundary cannot live with:

1. **No verifiable belonging.** A receiver cannot prove "node N is a
   member of org O" — it can only observe that N emitted the right
   bytes. Anyone the secret leaks to is indistinguishable from a
   member.
2. **No per-member revocation.** Rotating the secret evicts everyone
   at once and requires out-of-band redistribution to every member.
3. **The secret rides every announcement.** Group tags are inside the
   (signed, plaintext) `CapabilitySet`, so the credential is
   broadcast to the entire mesh, hop-forwarded up to
   `MAX_CAPABILITY_HOPS = 16`.
4. **Membership conflates into invocation authority.** v0.4's gate
   admits any caller matching any axis. There is no way to say "this
   node belongs to the org but may not dispatch work."

Separately, discovery is permissive-global: every announcement fans
out to the whole mesh. `SCOPED_CAPABILITIES_PLAN.md` added query-time
tag filtering but deliberately punted on real visibility control
("doesn't prove anything, leaks everything it always leaked").

The design target (per the visibility/admission review):

```
Visibility controls knowledge.  Admission controls authority.
Provider policy controls execution.  Never substitute one for another.
```

For an internal organizational capability the default is:

```
visibility:  owner members only            (Phase 3)
admission:   valid owner delegation        (Phase 2)
belonging:   org-signed membership cert    (Phase 1)
```

---

## Model

```
ORG IDENTITY
    OrgId = org's ed25519 public key (32 bytes, self-certifying —
    same construction as EntityId). Org signing key lives offline
    with the operator.

MEMBERSHIP (Phase 1)
    OrgMembershipCert {
        org_id:      OrgId        // 32 — issuer, also the verify key
        member:      EntityId     // 32 — the node's entity key
        not_before:  u64          //  8
        not_after:   u64          //  8
        generation:  u32          //  4 — reserved, see Locked #4
        nonce:       u64          //  8
        --- signed above (92 bytes) ---
        signature:   [u8; 64]     // ed25519 by org_id
    }                             // wire: 156 bytes

    Nodes attach certs to their CapabilityAnnouncement. Receivers
    verify at fold ingest:
        sig valid under org_id
        cert.member == announcement.entity_id   (TOFU-bound)
        now ∈ [not_before − skew, not_after + skew]
    Only verified OrgIds land in the fold's NodeId → member_orgs view.

SCOPED ANNOUNCEMENT (Phase 1)
    allowed_orgs: Vec<OrgId> — fourth allow-list axis, union
    semantics with the existing three.

EXECUTE check (Phase 1), caller C invokes tag T on announcer A:
    1–6. unchanged from v0.4 (§Model of CAPABILITY_AUTH_PLAN.md)
    6b.  if C's VERIFIED member_orgs ∩ A.allowed_orgs → allow
    7.   else → deny

ADMISSION (Phase 2), independent of the above:
    A's announcement may declare
        admission: OwnerDelegation(org_id)
    Caller must then carry, in the RPC request itself:
        AdmissionProof {
            membership_cert:   OrgMembershipCert,
            delegation_chain:  Vec<OrgDelegation>,   // root issuer == org_id
            call_binding_sig:  [u8; 64],             // replay binding
        }
    Callee verifies fresh, per call, before fold.apply. Membership
    without delegation does NOT admit — a GPU worker can be a member
    and still be denied dispatch authority.

DISCOVERY VISIBILITY (Phase 3):
    Owner-only capabilities stop riding the CAP-ANN flood (or ride
    it summary-only). Private detail is delivered over the sensing
    interest path after the consumer's DiscoveryAuthority
    (membership cert + signed interest) validates.

REVOCATION:
    Membership: cert expiry. Issue short-lived certs (recommended
    not_after − not_before ≤ 24h) and re-issue on the announcement
    cadence. `generation` is on the wire from day one so a floor-
    based scheme can ship later without a wire break (Locked #4).
    Delegation: expiry + the same reserved generation field.
    Announcement scope: unchanged — publish version+1 with new lists.
```

---

## What ships — Phase 1: org identity, membership, org axis

### 1.1 `OrgId` — `src/adapter/net/behavior/org.rs` (new)

32-byte ed25519 public key. `Debug + Display + Hash + Eq + Copy`,
serde (hex when human-readable, raw bytes otherwise — mirror
`EntityId`'s impls in `identity/entity.rs`), postcard round-trip,
`from_bytes` / `as_bytes`, `verifying_key()`.

**Deliberate difference from `GroupId`/`SubnetId`: derived
`PartialEq`, NOT constant-time.** An `OrgId` is a public key —
knowing it grants nothing — so the `subtle::ConstantTimeEq` treatment
those bearer-secret types need does not apply. Document this in the
module header so nobody "fixes" it into ct_eq or, worse, concludes
GroupId's ct_eq is optional.

### 1.2 `OrgKeypair` + `OrgMembershipCert` — same module

`OrgKeypair` mirrors `EntityKeypair` (generate, from_bytes, sign).
`OrgMembershipCert` mirrors `PermissionToken`'s canonical byte layout
discipline (`identity/token.rs`): fixed-offset signed payload, strict
length check on decode, `verify()` using `verify_strict` (see the
malleability rationale on `EntityId::verify` — same caching-on-bytes
concern applies here).

Constraints, mirroring token constants:

- `MAX_CERT_TTL_SECS` — reuse `MAX_TOKEN_TTL_SECS`' value; reject
  longer at issue AND at verify.
- Clock skew — reuse `MAX_TOKEN_CLOCK_SKEW_SECS` handling.
- `MAX_ORG_CERTS_PER_ANNOUNCEMENT: usize = 8`. Certs are 156 bytes
  each; 8 keeps the announcement well under the wire-size ceiling
  that motivated `MAX_ALLOW_LIST_LEN = 64`. A node in more than 8
  orgs is an operator smell, not a substrate requirement.

### 1.3 Two fields on `CapabilityAnnouncement` — `behavior/capability.rs`

```rust
/// Org membership certificates for THIS announcer. Verified at
/// fold ingest; unverifiable certs are dropped (not the whole
/// announcement). Capped at MAX_ORG_CERTS_PER_ANNOUNCEMENT.
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub org_certs: Vec<OrgMembershipCert>,

/// Org allow-list — callers with a VERIFIED membership cert for
/// any listed org may invoke. Empty = no org grant (the other
/// axes still apply). Capped at MAX_ALLOW_LIST_LEN.
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub allowed_orgs: Vec<OrgId>,
```

**`SignedPayloadCanonical` moves in lockstep** — add the two
`serialize_field` / `skip_field` pairs in declaration order, after
`allowed_groups`. This is the #1 way to break rolling upgrades; the
byte-identity test (§Tests, T1) pins it.

Builder helpers: `with_org_cert(cert)`, `allow_org(org_id)` with cap
enforcement at build time (mirror the existing allow-list builders).

### 1.4 Fold ingest verification — `behavior/fold/capability_bridge.rs`

Where announcements convert to fold entries, verify each cert per
§Model. Verified OrgIds land on the fold payload
(`behavior/fold/capability.rs`):

```rust
/// Orgs this publisher has PROVEN membership in — populated only
/// from certs that verified at ingest. Unlike allowed_groups /
/// the caller's group tags, this is not self-declared.
pub member_orgs: Vec<OrgId>,
/// Mirror of the announcement's allowed_orgs axis.
pub allowed_orgs: Vec<OrgId>,
```

Both fields also get `Vec::new()` at every existing payload
construction site (~5 sites in `fold/capability.rs`, one in
`mesh.rs:~22340`; grep `allowed_groups: Vec::new()` and mirror).

**Verification cost + cache.** One ed25519 verify per cert per
announcement, and announcements re-broadcast every ~TTL/2 with
identical certs. Add a small verified-cert LRU keyed on the cert's
signature bytes (sig uniquely commits to the payload under
verify_strict): hit → skip the verify, but ALWAYS re-check the time
window and the member == entity_id binding (those are contextual,
not cacheable). Size ~1024 entries; follow the `OriginKeyedLru`
pattern in `mesh_rpc.rs`.

### 1.5 Execute gate — org axis — `fold/capability_bridge.rs`

Extend `may_execute` (bridge line ~459), `may_execute_batch`,
`may_execute_with_caller`, and `derive_caller_axes`:

- Target side: accumulate `allowed_orgs` alongside the other three
  lists; the "all axes empty → permissive" check at ~487 now spans
  four axes.
- Caller side: `derive_caller_axes` returns
  `(caller_subnet, caller_groups, caller_member_orgs)` — the org set
  read from the caller's fold entry's **`member_orgs`** (verified),
  never from tags.
- Admit on `caller_member_orgs ∩ allowed_orgs ≠ ∅`, short-circuit on
  first match (Locked #2 of the v0.4 plan carries over).

Both existing wire points come for free: caller-side pre-flight in
`Mesh::call_service` and the callee-side defense-in-depth in
`serve_rpc` (`mesh_rpc.rs` ~2156, ~2406) already call `may_execute`.
No new plumbing in Phase 1.

### 1.6 CLI — `net org` verbs

- `net org keygen --out <path>` — generate an org keypair.
- `net org issue-cert --org-key <path> --member <entity-hex>
  --ttl-secs N --out <path>` — emit a cert (JSON bytes).
- `net cap announce` grows `--org-cert <path>` (repeatable, ≤8) and
  `--allow-org <hex64>` (repeatable, ≤64).

One issue verb, no `revoke` verb — revocation is expiry + declining
to re-issue, consistent with Locked #3 of the v0.4 plan.

---

## What ships — Phase 2: invocation delegation (admission)

Membership proves belonging; it must not imply dispatch authority.
Phase 2 adds the per-request proof.

### 2.1 `OrgDelegation` — `behavior/org.rs`

Structurally `PermissionToken` with the channel axis swapped for a
capability axis:

```
issuer:           32   // OrgId at the root; EntityId below it
subject:          32   // EntityId being empowered
scope:             4   // bits: INVOKE | DELEGATE (READ reserved)
capability_hash:   8   // blake2s64("net-org-cap-v1", tag), or
                       //   WILDCARD scope bit for all-capabilities
generation:        4   // reserved (Locked #4)
not_before:        8
not_after:         8
delegation_depth:  1
nonce:             8
--- signed above ---
signature:        64
```

Chain rules copied from `TokenChain`, not reinvented: root issuer
must equal the required `OrgId`; each link's issuer == previous
link's subject; scope monotonically narrows; validity windows
intersect; depth ≤ `MAX_CHAIN_DEPTH`; parent must carry DELEGATE.
The four-level "owner → org → team → service" shape the token module
already documents is exactly the org shape.

Decision point (see Open questions Q1): implement as a new type
mirroring `PermissionToken`, or generalize `PermissionToken` with a
scope-axis enum. Plan assumes the new type — the token's
`ChannelHash` coupling and `TokenCache` slot semantics are
channel-auth-specific, and a shared abstract type would force both
subsystems through one wire format forever.

### 2.2 `AdmissionRequirement` on the announcement

```rust
#[derive(Serialize, Deserialize, ...)]
pub enum AdmissionRequirement {
    /// v0.4 behavior: allow-list axes + transport auth only.
    PublicAuthenticated,
    /// Caller must present a delegation chain rooted at this org.
    OwnerDelegation(OrgId),
}

#[serde(default, skip_serializing_if = "Option::is_none")]
pub admission: Option<AdmissionRequirement>,
```

`None` ≡ `PublicAuthenticated` ≡ pre-Phase-2 bytes.
`SignedPayloadCanonical` lockstep again. `ExplicitGrant` /
`PolicyRef` variants from the review are future arms of this enum —
the enum is `#[non_exhaustive]` from day one so adding them is not a
break. Mirror the field onto the fold payload.

### 2.3 `AdmissionProof` carried in RPC headers

No nRPC wire break. The proof rides a well-known header on
`RpcRequestPayload.headers`:

```
name:  "net-org-admission"
value: postcard(AdmissionProof {
           membership_cert:  OrgMembershipCert,       // 156
           delegation_chain: Vec<OrgDelegation>,       // ≤ depth×~170
           call_binding_sig: [u8; 64],
       })
```

Worst case (cert + 4-link chain + sig) ≈ 900 bytes — comfortably
under `MAX_RPC_HEADER_VALUE_LEN = 4096`.

`call_binding_sig` is the caller entity's ed25519 signature over the
domain-separated transcript
`("net-org-call-v1", callee_node_id, service, deadline_ns,
blake2s(body))`. This binds the proof to THIS call: a relay or a
previously-authorized-now-revoked peer replaying a captured proof
against a different call, callee, or deadline fails the binding.
Deadline in the transcript bounds the replay window for a
byte-identical re-send; idempotency is the application's concern as
today.

### 2.4 Callee-side admission gate — `mesh_rpc.rs`

In `serve_rpc`'s intake, after the existing `may_execute` check and
before `fold.apply`:

1. Look up the target capability's `admission` from OUR OWN
   announcement state (the provider enforces its own policy — never
   the fold copy of someone else's).
2. `PublicAuthenticated` / absent → done (v0.4 path).
3. `OwnerDelegation(org)` → require the header; verify: membership
   cert valid and `member == caller EntityId` (resolved via the
   `peer_entity_ids` TOFU binding — Locked #1); chain roots at
   `org`, leaf subject == caller EntityId, leaf scope ⊇ INVOKE,
   `capability_hash` matches the invoked tag (or WILDCARD); windows
   valid; `call_binding_sig` verifies against the caller EntityId
   over this call's transcript.
4. Any failure → `RpcStatus::AdmissionDenied` (0x0009, next free
   after `CapabilityDenied = 0x0008`) + typed
   `RpcError::AdmissionDenied { target, capability }`.

Caller-side: `CallOptions` grows `admission_proof: Option<...>` (or
a mesh-level default provider closure so schedulers set it once).
Caller pre-flight can fast-fail from the fold's `admission` copy but
MUST NOT treat fold state as authoritative — the callee re-verifies
regardless.

Verification cost per call: 3 ed25519 verifies + chain walk. Cache
(cert, chain) validation keyed on concatenated signature bytes, same
LRU pattern as §1.4; `call_binding_sig` is verified every call, never
cached.

### 2.5 CLI

`net org delegate --key <issuer-key> --subject <entity-hex>
--capability <tag> [--delegatable] --ttl-secs N --out <path>` —
works for both the root link (org key) and sub-links (entity key).

---

## What ships — Phase 3: discovery visibility

### 3.1 Publication split — `behavior/capability.rs`

```rust
pub enum DiscoveryVisibility {
    Public,                 // default; today's behavior
    OwnerOnly(OrgId),
}

pub struct CapabilityPublication {
    pub public_summary:  Option<CapabilitySet>,  // coarse, floodable
    pub private_detail:  CapabilitySet,          // full manifest
    pub visibility:      DiscoveryVisibility,
}
```

`Public` publishes exactly as today. `OwnerOnly`: only
`public_summary` (possibly empty) enters the flooded CAP-ANN;
`private_detail` never rides the gossip. Scaffolding default for
org-provisioned nodes is `OwnerOnly(local_org)` + summary omitted —
a capability must opt OUTWARD explicitly (owner-only → public),
never the reverse.

### 3.2 Owner-validated detail delivery — over sensing

The sensing subsystem (`behavior/sensing/`) is already the
interest-routed, signed-attestation path: authenticated downstream
interests, provider-targeted routing, per-hop coalescing, signed
attestations. Owner-scoped discovery is an interest-validation hook,
not a new protocol:

- Interest frames (`sensing/frames.rs`) grow an optional
  `DiscoveryAuthority { org_id, membership_cert }`; the cert's
  `member` must match the interest's already-signed origin identity.
- Provider-side intake (`SensingInterestFrame::validated_spec` /
  `SensingLeader::register_from_frame`): interests targeting an
  `OwnerOnly(org)` capability without a valid, unexpired cert for
  `org` are refused at registration — no attestation row is ever
  created for that consumer. A matching capability digest alone MUST
  NOT authorize delivery.
- Attestations for owner-only capabilities carry the private detail
  only along rows that passed the check.

Wire caveat: `sensing/wire.rs` is frozen ("changing any wire-borne
type from here on is a wire break"). The frame extension therefore
follows sensing's own amendment process (the review-7
`ProviderRegistration` precedent) — Phase 3 does not start until
that's signed off. This is the main reason 3 is sequenced last.

### 3.3 Explicitly deferred within Phase 3

Routing scope ≠ confidentiality. If interest/attestation paths cross
only org-member nodes, owner-scoped routing suffices for v1. If
paths may cross foreign relays AND the metadata is sensitive, the
payload needs a signed outer routing envelope with an encrypted
capability payload readable only by authorized members. That is a
follow-up plan (recipient key wrapping / org broadcast encryption),
gated on an actual foreign-relay deployment. Until it ships, docs
MUST NOT claim confidentiality for owner-only announcements — only
reduced fanout.

---

## What this plan deliberately does NOT include

- **No policy language / ACL engine.** Admission classes are generic
  authority shapes; tool-specific rules (path scoping, rate limits,
  contract/payment for commercial capabilities) stay provider-local,
  as today.
- **No org hierarchy beyond delegation chains.** Sub-orgs, cross-org
  federation, org-to-org trust: out.
- **No revocation gossip.** `generation` is reserved wire space
  (Locked #4); the propagation mechanism for a floor bump is a
  follow-up. v1 revocation is short expiry.
- **No encrypted announcements** (§3.3).
- **No Go / TS / Python parity in this plan.** Rust core first. The
  Go side (`groups.go`, `capabilities.go`, golden vectors,
  `header_parity_test.go`) gets a parity follow-up plan; the wire
  formats here are frozen with cross-language golden vectors from
  day one (T2) so parity is mechanical.
- **No changes to channel-auth tokens.** `PermissionToken` is
  untouched; `OrgDelegation` is a sibling, not a refactor (Q1).

---

## Risks

1. **`SignedPayloadCanonical` drift.** Every field this plan adds to
   `CapabilityAnnouncement` (Phases 1 and 2) must be mirrored there
   in declaration order with identical skip predicates, or signature
   verification breaks across a rolling upgrade. Mitigation: T1
   byte-identity tests extended per field, plus a proptest that
   round-trips random announcements through both serializers.
2. **Old forwarders strip new fields.** A pre-upgrade forwarder
   deserializes into the old struct (unknown fields dropped),
   increments `hop_count`, re-serializes — downstream signature
   verification of the org fields' announcement then fails and the
   announcement is discarded. Same constraint v0.4 had. Mitigation:
   receivers/forwarders upgrade BEFORE any node emits `org_certs` /
   `allowed_orgs` / `admission` (§Migration). Failure mode is
   fail-closed (announcement dropped), never fail-open.
3. **Fold-ingest verify cost.** ed25519 verify per cert per
   announcement re-broadcast, mesh-wide. Mitigation: §1.4 cache;
   bench added to `benches/` alongside the existing capability
   benches; cap of 8 certs bounds worst case.
4. **Header-borne proof size.** Bounded by construction (≈900 B
   worst case vs 4096 cap), but `MAX_CHAIN_DEPTH` and cert size are
   now coupled to `MAX_RPC_HEADER_VALUE_LEN`. A static assert pins
   the relationship.
5. **Clock skew on short-lived certs.** Short TTLs + skewed clocks =
   spurious denials. Reuse the token module's skew allowance and its
   `TOKEN_CLOCK_SKEW_SECS_RECOMMENDED` guidance; conformance
   scenario T5 covers the boundary.
6. **Two gates, one outcome space.** Operators will see both
   `CapabilityDenied` and `AdmissionDenied`. Distinct status codes +
   distinct typed errors + a docs table (which gate, which evidence,
   which fix) keep triage sane.

---

## Phases

### Phase 1 — org identity + membership + org axis
`behavior/org.rs` (OrgId, OrgKeypair, OrgMembershipCert); two
announcement fields + canonical serializer + builders; fold payload
fields + ingest verification + cert cache; `may_execute` family org
axis; CLI keygen/issue-cert/announce flags; tests T1–T6.
Self-contained; ships alone. Exit: conformance green, byte-identity
pinned, bench within budget.

### Phase 2 — admission
`OrgDelegation` + chain validation; `AdmissionRequirement` field;
`AdmissionProof` header codec; callee gate in `serve_rpc` +
`RpcStatus::AdmissionDenied`; `CallOptions` proof plumbing; CLI
delegate verb; tests T7–T12. Depends on Phase 1 types only.

### Phase 3 — discovery visibility
`CapabilityPublication` split + scaffolding defaults; sensing
interest `DiscoveryAuthority` + provider-side refusal; tests
T13–T15. Gated on sensing wire-amendment sign-off.

---

## Test plan

- **T1 — signed byte identity.** Announcement with empty
  `org_certs` / `allowed_orgs` / absent `admission` serializes
  byte-identical to pre-plan form; derived-vs-canonical serializer
  proptest.
- **T2 — golden vectors.** Fixed-key cert, delegation, chain, and
  AdmissionProof bytes checked into `tests/` (Go/TS parity anchors,
  same pattern as `consent_golden_vectors_test.go`).
- **T3 — cert verification matrix.** Valid; expired; not-yet-valid;
  skew boundary; wrong member; forged sig; malleated sig (strict
  verify); TTL over cap.
- **T4 — fold ingest.** Bad certs dropped, good certs from the same
  announcement retained; `member_orgs` populated only from verified;
  cache hit still re-checks window + member binding.
- **T5 — execute-gate conformance.** Extend
  `tests/capability_auth_conformance.rs`: org-only allow-list admits
  verified member / denies non-member / denies expired-cert member;
  union with the three v0.4 axes; four-axes-empty stays permissive;
  a caller SELF-DECLARING an org via tag (no cert) is denied.
- **T6 — rolling upgrade.** Old-struct round-trip drops new fields →
  forwarded announcement fails verification downstream (fail-closed
  pinned); old reader accepts new announcement with fields defaulted.
- **T7 — chain validation matrix.** Root mismatch; broken
  issuer→subject linkage; scope widening; window non-intersection;
  depth over cap; missing DELEGATE on parent; WILDCARD vs
  capability_hash.
- **T8 — admission e2e.** `OwnerDelegation` target: member WITHOUT
  delegation denied (the load-bearing scenario); valid chain admits;
  `PublicAuthenticated` ignores proofs.
- **T9 — replay.** Captured proof re-sent against different callee /
  service / deadline / body denied via call-binding transcript.
- **T10 — revocation-by-expiry.** Chain valid at t0, expired at t1;
  fold state still lists the caller — callee still denies (proves
  admission ignores index staleness).
- **T11 — proof size static assert** vs header cap.
- **T12 — status mapping.** Wire `AdmissionDenied` → typed error,
  both call paths (mirror the existing 0x0008 mapping test at
  `mesh_rpc.rs:~3725`).
- **T13 — visibility.** OwnerOnly detail never enters CAP-ANN bytes;
  summary-only flood; scaffolding default is owner-only.
- **T14 — sensing refusal.** Interest without/with-invalid authority
  for an OwnerOnly capability refused at registration; no
  attestation row created; valid cert admits.
- **T15 — digest-alone insufficient.** Matching capability digest
  without authority does not receive detail.

---

## Migration

Rolling-upgrade ordering (Risk 2): ship the code mesh-wide with
emission OFF, then enable cert issuance / org fields per fleet. Old
nodes reading new announcements default the fields (harmless). New
fields on paths through old forwarders fail closed. Phase 2's
`admission` field follows the same ordering; callers must be able to
attach proofs before any provider declares `OwnerDelegation`, so the
enable order is: upgrade all → issue certs → callers gain proof
plumbing → providers flip admission → (optionally) providers tighten
`allowed_orgs`.

No data migration: fold state is TTL-bound (~5 min) and rebuilds
from announcements.

---

## Locked design points

1. **`OrgId` IS the org public key.** No registry, no separate id
   space, no lookup. Verification needs zero infrastructure —
   consistent with the substrate's EntityId construction and its
   no-coordinator axiom.
2. **`member_orgs` comes only from verified certs; org membership is
   NEVER tag-derived.** The `org:` tag prefix is not introduced at
   all, so there is no ambiguous half-verified path. Extends v0.4's
   Locked #1 (identity from the wire binding; tags only for
   bearer-style membership).
3. **Membership ≠ admission.** The Phase 1 org axis is an allow-list
   convenience with strictly better properties than groups
   (verifiable, per-member revocable, no broadcast secret). Anything
   labeled internal/sensitive declares `OwnerDelegation` and callers
   carry per-request proofs verified fresh at the callee — index
   state is never admission evidence (T10 pins this).
4. **`generation` is reserved wire space in both credentials from
   day one; enforcement ships later.** Adding the field after
   golden vectors freeze would be a wire break; carrying 4 dead
   bytes is not.
5. **Admission proofs ride RPC headers, not a new envelope.** The
   header surface exists, is size-capped, and old servers ignore
   unknown headers — no nRPC wire change, graceful partial-upgrade
   behavior.
6. **Visibility never substitutes for admission, and vice versa**
   (T8, T15 pin both directions). Owner-scoped routing is fanout
   reduction, not confidentiality, until the encryption follow-up
   ships — documentation must say so.

---

## Open questions

- **Q1 — `OrgDelegation` as new type vs `PermissionToken`
  generalization.** Plan assumes new sibling type (channel-auth
  coupling stays untouched; independent wire evolution). Revisit
  only if a third credential type appears.
- **Q2 — should Phase 1's org axis also admit at `OwnerDelegation`
  targets when the allow-list matches?** Plan says no (Locked #3):
  when `admission` is declared, the allow-list axes still gate
  discovery-time pre-flight but admission requires the proof.
  Confirm during Phase 2 review.
- **Q3 — cert distribution ergonomics.** Out-of-band file handoff is
  fine for v1; an org-membership issuance flow over the mesh itself
  (enrollment protocol) is a candidate follow-up.
- **Q4 — recommended cert TTL default.** Plan says 24h with
  re-issue automation guidance; tune after Phase 1 soak.
