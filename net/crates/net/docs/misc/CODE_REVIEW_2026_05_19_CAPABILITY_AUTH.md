# Code review — `capability-auth` branch (2026-05-19)

Branch base: `master` at `c2957141`.
Branch tip: `fd4111f0` ("net cap announce + 6-scenario conformance file (cap-auth Phase 3)").
Scope: ~2.6k LOC across the substrate, SDK, CLI, and integration tests. Implements `CAPABILITY_AUTH_PLAN.md` Phases 1–3 (data fields + execute gate + CLI + conformance).

Findings below are organised by severity. File paths are relative to repo root; line numbers reflect the branch tip and may drift.

---

## HIGH

### H1 — Callee bridge silently permissive when self never announced
`net/crates/net/src/adapter/net/mesh_rpc.rs:1610-1644`. The bridge skips the gate when `have_self_ann = false`:

```rust
let have_self_ann = index.get(self_node).is_some();
if have_self_ann
    && from_node != 0
    && !index.may_execute(self_node, &tag, from_node) { /* deny */ }
```

A node that registers `serve_rpc("admin", …)` and never calls `announce_capabilities` accepts any inbound call from any reachable peer — the caller-side gate can't catch this (direct `call()` bypasses caller-side discovery), and the callee gate gives up.

`CAPABILITY_AUTH_PLAN.md` §"Cold-start behavior" says no announcement = deny. The implementation says the opposite. Cleanest fix: have `serve_rpc` lazily emit a default-permissive self-announcement so `have_self_ann` is always true once any service is registered; the empty allow-lists then carry the permissive default through the existing path.

No conformance scenario covers this case — scenario 6 folds a `restrictive` announcement into the target's index, so `have_self_ann` is always true in test.

### H2 — `announce_capabilities` / `serve_rpc` ordering is now load-bearing
Implicit in `net/crates/net/tests/dataforts_blob_e2e.rs:875-887` (the test had to be reordered to keep working). The new contract is:

```
serve_rpc(...)              // first
announce_capabilities(...)  // merges nrpc:<service> tag
```

If reversed, the self-announcement lacks the `nrpc:<service>` tag, `may_execute`'s `has_tag` check fails, and **every inbound call to that service is denied** until a re-announce happens. The blob test comment is the only place this contract is recorded. Two mitigations:

- Document in the `serve_rpc` / `announce_capabilities` rustdoc with a `# Ordering` section.
- Better: `serve_rpc` schedules an immediate re-announce so the order doesn't matter to callers.

### H3 — `net cap announce --node-id` allows node/entity binding mismatch
`net/crates/net/cli/src/commands/cap.rs:258`:

```rust
let node_id = match args.node_id.as_deref() {
    Some(s) => parse_node_id(s)?,
    None => keypair.node_id(),
};
```

The `--node-id` override decouples the announcement's `node_id` from the keypair's derived id while still using the keypair's `entity_id` and signature. Receivers reject announcements where the `node_id ↔ entity_id` binding doesn't match the signing key, so any operator who reaches for this override generates unusable bytes that fail silently downstream.

Fix: either validate the override matches `keypair.node_id()` and `invalid_args` if it doesn't, or remove the flag. If a "sign on behalf of another node" workflow is actually wanted, it needs a separate code path that also resolves the other node's `entity_id` and signing key.

### H4 — Subnet parsing is non-deterministic across announcements with multiple `subnet:` tags
`net/crates/net/src/adapter/net/behavior/capability.rs:737` (`parse_membership_tags`):

```rust
for tag in tags {       // tags: &HashSet<Tag>
    let rendered = tag.to_string();
    if subnet.is_none() {
        if let Some(s) = SubnetId::from_tag(&rendered) {
            subnet = Some(s);
            continue;
        }
    }
    …
}
```

`HashSet<Tag>` iteration order is unspecified. If an operator publishes two `subnet:<hex>` tags (against the model's "one per announcement" expectation), which one binds to the gate depends on hash iteration — meaning the same announcement can grant or deny depending on receiver-local hash randomization. The doc comment acknowledges multiple subnet tags are out of model, but the parser silently picks one rather than rejecting. Either reject (treat as malformed) or collect-and-sort before picking so behavior is deterministic across receivers.

---

## MED

### M1 — `MAX_ALLOW_LIST_LEN` enforced in CLI but not on the wire
`net/crates/net/cli/src/commands/cap.rs:241` correctly rejects oversized lists at announce time. But the receive path (`CapabilityAnnouncement::from_bytes` → `CapabilityIndex::index` in `behavior/capability.rs:2012, 2857`) accepts any vector length. A malicious or buggy peer can ship a million-entry `allowed_nodes` and it will fold; every `may_execute` then linearly scans it. Add the cap check in `from_bytes` (or as a step in `verify`) so the wire is symmetric with the CLI.

### M2 — Plan §3 vs Locked design point #1 still contradicts
`net/crates/net/docs/plans/CAPABILITY_AUTH_PLAN.md` §3 step 5/6 reads the caller's announcement for subnet/group membership, but "Locked design point #1" says "may_execute does NOT consult the caller's own `CapabilityAnnouncement`." The implementation goes with §3 (does consult for subnet/group). Reword the locked point to "does not consult for capability claims — only for self-declared membership," or revisit which behavior is wanted.

### M3 — `call_service` gates after target selection instead of filtering candidates
`net/crates/net/src/adapter/net/mesh_rpc.rs:2426-2446`. The flow today is:

```rust
let target = self.select_target(&candidates, &opts.routing_policy);
let tag = format!("nrpc:{service}");
if !self.capability_index_arc().may_execute(target, &tag, self.node_id()) {
    return Err(RpcError::CapabilityDenied { … });
}
```

If `candidates = [A, B, C]` and A denies the caller but B/C admit, `select_target` may pick A and the call fails with `CapabilityDenied` even though two valid targets existed. The routing policy is being applied to a set that includes peers the caller can't actually call. Filter `candidates` by `may_execute` BEFORE selection — and on empty-after-filter, return `CapabilityDenied` (or a new `NoAuthorizedTarget`) so the caller can distinguish "found nobody" from "found peers, none let me in."

### M4 — Duplicate `--tag` CLI arg falsely reported as a parse error
`net/crates/net/cli/src/commands/cap.rs:272-281`. The size-delta heuristic conflates "tag was rejected by the parser" with "tag was already present in the set":

```rust
let before = caps.tags.len();
caps = caps.add_tag(tag.clone());
if caps.tags.len() == before {
    return Err(invalid_args(format!("tag {tag:?} could not be parsed …")));
}
```

`--tag nrpc:echo --tag nrpc:echo` errors out with the reserved-prefix message. Use the `Tag::parse_user` result directly (return `Err` on parser failure, ignore the dedup case).

---

## LOW

### L1 — `from_node == 0` skip is safe only by implicit invariant
`mesh_rpc.rs:1641`. Production wire delivery (`mesh.rs:4147`) drops events with no resolvable NodeId rather than passing 0, so the gate's `from_node != 0` skip is effectively unreachable from over the wire. Good today. The risk is that a future refactor of `mesh.rs:4133-4156` falling back to `from_node = 0` instead of dropping silently opens the gate. Add a doc-comment on `RpcInboundEvent::from_node` (`cortex/rpc.rs:944`) declaring "production wire delivery MUST NOT use 0" so the invariant is recorded next to the field that carries it.

### L2 — Conformance scenarios 2/3/4 weak-form-assert the allowed path
`net/crates/net/tests/capability_auth_conformance.rs`. The allowed-caller assertion is `!matches!(err, CapabilityDenied { .. })` rather than `Ok(...)`. That pins the gate verdict but not end-to-end RPC delivery. Scenario 1 does register a handler and assert success, so the end-to-end path is covered once — but the per-axis tests could be tightened (or at minimum, note in their docstring that this is intentional to skip the handler-not-registered detour).

### L3 — `signature_byte_identity_with_pre_v04_unrestricted_announcement` overpromises its name
`behavior/capability.rs:5290` (test name). The body never compares against a "pre-v0.4 producer" — it just asserts the v0.4 JSON object lacks the three keys when empty. That's a useful invariant, but it's the same one already pinned by `empty_allow_lists_omit_fields_from_wire`, just through `signed_payload()` instead of `to_bytes()`. Either rename to reflect what it actually checks (e.g. `signed_payload_omits_empty_allow_lists`) or strengthen it by crafting the pre-v0.4 byte form and asserting equality.

### L4 — Duplicated hex codec across `subnet.rs` and `group.rs`
`behavior/subnet.rs` and `behavior/group.rs` ship identical `hex_nibble` + near-identical `from_tag`/`to_tag`. `Signature64` in `behavior/capability.rs` already uses the `hex` crate; reusing it here would delete ~20 lines per file. Pure cleanup.

### L5 — `SubnetId(pub [u8; 16])` / `GroupId(pub [u8; 32])` inner field is public
Both types expose the inner array as `pub`. The new tests in `capability.rs` rely on this for `SubnetId([0x55; 16])` construction. Consistent with the existing `Signature64(pub [u8; 64])` style, so probably intentional, but `from_bytes` already exists — the `pub` is redundant API surface.

### L6 — `NodeId` type is fragmented across the crate
`allowed_nodes: Vec<u64>` matches the in-file `node_id: u64` style in `capability.rs`, but the project has `pub type NodeId = u64` in `behavior/placement.rs` AND `pub type NodeId = [u8; 32]` in `behavior/metadata.rs`. Not worth introducing a dependency in this change, just noting that "what is a NodeId?" has two answers in this crate.

---

## What's solid

- Wire-format approach (`#[serde(default, skip_serializing_if = "Vec::is_empty")]`) is the right call; byte-identity tests pin the rolling-upgrade contract.
- `CapabilityAnnouncement` is JSON-only on the wire (`to_bytes` → `serde_json::to_vec`); no postcard call site means the empty-vec-omit invariant is actually preserved on the only encoding path.
- `RpcStatus::CapabilityDenied = 0x0008` correctly bumps the reserved range, and the existing reserved-range test gets shifted (catches drift if someone forgets the bump again).
- `default_retryable(RpcError::CapabilityDenied) → false` — a deny verdict won't change on retry. Correct.
- SDK aliases `CapabilityGroupId` / `CapabilitySubnetId` cleanly avoid collision with the unrelated `subnets::SubnetId`.
- `emit_for_bridge` cloned before the fold takes ownership — no race in the callee-side rejection path.
- Conformance scenarios 1, 5, and 6 are strong-form (assert success / explicit denial type). `helper_fold_announcement_lands_in_every_index` guarding the helper itself is a nice touch.
- CLI test coverage: signed-bytes round-trip, stdout-vs-file equivalence, malformed-arg exit codes — covers the CLI's contract well.
- `from_tag` length pre-check makes the `chunks(2)` decode panic-free.
- Doc-comments explain the *why* (wire compat, self-declared membership safety, value-as-secret pattern) rather than restating the code.

---

## Summary

Four HIGH issues cluster around two themes: the callee bridge / `serve_rpc` flow (H1, H2) needs a `serve_rpc`-side fix that lazily emits a default-permissive self-announce so `have_self_ann` is always true with the right tag merged; the CLI + index correctness (H3, H4) needs the `--node-id` flag to refuse binding mismatches and the membership-tag parser to behave deterministically across receivers. M3 (target-selection ordering) is the next-most-impactful — today's flow can return `CapabilityDenied` when an authorized peer exists. M1 (wire-side cap enforcement), M2 (plan-doc contradiction), and M4 (CLI dedup heuristic) are small hardening / clarity steps. Phases 1–3 are otherwise a clean, well-tested landing.
