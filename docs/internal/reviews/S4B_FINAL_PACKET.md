# S4B final packet (corrected) — learned multi-hop, replay, seams, and the round-3 production defects

Supersedes the packet at `dfe54dbeb` (HOLD — it overstated the learned
witness and the closure count). This packet maps the round-3 repairs
and corrects the evidence claims. The behaviour map of
`S4B_REVIEW_PACKET.md` still applies to the surfaces it covers.

## Pinned revisions

```text
historical dispositions, retained as written:
  19edebe615b89fbadca612d23170fa5118d4b632   HOLD as written
  186d1667fd2f5b6f80e057d65686abfcde89ded4   HOLD as written
  b94e819cc92415a59d63e443651ea6345847140c   HOLD as written
    (learned-route/replay repair — stale migration + one-gateway
     evidence; repaired by this candidate)
  dfe54dbeb1314f11035ab4d8d10ed8576a5df736   HOLD as written
    (prior final packet — overstated witness and closure)

candidate head (this packet's subject):
  35d63fe6d46a92b88e24f3ecbb85696fa9125c74
  round-3 production repairs + honest witnesses
```

## Round-3 production repairs

### 1. Critical — `connect_via` no longer installs false adjacency

Route installation in `install_peer_cas` moved INSIDE the
`AddrInstallMode` match:

```text
DirectOverwrite  → add_direct_route(peer, addr)   identity-qualified
                   by construction
RoutedPreserve   → add_route(peer, addr)          LEGACY — a routed
                   end-to-end session authenticates the far endpoint,
                   not the adjacent relay whose address it records
```

The pre-fix unconditional `add_direct_route` bound
`(destination = endpoint, next_hop = relay addr,
next_hop_id = endpoint)` — protected forwarding selected the
endpoint's end-to-end session and aimed the envelope at the relay.

Witness (in-crate): `a_routed_install_is_not_an_authenticated_adjacency`
— after a RoutedPreserve install, `lookup` resolves the relay address
while `lookup_authenticated` and `authenticated_next_hop` resolve
nothing; a DirectOverwrite install still binds.

### 2. Critical — pingwave routes are legacy, always

Pingwaves are unauthenticated UDP datagrams. The reverse index plus a
peer-registry address cross-check is two ADDRESS registries agreeing —
no authenticated session verified the datagram, and a spoofed source
tuple could otherwise install a protected route bound to an innocent
registered peer. The pingwave writer now installs
`add_route_with_metric` only (address-only), with the rationale pinned
in a comment at the install site.

The authenticated learning path for the same edge is the capability
announcement (`from_node` = the AEAD-resolved session peer); its
equal-metric same-address install upgrades the pingwave's legacy entry
in place. This also makes the learned-route witnesses DETERMINISTIC:
only capability learning can satisfy an `authenticated_next_hop` poll,
so a broken capability writer cannot be masked by the pingwave path.

### 3. High — withdrawal and failure invalidation are identity-qualified

```text
withdrawal  RoutingTable::remove_route_if_from_hop(dest, addr, sender)
            removes iff next_hop == addr AND (next_hop_id is None
            OR == Some(sender)) — a sender may withdraw its own bound
            route or a legacy entry at its address, never a route
            bound to another peer at a reused/shared address.
            (`remove_route_if_next_hop_is` remains for local rollback,
            where the caller undoes a write it just made.)

failure     ReroutePolicy::on_failure affects: identity-bound entries
            iff bound to the failed peer (even at a drifted address);
            legacy entries by address match, as before.

alternate   the per-destination alternate resolution excludes by
resolution  identity as well (lookup_alternate_excluding): a failed
            peer's bound route whose address drifted off the excluded
            one must not be offered as its own "alternate". Found by
            the drifted-address case of the reuse-inverse witness
            during this repair.
```

Witnesses: `remove_route_if_from_hop_is_identity_qualified` (route.rs)
and `failure_invalidation_never_affects_another_identity_at_a_reused_address`
(reroute.rs) — both include the address-reuse inverse.

### 4. High — migration requires the expected old address

`migrate_next_hop`'s identity arm now requires BOTH
`next_hop_id == Some(identity)` AND `next_hop == old`. Identity
equality alone let a stale migration roll a route backward: two
accepted re-handshakes serialize their peer-map replacement but can
finish route migration in the opposite order, and the older `A→B`
would overwrite the newer `A→C`.

Witness: `migrate_next_hop_stale_caller_cannot_roll_back` — newer
`A→C` lands, older `A→B` finishes late, the route remains at `C` and
the stale migration returns 0.

### 5. High — freshness belongs to the installed path

`add_authenticated_route_with_metric`, equal/worse arrivals:

```text
same addr + same identity   → refresh
same addr + no identity     → identity upgrade + refresh
same addr + conflicting id  → no rewrite, no refresh
different next hop          → no rewrite, NO refresh
```

Evidence from live peer C can no longer keep a dead route through B
fresh forever while the single-entry table refuses to switch — the
installed route ages out normally. (The legacy writer is unchanged;
legacy entries carry no protected traffic. A future multi-route table
may retain the alternate separately.)

Witness: `another_peer_cannot_refresh_the_installed_route` — the
installed route is backdated, an equal-metric announcement arrives
through another peer, and the route stays stale with its binding
untouched.

## Witnesses, honestly named

### The learned-route FIRST-HOP production witness

`a_production_learned_route_selects_the_authenticated_first_hop`
(previous name overstated it as "two-gateway"). One credentialed
gateway; topology `left ↔ gw ↔ right ↔ dest`; the route to the
non-adjacent destination arises only through production capability
propagation (deterministically — see repair 2); the relay's output is
opened under the gw↔right edge key with `dest_id` unchanged and the
hop budget moved exactly once.

### The LIVE two-gateway witness

`a_protected_hop_traverses_two_credentialed_gateways`. Topology
`left ↔ gw1 ↔ gw2 ↔ dest`; BOTH gateways hold credentials, admitted
contexts for their neighbours, and boundary sets; gw1's route to
`dest` arises only through production propagation via gw2. One
envelope: `left → gw1 → gw2 → dest-side watcher`, with the
inter-gateway leg landing on gw2's REAL socket and both relays running
the full production path (classification, ingress MAC + replay,
authenticated route lookup, transition authorization, TTL mutation,
re-tag). Final assertions: opens under the gw2↔dest edge key,
`dest_id` unchanged, inner bytes unchanged, `ttl == input - 2`,
`hop_count == input + 2`.

Authority inverse:
`a_second_gateway_without_route_authority_stops_the_hop` — same
topology and envelope, gw2 holds only ATTACH; gw1 forwards, gw2
refuses, nothing reaches the destination side.

## Replay — evidence hardening

The accepted `SharedHopReplayWindow` design is unchanged. Added:

```text
RAII claim guard      ReplayClaim releases on drop, so an unwind can
                      never leave the window permanently claimed
deterministic         a_held_claim_makes_admission_refuse_contended —
contention            holds the claim via the private try_claim and
                      asserts admit refuses Contended, state untouched,
                      and the window frees on drop (no thread race to
                      hope for; the prior 8-thread test remains as the
                      concurrent-misuse sweep). This witness earned its
                      keep immediately: the first RAII refactor built
                      the guard eagerly on the LOST-claim path
                      (`then_some`), whose drop released the holder's
                      claim — the thread test stayed green, this one
                      went red, and the lazy `then(..)` fix is now
                      pinned by a comment at the construction site.
bad-tag no-burn       a_bad_tag_does_not_burn_the_replay_sequence — a
                      forged tag at sequence N rejects; the legitimate
                      envelope at N still admits exactly once
fresh incarnation     a_new_session_incarnation_starts_a_fresh_replay_window
                      — a re-handshaked session admits a sequence the
                      old incarnation had seen
```

## Seam robustness inverses

`set_peer_addr_for_test` (in-crate witnesses):

```text
a routed/absent/legacy/conflicting-identity direct route refuses the
move with ZERO mutation (peer record, both indexes, routing table):
  set_peer_addr_for_test_refuses_without_tearing_state
a reused old address re-indexed to another owner keeps that owner's
mapping across a successful move:
  set_peer_addr_for_test_leaves_a_reused_old_address_with_its_owner
```

## CLI — digit-only names are names

The parse rule is now unambiguous without sacrificing valid names:

```text
exactly lowercase 0x + 16 lowercase hex digits   → canonical literal
                       (the exact string `exports` renders)
0x + hex digits at any other width or case       → refused with the
                       width message (a mispasted wire hint must not
                       silently become a name hash)
everything else, including digit-only strings    → channel name
                       ("66" and "65536" are valid names)
```

Uppercase `0X…` is not a literal and fails name validation (channel
names reject uppercase), so the lowercase-only promise holds end to
end. Witnesses:
`parse_channel_hash_accepts_only_the_exact_render_format_as_a_literal`,
`parse_channel_hash_treats_digit_only_strings_as_names`,
`exports_render_width_round_trips_through_the_parser`.

## Gate at the candidate head

Local runs at `35d63fe6d`, exact commands:

```text
cargo test --features net --lib
  → 5485 passed, 0 failed, 1 ignored   (+10 over b94e819cc: the
    stale-migration, freshness, and withdrawal route.rs witnesses;
    the reroute reuse/drift inverse; the deterministic contention
    witness; and five in-crate mesh witnesses — routed-install
    inverse, two consistent-move seam inverses, bad-tag no-burn,
    fresh-incarnation replay)

cargo test --features "net fixtures" --test subnet_gateway_auth
  → 16 passed, 0 failed   (+2: the live two-gateway witness and the
    second-gateway authority inverse; the prior learned test renamed
    to the first-hop witness it is)

cargo test --features "net fixtures" --test route_withdraw
  --test three_node_integration --test capability_multihop
  --test chain_discovery
  → 4 / 66 / 7 / 11 passed, 0 failed   (the suites whose withdrawal,
    connect_via, and learned-route semantics this repair touches)

cargo test -p net-cli
  → 102 binary-unit tests + every integration suite pass
    (net-cli has no library target — these are the bin's unit tests)

cargo fmt --check                                            clean
cargo clippy --lib --features net     -- -D warnings         clean
cargo clippy --lib --features cortex  -- -D warnings         clean
cargo clippy --all-features --lib --bins -- -D warnings      clean
cargo clippy --all-features --all-targets -- -D warnings \
  -A clippy::unwrap_used -A clippy::expect_used \
  -A clippy::undocumented_unsafe_blocks \
  -A clippy::multiple_unsafe_ops_per_block                   clean
RUSTDOCFLAGS="-D warnings" cargo doc --features net --no-deps  clean
```

## CI provenance

The authoritative workspace run for a candidate is the run at the
PACKET head (the branch head CI actually built), not at the
implementation parent. For the superseded candidate that run was
https://github.com/ai-2070/net/actions/runs/30793358324 at
`dfe54dbeb` — 45/45 jobs successful. The authoritative run for THIS
packet's head completes after this commit is pushed; its link and
per-job numbers are recorded in the review thread, and a reviewer
should read that run, not the local gate above.

## Explicitly open (retained)

```text
production relay_protected_hop allocation:
  OPEN — planned E2E harness

  Unchanged: tests/subnet_route_hop_alloc.rs measures the sealing
  PRIMITIVE, not the production branch; the source pins remain interim
  structural guards. Runtime measurement of relay_protected_hop is a
  hard requirement at the E2E stage.
```

S5 and the live E2E suite remain not started.
