# Org Capability SDK Plan (OSDK)

**Version:** v0.1 — design for review (2026-07-21). Originating
specification: Kyra's SDK ruling (2026-07-19, "the org capability SDK
should be a thin, role-oriented facade over the canonical OA types,
not another authorization model"). Companion to
[`ORG_CAPABILITY_AUTH_PLAN.md`](ORG_CAPABILITY_AUTH_PLAN.md) (the
substrate — OA-1..OA-4 all CLOSED) and
[`OA2E_INTEGRATION_DESIGN.md`](OA2E_INTEGRATION_DESIGN.md) (the live
caller/provider seams this plan wraps).

**Status:** design only; no code authorized. Activation gate: review
sign-off. Prerequisites (all met): OA-1..OA-4 closed
(`OA3_EXIT_GATE.md`, `OA4_EXIT_GATE.md`), `#47` live wiring signed
off, OA2-F grant management landed.

**The named consumer (OA-4 STOP-gate answer).** OA-4 ends with "no
further organization work without a named consumer or a measured
failure." This plan is the consumer surface, and it is justified by a
measured gap, not speculation:

- OA-3's verified private-discovery store has **no production
  reader**: the only query surface is two `#[doc(hidden)]` test seams
  (`MeshNode::scoped_owner_providers_for_test` /
  `scoped_granted_providers_for_test`, `mesh.rs:8900/8931`). A
  protected-and-private service cannot be privately discovered by any
  production caller today.
- OA-2's caller seam requires hand-assembling all nine
  `OrgProofIntent` fields (`mesh_rpc.rs:230`) with **no local
  validation of any kind** — wrong grant, expired window, wrong
  target scope, mismatched org all surface only as a remote
  `0x0009` whose coarse reason byte **no caller-side code decodes**
  (`CoarseAdmissionReason::from_wire` is unused off the provider).
- The SDK `Mesh` exposes **no protected serve at all**, and
  `serve_rpc_typed` handlers structurally cannot see the admitted
  facts (`TypedRpcHandler::call` discards `ctx`,
  `sdk/src/mesh_rpc.rs:833`).

**Design principle (locked, from the ruling):** the ergonomic API
makes the secure path short —

```rust
client.call_service("customer.read", &req, OrgCallOptions::default()).await
```

— while internally preserving the full exact chain: actor identity →
membership → dispatcher delegation → cross-org capability grant if
required → verified private discovery → exact provider selection →
canonical request-bound proof → live provider admission →
provider-local policy → handler. `OrgProofIntent` remains the
advanced low-level escape hatch, unchanged.

**What the SDK is NOT:** another authorization model. It never
admits, never weakens admission, adds no wire objects, headers,
status codes, or authority semantics. `verify_org_admission`,
`may_execute`, the serve gates, and every wire codec are untouched.
Local pre-flight validation only ever *refuses to send* — it is
structurally incapable of making a provider accept anything.

---

## Grounded inventory — what exists vs. what is missing

(As of `org-capability-auth` HEAD, 2026-07-21. Line numbers are
snapshot references, not contracts.)

| Area | Exists (canonical) | Missing (this plan) |
|---|---|---|
| Caller proof seam | `CallOptions.org_proof_intent`; `call()` mints call_id, computes `org_request_digest`, signs via `OrgCallProof::sign_for_call`, appends exactly one `net-org-admission` header, pins provider identity (`mesh_rpc.rs:4984–5041`, `sign_admission_proof` `:5525`) | Builder/assembly, wallet, local pre-flight, mode inference, typed errors |
| Caller validation | Provider-side only (`org_admission.rs`); caller checks capability-tag match, TTL 1..=30 s, single header | Everything else: window/rights/target/relation checks before send |
| Denial surface | `RpcStatus::AdmissionDenied = 0x0009`, body = 1 coarse byte (`emit_admission_denial`, `mesh_rpc.rs:856`); detailed `AdmissionDenied` (30 variants) stays provider-side | Caller-side decode of the coarse byte; a typed local-vs-remote error split |
| Provider serve | Core `MeshNode::serve_rpc_protected` / `serve_rpc_owner_scoped` / `serve_rpc_granted` (`mesh_rpc.rs:3072/3139/3170`); `RpcContext.org_admission: Option<Admitted>`; header stripped | SDK-level protected serve; typed handlers that receive `Admitted`; policy ergonomics |
| Provider policy | `OrgProviderPolicy = Arc<dyn Fn(&OrgCallProof) -> bool>` — proof only, runs step 11, after replay insert | Request-aware policy (Q1); grant-id denylist helper (§D1 residual) |
| Private discovery | `ScopedDiscoveryStore::find_capabilities_for_grant` / `find_owner_private_capabilities` (`org_scoped_store.rs:202/231`), expiry- and floor-safe | **Any** production/SDK query surface (today: `#[doc(hidden)]` test seams only) |
| Public discovery | `find_nodes`/`find_best_node` over the plaintext fold; ownership projection `owner_org_for` + `verify_announced_owner_cert` (`capability_bridge.rs:476/355`) | Ownership-verified candidate model unified with the scoped plane |
| Credentials | `OrgAudienceSecret::matches_grant` (`org_grant.rs:369`), commitment check, structurally non-serializable + zeroizing secrets | A wallet: exact matching, ambiguity-is-an-error, loud install |
| Admin issuance | Canonical `try_issue` constructors; CLI already calls them **through `net_sdk::org` re-exports** (no parallel logic); offline; secret-hygiene hardened (`cli/src/secret.rs`, staged 0600 publish) | An `OrgAdmin` grouping as the single choke point; shared file-envelope codecs so apps can read CLI-minted files |
| SDK module shape | Flat re-export file `sdk/src/org.rs`; `OrgProofIntent` re-exported via `net_sdk::mesh_rpc`, not `org` | The role-oriented module split below |

---

## Module layout

```
net_sdk::org::types      canonical OA type re-exports (spine; includes
                         OrgProofIntent, Admitted, OrgAdmission,
                         CapabilityVisibility, CoarseAdmissionReason)
net_sdk::org::admin      offline root operations (OrgAdmin)
net_sdk::org::client     caller credentials + protected calls
                         (OrgActor, OrgCredentialStore, OrgClient)
net_sdk::org::provider   protected registration + policy
                         (serve_org_typed, OrgServicePolicy, OrgCall)
net_sdk::org::discovery  verified discovery + audience state
                         (find_service, install wrappers)
```

The existing flat `net_sdk::org::*` paths remain valid (`pub use
types::*` at the module root) — no downstream churn. `types` also
re-exports `OrgProofIntent` (today only on `net_sdk::mesh_rpc`) so
the whole org surface is reachable from one module.

---

## 1. Errors — `OrgSdkError`

A caller must be able to distinguish: (a) it lacked authority
locally; (b) no authorized provider was discoverable; (c) the
provider's live authority state denied; (d) provider policy vetoed;
(e) transport failed. Today (a)–(d) are one opaque
`RpcError::ServerError`/`Codec` soup.

```rust
#[derive(Debug, thiserror::Error)]
pub enum OrgSdkError {
    // ---- local, before anything is sent ----
    CredentialMismatch(CredentialMismatch),   // actor assembly (§3.1)
    MissingCapabilityGrant { capability: CapabilityAuthorityId,
                             provider_org: OrgId },
    AmbiguousCapabilityGrant { capability: CapabilityAuthorityId,
                               matches: Vec<[u8; 32]> },   // grant_ids
    AudienceSecretMismatch { grant_id: [u8; 32] },
    PreflightRefused(PreflightReason),        // window/scope/rights/target (§3.4)
    StreamingUnsupported,
    // ---- discovery / selection ----
    NoAuthorizedProvider { capability: CapabilityAuthorityId,
                           considered: usize },
    ProviderNotDirect { provider: EntityId },  // E0.3: direct-session-only
    // ---- remote ----
    RemoteAdmissionDenied { coarse: CoarseAdmissionReason },  // 0x0009 decoded
    RemoteError { status: u16, message: String },
    Transport(#[from] RpcError),
}
```

Notes:

- `RemoteAdmissionDenied` is produced by decoding the existing wire
  shape — `ServerError { status: 0x0009, body: [coarse_byte] }` —
  with `CoarseAdmissionReason::from_wire`. **No new wire surface**;
  the SDK finally consumes the byte E2.2 shipped. An undecodable
  body maps to `RemoteAdmissionDenied { coarse: Denied }` (fail
  toward the least-informative bucket, never an error-in-error).
- The coarse 3-bucket wire enum stays coarse **by design** (denial
  reasons must not become a credential oracle — E2.2). The SDK does
  not attempt to reconstruct detailed reasons from timing or
  retries, and the docs say so.
- Local pre-flight reasons (`PreflightReason`) are detailed and
  free: they leak nothing because they never leave the caller.
- Exact enum shape is provisional; frozen at S0 review.

---

## 2. `net_sdk::org::admin` — offline root operations

Thin grouping over the canonical constructors the CLI already calls;
the value is a single choke point plus app-readable credential
files, not new logic.

```rust
pub struct OrgAdmin { root: OrgKeypair }

impl OrgAdmin {
    pub fn from_root_key(root: OrgKeypair) -> Self;
    pub fn org_id(&self) -> OrgId;

    pub fn issue_membership(&self, member: EntityId, generation: u32,
        ttl_secs: u64) -> Result<OrgMembershipCert, OrgError>;
    pub fn issue_floors(&self, floors: &BTreeMap<EntityId, u32>)
        -> Result<OrgRevocationBundle, OrgError>;
    pub fn grant_dispatcher(&self, dispatcher: EntityId,
        scope: DispatcherScope, ttl_secs: u64)
        -> Result<OrgDispatcherGrant, OrgError>;
    pub fn grant_capability(&self, grantee_org: OrgId,
        capability: CapabilityAuthorityId, rights: GrantRights,
        target_scope: GrantTargetScope, ttl_secs: u64)
        -> Result<(OrgCapabilityGrant, Option<OrgAudienceSecret>), OrgError>;
}
```

Decisions:

- **Pure delegation.** Each method is one call to the existing
  `try_issue` (`org.rs:458/773`, `org_grant.rs:491/787`). All
  issuance invariants (TTL caps, `rights ⊇ DISCOVER ⇔` fresh
  audience material, zero-grant-id reservation) stay enforced where
  they live today — in the constructors. `OrgAdmin` adds none and
  removes none.
- **Offline by construction.** The module imports no `MeshNode` /
  `Mesh` / transport type (witnessed by a compile-time/module-dep
  check in S1). Issuance runs on an air-gapped machine exactly as
  the CLI does today.
- **CLI rebased onto `OrgAdmin`.** The CLI keeps everything that is
  genuinely CLI-shaped — argv parsing, `load_org_key`, `ScrubbedString`
  hygiene, 0600 staged no-clobber publication, `--force` refusal —
  and routes the issuance call itself through `OrgAdmin`. This makes
  "CLI and SDK call the same canonical constructors" structural
  rather than incidental.
- **Shared file envelopes.** The versioned JSON envelope codecs the
  CLI writes (`OrgCertFile` / `OrgFloorsFile` /
  `OrgDispatcherGrantFile` / `OrgCapabilityGrantFile`,
  `ORG_FILE_VERSION = 1`) move to a location both the CLI and
  `net_sdk::org` can use (exact home decided at S1 review — Q3), so
  an application can load a CLI-minted grant file directly into the
  wallet. The audience-secret config codec
  (`OrgAudienceSecret::{encode,decode}_config`) is already shared.
- **Root-key loading stays out.** `OrgAdmin` takes an in-memory
  `OrgKeypair`; reading seed files, permission gates, and DACL
  warnings remain the CLI's (`load_org_key`) or the application's
  responsibility. The SDK does not invent a second seed-file reader.

Deliberately NOT in `OrgAdmin` v1: live revocation push (applying an
`OrgRevocationBundle` to a running node exists as library machinery —
`OrgRevocationStore::apply_bundle` +
`MeshNode::install_org_revocation_store` — but has no driver verb;
that is separate admin-ops tooling with its own review), key
rotation ceremonies, and any daemon/HTTP admin endpoint.

---

## 3. `net_sdk::org::client` — caller credentials and protected calls

### 3.1 `OrgActor` — who is calling, for whom

```rust
pub struct OrgActor {
    caller: Arc<EntityKeypair>,
    membership: OrgMembershipCert,
    dispatcher: OrgDispatcherGrant,
}

impl OrgActor {
    pub fn new(caller: Arc<EntityKeypair>, membership: OrgMembershipCert,
        dispatcher: OrgDispatcherGrant) -> Result<Self, OrgSdkError>;
    pub fn acting_org(&self) -> OrgId;   // == membership.org_id
}
```

`new` fails loudly (`CredentialMismatch`) when:

- `membership.member != caller entity id` (the TOFU member binding
  would fail remotely anyway — fail here instead);
- `membership.org_id != dispatcher.org_id` (acting-org agreement,
  mirrors admission step "acting-org mismatch");
- either credential's signature fails `verify` against its org id,
  or its window is already expired at construction (skew-tolerant,
  same `is_valid_at_with_skew` predicates the provider uses).

### 3.2 `OrgCredentialStore` — exact wallet, not an ambient bag

```rust
pub struct OrgCredentialStore {
    actor: OrgActor,
    capability_grants: Vec<OrgCapabilityGrant>,
    audience_secrets: Vec<OrgAudienceSecret>,
}
```

- `install_grant(grant)` — verifies the grant signature, rejects the
  reserved zero grant id, rejects `grantee_org != actor.acting_org()`
  (a wallet holds only grants naming this actor's org), rejects
  duplicates by `grant_id`.
- `install_audience_secret(secret)` — **requires** a previously
  installed grant with `secret.matches_grant(&grant)`
  (`org_grant.rs:369` — grant-id match AND key-commitment match);
  otherwise `AudienceSecretMismatch`. This reuses the §2.6-witnessed
  commitment check; a wrong or stale secret never sits silently in
  the wallet.
- **Grant lookup matches the complete authority relation** — all of:
  - `grant.grantee_org == actor.acting_org()`
  - `grant.issuer_org == provider_owner_org`
  - `grant.capability == capability`
  - `grant.rights` contains the required right (INVOKE for calls,
    DISCOVER for private discovery)
  - `grant.target_scope.covers(provider_entity, Some(provider_owner_org))`
    (the same `GrantTargetScope::covers`, `org_grant.rs:223`)
  - window valid at now (skew-tolerant).

  Zero matches → `MissingCapabilityGrant`. **Two or more matches →
  `AmbiguousCapabilityGrant`** listing the candidate grant ids —
  never a silent choice. (Ambiguity is possible: overlapping
  `ExactNode` + `AnyNodeOwnedBy` grants for one capability.) An
  explicit `OrgCallOptions::use_grant(grant_id)` override resolves
  ambiguity deliberately.
- The store derives `Debug` by hand (redacted), implements neither
  `Serialize` nor `Deserialize` (type-level witness, same pattern as
  `OrgAudienceSecret`), and drops secrets zeroized (inherited from
  the contained types).

### 3.3 `OrgClient` — the call surface

```rust
impl Mesh {
    pub fn org_client(&self, actor: OrgActor) -> OrgClient;
}

impl OrgClient {
    pub fn install_grant(&mut self, g: OrgCapabilityGrant) -> Result<(), OrgSdkError>;
    pub fn install_audience_secret(&mut self, s: OrgAudienceSecret) -> Result<(), OrgSdkError>;

    /// Exact-target protected call (provider already known).
    pub async fn call<Req, Resp>(&self, provider: EntityId, service: &str,
        req: &Req, opts: OrgCallOptions) -> Result<Resp, OrgSdkError>;

    /// Discover → verify → select → pin → call (§5, §6).
    pub async fn call_service<Req, Resp>(&self, service: &str,
        req: &Req, opts: OrgCallOptions) -> Result<Resp, OrgSdkError>;

    pub fn find_service(&self, service: &str) -> FindService;   // §5
}
```

`OrgCallOptions`: codec (reuses the typed-RPC `Codec`), timeout,
`proof_ttl_secs` (default = the shared 30 s, clamped to
`MAX_ORG_PROOF_TTL_SECS`), `use_grant`, provider-selection hook
(§6). Streaming shapes are refused locally with
`StreamingUnsupported` before core would refuse them.

### 3.4 What `call` does internally (exact-target)

1. `capability = CapabilityAuthorityId::for_tag("nrpc:<service>")` —
   the same derivation `sign_admission_proof` re-checks.
2. Resolve `provider_owner_org`: the ownership projection for the
   pinned provider (public plane: `owner_org_for`; private plane: the
   verified record's `owner_org` — §5). No projection → the call
   cannot be classified → `PreflightRefused(UnknownProviderOwner)`
   unless `opts` supplies the expected owner org explicitly.
3. **Mode inference — derived, never caller-specified:**
   - `provider_owner_org == actor.acting_org()` → owner-delegated:
     `capability_grant = None` (attaching one is
     `UnexpectedCapabilityGrant` remotely; the SDK never does).
   - otherwise → cross-org: wallet lookup per §3.2 (INVOKE).
4. **Local pre-flight (advisory, fail-fast, never authority):**
   membership + dispatcher + grant windows at now; dispatcher scope
   covers the capability (`covers_capability`); grant relation per
   §3.2. Pre-flight uses **the caller's local knowledge only** — it
   cannot see the provider's floors, replay state, or live policy,
   and the docs say so. Its contract: every refusal is a call the
   provider was certain to deny *for the checked reason*; it makes
   no promise in the accept direction.
5. Build the canonical `OrgProofIntent` (all nine fields) and place
   it on `CallOptions.org_proof_intent`.
6. Delegate to `MeshNode::call` — which (unchanged) pins
   `peer_entity_id(target) == intent.provider`, mints the call id,
   computes the canonical digest, signs, appends exactly one header,
   sends one exact-target request.
7. Map the result: decode `Resp` on success; `0x0009` → coarse
   decode → `RemoteAdmissionDenied`; pin failure ("target is not the
   pinned provider" / no direct session) → `ProviderNotDirect`;
   everything else → `RemoteError` / `Transport`.

The application never signs an `OrgCallProof`, never touches the
header, and never sees the proof bytes.

**Retry rule (locked):** the SDK never resends a signed proof. Any
retry is a new call — new call id, new expiry, new signature —
because the replay guard is volatile-by-contract and keyed on
`(caller, call_id)`; cross-restart idempotency remains the
application's. `RemoteAdmissionDenied` is never auto-retried.

### 3.5 Pre-flight uses the provider's own predicates

Locked: pre-flight calls the exact functions admission calls —
`is_valid_at_with_skew`, `covers_capability`,
`GrantTargetScope::covers`, `GrantRights::contains`,
`matches_grant` — not a parallel reimplementation. A divergence
class (SDK says yes, provider says no, or vice versa) can then only
come from state the caller genuinely cannot see (floors, replay,
policy, live authority), which is the honest boundary.

---

## 4. `net_sdk::org::provider` — protected registration and policy

### 4.1 `serve_org_typed`

```rust
impl Mesh {
    pub fn serve_org_typed<Req, Resp, F, Fut>(&self, service: &str,
        codec: Codec, policy: OrgServicePolicy, handler: F)
        -> Result<ServeHandle, OrgSdkError>
    where F: Fn(OrgCall, Req) -> Fut + Send + Sync + 'static,
          Fut: Future<Output = Result<Resp, String>> + Send;
}

pub struct OrgCall { /* borrowed view over RpcContext */ }
impl OrgCall {
    pub fn admitted(&self) -> &Admitted;      // guaranteed present (below)
    pub fn caller(&self) -> EntityId;
    pub fn acting_org(&self) -> OrgId;
    pub fn provider_org(&self) -> OrgId;
    pub fn provider(&self) -> EntityId;
    pub fn capability(&self) -> CapabilityAuthorityId;
    pub fn ctx(&self) -> &RpcContext;         // escape hatch (headers etc.)
}
```

- The wrapper mirrors `TypedRpcHandler` (decode `Req` → run → encode
  `Resp`) but **forwards the verified facts**: it reads
  `ctx.org_admission` and hands the handler an `OrgCall`. Today's
  typed wrapper discards `ctx` entirely — this is the ergonomic fix
  for "authenticated, provider-verified facts" without any
  `expect()` in application code.
- `org_admission == None` inside a protected typed handler is an
  invariant violation (the gate only dispatches admitted protected
  calls). The wrapper does not panic: it refuses with an internal
  server error and a loud log, and S3 carries a witness that the
  path is unreachable through the real gate.
- No mode fallback, ever: public handlers keep `serve_rpc` /
  `serve_rpc_typed`; protected handlers use `serve_org_typed`. There
  is deliberately no "public or protected" registration and no
  auto-upgrade.

### 4.2 `OrgServicePolicy` — explicit mode, explicit visibility

```rust
OrgServicePolicy::owner_delegated()        // OrgAdmission::OwnerDelegated
OrgServicePolicy::cross_org()              // OrgAdmission::CrossOrgGranted
    .visibility(OrgVisibility::Public)     // default
    .visibility(OrgVisibility::Private)    // owner_delegated → OwnerScoped
                                           // cross_org       → GrantedAudience
    .deny_grant_ids([g1, g2])              // §4.3
    .with_proof_policy(|proof: &OrgCallProof| ...)   // core step-11 hook
    .with_request_policy(|call: &OrgCall, req: &Req| ...)  // §4.4 / Q1
```

Mapping onto the existing core entry points (no new core serve
paths): `owner_delegated + Public` → `serve_rpc_protected(OwnerDelegated)`;
`owner_delegated + Private` → `serve_rpc_owner_scoped`;
`cross_org + Public` → `serve_rpc_protected(CrossOrgGranted)`;
`cross_org + Private` → `serve_rpc_granted`. The exact
(admission × visibility) matrix — including which combinations the
core constructors refuse — is pinned as a table at S3 review, not
invented here.

### 4.3 `deny_grant_ids` — the §D1 provider-local lever, built

`ORG_CAPABILITY_AUTH_PLAN.md` §D1 records that a provider org B has
no cryptographic lever to withdraw an issued cross-org grant before
`not_after`, and that the closing mechanism — "`grant_id` is in the
proof the policy sees — exists and is not built." This is it, at the
SDK layer, zero core change:

- `deny_grant_ids` compiles into the registered step-11 proof
  policy: a proof whose `capability_grant.grant_id` is listed →
  `false` → `AdmissionDenied::ProviderPolicyRejected` → `0x0009`.
- v1 is a static-at-registration set plus a shared
  `Arc<RwLock<HashSet<[u8;32]>>>` handle the operator can mutate at
  runtime (`OrgServicePolicy::deny_list_handle()`), so an emergency
  deny does not require re-registration.
- Honest limits, documented where the API is: it is provider-local
  (per node, per registration), volatile, and runs after the replay
  insert — exactly the §D1 caveats. It is a mitigation, not grant
  revocation; the deferred grant-revocation store remains deferred.

### 4.4 Two policy hooks, honestly separated

The core policy seam is `Fn(&OrgCallProof) -> bool`, runs as
admission step 11 (after replay insert, before the handler), and
produces `0x0009`. Kyra's sketch wants a policy over
`(acting_org, request)`. Two hooks, distinct semantics:

- **Proof policy** (`with_proof_policy`, plus `deny_grant_ids`):
  installed into the core seam verbatim. Sees credentials, not the
  request. Denial is admission denial (`0x0009`, coarse `Denied`).
- **Request policy** (`with_request_policy`): runs inside the SDK
  handler wrapper — after admission, after decode, before the
  application handler. Sees the `Admitted` facts and the typed
  request. Denial maps to an application-level error response
  (status/shape pinned at S3 review), **not** `0x0009` — because
  `0x0009` is the admission engine's word and the SDK must not
  counterfeit it.

This keeps the closed OA-2 seam byte-untouched. Extending the core
policy signature to see request bytes is explicitly rejected for v1
(it would reopen a signed-off gate for ergonomics); revisit only
with a concrete consumer need (Q1).

---

## 5. `net_sdk::org::discovery` — verified discovery, OA-3 consumed

### 5.1 The one real core touch: promote the store queries

`ScopedDiscoveryStore`'s two queries are production-grade
(expiry-safe, floor-current, verified-at-ingest) but reachable only
through `#[doc(hidden)] *_for_test` accessors. S4 promotes them:

```rust
impl MeshNode {
    /// Verified private candidates under an installed consumer grant.
    pub fn granted_capability_providers(&self, grant_id: [u8; 32])
        -> Vec<VerifiedProviderCandidate>;
    /// Verified owner-private candidates (own org).
    pub fn owner_private_capability_providers(&self)
        -> Vec<VerifiedProviderCandidate>;
}
```

Owned snapshot records (no lock-holding borrows across `await`),
projected from `VerifiedScopedCapability`: provider `EntityId`,
`owner_org`, capability tag(s), generation, effective expiry,
provenance. The `_for_test` seams become thin aliases or are
retired. **This is a read-only projection of already-verified state
— no authority semantics move.**

### 5.2 `find_service` — an authenticated query, not envelope surgery

```rust
let candidates: Vec<VerifiedProviderCandidate> = client
    .find_service("customer.read")
    .owner(provider_org)          // optional filter/expectation
    .private_only()               // optional; default = both planes
    .await?;
```

Semantics:

- **Granted plane:** for each installed (grant, secret) pair whose
  capability matches and whose rights ⊇ DISCOVER, query
  `granted_capability_providers(grant_id)`. The full chain — outer
  signature → owner cert/currentness → audience selection → AEAD
  open → descriptor↔grant binding — already ran at ingest
  (`verify_scoped_ingest`); the SDK surfaces only its output and
  re-checks currentness through the store's floor-aware query.
- **Owner plane:** `owner_private_capability_providers()` when the
  actor's org owns this node's authority.
- **Public plane:** the plaintext fold query for the `nrpc:` tag,
  with each candidate's ownership resolved through the ingest-verified
  owner-cert projection (`owner_org_for`); candidates with no
  verified owner projection are marked `owner: None` and are never
  eligible for automatic selection in `call_service` (they cannot be
  mode-classified or grant-matched).
- Every candidate carries provenance (`Public` / `OwnerPrivate` /
  `Granted { grant_id }`) — visibility is never conflated with
  authority, and DISCOVER-derived knowledge never implies INVOKE.
- One-shot query; no watch/subscribe in v1 (matches the substrate —
  nothing watches announcements today).

Raw `ScopedCapabilityAnnouncement` codecs remain public for advanced
consumers; normal applications receive only verified candidates.

### 5.3 Audience state

`OrgClient::install_audience_secret` (§3.2) also forwards to the
existing node-side consumer registry
(`MeshNode::install_consumer_grant_audience`) so ingest can open
envelopes for that grant, and removal forwards to `remove_*`. The
SDK wallet and the node's ingest registry are kept in lockstep by
construction — one install call, both surfaces. Provider-side
audience install (`install_provider_grant_audience`) is wrapped in
`net_sdk::org::provider` for symmetric ergonomics.

---

## 6. `call_service` — the composed path

`client.call_service(service, req, opts)`:

1. Derive the capability id (§3.4 step 1).
2. `find_service(service)` across the planes the wallet can see.
3. Filter to candidates the wallet could actually call: mode-classify
   each (owner-delegated vs cross-org by owner org), require an
   INVOKE-satisfying grant relation for cross-org candidates
   (§3.2), require a resolvable direct session (E0.3).
4. Select **exactly one** provider: `opts.selector` hook if provided;
   otherwise deterministic (documented tie-break: prefer
   `Granted`/`OwnerPrivate` provenance over `Public`, then lowest
   provider `EntityId` — stable across runs; no hidden load
   balancing in v1).
5. Zero eligible → `NoAuthorizedProvider { considered }` — the count
   distinguishes "nothing discovered" from "discovered but no
   authority/session".
6. Continue as the exact-target `call` (§3.4) against the selected
   provider — pin, pre-flight, intent, one request.

`call_service` never fans out, never retries across providers on an
admission denial (a second provider seeing a fresh proof is a new
authority decision the application must own), and never falls back
from protected to public.

---

## 7. Core-touch inventory (exhaustive)

Everything in this plan is SDK-crate code except:

1. **`MeshNode` discovery accessors** (§5.1) — promote two read-only
   store queries out of `#[doc(hidden)]`. New public surface,
   no semantic change.
2. **Ownership accessor** — a public `MeshNode`-level
   `owner_org_of(node_id) -> Option<OrgId>` over the existing
   projection, if the current accessor is not already publicly
   reachable (confirm at S4; the projection itself exists —
   `capability_bridge.rs:476`).
3. **Re-exports** — `net_sdk::org::types` additions
   (`CoarseAdmissionReason`, `Admitted`, `OrgAdmission`,
   `CapabilityVisibility`, `OrgProofIntent` alias). Pure re-export.
4. **File-envelope codec relocation** (§2, Q3) — move/share, not
   change.

Explicitly untouched: `verify_org_admission` and every admission
step, the serve gates and `UnaryAdmission`, all wire objects and
headers, `RpcStatus`/coarse-reason wire shape, the replay guard,
`may_execute` (byte-for-byte re-verified at the exit gate), the RED
seam, and `OrgProofIntent` itself.

---

## 8. Slices (six bounded commits, stop-and-review per slice)

**S0 — types + errors spine.** `net_sdk::org::{types}` restructure
(compat re-exports), `OrgSdkError`, the 0x0009 coarse decode.
Witnesses: exhaustive `CoarseAdmissionReason` wire round-trip at the
SDK layer; undecodable-body fallback; module-path compat (old
`net_sdk::org::X` paths still resolve). No core diff.

**S1 — admin.** `OrgAdmin`, envelope-codec sharing, CLI rebase.
Witnesses: CLI integration tests pass unchanged in behavior
(including every secret-hygiene witness — seed never echoed, 0600
staging, no-CWD-fallback); an `OrgAdmin`-minted cert/grant/floors
file is byte-decodable by the shared codecs and verifies under the
same org id; `admin` module has no transport dependency
(compile-time witness).

**S2 — client wallet + exact-target call.** `OrgActor`,
`OrgCredentialStore`, `OrgClient::call`. Witnesses:
- intent-equality: for each admission mode, the client-built
  `OrgProofIntent` equals a hand-assembled reference field-for-field
  (all nine fields) given the same inputs;
- live accept parity: `client.call` admits end-to-end against the
  existing T1 provider setups (owner-delegated + cross-org reuse of
  `integration_nrpc_protected.rs` fixtures);
- pre-flight agreement matrix: every `PreflightReason` row maps to
  the `AdmissionDenied` variant the provider produces for the same
  defect (referencing the OA-4 T2/T3 witnesses — same predicate,
  same verdict);
- ambiguity witness: two overlapping grants → `AmbiguousCapabilityGrant`,
  resolved by `use_grant`;
- wallet hygiene: non-serializability (type-level), redacted `Debug`,
  commitment-mismatch install refusal (reuses the §2.6 relation);
- retry rule: a transport-level retry produces a fresh call id and
  fresh proof (never a byte-identical resend).

**S3 — provider typed surface.** `serve_org_typed`, `OrgCall`,
`OrgServicePolicy` (+ `deny_grant_ids`, both policy hooks, the
(admission × visibility) matrix pinned). Witnesses:
- `Admitted` five-field facts reach the typed handler; raw
  `net-org-admission` header absent (reuses the OA-4 attribution
  assertions through the new wrapper);
- request-policy veto returns the pinned application-level status
  and never `0x0009`; proof-policy veto returns `0x0009`
  (`ProviderPolicyRejected` coarse `Denied`);
- `deny_grant_ids`: listed grant denied live, unlisted admitted;
  runtime deny-list mutation takes effect without re-registration;
- no-fallback: `serve_rpc`/`serve_rpc_typed` behavior byte-unchanged
  beside a protected registration (reuses the public-caps-unchanged
  T1);
- the `org_admission == None` internal-error path is unreachable
  through the real gate (bridge-level witness).

**S4 — discovery + `call_service`.** Core accessor promotion (§7.1–2),
`find_service`, the composed `call_service`. Witnesses:
- promoted queries return only ingest-verified, unexpired,
  floor-current records (reuses the store witnesses through the new
  surface); no raw envelope type appears in any SDK return type
  (type-level scan);
- provenance labeling: the same service visible publicly and under a
  grant yields two candidates with distinct provenance;
- DISCOVER-only grant: `find_service` resolves, `call_service` →
  `MissingCapabilityGrant`/`PreflightRefused(InsufficientRights)`
  locally — mirroring the OA-4 "resolves but cannot invoke" row
  without burning a provider round-trip; the live remote-denial row
  is retained by hand-building the intent through the
  `OrgProofIntent` escape hatch (no pre-flight-bypass seam is added
  to the SDK, in test builds or otherwise);
- ownership recheck: a `Public` candidate with no verified owner
  projection is never auto-selected;
- deterministic selection witness; `NoAuthorizedProvider` count
  semantics;
- `ProviderNotDirect` on a relayed-only candidate (E0.3 surface).

**S5 — composed exit gate.** `ORG_SDK_EXIT_GATE.md` mapping every
§1–§6 claim to its witness (new or referenced OA-3/OA-4 test);
the short-path acceptance witness — a complete working example
(actor → installs → `call_service` → typed response) in ≤ 25 lines
of application code, compiled and run live two-node; a full-chain
preservation assertion (the example's call traverses
`verify_org_admission` — witnessed via the admission stamp/audit
surface, not assumed); `may_execute` byte-identity re-check; source
scan: no new `#[cfg(test)]` bypass, no new authority seam outside
the §7 inventory. Then STOP: no further org-SDK work without a named
application consumer or a measured failure.

**Gate cadence** (mirrors OA-4): per slice — new focused tests + the
touched integration target + clippy for touched targets + fmt +
diff review. After S4 (the only core-touching slice) the relevant
full lib/integration gates. After S5 the full serial battery once
(clippy `--lib --features cortex -D warnings`;
`--no-default-features`; fmt; full lib cortex;
`integration_nrpc_protected`; `org_ownership`; `org_admission_wire`;
CLI suites; `may_execute` body unchanged).

---

## 9. Deliberately NOT in v1

- **Bindings parity** (Node / Python / Go) — Rust-first, consistent
  with the substrate plan's "language parity" deferral; a separate
  plan once the Rust surface is review-frozen.
- **Watch/subscribe discovery** — one-shot queries only; nothing in
  the substrate watches announcements yet.
- **Automatic credential renewal / refresh** — issuance is offline
  and occasional by design; the SDK reports expiry, it does not
  phone home for new certs.
- **Cross-provider retry / failover on admission denial** — a fresh
  authority decision belongs to the application.
- **Grant revocation store** — remains the deferred substrate item;
  `deny_grant_ids` (§4.3) is the documented provider-local
  mitigation, not a substitute.
- **Live revocation-bundle push tooling** — admin-ops follow-up with
  its own review (§2).
- **Core policy-signature extension** (request-aware step-11 policy)
  — rejected for v1 (§4.4, Q1).
- **Streaming protected calls** — inherited unary-only boundary.
- **Any change to proof, header, status, or announcement wire
  shapes.**

---

## 10. Locked design points (proposed; frozen at review)

1. The SDK is a facade over the closed OA substrate — it never
   admits, and pre-flight can only refuse to send.
2. Pre-flight predicates are the provider's own functions, never a
   reimplementation.
3. Credential matching is exact and total (the complete authority
   relation); ambiguity is a typed error, never a silent choice.
4. Admission mode is inferred from the org relation, never
   caller-specified.
5. Secrets remain structurally non-serializable end-to-end; the
   wallet adds no serialization and no `Debug` leak.
6. No new wire surface of any kind; `0x0009` + the coarse byte are
   consumed, not extended; `0x0009` is never emitted by SDK code.
7. A signed proof is never resent; every retry is a new call id and
   new signature.
8. Discovery returns only ingest-verified candidates; provenance is
   explicit; knowledge never implies invocation authority.
9. Protected and public serve surfaces are disjoint; no fallback
   registration mode exists.
10. `OrgProofIntent` stays public, unchanged, as the escape hatch;
    everything the SDK does is expressible through it by hand.
11. `OrgAdmin` and the CLI share one set of canonical constructors
    and one set of file-envelope codecs.
12. The core-touch inventory (§7) is exhaustive; anything beyond it
    requires reopening this plan's review.

---

## Open questions

- **Q1 — request-aware policy placement.** v1 ships the two-hook
  model (§4.4: core proof policy + SDK request policy). Confirm at
  S3 review that the application-level denial status for a
  request-policy veto is acceptable, or name the consumer that
  justifies extending the core step-11 signature instead.
- **Q2 — `VerifiedProviderCandidate` shape.** Trimmed projection vs
  a clone of `VerifiedScopedCapability`; decide at S4 with the
  owned-snapshot constraint (§5.1) in hand.
- **Q3 — file-envelope codec home.** CLI → `net_sdk::org::types` vs
  core `behavior::org_files`; decide at S1 (constraint: the CLI must
  not depend on more of the SDK than `net_sdk::org`, and the SDK
  must not depend on the CLI).
- **Q4 — selection determinism vs load.** The v1 tie-break (§6.4) is
  deliberately load-blind. If a real consumer needs spreading,
  extend `opts.selector` — never the default — and witness
  determinism separately.
- **Q5 — `OrgActor` construction-time expiry.** Hard error vs
  warn-and-construct for an already-expired membership at
  `OrgActor::new` (an actor may be assembled long before use).
  Provisional: hard error, `try_new_unchecked` escape hatch;
  confirm at S2 review.
