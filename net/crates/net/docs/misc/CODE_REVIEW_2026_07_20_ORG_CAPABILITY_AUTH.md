# CODE REVIEW 2026-07-20 — Organization Capability Auth (`org-capability-auth`)

> **Status: all 20 production findings + §T1-§T9 + §D1-§D4 RESOLVED on this
> branch** (`1e18c4827` §1, `2776e545d` §2, `ffd4bcfdc` §3+§4, `e21a9d23c`
> §5+§D3, `73839135b` §6, `a5dc376e6` §7, `35c2849ed` §8, `8e2b85596` §9,
> `7b54c211d` §10+§11+§17, `b937e63e6` §12, `49ed592af` §13, `6b492b349`
> §14-§19, `087a90d07` §T1, `353115c72` §T9, `fc94c960e` §T2-§T8, §20 in
> this commit).
>
> **§20 was found by the user reviewing this closure, not by the original
> review.** It is a Windows CONFIDENTIALITY break that the §3/§4 fix left open:
> the validator tolerated read-only aces, but authority files inherit the
> directory's ACL on Windows, so an `(OI)(CI)(R)` grant reached
> `owner-audience.key`. Reproduced against live NTFS, then fixed and
> red-witnessed. The original §3/§4 witnesses missed it because both used
> write-capable aces — a reminder that "the tests pass" and "the property
> holds" are different claims, which is the same failure mode §T1-§T8
> catalogues elsewhere in this document.
>
> Every fix is RED-WITNESSED: each new test was verified to fail against the
> original code and pass against the fix, and the witness is recorded in the
> commit. The crate passes `cargo fmt`, both CI clippy gates (production-strict
> and `--all-targets` with the four panic lints allowed), 5210 lib tests, and
> every affected integration suite.
>
> **Three corrections to this document, found while fixing it:**
>
> 1. **§T1's mutation analysis was incomplete.** The caller-identity property
>    is defended TWICE — step 5's TOFU member bind and step 7's
>    `dispatcher_grant.dispatcher != *ctx.authenticated_caller`. Deleting
>    either alone still denies (verified). The review's claimed mutation is
>    dangerous precisely because it repoints `authenticated_caller`, which both
>    checks read; a test aimed at one check would have missed it. The new test
>    is the only one of 40 that fails under it.
> 2. **§T1's proof-expiry gap does not exist as stated.** `proof_ttl_secs` is
>    applied when the proof is MINTED, and `call()` mints fresh on every
>    invocation, so no already-expired proof can be presented through the
>    public API. Expiry is reachable only via a captured proof being replayed —
>    the attack the binding and replay guard exist to stop — and is covered in
>    the `org_admission` units, which take an explicit `ClockSample`. Replaced
>    with the caller-side TTL-ceiling test, which IS reachable.
> 3. **§12 was worse than described.** `test_inject_capability_announcement` is
>    not merely `#[doc(hidden)] pub` in Rust — it is re-exported through the
>    Python, Node and Go bindings as a callable method of the shipped
>    libraries. It was therefore fixed at the producer
>    (`verify_announced_owner_cert`) rather than by gating the seam, which
>    would have broken real callers.
>
> Two findings were also fixed more narrowly than proposed, deliberately:
> §2 keeps `--force` available (certs and floor bundles are renewable by
> design) and makes the replace SAFE via staging + atomic rename, rather than
> refusing it as the grant verbs do; §9 is defense-in-depth only and does NOT
> substitute for the §3.4 key rotation, which remains unimplemented.

Findings are ordered by severity; §1 and §2 were merge-blocking.

Review of branch `org-capability-auth` against `master`: 217 commits, 81 files,
+42,871/−1,082. The change lands the OA (Organization Capability Auth) subsystem
— 14 new `adapter/net/behavior/org_*.rs` modules (~18k lines), the
`org_admission_gate.rs` decision point, ~10k lines of `mesh.rs` / `mesh_rpc.rs`
live wiring, the `net org` CLI verb family, the `net_sdk::org` re-exports, and
~10k lines of tests (plan: `docs/plans/ORG_CAPABILITY_AUTH_PLAN.md`, with
`OA2E_INTEGRATION_DESIGN.md`, `OA3_EXIT_GATE.md`, `OA4_EXIT_GATE.md`).

## Method

Seven adversarial deep-readers were run in parallel over disjoint surfaces
(authority/crypto, admission gate + replay, scoped-announcement
confidentiality, revocation + capability fold, RPC transport wiring, CLI/SDK,
test quality), each instructed to discard any finding it could not reduce to a
concrete failure scenario. **Every finding below at Medium or above was then
re-verified against source by hand**; per-finding confidence is stated inline.

Toolchain gates all pass on this branch: `cargo check --all-targets`,
`cargo clippy --all-features --lib --bins -- -D warnings`, and
`cargo clippy --all-features --all-targets` with the four CI `-A` lints. The
only clippy output is a benign `panic setting is ignored for bench profile`
note from the new `benches/org_signature.rs`.

**The cryptographic core is clean.** Domain separation across the four signed
object types is provably non-aliasing (pairwise non-prefixing tags *and*
pairwise-distinct signing-input lengths); every signed payload is an injective
fixed-offset encoding with zero-fill-enforced optionals; `verify_strict`
everywhere; secrets carry `Drop` zeroization plus a compile-time
`assert_not_impl_any<Serialize>` guard; the `OrgCallProof` binding transcript
binds every field an attacker could otherwise substitute, and all
provider-supplied values are read from provider-local facts rather than from
the proof. No fail-open path exists in `verify_org_admission` itself. Two
adversarial passes failed to construct a substitution, transplant, or
cross-protocol reuse attack.

**Findings concentrate in the seams**, not the core. The dominant pattern —
worth naming because it predicts where the next defect will be — is the
**propagation gap**: a hazard is correctly identified, the fix is correctly
designed, and it is applied to the one path under review while its siblings are
left alone. §2 (guard on 2 of 4 CLI verbs), §8 (strip on 1 of 4 serve bridges),
§4 (validator contradicts its own reader one function away), and §10/§11 (the
new staging helper zeroizes and awaits cleanup; the older shared helper it
delegates to does neither) are all this same shape.

Severity summary (20 production findings; §20 added during closure review):

| # | Sev | Location | One-line |
|---|-----|----------|----------|
| 1 | **Critical** | `mesh_rpc.rs:2805`, `:3469` | Org-protected RPC responses roster-fan to any peer squatting the reply channel |
| 2 | **Critical** | `cli/…/org.rs:343`, `:397` | `issue-cert --force` / `issue-floors --force` can truncate the org root key |
| 20 | **High** | `org_authority.rs` `validate_dacl_view` | Untrusted INHERITABLE read ace propagates onto `owner-audience.key` (found in closure review) |
| 3 | **High** | `org_authority.rs:1693` | Windows authority-dir validator never checks the directory OWNER (implicit `WRITE_DAC`) |
| 4 | **High** | `org_authority.rs:1713` | Windows ACE loop fail-open on every non-type-0 ACE, including *allow*-callback/object |
| 5 | **High** | `org_admission.rs:523` | Replay retention derives from mutable skew; widening it re-admits already-used proofs |
| 6 | **High** | `org_scoped_ann.rs:219` | Unpadded ciphertext length discloses the private capability's name length |
| 7 | **High** | `org_scoped_relay.rs:141` | Fail-closed relay gate keyed on free-to-mint identities → mesh-wide 600 s discovery blackhole |
| 8 | Medium | `mesh_rpc.rs:3425` | `strip_public_admission_header` wired into 1 of 4 public serve bridges |
| 9 | Medium | `org_scoped_ingest.rs:349` | Revoking a member does not revoke owner-audience read access |
| 10 | Medium | `cli/…/identity.rs:376` | Org root seed copied into a plain `Vec<u8>` that drops un-zeroed |
| 11 | Medium | `cli/…/identity.rs:400` | Rename failure orphans a seed-bearing temp file; cleanup is detached and never runs |
| 12 | Medium | `capability_bridge.rs:246`, `mesh.rs:22441` | Two release-compiled seams install ownership projections outside verified ingest |
| 13 | Medium | `org_revocation.rs:1506` | `is_poisoned()` does a full `canonicalize()` per call, including under the interprocess lock |
| 14 | Low | `org_revocation.rs:129` | `merge_bundle` persists no-op zero floors; install sweep takes one exclusive fold lock per entry |
| 15 | Low | `capability_bridge.rs:417` | Ownership retraction emits no `FoldAuditSink` event |
| 16 | Low | `cli/…/org.rs:260` | `--insecure-permissions` overloaded across two unrelated gates |
| 17 | Low | `cli/…/org.rs:1131` | Least-informed failure path steers the operator toward the clobbering flag |
| 18 | Low | `org_grant_registry.rs:253` | Non-constant-time `==` on two raw `discovery_key`s |
| 19 | Low | `cli/…/node.rs:239` | `default_authority_dir()` falls back to CWD; unguarded on Windows |

Test-quality findings are §T1–§T8; documentation-accuracy findings are §D1–§D4.

Paths are relative to `net/crates/net/src/adapter/net/behavior/` unless noted
(`mesh.rs`, `mesh_rpc.rs`, `org_admission_gate.rs`, `cli/`, `sdk/`, `tests/`
are relative to `net/crates/net/`).

---

## 1. Critical — Org-protected RPC responses roster-fan to a squatting subscriber

**Location:** `mesh_rpc.rs:2801-2806` (`publish_response_to_caller`, the
`RosterOnStaleDirect` arm) reached from `mesh_rpc.rs:3459-3469`
(`serve_rpc_unary_impl`'s response drain).
**Confidence: CONFIRMED** — traced end to end by hand.

### Current shape

`serve_rpc_protected` (`mesh_rpc.rs:3005`) delegates to `serve_rpc_unary_impl`,
which is the *same* impl that serves public unary registrations. Its response
drain hardcodes:

```rust
publish_response_to_caller(
    …,
    ResponseRouteFallback::RosterOnStaleDirect,   // mesh_rpc.rs:3469
)
```

`publish_response_to_caller` unicasts to the resolved direct session when it has
one. Otherwise — and also when the resolved node returns
`PeerPublishOutcome::NoSession` at send time (`:2776`) — it falls through to:

```rust
let publisher = ChannelPublisher::new(reply_channel.clone(), PublishConfig::default());
mesh.publish(&publisher, payload).await.map(|_| ())   // mesh_rpc.rs:2805
```

on `<service>.replies.<caller_origin>`. Nothing binds a *subscriber* of that
channel to the origin encoded in its name:

- `MeshNode::authorize_subscribe` (`mesh.rs:18534`) never parses the channel
  name against the requesting peer's pinned origin; with no auth gates
  configured it returns allow at `mesh.rs:18622`.
- The SDK auto-registers `<service>.replies.` as a **default-permissive prefix**
  (`sdk/src/mesh_rpc.rs:276-294`) whose own comment reads *"admits every
  per-caller `<service>.replies.<caller_origin>` subscribe."* `ChannelConfig::new`
  sets `publish_caps: None, subscribe_caps: None, require_token: false`, and
  `ChannelConfigRegistry::get_by_name` resolves every per-caller channel to it
  by longest-prefix fallback.
- `MeshNode::publish`'s recipient `retain` (`mesh.rs:19052`) applies subnet, the
  AuthGuard cache that `authorize_subscribe` itself populated, and a token check
  skipped outright when `!require_token` (`mesh.rs:19089`).

### Why this is a branch finding

The roster leg predates the branch (AV-5). What the branch adds is routing
**org-protected** responses through it, while asserting safety that does not
hold for them:

- `mesh_rpc.rs:2649` — *"Eviction is always safe: a miss just recomputes the
  value … or falls back to the roster lookup."*
- `mesh_rpc.rs:2617` — the LRU bound costs *"only a response-path cache miss
  (roster fallback), never correctness."*

For a `CrossOrgGranted` response, a cache miss is a confidentiality event, not a
performance event. `docs/misc/NRPC_DESIGN.md:477` separately claims reply
channels are *"naturally scoped to the caller's own origin (no other token has
the right to subscribe to your reply channel)"* — false as shipped on the
auto-registration path.

### Failure scenario

Two triggers, both reachable:

1. **Cache eviction.** The route cache is a `BoundedLru` capped at
   `RPC_CALLER_CACHE_CAP = 4096` (`mesh_rpc.rs:2631`). Past 4096 concurrent
   calls on one service, a victim's entry evicts; `get_node_by_origin_hash`
   also misses (it is populated only from signed announcements); control reaches
   `:2805`.
2. **Session loss.** The caller's session is gone at send time —
   `PeerPublishOutcome::NoSession` at `:2776`, which for `RosterOnStaleDirect`
   falls through rather than dropping.

Attacker A calls `subscribe_channel(S, "svc.replies.<V_origin>")` and is
accepted. On either trigger, V's protected response body is published to the
channel roster, which includes A.

**The branch's own test proves the precondition.** `tests/nrpc_streaming_gate.rs:254`
performs exactly this squat subscribe and asserts it succeeds
(`"bystander subscribes to caller's reply channel"`). It then asserts only that
*denials* stay private — which they do, because denials correctly use
`ResponseRouteFallback::DirectOnly` (`mesh_rpc.rs:823`, `:881`). Successful
responses were never covered.

On the public path the same leak exists with a weaker precondition (a caller
that has not yet been TOFU-pinned never populates the cache at all, since
`response_route_is_trustworthy` at `:566` requires
`authenticated_peer_origin == Some(claimed_origin)`).

### Fix direction

Give protected registrations `ResponseRouteFallback::DirectOnly` — they already
require a pinned direct session, so the roster leg buys them nothing — and/or
bind subscribe authorization for `*.replies.<hex>` to the requesting peer's own
pinned origin. The second is the durable fix and also repairs the public path
and the `NRPC_DESIGN.md` claim.

---

## 2. Critical — `issue-cert --force` / `issue-floors --force` can destroy the org root key

**Location:** `cli/src/commands/org.rs:343` (`run_issue_cert`), `:397`
(`run_issue_floors`) → `refuse_existing` (`:1121`) → `write_json_envelope`
(`:1138`).
**Confidence: CONFIRMED** — read by hand.

### Current shape

```rust
async fn refuse_existing(path: &Path, force: bool) -> Result<(), CliError> {
    if force {
        return Ok(());          // org.rs:1122 — no stat, no alias check
    }
    …
}

async fn write_json_envelope<T: Serialize>(path: &Path, value: &T) -> … {
    …
    tokio::fs::write(path, json).await …   // org.rs:1151 — truncates, follows symlinks
}
```

Neither verb calls `refuse_force` (`:828`) or `refuse_aliased_paths` (`:934`),
and neither routes through `stage_beside` / `publish_staged`. The grant verbs do
all three (`:426`, `:455`, `:485`, `:531`).

### Failure scenario

```
net org issue-cert --org-key ~/.config/net-mesh/orgs/org-ab12cd34.toml \
                   --member <64hex> \
                   --out    ~/.config/net-mesh/orgs/org-ab12cd34.toml --force
```

The org root seed file is truncated and replaced with ~300 bytes of cert JSON.
The org root is unrecoverable: no node can be re-certified, **no revocation
floor can ever be issued again**, and every outstanding membership cert stays
valid until natural expiry (up to `MAX_ORG_CERT_TTL_SECS`, ~2 years) with no
revocation path. Realistic triggers: a provisioning script where `$ORG_KEY` and
`$CERT_OUT` are adjacent variables; an operator who added `--force` for a
re-issue and later changed `--out`.

### Why this is a propagation gap, not an unknown hazard

The authors identified this exact scenario and wrote it into both the help text
(`:186-189`) and `refuse_force`'s doc comment (`:823-837`):

> *"on a case-insensitive filesystem, an aliased `--out` (e.g. `ORG.TOML` vs
> `org.toml`) could destroy the org key."*

…then applied the guard to `grant-dispatcher` and `grant-capability` only.
`cli/tests/org_grant.rs` has six tests covering force/alias/case-variant/symlink
for the grant verbs; `cli/tests/org_adopt.rs:62` covers only "refuses to clobber
*without* `--force`" for `issue-cert`.

### Fix direction

Route `issue-cert` and `issue-floors` through `refuse_force` +
`refuse_aliased_paths` + `stage_beside`/`publish_staged`, identically to the
grant verbs. Add the four missing negative tests, asserting **stderr content**
and not merely exit code (see §T4).

---

## 20. High — An untrusted INHERITABLE read ACE propagates onto `owner-audience.key`

**Location:** `org_authority.rs` (`validate_existing_dir_dacl` / `validate_dacl_view`,
the `mask & WRITE_MASK == 0 → continue` arm), with
`org_revocation.rs` `write_atomic_phased`.
**Confidence: CONFIRMED** — reproduced against live NTFS.
**Reported by the user during closure review; the §3/§4 fix did not close it.**

### Current shape

The validator tolerated any ACE with no write bits: *"a read-only grant to
anyone is tolerated."* That reasoning holds for a directory in isolation — read
access confers `FILE_LIST_DIRECTORY` and the authority file names are
compile-time constants.

It does not hold once inheritance is considered. `write_atomic_phased` sets
`mode(0o600)` under `#[cfg(unix)]` **only**; there is no Windows
explicit-DACL branch, so on NTFS every provisioned authority file gets whatever
it INHERITS from the directory. An `OBJECT_INHERIT` ACE therefore propagates
onto `owner-audience.key` — the raw owner discovery key, which decrypts every
`OwnerScoped` announcement for the org.

### Failure scenario

A directory carrying `D:P(A;OICI;FA;;;<owner>)(A;OICI;FR;;;WD)` — owner full
control plus **Everyone read**, both inheritable, no write bit for Everyone.
Measured on Windows 11 before the fix:

```
validator_accepted=true  adopt_ok=true  everyone_can_read_audience_key=true
```

The `§3`/`§4` witnesses missed it because both used **write-capable** ACEs,
which the write-mask check caught for unrelated reasons. The
`adopt_windows_authority_dir_and_files_are_owner_only` test also missed it: it
only ever inspects a directory `adopt` itself created, which is protected and
owner-only by construction.

This makes the plan's "owner-only authority directory" claim false, and it is a
CONFIDENTIALITY break rather than an integrity one — the attacker never needs to
write anything.

### Fix

Check inheritance FIRST, before any read/write distinction: any untrusted ACE
carrying `OBJECT_INHERIT` or `CONTAINER_INHERIT` is refused whatever it grants.
A non-inheriting read ACE stays tolerated, and the reason is now stated
explicitly rather than assumed.

### Residual (not closed here)

Authority files still depend on **directory inheritance** for their ACL on
Windows rather than being created with an explicit per-file security
descriptor. With the owner check (§3) and this rule in place the chain holds —
only a trusted principal has `WRITE_DAC` to change the directory ACL after
validation — but the stronger fix is `CreateFileW` with an explicit
`SECURITY_ATTRIBUTES` in `write_atomic_phased`. Deliberately not attempted in a
closure pass: it is real `unsafe` FFI on the write path shared with the
revocation store, and create-then-tighten is NOT an acceptable substitute (it
leaves the bytes readable for a window, exactly the pattern the Unix side
avoids).

---

## 3. High — Windows authority-dir validator never checks the directory OWNER

**Location:** `org_authority.rs:1693-1731` (`validate_existing_dir_dacl`); the
load-bearing incorrect claim at `:1334-1338`.
**Confidence: CONFIRMED** — `owner_sid`'s only consumer is a test.

### Current shape

`DaclView` populates `owner_sid` (`:1425`, `:1443`, `:1479`) but marks it
`#[allow(dead_code)]` with:

```rust
/// Owner SID string — inspected by the witnesses (owner-is-trusted); the
/// production validator reasons about the DACL, not the owner (a foreign
/// owner cannot itself grant access — the DACL governs that).
```

That last clause is false on Windows. An object's owner is implicitly granted
`READ_CONTROL` and `WRITE_DAC` on every access check unless an `OWNER RIGHTS`
(`S-1-3-4`) ACE is present — and none is required or checked here. The
`trusted(&dir_view.owner_sid)` assertion exists **only** in the test at `:2534`,
and only against a directory `adopt` just created.

### Failure scenario

Multi-user Windows host, custom `--authority-dir`. `C:\ProgramData` grants
`BUILTIN\Users` create-folder and `CREATOR OWNER` full control on subfolders by
default:

1. Low-privileged *Mallory* creates `C:\ProgramData\net-authority` — she is now
   its **owner**.
2. She sets a protected DACL containing exactly one ACE: `victim:(OI)(CI)F`.
   Her own explicit ACE is gone.
3. *Victim* runs `net node adopt --authority-dir C:\ProgramData\net-authority`.
   `ensure_secure_authority_dir` (`:1649`) → `validate_existing_dir_dacl` walks
   the single ACE, finds `ace.sid == user_sid`, returns `Ok`. Adoption
   provisions `owner-audience.key`, `owner-membership.json`, and
   `revocation-state.json` into it.
4. Mallory, still owner, uses implicit `WRITE_DAC` (`Set-Acl`) to grant herself
   full control, then reads the raw 32-byte owner discovery key — which decrypts
   **every** `OwnerScoped` announcement for that org — and can substitute
   `revocation-state.json` with lower floors.

Plan §13's contract ("a pre-existing directory is re-validated against its
BINARY DACL and fails closed unless every write-capable ACE grants only a
trusted principal") does not hold, because ownership is a write-capable path
that never appears as an ACE. The Unix side gets this right —
`authority_dir_policy_violation` (`:969`) checks `owner_uid != euid` first.

### Fix direction

Require `view.owner_sid` to be a trusted principal in
`validate_existing_dir_dacl` — i.e. promote the `:2534` test assertion into
production.

---

## 4. High — Windows ACE loop is fail-open on every non-type-0 ACE

**Location:** `org_authority.rs:1711-1715`.
**Confidence: CONFIRMED** — the reader and the validator contradict each other.

### Current shape

```rust
for ace in &view.aces {
    // Only ALLOWED ACEs (type 0) grant access; a DENY cannot broaden it.
    if ace.ace_type != 0 {
        continue;                       // org_authority.rs:1713
    }
    if ace.mask & WRITE_MASK == 0 { continue; }
    if ace.sid != user_sid && … { return Err(InsecureAuthorityDir { … }); }
}
```

Type 0 is not the only access-*granting* ACE type. `ACCESS_ALLOWED_OBJECT_ACE`
(5), `ACCESS_ALLOWED_CALLBACK_ACE` (9), and
`ACCESS_ALLOWED_CALLBACK_OBJECT_ACE` (11) all grant access and are all silently
skipped.

This directly contradicts the module's own reader, one function away
(`:1463-1470`):

```rust
// The SID begins at byte 8 for the SIMPLE ACE types only; other
// (e.g. object) ACEs place it elsewhere, so record a sentinel and
// let the validator treat a write-capable one conservatively.
let sid = if ace_type == 0 || ace_type == 1 {
    sid_to_string(base.add(8).cast())?
} else {
    String::from("<non-simple-ace>")
};
```

`read_object_security` dutifully records the sentinel so the validator can be
conservative; the validator's first line drops the ACE before it ever reads the
mask or the sid.

### Failure scenario

Anyone who can set the DACL on the candidate directory (its owner — see §3 — or
a prior aborted run) applies a *conditional* ACE via SDDL rather than
`icacls /grant`:

```powershell
$sd = New-Object Security.AccessControl.DirectorySecurity
$sd.SetSecurityDescriptorSddlForm('D:P(XA;OICI;FA;;;WD;(Member_of{SID(WD)}))')
Set-Acl C:\ProgramData\net-authority $sd
```

`XA` emits an `ACCESS_ALLOWED_CALLBACK_ACE` (type 9) granting `Everyone` (`WD`)
full control under a condition true for every token — so Windows' access check
grants Everyone full control on the directory and, via `OI|CI`, on every file
created in it. `validate_existing_dir_dacl` sees `ace_type == 9`, `continue`s,
finds no other write-capable ACE, and returns `Ok`. `adopt` then writes
`owner-audience.key` into a world-readable, world-writable directory.

The existing Windows witness at `:2607` exercises only
`icacls /grant *S-1-1-0:(OI)(CI)F`, which produces a **type-0** ACE — so it
passes while this variant slips through.

> The code defect is unambiguous and is the finding. The specific SDDL above is
> illustrative: it was not executed on a host during this review.

### Fix direction

Invert the filter — `continue` only on the known *deny* types (1, 6, 10, …), and
treat every other non-type-0 ACE with `mask & WRITE_MASK != 0` as a refusal,
since its SID could not be parsed. Add a witness using `XA`.

---

## 5. High — Replay retention derives from mutable skew, re-opening admitted proofs

**Location:** `org_admission.rs:522-525`; the enabling branch at
`org_admission_replay.rs:285-292`.
**Confidence: CONFIRMED** — arithmetic and both branches read by hand.

### Current shape

```rust
let skew_ns = ctx.skew_secs.saturating_mul(1_000_000_000);
let retain_until_wall_ns = proof.proof_expires_at_unix_ns.saturating_add(skew_ns);
let expires_at = clock.monotonic_deadline_for(retain_until_wall_ns);
```

Retention ends at wall `expiry + skew_at_admission_time`. Freshness
(`org_call.rs:296-311`) accepts while `now < expiry + skew_in_force_now`. These
are the *same variable read at different times* — `facts.skew_secs` is re-read
from the live authority on every call (`org_admission_gate.rs:255`). The
design's stated invariant ("unexpired keys never evicted", plan §2.5 / Locked
#8) therefore holds only while skew is constant.

The enabling branch treats an expired entry as reusable:

```rust
if let Some(existing) = inner.get(&call_id) {
    if existing.expires_at > now { /* Replay | CallIdCollision */ }
    inner.insert(call_id, ReplayEntry { binding_digest, expires_at });
    return ReplayOutcome::Admitted;      // org_admission_replay.rs:292
}
```

### Failure scenario

Provider P runs with `verification_skew_secs = 0` (the `#[serde(default)]`,
`org_authority.rs:117`). Legitimately-credentialed caller S issues a protected
call at wall `T`, proof expiry `T+30`; the guard entry is retained to monotonic
`M+30`. S keeps the exact frame and re-sends periodically (all denied
`ProofExpired`). An operator later runs `net node adopt --skew-secs 300` after a
clock-drift incident and calls `install_node_authority` — a supported same-org
renewal, runtime-installable, no restart, guard not cleared. S's next resend at
`T+200`:

- guard entry expired at `M+30` → expired-overwrite branch → `Admitted`;
- freshness: `T+200 < T+30+300` → fresh;
- TTL ceiling: `T+30 <= T+200+30+300` → OK;
- membership / dispatcher / capability grants (day–week windows), floors,
  binding, `has_local_capability`, provider self-verify, §9.5 stamp — all
  unchanged, all pass. The §9.5 recheck detects changes *during* one admission,
  not *between* two.

The handler runs a **second time** on the same signed proof and `call_id`, with
`Admitted` attribution. The fold's duplicate-REQUEST guard
(`cortex/rpc.rs:1825`) covers only *in-flight* calls, so the first call having
completed is precisely the enabling condition. Exposure window is
`(expiry + skew_old, expiry + skew_new)` — up to 300 s per proof, for every
proof admitted under the old skew.

A second, non-attacker-controlled trigger shares the root cause: retention is
anchored on `Instant` while freshness reads wall clock. A backward wall step
(NTP correction after the clock ran fast) makes monotonic elapse more than wall,
expiring the entry while the proof is still fresh and still under the TTL
ceiling. `admission_clock.rs`'s doc claims to close this; it closes only the
*intra*-admission case.

### Fix direction

Retain to `expiry + MAX_TOKEN_CLOCK_SKEW_SECS` (the hard ceiling,
`identity/token.rs:913`) rather than `expiry + ctx.skew_secs`, so retention
always dominates any acceptance window a future skew could produce.

### Related hardening observation (not filed as a finding)

`check_expiry_at` applies skew to *both* the expiry check and the TTL ceiling,
so the effective maximum proof lifetime is `MAX_ORG_PROOF_TTL_SECS + 2×skew` —
630 s at the permitted skew ceiling, against the plan's stated 30 s intent.
Retention currently tracks that window exactly, so it is not itself a hole.

---

## 6. High — Unpadded ciphertext length discloses the private capability's name length

**Location:** `org_scoped_ann.rs:219-256` (`seal_descriptor`), with the
cleartext `ct_len` at fixed offset 353 (`:613`).
**Confidence: CONFIRMED** for the leak; dictionary exploitability is
strong-PLAUSIBLE and depends on naming conventions.

### Current shape

`seal_descriptor` passes the descriptor plaintext straight to the AEAD with no
padding, so `ciphertext_len == plaintext_len + 16`. The file's own golden
vectors pin the arithmetic: `b"golden-descriptor"` (17 B) → `ct_len = 0x0021`
(33); `b"owner-golden-descriptor"` (23 B) → `0x0027` (39).

The OA3-4b2 tightening canonicalized the *granted* plaintext to **exactly one**
`nrpc:<svc>` tag (`descriptor_binds_grant_capability`), collapsing the plaintext
space to a bijection with the tag string.

### Failure scenario

Relay R is in no audience. From a granted envelope it reads in cleartext:
`provider` (offset 1), `owner_org` (offset 33), `grant_id`, `audience_handle`,
and `ciphertext_len` (offset 353). Because the plaintext is
`[0x01] ++ postcard(CapabilitySet{tags:[t], metadata:{}})`, R inverts to
`len(t)` exactly, hence `len(service_name) = ct_len − 25` exactly — for a
**named** provider in a **named** org. Against any candidate service-name
dictionary that is a strong filter.

For the owner envelope (one envelope carries all owner-scoped tags,
`mesh.rs:8071`), R watches `ct_len` across successive generations and reads off
the exact byte-length of each newly registered private service, and how many
exist — the counting channel, live.

Plan §3.2 asserts observers learn *"size — nothing matchable."* That was written
before the single-tag canonical bind existed; the bind made size matchable and
the claim was never revisited (see §D2).

### Fix direction

Pad inside the AEAD (length prefix + pad to a fixed bucket) in
`seal_descriptor`. `MAX_SCOPED_ANN_CIPHERTEXT_BYTES` is const-asserted > 512
with ~5.7 KB of headroom, so a fixed 256-byte padded plaintext is free against
the 8 KiB budget.

---

## 7. High — Fail-closed relay gate keyed on free-to-mint identities

**Location:** `org_scoped_relay.rs:133-149` (`ScopedAnnRelayGate::admit`),
`:213-221` (`RelayDedupKey` construction), with `org.rs:203`.
**Confidence: CONFIRMED.**

### Current shape

```rust
const MAX_ENTRIES: usize = 8192;
const RETENTION_SECS: u64 = 600;

if seen.len() >= Self::MAX_ENTRIES {
    // FAIL-CLOSED at capacity: never evict an active seen-key …
    return false;                        // org_scoped_relay.rs:145
}
```

`RelayDedupKey` is `{ provider, grant_id, audience_handle, generation }` — all
authenticated only by a **self-declared** provider key. `decide_scoped_relay`
returning `None` suppresses both the forward **and** the local
`ingest_scoped_announcement`.

### Failure scenario

M is any session-authenticated peer of V. M generates 8192 fresh
`EntityKeypair`s and builds one envelope per key:

- `OrgMembershipCert::from_bytes` (`org.rs:203`) does **not** verify — any 156
  bytes decode;
- `ScopedCapabilityAnnouncement::from_bytes` verifies only the outer signature,
  against the provider id M chose;
- M frames each at `hop_count = 1`, which skips the hop-0 direct-origin bind
  (`org_scoped_relay.rs:205`);
- M picks a far-future `expires_at` to clear the coarse expiry check.

All 8192 are distinct keys and all are admitted. Then `seen.len() >= 8192` and
every subsequent legitimate envelope hits `return false` → V is dark for
`RETENTION_SECS = 600`. Cost: ~3.6 MB, i.e. ~6 KB/s sustained.

It is worse than per-adjacency: every fill-phase envelope is admitted and
therefore **forwarded** by `forward_scoped_announcement` (`mesh.rs:17837`) to
all peers-except-ingress at `MAX_CAPABILITY_HOPS = 16`, so one 3.6 MB burst
fills the gate across the entire connected mesh. There is no per-peer rate limit
on `SUBPROTOCOL_SCOPED_CAPABILITY_ANN` (`mesh.rs:11631` calls
`decide_scoped_relay` directly per event).

Secondary amplification: at capacity, `seen.retain()` (`:137`) walks 8192
entries under a `parking_lot::Mutex` on the inbound dispatch path for *every*
inbound envelope.

The gate's docs justify fail-closed on the grounds that eviction would restart
flood loops. That reasoning holds only if identities are expensive; they are
not, so fail-closed converts a cheap flood into a total confidentiality-plane
outage.

### Fix direction

Partition the gate per ingress peer with a per-peer sub-cap, and/or require a
`hop_count > 0` envelope's `owner_cert` to verify against a known org root
before it may occupy a slot.

---

## 8. Medium — `strip_public_admission_header` is wired into 1 of 4 public serve bridges

**Location:** `mesh_rpc.rs:3425` — the sole call site, in the unary
`PublicAuthenticated` arm.
**Confidence: CONFIRMED** (single call site by grep; the three streaming bridges
read in full).

`serve_rpc_streaming` (`:3646`), `serve_rpc_client_stream` (`:3838`), and
`serve_rpc_duplex` (`:4171`) call `bridge_preflight` and then
`fold.lock().apply_inbound(&inbound)` directly — no strip. E1.6 and the
function's own doc comment (`:886-899`) state that a public or legacy handler
must **never** receive org-admission credential material; the invariant is
enforced on one bridge of four.

**Reachability is limited**, which is why this is Medium and not High: the
caller-side builder refuses to mint a proof on any non-unary path (`:3940`,
`:4264`, `:4390`, `:4687` all reject `org_proof_intent` as unary-only), and
`call()` rejects a caller-supplied proof header outright (`:4936-4949`). Only a
caller who hand-stuffs `net-org-admission` into `CallOptions.request_headers`
and issues `call_streaming` / `call_client_stream` / `call_duplex` reaches it,
and the credential is their own. Defense-in-depth gap and a stated-invariant
inconsistency, not a live third-party leak.

**Fix direction:** hoist the strip into `bridge_preflight`, so it cannot be
omitted per-bridge again.

---

## 9. Medium — Revoking a member does not revoke owner-audience read access

**Location:** `org_scoped_ingest.rs:349-394` (`verify_owner_ingest`), with
`org_authority.rs:791`, `:888`.
**Confidence: CONFIRMED** as code behavior.

The owner `discovery_key` is a symmetric key shared org-wide by file
distribution. `verify_owner_ingest` checks the **publishing provider's** cert
against `ctx.floors` and never consults whether the **local reader** is still a
member in good standing.

**Scenario.** Node M ∈ org B holds `owner-audience.key`. B revokes M by
publishing an `OrgRevocationBundle` raising M's floor — the only revocation
primitive actually wired. Every other B node now refuses M's announcements and
M's invocations. But M's key and the owner `audience_handle` are unchanged, and
nothing in `verify_owner_ingest` looks at M's own cert, so **M keeps ingesting
and storing every owner-scoped announcement from every remaining B node** — the
full name list of B's internal private capabilities — indefinitely. Closing it
requires an operator to hand-distribute a new `owner-audience.key` to every node
in B (the §3.4 hard cutover), which no code path triggers or even flags.

`OA3_EXIT_GATE.md`'s "raw decryption" bullet acknowledges that retained key
material can open *historically captured* ciphertext. This is a different thing:
continued acceptance of *future* announcements through the live ingest
authority.

**Fix direction:** `ScopedIngestContext` already carries `floors`; add the local
node's own cert generation and refuse ingest when the local node is itself
floored. Cheap defense-in-depth that also gives operators a signal that a
rotation is due.

---

## 10. Medium — Org root seed copied into a plain `Vec<u8>` that drops un-zeroed

**Location:** `cli/src/commands/identity.rs:376`.
**Confidence: CONFIRMED** — read by hand.

```rust
let bytes_owned = bytes.to_vec();   // plain Vec<u8>, moved into spawn_blocking, freed un-zeroed
```

`run_keygen` (`org.rs:304-316`) wraps the serialized seed TOML in
`ScrubbedString` specifically so it scrubs on every exit path, then hands
`toml_text.as_bytes()` to `write_identity_atomically` — which immediately
duplicates it into a plain `Vec<u8>`. The entire scrub ceremony in `org.rs` is
defeated by the one function it delegates the write to. Same gap applies to
`net identity generate`.

The contrast is decisive, and is the propagation pattern again: the *new*
staging helper on this branch gets it right — `org.rs:988` does
`let payload = ScrubbedBytes::new(bytes.to_vec());`.

**Scenario.** After `net org keygen`, the 64-hex org root seed remains in freed
heap memory. A core dump (`ulimit -c` unset on many distros), a swapped page, or
heap reuse into a buffer later written out discloses the org root — which signs
every membership cert, every revocation floor, and every dispatcher/capability
grant for the org.

**Fix direction:** `let bytes_owned = ScrubbedBytes::new(bytes.to_vec());`,
promoting `ScrubbedBytes` out of `org.rs` into a shared module.

---

## 11. Medium — Rename failure orphans a seed-bearing temp file; cleanup never runs

**Location:** `cli/src/commands/identity.rs:400-410`.
**Confidence: CONFIRMED** — read by hand.

```rust
tokio::fs::rename(tmp, final_path).await.map_err(|e| {
    let tmp_for_cleanup = tmp.to_path_buf();
    tokio::spawn(async move { let _ = tokio::fs::remove_file(tmp_for_cleanup).await; });
    generic(format!("rename identity tmp {} -> {}: {e}", …))
})?;
```

The cleanup is spawned, not awaited. This is a one-shot CLI: the error
propagates straight out of `dispatch` and the process exits, so the detached
task almost never gets scheduled.

**Scenario.** `net org keygen --out /mnt/vault/org.toml` where the destination
is on a full or read-only-remounted filesystem. The temp write succeeds,
`rename` fails, and `/mnt/vault/org.tmp.<pid>` is left holding the seed. The
operator sees only *"rename identity tmp … failed"* — nothing says a seed file
was orphaned. On Windows `enforce_strict_permissions` is a no-op and there is no
`mode(0o600)`, so the orphan carries the parent's inherited DACL.

Again the new code gets this right: `org.rs:1067` `remove_file_or_warn` is
synchronous, awaited, and warns loudly with the exact path — documented at
`:1063` as *"never silently ignored."*

**Fix direction:** await the removal and route it through `remove_file_or_warn`.

---

## 12. Medium — Two release-compiled seams install ownership projections outside verified ingest

**Location:** `fold/capability.rs` (`VerifiedOwner::new` docs),
`fold/capability_bridge.rs:246` (`apply_legacy_announcement`),
`mesh.rs:22441` (`test_inject_capability_announcement`).
**Confidence: CONFIRMED** — both verified by hand; neither carries `#[cfg(test)]`.

`VerifiedOwner`'s docs claim the verification bridge is *"the only legitimate
producer, so a caller outside this crate cannot synthesize an unverified
'verified' projection."* That holds for `VerifiedOwner::new`. Two `pub` wrappers
weaken it:

**(a) `apply_legacy_announcement` (`capability_bridge.rs:246`)** — `pub`, and
this branch changed it to:

```rust
let verified_owner = verify_announced_owner_cert(&ann, outer_signature_verified, None, 0);
//                                                                              ^^^^ floors
```

`floors = None` means **every certificate generation is admissible** — the
revocation floor check is skipped entirely. All ~40 in-tree call sites are
inside `#[cfg(test)] mod tests` blocks (so the "~30 production call sites" claim
in `CODE_REVIEW_2026_05_23_MULTIFOLD_DEFERRED.md` MD-1 is now stale), but it
remains crate-public API.

**(b) `test_inject_capability_announcement` (`mesh.rs:22441`)** —
`#[doc(hidden)] pub` with **no** `#[cfg(test)]`, so it is compiled into release
builds. It applies to the node's *real* `capability_fold` and omits the
`ann.entity_id.node_id() != ann.node_id` binding that production dispatch
enforces at `mesh.rs:17526`.

(b) is the sharper of the two. `retract_floored_ownership` locates entries via
`member.node_id()` (`capability_bridge.rs:423`), which works only because ingest
guarantees `ann.node_id == ann.entity_id.node_id()`. A projection injected
through this seam under a **mismatched** `node_id` lands in
`by_node[fabricated_id]` while retraction, the install sweep, and the post-apply
recheck all search `by_node[entity.node_id()]` — so **no floor raise, no store
install, and no recheck can ever clear it**, and `owner_org_for(fold,
fabricated_id)` keeps returning the revoked org indefinitely.

**Fix direction:** gate both behind `#[cfg(any(test, feature = "test-seams"))]`;
at minimum, have `retract_floored_ownership` sweep on payload identity rather
than deriving the node key from the member.

> Broader note: the branch adds ~40 `#[cfg(test)]` seams inside production
> modules, several documented as bypasses (*"this bypasses packet ingress"*,
> *"a shipping build cannot carry the bypass"*). Those do compile out and are
> not a shipped vulnerability — but it is a materially larger test-only bypass
> surface than the plan's *"one `#[cfg(test)]`-only RED seam"* describes. See §D4.

---

## 13. Medium — `is_poisoned()` does a full path resolution per call, including under the interprocess lock

**Location:** `org_revocation.rs:1506` (`poison_path_key` →
`std::fs::canonicalize`), called unconditionally by `is_poisoned` (`:1522`),
`mark_poisoned` (`:1512`), `clear_poison` (`:1608`).
**Confidence: CONFIRMED** (code fact). Severity is latency/blocking, **not** a
bypass.

The state being consulted is a process-local `HashSet<BackingId>` /
`HashMap<PathBuf, …>`. Reaching it costs a full path resolution: on Linux one
`lstat`/`readlink` per component; on Windows a `CreateFileW` +
`GetFinalPathNameByHandleW` — a real file open — per call.

**Held-lock stall (the sharp one).** `apply_bundle` calls `is_poisoned` at
`:1356` *inside* the block holding both `lock_state_file(path)` (the
interprocess `.lock` sidecar, `:1328`) and `self.core.reload` (`:1329`). On an
NFS/SMB or stalled-disk authority directory, that `canonicalize` blocks for the
filesystem timeout while holding the **cross-process** revocation lock — every
other process's `apply_bundle`, and every `StoreCore::publish` on that core
(hence every `barriered_generation()` / `snapshot_with_generation()` reader in
`verify_provider_authority`), blocks behind it. The module is careful to compute
the key *before* taking the poison registry lock; the outer locks were not
considered. The same pattern at `mesh.rs:7531` runs under `_pin`, freezing
publishes on two cores at once.

**Hot-path cost.** `org_admission_gate.rs:155`, `:223`, `:243` mean **3 path
resolutions per protected unary RPC**. `mesh.rs:16910` plus the post-verify
recheck mean 2 per inbound scoped-announcement envelope that clears the relay
gate. (Checked: `decide_scoped_relay` is ahead of it and is pure/no-I/O, and the
Ed25519 verify dominates, so this is *not* a meaningful remote amplification
vector — the held-lock case is the real problem.)

**Fix direction:** memoize the canonical key on `StoreCore` — it is computed
once per store, under the lock, where the file is known to exist. Correctness is
unaffected: `normalize_backing_path` (canonical parent + literal final
component) and `canonicalize(joined)` produce the same string on both Unix and
Windows, so the tombstone key never misses.

---

## 14. Low — `merge_bundle` persists no-op zero floors; install sweep locks per entry

**Location:** `org_revocation.rs:129-142`; sweep at `mesh.rs:7632-7648`.

```rust
let entry = self.floors.entry((bundle.org_id, member.clone())).or_insert(0);
if *floor > *entry { *entry = *floor; raised += 1; }
```

`or_insert(0)` materializes a key for **every** member named in a bundle,
including unchanged and zero floors. `floors` is never pruned, so
`revocation-state.json` accumulates semantically-null `floor: 0` entries
permanently (`issue_at` at `org.rs:784` caps only *per-bundle* size).

Compounding: the reconcile sweep iterates the whole snapshot and calls
`retract_floored_ownership` per entry, which takes `fold.with_state_mut` — an
exclusive `state.write()` (`capability_bridge.rs:417`, `fold/mod.rs:664`) —
**unconditionally**, before it even checks `by_node.get(&node_id)`.

**Scenario.** An operator ships bundles naming 2,000 members over the fleet's
lifetime. The next `install_node_authority` performs 2,000 sequential exclusive
acquisitions of the capability fold, most of which can retract nothing
(`owner.generation() < 0` is unsatisfiable for `u32`), stalling every concurrent
`may_execute`, `has_local_capability`, and discovery query on the node.

**Fix direction:** skip zero floors in the sweep; take the write lock only after
a read-side `by_node` probe.

---

## 15. Low — Ownership retraction emits no fold audit event

**Location:** `capability_bridge.rs:417-446` (`retract_floored_ownership`).

The function mutates `entry.payload.owner` through `with_state_mut` and then
calls `fold.notify_projection_changed()` (`fold/mod.rs:675`), which only
forwards to `signal_changed()`. Every other fold transition emits an
`AuditEvent` to the installed `FoldAuditSink` (`fold/mod.rs:376`, `:431`,
`:443`).

A deployment with an audit sink therefore records capability creation,
replacement, eviction and expiry — but is **silent on the one security-relevant
transition in this feature**: *"a revocation floor rose and retracted node N's
proven ownership under org O."* The `tracing::info!` at `mesh.rs:7599` is the
only trace and is not on the audit plane. `AuditKind::Custom(&'static str)`
exists for exactly this.

---

## 16. Low — `--insecure-permissions` overloaded across two unrelated gates

**Location:** `cli/src/commands/org.rs:260-263`.

The same flag (a) disables the Unix org-key mode gate and (b) silences the
Windows audience-secret DACL warning. An operator who adds it once on Linux to
get past a 0644 key checked out of git carries it into a Windows run, where it
now suppresses the **only** signal that a freshly minted discovery key landed
under a permissive inherited DACL. Two flags, or make the Windows suppression
require its own opt-in.

---

## 17. Low — Least-informed failure path steers the operator toward the clobbering flag

**Location:** `cli/src/commands/org.rs:1131-1134`, `identity.rs:147-151`.

When `try_exists` fails — e.g. `EACCES` on a parent directory, exactly the case
where you cannot tell what is at the path — the error reads *"failed to stat …;
pass `--force` to override."* `--force` does not override the stat failure; it
skips the existence check entirely and unconditionally clobbers. The advice the
CLI gives on its own least-informed code path is the advice that turns §2 into
data loss.

---

## 18. Low — Non-constant-time comparison of two raw discovery keys

**Location:** `org_grant_registry.rs:253`.

Install idempotency compares two raw `discovery_key()`s with `==`. Only
reachable from the local operator/SDK install API, and no attacker was
constructed who can both supply candidate keys and time the result — reported
because it is the one genuine secret-vs-secret comparison in the grant family.
Everything else compares public values (`OrgId`, `CapabilityAuthorityId`,
`audience_handle`, `key_commitment`) or the *commitment* rather than the key.

---

## 19. Low — `default_authority_dir()` falls back to CWD, unguarded on Windows

**Location:** `cli/src/commands/node.rs:239`.

Falls back to `PathBuf::from(".")` when `dirs::config_dir()` returns `None`,
putting the authority directory under the process CWD. On Unix the ancestor walk
refuses a hostile CWD; on Windows nothing would (and see §3/§4). Requires a
broken environment to trigger.

---

## Test quality

The suite is large and parts are strong: `nrpc_service_equality.rs` (genuine
sole-discriminator witness with a live positive control), the `org_request_digest`
lib units (golden literal digest, header order + multiplicity binding, over-cap
refusal), `live_two_node_owner_delegated_floor_survives_restart_denies`
(`integration_nrpc_protected.rs:2981` — real boundary, both sides),
`grant_capability_pair_rollback_leaves_no_grant_when_secret_publish_fails`,
`adopt_refuses_foreign_floor_bundle`, and
`floored_authority_fails_installation_and_emission_goes_dark`.

The findings below are places the suite gives **false confidence** — where the
named security property is not actually witnessed.

### T1. The sharpest auth property in the feature is untested

All 35 caller constructions in `tests/integration_nrpc_protected.rs` build the
calling node and mint the proof intent from the **same** `CALLER_SEED`
(`build_node_with(EntityKeypair::from_bytes(CALLER_SEED))` at line *N*, the
intent's keypair at line *N+30*). No test ever presents a proof minted for
identity X over identity Y's session.

**Mutation that stays green:** at `mesh_rpc.rs:1079`, change
`authenticated_caller: &caller` (the value from `resolve_direct_caller`) to the
caller decoded from the proof. Every integration test and every unit test still
passes — and captured proofs become universally replayable.

Same gap for three siblings:
- **Replay** — the guard is node-scoped (`mesh.rs:6115`, passed at
  `mesh_rpc.rs:3368`), but every test mints a fresh proof per `call()`, so no
  integration test replays anything. Constructing a fresh
  `AdmissionReplayGuard::with_defaults()` per call at the `:3368` call site
  leaves everything green.
- **Proof expiry** — `proof_ttl_secs: 30` at all four intent sites; nothing ever
  lets it lapse.
- **Wire tamper** — no integration test mutates proof bytes; there is no seam to
  inject a raw `net-org-admission` header.

Also absent: any live test of the §9.5 `AuthorityChanged` recheck, and any live
test of the E1.8 unary-only rejection (`is_unary`, `mesh_rpc.rs:1055`) —
`NotSupported` is asserted by no test.

**Compounding:** every deny test asserts only `status == 0x0009`, and
`CoarseAdmissionReason` has three values, while six pre-admission bailouts in
`admit_and_dispatch_protected` all emit `Denied`/0. So no deny test distinguishes
its named reason from any other. `live_two_node_missing_proof_denied:420` is the
extreme case — `matches!(message.as_bytes()[0], 0..=2)` accepts every possible
byte.

### T2. `nrpc_call_hijack.rs:107` — forged CANCEL never reaches the check it names

`a_forged_cancel_from_another_session_cannot_cancel_a_victims_call` publishes
`forged_cancel(route, victim_origin, victim_call_id)` on the attacker's own
session. `bridge_origin_check` (`mesh_rpc.rs:683`) drops it at `:708` on
`inbound.origin_hash != meta.origin_hash` — **before `apply_inbound` runs at
all**.

**Mutation that stays green:** revert `cortex/rpc.rs:1763`
`let key = (from_node, meta.origin_hash, meta.seq_or_ts)` to the pre-AV-1
`(meta.origin_hash, meta.seq_or_ts)` — i.e. delete precisely the check the
docstring names. The test never asserts `packet_origin_mismatch_dropped_total == 0`,
which is the assertion that would force the fold path to be exercised. Third
wrong-reason path: the `sleep(200ms)` at `:184` races `release.notify_one()`; a
late CANCEL finds the entry already removed (`rpc.rs:1970`) and is a documented
no-op.

### T3. `nrpc_response_routing.rs:120` — triple-redundantly guarded

`concurrent_injection_does_not_disturb_a_victims_response` uses
`forged_request(route, "other-service", victim_origin, …)`, rejected by (a)
`is_cross_service_request` (`mesh_rpc.rs:478`), (b) Gate-3 at `:708`, and (c)
`cache_authenticated_response_destination` (`:548`) keying on
`(inbound.from_node, …)`, which makes an attacker with a different `from_node`
*structurally* incapable of clobbering the victim's entry. Delete
`response_route_is_trustworthy` (`:566`) entirely and cache unconditionally —
still green. Net information content ≈ zero.

### T4. CLI negative tests: exit-code collisions mask the checks under test

- **`cli/tests/org_grant.rs:214` `grant_capability_flag_validation`** — three of
  five cases pass `--target-any-owned-by TARGET_ORG_HEX`, which is *not* the
  freshly keygen'd issuer org, so `check_target_owner` (`org_grant.rs:764`)
  returns `TargetOrgNotIssuer` → `invalid_args` → **exit 2**, the same code the
  test asserts for the check under test. Deleting the `(false,false)` no-rights
  arm (`org.rs:491`), the `(true,None)` `--discover requires --audience-out` arm
  (`:511`), or the `(false,Some(_))` arm (`:514`) leaves it green. The trailing
  `assert!(!stray_secret.exists())` also still holds, since failure precedes
  `stage_beside`. Cases 3 and 4 are sound.
- **`org_grant.rs:399` `grant_capability_rejects_aliased_paths`** — asserts only
  `.code(2)`. Delete `refuse_aliased_paths` (`org.rs:934`) wholesale: the
  no-clobber publish (`:1045`, `ErrorKind::AlreadyExists`) produces the identical
  exit code and the rollback at `:614-621` keeps the follow-up assertions
  holding. Production's own comment at `:922` says the alias check is
  "best-effort" and "the actual safety comes from no-clobber publication" — so
  this test pins nothing.
- **`org_grant.rs:441`, `:489`** — `refuse_force(args.force)?` is the **first
  statement** of both `run_grant_dispatcher` (`org.rs:426`) and
  `run_grant_capability` (`:484`), so the case-variant `--out` and the
  `--discover`/`--audience-out` args are never read. `:507`'s "org root key was
  not clobbered" is vacuous.
- **Systemic:** exit code 2 is clap's `USAGE_CODE`, identical to
  `ExitCodeKind::InvalidArgs` (`cli/src/error.rs:44`). ~17 `.code(2)` assertions
  across `org_grant.rs`/`org_adopt.rs`, **zero** stderr assertions. Renaming
  `--target-any-owned-by`, `--any-capability`, or `--skew-secs` turns whole
  validation branches into dead code with every test green.
- **`org_adopt.rs:72`** asserts the three authority files *exist* and (Unix only)
  that `owner-audience.key` is 0600 — never opens any of them. A zero-filled
  audience key, or a membership file recording the wrong `org_id`, ships green.

### T5. `org_ownership.rs:735` — the "restart" witness never restarts

`floors_gate_ingest_and_survive_restart_with_lower_valid_bundle`:
`join_or_create_core` (`org_revocation.rs:827`) keys a **process-global registry
of live `StoreCore`s** and returns the existing core whenever one is alive for
that path. The original store is still alive — moved into
`install_org_revocation_store` at `:748` and held by the node. `StoreCore::publish`
(`:437`) then merges incoming disk state *under* the live view with a per-key
max. So `restarted` at `:782` is the same in-memory core and floor 5 comes from
RAM; the disk round-trip is never exercised.

**Mutation that stays green:** make `to_file_bytes` emit an empty floor map, or
make `apply_bundle` skip the durable write. (The file asserts core-sharing as a
*feature* at `:1137` and `:1961` — which is why this test's premise fails.)
**Fix:** drop every live handle for the path before `open_existing`, or reopen in
a subprocess.

### T6. `capability_exposure_revocation.rs:365` — final assertion re-asserts its own precondition

`a_security_refused_deferred_flush_drives_a_corrective_send` asserts at
`:394-401` as a *precondition* that the client sees `nrpc:deferred-svc`, then at
`:434` asserts the identical predicate. Nothing between them can remove the
entry. The middle assertion (`:425`) checks only
`server.announcement_bytes_for_send_for_test().is_some()` — purely server-local,
never that anything reached the client. Make the refused-deferred-flush branch
`return` without the corrective pass while keeping the local republish: green.

The correct sibling at `:173` never seeds the tag, so client observation
genuinely proves a send happened. The deferred variant inverted that and
destroyed the witness.

### T7. `nrpc_streaming_gate.rs:153`, `:181` — server-side metrics only, and a hang swallowed into a pass

`client_streaming_denies_unauthorized_caller` and
`duplex_denies_unauthorized_caller` assert only `assert_gated` (server metrics);
neither asserts the caller receives a terminal `CapabilityDenied`. Line 171:
`let _ = tokio::time::timeout(Duration::from_secs(3), call.finish()).await;` —
`CallOptions::default()` has `deadline: None` (`mesh_rpc.rs:208`, "waits
indefinitely"), so if the server denies but never emits the terminal denial,
`finish()` never resolves and the `let _ =` converts the hang into a silent
pass. The duplex variant is worse: `sleep(200ms)`, never calls `finish()`.

**Mutation that stays green:** make `emit_capability_denial` (`mesh_rpc.rs:770`)
a no-op on those two bridges (bump metric, skip emit). Every denied streaming
caller now hangs forever in production.

### T8. Non-falsifiable and flaky shapes in `org_ownership.rs`

- **`:1480`, `:1544`** — `done_rx.recv_timeout(400ms).is_err()` as the positive
  "the opener was blocked" assertion. `RecvTimeoutError` has two variants; if
  `open_existing().expect(...)` panics in the detached thread (reachable via
  `BackingIdentityConflict`, `org_revocation.rs:850`, or poison-recovery
  failure), `done_tx` drops and `recv_timeout` returns `Err(Disconnected)`
  **immediately** — the test reports "correctly blocked" for the opposite
  reason, with the panic invisible. Send the `Result` over the channel and match
  `Err(Timeout)` explicitly.
- **`:951`, `:1054`** — spawn a raiser, `thread::sleep(300ms)`, assert
  `!raise_done`. Nothing establishes the raiser ever reached `apply_bundle`; a
  thread still in spawn latency satisfies it identically. Deleting the `_pin`
  from `install_org_revocation_store_locked` (`mesh.rs:7515`) leaves both green
  under load.
- **`:574`** — real 3-second `tokio::time::sleep` for cert expiry. Correct today,
  silently vacuous the moment anyone adds `start_paused = true`.
- **`:272`, `:343`, `:518`, `:762`, `:839`, `:850`, `:861`, `:917`, `:1412`,
  `:2017`** — all route through `test_inject_capability_announcement`
  (`mesh.rs:22441`), documented as mirroring inbound dispatch but a **hand-copied
  duplicate**; the real path re-implements the same three steps at
  `mesh.rs:17709`. Delete the owner-cert verification block there and pass `None`
  for `verified_owner`, and all ten tests — including the headline
  `node_ingest_drops_bad_cert_but_keeps_announcement` (`:694`) — stay green. **No
  over-the-wire bad-cert negative exists.** (This is the test-side face of §12b.)
- **`:104-155`** `owner_cert_projects_across_the_wire_only_when_emitted` — phase 1
  asserts "emission off ⇒ no ownership projected" *before* `install_node_authority`
  at `:140`, and `owner_cert_for_emission_at` (`mesh.rs:8220`) early-returns on
  the missing authority without ever reading the emission flag. Flip the flag's
  default to `true`: still green.
- **Residual hazard:** `scratch_dir("startup")` deliberately writes corrupt JSON
  and cleans up only on the happy path, so a PID-recycled rerun panics in
  `NodeAuthority::adopt` for an unrelated reason.

### T9. CI

**Good.** The new `Guard — every tests/*.rs is pinned to a step`
(`.github/workflows/ci.yml:94-143`) closes a real silent-skip hole: `--test`
pins were previously hand-maintained with nothing complaining about a new
unpinned file. Verified locally — 123 test files, all pinned, no stale pins.
Feature-gate alignment is correct (`cortex = ["redex"]`, `redex = ["net", …]`,
so `--features "cortex tool"` satisfies every
`#![cfg(all(feature="net", feature="cortex"))]`; `org_ownership` and
`org_admission_wire` are `net`-only and sit in the `net` group). No test compiles
to a silent 0-test binary. No `#[ignore]` in the new suites. CLI tests are
auto-discovered by `cargo test -p net-cli` (`ci.yml:1575`).

**Gaps:**

- **`net/crates/net/.config/nextest.toml` sets `retries = 2` globally.** Every
  integration test gets three attempts. The probe-driven race witnesses this
  branch adds (`a_visibility_change_during_serialization_refuses_the_stale_bytes`,
  `a_consumer_credential_replacement_racing_the_granted_insert_is_refused`, the
  deferred-flush tests, and T8's sleep-based blocking assertions) are exactly the
  class whose regression signal is intermittent — retries convert an intermittent
  security regression into a green run. The file's rationale ("a genuine logic
  bug fails all attempts") does not hold for races.
- **No `slow-timeout` / `terminate-after`.** T7's hang path burns the job's
  20-minute budget and surfaces as a job timeout rather than a test failure.
- **Every Windows security assertion is dead code.** `grep -c windows-latest`
  over `ci.yml` returns **0**; all jobs are `runs-on: ubuntu-latest`.
  `org_grant.rs:517` `grant_dispatcher_case_variant_no_clobber_preserves_root`
  and `:614` `grant_capability_discover_warns_about_windows_dacl` are
  `#[cfg(windows)]` and never compile, never link, never run. They are the only
  coverage of `warn_secret_permissions` (`org.rs:1086`) — the entire Windows
  substitute for the 0600 guarantee — and this is the platform whose file
  permission model is weakest. **Directly relevant to §3 and §4, which are
  Windows-only findings in code no CI job exercises.** Mirror problem:
  `#[cfg(unix)]` gates the only permission assertions in the suite
  (`org_grant.rs:201`, `org_adopt.rs:55`, `:99`), so a local `cargo test` on this
  repo's own win32 dev box reports green for a suite in which no confidentiality
  property is checked at all.
- The guard checks that a `--test` flag exists *somewhere* in `ci.yml`, not that
  it sits in a job that runs; and, as its own error text concedes, it does not
  verify feature-gate alignment.

---

## Documentation accuracy

These matter because the plan is the artifact the next reviewer will trust.

### D1. Cross-org grants have no revocation channel the issuing provider can use

The only revocation check in the admission order is the membership floor
(`org_admission.rs:464-469`), keyed on `(acting_org, member)` — the **caller's**
org. `OrgRevocationBundle` verifies against `bundle.org_id` (`org.rs:773`), so
only org A can sign floors that kill A's members. There is no floor, denylist, or
CRL keyed on `OrgCapabilityGrant::grant_id`.

**Consequence.** B issues A a `CrossOrgGranted` grant with the plan's stated
"days–weeks" TTL (§2.2). The A↔B relationship terminates. B has no cryptographic
lever — it cannot sign A's membership floors, and its own grant dies only at
`not_after`. Any A member holding a valid membership cert and dispatcher grant
keeps invoking B's protected service for the remainder of the grant window. B's
only recourse is the `provider_policy` closure (step 11, `org_admission.rs:545`)
— application code, and it runs *after* the replay insert.

This is deliberate per the plan ("Grants: expiry + non-renewal; provider-local
deny is immediate"), but the plan's pinned-invariant list reads *"revocation
monotonicity survives restart"*, which materially overstates what the cross-org
path delivers. The mechanism to fix it exists (`grant_id` is in the proof the
policy sees) but is entirely application-supplied. **Either add a grant-id
denylist or state the limitation in the invariant list.**

### D2. Plan §3.2's confidentiality claim is stale

*"size — nothing matchable"* was true before the OA3-4b2 single-tag canonical
bind. See §6.

### D3. `MembershipRevoked` doc contradicts the implemented boundary

`org_admission.rs:139-142` says a floor risen *"to or above"* the cert's
generation kills it. The code is `generation < floor` (`:468`) — a cert with
`generation == floor` is **alive**. `org.rs:718-720` and `org_authority.rs:210`
state the canonical rule correctly. An operator who reads this variant's doc and
issues a floor *equal* to the generation they intend to kill leaves the
credential fully live and gets no error. One-line doc fix.

### D4. Two stale in-code comments

- `mesh_rpc.rs:2617-2619` — *"the bridge inserts before the capability gate, so
  an unbounded map would let one authed peer spray distinct `(origin, call_id)`
  keys."* That ordering was inverted by this branch:
  `cache_authenticated_response_destination` now runs only on the accept path
  (`:754`, `:1135`). The comment describes a threat model the code no longer has.
- `CODE_REVIEW_2026_05_23_MULTIFOLD_DEFERRED.md` MD-1's "~30 production call
  sites" for `apply_legacy_announcement` is now zero — all in-tree callers are in
  test modules (see §12a).
- The plan's *"one `cfg(test)`-only RED seam"* understates the ~40 `#[cfg(test)]`
  seams the branch adds inside production modules (see §12's closing note).

---

## Claims verified as TRUE

Recorded because they were load-bearing in the plan and were checked rather than
assumed:

- **`may_execute` is byte-for-byte unchanged.** `has_local_capability`
  (`capability_bridge.rs:721`) was added adjacent to it, reads only tag presence,
  and evaluates no allow-lists. The §2.4a rationale is sound.
- **No new dependencies.** `Cargo.lock` gains exactly two lines: `zeroize` added
  to `chacha20` and `chacha20poly1305`, from enabling an existing feature of an
  already-vendored RustCrypto crate.
- **AEAD construction is sound.** 24-byte random XChaCha nonce per seal from
  `getrandom` with `abort()` on failure; deterministic-nonce builders are
  `#[cfg(test)]` only; AD binds
  `provider ‖ owner_org ‖ audience_handle ‖ grant_id ‖ generation ‖ expires_at`,
  and everything outside the AD is covered by the domain-prefixed outer Ed25519
  signature verified before the type can exist. Owner and granted AD domains are
  provably disjoint via the reserved all-zero grant id, rejected at both issuance
  and decode.
- **The relay never touches plaintext.** `decide_scoped_relay` performs no AEAD
  work; relay-path logs carry only `from_node` and typed enums whose `Debug`
  impls emit lengths, never bytes. Envelope bytes are forwarded verbatim.
- **Exactly one decrypt attempt per envelope** — the zero sentinel selects the
  single owner key; a nonzero `grant_id` is an exact `BTreeMap` lookup plus a
  handle equality check (`mesh.rs:17352`), never a scan across installed secrets.
- **Bridge coverage is complete.** `register_rpc_inbound` has exactly five call
  sites; the four inbound bridges all run `bridge_origin_check`, the three public
  streaming bridges run `bridge_preflight` on **every** frame including
  continuations, and `serve_rpc_owner_scoped` / `serve_rpc_granted` land in the
  protected arm. No fifth path.
- **Streaming continuation frames are session-bound.** All four folds key
  per-call state on `(from_node, caller_origin, call_id)` where `from_node` is
  the AEAD-resolved session peer, and ingress *drops* rather than falling back to
  the `0` sentinel (`mesh.rs:12364`).
- **Mixed-version is fail-closed, and is a hard wire break.** Ingress requires the
  `RpcRouteV1` discriminator for any RPC-dispatch frame (`mesh.rs:12402`); an
  absent/malformed route is `continue`d in both snapshot arms. Corollary worth
  planning around: a branch-version caller's frames also will not parse on a
  master-version provider, so **E2.3's staged rollout is load-bearing for
  availability**, not only for security.
- **Origin binding is correct.** `bridge_origin_check` (`:683`) binds packet
  origin to payload origin before admission, cache mutation, or fold execution;
  `resolve_direct_caller` (`:985`) resolves the TOFU-pinned entity for the AEAD
  session and requires its `origin_hash()` to equal the claimed one. The
  `EntityId` it returns — never a payload field — feeds
  `AdmissionContext::authenticated_caller`. The capability id derives from the
  *captured* registration, never from `payload.service`.
- **Registration races are closed.** `register_rpc_inbound` is vacant-only under a
  held DashMap shard entry with a monotonic `registration_id`;
  `unregister_rpc_inbound` uses `remove_if` so the wire-bucket empty-check and
  removal are atomic; `ServeHandle::drop` cannot evict a replacement. No same-name
  hijack path.
- **Response-route poisoning is structurally impossible.** The cache is keyed
  `(from_node, origin_hash, call_id)` with value `from_node` — redundant with the
  key, so a lookup returns either the session that delivered the REQUEST or
  `None`.
- **Digest-vs-strip ordering is correct.** `org_request_digest`
  (`org_admission_gate.rs:58-93`) validates the *finalized* request including
  proof headers before stripping, so a request invalid only in its proof headers
  cannot reduce to a valid canonical. Caller and provider call the same function
  over the same bytes.
- **Revocation ordering is monotonic.** Every floor comparison in the tree is
  uniformly `generation < floor → reject` (`capability_bridge.rs:309`,
  `org_authority.rs:210`, `org_admission.rs:467`, `org_scoped_ingest.rs:525`,
  `org_scoped_store.rs:250`) — no `>=`/`>` inversion anywhere. `FoldKind::merge`
  is strictly `incoming.generation > existing.generation`. `StoreCore::publish`
  takes a per-key max against the live view, making "an installed floor never
  lowers" structural.
- **Lock ordering is acyclic.** Interprocess file lock → `core.reload` → `live`,
  obeyed by `apply_bundle` and `join_or_create_core`; `Fold` is always `state`
  before `index` (all 12 sites); `publish_guard_pair` orders distinct cores by
  normalized path, and two live cores can never share a path (`:848`), so that
  comparison is a strict total order.
- **RAII lifecycle is sound.** `StoreCore::notify` drops `subscribers.read()`
  before invoking callbacks, so a self-unsubscribing callback cannot deadlock;
  `enter`/`leave` are paired by `LeaveOnDrop`; the `ACTIVE_LEASES` raw-pointer
  identity check is ABA-free; `MeshNode`'s callback captures only `Arc<Fold>`,
  `Arc<ArcSwapOption>` and `Weak<Store>`, so the cycle is broken by ordinary field
  drop.
- **`ProviderFacts` ABA is closed.** Both the `NodeAuthority` and
  `OrgRevocationStore` `Arc`s are pinned for the admission's lifetime, so the
  §9.5 raw-pointer comparison cannot false-match on a reallocated address.
- **Replay-cache flooding cannot evict a victim.** `reclaim_caller` /
  `reclaim_all` remove only expired entries; there is no LRU or pressure eviction
  path. The per-caller ceiling is checked before the global one and `validate()`
  enforces `0 < per_caller < global`.
- **SDK surface exposes no bypass.** `OrgAudienceSecret` and
  `OwnerAudienceCredential` keep `discovery_key` private, carry redacted `Debug`,
  zeroizing `Drop`, and the non-`Serialize` compile guard. `from_bytes` on both
  grant types is a strict canonical decoder; the grant structs' `pub` fields are
  caller-side only and every provider path re-verifies via `verify()`.
- **No reachable panics** in the crypto/authority or ingest→fold paths: every
  `unwrap()` is a `try_into()` on a fixed-offset slice behind an exact-length
  check with an `#[expect]` justification; `hop_count` uses `saturating_add`;
  `apply_bundle` is unreachable from any wire path.

---

## Recommendation

**Merge-blocking:** §1 (breaks the confidentiality guarantee the branch exists to
provide) and §2 (unrecoverable loss of the org root, with no revocation path
afterward).

**Before production traffic:** §3–§7. Note that §3 and §4 are Windows-only and
sit in code that **no CI job compiles** (§T9) — fixing them without adding a
Windows job leaves them unverified.

**Highest-leverage test work:** §T1. The caller-binding mutation is the single
most valuable defect in the feature and is currently invisible to CI; §T4's
exit-code collisions and §T9's global `retries = 2` are the systemic multipliers
behind several other blind spots.
