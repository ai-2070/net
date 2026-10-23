# CODE REVIEW 2026-10-23 — aggregators across hosts (`LZL0/aggregators`)

**Scope:** the branch diff `master...91664d6dd` (merge-base `541cc0078`).
12 commits, 38 files, +5462/−91. Seven commits are plan docs
(`AGGREGATOR_MULTI_HOST_PLAN.md` and its revisions, Phase 0). The code
commits are R1 (capability relay through a real daemon hub) and Q0–Q4:
`[[subnet_rule]]` subnet policy from TOML, one source subnet per daemon with
summary visibility enforced, `fold.query.v2` in core, the daemon serving
`fold.query` v1 and v2, and `fold.query.v2` in the Rust SDK, Node, Python,
Go and C.

**Method:** one review pass over the diff. No finding was re-read in source
on its own afterwards, so all are TRACED: found in the review pass and not
independently confirmed.

**Receipts:** none. No `cargo`, nextest, Go, Node or Python run backed this
pass. No finding below has a test that covers it.

**Verdict: HOLD.** Fix AG-1, AG-3 and AG-4 before opening a PR. AG-1 can
silently stop a summary update from being published. AG-3 and AG-4 give
operators and callers a wrong signal, or none at all, about why summaries
and queries are refused.

| ID | Severity | Area | File | Status |
|---|---|---|---|---|
| AG-1 | Must-fix | Query → publish | `src/adapter/net/behavior/aggregator/query_v2.rs:230` | TRACED |
| AG-2 | Should-fix | v1 fallback contract | `src/adapter/net/behavior/aggregator/query_v2.rs:410` | TRACED |
| AG-3 | Must-fix | Daemon config | `aggregator-daemon/src/scope.rs:46` | TRACED |
| AG-4 | Must-fix | v1 error mapping | `src/adapter/net/behavior/aggregator/query_v2.rs:305` | TRACED |
| AG-5 | Should-fix | Daemon config | `aggregator-daemon/src/scope.rs:46` | TRACED |
| AG-6 | Cleanup | Query path (perf) | `src/adapter/net/behavior/aggregator/query_v2.rs:224` | TRACED |
| AG-7 | Cleanup | Duplication | `src/adapter/net/behavior/aggregator/query_v2.rs:367` | TRACED |
| AG-8 | Cleanup | Binding parity | `src/ffi/aggregator.rs:865` | TRACED |
| AG-9 | Cleanup | Daemon boot | `aggregator-daemon/src/lib.rs:922` | TRACED |
| AG-10 | Should-fix | Query errors | `src/adapter/net/behavior/aggregator/query_v2.rs:227` | TRACED |

Paths are relative to `net/crates/net/`.

---

## Must-fix

### AG-1 — a query can stop a summary from being published (TRACED)

`src/adapter/net/behavior/aggregator/query_v2.rs:230`.

The daemon now serves SummarizeNow (v1 and v2) to any caller inside the
summary visibility, not only to operators. `answer` runs `tick_once`, which
adds the new summaries to the `latest` buffer without publishing them. The
next background `tick_and_publish` then finds nothing new in `filter_novel`
and publishes nothing.

**Scenario:** a fold changes. Before the next interval tick, a peer in the
subnet (not an operator) calls `query_group_summarize_now`. That tick's
summary goes into `latest` but is never published on `summary/0x0001`, so
subscribers miss the update until the fold changes again. Each call also
bumps the per-tick `generation`. Before this branch the daemon served no
`fold.query` at all, so this path is new.

Repeated calls can also force ticks on every group with no limit, because
`group: None` ticks the first replica of each group.

**Fix direction:** make SummarizeNow leave the publish dedupe state alone
(compute and return the summary without writing `latest`), or have it
publish what it ticked. Restrict it to operators or rate-limit it, so a
peer cannot drive unbounded ticks.

### AG-3 — subnet rules on a global daemon silently cut off peers (TRACED)

`aggregator-daemon/src/scope.rs:46`.

A global daemon (`subnet` absent) with `[[subnet_rule]]`s is accepted, and
the default `parent-visible` applies. With a GLOBAL source, `subnet_visible`
admits only destinations that are GLOBAL themselves or unrecorded. Every
peer the new rules classify is dropped from summary fan-out and refused by
the query access check.

**Scenario:** an operator adds Q0's `[[subnet_rule]] region:` rules to an
existing global daemon, for example to classify the hub's peers. Summaries
used to reach every peer, because no channel registry was installed before.
Now the leaves classified into `region:eu` (subnet `[1]`) stop receiving
`summary/*` publishes and get NotAuthorized from `fold.query.v2`. There is
no boot error and no warning.

**Fix direction:** refuse this combination at boot, or warn loudly, or
treat a global source as visible to every subnet.

### AG-4 — v1 authorization refusals show up as transport errors (TRACED)

`src/adapter/net/behavior/aggregator/query_v2.rs:305`.

The daemon-level v1 handler refuses with the application status `0x8001`,
but the v1 client maps every `RpcError` to `FoldQueryClientError::Transport`.
Every v1 binding therefore reports an authorization refusal as "transport".

**Scenario:** a peer outside the visibility calls `query_latest` (v1)
through Go, Node, Python or C and gets kind `transport` /
`NET_REGISTRY_ERR_TRANSPORT`, the same as a network timeout. Callers retry
or report a connectivity problem instead of "unauthorized". The new
`FOLD_QUERY_V1_NOT_AUTHORIZED` constant is not matched anywhere on the
client side.

**Fix direction:** match status `0x8001` in the v1 client, map it to a
not-authorized variant, and carry that kind through the FFI, Node, Python
and Go error mappings.

---

## Should-fix

### AG-2 — the "server too old, fall back to v1" advice rarely applies (TRACED)

`src/adapter/net/behavior/aggregator/query_v2.rs:410`.

`ServerTooOld` fires only when the peer answers NotFound. The FFI, `net.h`,
Go, Node and Python surfaces all document "fall back to v1" for it. The
cases that fallback is meant for don't answer that way:

- Hosts that serve only v1 through the SDK's `install_fold_query_service`
  always install a channel registry. The v2 reply-channel subscribe is
  refused and the call fails as TRANSPORT. `examples/c/aggregator.c` says
  so in its own comment.
- Nodes older than 0.39 time out instead of answering NotFound.
- Daemons from before Q3 do answer NotFound, but they serve no v1
  `fold.query` either, so falling back to v1 also fails.

**Scenario:** a client written to retry v1 on `SERVER_TOO_OLD` never falls
back where it should, or falls back to a service that does not exist.

**Fix direction:** either make the documented trigger match reality (for
example "on SERVER_TOO_OLD or TRANSPORT, try v1 once"), or drop the
fallback advice and state what each kind of old peer actually returns.

### AG-5 — no boot check that the rules can ever match the daemon's own subnet (TRACED)

`aggregator-daemon/src/scope.rs:46`.

For a non-global `subnet`, boot checks only that some rules exist. It never
checks that the rules can assign the daemon's own subnet, or a descendant or
ancestor that the visibility would admit.

**Scenario:** `subnet = "3.7"`, rules map only `region:eu`→1 and
`region:us`→2, `summary_visibility = "subnet-local"`. The daemon boots
cleanly, but no peer can ever be classified into 3.7. Every summary publish
reaches nobody and every non-operator query is refused. The only signal is
a warn log on each query.

**Fix direction:** at boot, check that at least one rule produces a subnet
the configured visibility admits from the daemon's subnet. Refuse or warn
if none does.

### AG-10 — a group with no replicas reports UnknownKind (TRACED)

`src/adapter/net/behavior/aggregator/query_v2.rs:227`.

When `request.group` names an existing group that has no replicas (taken by
an in-flight unregister), the group is skipped and the client gets
`UnknownKind`, even though the kind is configured for that group.

**Scenario:** a client queries `group: Some("alpha")` for kind 1 while
`alpha` is being unregistered or rebuilt, and receives
`NET_REGISTRY_ERR_UNKNOWN_KIND`. It may then treat kind 1 as permanently
unsupported.

**Fix direction:** return UnknownGroup, or an empty record list, for a
named group with no replicas.

---

## Cleanup

### AG-6 — every query polls the health of every replica (TRACED)

`src/adapter/net/behavior/aggregator/query_v2.rs:224`, and
`aggregator-daemon/src/scope.rs:152` (`occupied_kind`).

Both call `entry.snapshot()` for every group on every query. That runs a
concurrent `health()` poll on every replica and clones the placements, only
to read `replicas.first()`. It sits on a request path any peer in the
visibility can reach, including the server side of the LatestSummary path.

**Fix direction:** use `entry.replicas().await` (already on
`AggregatorGroupEntry`), which returns the same `Arc`s under one brief lock.
`registry.entries()` would also avoid the `names()` + `get()` double lookup.

### AG-7 — response encoding is duplicated (TRACED)

`src/adapter/net/behavior/aggregator/query_v2.rs:367`.

`encode` re-implements `query_service::encode_response` (postcard encode,
warn, empty-body fallback, Ok status) and only differs by being generic.
The two copies can drift, and already have: one log message says "replying
with empty body" and the other does not.

**Fix direction:** make `encode_response` generic over `T: Serialize` in
`query_service` and call it from both handlers.

### AG-8 — the v2 error mapping is hand-written three times (TRACED)

`src/ffi/aggregator.rs:865` (`classify_fold_query_v2`), plus
`fold_query_v2_err` in `bindings/node/src/aggregator.rs` and
`bindings/python/src/aggregator.rs`.

The `FoldQueryV2ClientError` → (kind, detail) mapping exists in three places
and the detail strings already differ: the FFI says `server decode: …` and
`unknown fold kind: 0x0001`, while Node and Python say `server-decode: …`
and `0x0001`. The next v2 error variant has to be added three times.

**Fix direction:** put one `kind()` / `detail()` on `FoldQueryV2ClientError`
in core (or the SDK) and have all three bindings call it.

### AG-9 — boot builds a throwaway daemon per fold kind (TRACED)

`aggregator-daemon/src/lib.rs:922`.

`register_summary_channels` builds an `AggregatorDaemon` for each fold kind,
resolving summarizers and a full config, only to reuse its channel-name and
registry-insert logic. It depends on `AggregatorDaemon::new` having no side
effects, and needs a `registered == 0` "no registry" check as a result.

**Fix direction:** a free `summary_channel_name(kind)` plus a direct
`channel_configs().insert(ChannelConfig::new(..).with_visibility(..))`,
so one function owns the channel-name format.
