# OA-3 Exit Gate — grant-scoped private discovery (§3.5)

Certification that the OA-3 exit gate of `docs/plans/ORG_CAPABILITY_AUTH_PLAN.md`
§3.5 is closed. Each criterion below is mapped to its exact witnessing test(s).
Paths are under `net/crates/net/src/adapter/net/` (behavior layer) and
`net/crates/net/tests/` (integration). Rotation is the hard-cutover procedure of
§3.4 (dual-publish deferred — see the end of this doc).

## Four authority distinctions (read first)

These are deliberately separate properties; conflating them is the classic
private-discovery error. Rotation/expiry act on some and not others.

- **Raw decryption** — whether a *key* can AEAD-open a *ciphertext*
  (`ScopedCapabilityAnnouncement::open_with`). A retained old key CAN still open
  ciphertext historically captured under it; rotation cannot revoke retained key
  material. What it cannot do is open a *successor* audience's envelope (fresh
  key) — per-grant key independence.
- **Accepted ingest (discovery)** — whether `verify_scoped_ingest` admits an
  envelope into the scoped-discovery store. This is much stronger than raw
  decryption: it additionally requires a valid grant/owner-cert against live
  floors, freshness (grant + envelope windows), the descriptor↔capability bind,
  and the exact audience selection. An *expired* grant is refused
  (`GrantInvalid`) even though its key could still decrypt its own old ciphertext.
- **Cached knowledge** — what a node already learned and stored. Rotation/expiry
  do not erase already-stored records; query-time currentness (the revocation
  floor, and — for granted records — the exact installed consumer grant authority
  `grant_id + audience handle + verified grant signature`) hides them at read
  time, and sweeps reclaim them, but the crypto cannot un-teach a peer.
- **Invocation authority** — a SEPARATE authorization path, never implied by
  discovery. Public nRPC remains governed by the legacy `may_execute` path.
  Organization-protected nRPC is governed by the distinct `OrgAdmission` path
  using a valid request-bound organization proof, local capability possession,
  and provider policy. These are NOT conjunctive: a valid protected call can be
  accepted even when target-wide legacy allow-list aggregation makes
  `may_execute` false — `may_execute` is not an authority requirement for
  protected organization calls. Discovery or possession of an audience credential
  never confers invocation authority (OA-2 admission / OA-4 end-to-end).

## §3.5 criteria → witnesses

**1. Golden vectors incl. the zero-sentinel owner AD**
- `org_scoped_ann.rs::golden_vector_pins_the_full_encoded_envelope` — full
  encoded GRANTED envelope (framing + ciphertext + outer ed25519 sig) frozen.
- `org_scoped_ann.rs::owner_golden_vector_pins_the_zero_sentinel_envelope` —
  full encoded OWNER envelope frozen, asserting the encoded grant id is the
  all-zero sentinel (OA3-6).
- `org_scoped_ann.rs::owner_audience_ad_uses_the_reserved_zero_sentinel_and_is_disjoint_from_granted`.

**2. AD-transplant matrix**
- `org_scoped_ann.rs::open_with_transplanted_aad_fails` — reopen under a
  different `grant_id` / bumped `generation` in the AD → `SealOpenFailed`.
- `org_scoped_ann.rs::open_with_wrong_key_fails`.
- Handle transplant at ingest: `org_scoped_ingest.rs::owner_ingest_rejects_wrong_owner_and_wrong_handle`.

**3. Generation freshness + expiry + dedup**
- Monotone generation / stale-replay: `org_scoped_store.rs::a_swept_newer_generation_cannot_be_revived_by_an_older_one`,
  `::ingest_reports_insert_update_and_stale`.
- Expiry-safe queries: `org_scoped_store.rs::queries_exclude_expired_entries_before_any_sweep`,
  `::sweep_removes_only_expired_entries`.
- Ingest freshness: `org_scoped_ingest.rs::owner_ingest_rejects_an_expired_envelope`,
  `::granted_ingest_rejects_an_expired_envelope`.
- Relay dedup key `(provider, grant_id, audience_handle, generation)`:
  `org_scoped_relay.rs::gate_admits_a_fresh_identity_once`,
  `::gate_expires_on_the_local_horizon`,
  `::gate_is_bounded_fail_closed_and_never_evicts_active`,
  `::decide_relay_admits_once_and_forwards_below_the_cap`.

**4. Scoped-fold isolation (Owner ↔ Grant ↔ Public mutually invisible, invisible to unscoped)**
- `org_scoped_store.rs::owner_and_grant_partitions_are_mutually_invisible`.
- `org_scoped_store.rs::public_scope_is_refused` (Public never enters the scoped store).
- Invisible to the plaintext fold: `integration_nrpc_protected.rs::owner_scoped_service_ships_only_inside_the_encrypted_owner_envelope`,
  `::owner_scoped_residue_is_stripped_from_the_plaintext_announcement`. The scoped
  store is a structurally separate surface from `capability_fold`.

**5. Owner-audience internal case**
- `integration_nrpc_protected.rs::an_inbound_owner_scoped_announcement_is_verified_and_stored`.
- `integration_nrpc_protected.rs::owner_scoped_service_ships_only_inside_the_encrypted_owner_envelope`.
- Unit: `org_scoped_ingest.rs::owner_ingest_happy_path`.

**6. Dual size bounds at builder AND decoder, typed `DescriptorTooLarge`, never trimmed**
- Builder: `org_scoped_ann.rs::seal_rejects_oversized_descriptor`.
- Decoder: `org_scoped_ann.rs::open_rejects_oversized_and_truncated_ciphertext`,
  `::decode_rejects_bad_version_length_and_bounds`.

**7. INVOKE-only grant holds no audience material by construction and cannot ingest**
- `org_grant.rs::capability_grant_invoke_only_roundtrip` (mints no secret;
  `discovery.is_none()`; `!permits_discover()`).
- `org_grant_registry.rs::invoke_only_grant_is_refused` (install refused
  `MissingDiscover`). With no audience material there is nothing to ingest with.

**8. Per-grant key independence + expired-grant-vs-new-audience**
- K1 cannot open K2: `integration_nrpc_protected.rs::overlapping_grants_emit_two_independently_decryptable_envelopes`
  (direct `open_with` cross-check), `org_grant.rs::discover_grant_always_mints_fresh_audience_material`.
- Across expiry/rotation: `org_scoped_ingest.rs::an_expired_grants_key_cannot_decrypt_a_freshly_issued_successors_envelope`
  (OA3-6) — expired G1/K1 → fresh G2/K2; K1 fails to AEAD-open G2's envelope
  directly; expired G1 refused `GrantInvalid`.

## Rotation (§3.4) → witnesses

**Owner audience (hard cutover):** `integration_nrpc_protected.rs::a_same_org_audience_rotation_refuses_the_stale_scoped_envelope`
— after a same-org authority replacement (K1→K2) the cached K1 envelope is
send-refused, a rebuild under K2 publishes E2, and E1 opens only under K1 / E2
only under K2.

**Granted audience (per-grant):** `org_scoped_ingest.rs::an_expired_grants_key_cannot_decrypt_a_freshly_issued_successors_envelope`,
`integration_nrpc_protected.rs::a_granted_envelope_never_outlives_its_grant`,
`org_grant_registry.rs::grant_active_for_emission_rechecks_window_issuer_and_target`.

**Observer / non-grantee recovers nothing:** `integration_nrpc_protected.rs::an_owner_scoped_announcement_floods_opaquely_through_a_relay_to_the_audience`,
`::a_granted_capability_floods_opaquely_through_a_relay_to_the_grantee`
(the relay is the non-grantee observer — stores nothing), and
`::an_inbound_granted_announcement_is_verified_and_stored` (a node without the
pair stores nothing).

## Dual-publish — deferred

Owner-audience rotation is a **hard cutover** (single owner credential per node,
send-refused on mismatch). §3.4's graceful **dual-publish** transition is
explicitly deferred as future operational tooling: it is not part of the v1 mesh
protocol or this exit gate, and if built must be an explicit **planned**-rotation
mode only, never the emergency/compromise path (which must never dual-publish the
compromised old key). Adding it would reopen the signed owner-emission,
authority-identity, cached-ciphertext, and send-coherence seams for an
availability feature §3.5 does not require.
