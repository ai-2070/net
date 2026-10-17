# Cross-org invocation: closing the open gaps

## Status

**Done, unreleased** (branch `LZL0/cross-org`, 2026-10-17, on top of 0.44.1).
Five of the six gaps below landed. The sixth, cross-org sensing, was left out
by owner decision. This plan was written after the work, in one session, from
the decisions taken at its start. It records what changed and why, so the PR
can be reviewed against intent.

## The gap

By 0.44.1, cross-org capability invocation over nRPC was functionally
complete. Protected unary calls and all three streaming shapes were admitted
`CrossOrgGranted` in every SDK. Six items were still open, each verified
against the tree before work started:

1. **A grant could not be revoked.** Floors covered membership certificates
   only. A capability grant (B → A) or dispatcher grant (A → S) was bounded
   only by its `not_after` (up to 30 days). A grant revoked mid-call or
   mid-stream ran on to its validity end. Sources: `ORGANIZATIONS.md`
   ("Explicit limitation (grant revocation)"), `ORG_CAPABILITY_AUTH_PLAN.md`
   §D1 ("The mechanism to close it exists … and is not built").
2. **Cross-org floors were "absent means zero".** Admission step 8
   (`org_admission.rs`, the §14 comment at the floor check) read A's floors
   from B's own store. Those floors existed only if B's operators imported A's
   signed bundles by hand, so every A certificate passed at B for its whole
   lifetime. The floor service (`sdk/src/org/floors.rs`) deliberately applied
   only a node's own org's bundles. That was correct as a guard against
   throwaway orgs, but it left no route for A's revocations to reach B. This
   was published as a known limitation in the 0.37 release notes.
3. **Cross-org sensing is refused** (`CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md`
   §Granted): invoke rights deliberately do not imply monitoring rights.
   **Not in scope** (owner decision).
4. **No cross-org CLI generated-client journey.** It was deferred by agreement
   in `NET_CLI_PLAN_V2.md` §8. Only the same-org journey
   (`protected_native_workflow.rs`) existed.
5. **No cross-process, cross-language matrix.** The only cross-process cell
   was `TestStreamingOpeningVectors_MixedPair_GoCallerPythonProvider` (Go
   caller, Python provider, streaming only), and it was skipped in CI behind
   `RUN_MIXED_CROSS_PROCESS=1`. `ORG_SDK_ASSESSMENT_2026_09_15.md` listed
   "X2 phase 2" as owed.
6. **`MeshNode::start()` could report success while starting nothing.** A
   start made while an `accept()` was in flight rolled back with only a
   `warn!`, and every binding returned success. This was finding 8 of
   `R4COREFIX.md`, the root cause of a full diagnostic round in the
   org-streaming S4 harness. The breaking and additive alternatives had been
   left for the owner to choose.

## The design

The owner chose one option for each item at the start of the session. The
alternatives considered are kept here because the reasoning outlives the code.

**Item 1: a grant-id denylist,** chosen over an issued-before floor. The
issuing org signs an `OrgGrantRevocationBundle` under its own domain
(`net-org-grant-revocation-v1`) naming each grant: a capability grant by
`grant_id`, a dispatcher grant by
`blake3::derive_key("net-org-dispatcher-grant-id-v1", wire bytes)`, since
dispatcher grants have no id field. Entries are keyed by the signing org, so an
org can withdraw only grants it issued. Each entry carries the revoked grant's
`not_after` and is pruned once that has passed, which keeps the set bounded
(grants live at most 30 days).
- *Rejected:* a per-(issuer, grantee, capability) issued-before floor. It is
  constant-size, but it cannot revoke one grant without revoking every
  sibling issued before it.
- *Why a new object rather than a new field in the floor bundle:* the floor
  bundle's wire format and golden vectors stay untouched, and the domains
  cannot be confused.

**Item 2: relay plus opt-in freshness,** chosen over an online status signer
and over flipping the default to fail-closed.
- *Relay.* Bundles authenticate themselves and can only raise state, so any
  carrier will do. A's own nodes carry A's material to every provider they
  call.
- *Freshness.* A provider that must not depend on relay can require recent
  material per org. "Nothing arrived" then becomes a refusal instead of a
  silent pass.
- *Rejected: an online signer.* Its bound would be tight, but it means a new
  authority object and a key that has to stay online, against the
  offline-root model.
- *Rejected: fail-closed by default.* It would break every existing cross-org
  deployment.

Three sub-decisions:
- *Freshness comes from the retained, re-verified signed bundles, not a new
  state-file field.* A new field would have pushed every node to state version
  2 on its first floor bundle. Bundles dated in the future do not count.
- *The provider accepts material only from its own org, orgs it granted to,
  and orgs its policy names.* Each foreign org is capped at 4,096 entries per
  kind, and grant-revocation expiries are clamped to the TTL ceiling. This
  keeps the original "no throwaway orgs" guard.
- *The relay rides the signed request.* It travels as a `net-org-revocations`
  header inside the request digest, ingested before the admission snapshot.
  One frame is one packet (8 KiB), so the header is capped at 4 KiB, and it is
  dropped and the request re-signed if it would not fit.

**Item 6: additive core, raising bindings,** chosen over changing `start()`
everywhere and over a purely additive fix. Core keeps `start()` and gains
`try_start() -> Result<(), StartRefused>`. Every binding starts through it:
SDK `Mesh::start() -> Result`, Python/Node raise, and C returns
`NET_ERR_MESH_START_REFUSED` (−129), mapped to Go `ErrStartRefused`.
- *Rejected: changing core `start()`'s signature.* About 980 call sites,
  most of them in tests.
- *Rejected: purely additive.* Existing Python and Node code would have kept
  failing silently.

**Item 5: four languages, unary plus server-streaming, separate OS
processes.** One stdin/stdout protocol (`tests/cross_lang_org/matrix/PROTOCOL.md`)
is spoken by a driver per language. A stdlib-only Python orchestrator runs
every caller × provider pair on a fresh `gen_org_scenario`. Payloads are JSON
bytes, so no binding's codec is assumed.

## The slices

Each slice is one commit on `LZL0/cross-org`.

1. **`22905e4c2` — refused start is reported (item 6).** Witnesses:
   `try_start_reports_the_inflight_accept_refusal_and_recovers` (mesh.rs);
   `headers_declare_the_start_refused_code` (ffi/mesh.rs, both header copies);
   `TestStartRefusedHeaderParity` (Go). About 200 ignored `Result`s from
   `Mesh::start()` were updated: `.expect` in tests, benches and examples,
   propagated in CLI and deck.
2. **`ea8d35fc6` — grant revocation (item 1).** Store: verify → lock →
   reread → merge → persist → publish → notify, shared with floors through
   `apply_locked`. Admission: `CapabilityGrantRevoked` /
   `DispatcherGrantRevoked` before the replay insert. Live streams: a grant feed
   (`subscribe_grants_revoked`) retires calls on a revoked grant. Leaf port
   pinned to the same golden hex. Floor-service request/attestation
   (`NOGR`/`NOGA`) and `net org revoke-grant`.
   Witnesses:
   - Codec, golden and tamper units: `org_grant_revocation::tests`.
   - Store persistence, version, expiry and notification: the
     `grant_revocation_*` units in `org_revocation.rs`.
   - Admission:
     `a_revoked_capability_grant_is_denied_without_consuming_the_replay_slot`,
     `a_revoked_dispatcher_grant_is_denied`,
     `a_revocation_by_anyone_but_the_issuer_or_of_another_grant_is_inert`.
   - Registry: `a_grant_revocation_retires_exactly_the_live_calls_under_that_grant`.
   - Live two-node:
     `live_cross_org_call_is_refused_after_the_provider_org_revokes_the_grant`.
   - Leaf: `a_grant_revocation_retires_only_the_call_under_the_revoked_grant`.
   - CLI: the `revoke_grant_*` tests in `cli/tests/org_grant.rs`.
3. **`75f649ade` — relay and freshness (item 2).** Pieces:
   - `behavior/org_revocation_relay.rs`: covering-set retention, header codec,
     node relay state.
   - Caller attach in `attach_signed_admission`.
   - Provider ingest in both admission paths.
   - The `RevocationsStale` hook.
   - The floor service accepts foreign orgs under the same rule.
   - `net org heartbeat` and `up --require-fresh-revocations`.
   - The policy in Python and Node.

   Witnesses:
   - Relay units: a heartbeat keeps the live floor's source bundle; a
     superseded bundle is dropped; expired grants release their bundle; the
     sidecar re-verifies.
   - Live three-node:
     `a_caller_relays_its_orgs_floor_and_the_provider_then_refuses_the_removed_member`.
     A floor applied only at A's caller reaches B on the caller's next call,
     and B then refuses the removed member. A stranger's bundle and an
     over-cap bundle are refused.
   - Freshness:
     `a_freshness_requirement_refuses_until_the_callers_org_proves_itself_current`.
   - CLI: `heartbeat_writes_an_empty_signed_floor_bundle`.
4. **`911806822` — the CLI cross-org journey (item 4).**
   `two_org_granted_native_denial_then_authorized_generated_call`, pinned in CI.
   It shows:
   - granted private discovery through a B→A grant;
   - an uncredentialed call denied (`0x0009`) before any handler effect;
   - the CLI-generated Python client denied, then admitted, exactly once each;
   - every record attributed to A acting under B's grant (`!is_same_org`).
5. **`a2f329773` — the cross-process matrix (item 5).** Drivers:
   `sdk/examples/org_matrix_peer.rs`, `go/cmd/org-matrix-peer`, and
   `tests/cross_lang_org/matrix/driver.{py,mjs}`; the orchestrator is
   `run_matrix.py`; the new CI job is `org-matrix`. Witness: all 32 cells (4×4
   × unary/server-streaming) pass locally on Windows, re-run on the final head.
6. **`65433a365` — two follow-ups from the wider suites.** See "Defects found
   on the way".

**Wider evidence, all at the final head:**
- `cargo tl`: 6,095 passed.
- The org security integration binaries (`integration_nrpc_protected`,
  `org_rpc_streaming`, `org_admission_wire`, `org_admission_gate`,
  `nrpc_streaming_gate`, `org_ownership`, `nrpc_call_hijack`,
  `capability_exposure_revocation`, `org_scoped_cross_process`): 147 passed.
- SDK lib plus `enrollment_org` and `org_streaming`.
- Python `test_org_live.py` (25), Node `org_live.test.ts` (13), and the Go org
  tests.
- Leaf native (491) plus wasm32 check and clippy.
- Strict clippy on lib/bins (all features, default, no-default-features) and
  on all targets.
- Per-member clippy for SDK, CLI, deck, Node and Python.
- Rustdoc for the root crate and the SDK, with `-D warnings`.

**Defects found on the way:**
- **Fixed in `65433a365`:** the floor service answered a stranger org's
  pushed bundle `Refused` instead of `NotMember`. Nothing was stored either
  way, but the answer was wrong; `enrollment_org`'s live floor test caught it.
- **Fixed in `65433a365`:** `org_ownership` pinned two store subscribers per
  installed node, but the registry's grant feed makes three. Teardown must
  still remove all of them.
- **Fixed in slice 2:** the leaf boundary test's exhaustive vocabulary needed
  the new `GrantRevocationBundle` carrier type. The leaf CI floors went to 488
  native and 30 for `org_streaming_lifecycle`.
- **Fixed in slice 5:** shard counts must match across language drivers (4,
  the Python and Node default), or capability announcements strand on unpooled
  shards. The Go driver comment and the matrix protocol record it.

## Risks

- **The `org-matrix` CI job has never run on Linux.** It builds libnet, the
  wheel and the napi addon in one job; if it is too slow or flaky, split the
  builds or reuse artifacts from the Go, Python and Node jobs. Fallback: mark
  it non-required while it settles. Do not skip cells.
- **State version 2 on downgrade.** A node holding a grant revocation writes
  `revocation-state.json` v2, which older builds refuse. That is intended
  (fail-closed), but it should be called out in the release notes.
- **Freshness ages and the offline root.** A requirement shorter than the
  org's heartbeat cadence refuses that org's callers. Recommend an age that
  matches how often the root already signs (grants are re-issued at least
  every 30 days).
- **Relay is best-effort by design.** A revocation reaches B only when an
  honest A node calls B or A's operator pushes. The freshness requirement is
  the bound for providers that need one.
- **A grantee org can make a provider verify and write.** It can relay many
  distinct bundles it signed itself. This is bounded by the per-org caps,
  the 16-item, 4 KiB header, digest de-duplication and admission throttling,
  but a malicious grantee root can still cause sidecar writes at the call
  rate. Accepted, because that org can already load the provider by calling
  it.

## Not in scope

- Cross-org and granted-scope sensing (item 3, owner decision).
- Relay and freshness in the browser leaf. It applies grant revocations and
  floors fed through its control plane, but neither relays nor enforces
  freshness.
- The freshness policy over the Go/C ABI (available in Rust, the SDK, CLI,
  Python and Node).
- Withdrawing a revoked capability grant's private announcements: admission
  refuses the call; discovery visibility is not admission.
- Release notes. They belong to the next release's notes. Points to cover:
  `Mesh::start()` now returns `Result`; `AdmissionDenied` gained three
  variants; state v2; and the new CLI verbs `org revoke-grant` and
  `org heartbeat` plus `up --require-fresh-revocations`.
