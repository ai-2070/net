# CODE REVIEW 2026-08-04 — Subnet auth branch (`subnet-auth-e2e`)

> **STATUS: OPEN, not signed off.** Findings below are unaddressed as
> written. The primary source pass and an independent parallel E2E/evidence
> pass both reviewed code head `94ef4e092`; the branch has since advanced only
> by adding this document. See [Verification](#verification) for the exact
> commands, feature sets, and review boundaries.

**Scope:** the full branch diff `master..94ef4e092` (merge base `313323988`)
— 40 commits, 53 files, +24846/−1561.

| file | what |
|---|---|
| `subnet/auth.rs` | **new** — `SubnetGrant` / `SubnetIssuerGrant` / `SubnetRevocationFloor`, the fail-closed verifier, `VerifiedSubnetContext`, `VerifiedGatewayContextSet` + the D6 transition decision, `SubnetExportBinding` |
| `subnet/control.rs` | **new** — S5 signed control facts (descriptor / gateway ad / export policy / floor) and the revision-monotonic store |
| `subnet/route_hop.rs` | **new** — the per-hop MAC envelope, `HopReplayWindow` / `SharedHopReplayWindow` |
| `subnet/admission.rs` | **new** — challenge store + per-session compiled-context store |
| `subnet/id.rs` | `is_ancestor_or_self_of`, `ancestor_path`, `common_ancestor`, `TopologySubnetId` |
| `subnet/gateway.rs` | export table re-keyed `u16` wire hint → canonical `ChannelHash` |
| `route.rs` | `DestRoutes` provenance split (ordinary / protected), transition tokens, CAS writers |
| `reroute.rs` | failure removes evidence, discovery restores it; `SavedRoute` / graph alternates deleted |
| `failure.rs` | `PeerFailureEvent` — verdicts carry incarnation + monotonic order |
| `crypto.rs` / `session.rs` | directional route-hop keys off the handshake hash; seal/open on `NetSession` |
| `mesh.rs` | `SubnetGatewayAuthorityState` coherent publication, `relay_protected_hop`, control-fact dispatch, `PeerTransport`, `subnet_visible` |
| `mesh_rpc.rs` / `org_admission_gate.rs` | D7 `serve_rpc_subnet_exported` + per-call `verify_subnet_export` |
| `fold/capability_bridge.rs` | S1 — `may_admit` split out of `may_execute`; self-declared axes never admit |

**Overall.** Strong work, and the central moves are the right ones. The
credential family is fail-closed and correctly ordered — verifier-owned
bindings are checked before any signature work, the leaf subject is bound to
the full 32-byte `EntityId` rather than the truncated `NodeId`, and the
domain prefixes make cross-domain presentation fail by construction rather
than by hash-space luck. I tried the obvious cross-shape attacks on
`verify_credential_set` (present a one-hop leaf as `Direct`; wrap a
root-signed broad leaf in a narrow `SubnetIssuerGrant`) and both are refused,
the first because a delegated issuer is not in `config.roots` and the second
because the envelope only ever attenuates.

Two changes are vulnerability closures rather than hardening. Re-keying the
gateway export table on the canonical `ChannelHash` fixes a case where two
unrelated channels sharing a 16-bit bucket shared export policy, with an
attacker free to pick a colliding name — and the CLI's refusal to widen a
short hex literal rather than reinterpret it is the right call. The
`may_execute` / `may_admit` split makes the self-declared subnet/group axes
structurally incapable of admitting, which is the honest fix for an axis
whose admitted values are broadcast by the very announcement that names them.

The `DestRoutes` provenance split is the other load-bearing piece and it
holds: an unauthenticated writer cannot reach the protected candidate's
identity, address, metric, or freshness, and cannot occupy the destination
against it. Doc comments throughout explain *why* — including why earlier
shapes were wrong — rather than restating the code.

The findings below combine the primary source review with a separate audit of
the branch's semantic witnesses. The production authority path held up under
that second pass, but two claimed closure witnesses are structurally invalid,
three other E2E/build-surface claims are incomplete, and the original source
findings remain open.

Per the review-tracking rule, the `§N` labels are for this document only —
they do not belong in code or commit messages.

---

## P1 findings

### §1 — `Visibility::Exported` goes from inert to active, so propagation widens on upgrade with no config change

`net/crates/net/src/adapter/net/mesh.rs:26554` (`subnet_visible`), with the
resolution sites at `:26044` (subscribe gate) and `:26838` (publish fan-out).

Before this branch the `Exported` arm was:

```rust
Visibility::Exported => false,
```

— unconditionally. A channel configured `Exported` reached nobody, and the
export table an operator populated was consulted only by
`SubnetGateway::should_forward`, which has **no production callers** (on
`master` either; it is reachable only from its own unit tests). So the
export table was, in practice, write-only state.

This branch wires it in on both paths. `subnet_visible` now takes
`export_targets: Option<&[SubnetId]>`, resolved once per subscribe and once
per publish from `SubnetGateway::export_targets(canonical_hash)`, and the arm
becomes a containment test over the declared targets.

The new logic is itself fail-closed in every direction that matters — no
declared rule resolves to `None` and denies; an underivable peer subnet
denies with no permissive flat-mesh fallback, unlike the `SubnetLocal` /
`ParentVisible` arms above it. That is all correct.

The problem is the upgrade step, not the logic. Any deployment that both
(a) configured one or more channels as `Exported` and (b) populated export
rules through `net gateway export` will, on upgrade, begin propagating
traffic that previously went nowhere — no config file changes, no operator
action, no log line at the moment it starts. An operator who set `Exported`
and observed that nothing shipped could reasonably have concluded the channel
was closed and left the rules in place.

Two things compound it. The rule is subtree-containment
(`targets.iter().any(|t| t.is_ancestor_of(dest))`), so a target declared at a
fleet reaches every vehicle under it — a rule written when it did nothing has
a blast radius nobody had reason to check. And a target of `SubnetId::GLOBAL`
in an existing rule now matches every destination.

Not a defect in the code as written. It needs an explicit disposition:
release-note the activation, and ideally emit one `info` line at gateway
install naming the channels whose `Exported` rules just became live — the
same instinct as `note_if_visibility_only` in `channel/config.rs`, which this
branch added for precisely this class of "the operator believed something
else" gap.

### §2 — Scenario B's cross-capability inverse mints a fresh valid grant for the capability it invokes

`net/crates/net/tests/subnet_auth_e2e.rs:1900-1939`
(`partner_intent`) and `:1995-2012` (the inverse).

`partner_intent(provider, service, target_scope)` derives `cap` from its
`service` argument and then uses that same capability in all three authority
objects:

- the BMW capability grant (`:1907-1915`);
- the Partner dispatcher grant (`:1923-1928`); and
- `OrgProofIntent.capability` (`:1930-1939`).

The inverse says that a grant naming `diagnostic.snapshot` cannot invoke
`perception.roi`, but it invokes `SERVICE` and also passes `SERVICE` to
`partner_intent`:

```rust
partner_intent(
    provider.clone(),
    SERVICE,
    // The grant names the DIAGNOSTIC capability, but the
    // call invokes perception.roi.
    GrantTargetScope::ExactNode(provider.clone()),
)
```

The comment is false: this creates a fresh valid `perception.roi` capability
grant and exact dispatcher scope. The call can still be denied because
`perception.roi` is registered `OwnerDelegated` (`:583-595`), whereas only
`diagnostic.snapshot` is registered `CrossOrgGranted` (`:1958-1969`). Thus
both capability and admission mode differ. The explicit denial and dark
handler prove that *some* gate denied, not that the diagnostic grant is
capability-bounded.

Closure requires separating the granted capability from the invoked service
while holding provider target, registration mode, topology, and every other
gate constant.

### §3 — the deterministic lost-update witness does not hold the production gateway installation sibling as the stale writer

`net/crates/net/src/adapter/net/mesh.rs:11663-11734` and
`net/crates/net/tests/subnet_auth_e2e.rs:1371-1510`.

The public `install_subnet_gateway_credentials` path independently compiles
the credential set and publishes at `mesh.rs:11692`. The paced fixture
duplicates that installation/compilation branch and publishes at `:11733`.
Phase B holds the fixture-only branch at `subnet_auth_e2e.rs:1463-1473`, not
the public installation method.

The shared `publish_authority_member` retry primitive is coherent, and the
boundary production/fixture paths both reach it. The gap is branch
reachability: a mutant changing only the production gateway publication at
`mesh.rs:11692` to a naive load/modify/store remains deterministic-green.
Phase A uses the production gateway publication only as the unblocked
intervening writer; Phase B holds the duplicated fixture branch. The
supplemental storm at `subnet_auth_e2e.rs:1292-1351` is intentionally
nondeterministic and does not close that mutant.

Closure requires one shared gateway-installation implementation parameterized
by the pacing hook, with both the public method and fixture driver delegating
to it. The two deterministic schedules must then hold that shared production
implementation in both sibling roles.

---

## P2 findings

### §4 — `AncestorPath` violates its `ExactSizeIterator` contract on interior-zero paths, and the test that pins exactness excludes exactly the domain the file argues matters

`net/crates/net/src/adapter/net/subnet/id.rs:325` (`size_hint`), `:334`
(`impl ExactSizeIterator`), `:525` (the test).

```rust
fn size_hint(&self) -> (usize, Option<usize>) {
    let remaining = match self.next {
        None => 0,
        Some(id) => id.depth() as usize + 1,
    };
    (remaining, Some(remaining))
}
```

`depth()` returns *the index of the last non-zero level, plus one*
(`:138`). `parent()` clears *the deepest non-zero level* (`:157`). Those are
different quantities the moment a path has an interior zero, and the walk
follows `parent()` while the hint follows `depth()`:

| raw | `len()` says | walk actually yields |
|---|---|---|
| `0x03_00_07_00` | 4 | `0x03000700`, `0x03000000`, `GLOBAL` — 3 |
| `0x00_00_00_09` | 5 | `0x00000009`, `GLOBAL` — 2 |
| `0x01_02_03_04` | 5 | 5 ✔ |

`ExactSizeIterator::len()` is documented as exact, and third-party code is
entitled to rely on it. `AncestorPath` is public (`pub use` from
`subnet/mod.rs:40`), and its own rustdoc invites the reliance the impl then
breaks: *"a caller sizing a stack buffer from it must not be lied to"*.

**Currently latent.** The only two consumers are the `for` loops in
`authorize_transition_counted` (`subnet/auth.rs:1957`, `:1992`), which use
`next()` and never ask for a length. The walk itself is semantically correct
for interior-zero paths — every containing scope is on the chain, which is
what the meet-property test establishes — so no forwarding decision is
affected. This is a latent public-API defect, not a live one.

The test gap is the reason to fix it now rather than file it.
`ancestor_path_is_deepest_first_and_bounded` (`:525`) asserts
`"size_hint must be exact for {id}"` over
`[0x00000000, 0x03000000, 0x03070000, 0x03070200, 0x01020304]` — every one a
canonical prefix with no interior zero. Immediately below it,
`the_meet_property_holds_over_raw_paths_including_interior_zeros` (`:637`)
argues at length that this is not the domain that counts:

> Every decoder on the wire side (`SubnetGrant`, `SubnetIssuerGrant`,
> `SubnetRevocationFloor`, `SubnetAuthPresentation`) reaches `from_raw`
> without canonical rejection, so this domain is what the security surface
> actually accepts.

That argument is right, and it applies to the exactness claim one test
earlier. The meet property got the exhaustive 81-path treatment; `size_hint`
did not, and it is the one that is wrong.

There is a closed form. `parent()` removes one non-zero level per step and
terminates at `GLOBAL`, so the chain length is the number of non-zero levels
plus one:

```rust
Some(id) => id.raw().to_be_bytes().iter().filter(|b| **b != 0).count() + 1,
```

That gives 3, 2 and 5 for the rows above. Extending the existing loop's raw
list with `0x03_00_07_00` and `0x00_00_00_09` turns the current
implementation red and the corrected one green.

### §5 — a root-signed floor advances the auth epoch for any never-seen `(authority, topology_epoch, path)` triple, including one that revokes nothing

`net/crates/net/src/adapter/net/subnet/auth.rs:843` (`SubnetFloorRegistry::apply`).

The `and_modify` arm is properly monotonic — a replayed or reordered floor
that raises neither `revision` nor `minimum_generation` is `Ok(false)` and
changes nothing. The `or_insert_with` arm is not conditional at all:

```rust
.or_insert_with(|| {
    changed = true;
    FloorEntry { minimum_generation: floor.minimum_generation, revision: floor.revision }
});
if changed { /* auth_epochs[authority] += 1 */ }
```

So a floor whose `minimum_generation` is `0` — revoking nothing, by
definition — still bumps the authority's auth epoch. Because the epoch is
per *authority* while floors are keyed per `(authority, epoch, path)`, that
bump invalidates every compiled context under that authority:
`apply_floor_with` (`mesh.rs`) calls
`SubnetContextStore::invalidate_stale_epoch`, and every published
`VerifiedGatewayContextSet` pinned to the old epoch stops authorizing until
recompiled.

`floor.topology_epoch` is also never compared against the node's current
topology epoch. `apply` validates the authority and the root signature and
then keys on whatever epoch value the floor carries, so a root-signed floor
minted for an arbitrary or stale epoch inserts a fresh key and fires the same
node-wide invalidation. (Replaying the *same* floor twice is correctly inert —
the second one hits `and_modify`.)

Every path here requires an authority root signature, so this is an operator
footgun rather than an attack: a provisioning run that publishes one floor
per scope churns every peer's admitted context once per scope, and every one
of those peers must re-present against a fresh challenge. Two candidate
dispositions, either acceptable:

- gate the epoch bump on the entry being *materially* restrictive
  (`floor.minimum_generation > 0` on insert), so a placeholder floor is
  storable without being disruptive; and/or
- reject a floor whose `topology_epoch` exceeds the node's current one, or
  document that a lagging node deliberately accepts future-epoch floors as
  inert-but-epoch-bumping state.

At minimum the rustdoc on `apply` should say that accepting *any* new
`(scope, epoch)` key costs an authority-wide context invalidation — the
current text ("Returns `Ok(true)` iff registry state changed") does not
convey the blast radius.

### §6 — Scenario C does not establish its claimed two-way authority independence

`net/crates/net/tests/subnet_auth_e2e.rs:2105-2206`.

The test claims both that a valid subnet context cannot replace a channel
token and that a channel token creates neither subnet attachment nor provider
invocation authority. The first direction checks only `.is_err()` at
`:2160-2169`; that does not exclude timeout, disconnect, setup failure, or a
different subscription error. The token-bearing subscription then succeeds.

The reverse direction proves an out-of-scope `ATTACH` attempt returns
`ScopeNotAncestor` and that no gateway context was published. It never invokes
an org-protected provider with the token-bearing peer and never proves a
provider handler remains dark. Absence of subnet/gateway context does not by
itself establish absence of provider invocation authority; that is a separate
admission plane.

Closure requires an exact typed unauthorized subscription result, followed by
a real protected RPC attempt from the channel-token holder with phase-local
dark-handler evidence.

### §7 — Scenario H proves destination silence, not gateway 2's exact denial or gateway 1's forwarding

`net/crates/net/tests/subnet_auth_e2e.rs:3266-3296`.

The inverse removes gateway 2's `ROUTE` right, redirects destination egress
through `set_peer_addr_for_test`, and asserts only that no route-hop datagram
appears at the destination-side socket within 800 ms. It exposes no gateway-1
egress marker, typed `ForwardDenial::RouteMissing`, gateway-2 drop reason, or
same-fixture restoration.

Destination silence is also explained by gateway 1 dropping, packet loss,
timing, fixture/routing failure, or any rejection before gateway 2's authority
decision. The positive path in a separate fixture does not isolate the
mutated axis or prove the comment's claim that gateway 1 forwarded correctly.

Closure requires a phase-local gateway-1-forwarded marker and exact gateway-2
`RouteMissing` observation, or equivalent typed drop telemetry, followed by
restoration and recovery in the same fixture.

### §8 — Cargo metadata permits a vacuous green `subnet_auth_e2e` target without `fixtures`

`net/crates/net/tests/subnet_auth_e2e.rs:67` gates the whole binary on the
`fixtures` feature. `net/crates/net/Cargo.toml` declares the feature but has no
explicit `[[test]]` target for `subnet_auth_e2e` with `required-features`.

The result is a successful zero-test invocation:

```text
cargo test --no-default-features --features "net cortex" \
  --test subnet_auth_e2e -- --list

0 tests, 0 benchmarks
exit 0
```

CI's committed invocation is protected by `--no-tests=fail`, includes
`fixtures`, and listed and ran all 23 tests. That protects the current
workflow, not the Cargo target contract: another runner can omit the
load-bearing feature and receive a vacuous green binary.

Closure is an explicit `[[test]]` declaration whose `required-features`
encode the target's real minimum feature set, including `fixtures`.

---

## P3 findings

### §9 — `SubnetChallengeStore` self-evicts only per peer, and only when that peer is touched again

`net/crates/net/src/adapter/net/subnet/admission.rs:82` (`issue`), `:112`
(`consume`).

Expired entries are pruned by the `slot.retain(...)` inside `issue` and
`consume` — both of which act on **one** peer's vector, reached by that
peer's own next request. Nothing sweeps `by_peer` across peers on a timer,
and the map shrinks only when `consume` empties a peer's vector or
`forget_peer` fires.

The consequence is a ceiling that can be held indefinitely. `issue` refuses a
peer it has never seen once `by_peer.len() >= MAX_CHALLENGE_PEERS` (4096):

```rust
if !self.by_peer.contains_key(&node_id) && self.by_peer.len() >= MAX_CHALLENGE_PEERS {
    return None;
}
```

4096 peers that each request one challenge and never present it wedge subnet
admission for every subsequent peer, even though every one of those 4096
entries is long past `SUBNET_CHALLENGE_TTL` (30s).

Substantially mitigated in practice, which is why this is P3 and not higher.
`issue_subnet_challenge` requires a live session (`mesh.rs:11447` looks up
`self.peers`), so reaching the ceiling means 4096 completed Noise handshakes;
and `forget_peer` is wired at both the failure-suspicion callback
(`mesh.rs:9084`) and permanent eviction (`mesh.rs:20143`), so peers that die
are reclaimed. The residual case is a peer that stays connected and idle
after abandoning an attempt.

Worth noting because the module header sets the bar itself:

> Both are bounded and self-evicting. The `auth_failures` map is the
> cautionary precedent here: it has no cap and no eviction site, so neither
> structure copies its shape.

Bounded, yes. Self-evicting only for peers that come back or die. Either a
TTL sweep on the `MAX_CHALLENGE_PEERS` refusal path (evict expired peers
before refusing) or a correction to that paragraph would close the gap
between the claim and the code.

### §10 — bare `#[allow(clippy::too_many_arguments)]` against the module's `#[expect(..., reason = ...)]` convention

`net/crates/net/src/adapter/net/subnet/control.rs:288`
(`GatewayAdvertisement::try_issue`) and `:475`
(`SubnetExportPolicy::try_issue`).

Every other suppression in the subnet module carries a reason and uses
`expect` so it fails when it stops applying — `auth.rs:302`, `:523`, `:2058`,
`:2208` all use the same lint with

```rust
#[expect(
    clippy::too_many_arguments,
    reason = "explicit wire fields; a params struct would only rename them"
)]
```

These two are the only bare `allow`s in the module, and the reason that
applies to the `auth.rs` sites applies verbatim to both.

### §11 — scratch-store cleanup errors are ignored despite PID-based path reuse

`net/crates/net/tests/subnet_auth_e2e.rs:256-315`.

`ScratchDir::fresh` correctly takes ownership before the first filesystem
operation, but silently ignores failure to remove startup residue:

```rust
let _ = std::fs::remove_dir_all(&path);
```

`Drop` silently ignores the same error during final cleanup. Paths are keyed
by tag plus process ID (`:307-309`), so failed deletion can contaminate a
later run after PID reuse. RAII now covers adoption and panic windows, but it
does not make stale-residue removal failure-closed or observable.

Startup cleanup should return an error and refuse to adopt the path if stale
state cannot be removed. Final cleanup failure should at least be surfaced
with enough path/error detail to diagnose test contamination.

---

## Nits

- **`verify_admission`'s nonce re-comparison is tautological.**
  `auth.rs:2234` constant-time-compares `presentation.verifier_nonce` against
  `expected.verifier_nonce`, but `expected` was built *from* the challenge
  that `SubnetChallengeStore::consume` matched against that same nonce
  (`admission.rs:122`). It can never fail. Harmless defence in depth, but it
  reads as a check that is load-bearing and is not — the actual one-use
  enforcement is `consume`'s unconditional removal.

- **`SubnetChallengeStore::issue` has a TOCTOU on its own ceiling.**
  `admission.rs:83` checks `by_peer.len()` and `admission.rs:93` inserts via
  `entry().or_default()`; concurrent issuers can push the map slightly past
  `MAX_CHALLENGE_PEERS`. Immaterial at this bound.

- **`build_gateway_context_set` merges before it caps.** `auth.rs:2163`
  does an O(n²) `entries.iter_mut().find(...)` over a caller-supplied
  unbounded `Vec`, and only checks
  `MAX_GATEWAY_CONTEXTS_PER_AUTHORITY` afterwards (`:2173`). Publication-only,
  off the packet path, and every caller today passes an operator-sized set —
  but moving the length check above the loop costs nothing.

- **`SubnetExportPolicy`'s public fields allow a count-byte truncation.**
  `control.rs:518` writes `self.exported_channels.len() as u8`. `try_issue`
  and `from_bytes` both enforce `MAX_EXPORTED_CHANNELS`, so this is reachable
  only by struct literal, and the payload length still differs so no
  signature collision is constructible — it just breaks its own round trip.
  Worth a `debug_assert!` or making the field private behind an accessor.

- **The SDK's `fixtures` dev-dependency unifies wider than the comment
  implies.** `net/crates/net/sdk/Cargo.toml` re-declares `net-mesh` with
  `features = ["fixtures"]` under `[dev-dependencies]`. The comment's claim
  is right for downstream consumers — dev-deps do not propagate. Within the
  workspace, though, `cargo test --workspace` builds dev-deps, so `fixtures`
  is enabled on `net-mesh` for every crate in that invocation, not just the
  SDK's own test targets. Test-only and harmless; worth knowing before
  someone debugs why a seam is reachable in a sibling crate's tests.

- **Gateway nodes stop relaying untagged legacy routed packets entirely.**
  `mesh.rs:16765`. Deliberate and documented at the site ("a protected route
  may never downgrade to the public path"), but it means a node that acquires
  gateway credentials silently stops being a general-purpose relay. Belongs
  in the same release note as §1.

---

## Verification

**Primary source pass.** `cargo check --all-targets --all-features` from
`net/crates/net` — clean, exit 0. That pass did not run tests, clippy, docs, or
`cargo fmt --check`.

**Independent E2E/evidence pass at exact code head `94ef4e092`.** The detached
worktree remained clean, and `git diff --check f87a7dffc..94ef4e092` passed.
The following commands were reproduced:

```text
cargo test --features "net cortex fixtures" --test subnet_auth_e2e -- --nocapture
  23 passed, 0 failed

cargo test --features "cortex tool fixtures" --test subnet_auth_e2e -- --nocapture
  23 passed, 0 failed

cargo test --features "cortex tool fixtures" --lib adapter::net::route::tests -- --nocapture
  36 passed, 0 failed

cargo test --features "cortex tool fixtures" --lib adapter::net::reroute::tests -- --nocapture
  15 passed, 0 failed

cargo test --features "net fixtures" --test subnet_gateway_local_auth -- --nocapture
  20 passed, 0 failed

cargo check --features "net cortex" --lib
  exit 0

cargo test --no-default-features --features "net cortex" --test subnet_auth_e2e -- --list
  0 tests, 0 benchmarks; exit 0
```

The exact pinned head had 62 GitHub check runs with no incomplete or failing
conclusions; the CortEX integration and integration pin guard both completed
successfully. The branch later advanced to `2e6392edb` only by adding this
review document, so the code findings and results still apply.

**What was read in full.** `subnet/auth.rs` (2327), `subnet/control.rs`
(1300), `subnet/route_hop.rs` (1005), `subnet/admission.rs` (236), and the
complete diffs of `route.rs`, `reroute.rs`, `failure.rs`, `crypto.rs`,
`session.rs`, `subnet/gateway.rs`, `subnet/id.rs`, `mesh_rpc.rs`,
`org_admission_gate.rs`, `fold/capability_bridge.rs`, `behavior/capability.rs`,
`behavior/subnet.rs`, `channel/config.rs`, `router.rs`, `transport.rs`,
`meshdb/transport.rs`, `redex/replication_runtime.rs`, the CLI/deck/SDK
changes and `ci.yml`.

**What was read selectively.** `mesh.rs` (+3804): the gateway-authority
publication block, `relay_protected_hop`, the control-fact dispatch
insertion, `subnet_visible` and both its resolution sites, `PeerTransport` /
`PeerInfo`, `PeerRegistrationGuard`, the failure/eviction cleanup paths, and
`DispatchCtx` / `MeshNodeConfig` additions. The remaining hunks are almost
entirely the mechanical `peer.addr` → `peer.addr()` rewrite and were skimmed,
not read.

**Parallel evidence coverage.** The parallel pass audited
`subnet_auth_e2e.rs` in full against its production entrypoints, including the
credential/proof objects, topology roles, exact denial claims, handler
darkness, gateway-local authority, protected routing, coherent publication,
feature gates, and CI target selection. It read the packet documents as
claims rather than proof. The other integration-test files listed above were
used selectively for focused production-path regression evidence; they did
not receive the same line-by-line semantic audit as `subnet_auth_e2e.rs`.

**Checked by hand and found sound.**

- The `MAX_TRANSITION_LOOKUPS` bound. Worst case is a crossing transition:
  ≤4 boundary probes plus ≤4 `EXPORT` probes per endpoint, both endpoints,
  and the internal-`ROUTE` loop is unreachable once `crossed_any` — 16, which
  is `4 * MAX_DEPTH` exactly. The non-crossing worst case is 13.
- The empty-index shortcut in `authorize_transition_counted` (`auth.rs:1928`)
  cannot fail open. With an empty index `crossed_any` implies an immediate
  `rights_at` miss → `ExportMissing`, and the internal branch finds no
  `ROUTE` → `RouteMissing`. Both deny.
- `PROTECTED_FORWARD_BUF_SIZE`'s const-assert, and the claim behind it: a
  re-sealed hop is exactly as long as the one that arrived, and the inbound
  datagram is bounded by `MAX_PACKET_SIZE`, so the buffer is an exact ceiling
  rather than an estimate.
- `HopReplayWindow::admit`'s `advance == W` arm and both window edges;
  `SharedHopReplayWindow` leaves state untouched on every error path, and
  `try_claim`'s `.then(..)` (not `.then_some(..)`) is load-bearing exactly as
  its comment says.
- Route-hop key derivation is directional and disjoint from the packet AEAD
  labels, and both sides agree.
- Credential cross-shape attacks (one-hop leaf presented as `Direct`;
  root-signed leaf wrapped in a narrow issuer grant) — both refused, neither
  escalates.
- D7 cannot be dodged by admission mode: both
  `MeshNode::serve_rpc_subnet_exported` and
  `RegisteredRpcService::subnet_exported` reject `PublicAuthenticated`, so a
  subnet-exported registration always dispatches through
  `admit_and_dispatch_protected` (`mesh_rpc.rs:3746`), which is where
  `verify_subnet_export` runs.
- The transition-token design in `route.rs`: table-wide and never reused,
  fail-closed on exhaustion, no stamp on a pure freshness refresh, and the
  absent-destination case declines a CAS because `observed.token` can never be
  the fresh record's `0`.
- `PeerRegistrationGuard`'s rollback is all-or-nothing, keyed on the exact
  session id and the exact install token rather than on the shared address.
- `SubnetExportFacts` pins the aggregate `Arc` for the admission's lifetime,
  so the pointer identity in `is_current` cannot be reused by a replacement
  mid-check.
- The deprecated `RoutingTable::remove_route` has zero remaining callers
  crate-wide; the three surviving `remove_route` call sites all target
  `NetRouter` / `NetProxy`.
