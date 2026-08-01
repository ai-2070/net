# CODE REVIEW 2026-08-01 — Payments security branch (`security-payments`)

> **STATUS: ADDRESSED at `df54a696f`, NOT signed off.** Every finding below
> has a landed fix — see [Closure](#closure) for the commit against each.
> All three hygiene gates now pass and the suite is green (308 passed across
> 30 binaries).
>
> One disposition differs from what this document originally recommended,
> and is argued in place: §1's fix is not the bare `.no_proxy()` suggested
> here but a rule making a system proxy and a destination policy mutually
> exclusive, with `DestinationPolicy::Unrestricted` as the operator's
> explicit way to accept the trade. Fixing §B1 also turned up a defect no
> pass had noticed — `tracing_capture`'s constructor tripped
> `clippy::type_complexity` on the relaxed all-targets run, which is a gate
> CI runs and this document did not.
>
> **This review still has not exercised the branch's unix-specific
> file-permission claims.** `file_mode`'s symlink / FIFO / migration tests,
> the billing log's symlink test, and both `read_only_writes_audit.rs` and
> `redeem_denial_no_write.rs` are `#[cfg(unix)]` and ran **0 tests** on the
> Windows host both this pass and the fix pass ran on — and those are
> exactly the guarantees the `file_mode` module was written to establish. A
> Linux/macOS run is required before merge, and it is not dischargeable by
> the author of the fixes. See [Verification](#verification).

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

**Disposition: FIXED (`e61641a5a`).** Both `expect`s are gone rather than
annotated — `BillingLog::append` takes the `&mut` back from
`Option::insert`, and `release_reservation` removes the record up front and
puts it back when a holder remains, so neither has a "look it up, then
assert it is still there" step. The doc lint was one cause and one reword.
On the Windows side `CreateFileW` no longer takes `&mut`, the SDDL cast is
gone, and the two multi-operation `unsafe` blocks are split so each
operation carries its own justification.

Fixing this turned up an eleventh error the review missed:
`tracing_capture::FieldCapture::new`'s return type tripped
`clippy::type_complexity` on the relaxed all-targets run — also a CI gate,
also branch-added. Fixed with a `CapturedFields` alias in the same commit.

**The Windows clippy leg is NOT done** and is deliberately left open: it is
a CI change, not a code change, and the four lints above are only the ones
that exist today. Filed as a follow-up rather than folded in here.

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

**Disposition: FIXED (`d02d6ad08`), by a different route than recommended.**
Not a bare `.no_proxy()` and not a new flag: the two are made **mutually
exclusive**. A policy that restricts anything gets `no_proxy()`; an operator
who needs an egress proxy asks for `DestinationPolicy::Unrestricted`, which
already meant "the operator's choice is the policy" and now also means "and
this client's destinations are the proxy's business, not ours". That puts
the trade at the call site rather than in the environment, and needs no API
surface that did not already exist. The rule is a named predicate
(`honours_system_proxy`) so it is testable and so the omission stays
visible, which is the module's stated standard.

Tested at both levels: a predicate test for the rule, and
`tests/http_policy_proxy.rs` for the behaviour end to end. That file gets
its own process because `set_var` is global and cargo runs a binary's tests
on parallel threads — a proxy variable set in a unit test would leak into
every test beside it. Confirmed to fail without the fix: the request goes
to the configured proxy and the refusal names `proxy.invalid` rather than
the target host. The module header now says where the resolver argument
stops holding.

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

**Disposition: FIXED (`1402981fa`).** Delegated; the function is now one
line over the shared rule. The test that pinned the weaker behaviour is
replaced by one that pins the rule, plus the v4-in-v6 loopback spelling
(`http://[::ffff:127.0.0.1]/x`) which the old hand-rolled check refused —
correctly, but for the wrong reason, since it never normalized.

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

**Disposition: FIXED (`a31597700`).** Exactly that. The test pins the
invariant rather than the symptom: four live nonces fit a share of four
after one replacement, the fifth is refused for the real reason
(`CallerReplayQuotaExhausted`, not a phantom), and one `release` frees
exactly one slot rather than the two the entry had been charged.

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

**Disposition: FIXED (`f49bdc256`).** Constant moved above; the line is back
on the struct.

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

**Disposition: FIXED (`6b8e97cc7`).** Both ranges refused, the embedding
test gained two Teredo spellings, and the header now says the enumeration
*is* the guarantee — including why an allowlist is not writable here
("globally routable" being the complement of a registry rather than a set
with a syntactic mark). The test also gained the two ranges' immediate
neighbours (`192.88.100.1`, `2001:4860:4860::8888`) in the *admitted*
direction, so neither new rule can quietly widen to the surrounding /16.

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

**Disposition: FIXED (`eda6eba18`).** Two appends through the reopened log,
so the open and the held handle are covered separately, asserting the
earlier record is extended rather than truncated. Green on Windows, which
is the platform the access mask is for.

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

**Disposition: all three FIXED (`df54a696f`).** N1 and N2 are comments at
the sites that depend on the facts, not at the sites that state them. N3
added all three chain ids, deliberately wider than the shipped asset list —
the moment they cost something is the moment someone adds an asset on one
and does not think to come back.

---

## Verification

Ran on **Windows 11 Pro 10.0.26100**, rustc/cargo 1.97.1
(`x86_64-pc-windows-msvc`), from `net/crates/net/payments`.

| gate | at review (`b629783ca`) | after fixes (`df54a696f`) |
|---|---|---|
| `cargo check --all-features --tests` | clean | clean |
| `cargo test --all-features` | green — 30 binaries, 0 failed | green — **308 passed**, 5 ignored, 0 failed |
| `cargo fmt -p net-payments -- --check` | clean | clean |
| `cargo clippy --all-features --lib --bins -- -D warnings` | **FAILS — 10 errors** | clean |
| `cargo clippy --all-features --all-targets -- -D warnings -A unwrap_used -A expect_used -A undocumented_unsafe_blocks -A multiple_unsafe_ops_per_block` | **FAILS — 7 errors** | clean |

> The all-targets count is 7, not the 6 this document originally recorded:
> `tracing_capture`'s `type_complexity` was present at review time and was
> missed here. It is fixed in `e61641a5a` with the rest.

**Build-environment note, so the next reader does not mistake it for a code
problem.** Several runs on this host produced `E0786 found invalid metadata
files for crate net` and a cascade of `internal compiler error` lines. The
root cause is in the message: `failed to mmap … The paging file is too small
for this operation to complete. (os error 1455)` — the machine ran out of
commit charge linking many test binaries at once. `cargo test -j 3`
reproduces green every time. Nothing to do with this branch.

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

The fix pass ran on the same host, so this gap is unchanged — and the two
findings whose fixes most want a unix run are §B1 (the `expect` removals
touch `BillingLog::append` and `release_reservation`, both on paths those
suites exercise) and §6 (whose new test is cross-platform but whose
motivation is the Windows mask).

Also not exercised anywhere: the clippy job runs `ubuntu-latest`, so the four
Windows-only `unsafe` FFI lints in §B1 — and the `windows_impl` module
generally, which is this crate's entire raw-Win32 surface — have no CI
coverage at all. **Still true after the fixes**: they are corrected in the
source but nothing in CI would catch the next one. See the follow-up below.

---

## Closure

Every finding has a landed fix. Gates green as of `df54a696f`; the unix run
in [Verification](#verification) is still outstanding and is what blocks
sign-off.

| finding | disposition | commit |
|---|---|---|
| B1 — clippy fails on production code | FIXED (+1 the review missed) | `e61641a5a` |
| §1 — env-proxy bypasses the destination policy | FIXED, different route | `d02d6ad08` |
| §2 — `is_payment_safe_url` admits the name `localhost` | FIXED | `1402981fa` |
| §3 — `SeenNonces` per-caller quota double-count | FIXED | `a31597700` |
| §4 — orphaned `X402HttpFlow` doc comment | FIXED | `f49bdc256` |
| §5 — Teredo / 6to4 relay; fail-closed claim | FIXED | `6b8e97cc7` |
| §6 — reopen-and-fsync untested | FIXED | `eda6eba18` |
| N1 — tautological `served_capability` | FIXED | `df54a696f` |
| N2 — Windows migration latent dependency | FIXED | `df54a696f` |
| N3 — testnet list | FIXED | `df54a696f` |

### Follow-ups, deliberately not folded in

1. **A Windows clippy leg.** This branch adds the crate's first
   `#[cfg(windows)]` `unsafe` FFI, and `runs-on: ubuntu-latest` means no job
   lints it. The four lints §B1 found are the ones that exist today; the
   point is that nothing would find the next four. This is a CI change and
   belongs in its own commit against `.github/workflows/ci.yml`.
2. **A unix CI run of the `#[cfg(unix)]` payment-store suites**, per
   [Verification](#verification). Not dischargeable by whoever wrote the
   fixes.
