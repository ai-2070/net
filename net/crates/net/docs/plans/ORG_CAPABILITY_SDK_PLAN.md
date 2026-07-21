# Org Capability SDK Plan (OSDK)

**Version:** v0.3 — applies Kyra's v0.2 verdict (2026-07-21): public
facade **APPROVED**; six narrow authority/lifecycle corrections
applied — (1) common discovery is private-only (no public plaintext
plane, no provenance ranking, no ownership accessor); (2) `OrgCaller`
is an exact projection of canonical `Admitted` — the `grant_id`
field and the `Admitted` extension are deleted; (3) consumer-audience
installs use an internal reference-counted lease, not
node-lifetime persistence; (4) the facade requires a configured
durable mesh identity (`Mesh::identity()`) — no keypair accessor is
added; (5) the facade error is `OrgSdkError` — the canonical
`net_sdk::org::OrgError` is not renamed or shadowed; (6) every v0.2
open question is closed in this revision. The core-touch inventory
reduces to one promotion plus re-exports.

Lineage: v0.1 rejected (enterprise SDK program); v0.2 public shape
approved with corrections; v0.3 is the corrected design of record.
Companion docs:
[`ORG_CAPABILITY_AUTH_PLAN.md`](ORG_CAPABILITY_AUTH_PLAN.md)
(substrate — OA-1..OA-4 CLOSED),
[`OA2E_INTEGRATION_DESIGN.md`](OA2E_INTEGRATION_DESIGN.md) (the live
caller/provider seams the verbs sit on).

**Status:** design of record for the facade; implementation **not
yet authorized**. Activation requires organization-auth review
sign-off **at an exact pinned substrate commit**, recorded here at
authorization time once the open organization-auth Pass-2 findings
are closed. The SDK must not begin against a moving authority
substrate.

**The measured gap (OA-4 STOP-gate answer).** The gap is NOT
"applications need to inspect private-discovery records." The gap
is: **applications cannot privately discover and invoke a protected
service.** Today the only readers of OA-3's verified discovery store
are two `#[doc(hidden)]` test seams; the caller seam requires
hand-assembling all nine `OrgProofIntent` fields; the SDK has no
protected serve; and the coarse denial byte E2.2 put on the wire is
decoded by nothing on the caller side. The verbs below close exactly
that, and nothing else.

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
    |caller: OrgCaller, request| async move { read_customer(caller, request).await })?;

// provider — same-organization, encrypted owner-audience discovery
mesh.serve_org("internal.reindex", OrgAccess::SameOrg,
    |caller: OrgCaller, request| async move { reindex(caller, request).await })?;
```

The two verbs are symmetric: **`serve_org` emits privately;
`org.call` discovers privately.** Protected-but-publicly-discoverable
registration AND discovery both remain low-level.

Public types, complete list: `OrgCredentials`, `OrgClient`,
`OrgAccess`, `OrgCaller`, `OrgSdkError`. Everything else must earn
its way in through a named consumer. The low-level canonical APIs
(`OrgProofIntent`, `serve_rpc_protected` / `serve_rpc_owner_scoped` /
`serve_rpc_granted`, `OrgProviderPolicy`, the raw envelope codecs)
remain available and unchanged — reducing the facade does not reduce
substrate power.

**The design test (acceptance criterion, not aspiration).** A user
must be able to make the secure common path work without knowing
these names: `OrgProofIntent`, `OwnerDelegated`, `CrossOrgGranted`,
the `OrgAudienceSecret` commitment, `ScopedCapabilityAnnouncement`,
`VerifiedScopedCapability`, `CoarseAdmissionReason` (the *name*; the
decoded value appears inside `OrgSdkError`), `GrantTargetScope`.
They may meet them when debugging or using the advanced API. S3's
example witness enforces this: the composed example imports none of
them.

---

## Grounded reality (what the verbs sit on)

As of `org-capability-auth`, 2026-07-21; line numbers are snapshot
references.

- `call()` already does the hard caller work: mints the call id,
  computes the canonical digest, signs via
  `OrgCallProof::sign_for_call`, appends exactly one
  `net-org-admission` header, and pins
  `peer_entity_id(target) == intent.provider`
  (`mesh_rpc.rs:4984–5041`, `sign_admission_proof` `:5525`).
  `org.call` supplies what is missing: credential assembly, exact
  grant matching, private verified discovery, deterministic
  selection, and denial decoding.
- The SDK already exposes the durable identity the facade signs
  with: `Mesh::identity() -> Option<&Identity>`,
  `Identity::keypair() -> &Arc<EntityKeypair>`,
  `Mesh::entity_keypair()`. No new accessor is needed — the facade
  *requires* a configured identity instead of supporting ephemeral
  ones (§1).
- The provider gate already delivers verified facts: protected
  handlers receive `RpcContext.org_admission: Option<Admitted>`
  (five fields, `org_admission.rs:303`), with the raw proof header
  stripped. `serve_org` supplies the typed wrapper that today
  discards `ctx` (`TypedRpcHandler::call`,
  `sdk/src/mesh_rpc.rs:833`) and therefore loses the facts.
- Core serve entry points already exist per mode:
  `serve_rpc_owner_scoped`, `serve_rpc_granted`
  (`mesh_rpc.rs:3139/3170`). `serve_org` maps onto exactly those
  two; it invents no registration path.
- The verified private-discovery queries exist and are expiry- and
  floor-safe (`ScopedDiscoveryStore::find_capabilities_for_grant` /
  `find_owner_private_capabilities`, `org_scoped_store.rs:202/231`)
  but are reachable only through `_for_test` seams
  (`mesh.rs:8900/8931`) — the one core promotion this plan needs.
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

- **No keypair.** The mesh's configured durable identity signs.
  `mesh.org(credentials)` refuses if:
  - `mesh.identity()` is `None` →
    `OrgCredentialError::PersistentIdentityRequired`. Organization
    membership binds to a durable cryptographic entity; a generated
    ephemeral node identity has no place in the common
    organizational facade (the low-level API remains for exotic
    after-start issuance scenarios);
  - `membership.member != mesh.entity_id()` — the TOFU member
    binding would fail remotely anyway; fail here instead.
- **Construction checks structural relationships and signatures;
  calls check temporal validity.** `new` verifies: each credential's
  signature against its org id; `membership.org_id ==
  dispatcher.org_id` (acting-org agreement); every grant names
  `grantee_org == membership.org_id`; no reserved zero grant id;
  every audience secret satisfies `matches_grant(&grant)` against
  exactly one held grant (the §2.6-witnessed commitment relation);
  no duplicate grant ids. Windows are NOT checked here — an actor
  may be assembled long before use; expiry is a call-time refusal.
- **No public `OrgActor`, no public `OrgCredentialStore`, no mutable
  install API.** The collection is closed at construction. Changing
  credentials = construct new `OrgCredentials`, bind again.
- **Consumer-audience lifecycle: internal reference-counted lease**
  (locked). Binding acquires a lease per (grant, secret) pair:
  the first client using grant G →
  `install_consumer_grant_audience(G)`; additional clients using G →
  SDK-local reference count; a clone shares its client's lease; the
  last independently bound client using G drops →
  `remove_consumer_grant_audience(G)`. The lease is internal and
  adds no public API. This keeps installed ingest authority in exact
  correspondence with live credential possession: no ambient
  decryption authority after credentials drop, no immortal registry
  slots, no accumulation across credential rotation, and the
  two-clients-one-grant case is correct by counting. Install
  failures at bind are loud (`OrgSdkError::Credentials`).
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
derive capability          CapabilityAuthorityId::for_tag("nrpc:<svc>")
→ private verified discovery
                           SameOrg → owner-private scoped store;
                           Granted → grant-audience scoped store (per
                           leased grant). The public plaintext plane is
                           NOT searched — no ownership projection, no
                           provenance model, no plaintext fallback.
→ authority-relation classification
                           by plane: an owner-plane candidate is a
                           SameOrg call (no grant attached — attaching
                           one is UnexpectedCapabilityGrant remotely);
                           a granted-plane candidate carries the grant
                           whose audience produced it.
→ exact grant matching     the complete authority relation: grantee org,
                           issuer org, capability, INVOKE,
                           target_scope.covers(provider, owner), window
                           valid now — the provider's own predicates,
                           never a reimplementation. Overlapping valid
                           matches → typed ambiguity error; nothing sent.
→ deterministic selection  eligible = classified + grant-matched +
                           direct-session-reachable (E0.3);
                           → lowest provider EntityId. No provenance
                           ranking, no load heuristics, no selector.
→ canonical OrgProofIntent all nine fields; signed with the mesh's
                           configured durable identity.
→ exact-target protected call
                           core call() pins, mints, digests, signs,
                           sends one request — unchanged.
→ coarse denial decoding   0x0009 body → CoarseAdmissionReason →
                           OrgSdkError::AdmissionDenied.
```

Ruling-locked exclusions: **no** public exact-provider call, **no**
provider selector, **no** `use_grant`, **no** discovery builder,
**no** options object, **no** `call_with` until a real consumer
needs one specific option.

**The SDK owns** (not the caller): proof TTL (the shared 30 s,
`MAX_ORG_PROOF_TTL_SECS`); grant matching; provider selection;
provider pinning; retry prohibition (a signed proof is never resent;
the SDK performs no automatic retry of any kind — every `org.call`
is one fresh call id and one fresh signature; idempotency across
calls is the application's, per the volatile replay-guard contract);
codec (`Codec::Json`, locked — the existing typed-RPC default);
timeout (inherited core default).

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
    pub entity: EntityId,
    pub acting_org: OrgId,
    pub provider_org: OrgId,
    pub provider: EntityId,
    pub capability: CapabilityAuthorityId,
}
```

An **exact projection of current canonical `Admitted`** — the same
five fields, nothing added, nothing renamed into a new authority
object. Applications authorize ordinary requests on verified entity
and organization identity. Advanced providers requiring exact-grant
policy use the low-level `OrgProviderPolicy`, whose verified proof
contains the grant id — that low-level closure also remains the home
of the §D1 provider-local grant-refusal lever. There is **no
`grant_id` on `OrgCaller`** and **no extension to `Admitted` or
`verify_org_admission`** (deleted with the deferred denylist story
it existed to serve).

The handler receives `(OrgCaller, Req)` and returns
`Result<Resp, _>`; the wrapper decodes/encodes with the locked
codec. **No `RpcContext` exposure in the common handler** —
applications needing headers, packet metadata, or proof-level policy
use the existing low-level protected serve API.

If `org_admission` is absent inside a `serve_org` handler, that is
an invariant violation (the gate dispatches only admitted protected
calls): the wrapper returns an internal error loudly, never panics,
and S2 witnesses the path unreachable through the real gate.

### 5. `OrgSdkError`

```rust
pub enum OrgSdkError {
    Credentials(OrgCredentialError),   // mismatch, expired, missing grant,
                                       // ambiguous grant, audience mismatch,
                                       // persistent identity required
    Discovery(OrgDiscoveryError),      // no authorized provider (with the
                                       // considered count), provider not direct
    AdmissionDenied(CoarseAdmissionReason),
    Rpc(RpcError),
}
```

- **Name locked: `OrgSdkError`** — both `mesh.org` and `serve_org`
  return it. The canonical issuance error `net_sdk::org::OrgError`
  keeps its name and path untouched; an existing public type name is
  never hijacked for aesthetic cleanliness.
- Four meaningful domains to branch on — not a flattened
  implementation map. The nested enums carry the detail (all local,
  so they leak nothing); the remote reason stays the coarse 3-bucket
  wire value by design (denial reasons must not become a credential
  oracle — E2.2). An undecodable 0x0009 body maps to
  `AdmissionDenied(Denied)`, never an error-about-an-error.

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
through the existing low-level core API (`serve_rpc_protected`); if
repeated consumers demand it, an explicit `serve_org_public(...)` is
added then — not before (§Deferred).

**Provider policy in v1 is the handler**: `serve_org` installs the
trivial `|_| true` proof policy, and application-level decisions are
made in the handler body with `OrgCaller` in hand. The step-11
proof-policy hook stays fully available on the low-level serve APIs.

**Provider-audience provisioning contract (locked).**
Provisioning B's (grant, secret) pairs so a node can seal granted
envelopes (`install_provider_grant_audience`) is operational state,
like adoption and revocation files; it stays at the core/operational
layer in v1. The contract, stated explicitly:

```
serve_org(..., OrgAccess::Granted, ...) may register before a
matching provider audience exists; admission protection is active
immediately; the service remains encrypted and undiscoverable until
install_provider_grant_audience is called; installation triggers
coherent reannouncement (the existing core contract).
```

Facade registration therefore **never fails for lack of an
audience** — failing would break valid startup ordering and dynamic
grant installation. The S2/S3 witnesses actually perform the
provider-audience install; the two-line application example assumes
node authority is already provisioned, just as it assumes a running
mesh.

---

## Discovery is internal — and private-only

`org.call(...)` consumes verified discovery internally, and in v1
searches **only** the private planes: the owner-private scoped store
for `SameOrg`, the grant-audience scoped store for `Granted`. The
public plaintext plane is not consulted — searching it would
reintroduce a public ownership projection, provenance distinctions,
private-before-public ranking, an ownership core accessor, and
support for registrations the common provider API never creates.
This makes the verbs symmetric: `serve_org` emits privately,
`org.call` discovers privately.

All discovery inputs are ingest-verified state: the scoped store's
floor/expiry-safe queries, whose records already passed the full
envelope chain (outer signature, owner cert, audience selection,
AEAD, descriptor↔grant binding) at `verify_scoped_ingest`.
Knowledge never implies invocation authority; the credential-relation
match and the provider's admission remain the only authority steps.

No public candidate ontology, no `VerifiedProviderCandidate`, no
`find_service` builder — the implementation needs a candidate model;
the public surface does not. If a named consumer later needs to
enumerate providers, inspect expiry, or rank manually, add
`org.discover("customer.read")` shaped by that consumer's actual
requirements (§Deferred). Calling a protected-but-public service
remains possible today via the low-level seam (`OrgProofIntent` +
`call`).

---

## Credential loading is separable

`OrgCredentials::new` takes in-memory canonical types. File formats,
permissions, DACL checks, CLI envelopes, and secret publication stay
where they are (CLI + config codecs) — filesystem credential
handling has demonstrated review gravity and must not block the
call/serve facade. An optional `org_files::load_credentials(path)`
helper may come later (§Deferred), reusing the CLI's envelope codecs
rather than inventing new ones.

---

## Core-touch inventory (exhaustive)

Everything is SDK-crate code except:

1. **Promote the two scoped-store queries** out of their
   `#[doc(hidden)] *_for_test` names into production `MeshNode`
   methods (owned snapshot returns, no lock-holding borrows). They
   are consumed internally by `org.call`; nothing new is re-exported
   publicly from the SDK. Test seams become aliases or are retired —
   test-named seams must not be load-bearing production API.
2. **Pure re-exports** in `org::types` as needed (`Admitted`,
   `OrgAdmission`, `CoarseAdmissionReason`, `OrgProofIntent` alias).

Explicitly removed from the v0.2 inventory by the verdict: the
`Admitted.grant_id` extension (grant-id ergonomics deferred with the
denylist story), the node-identity/keypair accessor
(`Mesh::identity()` already provides the signing `Arc`), and the
public ownership accessor (v1 no longer consumes the public
discovery plane).

Explicitly untouched: `Admitted` and every `verify_org_admission`
step and its order, the serve gates and `UnaryAdmission`, all wire
objects, headers, and status codes, the replay guard, the RED seam,
`OrgProofIntent` itself, the canonical `net_sdk::org::OrgError`, and
`may_execute` (byte-for-byte re-verified at the exit gate).

---

## Slices (four bounded commits, stop-and-review per slice)

**S0 — credentials and errors.** `OrgCredentials` + structural
validation, mesh-identity binding, the internal audience **lease**,
`OrgSdkError`, `org::types` re-exports. Witnesses:
- structural-validation matrix (each broken relationship → its typed
  `OrgCredentialError`);
- `PersistentIdentityRequired` on a mesh with no configured
  identity; member-binding refusal on
  `membership.member != mesh.entity_id()`;
- audience-mismatch refusal (reuses the §2.6 commitment relation);
- **lease semantics**: two clients bound with one grant → exactly one
  install; a clone shares its client's lease; dropping one client
  removes nothing; dropping the last removes the registration;
  re-binding after full drop re-installs;
- container non-serializability + redacted `Debug` (type-level);
- the canonical `net_sdk::org::OrgError` still resolves to the
  issuance error (compile witness); existing flat `net_sdk::org::*`
  paths unchanged. No core diff.

**S1 — `org.call`.** The core query promotion (item 1), private-only
discovery, classification, exact matching, selection, intent, coarse
decode. Witnesses:
- intent-equality: for both modes, the facade-built `OrgProofIntent`
  equals a hand-assembled reference field-for-field given the same
  inputs;
- **one live SameOrg witness and one live Granted witness** — real
  two-node `MeshNode::call` traversal end-to-end (reusing the
  `integration_nrpc_protected.rs` fixtures), the Granted one
  discovering through a real encrypted envelope;
- **private-only**: a protected service registered
  publicly-discoverable via the low-level API is NOT found by
  `org.call` (no plaintext fallback — negative witness);
- ambiguity: two overlapping valid grants matching the selected
  provider → typed ambiguity error, nothing sent;
- temporal refusal: expired grant/membership → local
  `Credentials(…)` refusal, mirroring the provider's T3 verdict for
  the same defect;
- DISCOVER-only grant: resolves internally, refused locally as
  missing-INVOKE — no provider round-trip consumed;
- deterministic selection: two eligible providers → lowest
  `EntityId` chosen, stable across runs;
- exhaustive coarse-byte decode round-trip + undecodable-body
  fallback; `NoAuthorizedProvider` considered-count semantics;
  `ProviderNotDirect` for a discovered-but-not-direct candidate;
- no-retry: consecutive `org.call`s produce distinct call ids and
  proofs; no code path resends a signed proof.

**S2 — `serve_org`.** `OrgAccess`, private-by-default visibility
mapping, typed request/response wrapper, `OrgCaller` projection,
public path unchanged. Witnesses:
- `OrgCaller` is the exact five-field `Admitted` projection and
  reaches the handler; raw `net-org-admission` header absent (reuses
  the OA-4 attribution assertions through the wrapper);
- Granted registration: service absent from plaintext CAP-ANN,
  present only inside the grant envelope; SameOrg registration:
  owner envelope only (both reuse the closed OA-3/OA-4 emission
  witnesses through the new registration path);
- **provisioning contract**: `serve_org(Granted)` with no provider
  audience → registration succeeds, admission active, service
  undiscoverable; `install_provider_grant_audience` → coherent
  reannouncement → the grantee discovers and invokes;
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

— live two-node, both modes, with the provider audience actually
installed in the fixture and an assertion that the call traversed
full canonical admission (witnessed via the admission audit surface,
not assumed); the design-test witness (the example compiles
importing none of the §Design-test names); `may_execute`
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

## Closed questions (all locked; reopening any reopens review)

- **Codec** — `Codec::Json`, the existing typed-RPC default, on both
  sides of the wire.
- **Error naming** — `OrgSdkError`; canonical `net_sdk::org::OrgError`
  preserved untouched.
- **Provider audience provisioning** — operational/low-level for v1
  with the explicit register-before-audience contract above; facade
  registration never fails for lack of an audience; exit witnesses
  perform the install.
- **Grant-id extension** — deleted, with `OrgCaller.grant_id`.
- **Consumer registration lifecycle** — the internal
  reference-counted lease; install on first use, remove on last
  drop; clones share their client's lease.

---

## Deferred — must earn its way in via a named consumer

Removed from the common facade by the v0.2/v0.3 rulings (none
inherently bad; none belong in the first facade). The low-level
canonical APIs keep every capability available meanwhile.

- Public-plane discovery in `org.call` (and the ownership accessor
  it would require); `serve_org_public`.
- `OrgCaller.grant_id` / any `Admitted` extension / exact-grant
  facade policy — the low-level `OrgProviderPolicy` proof already
  carries the grant id, including for the §D1 provider-local
  refusal lever.
- `OrgAdmin`, file-envelope relocation, CLI rebase — issuance
  already flows through the canonical constructors via the
  `net_sdk::org` re-exports; a grouping type waits for an admin-tool
  consumer.
- `OrgActor` / public `OrgCredentialStore` / mutable
  `install_grant` / `install_audience_secret` — replaced by the
  closed `OrgCredentials` collection + internal leases.
- Public discovery surface: `find_service` builder,
  `VerifiedProviderCandidate`, provenance/expiry inspection →
  future `org.discover(...)` shaped by a real consumer.
- `OrgCallOptions` / `call_with` / provider selector / `use_grant` /
  exact-target `OrgClient::call(provider, …)`.
- `OrgServicePolicy`, proof/request policy hooks, the denylist
  sugar, public visibility configuration, `OrgCall::ctx()`.
- `org_files::load_credentials` file helper.
- Live revocation-bundle push tooling; grant-revocation store
  (substrate deferrals, unchanged).
- Bindings parity (Node/Python/Go); watch/subscribe discovery;
  automatic credential renewal; cross-provider failover on denial;
  streaming protection; any wire-shape change.

---

## Locked design points (frozen at review)

1. The SDK is a **verb layer** over the closed OA substrate — two
   verbs (`org.call`, `serve_org`) plus five public types. It never
   admits; local checks only ever refuse to send.
2. The verbs are symmetric: `serve_org` emits privately, `org.call`
   discovers privately; the public plane is low-level on both sides.
3. Structural + signature validation at construction; temporal
   validity at call time.
4. Local predicates are the provider's own functions
   (`is_valid_at_with_skew`, `covers_capability`,
   `GrantTargetScope::covers`, `GrantRights::contains`,
   `matches_grant`) — never a reimplementation.
5. Credential matching is exact and total; ambiguity is a typed
   error, never a silent choice, with no override in the facade.
6. Admission mode is inferred (plane + org relation), never
   caller-specified; access implies encrypted visibility; no
   fallback mode exists.
7. The facade requires a configured durable mesh identity; it never
   clones secret key material and adds no keypair accessor.
8. Installed consumer-audience state corresponds exactly to live
   credential possession (the reference-counted lease); authority is
   never immortal.
9. Secrets stay structurally non-serializable end-to-end; the
   credential container adds no serialization and no `Debug` leak.
10. No new wire surface; `0x0009` + the coarse byte are consumed,
    not extended, and never emitted by SDK code.
11. A signed proof is never resent; the facade performs no automatic
    retry.
12. `OrgCaller` is an exact projection of canonical `Admitted`; the
    common handler never sees `RpcContext`, proof bytes, or grant
    ids.
13. `OrgProofIntent` and the low-level serve/discovery APIs stay
    public and unchanged as the advanced seam; everything the facade
    does is expressible through them by hand.
14. The core-touch inventory (one query promotion + re-exports) is
    exhaustive; anything beyond it reopens this plan's review.
