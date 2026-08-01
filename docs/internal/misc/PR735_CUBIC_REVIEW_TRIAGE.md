# PR #735 (`security-channel`) — cubic review triage

**Source:** all 45 inline review comments on
[ai-2070/net#735](https://github.com/ai-2070/net/pull/735), fetched via
`gh api repos/ai-2070/net/pulls/735/comments --paginate` on 2026-08-01.
Every comment is from `cubic-dev-ai[bot]`; there are no human review
comments on the PR.

**Result:** 43 of 45 are addressed on the current branch and verified
against HEAD, not against the commit each comment was written on. The
remaining two are below: one is a deliberate deferral tied to H1, the
other is factually incorrect.

> Verification method: each claim was checked by reading the code at HEAD
> rather than by trusting the fix commit. Several comments in this set
> were duplicates raised across successive review runs (the registry
> index race appears four times, the redex constructor three, the blob
> `Node` principal three), and several described defects that a *later*
> commit in the same branch had already introduced a fix for — so
> "a commit says it fixed this" was not treated as evidence.

---

## Open 1 — `Unauthorized` is not specific to the reply-origin pin (P2)

**Comment #18**, `net/crates/net/src/adapter/net/mesh_rpc.rs`.

> RPCs rejected for any ordinary ACL reason now trigger
> rate-limit-bypassing capability broadcasts and retries. `Unauthorized`
> is not specific to the reply-origin pin; preserve that cause in the
> membership ACK (or expose a distinct reason) and retry only that
> reason.

**Status: valid, and deliberately not fixed here.**

The observation is correct. `MembershipFailure::warrants_reannounce()`
matches `Rejected(Some(AckReason::Unauthorized))`, and the publisher
returns `Unauthorized` for cap-filter, token, visibility, queue-group
and missing-`TokenCache` rejections as well as the origin pin. So the
corrective-announce path fires for causes a re-announce cannot repair.

**Why the proposed fix cannot land on this branch.** It asks for a new
`AckReason` variant (or a preserved cause) on the membership ACK. An
unknown reason byte is a **hard decode error** on existing peers, not an
ignorable field — so adding a variant breaks every peer that has not
been upgraded. Doing it safely needs the versioned membership cutover
described as open decision **D7** in
[`RECEIVE_SIDE_PUBLISH_AUTHORITY_PLAN.md`](../plans/RECEIVE_SIDE_PUBLISH_AUTHORITY_PLAN.md),
which is under a standing STOP NOTICE: nine decisions (D1–D9) must be
settled before any of it is coded.

**What was done instead.** The amplification is bounded structurally, on
two axes, so the imprecise signal cannot be turned into unbounded work:

| bound | mechanism | commit |
|---|---|---|
| per target | `claim_corrective_announce` — at most ONE corrective announce per target, ever; cleared only on that peer's session failure | R1 |
| node-wide | `CorrectiveAnnounceBudget` — 8 announces per 10 s across all targets, checked *before* the per-target latch is consumed | R17 / `4a5b874bd` |
| wasted claims | a failed send refunds both the latch and the budget, so a dropped datagram does not permanently deny a target its repair | R16 / `4a5b874bd` |

A target refused by the budget keeps its claim and retries on a later
call, so a burst cannot permanently deny corrective announces to
whichever targets happened to arrive during it.

**Residual risk.** A peer refusing for an unrelated ACL reason still
costs one mesh-wide broadcast, once, until its session fails. That is
bounded and small, but it is not zero, and it is not what the reviewer
asked for.

**To close properly:** settle D1–D9, take the versioned membership
cutover, add a distinct `AckReason` for the origin-pin rejection, and
narrow `warrants_reannounce()` to it. At that point both bounds above
become belt-and-braces rather than the primary defence.

---

## Open 2 — "RPC serving never invokes `install_rpc_service_defaults`" (P1)

**Comment #36**, `net/crates/net/src/adapter/net/channel/config.rs:827`.

> RPC serving never invokes this helper, so its reply-prefix origin
> binding is never installed and reply subscriptions remain governed by
> the unregistered-channel policy. Wire this shared registration into
> every serving entry point after service-name validation.

**Status: incorrect. No change made.**

The delegation chain is intact and every hop is asserted by a test.

```
serve_rpc*  (8 variants, sdk/src/mesh_rpc.rs:270,367,438,498,574,616,656,694)
  └─> Mesh::auto_register_rpc_channels
        └─> Mesh::register_rpc_service_channels        (sdk/src/mesh.rs)
              └─> ChannelConfigRegistry::install_rpc_service_defaults
```

`Mesh::register_rpc_service_channels`'s entire body is
`self.channel_configs.install_rpc_service_defaults(service)`.

Verified at HEAD:

- `every_serve_path_delegates_to_the_shared_registration`
  (`sdk/src/mesh_rpc.rs`) asserts each hop, for `serve_rpc*`, the
  aggregator and the org path, and that no path registers directly.
- `every_serve_rpc_variant_calls_auto_register` enumerates all 8
  `serve_rpc*` variants deliberately, so a new variant that skips
  registration fails rather than being discovered.
- `rpc_service_defaults_are_install_if_absent_and_origin_bound`
  (`channel/config.rs`) asserts the installed prefix behaviourally —
  `resolve_by_name("svc.replies.<16 hex>")` carries
  `OriginBinding::OriginHashHex16`.
- `both_aggregator_services_bind_their_reply_prefix`
  (`sdk/src/aggregator.rs`) asserts the same through the aggregator's
  own entry points on its real service names.

The comment was likely produced against a revision where the policy had
just moved onto the registry (R9) and the `Mesh` hop read as an
unreferenced new helper.

---

## Appendix — disposition of all 45

Ordered by creation time. "Fix" names the task ID used in this branch's
commit messages; where a comment was closed by work outside that
sequence, the commit is given.

| # | Sev | File | Finding | Disposition |
|---|---|---|---|---|
| 1 | P1 | mesh_rpc.rs | corrective re-announce on every rejection | Fixed — R1 |
| 2 | P1 | mesh_rpc.rs | reply cache survives peer failure | Fixed — R2 |
| 3 | P1 | mesh.rs | aggregator reply prefix unbound | Fixed — R3, R9; covered `a6ba68258` |
| 4 | P2 | config.rs | remove/re-register strands the index | Fixed — R4, superseded by R7 |
| 5 | P3 | sdk/mesh.rs | prefix token-gate doc inverted | Fixed — `a664627` |
| 6 | P3 | config.rs | prefix matching duplicated | Fixed — `longest_matching_prefix` |
| 7 | P0 | config.rs | queue-group policy bypassed on open channels | Fixed — `283d77e76` |
| 8 | P1 | mesh.rs | TokenBound workers accepted then starved | Fixed — `8d20c8b4d` |
| 9 | P3 | name.rs | `queue_group_hash` duplicates `channel_hash` | Fixed — R15 |
| 10 | P3 | audit doc | lead-in contradicts the I1 table row | Fixed — `a8d59b0b1` |
| 11 | P3 | mesh.rs | recovery summary omits unsubscribe | Fixed — R19 |
| 12 | P0 | blob/mesh.rs | `Node` grant authorizes blob mutation | Fixed — `d8fb743d8` |
| 13 | P0 | admission.rs | restrict helper to `Origin` | Fixed — `d8fb743d8` |
| 14 | P1 | manager.rs | `Redex` storage accepts `Node` | Fixed — `with_auth(origin_hash)` |
| 15 | P3 | channel_auth_hardening.rs | test helper named `origin_hash` | Fixed — `ae68adf98` |
| 16 | P3 | mesh.rs | `subscriber_principal` doc heading stale | Fixed — `627b8e304` |
| 17 | P1 | admission.rs | reject non-`Origin` principals | Fixed — `d8fb743d8` |
| 18 | P2 | mesh_rpc.rs | `Unauthorized` not pin-specific | **Open — deferred, see above** |
| 19 | P2 | mesh.rs | scan test matched its own literals | Fixed — replaced with a behavioural test |
| 20 | P2 | manager.rs | public ctor accepts `Node` | Fixed — same as 14 |
| 21 | P2 | config.rs | phantom reverse-index entry | Fixed — R7 |
| 22 | P3 | channel_auth_hardening.rs | helper + `b_origin` locals | Fixed — `ae68adf98`; no `b_origin` remains |
| 23 | P3 | sdk/mesh_rpc.rs | `channel_configs_arc` dead | Fixed — `ae68adf98` |
| 24 | P1 | mesh_rpc.rs | latch eats retry budget | Fixed — R5 |
| 25 | P1 | mesh.rs | in-flight subscribe repopulates cache | Fixed — R6, R11 |
| 26 | P2 | config.rs | serialize map + index updates | Fixed — R7 |
| 27 | P2 | manager.rs | `with_auth_principal` too visible | Fixed — R8 |
| 28 | P2 | sdk/mesh.rs | org registration is a separate copy | Fixed — R9 |
| 29 | P3 | admission.rs | error leaks `Node(<raw id>)` | Fixed — R10 |
| 30 | P2 | config.rs | stale reverse-index entry | Fixed — R7 |
| 31 | P2 | manager.rs | make `with_auth_principal` private | Fixed — R8 |
| 32 | P2 | mesh.rs | aggregate announce amplification | Fixed — R17 |
| 33 | P2 | mesh_rpc.rs | only the latch winner retries | Fixed — R5 |
| 34 | P3 | admission.rs | raw node id in `Unauthorized` | Fixed — R10 |
| 35 | P1 | config.rs | near-limit name installs half a policy | Fixed — R12 |
| 36 | P1 | config.rs | serving never invokes the helper | **Incorrect — see above** |
| 37 | P1 | mesh_rpc.rs | retain completes before the gen bump | Fixed — R11 |
| 38 | P2 | mesh.rs | generation map grows without bound | Fixed — R11 |
| 39 | P2 | mesh.rs | lost datagram spends the latch | Fixed — R16 |
| 40 | P3 | org/serve.rs | doc link out of scope | Fixed — R19 |
| 41 | P3 | mesh_rpc.rs | rollback deletes a newer entry | Fixed — R18 |
| 42 | P2 | config.rs | sentinel is shorter than a real reply name | Fixed — R12 |
| 43 | P2 | mesh_rpc.rs | gen snapshot spans the retry loop | Fixed — R12 |
| 44 | P3 | blob/mesh.rs | docs still say `origin_hash` | Fixed — R12 |
| 45 | P3 | sdk/mesh_rpc.rs | aggregator absent from delegation scan | Fixed — `a6ba68258` |

### Notes on the set

- **Twelve of the 45 are duplicates** of five underlying defects, raised
  again on later review runs because the fix had not landed when the run
  started: the registry index race (4, 21, 26, 30), the redex
  constructor (14, 20, 27, 31), the blob `Node` principal (12, 13, 17),
  and the error-message leak (29, 34).
- **Four comments describe defects introduced by fixes earlier in this
  same branch** — 35/42 (R9's name validation), 37 (R6's eviction
  ordering), 41 (R6's rollback), 43 (R12's snapshot). Each is recorded
  in the fixing commit rather than folded in silently.
- **Two review claims that a fix "was addressed" were wrong in the
  strict direction** and needed further work: the R6 fence (#25) was
  marked addressed but #37 later found the ordering was backwards, and
  R9's validation (#35) was marked addressed but #42 found it checked
  the wrong-length name.

## Still outstanding on this branch, independent of this review

- **H1** — receive-side publish authority. Open finding, under a STOP
  NOTICE with nine unsettled decisions. Not implementable from the
  current constraint set.
- **Integration suites have not been swept.** Only the `--lib` suites
  and `channel_auth*` have run (net-mesh 5406, SDK 238). The full
  `--tests` run needs `-j 2` on this machine — at higher parallelism it
  exhausts the paging file and rustc reports the resulting unmappable
  rlib as a cascade of ICEs.
