# CODE REVIEW 2026-10-17 — cross-org branch (`LZL0/cross-org`)

**Scope:** the branch diff `master...7afac4def`. 7 commits, 139 files,
+9149/−413. Covers grant revocation, the cross-org revocation relay and
opt-in freshness (`require_fresh_org_revocations`), the `start()` refusal,
the CLI cross-org journey, the cross-process org matrix, and the browser
leaf's revocation merge.

**Method:** one review pass over the diff. Findings 3, 9 and 10 were
re-read in source at `7afac4def` and hold (CONFIRMED). The rest are
TRACED: found in the review pass and not independently re-read.

**Receipts:** none. No `cargo`, nextest, wasm or CI-script run backed this
pass. No finding below has a test that covers it.

**Verdict: HOLD** at review time; **all ten findings are now fixed** (see
Resolution at the end). Fix CO-1 to CO-3 before opening a PR. All three sit on
the relay path the branch adds. On the receiving side relayed bundles can
stall a service or be dropped for good. On the sending side the resend
timer can be set when nothing was sent.

| ID | Severity | Area | File | Status |
|---|---|---|---|---|
| CO-1 | Must-fix | Relay ingest | `src/adapter/net/mesh_rpc.rs:1678`, `:1995` | TRACED |
| CO-2 | Must-fix | Relay ingest | `src/adapter/net/mesh_rpc.rs:1195` | TRACED |
| CO-3 | Must-fix | Relay send | `src/adapter/net/mesh_rpc.rs:8968` | CONFIRMED |
| CO-4 | Should-fix | Relay send | `src/adapter/net/mesh_rpc.rs:8851` | TRACED |
| CO-5 | Should-fix | Foreign-org cap | `src/adapter/net/mesh.rs:22327` | TRACED |
| CO-6 | Should-fix | Relay persistence | `src/adapter/net/behavior/org_revocation.rs:2347` | TRACED |
| CO-7 | Should-fix | CLI startup | `cli/src/commands/lifecycle.rs:3655` | TRACED |
| CO-8 | Should-fix | Leaf parity | `leaf/src/org/revocation.rs:76` | TRACED |
| CO-9 | Cleanup | Relay send (perf) | `src/adapter/net/mesh_rpc.rs:8951` | CONFIRMED |
| CO-10 | Cleanup | Logging | `src/adapter/net/behavior/org_revocation.rs:900` | CONFIRMED |

Paths are relative to `net/crates/net/`.

---

## Must-fix

### CO-1 — relayed revocations are verified inline in the per-service bridge loop (TRACED)

`src/adapter/net/mesh_rpc.rs:1678` (unary). The streaming twin is at `:1995`.

The `serve_rpc` bridge handles one frame at a time
(`admit_and_dispatch_protected(...).await`, around line 5303). Ingesting a
`net-org-revocations` header waits on a blocking-pool task that does
ed25519 verification and an fsync'd persist, all inside that loop. The
ingest also runs before the registry reservation, which §3 requires to
come before any signature work.

**Scenario:** a peer sends protected REQUESTs, each with a
`net-org-revocations` header carrying up to 16 bundles. The bundles are new
by digest but either fail to verify or verify and need a disk persist.
Every frame on that service waits behind the verify and fsync, including
other callers' requests and stream openings. Because ingest runs before
reserve, duplicate or replayed `(caller, call_id)` openings still cost
signature checks and blocking-pool trips.

**Fix direction:** reserve first, then move relay ingest off the bridge
loop (a bounded background queue or a separate task). The call does not
need to wait for it, unless freshness is required and the relayed bundle
is the only fresh material.

### CO-2 — a bundle refused for local-state reasons stays marked as seen (TRACED)

`src/adapter/net/mesh_rpc.rs:1195`.

Only `Store` errors take the digest back out of the seen set. `NotAccepted`,
`NoAuthority` and `OverCap` depend on this node's configuration at that
moment, and those digests stay marked. `first_sighting` then skips every
later relay of that bundle until the 4096-entry seen set overflows.

**Scenario:** org A's caller relays a heartbeat before provider B has
installed its grant for A, before B has an authority, or before
`require_fresh_org_revocations(A)` is set. The result is `NotAccepted` and
the digest stays in `seen`. Once B is configured, every later relay of that
bundle is ignored. If B requires fresh material from A, every A caller is
refused with `RevocationsStale` until A issues a new bundle, even though B
already received a valid one.

**Fix direction:** keep only verification failures and successful applies
in the seen set. Forget the digest on any refusal that depends on local
state.

### CO-3 — `relay_due` marks the set as sent before anything is sent (CONFIRMED)

`src/adapter/net/mesh_rpc.rs:8968` (`attach_relayed_org_revocations`).

`relay_due(&intent.provider, digest)` records `(digest, now)` for the
provider when it returns true. The header is only attached after that. If
the header is later dropped, or the call never goes out, the relay is held
back for `RELAY_RESEND_INTERVAL` (5 minutes). The code comment says "the
next due call retries", but it won't.

**Scenario:** `attach_signed_admission` adds the relay header, finds that
the request plus proof is over `MAX_PAYLOAD_SIZE`, and removes the header
(around line 8856). Or the protected call fails to send or times out.
Either way `sent[provider]` already holds the digest, so cross-org calls to
that provider carry no relay for the next 300 seconds. A provider with a
short freshness max-age keeps refusing that org's callers with
`RevocationsStale` for that whole window.

**Fix direction:** split `relay_due` into a read-only check and a separate
`mark_relayed`. Call `mark_relayed` only after the header survives
finalization and the frame is sent, or roll it back on the drop and failure
paths.

---

## Should-fix

### CO-4 — the relay header can push a call past `MAX_RPC_HEADERS` (TRACED)

`src/adapter/net/mesh_rpc.rs:8851`.

The relay header takes one of the 32 `MAX_RPC_HEADERS` slots. The fallback
that drops it only checks byte size, not header count.

**Scenario:** the app sends a cross-org protected call with 31 headers while
a relay is due. `attach_relayed_org_revocations` pushes header 32.
`sign_admission_proof` passes (32 ≤ 32). The admission header is pushed
(33), and `req.validate_wire_bounds()` fails with "finalized request
exceeds wire bounds". On `master` this call succeeds. Whether it fails
depends on timing, because it only happens when a relay is due.

**Fix direction:** skip the relay when `headers.len() + 2 > MAX_RPC_HEADERS`,
or make the drop fallback also cover header count.

### CO-5 — the foreign-org cap counts repeated entries as new, and checks outside the lock (TRACED)

`src/adapter/net/mesh.rs:22327`.

The cap check adds the whole bundle's entry count to the org's existing
count, even when those entries are already stored. It also reads a
snapshot outside the apply lock, so the check and the apply can race.

**Scenario:** foreign org A already has 3,000 floors at provider B. A
re-issues, or relays again, a cumulative bundle naming the same 3,000
members with raised floors. 3000 + 3000 > 4096 gives `OverCap`. The
legitimate raise is refused and A's revoked members stay admitted.
Separately, two concurrent applies of different bundles can each pass
against the same snapshot and together push A past
`MAX_FOREIGN_ORG_ENTRIES`.

**Fix direction:** count only keys not already present, and run the check
under the same lock as the apply.

### CO-6 — the relay sidecar write can tear or land out of order (TRACED)

`src/adapter/net/behavior/org_revocation.rs:2347` (`retain_for_relay`).

The relay cache is serialized under the mutex, but the sidecar file is
written after the mutex is released. Every writer uses the same fixed
`.tmp` path. Another process's core sharing the directory overwrites the
whole file.

**Scenario:** two applies on different threads (for example a relayed
ingest and the floor service) each take sidecar bytes. The thread with the
older snapshot can write and rename last. Or both `std::fs::write` the same
tmp file at once and the rename publishes torn JSON. On restart
`RetainedMaterial::from_file_bytes` loads an empty or stale cache and
`latest_issued_at` returns `None`. A provider that requires freshness then
refuses every caller of that org with `RevocationsStale` until new material
arrives.

**Fix direction:** write while holding the lock (or a dedicated write lock)
using a unique temp file name, and skip a write whose generation is older
than the last one written.

### CO-7 — `--require-fresh-revocations` is applied after the node starts serving (TRACED)

`cli/src/commands/lifecycle.rs:3655`.

The policy is set after `mesh.start()`, the org floor service, and the
earlier serving steps. Until then the node accepts protected calls without
the freshness requirement, so startup fails open.

**Scenario:** `net-mesh up --require-fresh-revocations <A>=1h` restarts.
Between start and the policy loop, a caller from org A, which has published
nothing recent, is admitted (and counted as an unverified admission)
instead of refused with `RevocationsStale`. That is the case the flag
exists to refuse.

**Fix direction:** set the freshness policy before `start()`.

### CO-8 — the leaf's `merge_grant_revocations` does not clamp `not_after` (TRACED)

`leaf/src/org/revocation.rs:76`.

The leaf function says it mirrors core, but it leaves out core's clamp of
`not_after` to now + `MAX_ORG_GRANT_TTL_SECS` + skew.

**Scenario:** a signed bundle, from any accepted org or a buggy issuer,
names entries with `not_after = u64::MAX`. Core clamps them and drops them
within about 30 days. The browser leaf keeps them forever:
`revocation_entry_expired` never fires, `revoked_grants` grows across
bundles without limit, and the two implementations disagree on how long an
entry lives.

**Fix direction:** apply the same clamp in the leaf, and add a parity test
with an oversized `not_after`.

---

## Cleanup

### CO-9 — every cross-org call encodes and hashes the relay set before the `relay_due` check (CONFIRMED)

`src/adapter/net/mesh_rpc.rs:8951`.

`attach_relayed_org_revocations` clones all of the org's retained material
(up to 64 KiB), encodes the header, and blake3-hashes every bundle
(`relay_set_digest`), and only then calls `relay_due`. On the provider
side, every protected request also gets a full byte-window scan for the
header name.

**Scenario:** in steady state `relay_due` returns false for 5 minutes at a
time. Every call in that window still copies tens of KB under the relay
mutex and hashes every bundle, then throws the result away.

**Fix direction:** cache the set digest in `RetainedMaterial`, check
`relay_due` first, and encode only when a relay is due.

### CO-10 — the grant-regression log message lost its line continuations (CONFIRMED)

`src/adapter/net/behavior/org_revocation.rs:900`.

The `\` line continuations were removed from the existing "the persisted
state is BEHIND the enforced view … grant revocation(s)" error string. The
logged message now has long runs of spaces in it. Its floor twin at line
866 still has them.

**Scenario:** when the grant-regression path fires, the operator sees
"enforced view                  for N grant revocation(s); the live view is
                 preserved…". That breaks log grepping and alert matching
keyed on the floor message's wording. This matches the known problem where
Bash heredocs collapse backslashes.

**Fix direction:** restore the `\` continuations so the string matches the
floor message's format.

---

## Resolution (2026-10-17)

Every finding is fixed on `LZL0/cross-org`:

| ID | Commit | Test |
|---|---|---|
| CO-1 | `7bc350456` | `background_ingest_is_bounded_and_a_bad_bundle_stays_seen`, `a_bundle_refused_for_this_nodes_state_is_tried_again` (inline path), SDK `a_freshness_requirement_refuses_until_the_callers_org_proves_itself_current` |
| CO-2 | `7bc350456` | `a_bundle_refused_for_this_nodes_state_is_tried_again` |
| CO-3 | `7bc350456` | `a_relay_is_marked_sent_only_once_its_request_is_out` |
| CO-4 | `7bc350456` | `a_relay_never_takes_the_last_header_slot` |
| CO-5 | `c9bc65b00` | `a_capped_apply_counts_only_the_floors_it_would_add` |
| CO-6 | `c9bc65b00` | `the_relay_sidecar_keeps_what_another_process_retained` |
| CO-7 | `0dc11cb6d` | none (startup ordering; a timing test would be flaky) |
| CO-8 | `7a2b02950` | leaf `an_oversized_not_after_is_clamped_like_core` |
| CO-9 | `7bc350456` | covered by the CO-3 test (the due check now runs first) |
| CO-10 | `b69c9e56a` | none (log text) |

**Behavior change from CO-1:** relayed material that the relaying call's
own admission does not need (no freshness policy names the signer, or the
held material is already fresh) is now applied in the background, shortly
after the call, not before it is admitted. The SDK live relay test waits
for the merge accordingly. Material a freshness policy needs is still
applied before admission, at most two bundles per request.

**Not addressed:** CO-1's note that relay ingest runs before the §3
registry reservation. Ingest is now a decode and digest check on the
bridge path, plus at most two inline applies for a stale freshness-policy
org, and a replayed opening hits the seen set. Moving it after the
reservation would mean restructuring `admit_protected_opening`.

**Receipts:** full unit suite under `UNIT_FEATURES` (6101 passed),
`net-mesh-sdk` lib (349 passed), `net-cli` `cross_org_protected_workflow`
and `org_grant` (21 passed), the leaf `org::revocation` test, `cargo
clippy` (`--all-features --lib --bins -D warnings`; `--all-features
--all-targets` with CI's `-A` flags; `-p net-mesh-sdk`; `-p net-cli`),
root `cargo doc --all-features -D warnings`, and fmt (workspace and leaf).
Not run locally: Go, Python, Node, Windows-only and Linux-only jobs.
