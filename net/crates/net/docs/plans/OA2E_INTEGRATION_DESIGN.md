# OA2-E Live Integration — Design v3

**Status (2026-07-19):** the design of record. E0 substrate, the E1
provider-admission primitives, AND the E1 **live wiring** + E2 caller seam
(`#47`) are all IMPLEMENTED and **SIGNED OFF / CLOSED** (Kyra, `512cd1588`);
**OA2-F** (CLI/SDK grant management + the §2.6 exit-gate closure witnesses) is
landed. Gates 1–3 are all SIGNED OFF.

The design body below is unchanged as the specification; see the
**Progress ledger** for what has landed and the live gate state.

**Authorization is SPLIT — E0 is OA-2-neutral, so it does not inherit
the OA-1 gate:**

- **OA2-E0** (nRPC substrate hardening) — LANDED and independently
  signed off (E0.1/E0.2/E0.3/E0.4). It enabled no organization authority
  and did not wait on OA-1.
- **OA2-E1 / OA2-E2** (provider admission + caller/wire) — the primitives
  AND the live wiring (`#47`) are landed and **SIGNED OFF / CLOSED**; both
  Gate 1 (OA-1 revocation store) and Gate 3 (E0/nRPC) are re-signed.

## Progress ledger (2026-07-19, branch `org-capability-auth`)

`may_execute` (`capability_bridge.rs`) is **byte-for-byte unchanged**
across the entire series; every fix carries a red-witnessed test.

**Landed:**

- **E0 substrate** — E0.1 non-destructive registration + generation
  tokens; E0.2 channel/service equality + canonical-`u64` RpcRouteV1
  discriminator on every nRPC frame; E0.3 direct-session caller identity;
  E0.4 one wall+monotonic `ClockSample`. Plus E0-Fix1 captured-service
  equality, Fix2 token-owned service retirement, Fix3 seven-frame
  collision witness.
- **E1 primitives** — E1.1/E1.3 `RegisteredRpcService` +
  `verify_provider_authority`/`ProviderFacts`; E1.4 admission stamp +
  §9.5 stability hook; E1.5 per-caller replay ceiling; E1.7 shared
  canonical request digest (`org_admission_gate.rs`). Provider-local
  admission engine `verify_org_admission` (`org_admission.rs`). Now
  LIVE-WIRED through `serve_rpc_protected` + the unary bridge (`#47`).
- **Closure series** (all review-driven, red-witnessed, per-item commits):
  KC1–KC10, NC1–NC5, the AV series (8 commits), R2-1..R2-7 (5 commits),
  R3-1..R3-4 (4 commits, `e6b7925a5..fda5e2ed0`).

**Gate ledger:**

| Gate | Scope | State |
|---|---|---|
| Gate 1 | OA-1 revocation store | **SIGNED OFF** (Kyra) |
| Gate 2 | E1 primitives audit | **SIGNED OFF** (Kyra) |
| Gate 3 | E0 / nRPC substrate | **SIGNED OFF** (Kyra) |
| `#47` | E1 **live wiring** + E2 caller seam (serve_rpc_protected, live gate, `RpcContext.org_admission`, `RpcStatus 0x0009`, proof-intent builder) | **SIGNED OFF / CLOSED** (Kyra, 2026-07-19, `512cd1588`) |

**Landed since:** OA2-F (CLI/SDK grant management + §2.6 exit-gate closure
witnesses), 2026-07-19.

**Not started:** OA-3 (grant-scoped private discovery / visibility state
machine) → OA-4 (end-to-end seam witnesses).

## The core problem (why this is not one mechanical commit)

The current nRPC dispatch does **not** have one atomic, collision-safe,
**authenticated** mapping from `caller + service + policy → handler`
(v1 authenticates the caller at the DIRECT session — end-to-end
identity through a relay is explicitly deferred, §E0.3). OA-2 admission
must **make that mapping load-bearing**, not assume it exists. Every
correction below is an instance of that:

- two mutable truths (channel→handler, name→policy) can disagree;
- a wire `u16` bucket fans frames to every dispatcher in it;
- control frames (CANCEL/CHUNK/GRANT) carry no service identity;
- `from_node` is the AEAD last-hop peer, not the end-to-end caller;
- installed authority at registration ≠ usable authority at call time;
- a floor can rise between proof verification and dispatch.

Admission is only sound once that one seam is singular and fail-closed.

## Revised sequence (supersedes "one commit")

| Slice | Content | Enables OA-2? |
|---|---|---|
| **OA2-E0** | nRPC substrate: non-destructive registration + generation tokens, channel/service equality, registration-bound call state, caller-identity model. Reviewable WITHOUT enabling admission. | no |
| **OA2-E1** | Unary provider admission: captured immutable policy, live self-verify, stable authority stamp + replay ordering, single clock sample, per-caller replay, policy + attribution wiring. | yes (provider side) |
| **OA2-E2** | Caller + wire: proof-intent builder in `call()`, `RpcStatus 0x0009`, coarse external reasons + detailed audit, mixed-version fleet gate. | yes (caller side) |
| **OA-3** | Visibility: `OwnerScoped` / `GrantedAudience`, encrypted `ScopedCapabilityAnnouncement`, audience-partitioned discovery, baseline sanitization. | discovery |

Visibility is **removed from the admission commit** (verdict §11): E1
registration LOUDLY REJECTS non-`Public` visibility until OA-3, so the
authority commit does not also span the announcement state machine.

---

## OA2-E0 — nRPC substrate prerequisites (OA-2-neutral)

These are correctness repairs to the dispatch primitive. They enable
nothing on their own and are reviewable independently.

### E0.1 Non-destructive registration (addendum §1)

`register_rpc_inbound` today does
`Some(std::mem::replace(existing_disp, dispatcher))` — the replacement
has ALREADY happened by the time the caller reads `Some` as
`AlreadyServing`, so a duplicate `serve_rpc` silently breaks the old
registration and installs a dispatcher with no live bridge consumer.

Repair:

- vacant-only insertion; an existing canonical channel returns
  "occupied" WITHOUT mutating;
- each registration carries a `registration_id` (monotonic generation)
  + a token;
- teardown is conditional: a `ServeHandle` removes ONLY its own
  `registration_id` (a stale handle cannot evict a newer registration);
- registration failure rolls back leaving no dispatcher, tag, or
  policy.

Useful nRPC hardening independent of OA-2; **prerequisite** because OA-2
policy cannot be layered on a destructive primitive.

### E0.2 Channel/service equality + a wire discriminator on control frames (verdict §3, addendum §2)

The wire carries a `u16` channel bucket; a collision fans a frame into
every canonical dispatcher in that bucket. Initial REQUEST frames
carry `payload.service`; **CANCEL / REQUEST_CHUNK / STREAM_GRANT do
not**.

- Every REQUEST dispatch requires `payload.service == captured
  registration.service`; a non-matching dispatcher **silently
  discards** (never emits a competing response). This closes INITIAL
  request service-confusion.

Local state keyed by `(authenticated caller, call_id, registration_id)`
does NOT by itself close colliding-bucket CONTROL frames: a CANCEL
carries no `registration_id`, so when fanned into two dispatchers each
supplies its OWN captured `registration_id` and both can match if the
caller reused the `call_id`. The discriminator MUST live on the WIRE.

**Requirement (MUST — the one open E0 decision):** every control frame
(CANCEL / REQUEST_CHUNK / STREAM_GRANT) MUST carry an unambiguous
discriminator selecting exactly one dispatcher. One of:

- **(A, recommended)** the exact canonical channel identity — the
  `u32` canonical `ChannelHash` the dispatcher is already keyed on
  within the bucket — added to every control frame, so it routes to
  exactly one dispatcher deterministically (a natural extension of the
  existing "canonical hash is what each dispatcher is keyed on"
  bucket-collision handling); or
- **(B)** a collision-free per-call token minted by the initial
  REQUEST and echoed by CANCEL / CHUNK / GRANT, matched against the
  registration-bound active-call state.

**The exact wire shape is selected at E0 review.** Recommendation: (A)
— it reuses the canonical-hash dispatch keying already present and
needs no per-call token distribution to the caller. Either way, a
control frame reaches a fold ONLY when its authenticated origin + call
identity + on-wire discriminator resolve to exactly one registration;
two colliding services sharing a caller/call_id can never cross-mutate.
Protected calls, being unary, still see CANCEL — this requirement is
what makes CANCEL registration-exact.

### E0.3 Caller-identity model — DECISION: direct-session-only in v1 (verdict §4, addendum caller-binding)

The live code defines `from_node` = AEAD-authenticated **last-hop
session peer**; `origin_hash` = routing metadata, **not** an
authenticated identity. For a relayed RPC, `from_node` is the RELAY,
not the caller. Relay identity and caller identity must not collapse.

**Chosen cut for v1 (smaller, fail-closed):** org-protected RPC is
**direct-session-only**.

```text
let caller = peer_entity_id(inbound.from_node)
    .ok_or(CallerIdentityUnavailable)?;
require(meta.origin_hash == caller.origin_hash());   // bound, not trusted alone
// A relayed protected request (from_node != proof subject's session)
// is LOUDLY DENIED — never inferred through a relay.
```

Rationale: end-to-end authenticated caller identity (carry an
authenticated origin entity independent of the relay, keep
`transport_relay: NodeId` separate for attribution) is strictly more
surface. Direct-only is the minimal sound seam; the end-to-end path is
a later E2+ evolution, not a v1 requirement. Registration of a
protected service records that protected calls require a direct
authenticated session to the proof subject.

Witnesses: direct-session admit; relayed protected request denied
(`CallerIdentityUnavailable` / relay-mismatch); forged `origin_hash`
vs authenticated session entity denied.

### E0.4 One clock sample — helper here, RULE applies in E1 (addendum §3)

Conceptually this belongs to E1 admission; the reusable helper
(a `struct` capturing `wall_now` + `monotonic_now` together) is
harmless groundwork to land in E0. The RULE is enforced in E1.4/E1.5:
capture `wall_now` and `monotonic_now` ONCE per admission; all
certificate/grant/proof freshness uses `wall_now`; the replay deadline
is derived from the SAME `wall_now` relative to `monotonic_now`, so a
wall-clock jump between a freshness check and replay-retention
derivation cannot immediately expire a just-fresh proof. Deterministic
clock-step witness lands with E1.

---

## OA2-E1 — unary provider admission

### E1.1 Registration owns the policy (verdict §1, §2, §7, §12)

Replace `LocalServiceRegistry`'s name-only set with a
generation-owned registration whose policy is **captured by the
handler bridge**, so there is ONE truth, not a name→policy side map:

```rust
type OrgProviderPolicy = Arc<dyn Fn(&OrgCallProof) -> bool + Send + Sync>;

struct RegisteredRpcService {
    registration_id: u64,           // generation (E0.1)
    service:  Arc<str>,
    visibility: CapabilityVisibility, // E1: MUST be Public (else refuse)
    admission:  OrgAdmission,
    provider_policy: OrgProviderPolicy,
}
```

- Legacy `serve_rpc` constructs `Public + PublicAuthenticated` with a
  trivial `|_| true` policy — so **every live handler has a policy**.
- Protected registration constructs an immutable
  `Arc<RegisteredRpcService>`; the dedicated bridge captures that EXACT
  `Arc`. No per-request name→policy lookup; **no unknown-policy
  fallback** (a live handler with no policy = inconsistent state =
  LOUD DENY, never `may_execute` fallback — verdict §2).
- The cold registration is ONE serialized transaction (a cold-path
  mutex, not lock-free cross-map coordination): validate → capture
  immutable policy with the handler → establish exact local capability
  → install dispatcher → (only later) schedule announcement.
- No hot `Public→Protected` switch in v1 — require teardown/re-register
  or restart, with the old bridge DRAINED before the protected mode is
  claimed active (verdict §12).
- Protected registration REQUIRES an installed authority AND
  `visibility == Public` (OA-3 defers the rest); either failing is a
  loud refusal that rolls back cleanly.

### E1.2 The gate (verdict §2, §3; the load-bearing seam)

At the REQUEST dispatch, on the captured `Arc<RegisteredRpcService>`:

```text
require(payload.service == reg.service)          // E0.2 — authority not payload-selected
tag = nrpc_tag_for(reg.service)                  // from the CAPTURED registration
cap = CapabilityAuthorityId::for_tag(tag)

match reg.admission {
  PublicAuthenticated =>
      require( may_execute(fold, self_id, tag, caller_node) )   // UNCHANGED
  OwnerDelegated | CrossOrgGranted => {
      require( has_local_capability(fold, self_id, tag) )       // no allow-list union
      let admitted = admit_protected(reg, cap, caller, ctx)?;   // E1.3–E1.5
  }
}
```

`may_execute` is consulted ONLY on the public arm and stays byte-for-
byte identical. The capability id derives from the CAPTURED
registration, never from attacker-selected `payload.service`.

### E1.3 Live provider self-verification (verdict §5)

Registration-time authority ≠ usable authority at call time. For every
protected admission, as a call-time prerequisite:

```text
authority = installed NodeAuthority        else ProviderAuthorityUnavailable
store     = installed OrgRevocationStore    else ProviderAuthorityUnavailable
require( !store.is_poisoned() )
authority.config.self_verify(provider_entity, store.snapshot())  // expiry + floor + binding
```

Any failure → `ProviderAuthorityUnavailable`; the handler stays dark.
An expired/revoked/poisoned node cannot keep admitting as org B.

### E1.4 Stable security view + replay ordering (verdict §6, addendum §3)

Reading one floor snapshot is insufficient — a floor can rise between
verification and dispatch. Give admission a linearization point using a
stamp analogous to (but distinct from) the OA-1 send seqlock:

```text
AdmissionStamp { authority_ptr, store_ptr, store_generation, poisoned }
```

Order INSIDE `verify_org_admission` (a narrow §9.5 hook between call
binding and replay insertion):

```text
1..9   existing verification (against the captured snapshot + wall_now)
9.5    provider security view still current?
         recompute AdmissionStamp; if != captured OR poisoned:
             retry from a fresh view, or deny AuthorityChanged
10     replay insert (atomic)          — AFTER 9.5, so a stale attempt
                                          never consumes (caller,call_id)
11     provider policy (captured Arc)  — runs LAST
```

A floor raise AFTER 9.5 is ordered after this admission; a raise BEFORE
it forces retry/deny. The stability check precedes replay insertion so
a stale attempt cannot burn a `(caller, call_id)` slot. Deterministic
witness: pause after binding verification, raise caller/provider floor
(and separately: replace authority, poison store), resume → no replay
record under the stale view, handler never runs.

### E1.5 Replay fairness (verdict §10)

`AdmissionReplayConfig` gains `max_entries_per_caller` beside the
global `max_entries`. At per-caller capacity, deny ONLY that caller;
the global ceiling stays fail-closed. This matters because policy runs
AFTER replay insertion, so even policy-vetoed valid proofs consume a
slot — a single credentialed caller must not be able to starve every
other org. No dedicated eviction task (lazy reclamation in `admit`
suffices; add one only if measured).

### E1.6 Policy + attribution wiring (verdict §7)

- The captured `provider_policy` runs as step 11 — real destination,
  not `|_| true`.
- `RpcContext` gains `org_admission: Option<Admitted>`. Protected calls
  place the four-party `Admitted` there; the raw `net-org-admission`
  header is STRIPPED from the headers handed to application code (the
  app receives verified attribution, never raw credential material).
  Public calls receive `None` and keep existing header behavior.

### E1.7 Canonical request digest via the existing codec (verdict §8)

No second hand-written concatenation codec. One shared helper:

```text
digest(req) =
  clone request view
  → remove EVERY exact net-org-admission header
  → byte-sort remaining (name,value) pairs
  → encode with RpcRequestPayload's existing canonical wire encoder
  → blake3::derive_key("net-org-rpc-request-v1", encoded_bytes)
```

This binds payload version, service, deadline, flags, header
count/lengths, duplicate-header multiplicity, and body length+bytes
automatically. The SAME function is used by the E1 wire witnesses and
the OA2-E2 caller helper — divergence would fail every legitimate call
closed (safe, caught by the admit witness).

### E1.8 Unary-only boundary (verdict §9)

- Existing server-streaming / client-streaming / duplex serving APIs
  remain explicitly `PublicAuthenticated`; no protected policy can be
  installed through a streaming/duplex registration path.
- Protected registration is accepted ONLY by the unary registered API.
- Any streaming flag on a protected unary REQUEST → `StreamingUnsupported`.
- CANCEL for an admitted unary call reaches the fold only when its
  authenticated origin + call identity + registration match (E0.2).

---

## OA2-E2 — caller + wire surfacing

### E2.1 Proof-intent builder inside `call()` (addendum §4)

`CallOptions.request_headers` is supplied BEFORE `call()` mints the
final `call_id`, yet the proof binds `call_id`, final deadline, final
flags, generated headers, and body — so an ordinary caller cannot
prebuild the admission header. E2 adds a minimal proof-intent seam:
`call()` mints the `call_id`, finalizes the request, computes the
shared digest (E1.7), signs the proof, and appends the
`net-org-admission` header. This is NOT the full grant-management CLI
(OA2-F) — it is the minimum caller seam to exercise the protocol
honestly. Until it lands, E1 tests are explicitly low-level
injected-frame witnesses and the feature is unreleasable.

### E2.2 Status + reasons (verdict §8 mapping)

- `RpcStatus::AdmissionDenied = 0x0009` (+ `to_wire`/`from_wire` +
  reserved-range round-trip).
- COARSE, stable external reason on the wire (a small enum: e.g.
  `denied` / `not-supported` / `unavailable`) — the DETAILED
  `AdmissionDenied` variant stays provider-side audit only, so denial
  reasons don't become an oracle and don't leak credential state.
- Retry/breaker/binding behavior specified for callers.

### E2.3 Mixed-version fleet gate (addendum §5)

"New caller sends a proof to an old provider" is NOT an upgrade
mechanism — the old provider ignores the header and applies legacy
`may_execute` (permissive). Before a service is advertised as
protected: all serving replicas upgraded → old public advertisements
withdrawn/expired → every replica registered protected → only THEN
enable discovery/traffic. A mixed-version witness must prove the
OLD-PROVIDER case, not only that old clients decode 0x0009.

---

## OA-3 — visibility (out of the admission commit)

`OwnerScoped` semantics, `GrantedAudience` encryption, audience-
partitioned discovery, baseline sanitization, and the full send-path
leak matrix (immediate / explicit-baseline / change-driven / keepalive
/ rate-limited flush / late-join push / unregister / restart). E1
registration rejects non-`Public` visibility until this lands.
`OwnerScoped` wording corrected: a reserved plaintext scoped tag
excluded from generic lookup is **query-projection control**, not
fanout control, unless recipients and forwarded propagation are
actually restricted.

---

## Witness matrix (E0–E2)

Substrate (E0): 1 non-destructive duplicate registration returns
occupied without mutating; 2 stale handle cannot evict a newer
registration; 3 failed/duplicate registration leaves no tag/policy;
4 channel A + payload B service-confusion → deny, neither handler runs
(both directions); 5 colliding-bucket control frame bound to
`(caller, call_id, registration_id)`; 6 direct-session caller binding;
7 relayed protected request denied; 8 forged `origin_hash` vs session
entity denied; 9 clock-step: freshness+retention from one sample.

Admission (E1): 10 cross-org admit end-to-end with four-party
attribution; 11 owner-delegated admit; 12 deny classes → 0x0009 with
coarse reason — a REPRESENTATIVE matrix (missing/dup header, malformed,
member-binding, foreign issuer, target-not-covered, insufficient
rights, revoked floor, transplanted binding, replay, call-id collision,
expired, policy veto) PLUS an exhaustive enum-driven test asserting
EVERY `AdmissionDenied` variant maps to `0x0009` with a defined coarse
reason (so a newly added variant cannot silently escape the mapping);
13 missing registration policy NEVER falls back — loud deny;
14 provider owner cert expires after registration → `ProviderAuthority
Unavailable`; 15 provider floor rises after registration → deny;
16 floor raise DURING verification (paused) → no replay record, handler
never runs; 17 authority replaced mid-admission → retry/deny; 18 active
store poisoned mid-admission → deny; 19 provider-policy result reaches
the live gate; 20 `Admitted` reaches the handler AND the raw proof
header does not; 21 one caller cannot consume another caller's replay
allocation; 22 protected streaming/client-streaming/duplex cannot
bypass; 23 concurrent duplicate proof invokes the inner handler EXACTLY
once; 24 Public unchanged (with and without an installed authority);
25 authority-dark node (no protected registrations) == pre-OA-2.

Caller/wire (E2): 26 `call()` builds a valid proof end-to-end;
27 mixed-version: NEW caller → OLD provider does not become protected
(old provider applies legacy gate); 28 coarse external reason is not a
credential oracle.

---

## Boundary / risk register

- `may_execute` byte-for-byte identical (re-verify after E1).
- **No fail-open path**: no unknown-policy fallback; org-protected
  without authority / with a bad or missing proof / with an unhealthy
  authority all DENY; a handler never runs for an unverified call.
- **One seam**: `caller + service + policy → handler` is atomic,
  collision-safe (E0.1/E0.2), and direct-session-authenticated in v1
  (E0.3; relayed end-to-end identity is explicitly deferred).
- **Replay before handler; stability before replay** (E1.4) — a
  handler never runs twice for one `(caller, call_id)`, and a stale
  view never consumes a slot.
- **One clock sample** drives freshness + retention (E0.4).
- Digest via the existing RPC codec, shared caller/provider (E1.7).
- Admission reads review-11-correct accessors (installed store
  snapshot, persisted skew); it does not touch the `StoreCore` /
  `PublishGuard` / send-seqlock surfaces or `may_execute`.
- Visibility deferred to OA-3; E1 registration rejects non-`Public`.

---

## Execution checklist

**OA2-E0 gate (independent — OA-2-neutral):**

- [x] This revised design accepted.
- [x] The E0.2 control-frame discriminator wire shape chosen — A,
      canonical `u64` channel identity (RpcRouteV1), ratified at E0 review.
- [x] → E0 AUTHORIZED to proceed (does not wait on OA-1).

**OA2-E0 work (nRPC hardening, enables no org authority):**

- [x] Non-destructive vacant-only registration + generation tokens +
      conditional teardown (E0.1).
- [x] Channel/service equality on REQUEST + the chosen control-frame
      discriminator + registration-bound call state (E0.2). Call/control
      state re-bound to the authenticated `from_node` (AV-1).
- [x] Direct-session caller-identity binding; relayed protected denied
      (E0.3).
- [x] Single wall+monotonic clock-sample helper (E0.4).
- [x] Witnesses 1–9; gates (fmt, both clippy, full suites). Plus the
      E0-Fix1/2/3 closures and the R3-1 session-scoped REQUEST_GRANT
      closure.
- [x] Gate 3 (E0/nRPC) RE-SIGNED after R3-1 — **SIGNED OFF (Kyra)**.

**OA2-E1 / E2 gate:**

- [x] Gate 1: OA-1 revocation store re-signed — closures R3-2/3/4 landed —
      **SIGNED OFF (Kyra)**.
- [x] Gate 2: OA2-A–E-partial primitives audited — **SIGNED OFF**.
- [x] Gate 3: OA2-E0 landed and reviewed — R3-1 landed — **SIGNED OFF (Kyra)**.

**OA2-E1 primitives (landed as primitives, since live-wired by `#47`):**

- [x] Registration-owned policy (`RegisteredRpcService`, E1.1); live
      self-verify (`verify_provider_authority`/`ProviderFacts`, E1.3);
      admission stamp + §9.5 ordering (E1.4); per-caller replay (E1.5);
      shared canonical digest (E1.7). All red-witnessed, `may_execute`
      untouched.

**OA2-E1 live wiring + E2 (the atomic `#47` unit — SIGNED OFF / CLOSED
by Kyra 2026-07-19, `512cd1588`):**

- [x] OA2-E1 wiring: the E1.2 gate in the unary serve bridge; `Admitted`
      into `RpcContext` + proof-header strip (E1.6); unary-only boundary
      rejection (E1.8); witnesses 10–25 (incl. the exhaustive
      enum→`0x0009` test). Plus B1 direct-session identity, B2 node-owned
      replay, B3 atomic caller construction, and the protected `call_service`
      authority split.
- [x] OA2-E2: proof-intent builder; `RpcStatus 0x0009` + coarse
      reasons; mixed-version fleet gate; witnesses 26–28. Plus the shared 30 s
      TTL (fail-loud), exact provider binding, SDK re-export, and the live
      two-node transport/provider-state/mixed-version witnesses
      (`tests/integration_nrpc_protected.rs`).
- [x] Gates: `may_execute` byte-identity, fmt, both clippy, full lib +
      org_ownership + CLI + the `org_admission_wire` integration file.

**Later:**

- [ ] OA-3 (separate): visibility state machine.
- [x] OA2-F: CLI/SDK grant management + §2.6 gate — LANDED 2026-07-19
      (`net_sdk::org` grant re-exports; `net org grant-dispatcher` /
      `grant-capability` incl. `--discover`; the two §2.6 closure witnesses +
      `tests/org_admission_wire.rs`).
```
