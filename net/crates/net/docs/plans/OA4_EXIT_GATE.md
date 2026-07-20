# OA-4 Exit Gate — end-to-end composition witnesses

Certification that the OA-4 phase of `docs/plans/ORG_CAPABILITY_AUTH_PLAN.md`
(§OA-4) is closed. OA-4 is a **composition-proof** phase: no authority
semantics, wire objects, stores, or SDK surfaces changed. The one production
touch is the `#[cfg(test)]`-only RED seam (block 4), which compiles out of every
non-test build.

Each requirement below is mapped to its witnesses, tagged by evidence tier:

- **T1 — live two-node transport** (`tests/integration_nrpc_protected.rs`): a real
  `MeshNode::call` (or a live emission/receive) over the actual transport.
- **T2 — provider bridge** (`src/adapter/net/mesh_rpc.rs` lib tests, via
  `deliver_rpc_inbound_for_test` → `admit_and_dispatch_protected` →
  `verify_org_admission` + the real replay guard): synthetic inbound frames for
  adversarial states the honest caller API cannot produce.
- **T3 — pure authority / engine / crypto unit** (referenced, not repeated): pins
  the exact typed reason, boundary arithmetic, or predicate.
- **Prior OA3** — a witness closed in an earlier OA phase (OA3-4b2 / OA3),
  referenced not repeated: a component, live, ingest, registry, or relay witness
  in the already-signed-off suite. NOT a pure unit (that is T3), and NOT part of
  this phase's live matrix.

Live/bridge tests prove handler darkness and coarse wire denial; T3 pins the
exact reason.

## Four authority distinctions (read first)

- **Discovery / possession** — a node learned of and can open a capability
  announcement. Confers NO invocation authority.
- **Invocation authority** — a request-bound `OrgAdmission` decision
  (`verify_org_admission`): a valid membership + dispatcher-grant scope + (cross-
  org) capability grant + freshness/floors + binding + replay + provider policy.
- **Legacy `may_execute`** — the v0.4 allow-list gate. Public nRPC is governed by
  it; org-protected nRPC is NOT (it uses the distinct `OrgAdmission` path). The two
  are not conjunctive — a protected call is admitted even when target-wide
  allow-list aggregation makes `may_execute(P,C,S)` false (block 4).
- **Provider possession precheck** — `has_local_capability` (tag presence only, no
  allow-lists) is the sole fold precondition for a protected dispatch; the
  admission engine is the load-bearing authority.

---

## Block 1 — Internal (P₁, `OwnerDelegated`)

| Requirement | Tier → witness |
|---|---|
| Valid dispatcher grant ACCEPTED | **T1** `live_two_node_owner_delegated_admit` · **T3** `org_admission::owner_delegated_happy_path_admits`, gate `mesh_rpc::protected_owner_delegated_call_admits_end_to_end` |
| Membership-only DENIED (`DispatcherGrantScope`) | **T1** `live_two_node_owner_delegated_membership_only_denied` · **T2** `owner_delegated_admission_denial_matrix` (row 1) |
| Copied proof DENIED (`MemberBindingMismatch`) | **T2** matrix (row 2) · **T3** `org_admission::tofu_member_binding_rejects_a_relayed_proof` |
| Wrong callee/target DENIED (`BindingInvalid`) | **T2** matrix (row 3) · **T3** `org_admission::binding_rejects_a_transplanted_call` |
| Wrong capability DENIED (`BindingInvalid`) | **T2** matrix (row 4) · **T3** `org_admission::capability_mismatch_when_invoked_tag_differs` |
| Wrong body/digest DENIED (`BindingInvalid`) | **T2** matrix (row 5) · **T3** `org_admission::binding_rejects_a_transplanted_call` |
| Expired proof DENIED (`ProofExpired`) | **T2** matrix (row 6) · **T3** `org_admission::expired_proof_is_refused` |
| Unexpected capability grant DENIED (`UnexpectedCapabilityGrant`) | **T2** matrix (row 7) |
| Grantee mismatch DENIED (`GranteeMismatch`, acting ≠ owner) | **T2** matrix (row 8) |
| Missing header DENIED | **T2** matrix (row 9) |
| Multiple headers DENIED | **T2** matrix (row 10) |
| Replay refused (real replay guard) | **T2** matrix (replay control) · **T3** `org_admission::replay_then_collision_are_distinguished` |
| Floored-after-reload DENIED at the live gate (`MembershipRevoked`) | **T1** `live_two_node_owner_delegated_floor_survives_restart_denies` · **T3** `org_admission::revocation_floor_kills_the_membership` |
| Public capabilities unchanged | **T1** `live_two_node_public_capability_unchanged_beside_protected`, `live_two_node_public_handler_never_sees_proof_header` |
| `may_execute` pinned unchanged | **T3/NODE** `org_ownership::may_execute_is_identical_with_and_without_verified_cert` |
| OA-1 restart chain end-to-end (floor 5 → bundle 3 → restart → gen-4 denied) | **T1** `live_two_node_owner_delegated_floor_survives_restart_denies` (through the live admission gate) · **T3/NODE** `org_ownership::floors_gate_ingest_and_survive_restart_with_lower_valid_bundle` (projection level) |

---

## Block 2 — Cross-org invocation (P₂, `CrossOrgGranted`)

| Requirement | Tier → witness |
|---|---|
| Full CrossOrgGranted ACCEPT over live transport | **T1** `live_two_node_cross_org_granted_admit` · **T2** gate `mesh_rpc::protected_cross_org_call_admits_end_to_end` · **T3** `org_admission::cross_org_happy_path_admits_with_four_party_attribution` |
| Four-party audit attribution (five-field `Admitted` via `RpcContext::org_admission`) | **T1** `live_two_node_cross_org_granted_admit` (caller S / acting A / provider-org B / provider P₂ / capability C; header stripped; reply to S) · **T3** `org_admission::cross_org_happy_path_admits_with_four_party_attribution` |

**Eleven-denial matrix** — `T2` `cross_org_admission_denial_matrix` (each row handler-dark), plus a T1 valid-but-unauthorized denial and the T3 exact-reason pins:

| # | Denial (typed reason) | Tier → witness |
|---|---|---|
| 1 | Wrong grantee org (`GranteeMismatch`) | **T2** matrix (row 1) — OA-4 addition |
| 2 | Foreign issuer (`ForeignIssuer`) | **T2** matrix (row 2) · **T1** `live_cross_org_any_node_owned_by_reuse_and_deny` (non-B deny = valid-but-unauthorized) · **T3** `org_admission::cross_org_mode_check_matrix` |
| 3 | Insufficient rights (`InsufficientRights`) | **T2** matrix (row 3) · **T3** `cross_org_mode_check_matrix` |
| 4 | Missing capability grant (`MissingCapabilityGrant`) | **T2** matrix (row 4) · **T3** `cross_org_mode_check_matrix` |
| 5 | Wrong target (`TargetNotCovered`) | **T2** matrix (row 5) · **T3** `cross_org_mode_check_matrix` |
| 6 | Wrong capability (`CapabilityMismatch`) | **T2** matrix (row 6) · **T3** `capability_mismatch_when_invoked_tag_differs` |
| 7 | Wrong body/digest (`BindingInvalid`) | **T2** matrix (row 7) · **T3** `binding_rejects_a_transplanted_call` |
| 8 | Expired proof (`ProofExpired`) | **T2** matrix (row 8) · **T3** `expired_proof_is_refused` |
| 9 | Copied proof (`MemberBindingMismatch`) | **T2** matrix (row 9) · **T3** `tofu_member_binding_rejects_a_relayed_proof` |
| 10 | Missing header | **T2** matrix (row 10) |
| 11 | Multiple headers | **T2** matrix (row 11) |

Separate from the eleven-row bridge matrix (per the tier ruling — these are NOT
among the eleven adversarial rows):

| Requirement | Tier → witness |
|---|---|
| Missing local tag / unregistered (OA-4 addition) | **T1** `live_two_node_protected_missing_local_capability_denies` |
| Replay refused (real replay guard) | **T2** `cross_org_admission_denial_matrix` (replay control: an admitted V1 re-sent identically → refused) · **T3** `replay_then_collision_are_distinguished` |
| Floored membership (`MembershipRevoked`) | **T3** `revocation_floor_kills_the_membership` — the membership-floor check runs AFTER mode validation and is shared by both modes; its live floor-persistence composition is the Block-1 restart witness. **Not** claimed live-witnessed for cross-org. |

| `AnyNodeOwnedBy(B)` requirement | Tier → witness |
|---|---|
| Reuse ACCEPTED against a SECOND B-owned node | **T1** `live_cross_org_any_node_owned_by_reuse_and_deny` |
| DENIED against a non-B node | **T1** `live_cross_org_any_node_owned_by_reuse_and_deny` (ForeignIssuer at the C-owned provider) · **T3** `org_grant::target_scope_coverage_matrix` (the `covers()` predicate) |

Note on replay tier: the honest caller API cannot resend an identical `call_id`,
so replay is witnessed through the REAL replay guard via the provider bridge
(deliver identical admitted frame twice → refused). This is the substantive
property; a `MeshNode::call`-level replay would add no provider-side evidence.

---

## Block 3 — Private discovery (`GrantedAudience` on P₂)

The OA-4 additions compose live private discovery (0x0C04 provider emission →
consumer receive/open/store) with cross-org invocation; the remaining rows are
referenced to the closed OA3-4b2 / OA3 witnesses.

| Requirement | Tier → witness |
|---|---|
| DISCOVER\|INVOKE resolves AND calls exact P₂ | **T1** `live_granted_audience_discovers_then_invokes` (live provider emission → resolve → live invoke) |
| DISCOVER-only resolves but cannot invoke (decrypt-without-invoke) | **T1** `live_granted_audience_discover_only_resolves_but_cannot_invoke` (`InsufficientRights`) |
| Wrong dispatcher — resolves but invocation denied | **T1** `live_granted_audience_wrong_dispatcher_resolves_but_invocation_denied` (`DispatcherGrantScope`, post-discovery) |
| Provider policy final | **T1** `live_granted_audience_provider_policy_final` · `live_two_node_policy_veto_denies` |
| INVOKE-only holds no audience material | **T3 / composed structural** `invoke_only_grant_carries_no_discovery_material` (a `#[test]` asserting only issuance shape: `secret == None`, `!permits_discover`, `permits_invoke`), `org_grant_registry::invoke_only_grant_is_refused`, `org_grant::capability_grant_invoke_only_roundtrip`. Its location in the integration file does not make it live transport. |
| Private service absent from plaintext CAP-ANN | **Prior OA3 component** `a_granted_service_ships_only_inside_an_encrypted_grant_envelope`, `serve_rpc_granted_is_dispatchable_but_undiscoverable_without_a_grant` |
| No grant ⇒ no enumeration | **Prior OA3 ingest/component** `an_inbound_granted_announcement_is_verified_and_stored` |
| Copied credential by wrong grantee | **T3 pure unit** `granted_ingest_rejects_wrong_grantee` |
| Wrong handle / capability | **T3 pure unit** `granted_ingest_binds_descriptor_to_the_grant_capability`, `owner_ingest_rejects_wrong_owner_and_wrong_handle` |
| Stale registration | **Prior OA3 registry/component** `removing_a_provider_grant_refuses_the_cached_granted_envelope` |
| Observer recovers nothing | **Prior OA3 live relay** `a_granted_capability_floods_opaquely_through_a_relay_to_the_grantee` |
| AD-transplant fails | **T3 pure unit** `org_scoped_ann::open_with_transplanted_aad_fails` |
| Post-rotation decryption failure | **Prior OA3 component** `a_same_org_audience_rotation_refuses_the_stale_scoped_envelope` · **T3 AEAD/ingest** `org_scoped_ingest::an_expired_grants_key_cannot_decrypt_a_freshly_issued_successors_envelope` |

---

## Block 4 — Seam-red (review-7)

Witness: **T2** `seam_red_org_admission_is_load_bearing` (lib, via
`deliver_rpc_inbound_for_test` + a real protected dispatcher + real fold/handler).
The provider carries TWO distinct fold entries: C (class-0 self entry, empty
allow-lists) and an unrelated D (separate native class `0xD00D`, `nrpc:d`,
`allowed_nodes = [0xDEAD]`).

| Requirement | Witness |
|---|---|
| `may_execute(P,C,S)` false via UNRELATED-D target-wide aggregation, `has_local_capability(P,C)` true | before/after `may_execute` delta: true before the D entry (C unrestricted) → false after (only D changed), with `has_local_capability` true and `may_execute(P,D,S)` false |
| Valid org proof for C → ACCEPTED through `OrgAdmission` | positive control (admits) |
| Invalid / no proof for C → denied, handler dark | negative control |
| Public D still governed by `may_execute` | D's restrictive `may_execute` assertion here **+** the live public-path behavior in Block 1 (`live_two_node_public_capability_unchanged_beside_protected`). This test does not itself execute a public-D call. |
| Private C absent from plaintext | N/A here — the RED's C uses `serve_rpc_protected`, which intentionally has PUBLIC discovery; the RED is about INVOCATION authority independent of visibility. Plaintext-private capabilities are the `serve_rpc_granted` / OA3 / Block-3 witnesses. |
| **RED**: disable ONLY `verify_org_admission` → the same unauthorized C request runs | RED control (`admits: 2 → 3`) |

### RED seam contract (`#[cfg(test)]`-only, compiled out of production)

- `RegisteredRpcService.red_witness_disabled` field (default `false` in every
  production constructor) + `red_witness_admission_disabled()` accessor +
  `with_red_witness_disabled()` builder;
- `UnaryAdmission::ProtectedRedWitnessDisabled` variant + `visibility()` arm +
  `serve_rpc_unary_impl` match arm;
- `MeshNode::serve_rpc_protected_red_witness_disabled` (requires an installed
  authority + org-protected mode);
- the dispatch bypass in `admit_and_dispatch_protected`, placed right BEFORE the
  `verify_org_admission` call — AFTER the bridge origin bind, authenticated-caller
  resolution, `has_local_capability` possession, request decode/digest, and
  provider self-verification. It replaces ONLY the admission engine.

**Containment (source scan):** every `red_witness*` / `ProtectedRedWitnessDisabled`
symbol is under `#[cfg(test)]`. No cargo feature, no public API, no
global/static/env toggle. `cargo build --lib --no-default-features` and
`cargo clippy --lib --features cortex` compile WITHOUT the seam — a shipping build
cannot carry the bypass.

---

## Frozen invariant

`may_execute` (`capability_bridge.rs`) is byte-for-byte unchanged across all of
OA-4 (`git diff 347860feb..HEAD -- …/capability_bridge.rs` is empty). Discovery
or possession of an audience credential never confers invocation authority; the
`OrgAdmission` engine is the load-bearing authority for protected services,
independent of the legacy `may_execute` verdict (block 4 RED).

## Then STOP

Per §OA-4: no further organization work without a named consumer or a measured
failure.
