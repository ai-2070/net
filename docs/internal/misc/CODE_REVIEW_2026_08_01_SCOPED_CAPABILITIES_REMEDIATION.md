# Code Review — Scoped-Capabilities Remediation (2026-08-01)

Scope: the 23 commits on `security-scoped-capabilities` versus `master` — the
`SECURITY_AUDIT_2026_07_31_SCOPED_CAPABILITIES.md` audit itself plus every fix
landed against it. Covers `subnet_visible`, `find_nodes_by_filter_scoped` and
the fold bridge, `PreparedScope` / `tags_match_scope`,
`SubnetPolicy::assign_from_rendered_tags`, the four binding converters, the
`NET_ERR_INVALID_ARGUMENT` surface, and the advisory-axis documentation pass.

**Reviewed source:** `4427ce2edef88bb3f131e7a9fc72f8f3b94086ea` (branch
`security-scoped-capabilities`). All line references are against that SHA;
findings 6-8 were added in a second pass and are equally valid at `3e865a02e`,
which changed only this document.

Method: manual read of the full diff, hand-verification of the two load-bearing
equivalence claims, then a build/lint/test pass. This document was subsequently
reviewed itself — see *Review of this review* for what that pass corrected.

## Verification performed

| Check | Result |
| ----- | ------ |
| `cargo test --lib -- scope subnet_visible tags_match assign_from_rendered` | 191 passed, 0 failed |
| `cargo test --test capability_scope` | 8 passed, 0 failed |
| `cargo clippy --lib --all-features` | clean |
| `cargo doc --no-deps --lib` | clean |

Two equivalence claims were re-derived by hand rather than taken from their
tests:

- `tags_match_prepared` reproduces
  `matches_scope(&scope_from_membership_tags(tags), ..)` on every arm. The
  `SameSubnet` early return is sound because every arm of the reference reduces
  to `same_subnet` under that filter — including the `SubnetLocal` candidate
  arm — so the tags cannot move the verdict.
- `assign_from_rendered_tags`' "lexicographically smallest matching tag per
  rule" is exactly `assign`'s post-sort first-match, because the original
  `break` fires only when the prefix matches **and** the value is mapped.

`SubnetId::is_same_subnet` is plain `u32` equality, so the `SubnetLocal`
unknown arm (`None => source.is_global()`) is byte-identical to the previous
`source.is_same_subnet(GLOBAL)`. Only `ParentVisible` changes verdict, as the
commit claims.

## Assessment

The remediation is sound and the test shape is right. Two things are worth
calling out as done well:

- `SubnetPolicy::assign` delegating to `assign_from_rendered_tags` means the
  fast path and the reference cannot drift apart — the same discipline applied
  to `matches_scope` / `tags_match_scope` via
  `tags_match_scope_agrees_with_materialized_scope`. Reference-oracle tests
  rather than hand-enumerated expectations.
- `same_subnet_admits_forwarded_peer_in_the_same_subnet` is the correct
  companion to the exclusion test. Without it, "exclude every unresolved
  candidate" would pass the security assertion while silently making
  `SameSubnet` useless past one hop.

The findings below are one residual security gap, one user-facing defect, and
six documentation/comment defects. None block the branch.

Findings 6-8 were added in a second pass (see *Review of this review*, below).
The first pass asserted that the remaining contract drift was confined to
findings 3-5; that was wrong. Three further source comments still describe
behaviour the branch replaced, and one of them — `local_subnet_policy`'s — is
what makes the finding-2 misconfiguration look correct to an operator reading
the API docs.

---

## 1. CLI security warning names flags that do not exist

**Severity:** Low, but directly operator-facing.
**Location:** `net/crates/net/cli/src/commands/cap.rs:267`, `:270`, `:276`

The runtime warning added for the advisory allow-list axes reads:

```
warning: --allow-subnets / --allow-groups are ADVISORY and do not restrict access.
         ...
         Use --allow-nodes for access control.
```

The clap long names are singular:

- `--allow-node` (`cap.rs:105`)
- `--allow-subnet` (`cap.rs:111`)
- `--allow-group` (`cap.rs:117`)

The comment at `:267` carries the same slip. `--allow-nodes` is the single
actionable instruction in a security warning, and an operator who follows it
verbatim gets a clap parse error. The doc comments at `:43-44` already use the
correct singular spellings, so the drift is confined to the new warning text.

**Fix:** correct the three plural spellings in the `eprintln!` body and the
comment above it.

---

## 2. Residual `ParentVisible` fail-open when a policy is installed but `local_subnet` is left global

**Severity:** Medium. Narrower than the original HIGH #2 but the same shape.
**Locations:**

- `net/crates/net/src/adapter/net/mesh.rs:23713` — the `ParentVisible` unknown arm
- `net/crates/net/src/adapter/net/mesh.rs:8225` — `let local_subnet = config.subnet;`
- `net/crates/net/src/adapter/net/mesh.rs:2332` / `:2341` — `with_subnet` / `with_subnet_policy`

The fix resolves an underivable peer subnet to `source.is_global()`:

```rust
Visibility::ParentVisible => match dest {
    Some(dest) => dest.is_ancestor_of(source),
    None => source.is_global(),
}
```

The flat-mesh justification is correct when no policy is installed. But
`local_subnet` is read only from `config.subnet`, defaults to `SubnetId::GLOBAL`,
and is **never** derived from `subnet_policy` — the policy assigns *peers'*
subnets, not this node's. `with_subnet_policy(...)` without `with_subnet(...)` is
therefore a reachable configuration in which the fix does not engage:

| Peer state on a `local_subnet == GLOBAL` node, `ParentVisible` | Verdict |
| ------------------------------------------------------------- | ------- |
| Subnet resolved to a concrete value, e.g. `[3]` | rejected (`[3].is_ancestor_of(GLOBAL)` is false) |
| Subnet underivable | **admitted** (`source.is_global()`) |

An unresolved peer ends up strictly more privileged than a resolved one, on the
authorization path that otherwise answers `AckReason::Unauthorized` — which is
the inversion HIGH #2 was filed against.

`unscoped_node_still_admits_unknown_peer_subnet` (`mesh.rs:36177`) pins this as
intended flat-mesh compatibility. That is defensible with no policy installed;
it is not obviously intended when the operator has installed one and is running
`ParentVisible` channels.

The audit's remediation table marks HIGH #2 **Fixed** without qualification. It
is fixed for nodes that also set `with_subnet`.

**Suggested fix:** `warn!` (or `debug_assert!`) at `MeshNode::new` when
`subnet_policy.is_some() && subnet.is_global()`, since that pairing is almost
certainly a misconfiguration; and qualify the HIGH #2 row to say the fail-closed
behaviour requires a non-global `local_subnet`.

### Decision (2026-08-01)

Two fixes were put to the operator and the **warning-only** one was chosen.

The alternative considered and **rejected** was a behavioural fix: thread
"a `SubnetPolicy` is installed" into `subnet_visible` and fail closed on an
unknown peer subnet whenever it is, staying permissive only when no policy
exists (in which case `peer_subnets` is structurally always empty and every peer
is unknown forever, so permissiveness is the only workable default). That would
have closed the gap rather than annotating it.

It was rejected because of its blast radius on the channel paths. `peer_subnets`
is written only for `signature_verified && hop_count == 0`, so a direct session
peer that subscribes without ever publishing a capability announcement is
unknown *permanently*, not transiently. Under the behavioural fix every such
subscriber would flip from admitted to `AckReason::Unauthorized` on
`ParentVisible` channels the moment a policy was installed — a silent,
deployment-wide subscription outage in exchange for closing a gap that only
opens under a configuration (`with_subnet_policy` without `with_subnet`) that is
itself a mistake. The branch's own `subnet_visible` fix was deliberately
minimal, reproducing the previous verdict for every input except the vulnerable
one; extending it this far is a separate decision that deserves its own change,
not a rider on a review pass.

**Therefore:** `subnet_visible` is unchanged. The residual gap stays open, by
decision, and is recorded here so it is not rediscovered as an oversight.

**Status: the chosen mitigation is NOT yet implemented.** Two changes are
outstanding and neither exists in the tree as of `3e865a02e`:

1. A `warn` at `MeshNode::new` for `subnet_policy.is_some() && subnet.is_global()`.
   `mesh.rs:8225-8226` copies the two settings independently with no check
   between them.
2. A qualifier on the audit's HIGH #2 row
   (`SECURITY_AUDIT_2026_07_31_SCOPED_CAPABILITIES.md:42`), which still reads a
   flat **Fixed** and does not record that fail-closed behaviour requires a
   non-global `local_subnet`, nor that warning-only was a decision.

Until both land, this section describes an intent, not a state. The warning's
condition must be the *conjunction* — a policy with a scoped local subnet is
correct, and a global local subnet with no policy is correct; only the pairing
is suspect — so the test for it should pin all four combinations rather than
either condition alone.

---

## 3. Test comments contradict the invariant the branch establishes

**Severity:** Low (documentation), but it contradicts a headline invariant.
**Location:** `net/crates/net/tests/capability_scope.rs:598`, `:615`

```rust
// The direct path still works alongside it: B handshook with D
// directly, so it resolves through `peer_subnets`.
...
"B (direct peer, resolved via peer_subnets) must still appear \
 under SameSubnet"
```

Scoped discovery no longer reads `peer_subnets` at all. That is the entire point
of `01a47e7df`, is stated in a 20-line doc block on
`find_nodes_by_filter_scoped` (`mesh.rs:26441-26511`), and is pinned by the
three tests in `scoped_discovery_ignores_peer_subnets_tests` (`mesh.rs:35986`).
B resolves from its own `region:us` tag through the fold, exactly as A does —
which is the property `resolution_does_not_depend_on_whether_a_sidecar_entry_exists`
asserts.

Left as-is, a future reader looking for the sidecar's remaining query-path role
will find a test claiming one exists.

**Fix:** reword both to say B resolves from its announced `region:us` tag via the
fold, same as the forwarded peer — the point of the assertion is that a direct
peer is not treated differently, not that it takes a different route.

---

## 4. Merge artifact in the `tags_match_scope` doc comment

**Severity:** Low.
**Location:** `net/crates/net/src/adapter/net/behavior/capability.rs:888-908`

Two doc headers are concatenated without a separator:

```rust
/// `scope_from_membership_tags` is retained as the readable reference
/// definition; `tags_match_scope_agrees_with_materialized_scope` pins
/// the two to the same verdict across the matrix.
/// Convenience wrapper that prepares and evaluates in one call.
///
/// NOT for the query path — ...
```

The first three paragraphs (`:888-902`) describe the allocation-free query path
— that is, `PreparedScope::matches` (`:958`) — while the paragraph immediately
after states this wrapper is explicitly *not* for the query path. As rendered,
the item has two summary lines and a self-contradicting body.

**Fix:** move `:888-902` onto `PreparedScope::matches`, leaving
`tags_match_scope` with the "convenience wrapper, not for the query path" text.

---

## 5. Subnet derivation moved *into* the fold locks

**Severity:** Low. Correct trade-off; wants a stated bound.
**Locations:**

- `net/crates/net/src/adapter/net/mesh.rs:26536` — `policy.assign_from_rendered_tags(tags) == my_subnet`
- `net/crates/net/src/adapter/net/behavior/fold/capability_bridge.rs:1377` — the closure call inside `with_state_and_index`

Under `ScopeFilter::SameSubnet` the subnet closure now runs
`assign_from_rendered_tags` — O(rules × announcer_tags) string prefix compares
per candidate — *inside* the fold's state and index read locks, replacing a
`DashMap` lookup that previously ran after the locks dropped.

This is the right trade on correctness grounds: the single-snapshot argument in
`find_nodes_matching_scoped`'s "Single snapshot" section requires the closure to
run under the selecting snapshot, and there is no way to satisfy it while
keeping the resolution outside the locks.

What can be stated about cost is bounded and factual:

> The path performs no per-candidate allocation. Lock-held work is
> O(operator_rules × announcement_tags), with announcement tags bounded by the
> wire payload and rules controlled locally.

An earlier draft of this finding claimed the new work was "strictly cheaper"
than the per-candidate `String`/`Vec` allocation `PreparedScope` removed from
the same region. That is not established and should not be asserted: the two
paths do different work — scope matching scans tags once, subnet assignment does
`rules × tags` prefix strips and map lookups — so which dominates depends on
rule count and tag shape. Allocation-freedom is observable from the source;
"cheaper" is a benchmark claim and no benchmark was run.

Recorded because it cuts slightly against that commit's own rationale —
attacker-shaped work under the fold locks — and the input is announcer-
controlled: no per-announcement tag-count cap was found in `capability.rs`, so
the tag count is bounded only by wire size.

**Suggested action:** either state the bound explicitly in the
`find_nodes_by_filter_scoped` doc block (wire size caps tag count, rules are
operator-configured and small), or add a tag-count cap if one is wanted for
other reasons. No code change required for correctness.

---

## 6. `local_subnet_policy()` documents a derivation that does not exist

**Severity:** Medium. False API documentation that argues *for* the finding-2
misconfiguration.
**Location:** `net/crates/net/src/adapter/net/mesh.rs:10213-10217`

```rust
/// Read-only handle to the `SubnetPolicy` that derived this
/// node's `local_subnet`, when one was supplied. `None` when
/// the local subnet came from `MeshNodeConfig::subnet`
/// directly without going through a policy. Operator tools
/// surface this to explain "why is this node in subnet X."
pub fn local_subnet_policy(&self) -> Option<&Arc<SubnetPolicy>> {
```

Every clause here is false. `local_subnet` is *always* `config.subnet`
(`mesh.rs:8225`); a `SubnetPolicy` never derives it. The policy applies only to
inbound peer announcements, as the field's own doc at `mesh.rs:7423-7425`
correctly says. The documented `Some`/`None` distinction — policy-derived versus
config-supplied — does not exist as a distinction at all: the local subnet comes
from config in both cases, and `local_subnet_policy()` returning `Some` says
nothing whatsoever about where `local_subnet` came from. An operator tool that
followed the last sentence and surfaced this to explain "why is this node in
subnet X" would print a policy that had no part in the answer.

This is worse than ordinary drift. An operator reading it concludes that
installing a `SubnetPolicy` is how a node acquires its own subnet — and so
reaches for `with_subnet_policy` *without* `with_subnet`, which is exactly the
configuration finding 2 identifies as leaving `ParentVisible` fail-open. The
documentation recommends the hazardous configuration.

**Fix:** state that the policy assigns peers' subnets only, that `local_subnet`
comes from `MeshNodeConfig::subnet` independently, and that `Some`/`None` here
reflects only whether per-peer subnet tracking is enabled. Cross-reference
`with_subnet` as the way to set this node's own subnet.

---

## 7. `peer_subnets` field doc still describes the pre-fix coercion

**Severity:** Low-Medium. Describes the HIGH #2 vulnerability as the contract.
**Location:** `net/crates/net/src/adapter/net/mesh.rs:7426-7429`

```rust
/// Per-peer subnet map. Keys are `node_id`; values are the
/// subnet derived from each peer's most recent announcement via
/// `local_subnet_policy`. Unknown peers default to
/// [`SubnetId::GLOBAL`] at read time.
peer_subnets: Arc<DashMap<u64, SubnetId>>,
```

"Unknown peers default to `SubnetId::GLOBAL` at read time" is a verbatim
statement of the defect HIGH #2 removed. Both surviving read sites
(`mesh.rs:23483`, `:23969`) now preserve absence as `Option<SubnetId>` and hand
`None` to `subnet_visible`, precisely so that unknown is *not* coerced to
`GLOBAL`. The field doc instructs a future reader to reintroduce the bug.

**Fix:** replace the last sentence with the actual contract — absence means the
subnet was not derived, is preserved as `None`, and is resolved by
`subnet_visible` per the fail-closed rules documented there. Worth noting the
population gate (`signature_verified && hop_count == 0` with a policy installed)
since that is what makes absence common rather than transient.

---

## 8. CLI `--tag` doc claims warn-and-drop; the code hard-errors

**Severity:** Low, operator-facing.
**Location:** `net/crates/net/cli/src/commands/cap.rs:91-97` versus `:321-328`

The `--tag` doc comment says:

> Reserved-prefix tags (`causal:` / `fork-of:` / `heat:` / `dataforts:` /
> `scope:`) are dropped by the parser and a warning is logged — they will NOT be
> on the announcement.

The code does not drop them. It validates each tag through `Tag::parse_user`
*before* building the set and returns `invalid_args` on rejection, so
`net cap announce --tag scope:tenant:acme` fails the command outright rather
than emitting an announcement missing that tag.

The doc describes `CapabilitySet::add_tag`'s warn-and-drop behaviour, which this
path never reaches for a rejected tag. Notably this contract was *introduced* by
commit `610bc2f70`, whose stated purpose was correcting stale contracts, and the
audit's follow-up table records the CLI contract as fixed — so the audit
currently vouches for a description that was wrong when written.

Two smaller inconsistencies in the same area: the error message at `:324-326`
omits `dataforts:` from the reserved list the doc comment includes, and the
hard-error behaviour makes the doc's "a dropped `scope:` tag leaves the
announcement globally visible" warning inapplicable to this subcommand (it is
true of the SDK builders, not of `net cap announce`).

**Fix:** state that reserved-prefix tags are rejected with an error here, keep
the pointer to the dedicated scope builders, align the two reserved-prefix
lists, and move the globally-visible caveat to where it applies.

---

## Review of this review

This document was itself reviewed at `3e865a02e`. That pass reproduced the
focused Rust result (191 passed, 0 failed), confirmed the technical conclusions
listed under *Verification performed*, and returned **HOLD** on two accuracy
defects, both now corrected above:

| Defect | Correction |
| ------ | ---------- |
| Finding 2's decision recorded the `warn` and the audit qualifier as done; neither exists at `3e865a02e` | Decision section now carries an explicit **not yet implemented** status with both outstanding items enumerated |
| Finding 5 asserted the new lock-held work was "strictly cheaper" than the removed allocation — a benchmark claim with no benchmark | Replaced with the bounded factual statement; the unsupported comparison is called out as withdrawn |

The same pass found the three contract defects now recorded as findings 6-8,
against the first pass's assertion that drift was confined to findings 3-5.
Finding 6 in particular is the one this review should have caught on its own:
having established that `with_subnet_policy` does not set the local subnet, it
did not check whether the API documentation said otherwise. It does.

---

## Not included

A sixth observation from the review — that the branch makes breaking API changes
(`withTenantScope` / `withRegionScope` now throw, `findNodesScoped` throws on
empty selectors, all three native converters now error where they returned
`Any`) without a version bump or changelog entry — is deliberately excluded
here. It is a release-process question rather than a defect in the code, and the
behaviour changes themselves are correct.
