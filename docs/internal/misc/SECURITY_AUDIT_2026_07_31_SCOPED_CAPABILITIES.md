# Security Audit — Scoped Capabilities (2026-07-31)

Scope: the tag-based capability scope system — `scope:tenant:*` /
`scope:region:*` / `scope:subnet-local` resolution, `ScopeFilter` /
`matches_scope`, `find_nodes_by_filter_scoped` / `find_best_node_scoped`, and
the four binding surfaces (Node/NAPI, Python, C ABI, TS). Extended to the two
authorization surfaces the scope model leans on in practice: the `may_execute`
allow-list axes (`allowed_nodes` / `allowed_subnets` / `allowed_groups`) and the
`subnet_visible` channel-subscription ACL.

Method: manual read of the resolution → query → enforcement chain, with each
exploit link re-checked against the live code path.

**Audited source:** `fb0f5803ee2136ce730e0c3af18e0d712dd92031` (branch
`security-scoped-capabilities`, which had no diff versus `master` and
`origin/master` at that commit). All line references below are against that SHA,
not against a branch name.

Revised twice following review (Kyra, 2026-07-31). First pass: the remediation
for the two HIGH findings was rewritten, two findings narrowed for precision, and
the query-cost finding elevated. Second pass: the multi-hop discovery repair was
made generation-coherent — re-keying `peer_subnets` to `ann.node_id` is necessary
but not sufficient, because the write precedes the authoritative fold apply and
would otherwise admit stale-replay and reordering divergence. The exploit
analysis and severity of the HIGH findings are unchanged throughout.

The scope feature itself is well-scoped and honestly documented. The findings
below cluster in two places: the *adjacent* enforcement gates that inherit
self-asserted membership data, and the binding layer, which widens semantically
invalid filters instead of rejecting them.

---

## Design baseline (documented, not a defect)

`docs/internal/plans/SCOPED_CAPABILITIES_PLAN.md:31-88` is explicit that scope is
a query-time concern: announcements gossip permissively to all
`MAX_CAPABILITY_HOPS = 16` hops and the *consumer* of the index does the filter.
Wire-level scope, signed envelopes, and forwarder enforcement are deferred to a
"v3", gated on a real anti-widening threat model.

Everything below is judged against that stated intent. "No path-level
enforcement" is **not** treated as a bug. What is flagged is (a) where gates that
*do* enforce read self-asserted data, and (b) where the implementation fails open
in ways the plan does not call for.

---

## 🔴 HIGH — `allowed_groups` publishes the secret that protects it

`behavior/group.rs:12-24` states that group membership is self-declared via
`group:<hex64>` tags, and that unauthorised claims are prevented by choosing a
random, unguessable `GroupId` — "values that double as secrets (random 32 bytes)
prevent unauthorised membership claims."

The transport publishes that value to the entire mesh.

### Locations
- `src/adapter/net/behavior/group.rs:12-24` — self-declaration + value-as-secret
  security model.
- `src/adapter/net/behavior/capability.rs:2136-2141` — `allowed_groups:
  Vec<GroupId>` on `CapabilityAnnouncement`.
- `src/adapter/net/behavior/capability.rs:2222-2225` — serialized into the
  announcement whenever non-empty.
- `src/adapter/net/behavior/capability.rs:2244-2247` — `MAX_CAPABILITY_HOPS = 16`;
  the announcement fans out to every directly-connected peer and forwards
  hop-by-hop.
- `src/adapter/net/behavior/tag.rs:126` — `RESERVED_PREFIXES` does **not**
  include `group:`, so `CapabilitySet::add_tag("group:<hex>")` is accepted by the
  public API in every language.
- `src/adapter/net/behavior/fold/capability_bridge.rs:938-944` —
  `derive_caller_axes` reads the caller's groups straight from
  `entry.payload.tags` via `GroupId::from_tag`.
- `src/adapter/net/behavior/fold/capability_bridge.rs:995-1001` — `may_execute`
  admits on any group match.
- `src/adapter/net/mesh_rpc.rs:748-749` — `bridge_preflight` consumes
  `may_execute` as the callee-side gate for public nRPC services.

### Exploit chain
1. Provider P restricts a capability with `allowed_groups: [G]`, where `G` is a
   random 32-byte id per the documented model.
2. P announces. The announcement JSON carries `allowed_groups: ["<hex of G>"]`
   and is broadcast + forwarded up to 16 hops. Attacker A receives it — as does
   every other node in the mesh.
3. A announces `CapabilitySet::new().add_tag("group:<hex of G>")`. `group:` is
   not reserved, so `add_tag` accepts it; the tag lands in A's fold entry as
   `Tag::Legacy`.
4. A invokes P's gated service. `derive_caller_axes` parses A's own tag back into
   `G`; `may_execute` finds `G` in `allowed_groups` and returns `true`.

The attacker never needs to observe a legitimate member's announcement — the
*provider* publishes the allow-list. Confidentiality is hop-by-hop AEAD only, so
every receiving and relaying node sees it in the clear.

### Impact
Complete bypass of the `allowed_groups` axis of the nRPC capability gate. The
`allowed_subnets` axis has the same shape and worse arithmetic: `SubnetId` is 32
bits (`src/adapter/net/subnet/id.rs:82-86`) and typically holds small level
values like `[3, 7]`, so it is enumerable even without observing an
announcement — and it is published too.

`allowed_nodes` is **not** affected and is currently the only sound axis: node
ids are not claimable, because `ann.node_id` is a blake2s derivation over the
announcing key and is checked against `entity_id` at dispatch
(`src/adapter/net/mesh.rs:22358-22373`).

Confidence: **high**. Every link read against the live path.

### Fix — requires a signed entitlement that does not yet exist

An earlier draft of this audit proposed routing the subnet/group axes through
`verify_announced_owner_cert` / `VerifiedOwner`
(`capability_bridge.rs:355-436`). **That fix was wrong and has been withdrawn.**

`VerifiedOwner { org, generation }` is an *organization-belonging* projection and
is deliberately authority-dark:

- `capability_bridge.rs:320-347` — the certificate proves an entity belongs to an
  organization, nothing more.
- `capability_bridge.rs:346-347` — "Belonging only: the returned projection feeds
  discovery, never `may_execute`."
- `capability_bridge.rs:488-493` — ownership retraction leaves `may_execute`
  explicitly untouched.
- `capability.rs:2150-2152` — `owner_cert` "NEVER participates in `may_execute`
  or any execute-authorization axis (OA-1: authority-dark)."

The projection carries no group, subnet, role, or invocation entitlement.
Feeding subnet/group tags through a verified-owner-shaped ingest path would prove
only *node N belongs to org O* — never *org O authorized N to claim group G*. A
signed announcement from a valid org member is still a **provider-authored tag**,
not an issuer assertion. Routing it into `may_execute` would also breach the
OA-1 authority-dark invariant.

The correct fix requires an entitlement primitive that does not exist today:

- an org-signed (or delegated) membership assertion binding
  `(subject, axis, value, validity/generation)` — i.e. the issuer states that
  this subject may claim this group/subnet, rather than the subject stating it;
- verified at ingest against the issuer's key, with revocation and currentness
  semantics appropriate to an **execution** gate (the existing revocation-floor
  machinery in `org_revocation.rs` is the right shape to borrow, but the
  assertion itself is new);
- consumed by `may_execute` in place of the self-declared `group:` / `subnet:`
  tag scan in `derive_caller_axes`.

`org_grant.rs` / `org_grant_registry.rs` may be the closest existing primitive to
extend; that assessment is out of scope for this audit and needs its own design
pass.

**Until that lands:** `allowed_groups` and `allowed_subnets` should be disabled,
or documented as non-security-bearing advisory filters. `allowed_nodes` is the
only axis that currently carries weight. `group.rs:18-24` must be corrected
regardless — the value-as-secret claim is not true of the deployed transport,
because every provider's allow-list broadcasts the secret.

---

## 🔴 HIGH — Subnet membership is self-asserted, and it gates channel subscription

### Locations
- `src/adapter/net/subnet/assignment.rs:102-124` — `SubnetPolicy::assign` derives
  a peer's subnet by prefix-matching against *that peer's own announced tags*.
- `src/adapter/net/mesh.rs:22452-22456` — `handle_capability_announcement` writes
  the result into `peer_subnets`.
- `src/adapter/net/mesh.rs:23480-23494` — `subnet_visible` consumes it as a
  subscribe ACL, returning `AckReason::Unauthorized` on failure.
- `src/adapter/net/mesh.rs:23678-23693` — the visibility matrix.
- `src/adapter/net/subnet/id.rs:145-152` — `is_ancestor_of` short-circuits to
  `true` when the receiver is global.

### Issue 1 — self-assertion
The gate comment at `mesh.rs:22436-22443` correctly identifies that unsigned
input must not feed `peer_subnets`, and gates the write on `signature_verified &&
hop_count == 0`. But signing does not close this: a signature proves the
announcer holds its own key, not that it is entitled to claim `region:us`. Any
peer that completes a handshake can announce the victim subnet's tag and
subscribe to its `Visibility::SubnetLocal` channels.

This is the same missing primitive as the previous finding, on the same axis.

### Issue 2 — unknown subnet is a universal ancestor
`subnet_visible` defaults an unresolved peer to `SubnetId::GLOBAL`
(`mesh.rs:23484`). Under `Visibility::ParentVisible` the check is
`dest.is_ancestor_of(source)` — and `is_ancestor_of` returns `true`
unconditionally when the receiver is global. So **every peer whose subnet has not
been derived is admitted to every `ParentVisible` channel**.

That configuration is easy to reach. `peer_subnets` is only written when a
`local_subnet_policy` is installed, so a node that calls `with_subnet()` without
`with_subnet_policy()` leaves the map permanently empty — the pattern
`tests/capability_scope.rs::build_node_in_subnet` uses. Note the asymmetry in
that state: `SubnetLocal` denies everyone (fail-closed, channel simply breaks)
while `ParentVisible` admits everyone (fail-open, silently).

Confidence: **high** for both.

### Fix
Issue 2 is small, local, and independent of the missing entitlement model:
unknown subnet must not resolve to a universal ancestor on an authorization path.
Either fail closed on `None`, or make the unknown case explicit in
`subnet_visible`'s signature (`Option<SubnetId>`) so each visibility arm decides
deliberately. **This should ship first** — it is the only finding here
exploitable with no crafted tags at all, only a peer that has not announced.

Issue 1 shares its root cause and its fix with the previous finding, and cannot
be closed by any change to subnet *resolution* — including the discovery repair
recommended below.

---

## 🟠 MEDIUM — `scope:subnet-local` leaks cross-subnet on any multi-hop path

### Locations
- `src/adapter/net/mesh.rs:22452` — `peer_subnets` is written only when
  `signature_verified && ann.hop_count == 0`, keyed by `from_node`.
- `src/adapter/net/mesh.rs:26451-26459` — the scope closure's
  `None => policy_installed` warm-up branch.
- `src/adapter/net/behavior/capability.rs:668-673` — `SubnetLocal`'s documented
  contract: "visible only under `ScopeFilter::SameSubnet`".
- `src/adapter/net/behavior/capability.rs:803` — `(F::SameSubnet, S::SubnetLocal)
  => same_subnet`.
- `tests/capability_scope.rs:527-628` — pins the leaking behaviour as intended.

### Issue
Forwarded announcements (`hop_count > 0`) are indexed but never populate
`peer_subnets`. The stated reason (`mesh.rs:22444-22451`) is sound as far as it
goes: `from_node` is the relay, not the origin, so writing the origin's derived
subnet under `from_node` would let any last hop shift a legitimate peer's subnet
binding. But the remedy chosen was to skip the write entirely, which means that
for any peer learned via a relay rather than a direct session, the subnet is
*permanently* unknown. The scope closure then hits `None => policy_installed` and
admits.

Result: `ScopeFilter::SameSubnet` returns peers in arbitrary subnets, and
`scope:subnet-local` providers — whose entire contract is same-subnet-only
visibility — are returned to cross-subnet callers. In a mesh of any size,
multi-hop is the normal way peers are learned, so this is the default outcome
rather than an edge case.

The behaviour is deliberate and tested:
`same_subnet_with_policy_admits_unresolved_peers_via_warm_up` asserts that D
(subnet `[3]`) returns A (subnet `[4]`) under `SameSubnet`, and the test comment
states this is the only way the `None` branch could have produced the result.

The earlier "Cubic P1" fix closed only the no-policy case. The warm-up window the
comment describes is unbounded for forwarded-only peers, so the branch cannot
distinguish "not yet resolved" from "never resolvable".

Confidence: **high** — confirmed by a passing test that asserts the leak
(re-run during review: 1 passed, 5 filtered).

### Fix — resolve forwarded announcements under `ann.node_id`
The relay-confusion hazard that motivated the `hop_count == 0` gate is an
artifact of keying the write on `from_node`. The origin identity is already
available and already authenticated on the forwarded announcement:

- `capability.rs:2425-2432` — `verify()` checks the Ed25519 signature against
  `ann.entity_id`;
- `mesh.rs:22358-22373` — dispatch binds `ann.node_id` to `entity_id` via the
  blake2s derivation and returns early on mismatch, so a signed announcement
  cannot claim another node's id;
- `ann.capabilities` is inside the signed transcript, so the tags the policy
  reads are bound to the origin's key.

So for **discovery-only** subnet resolution, derive `policy.assign(&ann.capabilities)`
and key it on `ann.node_id` rather than `from_node`. The `hop_count == 0`
condition can then be dropped; `signature_verified` must be retained, because the
node_id↔entity_id binding proves nothing when `entity_id` is itself unverified.
This closes the forwarded-only unknown without pretending the relay is the origin.

**Re-keying alone is not sufficient — the write must be generation-coherent.**
Keying by `ann.node_id` is necessary but does not by itself make `peer_subnets`
agree with the fold, because the current write happens *before* the authoritative
apply and its result is never consulted:

- `mesh.rs:22428-22456` — the `peer_subnets` write.
- `mesh.rs:22550-22555` — authoritative fold ingest, later in the same handler.
- `mesh.rs:7601-7619` — `let _ = capability_fold.apply(fold_ann);` discards the
  `ApplyOutcome`.
- `fold/mod.rs:141-143` — the merge rule: `incoming.generation > existing =>
  Replace`, otherwise `Reject`. `translate_announcement` supplies
  `ann.version.max(1)` as the generation (`capability_bridge.rs:1074`).
- `fold/state.rs:285-292` — `ApplyOutcome::{Inserted, Replaced, Rejected}`.
- `mesh.rs:22315-22330` — the dedup key is `(ann.node_id, ann.version,
  is_direct)`. It stops an exact duplicate; it does **not** establish that the
  incoming version is current.

Two divergence modes follow, both of which would leave discovery evaluating a
current capability entry against stale subnet state:

1. **Stale replay.** v10 is accepted and resolves subnet A. A valid replay of v9
   carries a distinct dedup key, so it is processed; it overwrites
   `peer_subnets[node]` with subnet B; the fold then rejects v9 as stale. The
   sidecar now disagrees with the fold.
2. **Delayed publication.** Even if the write is moved after `apply` and gated on
   `Inserted | Replaced`, concurrent ingest can reorder: A applies gen 10 and
   pauses before the sidecar write, B applies gen 11 and writes sidecar 11, A
   resumes and overwrites with 10. `Fold::apply` serialises internally, but a
   separate `DashMap` write is outside that lock.

The requirement is therefore: **publish the derived subnet only as part of the
same accepted-generation transition as the authoritative capability membership.**
A rejected apply must produce zero `peer_subnets` mutation, and a delayed older
accepted transition must not overwrite a newer value. Two implementations satisfy
this:

- **Derived field on the membership payload**, computed at
  `translate_announcement` time and committed under the fold's mutation lock, so
  it moves with the entry by construction. There is in-tree precedent: `region`
  is already derived from the `scope:region:` tag this way
  (`capability_bridge.rs:1066-1068`). *Constraint to verify first:*
  `policy.assign` is a **receiver-local** computation — different receivers may
  hold different policies — so this is only sound if the payload is not shared
  across nodes. `Fold::snapshot` / `restore` exist (`fold/mod.rs:521`, `:554`)
  and no cross-node use appears in `mesh.rs`, but that must be confirmed before
  committing to this shape.
- **Version-stamped conditional sidecar update** under a shared
  ingest/publication serializer: store `(generation, SubnetId)` and apply the
  write only when the accepted generation is strictly greater than the stored
  one, after a successful apply.

This composes with the query-cost finding below: canonical scope derivation and
receiver-local subnet derivation are the same class of problem, and both belong
as derived state tied to the accepted membership generation rather than written
independently ahead of acceptance.

**Required witnesses** for whichever shape is chosen:

1. *Stale replay* — accept v10/subnet A → receive signed v9/subnet B → fold
   remains v10 and peer subnet remains A.
2. *Delayed publication* — v10 apply pauses before derived publication → v11
   completes → release v10 → both fold and peer subnet reflect v11.
3. *Rejected input* — a rejected fold apply produces zero `peer_subnets`
   mutation.

An earlier draft suggested instead admitting unknowns only for peers with a live
direct session. **That suggestion has been withdrawn** — it does not bound the
permissive state at all. A node can be indexed from a forwarded announcement,
later open a direct session, and simply never send a direct announcement; it
stays `None` in `peer_subnets` while the live-session condition keeps admitting
it indefinitely. A first-seen expiry would be a valid alternative bound, but only
if paired with a finite deadline and fail-closed expiry.

Note this repair fixes **discovery** only. It does not make the subnet
authoritative enough for the channel ACL — the self-assertion HIGH above stands
regardless, because a peer still chooses its own tags.

If the leak is instead accepted as intended, `capability.rs:668-673` should say
"same subnet, best-effort" rather than "visible only to" — the guarantee as
written is not delivered.

---

## 🟠 MEDIUM — Semantically invalid scope-filter objects widen to `Any` at the native converters

### Locations
- `bindings/node/src/capabilities.rs:517-560`
- `bindings/python/src/capabilities.rs:462-505`
- `src/ffi/mesh.rs:3477-3519`

### Issue
This is narrower than "all malformed input". Structurally malformed input is
already rejected: the C ABI returns `NetError::InvalidUtf8` / `InvalidJson`
before the converter runs (`ffi/mesh.rs:3591-3603`), missing or wrongly typed
required fields fail deserialization, and Python extraction/type errors propagate
as `PyResult` errors.

The defect is confined to objects that deserialize cleanly but carry no usable
selector. Three shapes collapse to `ScopeFilter::Any` — the *broadest* filter:

1. an unrecognised `kind` (Python documents this fallthrough as "defensive" at
   `:462-464`);
2. a missing or empty required selector (`{kind: 'tenant', tenant: ''}`);
3. a list that is empty after empty entries are dropped.

The stated rationale is that `Tenants([""])` would match only `Global` candidates
and so is not useful. But `Any` is strictly broader than the outcome being
avoided: it returns every non-`SubnetLocal` peer in the mesh
(`capability.rs:807`) — `Global` candidates *plus* every tenant- and
region-scoped peer.

A caller doing `{kind: 'tenant', tenant: currentTenantId}` where the id is empty
or undefined silently queries the whole mesh and picks a provider from it.

### Scope note — TS vs NAPI
The TS helper `scopeFilterToNapi` (`sdk-ts/src/capabilities.ts:409-424`) is
exhaustive over the declared `ScopeFilter` union and does **not** itself map an
unknown case to `Any`; a TS caller on the typed path is protected by the
compiler. The fail-open lives in the Rust converter behind the NAPI boundary,
which raw-JS and native callers reach directly. The two should not be conflated
when scoping the fix.

### Fix
Return an explicit error for all three shapes. `GlobalOnly` is **not** an
acceptable fallback and should not be presented as fail-closed: a caller who
supplied no usable tenant identity would still receive — and potentially select
from — every global provider. That is a different provider population, not a
safe one. When the caller asked for narrowing and gave nothing to narrow on, the
correct outcomes are an error or an empty result set.

---

## 🟠 MEDIUM — Node binding diverges on filter-kind spelling

### Locations
- `bindings/node/src/capabilities.rs:523-526` — accepts only `"any"`,
  `"globalOnly"`, `"sameSubnet"`.
- `bindings/python/src/capabilities.rs:469-470` — accepts
  `"global_only" | "globalOnly"` and `"same_subnet" | "sameSubnet"`.
- `src/ffi/mesh.rs:3479-3480` — same dual acceptance as Python.

### Issue
Python and the C ABI both accept snake_case and camelCase; the Node converter
accepts camelCase only. A caller passing `{kind: 'global_only'}` — a natural
mistake given the Python docs and the dual acceptance everywhere else — falls
through to `_ => Any` and receives the entire mesh instead of unscoped peers
only.

Combined with the previous finding, none of the three native converters returns
an error for these semantically invalid shapes.

### Fix
Accept both spellings in the Node converter, matching its two siblings. Longer
term the three converters are near-identical and have drifted once already —
worth collapsing to one shared parser, which would also let the error handling
above be fixed in a single place.

---

## 🟠 MEDIUM — Scope derivation runs under the fold locks, with attacker-shaped input

*Elevated from LOW on review.*

### Locations
- `src/adapter/net/behavior/fold/capability_bridge.rs:1302-1318` — the derivation
  runs inside `fold.with_state_and_index(...)`.
- `src/adapter/net/behavior/fold/capability_bridge.rs:1209-1242` —
  `scope_from_membership_tags` re-parses the tag list and allocates a `String`
  per scope tag.
- `src/adapter/net/behavior/capability.rs:830-833` — `Tenants` matching is O(n·m)
  with no dedup on either side.

### Issue
`find_nodes_matching_scoped` deliberately hoists the caller-supplied
`same_subnet_lookup` closure *out* of the lock (the comment at `:1296-1301`
explains the re-entrancy risk), but keeps scope derivation inside it, on the
reasoning that it is "cheap, just parses the borrowed tags". The parse allocates
a `String` per scope tag per candidate, and it does so while both the fold state
and index read locks are held.

The cost is therefore not merely query CPU — it extends lock hold time and
contends against mutation and index work for the duration of every scoped query.

The per-announcement bound is `MAX_PACKET_SIZE = 8192`
(`src/adapter/net/protocol.rs:33`); there is no per-set tag cap analogous to
`MAX_ALLOW_LIST_LEN = 64` (`capability.rs:2169`), which exists precisely to avoid
"scanning unbounded vectors inside `may_execute` on every call" (`:2455`). But
8 KB bounds a *single candidate*: aggregate cost per query is
`candidate count × per-announcement tag payload × requested selector count`, and
is not bounded to 8 KB. The asymmetry is the point — announce once, and every
peer pays on every scoped query for as long as the announcement lives.

### Fix
Move canonical scope derivation to verified ingest and store the resolved
`CapabilityScope` on the membership payload, so the query path reads a
precomputed value instead of re-parsing under the locks. Any cached
representation must be derived transactionally from the authoritative membership
payload at ingest — not supplied independently alongside it, which would
reintroduce a claim the substrate never checked. Capping scope tags per set is
worth doing regardless, consistent with the reasoning already applied to the
allow-lists, but a cap alone does not address the lock-hold amplification.

---

## 🟡 LOW — `Global` is permissive, so tenant scope excludes only cooperative peers

`src/adapter/net/behavior/capability.rs:821-824` — a candidate resolving to
`CapabilityScope::Global` matches every tenant and region query.

This is correct per the plan's design invariant 3 (`SCOPED_CAPABILITIES_PLAN.md`,
"Permissive default") and is required for v1 compatibility. The security
consequence is worth stating plainly: an adversary evades tenant partitioning by
simply *omitting* the tag. `ScopeFilter::Tenant` therefore carries no exclusion
guarantee against a non-cooperating peer.

The plan doc is honest about this. The exported API is not — `ScopeFilter`,
`CapabilityScope`, and the TS `ScopeFilter` type
(`sdk-ts/src/capabilities.ts:391-398`) read like enforcement to an integrator who
has not read the plan.

### Fix
Doc-only. Note on the exported types that scope is a discovery convenience with
no path-level enforcement, and that `Global` peers match every scoped query.

---

## 🟡 LOW — Silent widening in the scope builders, and binding policy divergence

Two real footguns, both of which widen visibility silently:

- `src/adapter/net/behavior/capability.rs:976-982` — `add_tag` routes through
  `Tag::parse_user`, which rejects reserved prefixes
  (`behavior/tag.rs:286-294`). `add_tag("scope:tenant:x")` is silently dropped
  and the node announces as `Global`.
- `capability.rs:1046-1050`, `:1061-1065` — `with_tenant_scope("")` /
  `with_region_scope("")` return `self` unchanged, likewise leaving the node
  `Global`.

**Not** a defect, and removed from an earlier draft: the three
`if let Ok(t) = Tag::parse(...)` arms in the scope helpers (`capability.rs:1052`,
`:1067`, `:1079`) are not meaningful swallowed failures. `Tag::parse` returns
`Err` only on the empty string (`tag.rs:254-259`, and the doc-comment says so
explicitly); the tenant/region helpers reject empty components before building
the tag, and `scope:subnet-local` is a non-empty constant. All three strings
start with a reserved prefix and therefore always return `Ok` at the first arm.
The `with_subnet_local_scope` failure arm is unreachable.

### Binding policy divergence
The three ingest paths are not duplicate workarounds for a single footgun — they
implement two different policies, which is worth documenting deliberately:

- `bindings/node/src/capabilities.rs:397-405` and
  `bindings/python/src/capabilities.rs:358-364` use unrestricted `Tag::parse`,
  allowing SDK callers to emit **any** reserved-prefix tag (`scope:`, `causal:`,
  `heat:`, `fork-of:`, `dataforts:`).
- `src/ffi/mesh.rs:3286-3302` recognises only the three `scope:*` shapes and
  routes everything else through the application-facing `add_tag`, so other
  reserved prefixes are dropped on the C ABI and Go paths.

Whether SDK callers should be able to emit substrate-reserved prefixes is a
policy question; today the answer differs by language, silently.

### Fix
Make the scope builders fallible or emit a `tracing::warn!` on the drop, so a
caller who asked for a scope and got none can tell. Separately, pick one
reserved-prefix policy and apply it across all three bindings.

---

## Recommended order

1. **Fix the `ParentVisible` unknown-subnet default.** Small, local, independent
   of everything else, and the only finding exploitable with no crafted input.
2. **Decide the disposition of `allowed_groups` / `allowed_subnets`.** Either
   disable them or mark them non-security-bearing, pending item 3. `allowed_nodes`
   is the only currently sound axis.
3. **Design the signed entitlement primitive** — an issuer-signed
   `(subject, axis, value, validity)` assertion with execution-grade revocation.
   This is a design pass, not a patch, and it is the prerequisite for both HIGH
   findings. Do not reuse `VerifiedOwner`, which is authority-dark by explicit
   invariant.
4. **Make semantically invalid binding filters return errors**, and align the
   Node kind spellings. Independently shippable.
5. **Resolve forwarded signed subnet discovery under `ann.node_id`**, dropping the
   `hop_count` gate while retaining `signature_verified` — and publish the derived
   value only as part of the accepted-generation transition, with the three
   witnesses listed under that finding. Fixes the discovery leak; does not affect
   item 2 or 3.
6. **Move scope derivation to verified ingest** so the query path stops parsing
   under the fold locks. Same generation-coherence requirement as item 5; the two
   are best done together.
7. **Correct the docs**: `group.rs`'s value-as-secret claim, the `SubnetLocal`
   contract wording, and the scope boundary on the exported types.
