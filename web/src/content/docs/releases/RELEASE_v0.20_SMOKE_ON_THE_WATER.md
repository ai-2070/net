# Net v0.20 — "Smoke On The Water"

*Named after Deep Purple's 1972 track — the one written from a hotel window across Lake Geneva on the night the Montreux Casino burned down. December 4th, 1971: Deep Purple had booked the casino's empty gambling theater to record what would become Machine Head, but the day before the session started they sat in on a Frank Zappa & the Mothers of Invention show in the same hall. Somebody in the audience fired a flare gun into the rattan-covered ceiling, the whole building went up, and the band watched the smoke drift out over the lake while the casino — and most of the gear they'd been planning to record on — collapsed into Funky Claude's frantic evacuation. The riff every guitar shop in the world has heard ten million times is what came out of that view. v0.19 pushed the substrate past its prior throughput ceilings: bidirectional streaming on nRPC, hierarchical manifests + erasure coding + durable staging on Dataforts. v0.20 turns the lens onto what every peer is allowed to actually do with that throughput. The mesh-wide capability surface gains a signed allow-list model — every announcer decides who can invoke its capabilities, the gate fires on both sides of the call, and the operator drives it from one CLI verb. Underneath, a deep-read audit of the cryptographic token surface closes a clutch of cross-channel collision, revocation, dedup-race, canonicalization, and clock-skew hazards that would otherwise have been load-bearing for the new gate.*

## When the smoke clears

For three releases the mesh-wide capability surface was discovery-only. A node `announce_capabilities()`'d, peers indexed the announcement, `find_service_nodes(...)` resolved targets, and that was it. Anyone who could reach a peer over the wire could `call_service` it; the only "auth" was channel-auth tokens scoped to channels (pub/sub), not capabilities. Anyone who could reach an nRPC endpoint could invoke any service the endpoint published.

v0.20 closes that gap end-to-end. **Every `CapabilityAnnouncement` is now a signed policy unit** — the announcer carries three explicit allow-lists (`allowed_nodes`, `allowed_subnets`, `allowed_groups`) directly on the wire, and the substrate honors them at every invoke. Empty allow-lists are the permissive default (any caller admitted, byte-identical to the pre-v0.20 wire form); non-empty lists union into a "node OR subnet OR group" admit rule. Revocation is just a new announcement at a higher version — no separate verb, no separate channel, no operator-key subsystem distinct from the entity key that already signs the announcement. The model is deliberately not an ACL engine: it's the smallest correct gate around announce and execute.

Underneath the new gate, the v0.20 hardening pass closes eight long-standing hazards on the cryptographic token surface and the multi-hop capability dispatch path — items that the v0.19 channel-hash widening had narrowed but not eliminated. Token revocation, forwarded-announcement dedup races, exact-match reserved-key gating on inbound peers, channel-name canonicalization, defense-in-depth subject cross-checks, clock-skew tolerance on delegation validity, and a wildcard-slot DoS shape. None of them were exploitable end-to-end on the v0.19 stack without operator misconfiguration, but several were one refactor away from becoming load-bearing — v0.20 closes them before the new authorization gate gets stacked on top.

---

## Capability execution authorization

The mesh now answers a question it couldn't answer before: *given that B can reach A's nRPC endpoint, is B authorized to invoke A's capabilities?* Pre-v0.20 the answer was always yes; v0.20 lets the answer be no, decided by A's own signed policy, enforced on both sides of the wire.

### The shape

Two new identity types land under `net::adapter::net::behavior::{subnet, group}`:

- **`SubnetId([u8; 16])`** — 16-byte opaque identifier for a topology partition. Operators pick the value (random 16 bytes, a `blake2s`-of-name truncated to 16, or any operator-stable convention); the substrate doesn't interpret the bytes. A peer self-declares subnet membership via a `subnet:<hex32>` tag on its own announcement; the capability index parses the tag at fold time and stores `NodeId → SubnetId` on the peer view.
- **`GroupId([u8; 32])`** — 32-byte opaque identifier for an operator-defined named collection of peers. Same self-declared pattern via `group:<hex64>` tags; a peer can emit multiple group tags to claim membership in multiple groups. The wider value-as-secret space lets operators use a random `GroupId` that's effectively unguessable, matching the substrate's existing channel-auth-token idiom.

Self-declaration is safe because the announcement is signed and TOFU-bound to the entity's ed25519 key — a peer can only claim membership for itself. Operators who want stricter membership use a random ID that's hard to guess; operators who want advisory routing use a public `blake2s`-of-name.

Three additive fields land on `CapabilityAnnouncement`:

```rust
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub allowed_nodes: Vec<u64>,
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub allowed_subnets: Vec<SubnetId>,
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub allowed_groups: Vec<GroupId>,
```

All three default empty + skip-when-empty, so the signed byte form of an unrestricted announcement is byte-identical to the v0.19 wire shape — pre-v0.20 peers round-trip cleanly, and a v0.19 signature verifies on a v0.20 reader. Length caps at 64 entries per axis enforced both on the announce side (the CLI rejects oversized lists at build time) and the wire side (`CapabilityAnnouncement::from_bytes` rejects oversized payloads at deserialize, so a malicious or buggy peer can't ship a million-entry list and force linear scans on every `may_execute` call).

### The gate

`CapabilityIndex::may_execute(target_node, capability_tag, caller_node) -> bool` is the canonical entry point. Permissive by default — an announcement with all three allow-lists empty admits any caller. Once any list is non-empty, the union of all three is enforced (node OR subnet OR group); the scan short-circuits on the first match. Returns `false` when the target has no indexed announcement, when the target's announcement doesn't list the requested capability tag, or when the target restricts and the caller matches no axis.

Two call sites consult it:

- **Caller-side**, inside `Mesh::call_service`. The candidate set returned by `find_service_nodes` is filtered through `may_execute` *before* the routing policy picks a target — so the policy never selects a peer the caller can't actually call, and "no peer advertises X" stays distinguishable from "every peer that does advertise X refused me." When the filter empties the set, the call returns `RpcError::CapabilityDenied { target, capability }` referencing one of the originally-advertised peers as a representative.

- **Callee-side**, inside `serve_rpc`'s bridge — defense in depth for the well-behaved client path. A caller that bypasses the caller-side gate (direct `call()` instead of `call_service`, an out-of-date local index, or a buggy client) gets rejected at the receiver. The bridge emits an `RpcStatus::CapabilityDenied` (`0x0008`) response that the caller's `MeshNode::call` surfaces as the typed `RpcError::CapabilityDenied` — same variant regardless of which side of the gate fired.

`serve_rpc` lazily emits a default-permissive self-announcement at registration time that merges every currently-registered `nrpc:<service>` tag, so the callee-side gate always observes a real policy from the very first inbound event — no cold-start window, no order dependency between `serve_rpc` and `announce_capabilities`. The same call also schedules a peer-side broadcast so other nodes learn about the new service without the operator having to re-announce manually.

### Membership parse determinism

Subnet membership is single-valued by design. An announcement carrying multiple distinct `subnet:<hex>` tags is out-of-model malformed input — the v0.20 parser collapses it to `None` (no membership) rather than picking one tag based on `HashSet<Tag>` iteration order, which is unspecified and would otherwise produce hash-order-dependent gate verdicts that diverge across receivers folding the same signed bytes. Single subnet tag works as expected; duplicate tags pointing at the same `SubnetId` also work (the underlying set dedups them). Groups sort by byte value so the iteration sequence is stable across receivers.

### The CLI

One new operator verb on the existing CLI:

```
net-mesh cap announce \
    --tag nrpc:my-service \
    --tag dataforts.blob.overflow \
    --allow-node 42 \
    --allow-node 0xDEADBEEF \
    --allow-subnet 112233445566778899aabbccddeeff00 \
    --allow-group deadbeefcafef00d... \
    --key /etc/net-mesh/operator.toml \
    --version 7 \
    --ttl-secs 300
```

Builds a signed `CapabilityAnnouncement` with the supplied allow-lists and emits the JSON bytes to stdout (or `--out <PATH>`). The operator ships those bytes through any pub/sub path that calls `CapabilityIndex::index` on receipt. There's no separate `revoke` verb — revocation is a new announcement with a tighter allow-list (or `[only_me]` to deny everyone else), so the audit trail stays uniform. The `--node-id` override is supported only as an explicit confirmation that the supplied id matches the signing key's derived value; a mismatch is rejected at the CLI rather than producing bytes a receiver would refuse.

### Wire status + caller-facing error

`RpcStatus::CapabilityDenied = 0x0008` slots into the reserved canonical-status band; the reserved range pushes to `0x0009..=0x7FFF`. `RpcError::CapabilityDenied { target: u64, capability: String }` is the typed caller-side variant. `default_retryable(RpcError::CapabilityDenied)` returns `false` — a deny verdict won't change on retry, so the retry budget isn't burned on a deterministic deny.

### Conformance

`tests/capability_auth_conformance.rs` pins the six-scenario contract end-to-end against real `MeshNode` instances: permissive baseline admits any caller; allow-by-node admits the listed peer and denies others; allow-by-subnet admits subnet members; allow-by-group admits group claimants; revocation via a new announcement supersedes the old policy; callee-side defense in depth rejects when the caller bypasses the local gate. Plus standalone regression tests for the multi-subnet collapse, the wire-side allow-list cap, the caller-side candidate filter, and the call-path filtering when only a subset of advertising peers authorize a given caller.

---

## Token + identity hardening

The v0.19 release widened `ChannelHash` from u32 to u64 (raising the targeted-collision cost from ~2^32 to ~2^64) and shipped the new 169-byte `PermissionToken` wire format. v0.20 closes the remaining items from the cryptographic-token surface audit — none of which were exploitable end-to-end on the v0.19 stack without operator misconfiguration, but several of which would have become load-bearing the moment the new capability-auth gate stacked on top.

### Token revocation

Pre-v0.20 the substrate had no way to invalidate a token short of natural expiry. A parent that delegated a 1-year token to a child carried the child's signature past any "revoke" intent — even after rotating the parent's key, every cache holding the old `EntityId` continued to honour the child. v0.20 adds a per-issuer **generation epoch** on `PermissionToken` that the cache cross-checks on every `check()`. Bumping an issuer's generation drops every descendant in `O(chain_depth)` at lookup time without per-token state. The new field rides inside the signed payload so it can't be tampered post-issue. Operators rotate via `EntityKeypair::bump_generation()` followed by a re-issue of the still-current tokens at the new generation; the rotation step is one operator call, and the propagation is the same gossip path the existing identity broadcast already uses.

`PermissionToken::delegate` also now caps the child's `not_after` at `min(parent.not_after, now + DELEGATION_MAX_TTL)` — the operator-recovery window for a compromised delegate is bounded by the constant rather than the parent's full remaining lifetime.

### Forwarded-announcement dedup race

The pre-v0.20 capability-announcement handler keyed the dedup table on `(node_id, version)` only, and the dedup insert ran BEFORE the TOFU bind that records `from_node → entity_id` for channel-auth lookups. A forwarded copy of a victim's signed announcement could land first, prime the dedup slot, and silently drop the victim's subsequent direct announcement on arrival — the victim's binding was never written, and any `require_token` channel keyed on that binding failed closed until the victim's next version increment.

v0.20 widens the dedup key to `(node_id, version, hop_count == 0)` so a direct announcement is never short-circuited by a prior forwarded copy. The TOFU bind runs unconditionally on every direct arrival. The forwarded-poisoning-then-direct-arrives race is now covered by a regression test alongside the existing `forwarded_announcement_does_not_tofu_pin_forwarder_to_victim_entity` invariant.

### Reserved-key gate on inbound metadata

`CapabilitySet::with_metadata` enforced the *prefix* reserved-key list but not the *exact-match* reserved-key list (`intent`, `colocate-with`, `priority`, `owner`). These four keys are intentionally writable by user code on the local node, but the same field is populated by deserializing inbound peer announcements — and that path ran no gate at all. A peer could stamp `intent = "high-priority-tenant-X"` on its own announcement and steer the receiving node's greedy-admission to itself for tenant X's workloads.

`CapabilityAnnouncement::strip_reserved_metadata` is the new boundary: receivers strip the exact-match reserved keys (and any reserved-prefix matches) from inbound peer announcements before metadata is consulted by greedy admission, placement scoring, or anything else that lets a metadata value steer substrate decisions. The local node's own announcements still carry the keys — the strip runs only on the receive path. Greedy admission's `chain_caps.metadata.get("intent")` lookup now reads the local node's view, never an attacker-stamped peer value.

### Channel-name canonicalization

`ChannelName::new` rejected explicit path-traversal (`/./` and `/../`) but admitted trailing dots, repeated dots within a segment (`foo..bar`), and case-folded duplicates — `foo.bar` and `FOO.BAR` hashed to different `ChannelHash` outputs and registered as parallel namespaces. Combined with a registry miss falling into the permissive "no ACL" branch of `authorize_subscribe`, this opened a quiet bypass path for operators who registered one casing of a name but not the other.

v0.20 canonicalizes channel names on construction. Names lowercase to a single form before hashing; trailing dots, leading dots, and empty/dot-only segments are rejected with `ChannelNameError::Malformed`. Existing registered names keep working — the lowercase normalization is idempotent on already-lowercase names and the rejection rules only fire on names that previously hashed to a separate namespace from a sibling. The `authorize_subscribe` fall-through is also tightened: a registry miss with no matching prefix entry now returns `Unauthorized` rather than the prior permissive default.

### Defense-in-depth subject cross-check on token cache

`TokenCache::check` walked the slot keyed on `(subject_bytes, channel_hash)` and authorized any token in the slot whose `authorizes(action, channel_hash)` returned true. Today the inserts always key by `token.subject.as_bytes()` so the invariant holds, but the predicate didn't *re-confirm* that the stored token's `subject` field matched the lookup key — a future refactor that indexed by `hash-of-subject` (for memory savings) or added a `replace_unchecked` constructor would silently authorize the wrong entity.

v0.20 adds the cross-check directly to the predicate: a stored token authorizes a lookup only if `token.subject.as_bytes() == lookup_subject.as_bytes()` in addition to the existing scope / channel / validity checks. Cost is one memcmp per check; benefit is a durable invariant the cache enforces on every lookup regardless of how future indexers are built.

### Clock-skew tolerance on token validity

`PermissionToken::is_valid` did raw `now < not_before` / `now >= not_after` comparisons with no skew window. A node whose system clock drifted forward refused to delegate a freshly-issued parent token (it appeared expired locally); a node whose clock drifted backward refused not-yet-valid tokens. Worse, a node with a clock that ran 30 seconds slow accepted tokens that the rest of the mesh treated as expired — the channel-auth fast path used the same `is_valid` call.

v0.20 adds `MeshNodeConfig::clock_skew_tolerance_secs` (default 60 seconds) that applies symmetrically to both bounds: `now + skew < not_before` for the not-yet-valid check, `now - skew >= not_after` for the expired check. The constant is operator-tunable; the documentation calls out the source-of-truth assumption (the mesh trusts its own clock source within the configured window). Token issuers compensate by pulling `not_after` back by the skew window on mint, so a clock that ran skew-forward sees the same expiry boundary as a clock that ran on-time.

### Wildcard-slot fast path

Every `WILDCARD` token landed in the slot `(subject_bytes, 0)`, and the fallback path on `check()` walked the wildcard slot every time the exact slot missed. An attacker with a valid signing key and `DELEGATE` scope could mint up to `MAX_TOKENS_PER_SLOT = 32` distinct WILDCARD tokens under the same subject and force every check for that subject to walk all 32 entries. Mostly a latency issue rather than a privilege gain, but a measurable CPU drag on hot lookup paths.

v0.20 caches a `bool` on each slot — `slot.has_wildcard` — that's set on insert and cleared on the last eviction. The check's fallback to the wildcard slot skips entirely when the bool is false. The exact-slot path is unaffected.

---

## Test hygiene

- **Lib suite at 3850+ tests** (was 3700+ at v0.19 release). 150+ net new tests across the capability-auth allow-list wire format (signed byte-identity vs the v0.19 shape, round-trips with each axis populated, tamper detection on each new field), the gate semantics (permissive / allow-by-node / allow-by-subnet / allow-by-group / no-tag / no-announcement / multiple allow-lists overlap / revocation supersedes / cap enforcement / subnet-parse determinism), the call-path integration (caller-side candidate filter + callee-side defense in depth + auto-self-index from `serve_rpc`), the six-scenario conformance file, the CLI `cap announce` regression suite (signed-bytes round-trip, stdout-vs-file equivalence, `--node-id` mismatch rejection, duplicate-tag acceptance, malformed-arg exit codes), and the token-surface hardening regressions (revocation via generation bump, forwarded-then-direct dedup race, reserved-key strip on inbound, channel-name canonicalization, subject cross-check, clock-skew bounds, wildcard fast path).
- **`cargo clippy --features meshos,deck --all-features --all-targets -- -D warnings` clean** across substrate + every binding crate + the deck demo + the deck TUI + the net-mesh CLI.
- **`cargo doc --features meshos,deck --no-deps` clean under `RUSTDOCFLAGS="-D warnings"`** — every public item in the v0.20 surface carries a doc comment; intra-doc links resolve through the public re-exports.
- **CI matrix carries the capability-auth feature gates** alongside the v0.19 nrpc-streaming and dataforts-tree feature builds. Python / Node / Go / C bindings pick up the `CapabilityDenied` status code in their status enums and the typed error variant in their error mapping; cross-binding round-trip tests run on every CI build.

---

## Breaking changes

### `CapabilityAnnouncement` fields

Three new fields on `CapabilityAnnouncement` (`allowed_nodes`, `allowed_subnets`, `allowed_groups`). Wire-compatible with v0.19 when empty — the signed byte form of an unrestricted announcement is byte-identical to the v0.19 shape via `#[serde(default, skip_serializing_if = "Vec::is_empty")]`. Direct struct construction in v0.19 code (uncommon — most callers go through `CapabilityAnnouncement::new`) needs the three new fields explicitly. `CapabilityAnnouncement::from_bytes` rejects (returns `None`) on any announcement whose allow-list axis exceeds 64 entries.

### `RpcStatus::CapabilityDenied` + `RpcError::CapabilityDenied`

New canonical status code `0x0008` and matching typed error variant. The reserved canonical-status range shifts to `0x0009..=0x7FFF`. Application-layer status codes (`0x8000..=0xFFFF`) are unaffected. Callers matching on `RpcError` exhaustively need a new arm for `CapabilityDenied { target, capability }`; `default_retryable` returns `false` for the variant.

### `serve_rpc` auto-self-indexes

`MeshNode::serve_rpc` now synchronously self-indexes a fresh `CapabilityAnnouncement` carrying every currently-registered `nrpc:<service>` tag before installing the dispatcher, and schedules a peer-side broadcast in the background. Callers that previously relied on `serve_rpc` being a no-op against the local capability index will see a self-announcement appear there immediately; callers that registered services *before* calling `announce_capabilities` no longer need to remember to re-announce.

### Channel-name canonicalization

`ChannelName::new` lowercases names on construction and rejects trailing / leading dots, repeated dots within a segment, and empty/dot-only segments. Existing all-lowercase, dot-segmented names are unaffected. Operators with mixed-case registered names need to lowercase the registration; subscribers automatically address the lowercased form via the new normalization. `authorize_subscribe` registry-miss-with-no-matching-prefix now returns `Unauthorized` rather than the prior permissive default — channels with no ACL configured at all are deny-by-default.

### `PermissionToken` generation epoch

`PermissionToken` wire size grows from 169 bytes to 173 bytes — a `generation: u32` field rides inside the signed payload so the cache can drop every descendant when an issuer rotates. Pre-v0.20 tokens (169 bytes) are rejected on decode; reissue tokens at v0.20. Short-TTL tokens roll naturally; long-TTL tokens require an explicit reissue pass.

### `delegate` not_after cap

`PermissionToken::delegate` caps the child's `not_after` at `min(parent.not_after, now + DELEGATION_MAX_TTL)` where `DELEGATION_MAX_TTL` is a substrate constant (default 30 days, operator-tunable on `MeshNodeConfig::delegation_max_ttl`). Existing long-lived delegations from v0.19 continue to verify until natural expiry; new delegations cap at the constant.

### `MeshNodeConfig::clock_skew_tolerance_secs`

New field on `MeshNodeConfig` (default 60 seconds). Symmetric tolerance window applied to both ends of `PermissionToken::is_valid`. Pre-v0.20 callers using struct-literal construction of `MeshNodeConfig` need to add the field; `MeshNodeConfig::new(addr, psk)` is unaffected (uses the default).

### `CapabilityAnnouncement::strip_reserved_metadata`

New method on `CapabilityAnnouncement`. The receive path (`handle_capability_announcement`) now calls it on every inbound peer announcement before metadata is consulted by greedy admission, placement scoring, or any other substrate decision-maker. Custom dispatchers that route around `handle_capability_announcement` should call `ann.strip_reserved_metadata()` after `from_bytes` and before consuming `ann.capabilities.metadata`.

---

## How to upgrade

1. **Operators issuing restrictive policies.** The new `net-mesh cap announce` subcommand builds and signs a `CapabilityAnnouncement` carrying allow-lists for the supplied identity. Pipe stdout into your gossip path or use `--out <PATH>` to write to disk. Use the same `--key <PATH>` your other operator subcommands already accept. Revocation is a new `cap announce` at a bumped `--version` with a tighter allow-list — there's no separate `revoke` verb.

2. **Callers handling `RpcError`.** Add a `RpcError::CapabilityDenied { target, capability }` arm to exhaustive match expressions. The variant is non-retryable; surface it to the application layer rather than looping. Caller-side `call_service` filters the candidate set before target selection, so a `CapabilityDenied` return from `call_service` means *every* peer advertising the service refused this caller — distinct from `NoRoute`, which means no peer advertises the service at all.

3. **Servers using `serve_rpc`.** No code change required. `serve_rpc` self-indexes a permissive baseline announcement immediately on registration and schedules a peer-side broadcast. Operators who previously called `announce_capabilities` after `serve_rpc` can remove the call (it's now redundant); operators who called it before `serve_rpc` no longer need to remember the order.

4. **Servers wanting to restrict access.** Publish a policy announcement via `net-mesh cap announce` (offline build + ship) or by constructing a `CapabilityAnnouncement` with non-empty allow-lists and folding via your existing capability-broadcast path. The version bump rule still applies — restrictive policies use a `version` strictly greater than any prior announcement from the same `node_id`.

5. **Subnet and group membership.** Peers self-declare via `subnet:<hex32>` and `group:<hex64>` tags on their own announcement. Use the CLI's `--tag subnet:<value>` and `--tag group:<value>` to add them, or `CapabilitySet::add_tag(...)` programmatically. Operators picking subnet/group identifiers can use random bytes (value-as-secret) or a `blake2s`-of-name (advisory routing).

6. **Reissue tokens.** v0.19 tokens (169 bytes) fail decode on v0.20 (which expects 173 with the generation epoch). Run your token-mint pipeline against the v0.20 SDK and propagate the new tokens to every client. Short-TTL tokens roll naturally; long-TTL tokens require an explicit reissue pass.

7. **Rotate compromised issuers.** Call `EntityKeypair::bump_generation()` on the rotated issuer, then re-issue any still-current tokens against the new generation. Every cache holding a descendant of the old generation drops the descendant on the next `check()` — no per-token revocation entry, no CRL gossip, just the generation cross-check the cache already runs.

8. **Channel-name casing.** Lowercase any registered channel names that contain uppercase characters. The new canonicalization treats `foo.bar` and `FOO.BAR` as the same channel rather than parallel namespaces. Subscribers calling `subscribe(name)` against a previously-uppercase name continue to work — the lowercase normalization is applied symmetrically on both sides.

9. **Clock-skew tuning.** The default 60-second `clock_skew_tolerance_secs` covers typical NTP-synced nodes. Tighten via `MeshNodeConfig::with_clock_skew_tolerance_secs(secs)` on networks with strict clock discipline; widen for satellite or air-gapped deployments where the clock source drifts more.

10. **Operator dashboards.** New per-node metrics: `capability_denied_caller_side` and `capability_denied_callee_side` (counts of denials by each gate), `subnet_parse_collapsed_multi_tag` (counts announcements with multiple subnet tags that collapsed to no membership), `token_dropped_by_generation` (counts tokens dropped by a generation-epoch bump). All start at zero in steady state and only fire under attack or misconfiguration; alarm on sustained non-zero.

---

Released 2026-05-20.

## License

See [LICENSE](../../LICENSE).
