# OA-5 — Capability sending: library wiring + SDK exposure (Status: EXPLORATORY DESIGN/ARCHIVED IN FAVOR OF ORG_CAPABILITY_SDK_PLAN)

**Version:** v1.0 (2026-07-21) — **Status: PROPOSAL — HOLD (not authorized).**
No phase below may start before OA5-Gate-0 is SIGNED OFF (Kyra). This plan
does not amend ORG_CAPABILITY_AUTH_PLAN.md; the named-consumer AUTHORIZED
entry it requests is Kyra's to write.

**Provenance.** Synthesized from three independently-designed plans
(security-sequencing, SDK/DX, library-wiring lenses), a two-judge
adversarial panel (both judges selected the security-sequencing plan as
backbone), a completeness critique, and a per-claim code verification
pass at HEAD `1830a3497`. Every load-bearing claim below was verified
against code, not doc headers (§2.3 lists two claims that were REFUTED
by that pass and are corrected here).

---

## 1. Goal

Make org-capability *sending* — a provider serving a protected
capability, an owner issuing and delivering a grant, a grantee
installing it and calling, both sides observing typed denials, and
revocation being applied — a first-class feature of the library and its
SDK surfaces, instead of a core-only seam reachable via deep paths.

Two halves, matching the user goal:

1. **Wire into the rest of the library** — the layers that serve unary
   nRPC (aggregator, cortex tools, payments negotiation, dataforts blob
   control plane) can register protected through one shared seam, with
   operational visibility (metrics/audit) landing before any layer flips.
2. **Expose as an SDK** — Rust first (smallest releasable unit), then —
   under its own explicit authorization, since language parity is
   "Deliberately NOT in v1" in the master plan — golden vectors, the C
   ABI, and the Go/Python/Node surfaces, with signing and secrets
   staying Rust-side.

## 2. Baseline at HEAD `1830a3497`

### 2.1 Status corrections (load-bearing)

- **OA-3 and OA-4 are CLOSED**, per ORG_CAPABILITY_AUTH_PLAN.md v1.4,
  OA3_EXIT_GATE.md, OA4_EXIT_GATE.md, and commit `347860feb`. The
  status header of OA2E_INTEGRATION_DESIGN.md ("OA-3 not started",
  2026-07-19) is **stale** and needs a dated correction callout (Phase 0).
  Consequence: `serve_rpc_granted` / `serve_rpc_owner_scoped` exist and
  are live-wired; "protected requires visibility==Public" was only the
  pre-OA-3 cut. **Unary-only and direct-session-only remain true.**
- **PASS2's standing verdict is "Do not sign off"** and its remediation
  table is stale vs HEAD (§1–§5, §7–§11 fixes landed after its last
  edit). Open residuals this plan sits on: Medium **§6** (unmetered
  signature verification for zero-credential callers on the protected
  bridge), **§12/§13** (Windows authority-dir ancestor chain;
  durability/poison inert), **§14** (no cryptographic cross-org grant
  revocation — expiry/local-uninstall/provider-policy only), SDK-facing
  lows **§22, §26, §27/§28/§29, §34**, test-quality **§T4–§T9**, CI
  **§C2/§C3**; accepted-limitation candidates §5-residual, §23.

### 2.2 What exists (verified)

- **Core, provider side:** `MeshNode::serve_rpc_protected` /
  `serve_rpc_owner_scoped` / `serve_rpc_granted` (mesh_rpc.rs:3072 /
  3139 / 3170), requiring `install_node_authority` (mesh.rs:7877) +
  `install_org_revocation_store` (:7642); admission via
  `admit_and_dispatch_protected` → `verify_provider_authority` →
  `verify_org_admission`; handlers read the four-party `Admitted` via
  `RpcContext::org_admission`; denials emit `RpcStatus::AdmissionDenied
  0x0009` with the coarse reason as **one body byte**
  (mesh_rpc.rs:866-868).
- **Core, caller side:** `CallOptions.org_proof_intent`
  (`OrgProofIntent`, mesh_rpc.rs:231-253 — 9 fields including
  `caller: Arc<EntityKeypair>`); `call()` mints the `OrgCallProof` over
  the shared digest and appends exactly one `net-org-admission` header
  (reject-if-present / append-exactly-one, mesh_rpc.rs:5027/5032).
- **Grant lifecycle:** issuance types in `behavior/org_grant.rs` (wire
  sizes const-pinned: cert 156B org.rs:1039, dispatcher 185B
  org_grant.rs:1185, capability 318B :1186; proof ≤1024B
  `MAX_ORG_CALL_PROOF_BYTES`); `OrgAudienceSecret` is structurally
  non-serializable (compile assert org_grant.rs:281-294) and zeroized on
  drop (:417-428). Delivery is **out-of-band files only** (CLI
  `net org grant-dispatcher` / `grant-capability`, 0600 secrets).
  Install/remove is **strictly in-process** (`MeshNode::
  install_provider_grant_audience` mesh.rs:8063 / `_consumer_` :8144;
  `OrgRevocationStore::apply_bundle`).
- **SDK today:** `net_sdk::org` re-exports offline credential
  primitives + `CallOptions.org_proof_intent` is reachable. No
  protected/granted serve wrapper, no `OrgAdmission`/`OrgProviderPolicy`
  re-export, no credential loader, no builder, no typed denial. The
  MeshBuilder cannot install a node authority.
- **Non-Rust surfaces (repo-root `go/` + `bindings/go/net`, `include/`,
  `bindings/python`, `bindings/node`, `sdk-py`, `sdk-ts`):** zero org
  symbols. Established parity machinery: golden-vector fixtures
  (`tests/cross_lang_capability_fixtures.rs`, payments/consent vectors),
  `header_parity_test.go`, `abi_stability_test.go`.

### 2.3 Claims REFUTED by the verification pass

- **There is no enforced `may_execute` freeze.** The byte-identity sha
  `f68895147478a11d` appears only in the PASS2 review doc (:1367), not
  in any test. The freeze the invariants depend on is currently
  *folklore*. Phase 0 adds a real, CI-run freeze witness.
- **There is no out-of-process admin path** to a running node's grant
  or revocation stores (no socket/watch/reload; CLI is offline
  authoring only, cli/src/commands/org.rs:5-8). Any "runtime install
  via CLI" design is unbuildable today; install/apply stays SDK
  (in-process), and a mesh admin channel is deferred (§6 register).

### 2.4 Other verified facts the design leans on

- Caller-side 0x0009 today falls through to generic
  `RpcError::ServerError { status, message, .. }` with the coarse byte
  UTF-8-stuffed into `message` (mesh_rpc.rs:5299, 5314-5324). Core
  `RpcError` (mesh_rpc.rs:284) is **NOT** `#[non_exhaustive]` and has
  no `AdmissionDenied` variant — adding one is semver-breaking.
  `SdkError` IS `#[non_exhaustive]` (sdk/src/error.rs:16) — safe.
- The CLI envelope structs `OrgDispatcherGrantFile` /
  `OrgCapabilityGrantFile` (cli org.rs:753/763) are `pub(crate)`,
  already versioned (`version: u32` = `ORG_FILE_VERSION`,
  `deny_unknown_fields`).
- The `nrpc:<service>` join key is built inline via
  `format!("nrpc:{service}")` at ~8 sites (serve bridges, proof,
  grant/announce) with **no shared helper**.
- Mixed fleet, case (b): a public/legacy registration **strips** a
  stray `net-org-admission` header and serves the call as public
  (`strip_public_admission_header`, mesh_rpc.rs:931-970, applied :774)
  — the caller's protection expectation is silently dropped (Q5).
- Payments: quotes are issued at discovery (`MeshPaymentChannel::quote`,
  payments/src/flow/mesh.rs:168) and `serve_tool_paid`'s wrapper is
  decode→redeem→run→encode with **no org-admission gate anywhere ahead
  of it** — an unadmitted caller can be quoted today. The Phase 4
  composition red witness is demonstrably red.
- Metrics precedent: per-service `capability_denied_total`
  (mesh_rpc_metrics.rs:118, exposition :680-689) — note it meters the
  legacy capability gate, distinct from org admission.
- `#[tool]` (sdk-macros) offers no protection/paid options; generated
  registration calls `serve_tool` only.
- ABI version mirrors (drift already present): source of truth
  `NET_RPC_ABI_VERSION = 0x0004` (bindings/go/rpc-ffi/src/lib.rs:181 +
  self-test :4163); `go/mesh_rpc.go:685` + typed test; duplicate tree
  `bindings/go/net/mesh_rpc.go` + test; **stale** `0x0002` in
  include/net_rpc.h:106; **stale** `0x0001` in
  tests/integration_nrpc_cross_lang.rs:235-236 and
  .claude/skills/net-event-bus/nrpc.md:263,326.
- Windows CI exists: job `windows-security-tests` (ci.yml:1701) — the
  Windows DACL witnesses in Phases 1–2 are satisfiable.

## 3. Invariants (pinned — inherited, this plan may not move them)

1. `may_execute` (behavior/fold/capability_bridge.rs) byte-for-byte
   unchanged; org admission never consults it; protected authority
   routes through `OrgAdmission`/`has_local_capability` only.
2. Fail-closed everywhere: no unknown-policy fallback; missing/bad
   authority, proof, or store ⇒ DENY; a handler never runs unverified.
3. Unary-only; streaming/duplex protected registration or a streaming
   flag on a protected request fails loud.
4. Direct-session-only in v1; relayed protected requests loudly denied.
5. Replay-before-handler, stability-before-replay (§9.5 stamp); one
   handler run per (caller, call_id); one clock sample per admission.
6. Membership ≠ invocation authority; discovery/decryption never
   confers invocation; fold state is never admission evidence;
   provider-local policy is final and runs last.
7. Secrets (`OrgAudienceSecret`, owner audience key) are structurally
   non-serializable and never cross a language boundary as bytes.
8. Coarse 3-bucket reasons on the wire; detailed `AdmissionDenied`
   stays provider-side.
9. OwnerScoped/GrantedAudience announcements are ENCRYPTED-only (Kyra
   ruling); the plaintext `scope:*` tags of SCOPED_CAPABILITIES_PLAN.md
   are a different, non-confidential feature.
10. Witness discipline: OA-4 evidence tiers (T1 real two-node
    transport / T2 bridge frames via `deliver_rpc_inbound_for_test` /
    T3 pure units); every fix red-witnessed, per-item commits; each
    phase ends "Stop. Review." with Kyra.

## 4. Locked design points (proposed — ratified at OA5-Gate-0)

1. **Envelope module lives in CORE** (`behavior/org_envelope.rs`):
   promote the already-versioned CLI file shapes; the CLI is rewritten
   to consume it (one schema codepath, grep-gated). Core placement is
   forced by Phases 5–6: bindings must reach it without `net-sdk`.
2. **`OrgProofIntentBuilder` lives in CORE** (mesh_rpc.rs), re-exported
   by the SDK — same reason. `build()` is fail-loud BEFORE signing:
   dispatcher scope must cover the derived capability; `acting_org`
   must equal the membership org (today a doomed intent signs and
   discloses a credential on-wire — red witness).
3. **Join-key helper in core**: `CapabilityAuthorityId::for_service()`
   (or `nrpc_capability_tag(service)`) replaces the ~8 inline
   `format!("nrpc:{service}")` sites, non-behaviorally (T3 golden pins
   the literal), so SDK/FFI/vectors share one derivation.
4. **Typed denial v1 is SDK-side**: classify
   `ServerError{status:0x0009}` by decoding the coarse **body byte**
   (`CoarseAdmissionReason::from_wire` on `message.as_bytes()[0]` /
   the preserved body — never string-parsing). A core
   `RpcError::AdmissionDenied` variant is a separate ruled decision
   (Q3): `RpcError` is not `#[non_exhaustive]`, so that change is
   breaking and rides the same gate as the ABI bump if taken.
5. **Custody:** org ROOT issuance does NOT cross FFI in v1 (stays
   CLI/Rust — ExternalSigner precedent); grants/certs cross as opaque
   bytes; audience secrets cross as **file paths only**, read by the
   hardened Rust loader; all signing executes Rust-side against
   existing opaque identity handles.
6. **FFI provider policy is declarative-only in v1**: an acting-org
   allow-list serialized across the boundary, evaluated Rust-side. No
   cross-boundary closure in the gate path (blocking-FFI DoS +
   §9.5-ordering hazard); full policy trampoline deferred (Q6).
7. **Everything default-off**: a fleet that upgrades with zero config
   is wire-identical (golden regression re-run at every gate).
8. **Admission strictly before payment**: an unadmitted caller of an
   org-protected paid tool receives 0x0009 and never a quote header or
   `0x8006`; pinned from both sides (Phase 4).
9. **Discovery-plane consumption (placement/scheduler merging
   org_scoped_store) is OUT of this track** — deferred with its own
   gate design (§6 register): it borders frozen
   `may_execute`/`target_matches_filter`.
10. **One registration seam**: `ServeProtection` enum (Public |
    Protected{admission, policy} | OwnerScoped{policy} |
    Granted{policy}) as pure dispatch sugar mapping 1:1 onto the four
    existing constructors; the Public arm is wire- and gate-identical
    (regression pin); the Protected arm preserves the existing loud
    rejection of `PublicAuthenticated`; streaming shapes cannot be
    registered protected.

---

## 5. Phase plan

Phases are individually releasable; each exit gate is a "Stop. Review."
with Kyra. Cross-language work (Phases 5–7) additionally requires its
own AUTHORIZED entry, because language parity is verbatim in the master
plan's "Deliberately NOT in v1".

### Phase 0 — Authorization, residual disposition, freeze witness (HOLD)

Goal: lift/scoped-supersede "Do not sign off" for these seams; make the
frozen gate actually frozen; correct the stale record. Docs + two small
witnesses; no surface work.

1. Request the named-consumer AUTHORIZED entry: "OA-5 capability SDK +
   library wiring" as the consumer required by the master plan's
   "Then STOP" rule (Kyra writes it; this doc only requests it).
2. **Residual disposition table** (Kyra rules each: FIX-FIRST /
   ACCEPTED / SCHEDULED-into-phase — the reviewer, not the planner,
   sets severity). Proposed dispositions:
   - **§6** signature-DoS: FIX before Phase 1 *exit* (it sits directly
     under the surface Phase 1 advertises). Witness: credential-less
     flood trips the pre-verify meter; verify-count instrumentation
     shows zero ed25519 work bought.
   - **§12/§13** Windows authority-dir: FIX-or-ACCEPT, ruled here;
     blocks Phase 2's Windows loader witnesses if accepted-with-caveat.
   - **§14** no cross-org revocation lever: ACCEPTED limitation ruling
     + mandatory restatement on every doc surface (Phase 7 doc lint),
     or a blocker demanding a grant-id floor design first (Q4).
   - **§22** (`seal_descriptor_with_nonce` pub) + **§26** (zero
     grant_id accepted): SCHEDULED as Phase 1's first two red-witnessed
     commits. **§27/§28/§29** (no hardened loader): closed BY the
     Phase 2 loader (the closing artifact is Phase 2's, witnessed at
     that gate — not claimed closed here).
   - **§T4–§T9**: SCHEDULED alongside Phase 1/2 witness work.
     **§C2/§C3**: disposition with the CI facts (§2.4: a Windows lane
     exists).
3. **Real `may_execute` freeze witness** (closes the §2.3 refutation):
   a CI-run test that extracts and hashes the `may_execute` fn source
   (and `ORG_RPC_REQUEST_DIGEST_CONTEXT` + the golden digest literal),
   pinned; every later exit gate re-runs it instead of citing the
   review doc's sha.
4. Doc corrections per retraction culture (dated callouts, no silent
   edits): OA2E_INTEGRATION_DESIGN.md status header (OA-3/OA-4 are
   CLOSED); PASS2 remediation table refreshed to HEAD.

Exit gate **OA5-Gate-0**: disposition table signed; freeze witness
green in CI; corrections landed; AUTHORIZED entry recorded with commit
hash. Stop. Review.

### Phase 1 — Rust SDK surface: the smallest releasable unit

Goal: an SDK consumer can stand up an org-owned node, serve a
protected/owner-scoped/granted **typed** service, and make a protected
typed call — zero deep-path imports, zero wire change.

1. §22 + §26 fixes land first (red-witnessed, per Phase 0 schedule).
2. `MeshBuilder::node_authority_dir(path)` forwarding
   `MeshNodeConfig::with_node_authority_dir` (mesh.rs:2080) — without
   it, SDK-built nodes can never hold authority.
3. `net_sdk::org` re-exports: `OrgAdmission`, `Admitted`,
   `CoarseAdmissionReason`, `OrgProviderPolicy`,
   `CapabilityVisibility`; the core join-key helper (design pt. 3).
   NOT re-exported: `verify_org_admission`, `org_request_digest`.
4. Typed protected serving mirroring `serve_rpc_typed`:
   `Mesh::serve_rpc_protected_typed` / `serve_rpc_owner_scoped_typed` /
   `serve_rpc_granted_typed` (same Codec/ServeHandle RAII plumbing,
   delegating to the three core entry points); handler receives the
   `Admitted` attribution. No streaming variants (invariant 3).
5. `OrgProofIntentBuilder` in core + SDK re-export (design pt. 2),
   with the fail-loud pre-signing validation and
   `for_service(name)` deriving the join key once.
6. Typed denial, SDK-side (design pt. 4): `SdkError::AdmissionDenied
   { coarse_reason }` + a classifier decoding the coarse body byte.
7. In-process runtime veneers: `Mesh::install_provider_grant_audience`
   / `install_consumer_grant_audience` / `remove_*` /
   `apply_revocation_bundle` (§2.3: in-process is the only buildable
   shape).
8. Example `sdk/examples/nrpc_protected_echo.rs` (replaces the §28
   unhygienic example), compiled+run in CI.

Witnesses (all red-first): builder-knob (no authority-dir ⇒
`ProtectedAuthorityRequired`; with it, green); T1 two-node admit with
attribution + denial-with-handler-darkness; typed-denial red (today:
generic ServerError, no reason); intent-builder scope-mismatch rejects
before signing (today: signs then denied on-wire); join-key golden;
example green with a grep gate: zero `net::adapter` deep paths.

Exit gate **OA5-Gate-1**: sdk+core suites green; freeze witness green;
§6 CLOSED (Phase 0 disposition); no wire-const diff; releasable alone.
Stop. Review.

### Phase 2 — Grant envelope + credential store (lifecycle DX)

Goal: ONE versioned envelope schema shared by CLI and SDK; hardened
loading; the retirement half gets minimum UX. Delivery remains
out-of-band in v1 (`OrgAudienceSecret` non-serializability is the
reason — a mesh grant-inbox is a deliberate carve-out, deferred, Q7).

1. Promote the versioned CLI file shapes into core
   `behavior/org_envelope.rs` (design pt. 1); rewrite the CLI to
   consume it; grep-gate against schema duplication.
2. `net_sdk::org::OrgCredentialStore::open(dir)`: loads
   membership/dispatcher/capability envelopes + audience secrets;
   enforces 0600/Windows-DACL; zeroizes intermediates; rejects zero
   grant_id; hands out secrets only via the non-serializable type.
   This is the closing artifact for §27/§28/§29.
3. Sprawl minimum: read-only accessors listing installed
   provider/consumer grant audiences (+ expiries) on a node, and CLI
   `net org verify-grant` / `show-grant` (offline, via the shared
   envelope module). No runtime-install CLI (§2.3).
4. T1 lifecycle witness: issue → deliver (file) → `OrgCredentialStore`
   → install → granted call admits → `remove_consumer_grant_audience`
   darkens; floors applied via `apply_revocation_bundle` darken a
   stale member (monotone floor witness).

Witnesses: loader refuses world-readable/bad-DACL secret (red against
today's `decode_config`-only path); envelope round-trip CLI↔SDK pin;
zero-grant_id reject; Windows lane runs the DACL witnesses
(windows-security-tests).

Exit gate **OA5-Gate-2**: one schema, two consumers, zero duplication;
§27/§28/§29 closed here with witnesses; Unix+Windows green. Stop. Review.

### Phase 3 — Operational substrate (before any consumer flips)

Goal: operators can watch a rollout before Phase 4 flips anything.
Wire stays byte-identical.

1. Provider metrics in mesh_rpc_metrics.rs mirroring the
   `capability_denied_total` pattern: per-service
   `admission_admitted_total`, `admission_denied_total` +
   per-coarse-reason counters; Prometheus exposition
   `nrpc_admission_denied_total{service,reason}`.
2. Caller-side denial counters (the consumer-side blind spot): count
   typed AdmissionDenied in the SDK call path metrics.
3. Provider-local audit record at the deny site: the DETAILED
   `AdmissionDenied` variant + caller + call_id + service + clock
   sample — with a byte-scan witness that the wire response still
   carries only the coarse byte.

Witnesses: one denial per bucket increments the right counter (T2
bridge); detailed-locally/coarse-on-wire byte-scan; defaults-off
wire-identity regression.

Exit gate **OA5-Gate-3**: counters visible in the integration harness;
freeze witness green. Stop. Review.

### Phase 4 — Library wiring: one seam, one flagship, composition pins

Goal: the "rest of the library" half — one shared registration seam,
one real consumer flipped (opt-in, default-off), and the
admission/payment composition contract pinned.

1. `ServeProtection` enum in core (design pt. 10) mapping 1:1 onto the
   four constructors; SDK + tool layer accept it.
2. Flagship consumer (Kyra picks, Q8; recommendation: aggregator
   `registry_service` — unary control plane, lowest blast radius):
   config-driven `protection: ServeProtection`, default Public;
   default behavior byte-identical.
3. Composition contract (design pt. 8), both directions pinned:
   unadmitted → 0x0009 with byte-scan-verified absence of
   `HDR_PAYMENT_QUOTE` and `0x8006` (demonstrably red today, §2.4);
   admitted-but-unpaid → `0x8006` still enforced. Billing attribution
   (`acting_org`/`grant_id` on billing events) is scoped OUT to a
   payments-side gate (it touches cross_lang_payments fixtures).
4. `#[tool]` macro: add a `protection` arg routing to the seam (or an
   explicit documented exclusion this cycle) — today the macro can
   only register public (§2.4).
5. Mixed-fleet matrix as named tests: (a) legacy caller vs protected
   provider → coarse 0x0009; (b) intent-bearing caller vs public
   provider → **pinned to the verified current behavior**
   (header stripped, served public — §2.4) + Q5 ruling on whether v1
   needs a caller-side guard; (c) relayed → loud deny (re-run);
   (d) `no_silent_downgrade_on_admission_denied` (no SDK helper ever
   retries without protection); defaults-off fleet golden regression.
6. Replay-quota headroom note recorded: any future chunky adopter
   (dataforts blob) must measure `AdmissionReplayGuard` per-caller
   quota headroom at the pinned transfer size before flipping
   (deferred adopter, §6 register).

Exit gate **OA5-Gate-4**: flagship protected in an opt-in two-node
test; composition + fleet witnesses green; scope-outs recorded.
Stop. Review.

### Phase 5 — Cross-language contract + C ABI (HOLD — new authorization)

Gate-in: an explicit AUTHORIZED entry amending "Deliberately NOT in
v1" language parity. Rust-only (Phases 0–4) is a legitimate stopping
point if withheld (Q2).

1. `tests/cross_lang_org/` golden vectors + Rust reference test
   (`cross_lang_org_fixtures.rs`), frozen BEFORE any binding code:
   grant vectors (156/185/318B + the three sig-domain strings), BLAKE3
   commitment vectors, digest vectors (header-strip, order,
   multiplicity), proof vectors (≤1024B, exactly-one-header), denial
   vectors (0x0009 + coarse byte + stable reason names), a
   schema-version field bound to `ORG_RPC_REQUEST_DIGEST_CONTEXT`.
   Non-tautology reds: mutate a sig-domain string in a scratch build ⇒
   reference test fails; a two-admission-header fixture must FAIL.
2. C ABI, additive, custody per design pt. 5 — extending the EXISTING
   header pairs (no `net_org.h`; no issuance surface in v1):
   `net_mesh_set_node_authority_dir`,
   `net_mesh_install_provider/consumer_grant_audience(grant_bytes,
   secret_PATH)`, `net_mesh_apply_org_revocation_bundle`;
   `net_rpc_serve_protected(service, admission_mode,
   allow_orgs_declarative)`, `net_rpc_org_intent_new(identity_handle,
   …) → opaque handle` (the intent holds a keypair — it can NEVER
   cross as a buffer), a call-with-intent entry,
   `NET_RPC_ERR_ADMISSION_DENIED` with wire string
   `admission_denied: reason=<coarse>`.
3. ABI bump `0x0004 → 0x0005` (the error contract is breaking), fixing
   ALL mirrors enumerated in §2.4 in the same commit series: rpc-ffi
   lib.rs + self-test, both Go trees + typed tests, the stale `0x0002`
   header text, the stale `0x0001` cross-lang fixture pin, the stale
   skill doc. Release-notes doc per repo convention (tracks every ABI
   bump).
4. Fuzz targets for the new hostile-input parsers: envelope JSON,
   `OrgCallProof::decode`, admission-header parse, FFI intent-handle
   argument validation, coarse-byte decode (fuzz/ precedent).

Witnesses: golden vectors committed before ABI code;
`header_parity_test.go` red until both headers match;
`abi_stability_test.go` pins the new prefix + 0x0005; secret-custody
byte-scan: no audience-secret bytes in any FFI buffer (path-only).

Exit gate **OA5-Gate-5**: vectors signed as the frozen contract (fixture
changes now require wire-change ceremony); ABI reviewed additive-plus-
versioned-error; mirrors enumerated-and-bumped. Stop. Review.

### Phase 6 — Go, Python (PyO3), Node (NAPI) bindings

1. Go: `ServeProtected`, `OrgProofIntent` config struct (opaque bytes,
   PermissionToken precedent), `RpcKindAdmissionDenied`,
   `ExpectedABIVersion 0x0005` — in BOTH Go trees; golden-vector test
   per the consent-vectors pattern.
2. Python: PyO3 org surface (loader-backed install, intent builder,
   `serve_protected` with declarative policy, `org_proof_intent` kwarg;
   sync, GIL released) + `AdmissionDenied` error subclass.
3. Node: NAPI surface (`serveProtected`, `orgProofIntent` call option,
   Promise/AbortSignal preserved) + `RpcAdmissionDeniedError`.
4. Handler attribution: a minimal read-only marshaled `Admitted` view
   (acting org, caller, mode, grant id) — no authority objects cross.
5. Custody witnesses, honestly scoped for GC runtimes: heap byte-scans
   are NOT implementable in CPython/V8 — the witness is API-shaped
   (no binding API can return secret/seed material; secrets enter only
   as paths) + the Rust-side scans from Phase 5.
6. CI: per-binding jobs consuming the shared fixtures (enumerated in
   the phase PR, not assumed).

Witnesses: per-language golden-vector byte-identity reds; per-language
live T1 (protected serve, typed denial, handler darkness); Go
`CheckABI` panic against a 0x0004 library.

Exit gate **OA5-Gate-6**: three bindings green on shared fixtures +
live witnesses in CI; per-binding custody checklist reviewed.
Stop. Review.

### Phase 7 — Wrappers, docs, packaging, exit gate, STOP

1. `sdk-ts` org wrapper; `sdk-py` decision per
   SDK_PYTHON_PARITY_PLAN.md (org above a missing nRPC re-export is
   inconsistent — coordinate, don't fork; Q9).
2. Docs with a MANDATED limitation box on every surface (lint-tested,
   mirroring load-bearing-doc culture): unary-only;
   direct-session-only; **no cryptographic cross-org grant revocation
   (§14)** — expiry ≤30d / local uninstall / provider policy only;
   known-target routing; 30s proof TTL ⇒ NTP/skew guidance; a
   "what is NOT protected" matrix (pub/sub channels, meshdb transport,
   streams, relays). Plus: key/authority lifecycle runbook (root
   rotation, owner-cert expiry mid-serve, leaked-secret/re-shared-
   envelope incident response — bearer-semantics threat stated, Q10);
   skill updates (net-event-bus ABI + protected variants;
   net-payments composition contract).
3. Packaging/release: crates.io `net-mesh-sdk` bump (CLI pins
   path+version), PyPI wheel + floor bump, npm prebuilds, Go module
   tags for both trees, coordinated cdylib rollout notes for 0x0005,
   the release-notes doc.
4. Polyglot lifecycle demo (T1): Rust provider; Rust/Go/Python/TS
   callers each do load → install → call → typed denial after floors.
5. `docs/plans/OA5_EXIT_GATE.md` in the OA3/OA4 style: requirement →
   tier (T1/T2/T3) → exact test name; frozen-contract register
   (fixtures, ABI 0x0005, freeze witness).
6. **Adversarial review pass** over the new SDK/FFI surface (the
   PASS1/PASS2 multi-reader instrument, not only per-gate sign-offs —
   the C ABI is forever), then re-arm the master plan's "Then STOP"
   rule.

Exit gate **OA5-Gate-7**: exit-gate doc rows all green; adversarial
pass filed; Kyra final sign-off; STOP re-armed.

---

## 6. Deliberately NOT in v1 (deferred register — each with the reason)

- **Discovery-plane merge** (placement/compute scheduler consuming
  `org_scoped_store`): borders frozen `may_execute` /
  `target_matches_filter`; needs its own gate + Kyra invariant ruling
  ("discovery never confers invocation"), default-off, with
  non-audience byte-scan and discovered-but-unproven-still-denied
  witnesses, and verification that the §9 sweep runs on the query path.
- **meshdb**: bespoke `MeshDbWireTransport`, not `serve_rpc` — bespoke
  admission wiring is its own work item.
- **MCP gateway composition** (CapabilityGateway/PaymentAdmission
  ordering vs org admission): unguarded seam until its own plan.
- **Streaming/duplex protection; relay-traversal protected calls**
  (end-to-end identity through a relay is an E2+ evolution).
- **Mesh-native grant delivery (grant-inbox) + any out-of-process admin
  channel**: requires a deliberate carve-out to secret
  non-serializability / a new admin surface (Q7).
- **Cross-org grant revocation lever (§14)**: grant-id floors in
  `OrgRevocationBundle` or equivalent — needs its own design.
- **Admission-confirmed responses**: a caller cannot today verify the
  provider actually enforced admission (see Q5).
- **FFI policy trampoline** (full closure policies over FFI): Q6.
- **Billing attribution schema** (`acting_org`/`grant_id` on billing
  events): payments-side gate (cross_lang_payments fixtures).
- **Replay-quota re-sizing for chunky adopters** (dataforts blob):
  measure-first precondition recorded in Phase 4.
- **Grant-sprawl tooling beyond Phase 2's minimum** (expiring-grant
  surfacing, bulk uninstall).

## 7. Open questions (Q1..Q10 — Kyra)

- **Q1** Named-consumer semantics: does SDK exposure itself satisfy the
  "named consumer" rule, or does the Phase 4 flagship? (Plan works
  either way; determines what Gate-0 records.)
- **Q2** Is Rust-only (Phases 0–4) the intended stopping point this
  cycle, with Phases 5–7 a later AUTHORIZED entry?
- **Q3** Typed denial home: SDK classifier only (v1 default here), or
  also a core `RpcError::AdmissionDenied` variant — breaking (enum not
  `#[non_exhaustive]`); if taken, does it ride the ABI-0x0005 gate,
  and should `RpcError` become `#[non_exhaustive]` at the same moment?
- **Q4** §14 ruling: accepted-and-documented limitation, or blocker
  requiring a grant-revocation lever before Phase 5 widens the
  audience?
- **Q5** Mixed-fleet case (b) is now verified: a public provider
  strips the admission header and serves as public — the caller's
  protection expectation silently drops. Pin as-is only, or add a
  caller-side guard (e.g. a `CallOptions` flag failing the call when
  the provider cannot prove admission — which needs the deferred
  admission-confirmed response)?
- **Q6** FFI provider policy: is declarative-only acceptable for v1
  (this plan's default), or is the closure trampoline required for
  parity (new dispatcher type + §9.5-ordering re-review)?
- **Q7** Confirm deferral of the mesh-native grant-inbox / admin
  channel (the non-serializability carve-out) to OA-6.
- **Q8** Flagship consumer: aggregator `registry_service`
  (recommended), `query_service`, or dataforts blob? Opt-in config
  per-service or node-wide?
- **Q9** sdk-py sequencing: land org alongside a minimal nRPC
  re-export (pulling parity-plan work forward), or document Python org
  at the native binding level for now?
- **Q10** Bearer semantics of delivered grant files (a grantee can
  re-share envelope + secret): accepted limitation with documented
  blast radius, or bind installs to grantee identity in the envelope
  (schema change — decide before Phase 5 freezes vectors)?

## 8. Risk register

- Building on an unsigned seam: PASS2's "Do not sign off" stands until
  Gate-0; §6 sits directly under the advertised surface (hence its
  FIX-before-Gate-1 disposition).
- §14 amplification: the SDK makes cross-org granting one call while
  revocation stays expiry-only — the limitation box + short default
  TTLs are load-bearing, not cosmetic.
- Envelope promotion freezes a de-facto five-language wire contract —
  mitigated by the existing `version` field + vectors landing before
  any binding.
- ABI drift is already real (0x0002/0x0001 stragglers); the Phase 5
  mirror enumeration is the fix, and any missed mirror is silent
  drift.
- Doc-state skew is endemic (three contradictory status surfaces
  found); every gate re-verifies against git/code, not headers, and
  corrections are dated callouts.
- GC-runtime custody cannot be witnessed by byte-scan; the API-shaped
  witness must be reviewed as sufficient (Phase 6) or the binding
  scope re-cut.
- Freeze-by-folklore: until Phase 0's witness lands, nothing enforces
  the `may_execute` freeze — treat any pre-Gate-0 refactor near
  capability_bridge.rs as hazardous.

## 9. Gate ledger

| Gate | Scope | State |
|---|---|---|
| OA5-Gate-0 | Authorization + disposition + freeze witness + doc corrections | HOLD |
| OA5-Gate-1 | Rust SDK smallest releasable unit (§6 closed) | HOLD |
| OA5-Gate-2 | Envelope + credential store (§27/§28/§29 closed) | HOLD |
| OA5-Gate-3 | Operational substrate | HOLD |
| OA5-Gate-4 | Library seam + flagship + composition pins | HOLD |
| OA5-Gate-5 | Cross-lang vectors + C ABI 0x0005 (needs new AUTHORIZED entry) | HOLD |
| OA5-Gate-6 | Go/Python/Node bindings | HOLD |
| OA5-Gate-7 | Wrappers, docs, packaging, adversarial pass, STOP | HOLD |
