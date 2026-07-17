# OA2-E Live Integration — Design (PROPOSAL, not yet authorized)

**Status:** DESIGN ONLY. No code, no behavior change. This document
specifies the bounded OA2-E integration commit that turns the parked
OA-2 groundwork into running provider-side authorization.

**It MUST NOT be implemented until BOTH gates pass:**

1. OA-1 review-11 independently signed off, AND
2. the OA2-A–E-partial primitives audited (§2.4 ordering, binding,
   replay, attribution) as unwired.

This is the step Kyra flagged as crossing the security boundary:
"preparing the authority engine" → "turning it into authority." The
purpose of writing it now is only to have a reviewable, ready-to-run
plan so the eventual commit is small and mechanical.

---

## 0. What already exists (parked, unwired)

| Symbol | Module | Role |
|---|---|---|
| `OrgCapabilityGrant`, `OrgDispatcherGrant`, `CapabilityAuthorityId`, `GrantRights`, `GrantTargetScope`, `OrgAudienceSecret` | `behavior/org_grant.rs` | grant family (§2.1–2.2) |
| `OrgCallProof`, `CallBinding`, `ORG_ADMISSION_HEADER`, `MAX_ORG_CALL_PROOF_BYTES` | `behavior/org_call.rs` | per-call proof + binding (§2.3) |
| `AdmissionReplayGuard`, `ReplayOutcome`, `AdmissionReplayConfig` | `behavior/org_admission_replay.rs` | replay guard (§2.5) |
| `OrgAdmission`, `AdmissionContext`, `AdmissionDenied`, `Admitted`, `verify_org_admission` | `behavior/org_admission.rs` | ordered decision (§2.4) |
| `has_local_capability` | `behavior/fold/capability_bridge.rs` | tag-presence, no allow-lists (§2.4a) |

None of these has a caller outside its own module + tests today. The
integration commit is what adds the first real callers.

---

## 1. Scope of the OA2-E commit

**In (one bounded commit):**

- `RegisteredCapability { descriptor, visibility, admission }` and the
  per-service registration surface.
- Policy-directed gate selection in the callee-side RPC intake.
- `AdmissionContext` assembly from the verified inbound call.
- `request_digest` canonicalization (proof header removed).
- `RpcStatus::AdmissionDenied = 0x0009` + reason surfacing.
- One per-node `AdmissionReplayGuard` + eviction.
- Emission-by-visibility split (plaintext `Public` / `OwnerScoped`
  fanout-controlled; `GrantedAudience` withheld from plaintext).
- End-to-end serve_rpc dispatch witnesses (admit + each deny class).

**Out (deferred):**

- The encrypted `ScopedCapabilityAnnouncement` envelope + private
  discovery propagation — that is OA-3. OA2-E's emission split only
  needs to (a) carry `visibility` on the registration and (b) keep a
  `GrantedAudience` descriptor OUT of plaintext broadcast bytes. The
  encrypted form is an OA-3 concern.
- CLI/SDK grant-minting and the §2.6 wire gate — that is OA2-F, which
  layers on top of this commit.

**Untouchable (invariants this commit must preserve):**

- `may_execute` stays **byte-for-byte identical** (verified 2,586
  bytes at review-11). Public services route through it unchanged.
- The review-11 `StoreCore` / `PublishGuard` / send-seqlock surfaces
  are not modified — admission only READS `org_revocation` snapshots
  and the installed `NodeAuthority`.
- Fold state, decrypted announcements, and discovery responses are
  never admission evidence (Locked #3).

---

## 2. The registration model (§2.4a)

```rust
// behavior/org_admission.rs (or a new behavior/org_registration.rs)
pub enum CapabilityVisibility { Public, OwnerScoped, GrantedAudience }

pub struct RegisteredCapability {
    pub descriptor: CapabilityDescriptor, // existing service descriptor
    pub visibility: CapabilityVisibility, // emission projection
    pub admission:  OrgAdmission,         // the gate to run (already exists)
}
```

- The LOCAL registry / self-fold ALWAYS receives the capability (with
  empty v0.4 allow-lists), so the exact service is locally
  registered/capable — `has_local_capability(self)` is true for it.
- Registration of an `OwnerDelegated` / `CrossOrgGranted` service
  REQUIRES an installed `NodeAuthority` (the provider must have a
  proven owner org to admit against). Registering org-protected
  without an authority is a loud refusal at registration time — never
  a silent fail-open.

Where it plugs in: the existing per-node service registry
(`rpc_local_services`, the `nrpc:<service>` set on `MeshNode`) gains a
parallel `DashMap<service_name, RegisteredCapability>`. Public
services default to `RegisteredCapability { …, Public,
PublicAuthenticated }`, so pre-OA-2 registrations are unchanged.

---

## 3. The callee-side gate (the load-bearing change)

**Location:** the server-fold inbound RPC path, BEFORE the user
handler is spawned — the same point that today produces
`RpcStatus::CapabilityDenied` (0x0008). Concretely, the dispatch that
builds an `RpcContext` (`cortex/rpc.rs`; `RpcContext.caller_origin` is
the AEAD-verified peer origin_hash, `.call_id`, `.payload`).

**Gate selection (policy-directed, review-7 correction §2.4a):**

```text
resolve RegisteredCapability for payload.service   (unknown → today's path)
match registered.admission {
    PublicAuthenticated =>
        // EXACTLY today's behavior — may_execute unchanged.
        require( may_execute(fold, self_id, tag, caller_node) )

    OwnerDelegated | CrossOrgGranted => {
        // The legacy allow-list UNION is NOT authority here.
        require( has_local_capability(fold, self_id, tag) )   // exact service registered
        let admitted = verify_org_admission(&ctx, headers, &replay, now, policy)?;
        // admitted carries the four-party attribution for audit.
    }
}
```

`may_execute` is consulted **only** on the `PublicAuthenticated` arm.
Protected services never route authority through it — the OA-2 gate is
load-bearing, red-witnessed without depending on the legacy gate's
return value (Locked #3).

**`AdmissionContext` assembly** (all provider-side facts):

| field | source |
|---|---|
| `mode` | `registered.admission` |
| `authenticated_caller` | `caller_origin` → `EntityId` via the TOFU `peer_entity_ids` binding (NOT self-claimable) |
| `provider` | `self.identity.entity_id()` |
| `provider_owner_org` | installed `NodeAuthority.owner_org()` (loud deny if none) |
| `invoked_capability` | `CapabilityAuthorityId::for_tag(nrpc_tag_for(service))` |
| `call_id` | `ctx.call_id` |
| `request_digest` | canonical request, admission header removed (§4) |
| `is_unary` | `false` for streaming REQUEST kinds → `StreamingUnsupported` |
| `floors` | `org_revocation` snapshot (installed store) |
| `skew_secs` | `NodeAuthority.config.verification_skew_secs` |

**Header extraction:** collect every `payload.headers` value whose
name == `ORG_ADMISSION_HEADER`; pass the slice to
`verify_org_admission`, which enforces exactly-one.

**Deny mapping:** `AdmissionDenied` → `RpcStatus::AdmissionDenied`
(0x0009). The reason rides a response header (e.g.
`net-org-admission-reason: <variant>`) for audit — distinguishable
`Replay` vs `CallIdCollision` vs `BindingInvalid`, etc. The body stays
a short diagnostic. No reason leaks credential material.

---

## 4. `request_digest` canonicalization

The binding covers "the whole canonical request minus the proof
header" (§2.3). The caller and the provider must compute an IDENTICAL
digest.

Proposed canonical form (deterministic, both sides):

```text
digest = blake3(
    service_len ‖ service
  ‖ deadline_ns
  ‖ flags
  ‖ sorted(headers WITHOUT net-org-admission)  // (name,value) length-prefixed, byte-sorted
  ‖ body_len ‖ body
)
```

- The `net-org-admission` header is excluded (it carries the proof,
  which is being signed).
- Headers are byte-sorted so header ordering is not load-bearing (the
  transport may reorder). Documented as the canonical rule; the caller
  helper and the provider gate share ONE implementation.
- Everything else (`service`, `deadline_ns`, `flags`, `body`) is bound
  — a relay cannot re-point the proof at a different body/deadline.

Caller side (OA2-F/SDK): a helper that builds the proof header
computes this digest over the request it is about to send, minus the
header it is about to add.

---

## 5. Replay guard lifecycle

- One `AdmissionReplayGuard` per `MeshNode` (a new field), created with
  `AdmissionReplayConfig::default()`.
- `verify_org_admission` calls `.admit(caller, call_id, binding_digest,
  expires_at, now)` as its step 10 — atomic insert-or-deny BEFORE the
  handler runs.
- Eviction: a low-frequency sweep (piggyback an existing timer, or a
  dedicated interval) calls `evict_expired(Instant::now())`. Lazy
  reclamation at capacity is already built in.
- Volatile by contract; cross-restart idempotency is the application's
  (Locked #8).

---

## 6. Emission by visibility (the projection half of §2.4a)

The announce path (`announce_from_baseline` / `index_self_with_local
_services`) branches by each registered capability's visibility:

- `Public` → plaintext `nrpc:<service>` tag as today.
- `OwnerScoped` → plaintext scoped form, EXCLUDED from unscoped query
  projection (labeled fanout control, not confidentiality — Locked
  #10). Realized as a reserved scoped tag the unscoped discovery query
  filters out.
- `GrantedAudience` → **withheld from plaintext broadcast bytes
  entirely** in OA2-E. The encrypted `ScopedCapabilityAnnouncement`
  envelope that actually delivers it is OA-3; OA2-E's obligation is
  only the negative one: a `GrantedAudience` descriptor NEVER appears
  in plaintext broadcast bytes (witnessed by a byte-scan).

This interacts with the review-11 send path only as an input to what
`caps` the announcement carries — it does NOT touch the
`SendStamp`/seqlock or the owner-cert emission machinery.

---

## 7. Migration ordering (§Migration step 5)

An OLD provider with a permissive local entry serves any authenticated
caller. Therefore, PER protected service, the operator order is fixed:

```text
upgrade provider code
  → register the service with OwnerDelegated / CrossOrgGranted admission
    → only THEN emit / enable the protected service
```

Enabling a protected service before its admission is registered would
be a fail-open window; registration refuses org-protected modes
without an installed authority, and the emit step is gated on
registration.

---

## 8. Wire / status additions

- `RpcStatus::AdmissionDenied = 0x0009` (next after `CapabilityDenied
  = 0x0008`), plus `to_wire` / `from_wire` arms and the reserved-range
  round-trip test.
- No change to `RpcRequestPayload` layout — the proof rides an
  existing header; the response reason rides an existing header.
- Static size assertion: the encoded proof header ≤
  `MAX_RPC_HEADER_VALUE_LEN` (4096) — already pinned at 1024 in
  `org_call.rs`.

---

## 9. Witnesses the OA2-E commit must add (the dispatch proof)

Integration tests against real `MeshNode` RPC, mirroring the OA-1
`org_ownership.rs` style:

1. **Admit end-to-end** — B-owned provider registers a
   `CrossOrgGranted` service; an A-caller with membership + dispatcher
   + B→A INVOKE grant calls it; the handler runs; response OK; the
   audit attribution is the four-party identity.
2. **Owner-delegated admit** — same-org caller, no capability grant,
   admitted.
3. **Each deny class reaches the wire as 0x0009** with the right
   reason header: missing/duplicate header, malformed proof,
   member-binding mismatch, foreign issuer, target-not-covered,
   insufficient rights, revoked membership (floor), transplanted
   binding (wrong call_id/body), replay, call-id collision, expired
   proof, provider-policy veto.
4. **Streaming rejected** — an org-protected streaming REQUEST →
   `AdmissionDenied` / `StreamingUnsupported`, never admitted under a
   binding covering only the initial payload.
5. **Public unchanged** — a `PublicAuthenticated` service admits/denies
   EXACTLY as before (may_execute), with and without an installed
   authority present.
6. **Load-bearing gate (red witness)** — a protected service on a node
   whose OTHER capability carries a restrictive allow-list is admitted
   for a legitimate org caller (proving the legacy union is not
   consulted), and denied for a caller with a bad proof even though
   `may_execute`'s union would have said something else.
7. **Emission** — a `GrantedAudience` descriptor is byte-absent from
   plaintext broadcast; a `Public` one is present.
8. **Authority-dark preserved** — a node with NO org-protected
   registrations behaves identically to pre-OA-2 (may_execute only).

---

## 10. Risk register / boundary checks for the eventual commit

- **may_execute byte-identity** — re-verify the function bytes are
  unchanged after the commit (the review-7/Locked #3 invariant).
- **No fail-open** — an org-protected service with no installed
  authority, a missing proof, or ANY verification failure DENIES;
  there is no path where an unverified call reaches the handler.
- **Replay before handler** — the atomic insert-or-deny must precede
  handler dispatch; a handler must never run twice for one
  `(caller, call_id)`.
- **Digest agreement** — the caller-side and provider-side
  `request_digest` share one implementation; a divergence would fail
  every legitimate call closed (safe, but must be caught by test 1).
- **Skew/floor sourcing** — floors from the INSTALLED store snapshot
  (not a detached handle), skew from the persisted authority config —
  reuse the review-11-correct accessors.
- **Bounded** — one commit; touches the registry, the callee gate, the
  status enum, the announce visibility branch, and tests. It does NOT
  touch `may_execute`, the `StoreCore`/seqlock surfaces, or OA-3
  envelope machinery.

---

## 11. Execution checklist (run only when authorized)

- [ ] Gate 1: OA-1 review-11 signed off.
- [ ] Gate 2: OA2-A–E-partial audited as unwired.
- [ ] `RegisteredCapability` + registry surface + org-protected-needs-
      authority refusal.
- [ ] `request_digest` canonical helper (shared caller/provider).
- [ ] `RpcStatus::AdmissionDenied = 0x0009` + round-trip.
- [ ] Per-node `AdmissionReplayGuard` + eviction.
- [ ] Callee gate: policy-directed selection; `AdmissionContext`
      assembly; deny→status/reason mapping.
- [ ] Emission-by-visibility branch (+ `GrantedAudience` byte-absence).
- [ ] Witnesses §9.1–§9.8.
- [ ] Gates: `may_execute` byte-identity, fmt, both clippy, full lib +
      org_ownership + CLI + a new org_admission_wire integration file.
- [ ] Then OA2-F (CLI/SDK + §2.6 gate).
```
