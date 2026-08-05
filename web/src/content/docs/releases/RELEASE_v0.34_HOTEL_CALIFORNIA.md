---
title: "v0.34.0 — Hotel California"
description: "Release notes for Net v0.34.0 — Hotel California — what shipped, what changed, and what it means for compatibility."
---
# Net v0.34 — "Hotel California"

*Named after the Eagles' 1976 track, whose closing line — "you can check out any time you like, but you can never leave" — is usually read as a trap. This release reads it as a guarantee. v0.34 is about boundaries that admit deliberately and never leak: a capability announced into an owner-scoped audience is sealed to that audience and never appears in a plaintext announcement, so an unauthorized caller isn't refused — it never learns the service exists. A node placed inside a subnet is placed, not authorized: topology says where you are, signed grants say what you may do, and an exported service crosses the boundary through exactly one provider-local binding that is revalidated on every call. You can check in. What's inside never checks out.*

Six tracks land, and for the first time the center of gravity is the authority layer rather than the network layer:

- **Organization capability auth** — company identity as a first-class plane. An offline Ed25519 org root signs membership certificates, dispatcher grants, and capability grants; a protected nRPC service verifies a per-call proof that binds the exact provider, capability, request digest, call identity, and validity window. The confidential form of a capability announcement rides a new encrypted subprotocol (`0x0C04`) and is never emitted in plaintext.
- **Subnet authority — topology is not authority** — hierarchical transport admission, full-`EntityId` session proof, bounded routing/export authority, session-compiled enforcement, and signed revocation distribution. The self-declared `subnet:` / `group:` tags that used to admit an inbound call no longer do — a clean break, because the provider broadcast those values in cleartext and any observer could claim them with one `add_tag`.
- **Two verbs per plane, in five languages** — `serve_org` / `org.call` and `serve_subnet_exported` / `call_exported`, mirrored across Rust, TypeScript/Node, Python, Go, and C, plus offline `net-mesh org` and `net-mesh subnet` provisioning ceremonies. No language gets a way to put a discovery key in garbage-collected memory.
- **The money path, audited** — a full-surface security pass over `net-payments` went out at **HOLD** with three disqualifying findings and came back **RESOLVED**: quote issuance now authenticates the caller, the shipped provider bindings can no longer run without a real settlement backend, and the independent verification path can no longer be pointed at cleartext HTTP.
- **Four audits and a benchmark program** — channel auth, scoped capabilities, gang-scheduler and payments hot paths, subnet paths; plus the CPB, ICB, and payments benchmark suites, each of which publishes what its numbers *do not* mean as carefully as what they do.
- **The polyglot lens** — the docs stop being "documentation of a Rust system with bindings" and become documentation of a multilingual protocol, composed from one canonical operation model rather than five authored copies. And the project moves to **MIT OR Apache-2.0**.

The organizing observation follows the last two cycles and inverts them. v0.32 was *a fast path layered over a correctness path that never moves*; v0.33 was *one shared answer layered over an authority that never moves*. v0.34 is **a boundary layered over an identity that never travels.** Every plane added this cycle separates *where you are* and *who you belong to* from *what you may do*, and gives each question its own signed artifact: membership is not invocation authority, a dispatcher grant is not a provider's consent, a subnet coordinate is not permission to route, and a named export is not a discovery mode. The provider stays the final authority — again. What is new is what happens to the caller who has none: on the private plane it does not receive a denial, because a denial is information. It receives nothing at all.

---

## Organizations — the capability that isn't there unless it's yours

A transport session proves a peer identity. It does not prove company membership, and it does not prove permission to invoke. The **organization capability auth** track (OA-1 … OA-5) supplies that missing relation without requiring every participant to share one cloud account, cluster, or control plane.

- **One offline root, three artifacts, three different facts.** An organization is an Ed25519 root key designed to stay offline; nodes consume signed artifacts, never the signing key. A **membership certificate** proves one exact entity belongs to the org — and proves nothing about invocation. A **dispatcher grant** proves an entity may act for the org over a bounded capability scope. A **capability grant** proves a provider org has granted another org explicit rights over a capability and provider scope. The credentials make a valid proof *constructible*; the provider's live verification makes it *accepted*.
- **Ownership, then admission.** OA-1 lands the ownership plane — certificates, revocation floors, the `adopt` ceremony, the fold projection, and the `MeshNode` install lifecycle, with the revocation store hardened as the first admission gate. OA-2 lands provider-local admission end to end: `serve_rpc_protected`, the shared canonical org-RPC request digest, an admission stability stamp with a recheck hook, a per-caller replay ceiling, `RpcStatus::AdmissionDenied` (`0x0009`) carrying a deliberately **coarse** wire reason, and `RpcContext::org_admission` as the audit witness. Every primitive landed **unwired** first and was activated in one atomic slice with live two-node transport, provider-state, and mixed-version witnesses.
- **The announcement is encrypted, not filtered.** OA-3 is the part that earns the release its name. A scoped capability is emitted as a `ScopedCapabilityAnnouncement` on the new `0x0C04` subprotocol — AEAD-sealed to an owner or granted audience, with the scoped tags **excluded from the plaintext projection** so the confidential service never appears in a `0x0C00` payload at all. The emission is atomic (`LocalCapabilityEmission`, closing the torn-snapshot invariant), the consumer side has its own scoped-discovery store with owner and granted verification pipelines, and ingest carries a publication-race recheck, query-time revocation currentness, a cardinality bound, and a poison-refuse gate.
- **The footguns were closed at the type level.** The zero `grant_id` is reserved and rejected at issuance and decode; envelope dedup identity includes `grant_id`; secret-bearing runtime types are structurally non-serializable. `MeshNode::test_inject_capability_announcement` — `#[doc(hidden)] pub` and re-exported through the Python, Node, and Go bindings — could install an ownership projection under a node id no retraction path would ever visit; the fix went to the producer, and `verify_announced_owner_cert` now refuses that binding for every caller.
- **Grant management is an operator surface, not application code.** `net org grant-dispatcher` and `net org grant-capability` (including `--discover` for the audience secret) ship with the `net_sdk::org` re-exports, plus the exit-gate witnesses that pin the contract: no `discovery_key` in the proof or header, and an installed-secret commitment mismatch rejects locally.

---

## Subnets — topology is not authority

The subnet authority plan opens by falsifying a claim the codebase had been carrying: a full trace confirmed that neither the ambient `SubnetRights::PARTICIPATE` nor the general `SubnetAccessGrant` sketch was ever implemented. What replaced them is five orthogonal planes and one fixed-width authorization path.

```text
Organization = horizontal federation across independently operating nodes
Subnet       = vertical topology + transport scope inside one composed system
Channel      = data-plane publish/subscribe authority
Resource     = provider-local effect authority
Control      = signed transport admission, route, and export grants
```

- **The clean break (S1).** The callee-side nRPC gate used to admit an inbound call when the caller's self-declared `subnet:<hex32>` / `group:<hex64>` tag matched the provider's `allowed_subnets` / `allowed_groups` — values the provider itself broadcasts in cleartext up to `MAX_CAPABILITY_HOPS`, claimable by any observer with one `add_tag`. The new `may_admit()` is the callee-side gate and admits on `allowed_nodes` (blake2s-bound) or the all-empty permissive default only; a capability restricted by the demoted axes alone now denies **every** caller. `may_execute()` is re-documented as what it always was: a caller-side *routing* predicate that narrows and admits nobody. Multiple distinct `subnet:` tags now collapse to no membership deterministically instead of last-wins in wire order, and the divergent duplicate parser was deleted.
- **Credentials, sessions, and the relay (S2–S4).** Fixed subnet credentials with hierarchy and revocation; session admission producing a `VerifiedSubnetContext`; attachments, local gateway authority, and a dark route-hop wire; then authenticated relay enforcement with allocation-free route-hop sealing and an off-path gateway scope index for protected forwarding. Typed relay telemetry attributes the two-gateway inverse exactly.
- **Signed control facts (S5).** Descriptors, gateway advertisements, export policies, and revocation floors are independently signed and distributed over the existing transport — a node applies them without a synchronous authority fetch.
- **`Visibility::Exported`, and the repair that followed.** Export policy originally keyed on the 16-bit channel **wire hash**, documented elsewhere as a fast-path filter hint with routine collisions. It was reproduced: `collision/242` and `collision/351` share bucket `0x22f2`, so declaring targets for the first returned them for the second — two unrelated channels sharing policy through a collision an attacker can arrange by choosing a name. The whole export table is re-keyed on the canonical `u64` `ChannelHash`, with `export_channel_by_name` / `export_targets_by_name` as the preferred operator surface so the hint cannot reach a policy key through that door. The original argument for shipping it — that visibility pairs with token enforcement — is retracted in the commit message rather than quietly dropped: a tokenless channel has nothing else in front of it, and relying on another gate to contain one policy's aliasing is invalid composition.

---

## Two verbs per plane, five languages

Both new authority planes ship with an SDK deliberately *smaller* than the substrate. The normative application surface is two verbs each, and application code constructs no authority objects.

```rust
// organization
let org  = mesh.org(credentials)?;
let resp: Reply = org.call("billing.settle", &req).await?;
let _h = mesh.serve_org("billing.settle", handler)?;

// subnet export
let _h = mesh.serve_subnet_exported("fleet.telemetry", "factory-export", handler)?;
let resp: Telemetry = org.call_exported("fleet.telemetry", &request).await?;
```

- **`call_exported`, deliberately not `call_subnet`.** The caller never joins the provider's subnet, receives no `SubnetRef`, topology epoch, gateway, or boundary state, and discovers on the **public** plane through the verified ownership projection. An exported call is an organization call, and its failures are the four org error domains.
- **A named export is a provider-local label.** Configured at mesh construction (`subnet_authority` / `subnet_attachment` / `subnet_control_channel` / `subnet_export`), resolved once into a checked binding, never announced and never accepted from a caller. An unknown name fails locally (`subnet:unknown_export_name`) before anything registers. Dispatch revalidates the exact crossing against the node's **live** gateway authority on every call, before organization admission — a revoked or epoch-stale binding stops serving even though registration succeeded.
- **Five bindings, one boundary.** Rust facade, Node/TS, Python, Go, and C (`net_org.h` and `net_subnet.h` over `libnet_org`), each mirroring the same constructor keys and the same four error domains. Two cross-language fixtures pin it: an error-vocabulary fixture and a cross-org scenario generator whose manifest every language's live cell consumes, closing the per-language matrix with real admitted calls rather than shape assertions. Building those cells surfaced two more binding bugs that made a binding org *provider* silently non-functional — invisible to any test that only checked types.
- **Provisioning is an offline ceremony.** `net-mesh org` (`keygen`, `issue-cert`, `issue-floors`, `grant-dispatcher`, `grant-capability`) and `net-mesh subnet` (`keygen`, `issue-direct`, `issue-issuer`, `issue-delegated`, `issue-control-fact`, `inspect`) need no live node and never touch the mesh; every signed subnet artifact is written as framed **canonical wire bytes**, the exact form a node consumes, never a JSON mirror. The secret hygiene is spelled out where operators will meet it: the root seed is never echoed and its buffers are scrubbed, grant artifacts publish no-clobber (`--force` is refused, because an aliased `--out` on a case-insensitive filesystem could destroy the org key), and `--accept-windows-dacl` was split from `--insecure-permissions` after operators carried the Linux flag to Windows and silently killed the only warning that platform has.
- **Honest about what issuance proves.** `issue-delegated` checks that the leaf stays inside the issuer grant it was handed, but does not verify that grant's signature against an authority root — it is an offline ceremony with no trusted root supplied. A forged `--issuer-grant` frames cleanly and produces a credential set every node will reject. Successful issuance is not proof of deployability; `net-mesh subnet inspect` is.

---

## The money path, audited

A full-surface security pass over `net-payments` (~5k lines of non-test source across `x402/`, `core/`, `facilitator/`, `engine/`, `flow/`, `policy/`, `checker/`, `billing/`), the SDK seams it composes against, and the Node/Python provider bindings. It went out **HOLD**, on one theme: *the money path's trust roots were asserted in doctrine and documentation but not enforced at the boundaries that matter* — and in two places the documentation actively misdescribed the code. Every finding is now resolved, most with a red-coupled regression test.

- **Quote issuance authenticated nobody (H1).** There was no authenticated end-to-end caller on the public RPC surface to fix it with, so one was built: `net.payment.quote_request@1`, a caller-signed envelope binding the tag, destination provider, caller, capability, template hash, a bounded freshness window, and a nonce, behind a `SeenNonces` replay guard. A quote request is single-use, and the docs now say so.
- **The shipped bindings had no settlement backend (H2).** Provider constructors now require an explicit one — `facilitator_url` or `unsafe_dev_mock_facilitator`. Neither is an error, **both** is an error, and a real URL is never silently downgraded. `payments-http` ships by default and the active backend is observable.
- **Independent verification could be pointed at cleartext (H3, M4).** All three money-path HTTP clients go through one shared `http_policy`: scheme enforcement, destination policy, bounded reads on both outbound bodies, destination checked before the unpaid probe through the resolver (rebinding-safe) plus a literal check, including v4-in-v6 embeddings.
- **The invocation binding is now the default, not the upgrade (M1 — breaking).** The bindings already required it; `PaymentEngine::new` did not, so a Rust provider constructing the engine directly still admitted quote-id-only bearer redemption and the safe default existed only in the layer above. Bearer redemption is the exposure, so it is not the posture to default to — a provider that wants it now asks, and the asking is visible at the call site. Both the nonce and attempt counters became process-global in the same change: each was an instance field, so two flows over one spend store collided under a stopped clock — the exact bug the counters were added to fix, one scope out.
- **Four more, each narrower than it looks.** `tier` is gone from facilitator outcomes and the engine mints `Observed` itself (M2). The non-authorizing `quote_ref` in logs is **keyed** with a process-local secret, because the first unkeyed digest was invertible from the quote's construction inputs (M3). Replay identity comes from the scheme's signed material, namespaced by `scheme + network + asset + authorization`, and unknown schemes fail closed (M5). Spend reservations have an owner and an idempotent release independent of the caller's clock, and payment stores are owner-only on Windows too via an explicit DACL applied at create rather than after (L2, L3).
- **Storage-bound, and now bounded.** The hot-path audit's headline is that payment admission is **storage-bound, not crypto- or transport-bound**. Retention narrows to a lifecycle-compaction policy that qualifies exactly what it bounds and warns when it isn't enough; terminal quote records retire six hours past quote expiry; the store is written compact rather than pretty-printed; quote canonicalization happens only when holding a new approval; the nonce sweep is amortized and nonce length is bounded before verifying. Settlement-uniqueness tombstones remain unbounded by design, and the audit says so.

---

## The audits

Four separate passes landed this cycle, each with a published disposition table and each recording what it decided **not** to fix.

- **Channel auth.** `serve_rpc*` overwrote operator-installed RPC ACLs with permissive defaults and the documented escape hatch did not exist (H2 — fixed with `insert_if_absent` and a real `Mesh::register_channel_prefix`). Public-mode reply channels were world-subscribable and the roster fallback disclosed response bodies to raw event-plane readers (H3 — fixed with an `OriginBinding` on the reply prefix, evaluated against the pinned identity). Queue-group membership was unauthenticated, so a subscriber could steal another member's work (M2 — fixed with `QueueGroupPolicy::TokenBound`). Token TTL is now enforced on receipt at the common receiver seam, not only at issuance (M3). And `AclPrincipal` puts the derivation *in* the ACL key, so `Node(x)` and `Origin(x)` are disjoint principals even for an identical scalar (I1).
- **Channel auth H1 is open, and labeled a stop notice.** Publish authority is emitter-side only and nRPC direct sends bypass even that. Four review rounds found nine blocking defects across both candidate designs; one of them may disqualify both, since an nRPC *caller* installs no config for its own reply channel and a self-describing packet supplies identity, not authority. Nine decisions must be settled before any code. Until it ships, `require_token` is a **read** ACL, and that sentence is now in the audit rather than in a reader's head.
- **Scoped capabilities.** The multi-hop `scope:subnet-local` leak is fixed by resolving forwarded peers from the origin's own tags on the indexed announcement — generation-coherent, with no sidecar to diverge. All three native converters now error on semantically invalid scope filters instead of widening them to `Any`; the Node binding's filter-kind spelling drift is fixed; scope derivation evaluates off borrowed tags with no allocation under the fold locks; and `subnet_visible` no longer coerces an unresolved peer to `GLOBAL`. Two items are **deliberately not fixed**: `allowed_groups` publishes the secret that protects it, and subnet membership is self-asserted. Both need an issuer-signed entitlement primitive that is a design pass of its own, so instead they are corrected at every operator-facing surface — module docs, both announcement fields, `may_execute`, CLI flag help, and a runtime warning on `net cap announce` — and marked advisory rather than access control.
- **Three performance audits, with their status stated plainly.** The gang-scheduler audit shipped slices 1–3 measured, held slice 4 as a protocol decision rather than an optimization, and blocked slice 5. The payments hot-path audit implemented §1–§3 and §5 and withdrew §4. The subnet-paths audit is **triaged — nothing implemented, nothing measured**, with correctness HOLDs sequenced ahead of it and a measurement contract for whatever follows.

---

## Benchmarks

Three suites landed, and what makes them useful is the equivalences each one refuses to publish.

- **Capability propagation (CPB-0 … CPB-6).** Five false equivalences that must never appear in a published row: a watch wake is not query visibility, a version delta is not packets emitted, encoded payload size is not bytes sent, a capability query is not a scheduler decision. The burst benchmark is a coalescing-efficiency measurement and explicitly *not* a stale-sleeper correctness guard.
- **Island / gang claim (ICB-0 … ICB-6).** The load-bearing finding is a correctness one: `ReservationFold::merge` is arrival-order-dependent across publishers, so the cross-node `Reserved` path does not converge — there is no total-order tie-break, authoritative host CAS, or on-wire quorum on `reserve_island`. ICB-3 measures the *degree of divergence* rather than reporting a throughput number over a path that doesn't settle.
- **Payments (P1 … P7).** The published framing separates the ready-settled invocation gate from exact-payment acceptance, paid invocation, and external settlement, and every row states whether durable storage, facilitator I/O, and handler execution are included. External facilitator and chain latency are never blended into a Net-controlled number.
- **The perf work they justified.** Bounded selector-list and assignment work under the fold locks, selector lists that stop rescanning once a hit is recorded, gang matching that bails before building the band map and bands off the selection snapshot, step-1 candidate hosts resolved without cloning payloads, payload moved into the entry on fold apply, a memoized poison canonical path key, and the subnet publish path hoisting the `Global` verdict instead of rendering tags per assign.

---

## The docs

- **The polyglot lens.** The governing principle: *turn Net's docs from documentation of a Rust system with bindings into documentation of a multilingual protocol and runtime* — implemented, per the central review correction, as **one canonical operation model composed with a selected binding expression**, not five authored versions of every page. A persistent language selector is chrome at every breakpoint, code blocks scroll rather than wrapping mid-diagram, and the SDK spine (`quickstart`, `announce`, `discover`, `invoke`, `watch`, `errors`, `artifacts`) ships per-language for Rust, TypeScript, Python, and Go, with C's boundary pages in their own shape. Found en route: the sidebar footer had hardcoded `v0.17` on all 149 pages while the newest release note was v0.33.
- **`docs/data/` — public product metadata.** A sibling of `docs/internal/`, not part of it. One authored record per domain, and any portable copy is generated and equality-checked in CI: `capabilities/<domain>.yaml` is the record for which binding supports which operation *and why not when it does not*; `examples.yaml` records where each example's source lives and what CI proves about it; `tiers.yaml` assigns every docs page a migration state and is proven exhaustive and disjoint against the tree on every run, so a page added without a state fails rather than defaulting to one.
- **New pages for the new planes.** [Organizations](/docs/concepts/organizations), [Security model](/docs/concepts/security-model), [Agent identity](/docs/concepts/agent-identity), [Tool federation](/docs/concepts/tool-federation), a rewritten [Subnets](/docs/concepts/subnets), and the end-to-end [Private capabilities](/docs/guides/private-capabilities) walkthrough — which opens by telling you when *not* to reach for org auth, and says plainly that capability `scope:*` tags filter your own query and stop nobody. Plus [Production deployment](/docs/guides/production-deployment), [Troubleshooting](/docs/guides/troubleshooting), [Gang scheduler](/docs/guides/gang-scheduler), [Mesh streams](/docs/guides/mesh-streams), [Task lifecycle](/docs/guides/task-lifecycle), [Agent-to-agent](/docs/guides/agent-to-agent), a [Glossary](/docs/reference/glossary), and a [Versioning](/docs/reference/versioning) page.
- **The skills route by binding now.** Shared doctrine plus explicit routing plus thin per-language companions under `bindings/`, with a maintained coverage matrix that answers "does binding X support operation Y" in exactly one place — including the parts that are ugly, like `net.h` and `net.go.h` sharing an include guard so "event bus" and "capabilities" are both supported in C and still cannot be used in the same translation unit. The verification work behind it is CI, not prose: new `skills.yml`, `docs-api.yml`, `web.yml`, and `cla.yml` workflows and a dozen checkers covering compiled examples, cited source paths, snippet drift, vocabulary, doc-code width, and the tier assignment. The plan that produced them was itself held once for claiming more than the machinery delivered — compile-checking is not execution, and a resolving test name is not proof.

---

## Licensing

Net is now **MIT OR Apache-2.0**. Apache-2.0's patent-termination (§3) and NOTICE (§4d) clauses count as "further restrictions" under GPLv2 §6, so Apache-2.0-only code cannot be combined with GPLv2-only projects — the Linux kernel, OpenWrt userspace, BusyBox. Adding an MIT arm lets those consumers elect a GPLv2-compatible license while everyone else takes Apache-2.0 for the express patent grant; it is also the Rust ecosystem default. Seventeen `Cargo.toml`s, four `package.json`s, and four `pyproject.toml`s carry the dual expression, both license files ship in every published artifact, and the Python packages gained the MIT trove classifier. **This is not retroactive** — already-published crate versions remain Apache-2.0-only, and the CLA now states that released code stays under the license it shipped under permanently, since both grants are irrevocable.

---

## What's deferred (honestly)

- **Organization load balancing stays dark.** The OLB substrate landed in depth this cycle — indexed private-discovery storage, per-scope maintained counts, event-driven exact-expiry, a bounded node routing registry, a routing supervisor with incarnation fencing, per-slot `ArcSwap` publication, and the consumer-Grant installation/invalidation edge — but only the **exact-provider** relay re-authoring is live. The `OrgCapabilityRegistration` leader / capability-resolution path is deliberately dark, and the release boundary is explicit: subnet authority and its operator/SDK surfaces ship, organization load balancing does not.
- **Sensing still ships dark.** `enable_sensing_coalescing` remains `false` by default and an origin additionally requires a persisted `sensing_incarnation`. The organization-authenticated registration variants are **appended** to `SensingInterestFrame` under the existing `0x0C02` — never a new subprotocol — and land structurally dark. The sensing SDK lifecycle (S1–S4) is specified, not shipped.
- **Channel-auth H1 — receive-side publish authority.** Open, and explicitly a stop notice rather than a plan: nine blocking defects, two candidate designs, and a real chance both are disqualified. `require_token` is a read ACL until it ships.
- **The entitlement primitive.** An issuer-signed `(subject, axis, value, validity)` with a wire format, delegation model, execution-grade revocation, and cross-language surface. Both open scoped-capability items depend on it, and neither is closable without it.
- **No production subnet session handshake.** The SDK does not own one and does not pretend to. Nothing in the shipped surface implies an external caller joins the provider subnet, or that topology membership alone is authority.
- **The subnet-paths performance audit is unimplemented and unmeasured.** No profile was taken and no before/after numbers exist; its decision markers are a work ranking, not performance claims.

---

## Breaking changes

v0.34 is the first release in several cycles that breaks behavior deliberately, and every break is an authorization one.

- **Self-declared `subnet:` / `group:` axes no longer admit.** The callee-side gate is `may_admit()`: `allowed_nodes` (blake2s-bound) or the all-empty permissive default. A capability restricted only by `allowed_subnets` / `allowed_groups` now denies every caller. There is no compatibility bypass, per direction — the axes never provided access control, and continuing to honor them would have kept a claimable tag standing in for authority.
- **Export policy is keyed on the canonical `ChannelHash`.** The 16-bit wire hash is a filter hint with routine collisions and is no longer a policy key. `export_channel`, `export_targets`, `exports`, `exports_for_channel`, and `Deck::gateway_exports` all re-key; `export_channel_by_name` / `export_targets_by_name` are the preferred operator surface.
- **Payments: the invocation binding is required by default.** `PaymentEngine::new` now refuses bearer (quote-id-only) redemption. Providers that need it call the explicit opt-out, and the opt-out is visible at the call site.
- **Payments: provider constructors require an explicit settlement backend.** `facilitator_url` or `unsafe_dev_mock_facilitator` — neither is an error, both is an error. A real URL is never silently downgraded to a mock.
- **Payments: retention narrows to a lifecycle-compaction policy**, which states exactly what it bounds and warns when it isn't enough.
- **Scope-filter converters error instead of widening.** A semantically invalid filter object at the Node, Python, or C boundary is now a rejection, not a silent promotion to `Any`.
- **New wire frame `0x0C04` — scoped capability announcement.** Emitted only by a node serving an org-protected capability, always encrypted, and never present inside a plaintext `0x0C00`. A peer that has never heard of the id drops it at the dispatch loop's unknown-subprotocol guard; mixed-version degradation matches `0x0C01`.
- **License expression changed to `MIT OR Apache-2.0`** across every manifest. Not retroactive; already-published versions remain Apache-2.0-only.

Everything else is additive: the public capability plane, transports, folds, reliability, streams, and existing SDK paths are unchanged in shape, and a deployment that adopts no new plane sees a version bump.

---

## How to upgrade

1. **Pull the release.** Existing bus, stream, nRPC, payments, and persistence code behaves as before unless it relied on one of the breaks above.
2. **If you used `subnet:` or `group:` tags as access control, you did not have access control.** Move the decision to `allowed_nodes`, or — if the answer to "who is allowed?" is a company rather than a node — to organization auth. `net cap announce` now warns, and the axes are documented as advisory routing narrowing everywhere they appear.
3. **Payments providers: pass an explicit settlement backend**, and expect the invocation binding to be required. If you genuinely need bearer redemption, opt out explicitly; if you announce a price, the engine now refuses to advertise one the backend cannot settle.
4. **To ship a private capability**, run the offline `net-mesh org` ceremony, `net-mesh node adopt` the ownership onto each node, then `mesh.serve_org(...)` on the provider and `mesh.org(credentials)?.call(...)` on the caller. Read a refusal as *unauthorized*, and remember an unauthorized caller normally sees absence instead.
5. **To export across a subnet boundary**, configure `subnet_authority`, `subnet_attachment`, `subnet_control_channel`, and `subnet_export` at mesh construction — a broken config means no node, not a half-authorized one — then use `serve_subnet_exported` on the provider and `org.call_exported` on the caller. Verify every artifact with `net-mesh subnet inspect` before distributing it.
6. **If you vendor Net into a GPLv2-only project**, you may now elect the MIT arm.
7. **Everyone else** gets the new surfaces, the audits' fixes, and no behavior change to existing paths.

---

## Dependency updates

The crate version bumps `0.33.0 → 0.34.0`, propagated across the CLI, deck, SDK, payments, and language-binding manifests, alongside the dual-license expression. The cycle spans 1,221 non-merge commits over 1,232 files (+254.3k/−35.2k), so the refresh is correspondingly broad — but no first-party crypto moved, and neither new authority plane introduces a third-party dependency.

- **Rust:** `syn` v3, `comfy-table` v8, `async-nats` 0.50, `base64` 0.23.1, `clap` 4.6.5 / `clap_complete` 4.6.8, `lru` 0.18.2, `napi` 3.12 / `napi-derive` 3.6.2, plus `anyhow`, `async-trait`, `futures`, `hdrhistogram`, `libc`, `proc-macro2`, `webpki-root-certs`, and `xxhash-rust` lockfile refreshes. Toolchain to Rust 1.97.1.
- **Docs / web (`web/`):** Next.js 16.2.11, React 19.2.8, Prisma 7.9.1, `motion` v13, `shiki` 4.4.2, Tailwind 4.3.3, `sentry-javascript` 10.69, `tanstack-query` 5.101.4, `better-auth` 1.6.26, plus Radix, `react-hook-form`, `immer`, `axios`, `marked`, and the routine `posthog-js` / `posthog-node` cadence.
- **CI:** `actions/checkout`, `actions/setup-node`, `actions/setup-go`, and `actions/setup-python` all to v7, Node 24, `sccache-action` 0.0.11 — and four new workflows (`skills.yml`, `docs-api.yml`, `web.yml`, `cla.yml`) gating the docs and skills machinery described above.
- **Packaging:** a maturin sdist license collision in the PyPI packages is fixed, and both license files now ship in every published artifact.

---

Released 2026-08-05.

## License

Dual-licensed under [MIT](https://github.com/ai-2070/net/blob/master/net/crates/net/LICENSE-MIT) **OR** [Apache-2.0](https://github.com/ai-2070/net/blob/master/net/crates/net/LICENSE-APACHE), at your option.
