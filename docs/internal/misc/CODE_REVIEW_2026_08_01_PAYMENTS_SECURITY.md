# CODE REVIEW 2026-08-01 — Payments security branch (`security-payments`)

> **STATUS: OPEN, not signed off.** One blocker (§B1, clippy fails on
> production code — six of the ten errors are platform-independent and will
> fail the `Run clippy — net-payments (production code, strict)` CI step on
> ubuntu). Two security findings, one verified correctness defect, three
> lower-severity items, three nits.
>
> **This review did not exercise the branch's unix-specific file-permission
> claims.** `file_mode`'s symlink / FIFO / migration tests, the billing log's
> symlink test, and both `read_only_writes_audit.rs` and
> `redeem_denial_no_write.rs` are `#[cfg(unix)]` and ran **0 tests** on the
> Windows host this pass ran on — and those are exactly the guarantees the
> `file_mode` module was written to establish. A Linux/macOS run is required
> before merge. See [Verification](#verification).

**Scope:** the full branch diff `master..b629783ca` (merge base `7cd2d6584`)
— 47 commits, 74 files, +8208/−716.

| file | what |
|---|---|
| `payments/src/http_policy.rs` | **new** — shared money-path HTTP boundary: scheme, destination (`GuardedResolver`), body bounds |
| `payments/src/policy/file_mode.rs` | **new** — owner-only payment files on every platform; Windows DACL-at-creation, `O_NOFOLLOW`, permissive-predecessor migration |
| `payments/src/core/quote_request.rs` | **new** — `net.payment.quote_request@1`, the caller-signed demand-side identity proof, plus `SeenNonces` |
| `payments/src/x402/payload.rs` | `replay_key` rebuilt on scheme-signed material, fully namespaced |
| `payments/src/facilitator/{traits,client,mock}.rs` | `tier` removed from `VerifyOutcome`/`SettleOutcome` |
| `payments/src/engine/mod.rs` | engine mints `Observed`; `require_invocation_binding` defaults on |
| `payments/src/policy/spend.rs` | refcounted `Reservation` ownership; `approve()` no longer mints records |
| `payments/src/flow/{http402,mesh,mod}.rs` | destination policy on the outbound door; signed quote requests on the mesh; `quote_ref` log hygiene |
| `payments/src/core/registry.rs` | `production_registry_v1`; pinned EIP-712 domain |
| `bindings/{node,python}` | mandatory settlement backend; `payments-http` into default builds |
| `net/src/adapter/net/cortex/rpc.rs` | `RpcContext::caller_origin` doc corrected — routing metadata, not authentication |

**Overall.** Strong work, and the central moves are the right ones. Pulling
scheme, destination and body bounds out of three hand-rolled copies into one
module — and making the destination policy *be* the resolver rather than a
check standing beside one — closes the check/connect window by construction
instead of by discipline. Removing `tier` from the facilitator outcome types
so no third-party `Facilitator` implementation can mint `final` is
unrepresentable-not-discouraged, which is the correct instinct and is
explicitly argued as such in `traits.rs`. `create_owner_only` attaching the
descriptor at `CreateFileW` time rather than tightening afterwards, and
migrating a permissive predecessor rather than chmod'ing it, both follow from
the same correctly-identified fact (access is granted at open time, so a
handle already issued cannot be revoked). Refcounted `Reservation` ownership
replacing amount-and-caller's-clock arithmetic removes a real class of
double-release bug. Doc comments explain *why* rather than restating the code.

The findings below are one CI blocker, two places where a guard is narrower
than the module claiming to own it, one verified bookkeeping defect, and
several residual-scope items.

Per the review-tracking rule, the `B*` / `§N` labels are for this document
only — they do not belong in code or commit messages.

---

## Blocker

### B1 — `cargo clippy --all-features --lib --bins -- -D warnings` fails with 10 errors, all branch-added

CI runs exactly this from `net/crates/net/payments` (`.github/workflows/ci.yml`,
step *Run clippy — net-payments (production code, strict)*). It currently
fails. `master` is clean; `cargo fmt -p net-payments -- --check` is also clean,
so this is the only hygiene gate that breaks.

**Platform-independent — these fail the ubuntu CI leg:**

| site | lint |
|---|---|
| `src/billing/mod.rs:161` | `clippy::expect_used` — `handle.as_mut().expect("just opened")` |
| `src/policy/spend.rs:645` | `clippy::expect_used` — `s.reservations.remove(&quote_id).expect("just borrowed")` |
| `src/core/quote_request.rs:170,171,172,173` | `clippy::doc_lazy_continuation` ×4 |

The doc lint is one root cause, not four: `derive_nonce`'s doc wraps such that
line 169 ends `…caller + capability + template` and line 170 begins
`/// + timestamp, and …`. A line-initial `+ ` is a markdown list marker, so
the four lines after it are parsed as an unindented lazy continuation.
Rewording so the `+` is not line-initial fixes all four at once — and is
better than the `///  ` indent clippy suggests, because the intent was never a
list.

The two `expect`s are both genuinely-infallible-by-construction, which is
precisely the shape `[lints.clippy]`'s panic hygiene asks you to write
differently rather than annotate. `spend.rs:645` in particular is a
`get_mut` immediately followed by a `remove` of the same key — restructuring
to a single `remove`-then-reinsert-if-still-held, or an `if let Some(entry) =
s.reservations.remove(&quote_id)` with the decrement inside, drops the `expect`
entirely.

**Windows-only — CI (`runs-on: ubuntu-latest`) will not catch these, but every
Windows contributor's local clippy will:**

| site | lint |
|---|---|
| `src/policy/file_mode.rs:362` | `clippy::unnecessary_mut_passed` — `&mut attrs` to `CreateFileW` |
| `src/policy/file_mode.rs:499` | `clippy::unnecessary_cast` — `SDDL_REVISION_1 as u32` |
| `src/policy/file_mode.rs:543` | `clippy::multiple_unsafe_ops_per_block` — `GetCurrentProcess` + `OpenProcessToken` |
| `src/policy/file_mode.rs:586` | `clippy::multiple_unsafe_ops_per_block` — pointer deref inside the `while` |

Worth noting the asymmetry itself: this branch adds the crate's first
`#[cfg(windows)]` `unsafe` FFI block, and no CI leg lints it. If the
`windows_impl` module is going to carry raw `CreateFileW` / token / SID calls,
the clippy job (or a new one) should lint on a Windows runner too — otherwise
the `unsafe` surface with the least coverage is the one nothing checks.

**Fix:** address all ten, and consider adding a Windows clippy leg.

---

## P1 findings

### §1 — `http_policy::client` leaves reqwest's env-proxy auto-detection on, so the destination policy does not apply when a proxy is configured

`src/http_policy.rs:379-395`.

`reqwest::ClientBuilder` defaults `auto_sys_proxy: true`
(`reqwest-0.13.4/src/async_impl/client.rs:309`), and `client()` never calls
`.no_proxy()`. When `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` is set — routine
in corporate networks, CI images and containers — reqwest connects to the
*proxy* and the target hostname is resolved by the proxy, not locally.
`GuardedResolver` is never consulted for it.

That defeats the module's own central argument, quoted from its header:

> `GuardedResolver` closes that by construction — it *is* the resolver reqwest
> uses, so the addresses it approves are exactly the addresses the client
> dials.

With a proxy in the environment that sentence is false, and it is false
silently: nothing errors, nothing logs, the request simply goes somewhere the
policy never saw. The blast radius is worst exactly where the design took most
care — `X402HttpFlow`'s deliberate `PublicOnly` default exists because "this is
the one money-path client whose URL may be chosen by a model rather than an
operator" (`flow/http402.rs:143-158`), and that is the client whose guard the
proxy removes.

Partial mitigation, worth stating so the finding is not overread:
`check_url_destination` runs before the request on both the outbound door
(`flow/http402.rs:303-320`) and at facilitator/checker construction, so an
**IP-literal** URL is still refused — `http://169.254.169.254/` still fails.
What gets through is a *name*: an attacker-controlled hostname resolving to a
link-local or internal address, or a plain internal hostname, either of which
the proxy will happily reach.

**Fix:** `.no_proxy()` on the builder in `http_policy::client`. That is one
line, but it is a real posture decision for operators behind a mandatory
egress proxy, so it may want to be explicit — a `DestinationPolicy` variant or
a separate constructor argument that says "I accept that the policy does not
apply", rather than the current state where the guard silently does not apply
and nobody is told. Whichever way it goes, the module header's claim needs to
be qualified to match.

### §2 — `is_payment_safe_url` still grants the cleartext exception to the *name* `localhost`, which the shared policy deliberately removed

`src/flow/http402.rs:770-784`; test pinning the weaker behaviour at
`src/flow/http402.rs:803`.

Commit `83a255f48` ("make the cleartext exception address-level; refuse
fec0::/10") made this exception literals-only in `http_policy`, and the
reasoning in `is_loopback_host` is exactly right:

> `localhost` is a name, and a name is whatever DNS says it is — a host file or
> a resolver can point it at a public address, and then "http to localhost" is
> a cleartext request to a remote host, which is exactly what the scheme rule
> exists to prevent.

The outbound door kept its own private copy of the rule and did not get that
fix. `is_payment_safe_url` is the gate that decides whether the signed EIP-3009
authorization — a bearer instrument — goes on the wire in the clear, and it
still returns `true` for `host == "localhost"` with no address check at all.

Reachability, stated honestly: under the default `PublicOnly` the resolver
refuses whatever `localhost` resolves to, so the gap is closed downstream. It
opens only when a caller opts into `with_destination_policy(PublicOrLoopback)`
or `AllowPrivate` — at which point `http://localhost/...` resolving via hosts
file or split-horizon DNS to a LAN address passes both the resolver and this
check, and the authorization leaves unencrypted to a remote host. Both
bindings (`bindings/{node,python}/src/payment_http.rs`) use the default, so
this is Rust-only and requires an explicit opt-in. It is narrow.

It matters anyway because of what the module it bypasses says about itself:

> So they live here once. A new money-path client gets all three by
> construction or none by omission, and the omission is visible.

This is the last money-path client keeping a divergent private copy of a rule
`http_policy` was created to own, and the branch's own header names "they
drifted" as the reason the module exists. Leaving one copy behind reintroduces
the failure mode the refactor was for.

**Fix:** delegate `is_payment_safe_url` to
`http_policy::require_secure_endpoint` (it already takes a `&str` and returns
the same decision), and flip the `http://localhost/x` assertion at
`flow/http402.rs:803` — mirroring what
`facilitator/client.rs`'s test did at line 230.

---

## P2 findings

### §3 — `SeenNonces::admit` double-counts the per-caller quota when a nonce is re-admitted after its deadline

`src/core/quote_request.rs:414-462`. **Verified with a scratch test.**

Re-admitting a nonce whose deadline has passed is explicitly legal, and the
code says so at `quote_request.rs:420-424`:

> A caller reusing a nonce for a *new* request is not replaying anything: the
> old one can no longer be accepted by `verify` either way.

But the admission path does:

```rust
self.seen.insert(key, accept_until_ns);                      // :459 — replaces
*self.per_caller.entry(caller.clone()).or_insert(0) += 1;    // :460 — unconditional
```

When the key is already present (stale, past deadline, not yet swept), the
`insert` *replaces* — map length unchanged — while `per_caller` increments
anyway. The caller's recorded share then exceeds the entries it actually
holds, and stays wrong until the next `sweep` rebuilds `per_caller` from
`seen` (`:493-498`). Sweeps are amortized at `capacity/8`, so under ordinary
traffic that is a long time.

Reproduced with `SeenNonces::with_capacities(64, 4)`:

```rust
seen.admit(&caller, "n", 10, 0).unwrap();   // deadline 10
seen.admit(&caller, "n", 30, 20).unwrap();  // legal re-admit; len still 1
seen.admit(&caller, "a", 40, 20).unwrap();
seen.admit(&caller, "b", 40, 20).unwrap();
seen.admit(&caller, "c", 40, 20)            // len would be 4, cap is 4
    // -> Err(CallerReplayQuotaExhausted { capacity: 4 })
```

Three live nonces held, the fourth refused under a cap of four.

Severity is liveness, not security: reaching it requires a caller to reuse its
own nonce string, and the only identity harmed is that caller's own quota
(`derive_nonce`'s sequence counter means a well-behaved caller never does
this). The same drift also leaves a phantom count behind after `release`
(`:514-535`) when it fires on a double-counted entry.

**Fix:**

```rust
if self.seen.insert(key, accept_until_ns).is_none() {
    *self.per_caller.entry(caller.clone()).or_insert(0) += 1;
}
```

and a regression test in `quote_request.rs`'s module — the invariant worth
pinning is `per_caller[c] == seen.keys().filter(|(k, _)| k == c).count()`
after any sequence of `admit`/`release`.

---

## P3 findings

### §4 — an existing doc comment was orphaned onto the new constant

`src/flow/http402.rs:83-94`. The new `MAX_RESOURCE_BODY` const was inserted
between `/// The outbound paid-HTTP client.` and the item it described, so
that line now heads the constant (which has its own paragraph immediately
below it) and `pub struct X402HttpFlow` — a public type — is left with no doc
at all.

**Fix:** move the const above the orphaned line, or move that line back onto
the struct.

### §5 — `is_public_v6` covers every v4-in-v6 embedding family except Teredo, and the header's "fails closed" claim does not match the implementation

`src/http_policy.rs:219-257`.

The enumeration is genuinely thorough — mapped, translated, compatible,
well-known NAT64, local-use NAT64 and 6to4 are all refused, each with a
comment explaining that it carries an embedded v4 destination, and
`v4_in_v6_embeddings_are_refused_whatever_the_prefix` tests all of them. The
one member of that family not listed is **Teredo, `2001::/32`**, which encodes
the Teredo server's IPv4 address in bits 32–63 and the (obfuscated) client
IPv4 in the last 32 — the same "a v4 destination wearing a v6 address" shape
as the six that are refused, and natively supported on the platform this repo
primarily targets. The matching v4-side gap is `192.88.99.0/24` (6to4 relay
anycast), which `is_public_v4` (`:203-217`) admits.

Second, smaller point on the same function. The header says:

> The list is deliberately conservative — anything not recognised as globally
> routable unicast is refused, so a range this misses fails closed rather than
> open.

The implementation is the other way round: both `is_public_v4` and
`is_public_v6` are `!(blocklist of special-purpose ranges)`, so a range the
list misses is treated as public and fails **open** — which is why Teredo
passes today. The blocklist is good; the framing just claims an allowlist's
safety property.

**Fix:** add `first == 0x2001 && s[1] == 0x0000` (Teredo) and the 6to4 relay
`/24`, extend the embedding test with a Teredo spelling, and correct the
header sentence to describe a blocklist.

### §6 — the Windows reopen-and-fsync path on the billing log is never exercised by a test

`src/policy/file_mode.rs:389,402-411`.

`open_append_no_follow` requests `FILE_APPEND_DATA | READ_CONTROL | WRITE_DAC
| SYNCHRONIZE` and deliberately not `FILE_READ_DATA`; `BillingLog::append`
then does `write_all` followed by `sync_all` (`FlushFileBuffers`) through that
handle. No test covers that combination:

- `billing::tests::a_second_append_extends_the_log` reuses one `BillingLog`,
  so it only ever hits the *create* path;
- `file_mode::tests::appending_creates_then_extends_through_the_handle` does
  reopen, but never fsyncs;
- everything that exercises the reopen path meaningfully is `#[cfg(unix)]`.

This is the second-process-start path for every Windows deployment. I verified
by hand that it works — a scratch test constructing two successive
`BillingLog` values over one file, three signed appends, three lines read back,
passing on Windows 11 — so this is a coverage gap rather than a defect. But
the guarantee is currently untested, and the access mask is the kind of thing
that breaks quietly under a later edit.

**Fix:** a cross-platform test that drops the first `BillingLog` and appends
through a second one. It costs four lines and covers the branch on both
platforms.

---

## Nits

**N1 — the mesh quote handler's capability binding compares the request
against itself.** `src/flow/mesh.rs:133-139` passes `&claimed.capability` as
`verify`'s `served_capability`, so the check at `quote_request.rs:240` is a
tautology on the only wire path that uses it. This is not a hole — the field
is inside the signed transcript, and the provider then quotes exactly
`verified.capability` — and there is no out-of-band capability to compare
against on a service that serves all of them. But `verify`'s doc table sells
that field as "a request for a cheap tool cannot be replayed against an
expensive one", which is not what this call site gets from it. A line of
comment at the call site would stop the next reader trusting a check that
isn't one.

**N2 — `migrate_to_a_fresh_owner_only_file` is unreachable on Windows only
because `is_owner_only` is hardcoded `true` there.** `file_mode.rs:173-201`
does `existing.rewind()` + `read_to_end()`, which needs read access the
Windows `APPEND_AND_WRITE_DAC` mask (`:389`) does not grant. The dependency is
documented on `is_owner_only` (`:286-289`) but not on the function it
constrains, so a future real Windows `is_owner_only` would silently route into
a path that cannot work. A one-line note at `migrate_…` pins it.

**N3 — `is_valueless_chain`'s testnet list omits several common chains.**
`src/core/registry.rs:345-356` covers Base Sepolia, Sepolia, Goerli and
Holesky. Not listed: OP Sepolia (`11155420`), Arbitrum Sepolia (`421614`),
Polygon Amoy (`80002`). None are in the shipped registry today and the comment
argues the enumeration is deliberate, which is right — but the failure
direction it describes ("forgetting one leaves it listed") is precisely the one
that bites when a real network is added later, and these are free to add now.

---

## Verification

Ran on **Windows 11 Pro 10.0.26100**, rustc/cargo 1.97.1
(`x86_64-pc-windows-msvc`), from `net/crates/net/payments`.

| gate | result |
|---|---|
| `cargo check --all-features --tests` | clean |
| `cargo test --all-features` | **all green** — 138 lib + 29 integration binaries, 0 failed |
| `cargo fmt -p net-payments -- --check` | clean |
| `cargo clippy --all-features --lib --bins -- -D warnings` | **FAILS — 10 errors** (§B1) |
| `cargo clippy --all-features --all-targets -- -D warnings -A unwrap_used -A expect_used -A undocumented_unsafe_blocks -A multiple_unsafe_ops_per_block` | **FAILS — 6 errors** (§B1) |

**What did not run, and must before merge.** The Windows host skipped every
unix-gated test on the branch, and those are the tests for the branch's
file-permission guarantees:

| test | what it establishes |
|---|---|
| `file_mode::a_symlinked_path_is_refused` | `O_NOFOLLOW` atomicity |
| `file_mode::a_fifo_at_the_log_path_is_refused` | the regular-file check on the handle |
| `file_mode::a_permissive_predecessor_is_migrated_to_a_fresh_file` | migration, not chmod — the module's central claim |
| `file_mode::an_already_restricted_log_is_not_migrated` | the ordinary path does not rewrite on every start |
| `billing::appending_through_a_symlinked_log_path_is_refused` | the append writes through the secured handle |
| `tests/read_only_writes_audit.rs` | whole file `#![cfg(unix)]` — **0 tests ran** |
| `tests/redeem_denial_no_write.rs` | **0 tests ran** |

`tests/live_testnet_conformance.rs` reported 0 passed / 4 ignored, which is
expected (it needs live testnet credentials).

Also not exercised anywhere: the clippy job runs `ubuntu-latest`, so the four
Windows-only `unsafe` FFI lints in §B1 — and the `windows_impl` module
generally, which is this crate's entire raw-Win32 surface — have no CI
coverage at all.

---

## Closure

_Empty — no fixes landed against this document yet._

| finding | disposition | commit |
|---|---|---|
| B1 | open | — |
| §1 | open | — |
| §2 | open | — |
| §3 | open | — |
| §4 | open | — |
| §5 | open | — |
| §6 | open | — |
| N1 | open | — |
| N2 | open | — |
| N3 | open | — |
