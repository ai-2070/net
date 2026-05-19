# Capability Announcement + Execution Auth Plan

**Status:** Draft for review.
**Scope:** add the smallest correct gate around announce + execute on
the mesh-wide capability surface. Permissive by default, signed
announcements as the only auth vector, allow-lists on `NodeId` /
`SubnetId` / `GroupId`. Revocation is a new announcement.

This is deliberately **not** an ACL engine, not a policy language,
not a separate policy channel, not an operator-key subsystem. The
existing `CapabilityAnnouncement` IS the policy unit — the
announcing entity decides who can use their capabilities, signs it,
broadcasts it. Receivers honor it.

---

## Model

```
SIGNED announcement {
    node_id, entity_id, capabilities, version, ttl, signature,    // existing
    allowed_nodes:    Vec<NodeId>,         // new — empty = anyone
    allowed_subnets:  Vec<SubnetId>,       // new — empty = anyone
    allowed_groups:   Vec<GroupId>,        // new — empty = anyone
}

EXECUTE check, caller C invokes capability tag T on announcer A:
    1. lookup A's latest accepted announcement (already in the index)
    2. if it doesn't list T              → deny
    3. if all three allow-lists empty    → allow   (permissive)
    4. if C ∈ allowed_nodes              → allow
    5. if C's subnet ∈ allowed_subnets   → allow
    6. if C's group(s) ∩ allowed_groups  → allow
    7. else                              → deny

REVOCATION:
    A publishes a new announcement (version+1) with whatever allow-lists
    it now wants. The existing dedup + version-monotonicity logic in
    handle_capability_announcement makes the new announcement
    immediately replace the prior one. Setting allow_nodes to a
    1-element list `[only_me]` is the "deny everyone else" form.
```

That's it. No new channels, no operator keys, no rule precedence,
no fold beyond the existing capability index.

---

## What ships

Three additive fields on `CapabilityAnnouncement`, two new identity
types, one execute-side gate, one CLI helper, conformance test.

### 1. New identity types

- **`SubnetId([u8; 16])`** — `src/adapter/net/behavior/subnet.rs`.
  Membership: peer self-declares via a `subnet:<hex16>` tag on its
  own announcement. Receivers parse the tag at fold time and store
  `NodeId → SubnetId` on the peer view. Self-declaration is fine
  because the announcement is signed + TOFU-bound to the entity
  key; a peer can only lie about itself.
- **`GroupId([u8; 32])`** — `src/adapter/net/behavior/group.rs`.
  Same self-declared pattern: `group:<hex32>` tag. A peer can claim
  multiple group memberships by emitting multiple tags. Operators
  pick the group ids out-of-band (a 32-byte random or a
  blake2s-of-name); the substrate doesn't care, it just compares
  bytes.

Both types: `Debug + Display + Hash + Eq + Copy`, postcard round-
trip, hex parse/format.

### 2. Three fields on `CapabilityAnnouncement`

```rust
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub allowed_nodes: Vec<NodeId>,
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub allowed_subnets: Vec<SubnetId>,
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub allowed_groups: Vec<GroupId>,
```

All three default empty + skip when empty, so the SIGNED byte form
of an unrestricted announcement is byte-identical to today's. Pre-
this-plan peers round-trip cleanly. Length caps: 64 entries per list
(any restriction tighter than that should be a group, not an
inline node list).

The signature already covers these fields once they're inside the
struct — `signed_payload()` reflects the post-add layout. Tests pin
that a v0.x announcement still verifies against a post-plan reader
(byte-identical when lists are empty).

### 3. Execute-side gate

One method on the capability index:

```rust
pub fn may_execute(
    &self,
    target_node: NodeId,
    capability_tag: &str,
    caller: &EntityId,
    caller_node: NodeId,
) -> bool {
    let Some(ann) = self.latest_announcement(target_node) else {
        return false; // no announcement = can't address
    };
    if !ann.capabilities.has_tag(capability_tag) {
        return false;
    }
    if ann.allowed_nodes.is_empty()
        && ann.allowed_subnets.is_empty()
        && ann.allowed_groups.is_empty()
    {
        return true; // permissive default
    }
    if ann.allowed_nodes.contains(&caller_node) {
        return true;
    }
    if let Some(caller_subnet) = self.subnet_of(caller_node) {
        if ann.allowed_subnets.contains(&caller_subnet) {
            return true;
        }
    }
    for group in self.groups_of(caller_node) {
        if ann.allowed_groups.contains(&group) {
            return true;
        }
    }
    false
}
```

O(1) `latest_announcement` lookup (already exists) + a handful of
linear scans over short Vec<u8;16/32>'s. No new state machine, no
fold beyond what the capability index already does.

### 4. Wire points

Two call sites consult `may_execute`:

- **nRPC** — `Mesh::call_service` resolves target + capability tag,
  consults gate. Verdict false → return
  `CallError::CapabilityDenied`. Defense in depth: callee also
  consults `may_execute` with the inverted (caller, target) →
  `RpcRejectError::CapabilityDenied` on mismatch.
- **Future per-capability surfaces** (e.g. blob fetch with tag
  gating) wire the same way. Not in v1; the gate exists for
  callers to consult.

### 5. Announce-side: no separate gate needed

The model is "the announcer's allow-lists *are* the policy."
There's nothing to enforce at the receive side beyond what
`handle_capability_announcement` already does (sig verify + TOFU
bind + dedup). A peer publishing an announcement with itself in
`allowed_nodes` is just publishing a stricter policy — receivers
fold it and use it for the execute gate. No extra acceptance
check needed.

### 6. CLI helper

One subcommand on the existing operator CLI:

- `net cap announce --tag X --allow-node N1,N2 --allow-subnet S1
  --allow-group G1 --key <path>` — builds a signed announcement
  with the supplied allow-lists and publishes it. Operators wrap
  this in their own scripts.

That's the only CLI addition. No `policy view`, no `purge`, no
`group add` — group/subnet membership is just tags on
announcements, set the same way other capability tags are set.

### 7. Conformance test

`tests/capability_auth_conformance.rs`:

1. **Permissive baseline** — A publishes an announcement with all
   three allow-lists empty; B can execute.
2. **Allow-by-node** — A allows `[B]`; B can execute, C cannot.
3. **Allow-by-subnet** — A allows `[subnet S]`; nodes in S can
   execute, nodes outside cannot.
4. **Allow-by-group** — A allows `[group G]`; nodes claiming `G`
   via tag can execute, others cannot.
5. **Revocation** — A publishes v1 permissive, then v2 with
   `allowed_nodes = [self]`; B's execute fails after the v2
   announcement is folded.
6. **Receiver-side defense** — caller bypasses the local gate (test
   helper that skips it); callee independently rejects with
   `CapabilityDenied`.

---

## What this plan deliberately does NOT include

(All explicitly out of scope — these are the things Kyra flagged
in the prior draft as IAM-creep:)

- A policy language, allow + deny + priority rule lists.
- A separate policy channel.
- Operator keys distinct from entity keys (the entity key signs
  its own policy because the entity owns it).
- Group membership signed by an operator (it's a self-claimed
  tag).
- Snapshots, purges, advisory modes, compaction, retention
  strategies, audit-log browsers, fold-ready signals — none of it.
- A new error class on the binding surface beyond the one
  `CapabilityDenied` variant on the existing nRPC reject error.
- Cluster keys or threshold signatures.

If a real operator-driven policy surface is wanted later, it lands
as a separate plan on top of this one — but the substrate doesn't
need it.

---

## Cold-start behavior

There is no cold-start problem. The existing capability index
either has an announcement for peer X or it doesn't — `may_execute`
returns `false` if there's no announcement (you can't address what
you can't see). Once the announcement folds, the gate works the
same as steady-state. No "fold ready" signal, no buffered events,
no permissive-during-replay window distinct from the steady-state
permissive default.

The trade-off the prior draft was trying to solve (deny-until-fold-
ready vs allow-until-fold-ready) doesn't exist in this model
because the fold IS the announcement itself, and the announcement
is the same one that gets you the address in the first place.

---

## Risks

1. **Self-declared subnet/group membership lets a malicious peer
   claim any group.** Mitigation: an announcement's allow-lists are
   the *publisher's* assertion of who can call it. If A allows
   `Group(G)` and B falsely claims `G`, B can call A — but A is the
   one who declared the rule. The asymmetric trust is fine: A is
   trusting the *tag* system, and the tag system is signed by the
   claiming entity. Documented as a feature, not a bug. Operators
   who want stricter group membership use a 32-byte random `GroupId`
   that's hard to guess; the value-as-secret pattern is the
   substrate's existing "shared secret" idiom (e.g. channel auth
   tokens).

2. **A peer with no announcement is unreachable.** This is the same
   condition as today: a peer that hasn't published a
   capability announcement can't be `call_service`'d either, because
   the index doesn't know it. No regression.

3. **Per-tag granularity not supported.** The allow-lists apply to
   the whole announcement, not per-tag. An entity that wants
   different policies per tag publishes multiple announcements — but
   today's announcement model is one-per-entity, so this would
   require either multi-announcement-per-entity or per-tag overrides
   inside the announcement. Documented as a known limit; deferred
   until a real workload asks for it.

4. **Allow-list size ≤ 64.** A single allow-list field bounded at 64
   entries keeps the announcement under the existing wire-size
   ceiling. Operators with > 64 nodes use a group.

5. **Revocation latency = announcement TTL + propagation.** A
   revocation (new announcement with stricter lists) propagates at
   the same rate as any other announcement (mesh broadcast +
   pingwave forwarding). Operators who need < 1s revocation use
   the existing channel-auth `WriteToken` revocation path, not
   this; capability-auth is steady-state coarse, not crisis-grade
   fast.

---

## Phases

Small enough to land as one branch, but breaks naturally into
three commits:

### Phase 1 — types + wire format

Add `SubnetId`, `GroupId`, the three allow-list fields on
`CapabilityAnnouncement`. Wire round-trip tests, signed round-trip
tests proving byte-identity with v0.x when lists are empty.

### Phase 2 — execute gate

Implement `CapabilityIndex::may_execute`, wire into
`Mesh::call_service` (caller side + callee side). Add the
`CapabilityDenied` error variant. Unit tests on the gate function
in isolation, integration test for the call path.

### Phase 3 — CLI + conformance

`net cap announce` subcommand. The 6-test conformance file. Plan
doc updated to reflect ship status.

Total: ~400 lines of code + tests. One short feature, not a
subsystem.

---

## Test plan

- Unit: `may_execute` covered by ~10 cases (permissive / by-node /
  by-subnet / by-group / no-tag / no-announcement / multiple
  allow-lists overlap / revocation supersedes / hop > 0 forwarded
  / per-axis empty-vs-nonempty).
- Integration: the 6 conformance scenarios above.
- Wire compat: signed-payload byte-identity test (v0.x ann round-
  trips through v0.x reader after passing through a v0.4 writer
  with empty allow-lists).

---

## Migration

Pre-this-plan peers serialize and verify announcements unchanged
(empty allow-lists serialize to nothing, byte form is identical).
Post-this-plan peers reading a pre-this-plan announcement default
the three Vecs to empty → permissive. No flag day, no rolling-
upgrade ceremony.

---

## Open questions

1. **Should `may_execute` also check the *caller's* announcement
   (the receiver's view of "is this caller real"), or just trust
   the wire-level entity binding?** Default: trust the wire-level
   binding (which `handle_capability_announcement` already
   establishes via TOFU). The caller's announcement isn't needed
   because the execute-gate doesn't care about the caller's
   capabilities, just their identity + subnet/group membership.

2. **Should the allow-list checks short-circuit on first match (per
   the pseudocode above) or scan all three in case of operator
   intent ambiguity?** Default: short-circuit. The three lists are
   union-semantics, so any match suffices; there's no precedence
   issue because there are no deny rules.

3. **Do we need a `net cap revoke` CLI shortcut, or is `net cap
   announce` with a tighter allow-list good enough?** Default:
   no shortcut — `announce` is the only verb.
