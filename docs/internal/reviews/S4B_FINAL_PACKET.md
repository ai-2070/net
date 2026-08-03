# S4B final packet — learned multi-hop, replay desynchronization, seam closure

Closes the five S4B holds from the review at `c5adef63f`. Each section
names the production site, the witness, and what a reviewer should
mutate to see the witness go RED. The behaviour map of the previous
packet (`S4B_REVIEW_PACKET.md`) still applies to the surfaces it
covers; this packet maps only what moved since.

## Pinned revisions

```text
review base (previous candidate, HOLD):
  c5adef63f6b9fe06c3eb546748980fc0ec49d225
  false adjacency, deck/CLI canonical width, narrowed seams

provenance retained in ancestry:
  1211c4f25e203d3c9bc24338a4d0aaf48a22832a
  production route install + positive protected-hop witness

candidate head (this packet's subject):
  b94e819cc92415a59d63e443651ea6345847140c
  learned multi-hop identity, lock-free replay, seam closure,
  deck/CLI canonical completion
```

## 1. Critical — authenticated learned multi-hop (closed)

Every production learned-route writer now installs the required shape:

```text
RouteEntry {
    destination: remote_destination,
    next_hop:    adjacent_peer_addr,
    next_hop_id: Some(adjacent_peer_node_id),
    metric,
}
```

The bound identity is always the exact adjacent peer the local session
layer already authenticated — never the advertised origin, the final
destination, a bare reverse lookup of a mutable address, or a
packet-carried assertion. Where a writer cannot *confirm* the adjacent
identity, it installs a legacy (identity-less) entry: reachable for
ordinary routing, unresolvable for protected forwarding — fail closed,
not fail open.

Substrate (`route.rs`):

| Surface | Contract |
|---|---|
| `RoutingTable::add_authenticated_route_with_metric` | `add_route_with_metric` precedence (strictly-better replaces; equal/worse refreshes) plus: equal-metric same-address arrival **upgrades** an identity-less entry in place, and a conflicting identity at the same address can neither rewrite nor refresh an installed binding (left to age out). |
| `RoutingTable::migrate_next_hop` (now identity-qualified) | Identity-first matching: entries bound to the re-handshaking peer follow the identity; entries bound to a *different* identity are never retargeted by an address match; legacy entries migrate by address as before. |

Production writers:

| Writer | Site | Identity source | Confirmation |
|---|---|---|---|
| Pingwave acceptance | `mesh.rs` pingwave arm (`install_learned_route`) | reverse index nominates `from_node_id` | peer registry's **current** address for that identity must equal the UDP source (same forward confirmation the relay applies at egress); else legacy install |
| Capability-announcement learning | `mesh.rs` `handle_capability_announcement` (hop_count > 0 arm) | `from_node` — the AEAD-resolved session peer | sender address must map back to `from_node` in the reverse index (a relayed peer records its relay's address; that pair would be a false adjacency); else legacy install |
| Graph-alternate promotion | `try_promote_graph_alternate` | `first_hop` from the excluded-first-hop path | `promotable_direct_hop` (live + direct + reverse-index agreement) already proved it |
| Direct promotion on withdrawal | `handle_route_withdrawal` direct arm | destination == adjacent peer | `promotable_direct_hop`; installs via `add_direct_route` |
| Withdrawal/recovery rerouting | `ReroutePolicy::{on_failure,on_recovery}` → `install_route` | alternate / recovered peer | `direct_identity_for`: reverse index and forward map must agree; wired in production via `with_addr_to_node` |
| Refresh | equal-metric arm of both metric writers | n/a | refreshes freshness only; never rewrites an installed binding |
| NAT migration | `install_peer` DirectOverwrite arm + `accept` | `peer_node_id` of the re-handshaking session | identity-qualified `migrate_next_hop` |

Witnesses:

```text
tests/subnet_gateway_auth.rs
  a_learned_route_forwards_protected_hops_to_the_authenticated_adjacent_hop
    the focused two-gateway witness: left ↔ gw ↔ right ↔ dest.
    gw's only knowledge of dest comes from production propagation
    (pingwave flood / forwarded capability announcement via right);
    the test installs NO route by hand. The convergence poll requires
    authenticated_next_hop(dest) — reachability alone never satisfies
    it — and asserts the bound identity is the ADJACENT peer (right),
    then drives a protected hop and opens the forwarded envelope under
    the gw↔right edge key with dest_id unchanged and ttl/hop_count
    moved exactly once each. Replay of the ingress envelope stays
    covered by the direct witness.

src/adapter/net/route.rs
  add_authenticated_route_with_metric_binds_upgrades_and_refuses
  migrate_next_hop_is_identity_qualified

src/adapter/net/reroute.rs
  reroute_binds_identity_only_for_confirmed_direct_alternates
```

Mutation controls (reported; rerun by applying at the head):

```text
mesh.rs pingwave install → add_route_with_metric (drop identity)
  → RED: two-gateway witness times out at the authenticated_next_hop poll
mesh.rs capability install → bind ann.node_id as identity
  → RED: two-gateway witness ("must bind the ADJACENT authenticated peer")
route.rs migrate_next_hop → match address regardless of identity
  → RED: migrate_next_hop_is_identity_qualified
route.rs equal-metric conflicting identity → allow rewrite
  → RED: add_authenticated_route_with_metric_binds_upgrades_and_refuses
```

## 2. High — replay admission off the ordinary-path mutex (closed)

Direction taken: **bounded nonblocking single-writer state** (the
review's option 2), building on its single-consumer trace.

```text
session.rs
  route_hop_replay: SharedHopReplayWindow      (was parking_lot::Mutex<HopReplayWindow>)

subnet/route_hop.rs
  SharedHopReplayWindow
    - fixed-size atomic fields (claim flag, started, highest,
      seen bitmap split into two AtomicU64 halves)
    - claim is ONE compare_exchange; a losing caller returns
      RouteHopError::Contended immediately — the packet drops
    - no mutex, no wait, no spin, no retry, no allocation;
      `new()` is const, embedded inline in the session
    - the admission algorithm itself is the existing, fully
      pinned HopReplayWindow::admit, run on a local copy under
      the claim — one implementation, not two
```

The single production consumer (one receive loop, synchronous
dispatch) always wins the claim uncontended, so the ordinary path pays
two uncontended atomic ops where it paid a lock round trip.
`Contended` is reachable only by breaking the ownership rule, and
fails closed.

Witnesses:

```text
shared_window_matches_the_plain_window_verdict_for_verdict
  verdict-for-verdict equality with HopReplayWindow across fresh /
  duplicate / reorder / both window edges / the S4A advance==W
  regression / huge jump — including the split-u128 round trip
shared_window_under_contention_never_double_admits
  8 threads over overlapping sequence ranges: only Ok/Replay/Contended
  verdicts, no sequence admitted twice, and post-contention state
  still refuses every admitted sequence
(existing) window_boundary_matrix, a_duplicate_exactly_one_window_behind_is_refused,
  a_huge_advance_neither_panics_nor_remembers — unchanged, still pin
  the underlying algorithm
```

## 3. High — test seams closed out of production NAT builds (closed)

```text
mesh.rs — now #[cfg(any(test, feature = "fixtures"))], nat-traversal removed:
  peer_session_for_test
  seal_route_hop_to_peer
  open_route_hop_from_peer
  set_peer_addr_for_test
mesh.rs — newly gated (was unconditional):
  test_pin_peer_entity        (mutates an authority-relevant TOFU pin)
```

Consumers repaired rather than weakened:

```text
.github/workflows/ci.yml  NAT step: --features "net nat-traversal fixtures"
  (direct_upgrade uses peer_session_for_test; the tests opt in — the
   production feature no longer exposes anything)
sdk/Cargo.toml  dev-dependency re-declares net-mesh with `fixtures`
  (tests_call.rs pins TOFU entities; dev-dep feature unification
   applies to the SDK's own tests only, never to consumers)
```

`set_peer_addr_for_test` robustness (review's two notes):

- `rebind_authenticated_route`'s result is now honored: the move is
  REFUSED before any mutation when the route is absent, legacy, or
  bound to another identity.
- the old reverse-index entry is evicted identity-qualified
  (`remove_if(&old, |_, n| *n == node_id)`), so a concurrently reused
  address cannot lose another owner's mapping.
- the seam now also runs the identity-qualified `migrate_next_hop`,
  exactly as `install_peer`'s DirectOverwrite arm does — a moved peer
  carries its learned routes with it, which is the state production
  actually produces (and what the relay's incarnation check demands).

## 4. Medium — Deck demo/rendering + CLI literal contract (closed)

```text
deck/src/demo/fixtures.rs
  GatewayExportRow.channel_hash: u16 → u64
  values derived via ChannelName::hash on the fixture names
  (canonical_hash) — the same pure function a live node applies
deck/src/tabs/gateways.rs
  module prose: HASH documented as the canonical u64 policy identity
  HASH column: Constraint::Length(8) → Length(18)  (full {:#018x})

cli/src/commands/gateway.rs
  run_exports renders {channel_hash:#018x}
  parse_channel_hash: a literal is accepted ONLY as `0x` + exactly
  16 hex digits (the exact string exports renders); short hex and
  ALL decimal forms are refused with an actionable message; names
  remain the preferred form and hash directly
  tests updated to pin refusal of 0x42 / 0X42 / 66 / 0x1FFFF / 65536,
  width bracketing at 15/17 digits, and render→parse round-trip
  (exports output is copy-pasteable back into `gateway export`)
```

The parser documentation and the implementation now state the same
contract.

## Gate at the candidate head

Local runs at `b94e819cc`, exact commands. As before, these are local
evidence, not the candidate-wide CI result — read exact-head workspace
CI for the authoritative per-job numbers.

```text
cargo test --features net --lib
  → 5475 passed, 0 failed, 1 ignored   (+5 over the previous head:
    the two route.rs, one reroute.rs, two route_hop.rs witnesses)

cargo test --features "net fixtures" --test subnet_gateway_auth
  → 14 passed, 0 failed                (+1: the two-gateway witness)

cargo check -p net-deck --all-targets                        clean
cargo check -p net-deck --all-targets --features demo        clean
cargo test  -p net-deck             → 16 passed, 0 failed
cargo check -p net-cli  --all-targets                        clean
cargo test  -p net-cli              → 101 lib + all suites pass
cargo check -p net-mesh --features "net nat-traversal fixtures"
  --test direct_upgrade                                      clean
(sdk) cargo check --tests --features "net cortex dataforts testing
  compute nat-traversal port-mapping aggregator tool macros" clean

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

Authoritative per-job numbers: read exact-head workspace CI at
`b94e819cc` (link recorded in the review thread once the run
completes).

## Explicitly open (retained)

```text
production relay_protected_hop allocation:
  OPEN — planned E2E harness

  Unchanged from the previous packet: tests/subnet_route_hop_alloc.rs
  measures the sealing PRIMITIVE, not the production branch; the
  source pins remain interim structural guards. Runtime measurement of
  relay_protected_hop is a hard requirement at the E2E stage.
```

S5 and the live E2E suite remain not started.
