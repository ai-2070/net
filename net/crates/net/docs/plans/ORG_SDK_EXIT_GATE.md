# OSDK Exit Gate — the organization capability verb facade

Certification that [`ORG_CAPABILITY_SDK_PLAN.md`](ORG_CAPABILITY_SDK_PLAN.md)
(v0.4) is implemented and closed. Every claim in that plan is mapped below to
the witness that checks it.

The facade is a **verb layer**, not an authorization model: it never admits, and
every local check can only refuse to send. `verify_org_admission` and its step
order, the serve gates, all wire objects, headers and status codes, the replay
guard, the RED seam, `OrgProofIntent`, and `may_execute` are untouched.

**Public surface — five top-level concepts.** `OrgCredentials`, `OrgClient`,
`OrgAccess`, `OrgCaller`, `OrgSdkError` (with its public domain enums
`OrgCredentialError` / `OrgDiscoveryError`).

```rust
let org = mesh.org(credentials)?;
let customer: Customer = org.call("customer.read", &request).await?;

mesh.serve_org("customer.read", OrgAccess::Granted,
    |caller: OrgCaller, req: GetCustomer| async move { read_customer(caller, req).await })?;
```

Witness locations: `sdk/src/org/tests.rs` (S0), `tests_call.rs` (S1),
`tests_serve.rs` (S2), `tests_live.rs` (S3), plus a unit module in
`sdk/src/org/call.rs` for the denial decode.

## Evidence tiers

- **L — live two-node transport** (`tests_live.rs`): two real meshes, a real
  handshake, real private announcement propagation, a real `MeshNode::call`
  through `verify_org_admission` into the handler.
- **N — node-level**: one real `MeshNode` with a real installed authority, the
  real scoped-discovery store, the real ingest path (envelopes built by the
  canonical builders and admitted by `verify_scoped_ingest`), and the real
  registration/emission projection.
- **U — pure unit**: predicates, projections, and decode paths.
- **Prior** — a witness closed in OA-1..OA-4, referenced not repeated.

---

## §1 `OrgCredentials` — construction, binding, lease

| Requirement | Tier → witness |
|---|---|
| Structure + signatures at construction; a coherent set assembles | **U** `credentials_accept_a_coherent_set_without_checking_windows` |
| Windows are NOT a construction check (assemble before the window) | **U** `credentials_may_be_assembled_before_their_validity_window` |
| Dispatcher grant must empower the membership's member (admission step 7) | **U** `credentials_reject_a_dispatcher_grant_for_a_different_entity` |
| Membership and dispatcher must agree on the acting org (step 5) | **U** `credentials_reject_disagreeing_acting_orgs` |
| A wallet holds only grants naming its own org as grantee | **U** `credentials_reject_a_grant_issued_to_another_org` |
| Every audience secret matches exactly one held grant (§2.6 commitment) | **U** `credentials_reject_an_audience_secret_matching_no_held_grant` |
| No duplicate grant ids | **U** `credentials_reject_duplicate_grant_ids` |
| Signatures verify | **U** `credentials_reject_a_tampered_signature` |
| Container is non-serializable (type-level) + redacted `Debug` | **U** compile-time `AmbiguousIfSerialize` assertion in `credentials.rs`; `credentials_debug_is_redacted` |

**The binding relation** — all four conditions, each independently refused:

| Requirement | Tier → witness |
|---|---|
| Configured durable identity required | **N** `bind_refuses_a_mesh_without_a_durable_identity` |
| Installed node authority required | **N** `bind_refuses_without_an_installed_node_authority` |
| Authority owner org == membership org | **N** `bind_refuses_when_the_node_authority_belongs_to_another_org` |
| `membership.member` == mesh entity | **N** `bind_refuses_a_membership_for_a_different_entity` |
| A complete relation binds and leases the audience | **N** `bind_accepts_the_complete_relation_and_leases_the_audience` |
| Stage 2: a not-currently-installable DISCOVER grant fails the bind loudly, registry untouched | **N** `bind_refuses_a_grant_that_is_not_currently_installable` (surfaces canonical `GrantNotCurrent`) |
| Binding does NOT check membership windows (stage boundary) | **N** `an_expired_membership_refuses_at_call_time` (binds, then refuses at call) |

**The consumer-audience lease** (core lifecycle seam):

| Requirement | Tier → witness |
|---|---|
| Refcount across clients: one install, first drop removes nothing, last drop removes | **N** `lease_refcounts_across_clients_and_removes_only_on_the_last_drop` |
| A clone shares its client's guard | **N** same test |
| Never removes an installation it did not perform (`AlreadyPresent` ⇒ non-owning) | **N** `lease_never_removes_an_installation_it_did_not_perform` |
| A stale token cannot remove a successor under the same grant id | **N** `a_stale_lease_token_cannot_remove_a_successor_installation` |
| The mutex spans 0→1 and 1→0: a final release racing a new bind leaves the audience installed | **N** `a_final_release_racing_a_new_bind_leaves_the_audience_installed` (24 iterations) |

## §2 `OrgClient` — the call verb

| Requirement | Tier → witness |
|---|---|
| The facade builds the canonical nine-field intent — SameOrg (no grant, provider_owner_org == acting org) | **N** `plan_builds_the_canonical_same_org_intent` (all nine fields vs a hand-assembled reference) |
| …and cross-org (the matched grant rides it; provider_owner_org == issuer) | **N** `plan_builds_the_canonical_cross_org_intent` |
| Mode is inferred, never caller-specified | both intent tests (`Mode::SameOrg` / `Mode::Granted`) |
| Exact grant matching over the complete authority relation | **N** `a_grant_whose_target_scope_excludes_the_provider_does_not_authorize` |
| DISCOVER is never invocation authority — refused locally, no round trip | **N** `a_discover_only_grant_resolves_but_cannot_invoke` (`considered: 1`, zero targets) |
| Ambiguity is a typed error, never a silent pick | **N** `overlapping_grants_are_an_ambiguity_error` |
| Deterministic selection: lowest provider `EntityId`, stable | **N** `selection_is_deterministic_lowest_provider_id` (5 repeats) |
| Direct-session-only (E0.3): authorized-but-unreachable is distinct | **N** `an_authorized_but_unreachable_provider_is_reported_as_not_direct` |
| `NoAuthorizedProvider` considered-count semantics | **N** `nothing_discovered_reports_zero_considered` |
| Dispatcher scope excluding the capability refuses locally | **N** `a_dispatcher_scope_that_excludes_the_capability_refuses_locally` |
| Stage 3: a bound client refuses once its window closes | **N** `an_expired_membership_refuses_at_call_time` |
| Frozen proof TTL (30 s) owned by the SDK | asserted in `plan_builds_the_canonical_same_org_intent` |

**Private-only discovery:**

| Requirement | Tier → witness |
|---|---|
| The plaintext plane is never consulted, even when it genuinely carries the tag | **N** `the_public_plane_is_never_consulted` (injects a real public announcement, asserts `considered: 0`) |
| An owner record matches only the capability its descriptor declares | **N** `an_owner_record_matches_only_the_capability_it_declares` |
| Without the audience, a cross-org provider is not discoverable at all | **L** `live_cross_org_without_a_grant_discovers_nothing` (refused locally; provider handler stays dark) |

**Coarse denial decoding** (`sdk/src/org/call.rs` unit module):

| Requirement | Tier → witness |
|---|---|
| Every coarse reason decodes from a `0x0009` denial | **U** `every_coarse_reason_decodes_from_an_admission_denial` |
| An undecodable body still reports a denial, never an error-about-the-error | **U** `an_undecodable_denial_body_falls_back_to_denied` |
| An unknown reason byte is still a denial | **U** `an_unknown_reason_byte_is_still_a_denial` |
| Other server statuses stay transport errors — the facade never manufactures a denial | **U** `other_server_errors_are_not_admission_denials` |

## §3–4 `serve_org` — the provider verb

| Requirement | Tier → witness |
|---|---|
| `SameOrg` ⇒ OwnerDelegated + owner-scoped ENCRYPTED discovery | **N** `same_org_is_private_by_default` (one owner envelope ships; the tag is absent from the plaintext announcement while a public service beside it remains present) |
| `Granted` ⇒ CrossOrgGranted + grant-audience ENCRYPTED discovery | **N** `granted_is_private_by_default` |
| Public services unchanged beside a protected one | both tests (assert `nrpc:open` still in plaintext) |
| The provider still possesses the capability locally (§2.4a) | both tests (`has_local_capability`) |
| Protected registration requires an installed authority | **N** `serve_org_requires_an_installed_node_authority` |
| Duplicate registration refused without disturbing the first (E0.1) | **N** `a_duplicate_registration_is_refused_without_disturbing_the_first` |
| **Provisioning contract**: registration precedes the provider audience; nothing ships until it is installed; installing then emits exactly one envelope | **N** `granted_registration_precedes_provider_audience_installation` |
| `OrgCaller` is an exact five-field projection of `Admitted` | **U** `org_caller_is_an_exact_projection_of_admitted` |
| The verified facts reach a typed handler (which the existing typed wrapper drops) | **L** both live tests assert all five fields inside the handler |
| Missing admission inside a protected handler refuses loudly, never panics, never fabricates attribution | code path in `serve.rs`; unreachable through the real gate (only admitted protected calls dispatch) |

## §5 Composed exit — live, both modes

| Requirement | Tier → witness |
|---|---|
| The composed example works end to end, SameOrg | **L** `live_same_org_call_through_the_facade` |
| …and cross-org, with four-party attribution | **L** `live_cross_org_call_through_the_facade` |
| The call actually traversed canonical admission (asserted, not assumed) | both: the handler asserts caller / acting org / provider org / provider / capability, and that the handler ran exactly once |
| Response returns to the caller, typed | both (`Pong` equality) |

Both live tests model §3.4's out-of-band owner-audience pre-staging where the
owner plane is used — two independently adopted nodes each mint their own
credential, so same-org private discovery requires the operator's distribution
step, and the test performs it rather than pretending it is unnecessary.

**The design test.** `design_test_the_secure_path_is_short` and
`design_test_the_serve_path_is_short` (`tests_live.rs`) are the complete
application-facing surface, and they compile while naming NONE of:
`OrgProofIntent`, `OwnerDelegated`, `CrossOrgGranted`, the `OrgAudienceSecret`
commitment, `ScopedCapabilityAnnouncement`, `VerifiedScopedCapability`,
`CoarseAdmissionReason`, `GrantTargetScope`. The claim is checked by the
compiler, not asserted in prose.

---

## Core-touch inventory — exhaustive, verified

`git diff 07820a9de..HEAD -- src/` touches exactly four files:

1. **`org_grant_registry.rs`** — the ownership-safe lease seam:
   `GrantAudienceRecord.install_seq` (node-local, never on the wire, excluded
   from `records_identical` so idempotent re-install stays a no-op),
   `ConsumerAudienceLease`, `ConsumerAudienceInstall`.
2. **`mesh.rs`** — `install_consumer_grant_audience_leased` /
   `remove_consumer_grant_audience_if_current` (compare-and-remove under
   `consumer_grant_mu`, so replacement cannot race between the check and the
   removal); the promoted discovery queries
   `owner_private_capability_providers` / `granted_capability_providers`, with
   the two pre-existing `*_for_test` seams re-implemented on the same shared
   path so the production API is the load-bearing one; the install-sequence
   counter.
3. **`org_scoped_store.rs`** — `PrivateCapabilityProvider`, an owned projection
   of already-verified state.
4. **`org_scoped_ingest.rs`** — `descriptor_declares_capability`, read-only over
   already-ingest-verified bytes.

The pre-existing unleased install/remove API is unchanged in behavior and now
delegates to the leased implementation.

**Verified untouched** (`git diff` empty): `org_admission.rs`, `org_call.rs`,
`org_admission_replay.rs`, `org_admission_gate.rs`, `mesh_rpc.rs`, and
`capability_bridge.rs` — so `may_execute` is **byte-for-byte unchanged**, and no
admission step, wire object, header, status code, or replay behavior moved.

**No new bypass surface**: the diff adds no `#[cfg(test)]` admission bypass, no
`red_witness*` symbol, no feature flag, and no global toggle.

## Gates

- `cargo fmt --all -- --check` — clean.
- `cargo clippy --lib --features cortex` (core) — zero warnings.
- `cargo clippy --features net,cortex --lib --all-targets` (sdk) — clean.
- `cargo build --lib --no-default-features` — builds.
- Core lib (cortex): **5088 passed**, 0 failed.
- SDK lib: **191 passed**, 0 failed (46 org).
- `integration_nrpc_protected` **40**, `org_ownership` **31**,
  `org_admission_wire` **2** — all pass unchanged.
- The org suite was run six consecutive times to confirm no flakiness after the
  one load-sensitive assertion found in S2 was made deterministic.

## Then stop

Per the plan: no further org-SDK work without a named application consumer or a
measured failure. Everything in §Deferred — public-plane discovery,
`org.discover`, `OrgAdmin`, policy hooks, `OrgCaller.grant_id`, options objects,
file loading, bindings parity — stays deferred, and the low-level canonical APIs
remain available for anything the facade does not cover.
