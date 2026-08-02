# S4B review packet — authenticated relay enforcement + export activation

Reviewed by behaviour and provenance against the current tree. There is
no clean S4B commit boundary and none is invented here: the slice's
content is spread across five commits because S4B was begun before S4A
was signed. Each behaviour below names its current production function,
its witness, and the commit that introduced or repaired it.

## Pinned revisions

```text
review base:
  71581680890a08198efd0895ec4450655bc0dacc
  accepted S4A additive substrate state

candidate head:
  01ebae486845a58e8afc969eec6d5a3c3df395b1
  canonical export repair + subnet_org_boundary

historical intermediate:
  186d1667fd2f5b6f80e057d65686abfcde89ded4
  HOLD as written — export policy aliases on the u16 wire hash
```

Provenance of the S4B surface:

```text
7d34ecfbbddee3058bc7ac52ab3ee593e8bf9a08  relay enforcement (S4B, partial)
cf1cc1cd375050c404528e34097481af75deee59  S4A blockers 2/3/4 + API typing
117b12558fe345f2e478821e8bc703b4334916dd  allocation-free sealing
d365df7ead182c12fe34e5bf55ec8c516e179bf7  off-path scope index
71581680890a08198efd0895ec4450655bc0dacc  authority widening + immutability
186d1667fd2f5b6f80e057d65686abfcde89ded4  Exported activation + source pins
6fa7ab8dd92481e4e0e8e8636263cf5f28b95b68  canonical export key (breaking)
01ebae486845a58e8afc969eec6d5a3c3df395b1  cross-plane composition contract
```

`19edebe615b89fbadca612d23170fa5118d4b632` **is** in the candidate's
ancestry — the earlier claim that it "is not part of this candidate"
was graph-false. The correct statement: the exact historical revision
remains HOLD as written, and later additive commits repair aggregate
behaviour. The same holds for `186d1667f`.

## Behaviour map

| # | Behaviour | Production site | Witness | Commit |
|---|---|---|---|---|
| 0 | **A valid protected hop is forwarded at all** | `NetRouter::add_direct_route` installing identity-qualified routes at session establishment; `relay_protected_hop` | `a_valid_protected_hop_is_forwarded_and_retagged` + `a_valid_hop_without_route_authority_is_not_forwarded` | this repair |
| 1 | Protected-frame classification and downgrade refusal | `mesh.rs` dispatch arm on `ROUTE_HOP_MAGIC`; protected-mode branch refusing untagged legacy relay | `subnet_gateway_auth.rs`; `a_legacy_routing_packet_is_not_an_envelope` | 7d34ecfbb |
| 2 | Exact ingress-session resolution | `relay_protected_hop` step 1 — `parse_prefix` → `session_id_to_node` → session id equality | `subnet_gateway_auth.rs` | 7d34ecfbb, cf1cc1cd3 (`parse_prefix` typing) |
| 3 | MAC verification before replay mutation | `NetSession::open_route_hop` — `route_hop::open` then `admit` | `every_transcript_field_is_covered`; `a_tag_does_not_verify_under_the_reverse_direction_key` | 7d34ecfbb |
| 4 | Atomic sequence admission in that session | `HopReplayWindow::admit` | `a_duplicate_exactly_one_window_behind_is_refused`; `window_boundary_matrix`; `a_huge_advance_neither_panics_nor_remembers` | cf1cc1cd3 |
| 5 | Exact ingress attachment | `SubnetContextStore::get_for_session` (session-validated) | `subnet_session_auth.rs`; `attachment_is_the_exact_admitted_point_not_the_grant_scope` | 7d34ecfbb |
| 6 | Identity/session-qualified egress resolution | `RoutingTable::lookup_authenticated`; `RouteEntry.next_hop_id` | `route_identity_survives_address_change_and_resists_address_reuse`; `a_legacy_route_is_not_an_authenticated_next_hop` | cf1cc1cd3 |
| 7 | Exact egress attachment | `get_for_session` on the egress session | `forwarding_uses_attachment_not_scope` | 7d34ecfbb |
| 8 | Immutable boundary + local ROUTE/EXPORT decision | `VerifiedGatewayContextSet::authorize_transition_counted`; `SubnetBoundarySet` | `subnet_gateway_local_auth.rs` (20 tests) | cf1cc1cd3, d365df7ea, 715816808 |
| 9 | Authorization before TTL mutation | `relay_protected_hop` — `fwd_header.forward()` only after step 5 | `subnet_gateway_auth.rs` | 7d34ecfbb |
| 10 | Untouched inner end-to-end bytes | `opened.inner` copied through; inner `NetHeader.hop_ttl` never written | `seal_open_round_trips_and_preserves_inner_bytes` | 7d34ecfbb |
| 11 | Re-tagging under the egress session | `NetSession::seal_route_hop_into` with the egress session's tx key | `subnet_gateway_auth.rs` | 117b12558 |
| 12 | Fixed worker buffer | `FORWARD_BUF: RefCell<[u8; PROTECTED_FORWARD_BUF_SIZE]>`, const-initialized; size is exactly `MAX_PACKET_SIZE` with a const assertion | `the_worker_forward_buffer_is_fixed_not_growable`; `subnet_route_hop_alloc.rs` | 117b12558, 186d1667f |
| 13 | `try_send_to` egress | `NetSocket::try_send_to` | `relay_protected_hop_does_not_allocate_per_packet` | 117b12558 |
| 14 | Drop on `WouldBlock`, no task or owned-packet queue | `relay_protected_hop` step 6 — log and drop | source pin (no `tokio::spawn`, no `to_vec`) | 715816808 |
| 15 | Canonical-`u64` `Visibility::Exported` | `MeshNode::subnet_visible` + `SubnetGateway::export_targets` | `exported_admits_exactly_the_declared_targets`; `export_policy_does_not_alias_across_a_wire_hash_collision` (gateway and mesh); `export_lookups_use_canonical_channel_identity` | 186d1667f, repaired 6fa7ab8dd |
| 16 | Absent / empty / unknown export denial | `subnet_visible` `Exported` arm | `exported_admits_exactly_the_declared_targets` (empty list, absent rule, unresolved peer subnet) | 186d1667f |
| 17 | Organization / subnet / provider orthogonality | `authorize_transition` + `verify_provider_authority` + `verify_org_admission` | `subnet_org_boundary.rs` (14 tests) | 01ebae486 |
| 18 | Positive and inverse witnesses | — | baseline plus 8 same-org removals and 7 cross-org inverses, each with a distinguishable reason | 01ebae486 |
| 19 | Source-pin mutation controls | `protected_forward_allocation_pins` | each pin verified to turn RED under the regression it guards | 186d1667f, 6fa7ab8dd |
| 20 | Test and CI results | — | see below | — |

## Gate at the candidate head

These are **local** runs, with the exact commands. They are not the
candidate-wide CI result: the `integration-net-core` job is one of five
integration families plus separate unit, clippy, doc, and SDK jobs, so a
count from the command below is not "the gate". Read exact-head CI for
the authoritative per-job numbers.

```text
# suite list scraped from the integration-net-core job in ci.yml
cargo nextest run --features net $(--test flags from integration-net-core)
  → 376 tests run: 376 passed, 0 skipped   (48 suites)

cargo test --features net --lib
  → 5470 passed, 0 failed, 1 ignored

cargo fmt --check                                            clean
cargo clippy --all-features --lib --bins -- -D warnings      clean
cargo clippy --all-features --all-targets -- -D warnings \
  -A clippy::unwrap_used -A clippy::expect_used \
  -A clippy::undocumented_unsafe_blocks \
  -A clippy::multiple_unsafe_ops_per_block                   clean
RUSTDOCFLAGS="-D warnings" cargo doc --features net --no-deps  clean
git diff --check                                             clean
integration-guard scrape (every tests/*.rs pinned)           clean
```

An earlier revision of this packet cited "374 integration tests" as the
candidate-wide gate. That was one local command's count presented as
something broader; the exact CI step reports its own numbers and those
are what a reviewer should read.

CI registration added under `integration-net-core`:
`subnet_route_hop_alloc`, `subnet_org_boundary`.

## Mutation controls — reported, for independent rerun

These are **maintainer-reported** controls, not candidate-bound
evidence. Each was performed by editing the named production line,
running the named test, observing RED, and reverting. Reproduce by
applying the mutation at the candidate head and running the command:

```text
subnet/id.rs common_ancestor → zero-terminating form
  cargo test --features net --test subnet_gateway_local_auth interior_zero
  → RED: unwrap_err() on Ok(()) — the false authorization

tests/subnet_route_hop_alloc.rs: allocating seal in the measured loop
  cargo test --features net --test subnet_route_hop_alloc
  → RED: 512 allocations / 512 hops

mesh.rs relay → seal_route_hop (allocating)
  cargo test --features net --lib protected_forward_allocation_pins
  → RED: "must not call the allocating seal_route_hop"

mesh.rs export lookup → u64::from(channel.wire_hash())
  cargo test --features net --lib export_lookups_use_canonical
  → RED: "must resolve export targets by canonical ChannelHash"

router.rs add_direct_route → add_route (the dead-forwarding regression)
  cargo test --features net --test subnet_gateway_auth a_valid_protected_hop
  → RED: "exactly one hop must be forwarded"

deck.rs channel_canonical_hash → cfg.channel_id.hash()
  cargo test --features net --lib deck::tests::deck_with_mesh
  → RED: "a prefix-covered request must resolve to its own canonical hash"

set.entries = Box::new([])
  → E0616: field `entries` of struct `VerifiedGatewayContextSet` is private
```

## Explicitly open

```text
production relay allocation measurement:
  deferred to the planned E2E harness

  The witness in tests/subnet_route_hop_alloc.rs measures the sealing
  PRIMITIVE, not relay_protected_hop. It stays green if the production
  branch regresses to the allocating API or allocates inside the
  measured path. The source pins are interim structural guards and are
  NOT equivalent evidence.

  Measuring the production branch at runtime needs the E2E harness: the
  relay runs on a tokio worker thread and reception allocates per
  packet, so neither a global counter nor a per-packet marginal is a
  clean signal today. This becomes a hard requirement at the E2E stage.
```

Not claimed by this packet, and unchanged: `19edebe61` remains HOLD as
written; `186d1667f` remains HOLD as written; S5 and the live E2E suite
have not been started.

## Scope note on `subnet_org_boundary.rs`

It is a focused cross-plane composition contract calling the production
subnet and org/provider gate functions in the protected path's real
order. It is not the live multi-node E2E witness: nothing in it drives a
datagram through a relay or a call through nRPC dispatch. The channel
plane is not represented by a boolean there — its gates are pinned
directly by the `Visibility::Exported` truth table and the wire-hash
collision inverses, and the file adds only non-substitution assertions
for it.
