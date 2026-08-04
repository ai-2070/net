# S4B final packet — route provenance, install serialization, conditional reroute

Supersedes the packets at `dfe54dbeb` and `3754d5305` (both HOLD).
This packet maps the round-3 and round-4 repairs and states the
evidence at its actual strength. The behaviour map of
`S4B_REVIEW_PACKET.md` still applies to the surfaces it covers.

## Aggregate disposition map

Every revision this slice has passed through, with the exact
disposition each received. Historical HOLDs stand **as written** —
later commits repair aggregate behaviour, they do not retroactively
sign an earlier head.

```text
19edebe615b89fbadca612d23170fa5118d4b632   REJECT / HOLD — S4A
    (exact historical label; NOT an S4B disposition)
186d1667fd2f5b6f80e057d65686abfcde89ded4   S4B HOLD as written
    (Exported activation — export policy aliased on the u16 wire hash)
6fa7ab8dd92481e4e0e8e8636263cf5f28b95b68   canonical export key
    (breaking; repaired 186d1667f's aliasing)
71581680890a08198efd0895ec4450655bc0dacc   accepted S4A repair scope
b0ce4b3280cd95b44ce3a52e81f7670db02f51f7   S4B HOLD
01ebae486845a58e8afc969eec6d5a3c3df395b1   cross-plane composition
    (S4B contract test; carried in the prior packet's provenance)
1211c4f25e203d3c9bc24338a4d0aaf48a22832a   S4B HOLD
    (direct route + one-hop witness accepted)
c5adef63f6b9fe06c3eb546748980fc0ec49d225   S4B HOLD
b94e819cc92415a59d63e443651ea6345847140c   S4B HOLD
    (learned-route/replay repair — stale migration + one-gateway
     evidence)
dfe54dbeb1314f11035ab4d8d10ed8576a5df736   packet HOLD
    (overstated the witness and the closure count)
35d63fe6d46a92b88e24f3ecbb85696fa9125c74   S4B HOLD
    (route-lifetime identity closure + real two-gateway witness;
     withdrawal ownership and route provenance incomplete)
3754d530518a65e0afef942187a5d20a11e3ab6f   packet HOLD
    (implementation not signable; historical pins absent)
0b6cb551d573085cde8bb8a88c715713b7dd5a4a   S4B HOLD
    (provenance CONTAINER accepted in direction; lifecycle
     transitions still single-entry / cross-provenance)
0f0403dadef2c7fd78ada4793b1672fa63e00fa9   packet HOLD
    (implementation not signable; provenance and invariant
     descriptions inaccurate)

candidate head (this packet's subject):
  0b6cb551d573085cde8bb8a88c715713b7dd5a4a
  route provenance, install serialization, conditional reroute
```

## Round-4 repairs (this candidate)

### A. Route PROVENANCE — unauthenticated writes cannot reach authenticated state

`RoutingTable` now holds two candidates per destination
(`DestRoutes { ordinary, protected, generation }`) instead of one
entry both writer classes compete for. Making the pingwave writer emit
`next_hop_id: None` was not enough: through the shared entry it could
still replace an authenticated route by claiming a better metric, keep
one fresh forever, or occupy the destination so a legitimate
capability route could never restore protected reachability.

```text
ordinary   pingwaves, routed end-to-end installs, manual/legacy
           installs, reroute fallbacks — ordinary forwarding only
protected  identity-bound; written only where the next-hop identity
           came from an authenticated adjacent session. The ONLY slot
           lookup_authenticated reads.
```

`lookup` picks the lowest-metric live candidate (ties → protected), so
ordinary best-path selection is unchanged. Three consequences fall
out: a pingwave cannot touch protected identity/address/metric/
freshness; a forged legacy route cannot suppress a later authenticated
one (each has its own slot); and a routed `RoutedPreserve` install is
non-destructive to authenticated state **by construction** rather than
by a special-cased writer.

Witnesses: `unauthenticated_writes_cannot_reach_authenticated_route_state`
(all three of Kyra's mutations), `an_ordinary_install_does_not_erase_the_authenticated_candidate`.

**Behavioural consequence worth naming.** `lookup` breaking metric ties
toward the protected candidate means an ordinary route can no longer
displace an authenticated direct adjacency at equal metric — a
hardening the single-entry table did not have (`add_route` replaced
whatever was there, unconditionally, at metric 1). Three existing
tests depended on the old behaviour to force traffic through a relay
for a peer they ALSO held a direct session with, and were updated to
say what they mean: the two legacy-forwarding gateway tests now use a
genuinely learned destination, and `test_mesh_relay_tamper_detected`
drops the direct route before installing the relay route (it needs the
malicious relay to physically receive the datagram). No production
caller relied on the old precedence — the routed-install path wants
the protected candidate to win, and the withdrawal-promotion path now
installs identity-bound.

### B. Per-peer install serialization

`install_peer_cas` and `accept` now hold a sharded control-path lock
(`INSTALL_LOCK_SHARDS = 64`) across the WHOLE transition — peer record,
route, both address indexes, session index, withdrawal-gate reset,
learned-route migration. The `peers` entry lock ordered only the
record swap, so two installs could serialize peer state A → B → C and
publish sidecars C-then-B, leaving `peers` on C while route/addresses/
session index named B. The packet path never takes this lock, and
because `parking_lot` guards are not `Send` the compiler enforces that
it is never held across an `.await`.

Witness: `concurrent_installs_leave_a_self_consistent_peer_snapshot` —
64 rounds of two racing installers; whichever wins, every sidecar must
name the winner's incarnation.

### C. Conditional, incarnation-aware reroute

```text
RouteObservation { generation, next_hop, next_hop_id }
RoutingTable::observe / install_if_unchanged
```

`ReroutePolicy` observes before selection and writes only if the
destination is unchanged; recovery records the generation its own
alternate produced and restores only while that exact state is
current. A peer-incarnation probe (wired in production from `peers`)
abandons a decision whose subject session has been replaced.
Selection-time filtering was never mutation-time protection.

Witnesses: `a_fresh_route_landing_mid_failure_is_not_clobbered`,
`a_route_installed_after_the_reroute_survives_recovery`,
`a_delayed_failure_does_not_reroute_a_new_incarnation`.

### D. Failed transport excluded from every alternate source

Routing-table, graph-path, graph-fallback, and last-resort selection
all now exclude `candidate addr == failed addr` as well as
`candidate NodeID == failed NodeID`, and the graph path prefers
candidates whose forward/reverse indexes agree (so protected recovery
lands on a hop that can carry an identity).

Witness: `alternate_selection_excludes_the_failed_transport` — a
routed peer recording the failed relay's address is not selected.

### E. Withdrawal ownership completed

```text
protected  next_hop_id == authenticated_sender          (address-INDEPENDENT:
           a rebind race legitimately leaves the binding on the old
           address while the peer record already names the new one)
ordinary   sender_is_direct && next_hop == sender_addr  (forward and
           reverse indexes must agree the sender owns that address, so
           a routed peer at a shared relay tuple cannot remove the
           relay's legacy routes)
```

Witnesses in `remove_route_if_from_hop_is_identity_qualified`: rebind
race removes despite drift; shared relay retains; direct legacy
removes; another identity's binding is never removed.

## Round-3 production repairs (retained)

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
announcement (`from_node` = the AEAD-resolved session peer). Since the
provenance split it does NOT upgrade the pingwave's entry in place —
it lands in its own protected slot, which is what stops a forged
ordinary route from occupying the destination against it. This also
makes the learned-route witnesses DETERMINISTIC:
only capability learning can satisfy an `authenticated_next_hop` poll,
so a broken capability writer cannot be masked by the pingwave path.

### 3. High — withdrawal and failure invalidation are identity-qualified

```text
withdrawal  RoutingTable::remove_route_if_from_hop(dest, addr, sender,
            sender_is_direct)
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
same addr + conflicting id  → no rewrite, no refresh
different next hop          → no rewrite, NO refresh
```

(The "no identity yet → upgrade in place" case from the round-3 text
no longer exists: since the provenance split the protected slot is
separate, so an authenticated arrival lands there directly rather
than upgrading an ordinary entry.)

Evidence from live peer C can no longer keep a dead route through B
fresh forever while the table refuses to switch — the installed route
ages out normally. The ordinary writer's own refresh rule is
unchanged; ordinary candidates carry no protected traffic.

Witness: `another_peer_cannot_refresh_the_installed_route` — the
installed route is backdated, an equal-metric announcement arrives
through another peer, and the route stays stale with its binding
untouched.

### F. Transition tokens track observable CHANGE, not mutation attempts

The first cut of the token stamped a fresh value on every mutation
attempt, on the reasoning that over-stamping is the safe direction (a
conditional writer can only be made to skip). That is true about
safety and wrong about liveness: pingwave freshness refreshes arrive
every heartbeat and touch `updated_at` on an otherwise unchanged
candidate, so every compare-and-set spanning more than a moment
failed. `test_mesh_node_auto_reroute_recovery` caught it — recovery
could never restore, because a refresh had always re-stamped the
destination first.

The token now advances only when the candidate SET observably changes
(address, identity, metric, active flag, presence — in either slot),
which is exactly what a conditional writer reasoned about. A pure
freshness refresh leaves it alone. This does not reintroduce ABA: the
counter is table-wide and monotonic, so a destination that changes
`A → B → A` gets two distinct tokens, not the same one back.

Worth noting the same test was previously GREEN for a bad reason —
round-4 recovery bound the identity of whoever owned the alternate's
address, manufacturing a protected candidate at B for destination C.
That is the unsound inference this round removes, so the test had to
be repointed at a destination whose route genuinely rides through B.

## What this candidate does NOT claim

- The two-gateway witness is a focused S4B relay witness. It is not
  the later full nRPC / provider / live-fleet E2E.
- The provenance split gives each destination one candidate per
  class. It is not a ranked multi-route table; a second alternate of
  the same class still displaces the first by metric.
- `install_locks` serializes the install transition against other
  installs. It does not make the transition atomic with respect to
  concurrent READERS: a reader can still observe the peer record
  before a sidecar it will later see. What it removes is the
  cross-installer interleaving that left those sidecars permanently
  disagreeing.

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

The accepted `SharedHopReplayWindow` design is unchanged. Added in
this candidate, closing the two low evidence items:

```text
unwind release        a_panic_while_claimed_does_not_wedge_the_window —
                      panics inside the admission window via
                      catch_unwind, then proves the window is free and
                      its state intact (the RAII guard ran during the
                      unwind, so a panicking caller cannot leave every
                      later packet dropping as Contended)
production reset      a_production_re_handshake_resets_the_replay_window
                      — drives the real installer rather than two
                      independently constructed session pairs: after
                      install_peer replaces the session, the new
                      incarnation admits a sequence the displaced one
                      had already consumed
```

Retained from the previous candidate:

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

Local runs at `0b6cb551d`, exact commands:

```text
cargo test --features net --lib
  → 5495 passed, 0 failed, 1 ignored   (+10 over 35d63fe6d: the three
    provenance/ordinary-install/conditional-write route.rs witnesses;
    the three reroute race witnesses and the transport-exclusion
    inverse; the install barrier; the panic-unwind claim release; the
    production re-handshake replay reset)

cargo test --features "net fixtures" --test subnet_gateway_auth
  → 16 passed, 0 failed

cargo test --features "net fixtures" --test three_node_integration
  --test route_withdraw --test integration_net --test connect_direct
  --test capability_multihop --test chain_discovery
  → 66 / 4 / 13 / 12 / 7 / 11 passed, 0 failed

cargo test --features "net fixtures" --test sensing_fallback
  --test sensing_routed_origin --test sensing_resolver
  --test proxy_coverage_gaps
  → 10 / 1 / 2 / 1 passed, 0 failed   (the suites that install routes
    through relays, i.e. the ones the provenance split could move)

cargo test -p net-cli
  → 102 binary-unit tests + every integration suite pass
    (net-cli has no library target — these are the bin's unit tests)

cargo check -p net-deck --all-targets [--features demo]  clean
cargo test  -p net-deck             → 16 passed, 0 failed

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
PACKET head — the branch head CI actually built — not at the
implementation parent. Prior authoritative runs:

```text
dfe54dbeb   https://github.com/ai-2070/net/actions/runs/30793358324
            45/45 jobs successful
3754d5305   https://github.com/ai-2070/net/actions/runs/30801274130
            main CI 45/45; coverage + natsim successful;
            63 check runs: 58 success, 4 skipped, 1 neutral, 0 failed
```

The authoritative run for THIS packet's head completes after this
commit is pushed; its link and per-job numbers are recorded in the
review thread, and a reviewer should read that run, not the local
gate above.

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
