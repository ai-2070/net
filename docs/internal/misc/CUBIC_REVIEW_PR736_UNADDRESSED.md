# Cubic AI review of PR #736 — outstanding items

**PR:** [ai-2070/net#736 — Security: Scoped Capabilities](https://github.com/ai-2070/net/pull/736)
(`security-scoped-capabilities` → `master`, merged 2026-08-01)

**Checked against:** `net-scoped` @ `1f73d42dc` (merge of #736)
**Date of check:** 2026-08-01

Cubic left **24 inline review comments** across six passes. Twenty-three are
addressed in the merged tree. One is addressed only in part — the code took a
different remedy than the one requested, and the gap that remedy leaves is
reachable through a public API.

---

## OPEN (partial) — P1: `SameSubnet` derives a node property from a per-class entry

- **Comment:** [`#discussion_r3695392114`](https://github.com/ai-2070/net/pull/736#discussion_r3695392114)
- **File:** `net/crates/net/src/adapter/net/mesh.rs` (comment anchored at line 36268;
  the live code is `find_nodes_by_filter_scoped` at `mesh.rs:26686`)
- **Severity:** P1 (cubic's own classification)
- **Status:** partially addressed — documented and guarded, not fixed

### What cubic asked for

> `SameSubnet` is not safe to resolve from the selected entry's tags: remote nodes
> can publish nonzero-class `CapabilityMembership` entries through the public fold
> API, so one publisher may have sibling entries with different subnet tags and the
> query result can depend on the capability class searched. The invariant/test
> should cover the production `SUBPROTOCOL_FOLD` path and subnet resolution should
> use a node-wide tag union (or otherwise enforce one entry per publisher).

Two asks: (a) resolve the subnet from the node-wide tag union, and (b) make the
test cover the production `SUBPROTOCOL_FOLD` path.

### What shipped instead

Neither ask was implemented. What landed is a documented invariant plus a
tripwire test:

- `mesh.rs:26631-26643` — a rustdoc block on `find_nodes_by_filter_scoped`
  arguing the single-entry derivation is safe "only because a publisher owns
  exactly one fold entry on this path: `translate_announcement` pins `class_hash`
  to the 0 cutover sentinel", and naming `tags_union_for` as "the fix if that day
  comes".
- `mesh.rs:36341` — `a_publisher_owns_exactly_one_fold_entry_on_the_announcement_path`,
  a test that injects two announcements from one publisher and asserts the fold
  holds exactly one entry with `classes == vec![0]`.

The closure itself still derives from the selected entry (`mesh.rs:26701-26713`),
handed the borrowed tags of whichever entry the capability filter matched
(`capability_bridge.rs:1384`).

### Why the gap is still reachable

The shipped argument is scoped to the *in-crate announcement* path. The fold is
keyed per class regardless of how an entry arrived:

- `behavior/fold/capability.rs:498-500` — `key_for(node_id, payload) = (payload.class_hash, node_id)`.
  Sibling entries per publisher are the fold's normal shape, not an anomaly.
- `behavior/fold/capability.rs:78-82` — the payload doc says outright: "a publisher
  in multiple classes emits one announcement per class."
- `mesh.rs:24593` — `pub async fn publish_capability_membership(&self, membership: CapabilityMembership)`
  is a **public, non-`#[cfg(test)]` `MeshNode` API** that forwards the
  caller-supplied `class_hash` straight into `publish_fold` and out over
  `SUBPROTOCOL_FOLD`. It has no non-test callers in this crate — its only
  purpose is external use.

So the class-0 pin holds for `CapabilityAnnouncement` traffic this node
translates, but it does not constrain what a *remote* peer puts on the wire. A
peer that publishes a nonzero-class membership omitting the policy's tags gets a
sibling entry alongside its class-0 entry; a `SameSubnet` query whose capability
filter happens to select the sibling resolves that peer to `GLOBAL` and drops it.
The verdict then depends on which capability was searched for — the exact failure
mode the rustdoc at `mesh.rs:26637-26640` describes as "a real defect."

The test does not close this either: it drives
`test_inject_capability_announcement` (`mesh.rs:27682`), which is the
announcement path. Nothing exercises `publish_capability_membership` or an
inbound `SUBPROTOCOL_FOLD` fold packet carrying a nonzero class, which is where
cubic said the coverage needed to be.

The rustdoc's supporting claim — "Every non-zero `class_hash` writer in the crate
is `#[cfg(test)]`" (`mesh.rs:36334-36335`) — is true of in-crate *call sites* but
misleading as a safety argument, since the writer that matters is a public API
whose callers are by definition out of crate.

### Options

1. **Take the requested fix.** Switch the closure to
   `tags_union_for(state, node_id)` (`behavior/fold/capability.rs:915`) within the
   selecting snapshot. This is already named in the code as the intended remedy,
   and it makes the subnet a node property as the docs say it should be. Cost:
   the union walk allocates and runs under the fold read locks — the "Cost under
   the fold locks" section at `mesh.rs:26662-26685` would need updating, since
   the "no allocation" claim would no longer hold.
2. **Enforce the invariant rather than documenting it.** Reject non-zero
   `class_hash` on the inbound capability-fold path (or in `CapabilityFold::apply`)
   until per-class sharding is genuinely wanted. Turns a comment into a
   check, and makes the existing tripwire test redundant rather than load-bearing.
3. **Accept and re-scope the claim.** If neither is wanted, the rustdoc should say
   the invariant covers only announcements this node translates, and that a peer
   publishing directly through `publish_capability_membership` can produce a
   class-dependent `SameSubnet` verdict — rather than presenting the single-entry
   property as unconditional.

---

## Addressed — verified in tree

Recorded so a re-review does not have to re-derive these. Nine carry cubic's own
`✅ Addressed` marker; the rest were closed by commits after the marker was last
written, and were confirmed by reading the merged code.

| # | Comment | File / anchor | Verified at |
| - | ------- | ------------- | ----------- |
| 1 | `any` filter equated with unscoped query | `sdk-ts/src/capabilities.ts:401` | cubic-confirmed `7391b6f` |
| 2 | Warning named plural `--allow-subnets` / `--allow-groups` | `cli/src/commands/cap.rs:270` | `cap.rs:243-255` now names the singular longs; `advisory_warning_names_only_real_flags` pins it |
| 3 | `-12` could drift past the parity test | `src/ffi/mod.rs:463` | cubic-confirmed |
| 4 | `PartialEq` doc still called `GroupId` a bearer secret | `behavior/group.rs:81` | cubic-confirmed |
| 5 | Multi-class publisher misclassified (first P1) | `mesh.rs:26528` | cubic-confirmed; **reopened as the P1 above** |
| 6 | Second allocation + sort in direct subnet assignment | `subnet/assignment.rs:107` | cubic-confirmed |
| 7 | Rustdoc documented the old one-arg callback | `fold/capability_bridge.rs:1332` | `capability_bridge.rs:1336` signature and doc now agree |
| 8 | `tags_match_scope` no longer allocation-free | `behavior/capability.rs:915` | cubic-confirmed |
| 9 | Doc described a sort `assign` no longer performs | `subnet/assignment.rs:125` | `assignment.rs:197-210` describes the smallest-match scan |
| 10 | `with_subnet` called the "ONLY way" (×3 comments) | `mesh.rs:2333` | `mesh.rs:2333-2337` — "builder method … the `subnet` field is public and can be assigned directly"; second site dropped in `d7449eec0` |
| 11 | Warning fired for a no-op empty `SubnetPolicy` | `mesh.rs:8262` | `mesh.rs:8272-8275` gates on `can_assign_non_global()` |
| 12 | Test did not cover `cap announce` | `cli/src/commands/cap.rs:549` | `capability_set_from_tags` extracted from `run_announce` (`cap.rs:308`); `a_reserved_tag_never_reaches_the_announcement` (`cap.rs:596`) drives it |
| 13 | `ALL` list not coupled to the enum | `src/ffi/mod.rs:2368` | `net_error_codes!` generates both enum and `ALL_NET_ERRORS` (`ffi/mod.rs:415-448`) |
| 14 | Contract said "first tag wins", code used smallest | `subnet/assignment.rs:129` | `assignment.rs:35-45` — "Smallest matching tag wins per rule" |
| 15 | Empty-prefix/empty-value mapping reported as scoping (×2) | `subnet/assignment.rs:123`, `:130` | `assignment.rs:160-161` skips it; `assignment.rs:128-135` documents why |
| 16 | Later same-level rules zeroing an earlier assignment | `subnet/assignment.rs:123` | `assignment.rs:250-259` — a `0` mapping is skipped, not written |
| 17 | `add_tag` warning lost the "stays globally visible" consequence | `behavior/capability.rs:1215` | `capability.rs:1218-1219` restores it |
| 18 | Empty `--tag` diagnosed as a reserved-prefix problem | `cli/src/commands/cap.rs:295` | `tag_rejected_message` branches on `ReservedPrefix` vs `Empty` (`cap.rs:272-287`); pinned by `an_empty_tag_is_not_diagnosed_as_a_reserved_prefix` (`cap.rs:647`) |
| 19 | Any policy + GLOBAL local subnet described as the inversion | `mesh.rs:2350` | `mesh.rs:2351-2358` and `mesh.rs:23794-23798` both qualify with `can_assign_non_global()` |
| 20 | Predicate excluded an empty tag that `assign_from_rendered_tags` accepted | `subnet/assignment.rs:149` | `assignment.rs:229-243` discards empty tags, so the two agree (`12ddc6346`) |
| 21 | Audit cited `d1ac7e00c` as the terminal `-12` fix | `docs/internal/misc/SECURITY_AUDIT_2026_07_31_SCOPED_CAPABILITIES.md:79` | corrections table now says "Cite `2abee11cd`, not `d1ac7e00c`" (`109a4ed22`) |

---

## How this was checked

```
gh api "repos/ai-2070/net/pulls/736/comments?per_page=100" --paginate \
  --jq '.[] | select(.user.login|test("cubic";"i"))
        | [.id, .path, (.original_line|tostring),
           (if (.body|test("✅ Addressed")) then "MARKED-ADDRESSED" else "OPEN" end)] | @tsv'
```

Each result was then read against the merged tree at `1f73d42dc` rather than
trusting the marker — several comments with no `✅` were in fact closed by later
commits (`12ddc6346`, `4c1f0fa54`, `109a4ed22`, `d7449eec0`, `baf466fcb`,
`6ff237158`, `9ae19d4ed`, `5e4d914bb`, `20d978efc`), and one comment carrying an
`✅` (item 5) was reopened by a later pass.
