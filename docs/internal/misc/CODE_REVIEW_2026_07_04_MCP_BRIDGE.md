# Code review — `mcp-3` branch (net-mesh-mcp bridge)

**Date:** 2026-07-04
**Branch:** `mcp-3`
**Base:** `master`
**Scope:** 41 files, +9,873 LOC, 59 commits ahead.
**Plans:** [`MCP_BRIDGE_PLAN.md`](../plans/MCP_BRIDGE_PLAN.md),
[`MCP_CREDENTIAL_FORWARDING_PLAN.md`](../plans/MCP_CREDENTIAL_FORWARDING_PLAN.md)

The branch adds a new `net-mesh-mcp` adapter crate that bridges Model Context
Protocol (MCP) stdio servers over the Net mesh in both directions:

- **wrap / supply side** (`wrap/*`, `net wrap`) — discover a local stdio MCP
  server's tools, classify their credential status, lower them to owner-scoped
  mesh capabilities, and serve nRPC `describe` + per-tool `invoke` handlers;
- **serve / demand side** (`serve/*`, `net mcp serve`) — expose the mesh's
  bridged capabilities to a local MCP host as `net_*` meta-tools plus
  promoted first-class pinned tools, gated by a persistent consent/pin store;
- **Phase 4 collapse/failover** — collapse provably-interchangeable providers
  into one logical capability and fail invoke/describe over between them.

Plus two `net` CLI commands (`net wrap`, `net mcp serve|pin`) and a 2-line SDK
addition exposing a node's own `origin_hash()`.

> **Status — RESOLVED (2026-07-04).** All 13 findings below were fixed on
> `mcp-3`, each as its own commit with tests; two follow-up review comments
> (R1, R2) and a rustdoc-link fix landed on top. The file/line anchors in each
> finding point at the code **as reviewed** (pre-fix) and are left intact as the
> record; the **Resolution** table at the end gives the commit and what actually
> shipped for each (some fixes differ from the suggested direction — e.g. F2
> shipped as a fail-safe opt-in, not the deferred root-identity verification).
> Branch HEAD passes tests, clippy, `fmt --check`, and `cargo doc -D warnings`.

---

## Overall assessment

The **security core is genuinely well-hardened** and most of the branch's
`fix(mcp)` commits are real hardening that re-establishes its invariants.
Verified as correct (not findings):

- **Consent is fail-closed.** An empty policy gates *every* discovered
  capability; a wire-declared `credential_status` — including `none` — is never
  trusted across the demand-side trust boundary (`serve/consent.rs:92`,
  `CredentialStatus::from_wire`). The gate reloads the pin store per invoke, so
  an out-of-band `net mcp pin approve` takes effect immediately without a TOCTOU
  window that trusts a stale snapshot.
- **Owner-scope gates both surfaces.** Invoke (`wrap/invoke.rs:155`) *and*
  describe (`wrap/catalog.rs:136`) reject on the AEAD-verified `caller_origin`,
  served with the same `config.scope` (`wrap/session.rs:305`), so describe is
  visibility-equivalent to invoke.
- **The collapse fingerprint folds the gating fields.** `descriptor_fingerprint`
  (`serve/grouping.rs:102`) hashes `substitutability` + `credential_status`
  alongside the schema, so a credentialed / provider-local primary can never
  fingerprint-match a collapsible candidate — closing the cross-class failover
  hole (commit `dab94971c`).
- **Pin-store writes take a cross-process lock** on a sidecar `.lock`
  (`serve/pins.rs:222`), and load is fail-closed on a parse error.
- **The stdio oneshot reply plumbing is leak/hang-safe**: the pending slot is
  inserted before the write, reclaimed if the write fails, EOF/non-integer ids
  clear the map (awaiters get a transport error, not a hang), and closure is
  signalled with `send_replace` so a late `closed()` observes it.

The findings below are therefore mostly **correctness / robustness / altitude**
rather than doctrine breaks. The two that matter most in normal operation are
**F1** (duplicate execution of credentialed tools via same-node retry) and
**F4/F5** (the serial-shim hang and the 5-second call ceiling). **F2** is a
genuine trust gap that only bites on multi-identity meshes.

Method: 7 independent finder angles (line-by-line, removed-behavior,
cross-file, Rust/async pitfalls, wrapper/security, cleanup, altitude) over the
`master...HEAD` diff, then each surviving candidate verified by reading the
implicated code path directly.

---

## Findings

### F1 (High) — Same-node `call_retry` re-executes non-idempotent credentialed tools

`serve/mesh_gateway.rs:243` (`invoke_on` → `call_retry`, line 116;
`is_retriable`, line 404).

Every `invoke` routes through `call_retry`, which retries on
`Timeout | Transport | NoRoute` up to `MAX_ATTEMPTS = 4` times. The
at-least-once design note (`mesh_gateway.rs:357`) explicitly *names* the
same-node retry as able to run a tool more than once — but its safety argument
("bounded to uncredentialed, operator-declared-interchangeable tools") only
covers **failover** (`equivalent_providers`, which is gated to collapsible =
`provider_equivalent` + `none`). `call_retry` has **no such restriction**: it
runs for every invoke, including credentialed, provider-local tools that never
collapse and never fail over.

**Failure scenario:** a credentialed non-idempotent tool (`github.create_issue`,
a payment) is invoked. The provider executes it, but the first reply is lost to
the nRPC reply-channel first-reply race (a known race for ultra-fast handlers —
the very thing the retry exists to cover), or the call simply runs longer than
`CALL_TIMEOUT = 5s`. `is_retriable(Timeout)` is true, so `call_retry` re-sends
to the **same node** up to 4 times → a second (third, fourth) issue / charge.
Fires in ordinary single-owner operation; no attacker required.

**Fix direction:** distinguish idempotent from non-idempotent calls — retry
`describe` (a pure read) freely, but do not blind-retry a credentialed `invoke`
on timeout; or carry an idempotency key the provider dedups on.

### F2 (High, multi-tenant only) — Failover/collapse trusts wire-declared equivalence without verifying provider identity

`serve/mesh_gateway.rs:387` (invoke failover loop) + `serve/grouping.rs:79`
(`is_collapsible`).

`is_collapsible` reads `substitutability` and `credential_status` verbatim off
the wire, and `equivalent_providers` accepts any peer whose
`descriptor_fingerprint` matches the primary's. The fingerprint is computed over
`tool_id` + `compat_tier` + input/output schema + the two gating fields — **all
of which a hostile peer can forge**, since the schema is public and the peer
controls its own descriptor. Nothing checks that the equivalent provider shares
the primary's owner / root identity (mapping an origin to a root identity is
explicitly deferred — `wrap/invoke.rs:52`).

**Failure scenario (multi-identity mesh — the model the owner-scope gate exists
to defend):** a hostile co-tenant wraps the same public-schema tool
`--substitutable`, declaring `provider_equivalent` + `none` and serving its
describe handler with `OwnerScope::any()` so the operator can read it. Its
descriptor then fingerprint-matches the operator's own wrapped tool. Two
outcomes:

1. **Collapse squatting** — `group_capabilities` merges the attacker's node into
   the operator's logical capability, and `primary()` picks the **lowest node
   id** (`grouping.rs:50`). If the attacker's id is lower, the operator sees /
   pins / invokes the attacker's node *behind their own capability name*.
2. **Failover interception** — when the operator's real primary times out,
   `invoke` (`mesh_gateway.rs:387`) routes the model's tool arguments to the
   attacker's node, which harvests them and returns a plausible result.

Benign on single-owner meshes (every peer is the operator's own node); an
argument-exfiltration / confused-deputy path on shared ones.

**Fix direction:** restrict collapse and failover targets to providers proven to
share the primary's owner/root identity (the deferred root-identity mapping),
not merely a matching public contract.

### F3 (Medium) — Reader task can deadlock replying to a server-initiated request

`wrap/stdio.rs:331` (`reply_method_not_found` → `write_line`), reached from
`dispatch_line` (`wrap/stdio.rs:285`).

`read_loop` is the **sole drainer** of the wrapped child's stdout, yet it
answers server-initiated requests (`sampling` / `elicitation` / `roots`)
*inline* by writing a `method not found` reply to the child's stdin.

**Failure scenario:** the child sends a server-initiated request while its stdout
pipe is full (it is blocked writing and therefore not reading stdin). The reply
`write_line` blocks on the full stdin pipe; because the reader is now blocked, it
stops draining stdout, so the child stays blocked → both pipes wedge and every
in-flight request hangs forever.

**Fix direction:** perform the reply write off the read path — a dedicated writer
task fed by a channel, so draining stdout never depends on a stdin write
completing.

### F4 (Medium) — `tools/list` serially describes every pin through the full retry budget, freezing the serial shim

`serve/shim.rs:321` (`promoted_pinned_tools`).

The loop awaits `self.gateway.describe(&id).await` one pin at a time. Each
`describe` can burn `MAX_ATTEMPTS × CALL_TIMEOUT` (~20s) when the pin's provider
is down or hitting the reply-channel race. Because the serve loop is
single-threaded (a `select!` over one stdin line stream), a `tools/list` with
several such pins blocks the **entire server** — including the pin-store poll
that emits `list_changed` — for `pins × ~20s`.

**Fix direction:** bounded-concurrent fan-out, exactly as `MeshGateway::search`
already does with `stream::iter(...).buffered(MAX_CONCURRENT_FETCHES)`
(`mesh_gateway.rs:288`).

### F5 (Medium) — Fixed 5s `CALL_TIMEOUT` makes any slower tool permanently fail and re-run

`serve/mesh_gateway.rs:52`.

`CALL_TIMEOUT` is a global 5-second per-attempt deadline. A legitimately slow
MCP tool (web fetch, image generation, a long shell command) exceeds it on every
one of the 4 attempts → `invoke` returns `GatewayError::Transport` after ~20s
**despite the tool succeeding**, and the provider ran the tool 1–4 times
(compounding F1). The bridge is effectively unusable for any tool slower than
5 seconds.

**Fix direction:** make the deadline configurable and/or per-tool (with a much
larger ceiling for invoke than for describe), rather than one 5s constant for
both describe and invoke.

### F6 (Medium) — Promoted-pinned-tool dispatch forwards `Null` args instead of defaulting to `{}`

`serve/shim.rs:297`.

`CallToolParams.arguments` defaults to `Value::Null` when the host omits it
(`spec/mod.rs:307`, `#[serde(default)]`). The direct pinned-tool path forwards
`args.clone()` verbatim, so a **no-argument** pinned tool called as
`{"name":"foo"}` fails — `validate_args`/the provider's `parse_arguments` reject
a non-object body. The *same* capability invoked via the `net_invoke_capability`
meta-tool succeeds, because that path defaults a missing `arguments` to `{}`
(`serve/shim.rs:414`). Identical capability, opposite result depending on
invocation path.

**Fix direction:** normalize `Null → json!({})` at line 297 (or once inside
`invoke_capability`).

### F7 (Medium) — Unbounded line read from an untrusted wrapped server can OOM the node

`wrap/stdio.rs:239` (`BufReader::new(stdout).lines()` / `next_line()`).

`next_line()` accumulates until a newline with **no length cap**. A malicious or
buggy third-party stdio MCP server (an `npx`/`uvx` package) that writes gigabytes
to stdout without a newline grows a single `String` unbounded → OOM / process
abort of the wrapping node.

**Fix direction:** cap the line length (bounded read), and drop the line / kill
the child on overflow rather than buffering without limit.

### F8 (Low–Medium) — Pin-store temp file is created at the process umask before being chmod'd to 0600

`serve/pins.rs:192` → `serve/pins.rs:202`.

`tokio::fs::write(&tmp, &bytes)` creates the temp file at the current umask
(0644/0666 under a permissive umask); the `set_permissions(0o600)` only lands
afterwards. Between the two, another local user can read the consent store; and
if the process crashes after the write, a umask-perms `.tmp.<pid>` sibling
lingers group/world-readable — defeating the owner-only 0600 guarantee (commit
`a65f4603d`). Unix only.

**Fix direction:** create the temp via `OpenOptions` with `mode(0o600)` from the
start, so the file is never briefly world-readable.

### F9 (Low–Medium) — Pinned tool-name ↔ capability mapping is recomputed per call and can shift under out-of-band pin changes

`serve/shim.rs:340` (`resolve_pinned_tool`) + `assign_pinned_tool_names`
(`serve/shim.rs:593`).

`assign_pinned_tool_names` assigns the base name to the first capability that
wants it within the *current* approved set and hash-disambiguates later
colliders — so the assignment is **set-dependent**. Between a pin change and the
host re-fetching `tools/list` on `list_changed`, a cached `tools/call` name can
resolve to a *different* still-approved capability (invoked with the arguments
the model shaped for the originally-listed one), or to none.

Requires a base-name collision (two ids that sanitize to the same 64-char base)
plus the pin set changing before the host re-lists. The "unique, non-shadowing
name" guarantee (commit `567140df8`) holds only within a single store snapshot,
not across the host's cached list.

**Fix direction:** persist the assigned name in the pin record, or always
hash-suffix so a name depends only on its own id, not on set membership.

### F10 (Medium, altitude) — Wrap side drops tools whose MCP name isn't already channel-safe

`wrap/descriptor.rs:87` (`is_serviceable_tool_id`).

The substrate channel-name rule is lowercase `[a-z0-9._/-]`, so
`is_serviceable_tool_id` rejects any uppercase / camelCase name and
`discover_and_lower` **skips** it. Common real tools (`createIssue`, `getRepo`,
…) are therefore not bridged, making whole servers partially unbridgeable. The
demand side already implements charset-safe name allocation with deterministic
hash disambiguation (`safe_tool_name` / `assign_pinned_tool_names`,
`serve/shim.rs:593`), so the same problem has two divergent answers — "drop it"
on the supply side, "sanitize it" on the demand side.

**Fix direction:** a single shared ingestion-time name-allocation layer (with a
stored forward/reverse map for invoke), reused by both sides, so arbitrary MCP
tool names are bridged rather than dropped. Documented as deferred to Phase 4,
but worth tracking as a coverage gap.

### F11 (Low, altitude) — `CapabilityId.provider` is an unnormalized string while routing normalizes it

`serve/backend.rs:29` + `serve/mesh_gateway.rs:458` (`parse_node`).

`parse_node` normalizes the provider for **routing** — it trims whitespace and
accepts `0x2a` as well as `42` — but consent and the pin store key on the raw
`CapabilityId` / `id.display()`. So `0x2a/echo`, ` 42/echo`, and `42/echo` route
to the same node yet form three *different* consent/pin keys. Search only ever
emits the decimal form, so it is latent today, but a hex/whitespace-form
`net mcp pin approve` or a `net_request_pin` recorded under one spelling never
satisfies an invoke passed the other. Only the routing half is canonicalized;
the identity half is not.

**Fix direction:** make `provider` a typed `u64` (normalize once at `parse`), so
identity has a single canonical spelling instead of being re-normalized ad hoc
on the routing path only.

### F12 (Low) — `RequestId` cannot model out-of-range / fractional numeric ids

`spec/mod.rs:114`.

`RequestId` is `Number(i64) | Str(String)`. A JSON-RPC id that is a numeric
value outside `i64` range (or fractional) matches neither untagged variant, so
the whole `IncomingMessage` deserialization fails; the shim answers
`PARSE_ERROR` with a `null` id the host cannot correlate to its request, and
that call hangs / errors on the host side. Low real-world likelihood (MCP hosts
use small integer or string ids), but the id should round-trip as a raw JSON
number to be fully spec-safe.

### F13 (Low, cleanup) — CLI mesh/identity helpers duplicated instead of reusing `context.rs`

`cli/src/commands/mcp.rs:277,312` and `cli/src/commands/wrap.rs:251,286`.

`load_identity` and `build_{shim,wrap}_mesh` are **byte-identical** across the
two command files, and both re-implement existing `cli/src/context.rs` helpers:
`load_identity_keypair` (read-file + toml-parse + hex-decode + 32-byte validate)
and `build_remote_mesh` (MeshBuilder + start + routed-handshake connect), from
which they differ only by the bind address and an added `.identity(...)`.

**Fix direction:** hoist one `pub(crate)` seed loader and extend
`build_remote_mesh` to take an optional identity, then call from both commands —
three copies become one.

---

## Resolution

All findings fixed on `mcp-3`, each as its own commit with tests. Where the
shipped fix departs from the review's suggested direction, the note says why.

| # | Commit | What shipped |
|---|--------|--------------|
| F1 | `553c0d0` | `CapabilityGateway::invoke` takes an `InvokeSafety` flag; an at-most-once (credentialed) invoke retries only on `NoRoute` (proven non-execution), never on a timeout — a duplicate-safe (uncredentialed) invoke still retries transient timeouts for reply-race resilience. Retry-safety is derived from the provider's declared status (a resilience hint, never the security gate). |
| F2 | `ca0d38a` | Cross-provider collapse **and** failover are now **opt-in, off by default** (`MeshGateway::trust_equivalent_providers` / `net mcp serve --trust-equivalent-providers`): each provider is discovered, pinned, and invoked on its own node id, so a peer that forged a matching contract can't stand in for another. The suggested deeper fix (verify shared owner/root identity) stays deferred; the opt-in default closes the exposure until it lands. |
| F3 | `739535d` | The `method not found` reply to a server-initiated request is spawned off the reader, so draining stdout never blocks on a stdin write (no two-pipe deadlock). |
| F4 | `f12cc80` | `promoted_pinned_tools` describes pins with bounded concurrency (`buffered`), so `tools/list` latency is the slowest single describe, not the sum. |
| F5 | `5fb412e` | Split deadlines: describe keeps 5s, invoke defaults to 120s and is overridable via `MeshGateway::with_invoke_timeout`. |
| F6 | `c90b673` | `invoke_capability` normalizes a `null` argument to `{}` (covers both the pinned-tool path and an explicit `"arguments": null`). |
| F7 | `e2a5dc5` | A bounded line reader caps a stdout line at 32 MiB; an over-length line is drained and dropped rather than buffered. |
| F8 | `3d0dc41` | The pin-store temp is created `0600` up front via `OpenOptions`, closing the umask window; no leftover temp after a successful save. |
| F9 | `d1cf2d4` | Pinned tool names are id-local (always hash-suffixed), independent of the approved set, so a cached name never remaps to a different capability. (Simplified further in `4eeb784`.) |
| F10 | `33bcbab` | Non-channel-safe tool names (camelCase, spaced, punctuated) are sanitized into a stable channel-safe id and bridged; the original is kept in `LoweredTool::mcp_name` for invocation. Only an empty name is skipped. |
| F11 | `a0ec579` | `CapabilityId::new`/`parse` canonicalize the provider node id (decimal, trimmed) so identity and routing agree; still carried as a string, not a typed `u64` (the deeper form remains a later refinement). |
| F12 | `2f0aff5` | `RequestId::Number` carries a raw `serde_json::Number`, so any JSON-RPC numeric id round-trips. |
| F13 | `35b5d6f` | `load_operator_identity` + `build_attached_mesh` hoisted into `cli/src/context.rs`; the three duplicated CLI helpers collapse to one. |

**Follow-up review comments (`cubic-dev-ai`):** R1 (`4eeb784`) — remove the dead
reserved-name loop in `stable_pinned_tool_name`; R2 (`fce36b1`) — replace the
`duplicate_safe` bool with the `InvokeSafety::{DuplicateSafe, AtMostOnce}` enum
(closing the boolean trap). A rustdoc-link fix (`4610dee`) and a rustfmt-only
commit (`67c6679`) keep the branch green on all CI gates.

**Still deferred (tracked, not regressions):** F2's full fix (verify a failover
target shares the primary's owner/root identity) and F11's typed-`u64`
provider both wait on the permission-system work the plan already defers.
