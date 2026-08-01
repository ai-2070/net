# cubic AI review of PR #737 — what is still open

**PR:** [ai-2070/net#737 "Security: Payments"](https://github.com/ai-2070/net/pull/737) · base `master`, head `security-payments`
**Checked against:** local `net-x402` working tree on `security-payments` (8 commits ahead of the PR head `b629783ca`, plus uncommitted edits), 2026-08-01.

## Method

All 191 review comments were pulled from the GitHub API (`/pulls/737/comments`, 11 cubic review
runs against 11 successive commits) and de-duplicated to ~74 distinct findings — cubic re-reports the
same issue on every run until the anchor line changes, so raw comment count overstates the work by
roughly 2.5×. GitHub's own thread state is not a usable signal here: only 5 of 191 threads are marked
resolved, so every finding was checked against the source rather than against the resolution flag.

**Result: 15 of ~74 distinct findings are still open.** They are listed below, most severe first.
Everything else is fixed — see [Verified closed](#verified-closed) for the summary.

---

## Open findings

### 1 — P1 · `TOKEN_USER` is read through an alignment-1 buffer (UB on Windows)

`net/crates/net/payments/src/policy/file_mode.rs:590`

```rust
let mut buffer = vec![0u8; needed as usize];   // :573 — alignment 1
...
let sid = unsafe { (*(buffer.as_ptr() as *const TOKEN_USER)).User.Sid };
```

A `Vec<u8>` guarantees no alignment beyond 1. Constructing a `*const TOKEN_USER` from it and
dereferencing is undefined behaviour whenever the allocation is not suitably aligned. Every
owner-only file creation on Windows runs through this path.

**Fix:** `std::ptr::read_unaligned::<TOKEN_USER>(buffer.as_ptr().cast())` before touching `User.Sid`.

*cubic:* [#3695468319](https://github.com/ai-2070/net/pull/737#discussion_r3695468319),
[#3695255495](https://github.com/ai-2070/net/pull/737#discussion_r3695255495)

---

### 2 — P1 · `PublicOnly` admits IPv6 space outside `2000::/3`

`net/crates/net/payments/src/http_policy.rs:239` (`is_public_v6`)

The special-purpose enumeration now covers `fec0::/10`, `2001:2::/48`, `64:ff9b::/96`,
`64:ff9b:1::/48`, `2002::/16`, Teredo, and both v4-in-v6 embeddings — but the function is still a
blocklist over the whole address space, so IANA-reserved prefixes nobody enumerated pass. `4000::1`
is admitted today.

The module doc at `:201` argues the blocklist shape deliberately ("an allowlist of the globally-routable
space … is not writable"), which is a fair position for the *registry-driven* part. It does not cover
the structural case: `2000::/3` is the entire global-unicast assignment, and everything outside it is
reserved by RFC 4291 — that boundary is syntactic, not a registry lookup.

**Fix:** require `(first & 0xe000) == 0x2000` before applying the existing exclusions.

*cubic:* [#3695468322](https://github.com/ai-2070/net/pull/737#discussion_r3695468322)

---

### 3 — P1 · A real facilitator's `/supported` set is never checked

`net/crates/net/bindings/python/src/payment_provider.rs`

`build_pricing_terms` now validates every authored requirement against the *selected registry*
(good — that closed the mock-in-production-terms hole). Nothing validates it against what the
**facilitator** actually supports. A provider can be configured with a real facilitator that does
not handle an announced `(scheme, network)`, publish those terms, issue quotes under them, and fail
only at verify/settle — after the caller has signed an authorization.

There is no `supported` reference anywhere in the binding.

**Fix:** fetch and retain `/supported` at construction; reject announced requirements the facilitator
cannot serve, before publication.

*cubic:* [#3695562432](https://github.com/ai-2070/net/pull/737#discussion_r3695562432)

---

### 4 — P2 · `status().tier` reports the *last* verified tier, not the highest

`net/crates/net/payments/src/engine/mod.rs` — `last_verified_tier`

```rust
fn last_verified_tier(chain: &[VerificationEvent]) -> Option<VerificationTier> {
    chain.iter().rev()
        .find(|e| matches!(e.status, VerificationStatus::Verified))
        .map(|e| e.tier)
}
```

`QuoteStatus::tier` is documented as *"Highest verified tier reached"* (`:292`). It is not: a
`re_verify` after `re_verify_with_checker` appends a fresh `Observed` event, and `status()` then
reports `Observed` for a quote already independently confirmed on-chain. Idempotent `accept_payment`
retries read the same value.

The docs half of this finding **was** fixed (`re_verify`'s rustdoc now says it cannot raise
confidence and points at `re_verify_with_checker`). The tier accessor was not.

**Fix:** take the max over `Verified` events, or refuse to append a lower-tier event after a higher one.

*cubic:* [#3695255508](https://github.com/ai-2070/net/pull/737#discussion_r3695255508)

---

### 5 — P2 · Rust binding approval test asserts the pre-change contract and will fail

`net/crates/net/bindings/python/src/capability_gateway.rs:1790-1813`

`SpendPolicyEngine::approve` now returns early on an unknown id:

```rust
let Some(record) = s.approvals.get_mut(&quote_id) else { return (false, false) };
```

and `reject` returns `s.approvals.remove(&quote_id).is_some()`. But
`the_verbs_marshal_the_spend_store_round_trip` approves `"q-1"` against a **fresh** store and asserts:

```rust
assert_eq!(v["changed"], true);           // :1796 — now false
...
assert_eq!(v["changed"], true);           // :1808 — reject, now false
```

The Python-side fixture was updated to the new no-op contract; this one was not. It fails under
`--features payments`.

**Fix:** create a held quote first, or assert the unknown-id no-op behaviour as the Python test does.

*cubic:* [#3695273126](https://github.com/ai-2070/net/pull/737#discussion_r3695273126)

---

### 6 — P2 · Base Sepolia USDC is still an unpinned EIP-712 domain

`net/crates/net/payments/src/core/registry.rs:265`

```rust
display_name: Some("USDC (Base Sepolia testnet)".to_string()),
equivalence_class: None,
eip712_name: None,
eip712_version: None,
```

`check_requirements` only enforces the domain when the entry pins it, so any `name`/`version` passes
for this known deployment and is used to author typed data. The signature then fails at the wallet
or on-chain rather than at configuration time.

The related finding — that `production_registry_v1` should drop testnets entirely — **was** fixed
(there is now a test asserting no `eip155:84532` entry survives into it). This one is about
`default_registry_v1`, where the entry still lives and is still unpinned.

**Fix:** pin `USDC` / `2` on the Base Sepolia entry.

*cubic:* [#3695255515](https://github.com/ai-2070/net/pull/737#discussion_r3695255515)

---

### 7 — P2 · The retry-idempotency claim contradicts the replay guard

`net/crates/net/payments/src/flow/mesh.rs:264` and `core/quote_request.rs:175`

Both docs now say the same thing:

> a transport-level retransmit re-sends the already-serialized request rather than re-deriving, so
> retry idempotency is unaffected.

`SeenNonces::admit` refuses exactly that: the stored deadline is `expires_at_ns + skew`, and a
retransmit inside that window hits `now_ns < deadline` → `ReplayedNonce`. There is no response cache
in `serve_payments` — the quote handler encodes and returns, keeping nothing. So a caller whose quote
response is lost on the wire cannot get it back: re-deriving mints a new nonce (a second quote), and
re-sending is refused.

This is a doc/behaviour disagreement, not a security hole — but the doc is the contract clients will
build against.

**Fix:** either cache and return the first response per `(caller, nonce)`, or state plainly that a
quote request is single-use and a retry is a new request.

*cubic:* [#3694972908](https://github.com/ai-2070/net/pull/737#discussion_r3694972908),
[#3695160587](https://github.com/ai-2070/net/pull/737#discussion_r3695160587)

---

### 8 — P2 · The replay claim is taken before the scheme signature is verified

`net/crates/net/payments/src/engine/mod.rs:826` → `:840` → `:1013`

`payload.replay_key()` derives the semantic key from `(from, nonce)` and the claim transaction runs
at `:840`; `self.facilitator.verify(...)` does not run until `:1013`. An attacker who knows a real
authorization's `(from, nonce)` can submit a garbage signature and take the claim first, making the
genuine payment observe `InProgress`.

**Partially mitigated** — `release_claim` on the verify-failure path (`:1016`) shrinks the window to
one facilitator round-trip rather than the full `in_flight_ttl_ns`. The ordering cubic flagged is
unchanged, so a stuck or slow facilitator still parks a real payment behind a forged claim.

**Fix:** verify the scheme payload before claiming the semantic key, or claim under an unforgeable
pre-verification key.

*cubic:* [#3695255519](https://github.com/ai-2070/net/pull/737#discussion_r3695255519)

---

### 9 — P2 · `AllowPrivate` still admits `fec0::/10` (deliberate — flagged for the record)

`net/crates/net/payments/src/http_policy.rs:191`, reached from
`facilitator/client.rs:124` and `checker/transport.rs:70`

```rust
// fc00::/7 unique local, plus deprecated fec0::/10 site local
// — both are "an operator's own network", which is what
// `AllowPrivate` is for. The stricter policies refuse them via `is_public`.
(first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfec0
```

cubic asked twice to carve `fec0::/10` out of `AllowPrivate` so facilitator bearer tokens cannot go
to a deprecated site-local address. The branch instead wrote an explicit rationale for keeping it.
Listed here because the finding is technically unaddressed, not because the reasoning is wrong —
`AllowPrivate` is operator-configured, and the agent-supplied path (`PublicOnly`) does refuse it.
**No action needed unless the position changes.**

*cubic:* [#3695255461](https://github.com/ai-2070/net/pull/737#discussion_r3695255461),
[#3695273102](https://github.com/ai-2070/net/pull/737#discussion_r3695273102)

---

### 10 — P2 · `provider_mesh` is still an unowned local in the E2E fixture

`net/crates/net/payments/tests/mesh_payments_e2e.rs:84`

`World::start()` builds `provider_mesh`, hands its node id to the struct, and drops the wrapper on
return. `World`'s fields are `caller_mesh, caller_keys, provider_node, provider_id, provider_log,
registry, capability, template, terms_json, clock, dir, _serving` — no `provider_mesh`.

The suite passes, so `MeshNode` evidently self-owns its background loop. The lifetime is implicit
rather than stated, which is what the finding is about: if that ever stops being true, these tests
route to a dead provider and every H1 refusal assertion passes for the wrong reason.

**Fix:** keep `provider_mesh` in `World` alongside `caller_mesh`.

*cubic:* [#3695160598](https://github.com/ai-2070/net/pull/737#discussion_r3695160598)

---

### 11 — P3 · `Content-Length` is narrowed before the cap comparison

`net/crates/net/payments/src/http_policy.rs:491`

```rust
if let Some(len) = response.content_length() {
    if len as usize > cap {
```

`content_length()` is `u64`; `len as usize` truncates on 32-bit targets, so a declared body of
`4 GiB + 1` compares as `1` and the pre-read rejection is skipped. The streaming cap still holds, so
the impact is a missed early rejection, not an unbounded read.

**Fix:** compare in `u64` (`len > cap as u64`).

*cubic:* [#3695160608](https://github.com/ai-2070/net/pull/737#discussion_r3695160608),
[#3695273140](https://github.com/ai-2070/net/pull/737#discussion_r3695273140)

---

### 12 — P3 · Retention cutoff adds to an unvalidated `u64` day

`net/crates/net/payments/src/policy/spend.rs:365` and `:368`

```rust
s.counters.retain(|k, _| counter_day(k).is_some_and(|d| d + COUNTER_RETAIN_DAYS >= day));
s.reservations.retain(|_, r| r.day + COUNTER_RETAIN_DAYS >= day);
```

`d` and `r.day` come from the persisted JSON. A malformed or hostile policy file with a day near
`u64::MAX` panics the pruning pass in debug and wraps in release — dropping live counters.

The reservation-pruning half of the original finding **was** added (that is the `s.reservations.retain`
line); the saturating comparison it asked for was not.

**Fix:** `day.saturating_sub(COUNTER_RETAIN_DAYS) <= d` in both paths.

*cubic:* [#3695255539](https://github.com/ai-2070/net/pull/737#discussion_r3695255539)

---

### 13 — P3 · `binding_required` has no row in the failure-schematic contract table

`.claude/skills/net-payments/failure-schematic.md:76`

The table documents `binding_malformed` and its siblings; the newly emitted `binding_required` wire
reason is absent. Clients consulting the published recovery contract will not find the reason they
receive.

The *code* half of this finding was fully addressed — `R::BindingRequired` is in `all_reasons()`
(`flow/mod.rs:1154`), has its mapping row (`:101`), and has a dedicated test
(`binding_required_is_a_configuration_row_not_a_security_one`, `:1230`). Only the skill doc's table
was missed.

**Fix:** add the row — `caller_configuration_error` / `caller_operator` / not-retryable /
not-safe-to-requote / `fix_payment_client`.

*cubic:* [#3694972943](https://github.com/ai-2070/net/pull/737#discussion_r3694972943),
[#3695160603](https://github.com/ai-2070/net/pull/737#discussion_r3695160603),
[#3695255536](https://github.com/ai-2070/net/pull/737#discussion_r3695255536)

---

### 14 — P3 · Python skill doc presents the binding default as an opt-in

`.claude/skills/net-payments/bindings/python.md:114-119`

```python
#    Require the caller's possession proof (recommended for new deployments):
#    without it the quote id alone redeems, and it is not a secret.
provider = PaymentProvider(
    mesh, state_path,
    facilitator_url="https://facilitator.example.com",
    require_invocation_binding=True,
)
```

`require_invocation_binding` now **defaults to `True`** — the Node constructor rustdoc says so
explicitly, and `PaymentEngine::new` sets it (`engine/mod.rs:664`). Reading this page, an operator
concludes the safe posture is opt-in and that a deployment without the flag made a choice.

The neighbouring `production_registry=True` example on the same page **was** updated correctly, as
was the whole TypeScript page.

**Fix:** note "(this is the default)" on the line, and reframe the surrounding comment around the
`False` opt-out.

*cubic:* [#3695468335](https://github.com/ai-2070/net/pull/737#discussion_r3695468335)

---

### 15 — P3 · Audit resolution table cites commit subjects that do not exist

`docs/internal/misc/SECURITY_AUDIT_2026_08_01_PAYMENTS.md:22, 23, 25`

| Row | Cited subject | Actual branch commit |
|-----|---------------|----------------------|
| M2 | `fix(payments): remove tier from facilitator outcomes…` | no such subject on the branch |
| M3 | `fix(payments): log a non-authorizing quote_ref, never the quote id` | `fix(payments): key quote_ref so logs cannot be inverted back to a credential` |
| M5 | `fix(payments): key replay on the scheme's signed material, fully namespaced` | `fix(payments): canonicalize the EIP scope in the replay identity` + `fix(payments): 0x-prefix in replay identity; drop testnets from the production registry` |

The table exists to be an audit trail; a reader tracing M3 to its fix cannot find the commit.

Note the companion finding on the same document **was** fixed — the M3 body now says the quote id is
logged at **four** sites and lists `flow/mod.rs:761`.

**Fix:** align the cited subjects with the real ones, or cite short hashes.

*cubic:* [#3695468331](https://github.com/ai-2070/net/pull/737#discussion_r3695468331)

---

## Verified closed

Checked in source and confirmed fixed. Grouped by the area cubic raised them in.

**Windows / filesystem security (L3 family)** — `create_owner_only` now passes a
`SECURITY_ATTRIBUTES` with an explicit protected DACL at `CreateFileW` time rather than setting it
after; `open_append_owner_only` uses `O_NOFOLLOW` / `FILE_FLAG_OPEN_REPARSE_POINT` so a symlink or
FIFO planted at a shared path is refused atomically; `restrict_handle` is `fchmod` on the open
handle, not a pathname chmod; `migrate_to_a_fresh_owner_only_file` handles pre-existing permissive
logs; the billing log holds its handle instead of reopening by name; `store.rs` runs
`create_owner_only` and its stale-temp retry on `spawn_blocking`;
`Win32_Storage_FileSystem` was added to the `windows-sys` feature list.

**SSRF / outbound HTTP** — `X402HttpFlow::new` defaults to `PublicOnly`; `is_public_v6` refuses
`fec0::/10`, `2001:2::/48`, both NAT64 prefixes, `2002::/16`, Teredo, and the IPv4-translated block;
`is_public_v4` refuses the 6to4 relay anycast prefix; `http_policy::client` applies `no_proxy()` for
any restricting policy; the duplicate `is_payment_safe_url` predicate was deleted and the paid retry
routed through the shared function; the cleartext-`http` exception is address-level, so
`http://localhost` is refused; policy refusals are classified terminal (and release the reservation)
in both the checker and the 402 door; `ReadError::Transport` stays retryable.

**Replay guard** — `SeenNonces` is keyed per caller, bounds nonce length *before* signature
verification, carries both a global capacity and a per-caller share
(`CallerReplayQuotaExhausted`), sweeps against the stored acceptance deadline rather than map
membership, avoids re-scanning a full unchanged map via `sweep_could_free` / `earliest_deadline`,
counts entries rather than admissions against the quota, and releases the nonce when issuance is
denied. The nonce sequence is a process-global atomic, not per-channel.

**Spend policy** — an existing reservation is consulted *before* the per-day cap (retries stay
`Allowed`); `Reservation::holders` makes a reservation an in-flight claim so a failing retry cannot
free budget another attempt is spending; reservations are pruned on the counter-retention horizon;
the HTTP-402 pseudo-quote identity mixes the live pid, a process token, and a global atomic, so
forked workers and separate flows cannot collapse two payments into one reservation; an approved
402 quote is now redeemable (`approved_quote` → byte-compared requirements → `clear_approval`);
mock reservations are released on the oversized-body path.

**Invocation binding** — defaults to `true` at the engine and in both bindings; the
`redeem_for_invocation` rustdoc scopes bearer fallback to
`with_require_invocation_binding(false)`; `R::BindingRequired` is in `all_reasons()` with its
mapping row and tests; `bench_common` opts out so `cargo bench` still runs.

**Registry / terms** — `production_registry_v1` drops every valueless asset (test-pinned);
`default_registry_v1`'s doc says so; `build_pricing_terms` takes `production_registry` and validates
each requirement against the selected registry; a provider-derived authoring path exists in both
bindings; the `Self::check_requirements` intra-doc link points at `AssetRegistry`.

**Replay identity** — `hex_key` normalizes the optional `0x` prefix and case; `canonical_eip_scope`
re-renders the CAIP-2 decimal chain id and the contract address, so one authorization has one key.

**Bindings / packaging** — `payments-http` is in the default feature set for both the Node and
Python crates (so published artifacts can reach a real facilitator); the `.pyi` and CI comments say
so; `registryVersion` / `registry_version` is exposed and the downgrade tests assert it on the
success path (with a mock-side negative control) instead of passing vacuously; `provider.close()`
was removed from the Python test; the Node constructor synopsis documents the required backend
choice; the three stray `patch*.py` scripts were deleted.

**Doc links / formatting** — every `VerificationTier` and `DestinationPolicy` intra-doc link is
crate-qualified; `cargo fmt` was run; `X402HttpFlow`'s doc comment was restored; the
`DestinationPolicy` docs name `PublicOnly` as the default and `PublicOrLoopback` as the local-testing
opt-in.

**Tests** — `tracing_capture`, `scripted_checker`, and `rpc_fixture` are shared modules instead of
per-file copies; the unused `ScriptedChecker::query` accessor is gone; `accept_eip155_inner` is the
one eip155 fixture; the misleading binding opt-out and comment were dropped from
`mcp_gate_composition`; the `http402_outbound` doc says three fetches; the forged-identity test
asserts the refusal names the signature check; `mesh_payments_e2e` has a `quote_request` helper;
a Node test helper replaces the positional `undefined` filler.

**`rpc.rs`** — `RpcStreamingContext::caller_origin` was reworded to match `RpcContext`, and the
`session_node` reference now points at `from_node`.
