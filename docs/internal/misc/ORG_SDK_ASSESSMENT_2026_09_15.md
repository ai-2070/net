# Organization Capability SDKs — State Assessment (2026-09-15)

A ground-truth survey of the organization capability verb surface across
languages, and of the four surface gaps that keep coming up in conversation
(streaming, watch/sensing, "can a binding serve?", public-plane discovery).

Every claim below was checked against the tree at `685e249d4`, not against the
plan docs. Where a plan doc and the tree disagree, the tree wins and the
disagreement is recorded.

Plan lineage this assessment reads against:

- [`ORG_CAPABILITY_SDK_PLAN.md`](../plans/ORG_CAPABILITY_SDK_PLAN.md) (OSDK, v0.4) — the Rust facade.
- [`ORG_SDK_EXIT_GATE.md`](../plans/ORG_SDK_EXIT_GATE.md) — its requirement → witness map.
- [`ORG_CAPABILITY_LANGUAGE_SDKS_PLAN.md`](../plans/ORG_CAPABILITY_LANGUAGE_SDKS_PLAN.md) (OSDK-L, v0.5) — TS/Python/Go/C.
- [`ORG_CAPABILITY_AUTH_PLAN.md`](../plans/ORG_CAPABILITY_AUTH_PLAN.md) (OA, v1.4) — the substrate.
- [`ORG_CAPABILITY_LOAD_BALANCING_PLAN.md`](../plans/ORG_CAPABILITY_LOAD_BALANCING_PLAN.md) (OLB, v0.6) and
  [`ORG_EXACT_SENSING_ACQUISITION_PROJECTION_DESIGN.md`](../plans/ORG_EXACT_SENSING_ACQUISITION_PROJECTION_DESIGN.md) (OA-1..OA-6) — sensed selection beneath `OrgClient::call`.
- [`ORG_SENSING_LEADER_SUBSTRATE_PLAN.md`](../plans/ORG_SENSING_LEADER_SUBSTRATE_PLAN.md) (LS) — still design-only.

---

## Where the Organization capability SDKs stand

### Rust facade: implemented and closed

OSDK v0.4's four slices landed (`a9ec879a4` … `04d66e9b8`, substrate base
`07820a9de`). `ORG_SDK_EXIT_GATE.md` maps every plan claim to a witness. Public
surface is five concepts — `OrgCredentials`, `OrgClient`, `OrgAccess`,
`OrgCaller`, `OrgSdkError` — and two verbs, `mesh.org(creds)?.call(..)` and
`mesh.serve_org(..)`. The core-touch inventory is verified at four files;
`may_execute` is byte-for-byte unchanged. The plan's own ruling is "then stop":
no further org-SDK work without a named application consumer or a measured
failure.

### Language SDKs (OSDK-L v0.5): all four bindings implemented and gated

| Surface | State | CI evidence (`.github/workflows/ci.yml`) |
|---|---|---|
| Node/napi | `bindings/node/src/org.rs` + `org.ts`; suites `org_binding`, `org_error_vectors`, `org_live` | `org` in the napi feature list (`:2587`), release-wheel parity guard (`:2716`) |
| Python/PyO3 | `bindings/python/src/org.rs`, `org_serve.rs`, `python/net/org.py`; the three twin suites | `org` in `maturin develop` (`:2863`) and `maturin build` (`:2952`) |
| C | `bindings/go/org-ffi` + hand-written `include/net_org.h` (own `NET_ORG_*` namespace from `-1`, ABI stamp, Rust↔header numeric-mirror test) | `go-org-ffi` rows in the per-member clippy (`:3488`) and lib-test (`:3597`) matrices; `cargo doc -p net-org-ffi` with `-D warnings` (`:4189`); the S4 C live cell, the only FFI crate with integration targets (`:3850`) |
| Go | `go/org.go` over the C ABI; `org_test.go` (7 tests), `org_golden_vectors_test.go` | `net_org_` prefix asserted present in the single `libnet.so` (`:3156`); cgo suite runs on the Linux job |

`org` is in both the tested and the *released* feature sets — the guard at
`ci.yml:2788` exists precisely because `org` and `deck` once shipped absent from
the wheel while CI tested them.

### Conformance (Workstream X)

- **X1 — done.** `tests/cross_lang_org/error_vectors.json` (keys:
  `description`, `version`, `prefix`, `domains`, `vectors`,
  `unclassified_cases`), generated from one Rust source and consumed by the
  Rust, Node, Python and Go suites. The drift guard was proven to fail on a
  rename.
- **X2 phase 1 — done.** `sdk/src/org/fixtures.rs` +
  `sdk/examples/gen_org_scenario.rs` mint the whole issuance chain (org A
  caller, org B provider, a B→A DISCOVER|INVOKE grant) to a directory with
  seeded identities and a `manifest.json` contract. The Rust from-disk cell
  (`live_cross_org_call_from_a_generated_scenario`) plus **in-process** live
  admitted cross-org calls through Node (`bindings/node/test/org_live.test.ts`)
  and Python (`bindings/python/tests/test_org_live.py`), each self-generating
  the scenario via cargo and skipping cleanly without a toolchain.
- **X2 phase 2 — OWED.** No multi-process, cross-language matrix exists and
  none is wired: `grep` for `gen_org_scenario` / `org_live` across
  `.github/workflows/*.yml` returns nothing, and `go/org_test.go` contains no
  live admitted call (only refusal wire, seed stability, provisioning surface,
  access constants, caller projection). The Go live cell is the one remaining
  per-language gap.
- **X3 — partial.** Go pins the ABI in `init()`
  (`go/org.go:162`, `net_org_check_abi_version`) and carries
  `org_golden_vectors_test.go`. Node's `abi_stability.test.ts` and Python's
  `test_abi_stability.py` were never extended with org rows; the dedicated
  `org_error_vectors` suites cover the vocabulary instead.

### The program's frontier moved above the SDK

The SDK lane is quiet; the work went into the substrate beneath it.
Exact-provider org sensing and load balancing beneath `OrgClient::call` is
**merged and signed (2026-09-11, PR #943, `a2efc950a`)**, with named-witness CI
floors: `org_routing_wiring_tests` MIN=93, `org_gate` 60 /
`sensing_authority_witness_tests` 67, the nine-witness organization lease-wire
boundary, and OA-6's 22 production witnesses. The provider-free leader track
(LS-1..LS-6) is design-for-review and dark; `SAFE_LIVE_HEAD` is still not
established. Language bindings for the sensing surface are explicitly not
authorized by that sign-off.

---

## The four questioned items

### 1. Org-scoped streaming — PENDING, and it is a substrate limit, not a binding gap

`OrgClient::call` is request/response because the **core has no protected
streaming form**, in any language:

- `src/adapter/net/mesh_rpc.rs:6392` — "Streaming / duplex have no protected
  form (E1.8)."
- `mesh_rpc.rs:1116-1118` — "Unary only (E1.8): a streaming flag on a protected
  REQUEST is a distinct 'not supported' denial, never admitted under a unary
  binding."
- `mesh_rpc.rs:199`, `:230` — protected admission is unary-only; the
  `net-org-admission` header is minted inside the unary `call` only.
- Protected registration exists solely on `serve_rpc_unary_impl`
  (`UnaryAdmission::{Public, Protected, ProtectedRedWitnessDisabled}`,
  `mesh_rpc.rs:3211-3247`, `:3351`).

So "no org-scoped open-a-stream-and-keep-receiving" is true of Rust as well.
Closing it is protocol work, not marshaling: a new admission binding for the
streaming shapes (per-frame vs per-call proof lifetime, replay-guard semantics
across a long-lived stream, denial routing already hardened in
`tests/nrpc_streaming_gate.rs` NC1/NC2). That is why OSDK-L lists streaming
under §Non-goals rather than as owed binding work.

Note the contrast that makes the boundary concrete: the callee capability gate
*does* cover all four serve shapes (`nrpc_streaming_gate.rs`:
`client_streaming_denies_unauthorized_caller`,
`duplex_denies_unauthorized_caller`). Capability authorization spans streaming;
**organization admission does not.**

### 2. Watch / sensing from a binding — PENDING, entirely unstarted

- The Rust consumer watch exists and is signed: `sdk/src/sensing/consumer.rs:529`
  — `pub fn watch(&self, query: SensingQuery) -> Result<SensingWatch, SensingError>`.
- `git grep -il sensing -- bindings ../../../go sdk-ts sdk-py` returns **zero
  files**. The whole sensing/watch surface is Rust-only.
- This matches the sign-off text verbatim: what OA-1..OA-6 did *not* light
  includes "language bindings".

The enabling machinery is not the blocker — non-org streaming already crosses
the boundary (`bindings/python/src/lib.rs:2223` and `:3480`
`subscribe_channel`; `bindings/python/src/deck.rs:822`/`:839` log and failure
streams). What is missing is an authorized sensing/watch binding design. OSDK-L
§Scope boundary defers it until the Rust watch lifecycle proves itself, and
keeps it out of the org facade workstream deliberately.

Consequence, stated plainly: from Python today you can tail a channel, but you
cannot tail anything through the organization layer.

### 3. "A binding can only be a caller" — FALSE; every binding serves today

The provider verb ships in all four languages:

| Language | Provider verb |
|---|---|
| Node | native `serveOrg` + `serveOrgTyped` (`bindings/node/org.ts:191`), `OrgServeHandle` |
| Python | `serve_org` (`bindings/python/src/org_serve.rs:105`) + `serve_org_typed` (`python/net/org.py:177`) |
| Go | `ServeOrgBytes` (`go/org.go:892`) and `ServeOrg[Req,Resp]` (`:933`), with the `//export` trampoline + handler registry |
| C | `net_org_serve` in `org-ffi`, driven end-to-end by the S4 C live cell in CI |

And it is proven live, not merely compiled: `test_org_live.py:142` runs a
**Python provider** — `net.serve_org(provider, service, "granted", handler)`
plus `install_provider_grant_audience` (`:146`) — accepting an admitted
cross-org call with four-party attribution asserted inside the handler.
`org_live.test.ts` is the Node twin. Two real binding bugs were found *because*
those provider paths were exercised: `serve_org` spawning an inbound-event
bridge with no ambient tokio runtime ("there is no reactor running"), and
`install_org_authority` not enabling owner-cert emission, which left a binding
provider emitting no scoped announcements and undiscoverable
(`org:discovery:no_authorized_provider`, 0 candidates).

So a Python-side executor **can** be asked, in both `SameOrg` and `Granted`
mode.

Caller-side is also already wider than `call`: Go carries
`CallExportedBytes` / `CallExported[Req,Resp]` (`go/org.go:597`, `:627`) for
subnet-exported services, from the subnet S4d work.

### 4. Public-plane serve/call — PENDING, and narrower than "cannot serve"

The deferred item is the **protected-but-publicly-discoverable** variant: a
capability that is still admission-gated but whose announcement rides the
*plaintext* discovery plane. Shipped bindings are private-by-default —
`SameOrg` ⇒ `OwnerDelegated` + owner-scoped **encrypted** discovery, `Granted`
⇒ `CrossOrgGranted` + grant-audience **encrypted** discovery (exit gate §3-4:
`same_org_is_private_by_default`, `granted_is_private_by_default`, each
asserting the tag is absent from the plaintext announcement while a public
service beside it stays present). Rust keeps the public-plane form on the
low-level API on both sides; no binding exposes it.

This governs **who can discover** your service, not whether a binding can serve
one.

---

## Consolidated pending list (org bindings)

| Item | Blocked on | Where it lives |
|---|---|---|
| X2 phase 2 — multi-process cross-language matrix (Rust↔Go, Rust↔Node, Rust↔Python, Go↔Node) | nothing but execution; the `manifest.json` is the whole contract, and it must land where it can run (CI) | OSDK-L §X2 |
| Go live admitted cell | same as above | OSDK-L §X2 / `go/org_test.go` |
| X3 — org rows in Node `abi_stability.test.ts` and Python `test_abi_stability.py` | nothing but execution | OSDK-L §X3 |
| Sensing / watch bindings | an authorization decision + a binding design for the watch lifecycle | OA design §"did NOT light"; OSDK-L §Scope boundary |
| Org-scoped streaming | substrate: E1.8 in `mesh_rpc.rs` (protected admission is unary-only) | OA / OSDK-L §Non-goals |
| Public-plane protected serve/call | product decision; Rust keeps it low-level | OSDK-L §Non-goals |
| `org.discover` (discovery enumeration) | deferred in Rust first; bindings inherit | OSDK §Deferred |
| `OrgAdmin` / issuance in any binding | refused by design — a second issuance path; credentials arrive as CLI-minted files | OSDK §Deferred, OSDK-L §Non-goals |
| Org wrappers in the pure `sdk-ts` / `sdk-py` packages | deliberate: org lives in `@net-mesh/core` and the `net` wheel, matching nRPC's non-wrapping (`sdk-ts/src/tool.ts:7-13`). Only doc mentions exist today (`sdk-py/src/net_sdk/mesh.py:157`, `sdk-ts/src/mesh.ts:58`) | OSDK-L §What ships |
| `OrgCaller.grant_id`, options objects, policy hooks, pluggable codec, wasm/browser TS | deferred / standing category lines | OSDK-L §Non-goals, §Deferred |

## Verification method

- Binding surfaces: direct enumeration of `bindings/node/org.ts`,
  `bindings/python/python/net/org.py`, `bindings/python/src/org_serve.rs`,
  `go/org.go`.
- Substrate claims: `git grep` for `unary` / `Protected` in
  `src/adapter/net/mesh_rpc.rs`, `org_call.rs`, `org_admission.rs`.
- Sensing absence: `git grep -il sensing` over `bindings`, `go`, `sdk-ts`,
  `sdk-py` — no matches.
- CI gating: `grep` over `.github/workflows/ci.yml` for `org`,
  `gen_org_scenario`, `org_live` (the last two: no matches, which is the X2
  phase-2 finding).
- Signed heads and dark arms: read from the plan docs' own status blocks, which
  carry commit hashes.
