# Org Capability SDK Plan (OSDK)

**Version:** v0.2 — applies Kyra's simplification ruling (2026-07-21):
"the current plan is still designing an SDK system. What we need is an
SDK **verb layer** over the authority system that already exists."
v0.1's five-module design (admin / client / provider / discovery /
types, wallet, policy builders, candidate ontology, options objects)
is superseded; every removed surface is recorded in §Deferred with the
ruling's rationale. Companion to
[`ORG_CAPABILITY_AUTH_PLAN.md`](ORG_CAPABILITY_AUTH_PLAN.md)
(the substrate — OA-1..OA-4 all CLOSED) and
[`OA2E_INTEGRATION_DESIGN.md`](OA2E_INTEGRATION_DESIGN.md) (the live
caller/provider seams the verbs sit on).

**Status:** design only; no code authorized. Activation gate: review
sign-off. Prerequisites (all met): OA-1..OA-4 closed, `#47` live
wiring signed off, OA2-F grant management landed.

**The measured gap (OA-4 STOP-gate answer, corrected per the
ruling).** The gap is NOT "applications need to inspect
private-discovery records." The gap is: **applications cannot
privately discover and invoke a protected service.** Today the only
readers of OA-3's verified discovery store are two `#[doc(hidden)]`
test seams; the caller seam requires hand-assembling all nine
`OrgProofIntent` fields; the SDK has no protected serve; and the
coarse denial byte E2.2 put on the wire is decoded by nothing on the
caller side. The verbs below close exactly that, and nothing else.

---

## The surface

The everyday surface answers three questions only:

1. What credentials am I using?
2. What protected capability am I calling?
3. Who is allowed to call the capability I am serving?

```rust
// caller
let org = mesh.org(credentials)?;
let customer: Customer = org.call("customer.read", &request).await?;

// provider — cross-org, encrypted grant-audience discovery
mesh.serve_org("customer.read", OrgAccess::Granted,
    |caller, request| async move { read_customer(caller, request).await })?;

// provider — same-organization, encrypted owner-audience discovery
mesh.serve_org("internal.reindex", OrgAccess::SameOrg,
    |caller, request| async move { reindex(caller, request).await })?;
```

That is almost the entire common SDK. Everything else must earn its
way in through a named consumer. The low-level canonical APIs
(`OrgProofIntent`, `serve_rpc_protected` / `serve_rpc_owner_scoped` /
`serve_rpc_granted`, the raw envelope codecs) remain available and
unchanged — reducing the facade does not reduce substrate power.

**The design test (acceptance criterion, not aspiration).** A user
must be able to make the secure common path work without knowing
these names: `OrgProofIntent`, `OwnerDelegated`, `CrossOrgGranted`,
the `OrgAudienceSecret` commitment, `ScopedCapabilityAnnouncement`,
`VerifiedScopedCapability`, `CoarseAdmissionReason` (the *name*; the
decoded value appears inside `OrgError`), `GrantTargetScope`. They
may meet them when debugging or using the advanced API. S3's example
witness enforces this: the composed example imports none of them.

---

## Grounded reality (what the verbs sit on)

As of `org-capability-auth` HEAD, 2026-07-21; line numbers are
snapshot references.

- `call()` already does the hard caller work: mints the call id,
  computes the canonical digest, signs via
  `OrgCallProof::sign_for_call`, appends exactly one
  `net-org-admission` header, and pins
  `peer_entity_id(target) == intent.provider`
  (`mesh_rpc.rs:4984–5041`, `sign_admission_proof` `:5525`).
  `org.call` supplies what is missing: credential assembly, exact
  grant matching, mode inference, internal verified discovery,
  provider selection, and denial decoding.
- The provider gate already delivers verified facts: protected
  handlers receive `RpcContext.org_admission: Option<Admitted>`
  (five fields), with the raw proof header stripped. `serve_org`
  supplies the typed wrapper that today discards `ctx`
  (`TypedRpcHandler::call`, `sdk/src/mesh_rpc.rs:833`) and therefore
  loses the facts.
- Core serve entry points already exist per (admission × visibility)
  pair: `serve_rpc_protected(admission)` (public discovery),
  `serve_rpc_owner_scoped`, `serve_rpc_granted`
  (`mesh_rpc.rs:3072/3139/3170`). `serve_org` maps onto them; it
  invents no registration path.
- The verified private-discovery queries exist and are expiry- and
  floor-safe (`ScopedDiscoveryStore::find_capabilities_for_grant` /
  `find_owner_private_capabilities`, `org_scoped_store.rs:202/231`)
  but are reachable only through `_for_test` seams
  (`mesh.rs:8900/8931`) — the one real core promotion this plan
  needs.
- Remote denial reaches the caller as
  `ServerError { status: 0x0009, body: [coarse_byte] }`
  (`emit_admission_denial`, `mesh_rpc.rs:856`);
  `CoarseAdmissionReason::from_wire` is currently unused off the
  provider. The facade finally consumes it.

---

## The five public concepts

### 1. `OrgCredentials`

```rust
pub struct OrgCredentials {
    membership: OrgMembershipCert,
    dispatcher: OrgDispatcherGrant,
    grants: Vec<OrgCapabilityGrant>,
    audience_secrets: Vec<OrgAudienceSecret>,
}

let credentials = OrgCredentials::new(membership, dispatcher, grants, secrets)?;
let org = mesh.org(credentials)?;
```

- **No keypair.** The mesh's existing identity signs.
  `mesh.org(credentials)` binds to the node identity and refuses if
  `membership.member != mesh.entity_id()` — the TOFU member binding
  would fail remotely anyway; fail here instead.
- **Construction checks structural relationships and signatures;
  calls check temporal validity** (ruling). `new` verifies: each
  credential's signature against its org id;
  `membership.org_id == dispatcher.org_id` (acting-org agreement);
  every grant names `grantee_org == membership.org_id`; no reserved
  zero grant id; every audience secret satisfies
  `matches_grant(&grant)` against exactly one held grant (the
  §2.6-witnessed commitment relation — a wrong or stale secret never
  sits silently in the set); no duplicate grant ids. Windows are NOT
  checked here — an actor may be assembled long before use; expiry
  is a call-time refusal.
- **No public `OrgActor`, no public `OrgCredentialStore`, no mutable
  install API.** The collection is closed at construction. Changing
  credentials = construct a new `OrgCredentials`, bind again.
- **Binding has one internal side effect** (not a public mutable
  API): it idempotently installs the credential's (grant, secret)
  pairs into the node's consumer-audience ingest registry
  (`MeshNode::install_consumer_grant_audience`) — without this the
  scoped store never accumulates records for those grants and
  private discovery is structurally empty. Install failures are loud
  (`OrgError::Credentials`). Registrations persist for the node's
  lifetime (no uninstall-on-drop in v1 — dropping a client must not
  blind a second client bound with the same grant); pinned at S0
  review (Q5).
- The struct derives neither `Serialize` nor `Deserialize`
  (inherited structurally from `OrgAudienceSecret`; asserted
  type-level for the container too) and its `Debug` is redacted.

### 2. `OrgClient` — one common method

```rust
let org = mesh.org(credentials)?;
org.call("customer.read", &request).await
```

Internally, in order:

```
derive capability            CapabilityAuthorityId::for_tag("nrpc:<svc>")
→ verified discovery         internal; granted + owner planes via the
                             scoped store, public plane via the fold with
                             ingest-verified ownership (owner_org_for);
                             a public candidate with no verified owner
                             projection is never eligible
→ mode classification        candidate owner org == acting org → SameOrg
                             (no grant attached); else Granted
→ exact credential matching  the complete authority relation: grantee
                             org, issuer org, capability, INVOKE,
                             target_scope.covers(provider, owner),
                             window valid now — the provider's own
                             predicates, never a reimplementation
→ deterministic selection    eligible = classified + grant-matched +
                             direct-session-reachable (E0.3); tie-break
                             documented and stable (private provenance
                             before public, then lowest provider id);
                             zero eligible → typed discovery error
→ canonical OrgProofIntent   all nine fields; placed on CallOptions
→ exact-target protected call  core call() pins, mints, digests, signs,
                             sends one request — unchanged
→ coarse denial decoding     0x0009 body → CoarseAdmissionReason →
                             OrgError::AdmissionDenied
```

Ruling-locked exclusions: **no** public exact-provider call, **no**
provider selector, **no** `use_grant`, **no** discovery builder,
**no** options object. Overlapping valid grants for the selected
provider produce the typed ambiguity error — the operator removes
the ambiguity or drops to the low-level seam.

**The SDK owns** (not the caller): proof TTL (the shared 30 s,
`MAX_ORG_PROOF_TTL_SECS`); grant matching; provider selection;
provider pinning; retry prohibition (a signed proof is never resent;
the SDK performs no automatic retry of any kind — every `org.call`
is one fresh call id and one fresh signature; idempotency across
calls is the application's, per the volatile replay-guard contract);
codec default (one default, both sides — Q1); timeout default
(inherited core default). `call_with` arrives only when a real
consumer needs one specific option.

### 3. `OrgAccess`

```rust
pub enum OrgAccess { SameOrg, Granted }
```

Human-facing names in the common API, mapping directly onto the
canonical admission modes — `SameOrg → OwnerDelegated`,
`Granted → CrossOrgGranted` — which retain their canonical names in
`org::types`. There is no third variant: `PublicAuthenticated`
services are not org-protected and keep `serve_rpc`.

### 4. `OrgCaller`

```rust
pub struct OrgCaller {
    entity: EntityId,
    acting_org: OrgId,
    provider_org: OrgId,
    capability: CapabilityAuthorityId,
    grant_id: Option<[u8; 32]>,   // Some ⇔ Granted admission
}
```

A projection of canonical `Admitted` — not a new authority object.
The handler receives `(OrgCaller, Req)` and returns
`Result<Resp, _>`; the wrapper decodes/encodes with the default
codec. **No `RpcContext` exposure in the common handler** —
applications needing headers, packet metadata, or proof-level policy
use the existing low-level protected serve API.

Honest grounding note: `Admitted` today carries five fields and
**no `grant_id`** (`org_admission.rs:303`); the grant id lives in
the verified proof, which the gate does not forward. `OrgCaller.grant_id`
therefore needs a small additive core extension (§Core touches, item
2) — provider-side only, never on the wire, and independently useful:
it completes the §D1 audit story (a handler can log/deny by the
exact grant its caller invoked under, which §D1 names as the
existing-but-unbuilt lever).

If `org_admission` is absent inside a `serve_org` handler, that is
an invariant violation (the gate dispatches only admitted protected
calls): the wrapper returns an internal error loudly, never panics,
and S2 witnesses the path unreachable through the real gate.

### 5. `OrgError`

```rust
pub enum OrgError {
    Credentials(OrgCredentialError),   // mismatch, expired, missing grant,
                                       // ambiguous grant, audience mismatch
    Discovery(OrgDiscoveryError),      // no authorized provider (with the
                                       // considered count), provider not direct
    AdmissionDenied(CoarseAdmissionReason),
    Rpc(RpcError),
}
```

Four meaningful domains to branch on — not a 20-variant flattened
implementation map. The nested enums carry the detail (all local, so
they leak nothing); the remote reason stays the coarse 3-bucket wire
value by design (denial reasons must not become a credential oracle
— E2.2). An undecodable 0x0009 body maps to
`AdmissionDenied(Denied)`, never an error-about-an-error.

Naming collision, decided at S0 (Q2): `net_sdk::org` already
re-exports the canonical issuance error as `OrgError`
(`behavior::org::OrgError`). Recommendation: the facade error owns
the short name at the `org` root; the issuance error is additionally
aliased (`pub use ... as OrgIssuanceError`) and its existing path
under `org::types` remains valid — no break, one obvious name for
the common path.

---

## Secure defaults — access implies visibility

`serve_org` collapses admission and visibility into the secure
common interpretation. Protected services are **private by
default**:

```
serve_org(s, OrgAccess::Granted, h)  → CrossOrgGranted admission
                                       + GrantedAudience encrypted discovery
                                       (core serve_rpc_granted)
serve_org(s, OrgAccess::SameOrg, h)  → OwnerDelegated admission
                                       + OwnerScoped encrypted discovery
                                       (core serve_rpc_owner_scoped)
```

No common caller ever sees an admission × visibility matrix.
Protected-but-publicly-discoverable registration remains available
through the existing low-level core API
(`serve_rpc_protected`); if repeated consumers demand it, an
explicit `serve_org_public(...)` is added then — not before
(§Deferred).

Provider policy in v1 **is the handler**: `serve_org` installs the
trivial `|_| true` proof policy, and application-level decisions are
made in the handler body with `OrgCaller` in hand (including
`grant_id`-based refusal). The step-11 proof-policy hook stays fully
available on the low-level serve APIs — the §D1 provider-local
grant-denylist remains expressible there today; the facade sugar for
it is deferred.

Provider-side audience *provisioning* (installing B's (grant,
secret) pairs so a node can seal granted envelopes —
`install_provider_grant_audience`) is operational state, like
adoption and revocation files: it stays at the core/operational
layer in v1 and is not part of the verb facade (Q3).

---

## Discovery is internal

`org.call(...)` consumes verified discovery internally. No public
candidate ontology, no `VerifiedProviderCandidate`, no
`find_service` builder — the implementation needs a candidate model;
the public surface does not. All discovery inputs are
ingest-verified state: the scoped store's floor/expiry-safe queries
(the full envelope chain — outer signature, owner cert, audience
selection, AEAD, descriptor↔grant binding — already ran at
`verify_scoped_ingest`), and the plaintext fold with the
ingest-verified owner-cert projection for public candidates.
Knowledge never implies invocation authority; the wallet-relation
match and the provider's admission remain the only authority steps.

If a named consumer later needs to enumerate providers, compare
provenance, inspect expiry, or rank manually, add
`org.discover("customer.read")` shaped by that consumer's actual
requirements (§Deferred).

---

## Credential loading is separable

`OrgCredentials::new` takes in-memory canonical types. File
formats, permissions, DACL checks, CLI envelopes, and secret
publication stay where they are (CLI + config codecs) — filesystem
credential handling has demonstrated review gravity and must not
block the call/serve facade. An optional
`org_files::load_credentials(path)` helper may come later
(§Deferred), reusing the CLI's envelope codecs rather than inventing
new ones.

---

## Core-touch inventory (exhaustive)

Everything is SDK-crate code except:

1. **Promote the two scoped-store queries** out of their
  `#[doc(hidden)] *_for_test` names into production `MeshNode`
  methods (owned snapshot returns, no lock-holding borrows). They
  are consumed internally by `org.call`; nothing new is re-exported
  publicly from the SDK. Test seams become aliases or are retired —
  test-named seams must not be load-bearing production API.
2. **`Admitted.grant_id: Option<[u8; 32]>`** (or an equivalent
  additive surface on the admission result): populated by
  `verify_org_admission` from the verified capability grant;
  provider-side only; never a wire object change. Required for
  `OrgCaller.grant_id`; independently closes the §D1 attribution
  gap at the handler.
3. **Node-identity access for intent minting.** `OrgProofIntent.caller`
  is `Arc<EntityKeypair>`; the facade signs with the node's own
  identity. Confirm at S1 whether the SDK can already reach it, else
  add one narrow accessor (or an intent-minting helper on
  `MeshNode`) — decided at S1 review, recorded here either way.
4. **Ownership accessor reachability** — `owner_org_for`
  (`capability_bridge.rs:476`) needs to be publicly reachable from
  the SDK crate for the public-plane candidate check; confirm or add
  a thin `MeshNode` accessor at S1.
5. **Re-exports** in `org::types` (`Admitted`, `OrgAdmission`,
  `CoarseAdmissionReason`, `OrgProofIntent` alias, the
  `OrgIssuanceError` alias per Q2). Pure re-export.

Explicitly untouched: every `verify_org_admission` step and its
order, the serve gates and `UnaryAdmission`, all wire objects,
headers, and status codes, the replay guard, the RED seam,
`OrgProofIntent` itself, and `may_execute` (byte-for-byte
re-verified at the exit gate).

---

## Slices (four bounded commits, stop-and-review per slice)

**S0 — credentials and errors.** `OrgCredentials` + structural
validation, mesh-identity binding (including the internal audience
install), `OrgError` hierarchy, `org::types` re-exports + naming
decision (Q2). Witnesses: structural-validation matrix (each broken
relationship → its typed `OrgCredentialError`); binding refusal on
`membership.member != mesh.entity_id()`; audience-mismatch refusal
(reuses the §2.6 commitment relation); container
non-serializability + redacted `Debug` (type-level); existing flat
`net_sdk::org::*` paths still resolve. No core diff.

**S1 — `org.call`.** Internal verified discovery (core promotions,
items 1/3/4), mode classification, exact grant matching, selection,
intent, coarse decode. Witnesses:
- intent-equality: for both modes, the facade-built `OrgProofIntent`
  equals a hand-assembled reference field-for-field given the same
  inputs;
- **one live SameOrg witness and one live Granted witness** — real
  two-node `MeshNode::call` traversal end-to-end (reusing the
  `integration_nrpc_protected.rs` fixtures), the Granted one
  discovering through a real encrypted envelope;
- ambiguity: two overlapping valid grants → typed ambiguity error,
  nothing sent;
- temporal refusal: expired grant/membership → local
  `Credentials(Expired…)`, mirroring the provider's T3 verdict for
  the same defect (same predicate, same outcome direction);
- DISCOVER-only grant: resolves internally, refused locally as
  missing-INVOKE — no provider round-trip consumed;
- exhaustive coarse-byte decode round-trip + undecodable-body
  fallback; `NoAuthorizedProvider` considered-count semantics;
  `ProviderNotDirect` for a discovered-but-not-direct candidate;
- no-retry: consecutive `org.call`s produce distinct call ids and
  proofs; no code path resends a signed proof.

**S2 — `serve_org`.** `OrgAccess`, private-by-default visibility
mapping, typed request/response wrapper, `OrgCaller` projection
(+ core item 2), public path unchanged. Witnesses:
- `OrgCaller` fields (incl. `grant_id` presence ⇔ Granted) reach the
  handler; raw `net-org-admission` header absent (reuses the OA-4
  attribution assertions through the wrapper);
- Granted registration: service absent from plaintext CAP-ANN,
  present only inside the grant envelope; SameOrg registration: owner
  envelope only (both reuse the closed OA-3/OA-4 emission witnesses
  through the new registration path);
- `serve_rpc`/`serve_rpc_typed` behavior unchanged beside a
  `serve_org` registration (reuses the public-caps-unchanged T1);
- the `org_admission == None` internal-error path is unreachable
  through the real gate (bridge-level witness);
- streaming shapes refused (inherited unary-only, typed error).

**S3 — composed exit.** `ORG_SDK_EXIT_GATE.md` mapping every claim
above to its witness (new or referenced OA-3/OA-4 test); the
composed example —

```rust
let org = mesh.org(credentials)?;
let result: Customer = org.call("customer.read", &request).await?;
```

— live two-node, both modes, with an assertion that the call
traversed full canonical admission (witnessed via the admission
audit surface, not assumed); the design-test witness (the example
compiles importing none of the §Design-test names); `may_execute`
byte-identity; source scan proving the core-touch inventory is
exhaustive and no new bypass or authority seam exists. **Then
stop.** No further org-SDK work without a named application consumer
or a measured failure.

**Gate cadence** (mirrors OA-4): per slice — new focused tests, the
touched integration target, clippy for touched targets, fmt, diff
review. After S1 (the core-touching slice) the relevant full
lib/integration gates. After S3 the full serial battery once (clippy
`--lib --features cortex -D warnings`; `--no-default-features`; fmt;
full lib cortex; `integration_nrpc_protected`; `org_ownership`;
`org_admission_wire`; `may_execute` body unchanged).

---

## Deferred — must earn its way in via a named consumer

Removed from the common facade by the v0.2 ruling (none inherently
bad; none belong in the first facade). The low-level canonical APIs
keep every capability available meanwhile.

- `OrgAdmin`, file-envelope relocation, CLI rebase — issuance
  already flows through the canonical constructors via the
  `net_sdk::org` re-exports; a grouping type waits for an admin-tool
  consumer.
- `OrgActor` / public `OrgCredentialStore` / mutable
  `install_grant` / `install_audience_secret` — replaced by the
  closed `OrgCredentials` collection.
- Public discovery: `find_service` builder,
  `VerifiedProviderCandidate`, provenance/expiry inspection →
  future `org.discover(...)` shaped by a real consumer.
- `OrgCallOptions` / `call_with` / provider selector / `use_grant` /
  exact-target `OrgClient::call(provider, …)`.
- `OrgServicePolicy`, proof/request policy hooks, the mutable
  `deny_grant_ids` denylist sugar (the §D1 mitigation remains
  expressible today via the low-level `OrgProviderPolicy` closure,
  and at the facade level via `OrgCaller.grant_id` in the handler),
  public visibility configuration, `serve_org_public`,
  `OrgCall::ctx()`.
- `org_files::load_credentials` file helper.
- Live revocation-bundle push tooling; grant-revocation store
  (substrate deferrals, unchanged).
- Bindings parity (Node/Python/Go); watch/subscribe discovery;
  automatic credential renewal; cross-provider failover on denial;
  streaming protection; any wire-shape change.

---

## Locked design points (proposed; frozen at review)

1. The SDK is a **verb layer** over the closed OA substrate — two
   verbs (`org.call`, `serve_org`) plus five public concepts. It
   never admits; local checks only ever refuse to send.
2. Structural + signature validation at construction; temporal
   validity at call time (the ruling's split).
3. Local predicates are the provider's own functions
   (`is_valid_at_with_skew`, `covers_capability`,
   `GrantTargetScope::covers`, `GrantRights::contains`,
   `matches_grant`) — never a reimplementation.
4. Credential matching is exact and total; ambiguity is a typed
   error, never a silent choice, with no override in the facade.
5. Admission mode is inferred from the org relation, never
   caller-specified; access implies encrypted visibility
   (private-by-default), and no fallback mode exists.
6. Secrets stay structurally non-serializable end-to-end; the
   credential container adds no serialization and no `Debug` leak.
7. No new wire surface; `0x0009` + the coarse byte are consumed,
   not extended, and never emitted by SDK code.
8. A signed proof is never resent; the facade performs no automatic
   retry.
9. Discovery is internal and ingest-verified only; knowledge never
   implies invocation authority.
10. `OrgCaller` is a projection of canonical `Admitted`; the common
    handler never sees `RpcContext` or proof bytes.
11. `OrgProofIntent` and the low-level serve/discovery APIs stay
    public and unchanged as the advanced seam; everything the facade
    does is expressible through them by hand.
12. The core-touch inventory is exhaustive; anything beyond it
    reopens this plan's review.

---

## Open questions

- **Q1 — default codec.** One default owned by the SDK on both
  sides of the wire (provisional: `Codec::Json` for debuggability;
  postcard is the compact alternative). Freeze at S0.
- **Q2 — `OrgError` naming collision** with the canonical issuance
  `OrgError` re-export. Recommendation in §5; freeze at S0.
- **Q3 — provider-side audience provisioning boundary.** v1 keeps
  `install_provider_grant_audience` at the core/operational layer
  (like adoption). Confirm at S2 that no facade affordance is
  needed for the first provider consumer.
- **Q4 — shape of the `grant_id` admission extension** (field on
  `Admitted` vs a parallel accessor beside it). Additive either
  way; freeze at S2 with the OA-2 owners.
- **Q5 — binding side-effect semantics.** Consumer-audience installs
  are idempotent and persist for the node's lifetime (no
  uninstall-on-drop). Confirm at S0, including the
  two-clients-one-grant case.
