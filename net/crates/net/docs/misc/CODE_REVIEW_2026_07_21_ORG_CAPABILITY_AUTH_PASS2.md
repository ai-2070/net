# CODE REVIEW 2026-07-21 — Organization Capability Auth, second pass (`org-capability-auth`)

> **Status: ALL FINDINGS REMEDIATED — awaiting independent review.**
>
> Every finding below is closed, across 23 commits (`523bb9fc4` … `eeb7db040`),
> each red-witnessed against the branch's own standard: the guard was disabled,
> the targeted witness confirmed to fail while its positive controls still
> passed, then restored.
>
> **This document is now the record of what was found and what was done about
> it, not a list of outstanding work.** The findings are retained in full —
> they are the justification for changes a reviewer has to evaluate, and
> deleting them would leave 23 commits with no stated cause. Each carries its
> disposition in the [remediation table](#remediation-status).
>
> **What still needs doing is REVIEW, not remediation.** Nothing here has been
> independently checked. Two places encode a judgement about the deployment
> rather than a property of the code, and are the first things to scrutinise:
> the §5 replay partition envelope and the §6 rate-limit envelope. Both ship
> safe defaults and are operator-configurable; neither is a universal limit.
>
> This was a second, independent review of the branch at `187ef4213` — i.e.
> AFTER the 2026-07-20 pass and its full §1-§20 / §T1-§T9 remediation. It had
> two jobs: audit whether those remediations actually closed, and find what the
> first pass missed.
>
> **The core authorization design holds.** `may_execute` is byte-unchanged,
> the `#[cfg(test)]` RED-witness seam is unreachable in release, no scoped tag
> can reach a plaintext announcement, there is no authorization bypass in
> `verify_org_admission`, and there is no new bridge asymmetry. Those are the
> properties most likely to sink the feature and they all survived adversarial
> scrutiny — see [Verified-clean register](#verified-clean-register), which is
> written to be re-checkable rather than trusted.
>
> **What was open was the seams, again.** Six of the twenty 2026-07-20
> findings were INCOMPLETE, and five of those six were the same *propagation
> gap* the first review named as its dominant pattern — the fix applied to the
> path under review and not to its siblings. The new findings were largely a
> second instance of that shape, plus a second shape worth naming: **bounds
> that are correct in isolation and do not compose**.
>
> **The branch did not pass its own gates at review time.** `cargo clippy
> --lib --features cortex -- -D warnings` failed at `187ef4213`, and the
> branch's own new `windows-security-tests` CI job failed on its clippy step
> and therefore never ran any of the three security suites it was added to
> gate (§C1). Fixed first, in `523bb9fc4`, because until it landed nothing
> else could be verified on the platform §11/§12/§13 concern.
>
> Gates at the remediated head: lib cortex **5088**,
> integration_nrpc_protected 41, org_ownership 32, org_admission_gate 9,
> org_admission_wire 2, org_scoped_relay 18, net-cli 93 + integration; clippy
> clean under `cortex`, `net`, `cortex fixtures` and `--no-default-features`;
> `cargo fmt` clean.
>
> ---
>
> ## Remediation status
>
> Three dispositions, deliberately distinguished. **FIXED** = behaviour
> changed. **DECIDED** = behaviour kept, and the reasoning recorded at the line
> where a reader would otherwise assume it was an oversight. **BOUNDED** = a
> real property boundary stated at the point where the surrounding code reads
> as if it covers the case; not something code alone can close.
>
> | Item | Sev | Disposition |
> |---|---|---|
> | §C1 / §C2 / §C3 — CI gate blind, Windows job red | Med | **FIXED** `523bb9fc4` |
> | 2026-07-20 §2 — org root still destroyable | **Crit** | **FIXED** `67bc26f4d` |
> | §1 — floor reset on re-adopt | High | **FIXED** `59dc0bc70` |
> | §2 — audience key inherited across orgs | High | **FIXED** `54ec6fab2` |
> | §3 + §4 — scoped expiry clamp / per-scope budget | High | **FIXED** `417700942` |
> | 2026-07-20 §20 residual + §11 — Windows DACL | High | **FIXED** `d1e0f241b` |
> | 2026-07-20 §10 / §19 / §17 — CLI secret hygiene | Med/Low | **FIXED** `781d8991d` |
> | §T1 / §T2 / §T3 — test falsifiability | — | **FIXED** `4c5c6db6e` |
> | §D1 — 2026-07-20 production-closure overclaim | — | **FIXED** `0c074b48c` |
> | §8 / §9 — relay truncation, unswept store | Med | **FIXED** `b81d6c67d` |
> | §10 — GrantedAudience existence oracle | Med | **FIXED** `17e841ac0` |
> | §5 — replay budget partitioned by trust domain | Med | **FIXED** `8f35fd94c` |
> | §7 — denials off the serialized bridge loop | Med | **FIXED** `1830a3497` |
> | §13 + §19 — Windows durability, gated seams | Med/Low | **FIXED** `e3e866b5d` |
> | §15 / §18 / §20 / §21 — revocation lows | Low | **FIXED** `d8d59ac4a` |
> | §16 / §17 — poison durability limits | Low | **BOUNDED** `d8d59ac4a` |
> | §22 / §25 / §26 / §31 / §32 — low cluster | Low | **FIXED** `3c0f5897f` |
> | §33 — pre-epoch fail-safe reasoning inverted | Low | **FIXED** (doc) `3c0f5897f` |
> | §14 — cross-org floors absent-means-zero | Med/Low | **BOUNDED** `67dd13581` |
> | §23 — cleartext relay framing | Low | **BOUNDED** `67dd13581` |
> | §27 / §28 / §29 — secret-lifecycle obligations | Low | **BOUNDED** `67dd13581` |
> | §30 — baseline residue un-suppresses | Low | **DECIDED** `67dd13581` |
> | §T4 – §T9 — remaining test quality | — | **FIXED / DECIDED** `9234a4584` |
> | §12 — Windows ancestor chain | Med | **FIXED** `1801880b7` |
> | §24 — dedup identity consumed on refusal | Low | **FIXED** `129bf39ad` |
> | §6 — unmetered pre-credential signature work | Med | **FIXED** `eeb7db040` |
> | §34 — provider-posture oracle ordering | Low | **BOUNDED** (see below) |
>
> §34 is the one finding with no dedicated commit: the ordering it describes
> (provider self-verify before caller-credential checks, so `Unavailable`
> versus `Denied` is observable pre-credential) is unchanged, and §6's
> throttle now bounds how fast an uncredentialed peer can probe it. Reordering
> the gate to hide it would move provider self-verification after credential
> work, which is a worse trade: the check exists to fail fast when the provider
> cannot admit at all.
>
> Two findings were discovered while FIXING rather than reviewing. Neither was
> reachable by reading, and both are recorded because that is the point:
>
> - **`NO_COLOR=1` broke every subcommand.** `#[arg(long, global = true,
>   env = "NO_COLOR")] no_color: bool` made clap parse the variable's VALUE as a
>   bool literal, so the near-universal spelling — and the one no-color.org
>   specifies — exited 2 before the CLI did any work. It is why
>   `cargo test -p net-cli` could not run on a developer machine at all. Fixed
>   in `67bc26f4d`.
> - **A malformed identity file printed the operator's private seed.**
>   `load_identity_seed` interpolated the toml parse error, and
>   `toml::de::Error`'s `Display` embeds the offending source line — which for
>   that file is `seed_hex = "…"`. `org.rs` omits the error at both its parse
>   sites for exactly this reason. Fixed in `781d8991d`, red-witnessed.
>
> ## Corrections to this document and to the work
>
> Recorded rather than quietly amended, because a review that hides its own
> errors is worth less than one that does not.
>
> 1. **A stale-table claim in §D1 was wrong.** An earlier revision asserted the
>    2026-07-20 TEST table was stale at §T2/§T4. It is not — it reads DONE and
>    the commits it cites exist. The claim came from a remediation-audit
>    subagent that read an older revision and was relayed without checking the
>    file at HEAD. Retracted in `0c074b48c`; see §D1.
> 2. **The first §13 implementation was wrong.** It used `FlushFileBuffers` on
>    a `FILE_FLAG_BACKUP_SEMANTICS` directory handle, by analogy with the POSIX
>    parent fsync. Windows has no directory fsync — that returns
>    `ERROR_ACCESS_DENIED` and failed every store test. The documented
>    primitive is `MoveFileExW(..., MOVEFILE_WRITE_THROUGH)` on the rename.
> 3. **The first §20-residual fix was too blunt.** Refusing every unprotected
>    DACL broke 15 tests and would have rejected the ordinary
>    `mkdir && net node adopt`. Changed to repair-then-validate: check every
>    other rule first, then sever inheritance.
>
> **Two verification traps hit during this work, both of which produced a false
> PASS.** They are process findings, not code findings, and matter to anyone
> reproducing these results:
>
> - **Stale artifacts after a restore.** Restoring a file with `Copy-Item
>   backup → file` preserves the *backup's* mtime, which can predate the
>   compiled artifact, so cargo skips recompilation and reports results for
>   code that is no longer on disk. A §26 witness "failed" against a binary
>   predating its own fix. Red witnesses are safe (their writes move mtime
>   forward); *post-restore verification* is the exposed direction.
> - **A vacuous red witness.** A PowerShell string replace silently failed to
>   match, so "neutering the throttle" for §6 changed nothing and the suite
>   passed against unmodified code — briefly reported as the throttle being
>   unfalsifiable. **A red witness that PASSES means a broken test or a broken
>   patch; assuming the former is how a false green ships.** Confirm the patch
>   applied before trusting a red run.
>
> Every gate quoted in this document was re-run after explicitly touching all
> modified sources.

## Conventions

Findings introduced by **this** pass are numbered `§1…§14` (production),
`§T1…§T9` (test quality), `§C1…§C3` (build/CI), `§D1` (documentation).
Findings from the previous pass are always written with their date —
**2026-07-20 §2** — and never bare, so the two sets cannot be confused.

Per-finding verification status is stated inline:

- **[verified]** — I read the cited code at HEAD myself and reproduced the
  claim.
- **[reported]** — substantiated by a deep-reader with a code citation and a
  concrete failure scenario, not independently re-read line-by-line.

Paths are relative to `net/crates/net/src/adapter/net/behavior/` unless noted
(`mesh.rs`, `mesh_rpc.rs`, `org_admission_gate.rs`, `cli/`, `sdk/`, `tests/`,
`.github/` are relative to the repo or `net/crates/net/` as written).

## Method

Ten adversarial deep-readers were run in parallel over disjoint surfaces:
authority + clock, revocation store, admission gate + replay + call proof,
grant family + registries, scoped-discovery crypto (ann/ingest/relay/store),
`mesh_rpc` bridges, `mesh` emission chokepoint + capability fold, CLI/SDK,
test quality of the six suites the first pass never examined, and a dedicated
audit of the 2026-07-20 remediations. Each was given the prior pass's closed
findings and instructed to report a known item only if the fix was incomplete
or had introduced a new defect, and to discard anything it could not reduce to
a concrete failure scenario.

Every finding at Medium or above was then re-verified by hand against source
at HEAD; the build/CI findings were reproduced by running the commands.

Branch state at review: `187ef4213`, worktree clean, local == origin, 187
commits ahead of `master`, +47,711/−1,147 across 101 files.

---

## Severity summary

### Reopened from 2026-07-20

| # | Sev | Location | One-line |
|---|-----|----------|----------|
| 2026-07-20 §2 | **Critical** | `cli/…/identity.rs:138`, `cli/…/org.rs:300` | Org root seed still destroyable — the guard reached 2 of 4 seed-writing verbs |
| 2026-07-20 §20 | **High** | `org_authority.rs:1353` | Accepted residual rests on a false premise: `SE_DACL_PROTECTED` is read and never enforced |
| 2026-07-20 §5 | **High** | `org_admission.rs:556` | Bounded, not closed — code and commit message assert closure |
| 2026-07-20 §10 | Medium | `cli/…/identity.rs:125`, `:167`, `cli/…/context.rs:377` | Three live seed copies drop un-zeroed in the same crate |
| 2026-07-20 §19 | Low | `cli/…/org.rs:1209`, `cli/…/identity.rs:535` | CWD fallback still ships for both seed paths |
| 2026-07-20 §17 | Low | `cli/…/netdb.rs:578`, `:585`, `:835` | Anti-pattern survives verbatim at three unfixed sites |

### New production findings

| # | Sev | Location | One-line |
|---|-----|----------|----------|
| 1 | **High** | `org_revocation.rs:1018` | `init()` silently resets persisted floors to empty; `open_existing()` refuses the identical state |
| 2 | **High** | `org_authority.rs:713`, `:790`, `:307` | Owner audience key is silently inherited across an ownership change |
| 3 | **High** | `org_scoped_ingest.rs:599` | Ingest never clamps attacker-chosen `expires_at` to the credential that authorized it |
| 4 | High/Med | `org_scoped_store.rs:91`, `:134` | Permanent capacity wedge of the single global scoped store |
| 5 | Medium | `org_admission_replay.rs:53`, `:63` | 16 colluding credentialed identities deny the provider's entire protected surface |
| 6 | Medium | `org_admission.rs:466`, `mesh_rpc.rs:1160` | Unmetered signature work + awaited denial publish, reachable with zero org credentials |
| 7 | Medium | `mesh_rpc.rs:966-1173` | Denials became an awaited network publish on the single serialized bridge task (regression vs `master`) |
| 8 | Medium | `org_scoped_relay.rs:307` | Unsigned `hop_count` + one-shot admit lets a single relay truncate the flood |
| 9 | Medium | `org_scoped_store.rs:216` | `sweep_expired` is never driven in production |
| 10 | Medium | `mesh.rs:2966`, `sensing/scope.rs:99` | `GrantedAudience` existence oracle — grant scope flattened to owner scope in the self-fold |
| 11 | Medium | `org_authority.rs:1905` | Windows: the audience key file's own security descriptor is never validated |
| 12 | Medium | `org_authority.rs:1653` | Windows: no ancestor-chain validation for a pre-existing authority directory |
| 13 | Medium | `org_revocation.rs:1865`, `:1986` | Windows: no durability boundary; the entire poison machinery is inert |
| 14 | Med/Low | `org_admission.rs:471`, `org_revocation.rs:100` | Cross-org floors are "absent means zero" — the check is a permanent no-op for foreign orgs |

### New low-severity findings

| # | Sev | Location | One-line |
|---|-----|----------|----------|
| 15 | Low | `org_revocation.rs:455`, `:1390` | Nothing enforces `disk ≥ live`; a weaker file is absorbed and durably re-written |
| 16 | Low | `org_revocation.rs:293` | Poison is process-local, so a restart launders durability uncertainty |
| 17 | Low | `org_revocation.rs:1561` | Poison tombstone is launderable by deleting the state file |
| 18 | Low | `org_revocation.rs:1020` | `init` writes through `write_atomic`, which never poisons on post-rename fsync failure |
| 19 | Low | `org_revocation.rs:1127`, `:1187` | `#[doc(hidden)] pub` test seams ship in release and can hard-deadlock every admission read |
| 20 | Low | `org_revocation.rs:203`, `:458` | The 2026-07-20 §14 zero-floor fix is enforced at 1 of 3 entry points |
| 21 | Low | `org_revocation.rs:129` | Floors are never pruned, capped, or evicted |
| 22 | Low | `org_scoped_ann.rs:330` | `seal_descriptor_with_nonce` is public API accepting a caller-supplied nonce |
| 23 | Low | `org_scoped_ann.rs:698` | Cleartext framing gives every relay the org's private-discovery topology |
| 24 | Low | `org_scoped_relay.rs:302` | A fail-closed ingest refusal still consumes the dedup identity for 600 s |
| 25 | Low | `org_grant_registry.rs:224` | Capacity sweep is skew-blind; one forward clock jump wipes all 256 records |
| 26 | Low | `org_grant.rs:396` | `decode_config` does not reject the reserved zero `grant_id` |
| 27 | Low | `org_grant_registry.rs:288` | Install moves the secret by value, stranding un-zeroed stack copies |
| 28 | Low | `org_grant.rs:377` | No hardened in-crate loader for `OrgAudienceSecret`; the copyable pattern is unsafe |
| 29 | Low | `org_authority.rs:271`, `:307` | Key material passes through unscrubbed by-value returns |
| 30 | Low | `mesh.rs:19963` | Baseline-residue suppression is keyed on live registration, so deregistering un-suppresses |
| 31 | Low | `mesh_rpc.rs:4318` | `call_duplex` missing the `stream_window_initial == Some(0)` deadlock guard |
| 32 | Low | `mesh_rpc.rs:1042` | Frame body sliced without the local length guard both sibling helpers carry |
| 33 | Low | `admission_clock.rs:40` | The documented pre-epoch fail-safe is wrong about why it is safe |
| 34 | Low | `org_admission.rs:249`, `mesh_rpc.rs:1078` | Provider security-posture oracle available before any caller-credential check |

### Build / CI

| # | Sev | Location | One-line |
|---|-----|----------|----------|
| C1 | Medium | `capability_bridge.rs:37-38` | Branch fails `clippy --lib --features cortex -D warnings`; the new Windows security job is red and runs nothing |
| C2 | Low | `.github/workflows/ci.yml:1743` | Windows job's test filter silently excludes two security modules and no-ops on a rename |
| C3 | Low | `.github/workflows/ci.yml:1746` | Windows job uses `cargo test`, bypassing the new hang-timeout and no-retry overrides |

---

# Part I — Audit of the 2026-07-20 remediations

> **Findings below are stated AS FOUND, at `187ef4213`.** They are retained
> verbatim because they are the evidence for the changes that followed — a
> reviewer evaluating a commit needs the case that motivated it. Present-tense
> claims describe the branch at review time, not now. Disposition and commit
> for every item is in the [remediation table](#remediation-status).

Verdicts across all twenty production findings:

| § | Sev | Verdict |
|---|-----|---------|
| §1 | Critical | **CLOSED** |
| §2 | Critical | **INCOMPLETE** |
| §3 | High | CLOSED |
| §4 | High | CLOSED |
| §5 | High | **INCOMPLETE** |
| §6 | High | CLOSED (residual noted) |
| §7 | High | CLOSED |
| §8 | Medium | CLOSED |
| §9 | Medium | CLOSED |
| §10 | Medium | **INCOMPLETE** |
| §11 | Medium | CLOSED |
| §12 | Medium | CLOSED |
| §13 | Medium | CLOSED |
| §14 | Low | CLOSED |
| §15 | Low | CLOSED |
| §16 | Low | CLOSED |
| §17 | Low | **INCOMPLETE** |
| §18 | Low | CLOSED |
| §19 | Low | **INCOMPLETE** |
| §20 | High | **INCOMPLETE** |

No REGRESSED, no UNVERIFIABLE. Every fix changed production code; none was
cosmetic or test-only.

## I.1 — 2026-07-20 §2 was still open, and still Critical **[verified]** — FIXED `67bc26f4d`

**Location:** `cli/src/commands/identity.rs:138-189`, `cli/src/commands/org.rs:300-329`

`refuse_replacing_org_key` has exactly **one** call site — `cli/src/commands/org.rs:1129`,
inside `publish_json_artifact`, reached only by `issue-cert` and `issue-floors`.
Those two verbs are genuinely and well closed: content backstop, alias check,
staged publish, stderr-asserting tests.

The two verbs that write **seed** files were never brought onto it:

```rust
// identity.rs:138 — the whole existence check is inside `if !args.force`
if !args.force {
    match tokio::fs::try_exists(&path).await { … }
}
// …falls through to:
write_identity_atomically(&tmp, &path, toml_text.as_bytes()).await?;
```

```rust
// org.rs:300 — refuse_existing returns Ok(()) immediately on force (org.rs:1081)
refuse_existing(&path, args.force).await?;
// …same write path:
write_identity_atomically(&tmp, &path, toml_text.as_bytes()).await?;
```

`write_identity_atomically` is `create_new` on the temp plus
`tokio::fs::rename` (`identity.rs:449`). `create_new` protects only the *temp*.
`rename` always replaces, and the destination is never inspected.

**Failure scenario.** A provisioning script runs
`net identity generate --out "$KEY" --force` where `$KEY` has drifted onto
`~/.config/net-mesh/orgs/org-ab12cd34.toml` — the exact adjacent-variable drift
2026-07-20 §2 describes. Exit 0. The org root seed is replaced by an operator
identity: root unrecoverable, no revocation floor ever issuable again, every
outstanding membership cert live until natural expiry. Identical end state to
§2, through a verb the fix skipped.

Additionally, for `keygen` specifically, `refuse_existing`'s own docstring
(`org.rs:1077-1079`) reads *"This is UX, NOT the safety boundary — the publish
path fails closed on its own."* That is true of `stage_beside`/`publish_staged`
and **false** of `write_identity_atomically`. For `keygen` the TOCTOU stat *is*
the only boundary, so two concurrent runs, or a case-variant `--out ORG.TOML`
against `org.toml` on NTFS/APFS, both clobber.

**Fix direction.** Call `refuse_replacing_org_key` from both seed-writing verbs
before the write — it catches the identity file for free, since that also
carries a `seed_hex` key. For `keygen`, additionally route through
`stage_beside`/`publish_staged` so the no-clobber boundary is the hard link
rather than the stat, and adopt `stage_nonce()` for the temp name (keygen's
`org.rs:328` uses pid only, so a stale temp from a killed run permanently
wedges the path against `create_new`).

## I.2 — 2026-07-20 §20's accepted residual rested on a false premise **[verified]** — FIXED `d1e0f241b`

**Location:** `org_authority.rs:1353` (`DaclView.protected`)

The in-loop rule is correct: `org_authority.rs:1832-1846` checks inheritance
before the write-mask early-continue, covering `OBJECT_INHERIT` and
`CONTAINER_INHERIT`. The problem is the residual the review consciously
accepted, on the basis that *"only a trusted principal has WRITE_DAC to change
the directory ACL after validation."* Two independent reasons that does not
hold:

**(A) `SE_DACL_PROTECTED` is read into `DaclView` and never enforced.**

```rust
// org_authority.rs:1353
#[allow(dead_code)] protected: bool,
```

Set at `:1450`/`:1462`/`:1501`; read only at `:2669`, `:2891`, `:2901`,
`:2932` — all test assertions. `validate_dacl_view` (`:1737-1870`) never
inspects it. An *unprotected* pre-existing authority directory — the default
for anything not made by `create_dir_with_owner_only_dacl` (`mkdir`, an
installer, a backup restore) — receives inheritable ACEs pushed down by
Windows automatic inheritance propagation. Nobody needs `WRITE_DAC` on the
authority directory; the party who needs it is the ancestor's owner, whom
nothing checks.

**(B) There is no Windows ancestor-chain validation at all.**
`validate_unix_ancestor_chain` is `#[cfg(unix)]` (`:1021`), called only at
`:1614`/`:1643` inside `#[cfg(unix)]` blocks. The non-unix arm has no
counterpart, despite the Unix version's own doc declaring cross-account
mutation *through the parent* explicitly in scope. See also §12.

**Failure scenario.** Victim runs
`net node adopt --authority-dir D:\team\net-authority`, leaf pre-existing,
victim-owned, clean but unprotected DACL. Validation passes (owner trusted, no
NULL DACL, zero untrusted ACEs, `protected == false` never inspected).
`owner-audience.key` is written and inherits the directory ACL —
`write_atomic_phased` still has no Windows DACL branch, only
`#[cfg(unix)] mode(0o600)`. Mallory then runs
`icacls D:\team /grant "Everyone:(OI)(CI)(RX)"` — never touching the authority
directory, never needing `WRITE_DAC` on it. Auto-inheritance propagates a read
ACE onto `owner-audience.key`: the raw 32-byte owner discovery key that
decrypts every `OwnerScoped` announcement for the org. Same end state as the
original §20 repro, with zero writes to the authority dir.

**This is the third instance of one pattern in one struct.** A `DaclView` field
carrying `#[allow(dead_code)]` plus a doc comment asserting it is not a
production criterion: for `owner_sid` that assertion was wrong and became
2026-07-20 §3; for `flags` it is now merely stale (`:1320-1324` still says the
validator does not read it, `:1834` does); for `protected` it is still asserted
and still wrong.

**Minimum fix.** Enforce `view.protected` in `validate_dacl_view`, exactly as
`owner_sid` was promoted for §3.

## I.3 — 2026-07-20 §5 was bounded, not closed, while the code asserted closure **[reported]** — BOUNDED, recorded in the code

**Location:** `org_admission.rs:556-561`, `:569`

The skew-widening trigger is genuinely fixed and the ceiling claim checks out:
`org_call.rs:296-299` hard-rejects `skew_secs > MAX_TOKEN_CLOCK_SKEW_SECS` with
`ClockSkewTooLarge`, and `org.rs:596` validates at authority parse, so retention
(`expiry + MAX_TOKEN_CLOCK_SKEW_SECS`) now exactly dominates the acceptance
window. Good fix.

But the commit message and the comment at `org_admission.rs:556-561` both claim
it closes the **second** trigger — *"`admission_clock` closes the INTRA-admission
case; this closes the inter-admission one."* It does not; it widens the margin.

Retention is anchored in monotonic time at admission; freshness reads live wall
time at each subsequent attempt (`mesh_rpc.rs:1079` takes a fresh
`ClockSample::now()` per call). With `d` = monotonic elapsed since admission and
`Δ` = a backward wall step:

- freshness accepts while `d < (expiry − T₀) + skew_live + Δ`
- retention holds while `d < (expiry − T₀) + MAX_SKEW`

The replay window is non-empty iff **`skew_live + Δ > MAX_SKEW`**.

**Failure scenario.** The compound case the first review itself set up: an
operator raises skew to 300 s *because of* a clock-drift incident, then NTP
corrects the fast clock backward by 600 s. `skew_live + Δ = 900 > 300`. The TTL
ceiling does not rescue it — with proof expiry `T₀+30`, `TtlTooLong` requires
`d ≥ Δ − skew = 300`, while the replay window opens at `d = 330`. Net: a
600-second window in which every proof admitted before the step re-admits, the
handler runs a second time, with an `Admitted` attribution. At the default
`skew_secs = 0` it needs `Δ > 300 s`.

**Fix direction.** No code change necessarily required — but record the residual
honestly instead of asserting it away, and consider anchoring freshness in the
same monotonic base as retention.

## I.4 — 2026-07-20 §10: three live seed copies dropped un-zeroed **[reported]** — FIXED `781d8991d`

`identity.rs:420` is now `ScrubbedBytes::new(bytes.to_vec())`, and the wrappers
were correctly promoted into `cli/src/secret.rs`. The org-key path in `org.rs`
is disciplined end to end. The **identity** path has none of it:

1. `cli/src/commands/identity.rs:125` — `let seed = *identity.keypair().secret_bytes();`,
   a plain `[u8; 32]`, never scrubbed. (`org.rs:313` avoids this by consuming
   `secret_bytes()` inline.)
2. `cli/src/commands/identity.rs:167-175` — `seed_hex: hex::encode(seed)` and
   `toml::to_string_pretty(&file)`: two plain `String`s holding the 64-hex seed.
   `IdentityFile` (`:303-311`) has no `Drop` impl and **derives `Debug`**, which
   `OrgKeyFile` deliberately omits precisely so the seed cannot render into a
   log line.
3. `cli/src/context.rs:377-406` (`load_identity_seed`) — the read side shared by
   `net wrap`, `net mcp serve`, `net cap`: `text`, `parsed.seed_hex` and the
   decoded `seed_bytes` all drop un-zeroed.

Lower blast radius than the org root, but the same defect, in the same file,
~250 lines from the fixed line — and §10's own text says *"Same gap applies to
`net identity generate`."*

## I.5 — 2026-07-20 §19: CWD fallback shipped for both seed paths **[verified]** — FIXED `781d8991d`

`cli/src/commands/node.rs:260-262` correctly returns `Option<PathBuf>` with no
CWD fallback, with a written rationale. The same unguarded fallback still ships
in the two siblings that resolve **higher-value** material:

```rust
// cli/src/commands/org.rs:1209 — default --out for `net org keygen`
dirs::config_dir()
    .unwrap_or_else(|| PathBuf::from("."))
```

and `cli/src/commands/identity.rs:535-541` (`default_identity_path`).

On a CI runner or service account with no resolvable config dir, `net org keygen`
writes the **org root seed** into the CWD — a git checkout, an archived build
workspace, a shared directory. On Windows the file inherits the CWD's DACL and
`enforce_strict_permissions` is a no-op, so a world-readable CWD yields a
world-readable org root seed with no warning. Strictly worse than the
`owner-audience.key` case the fix refused to allow. `cli/src/config.rs:125`
already uses the `Option` pattern, so it was available and simply not
propagated.

Related, same verb: `net org keygen` emits no Windows DACL warning at all,
while both *lesser* secrets do (`warn_secret_permissions` at `org.rs:1041` for
the audience secret, `check_strict_permissions` for identity reads). The key
that signs all membership and revocation gets silence.

## I.6 — 2026-07-20 §17: anti-pattern survived verbatim at three sites **[reported]** — FIXED `781d8991d`

`org.rs:1090-1095` and `identity.rs:147-151` are fixed with explicit
"deliberately NO `--force` advice" comments. Still shipping:
`cli/src/commands/netdb.rs:578`, `:585`, `:835` — all
`"failed to stat/inspect …; pass --force to override"` in `try_exists` `Err`
arms structurally identical to the fixed one. `--force` at `netdb.rs:824` skips
the existence check and renames over the destination at `:900`. Snapshot payload
rather than key material, so below §17's org-key framing — but the fix cost two
lines at the sites that were done.

## I.7 — On the retraction

The retraction in the 2026-07-20 doc header is **honest in direction but stale
and incomplete**.

- *Honest:* it names the three things it got wrong (§20 existed, §11 half-closed,
  §12 half-closed) rather than quietly fixing them, and its per-item test table
  is more pessimistic than reality.
- *Stale:* `187ef4213` landed the §T2 scope note and both §T4 wrong-reason
  repairs and did not update the table, which still reads "§T2 PARTIAL / §T4
  PARTIAL". That understates, which is the safe direction.
- *Incomplete:* it retracts the **test**-closure overclaim but leaves the
  **production** claim — *"20 production findings resolved"* — intact. Six
  findings do not support it. See §D1.

## I.8 — Notable CLOSED findings worth a line

- **§1** properly closed. `response_fallback` is derived once from `mode`
  (`mesh_rpc.rs:3178`) and consumed at `:3511`; all four protected entrypoints
  funnel through `serve_rpc_unary_impl`. The three surviving
  `RosterOnStaleDirect` sites (`:3636`, `:3827`, `:4167`) are the streaming
  bridges, which take no admission mode and cannot carry protected
  registrations. Accepted tradeoff, correctly documented: a route-cache eviction
  past 4096 concurrent calls now **drops** a protected response rather than
  leaking it — size that cap against expected protected concurrency.
- **§8**'s "unrepresentable" claim is real, not rhetoric. `BridgePreflight::Proceed`
  carries the frame by value; all four bridges fold `&frame`, and no
  `apply_inbound(&inbound)` survives on any serve bridge. The digest-vs-strip
  ordering is **not** regressed — the protected path never calls
  `bridge_preflight` at all, so `org_request_digest` still runs on the finalized
  request including proof headers.
- **§9** is stronger than reported: `local_member` is `Some` at the single
  production `ScopedIngestContext` construction (`mesh.rs:17332`) and the check
  sits ahead of the `match authority`, therefore ahead of both production
  `open_with` decrypt sites. A revoked node cannot even decrypt, not merely not
  store.
- **§12**: seam (b) `test_inject_capability_announcement` (`mesh.rs:22449`) is
  still `#[doc(hidden)] pub` and release-compiled, contrary to the stated "gate
  both" fix direction — but the hazard is genuinely neutralized, because it
  passes live floors, runs the post-apply recheck, and the node-id bind is
  enforced at `verify_announced_owner_cert`, the sole `VerifiedOwner` producer
  (`VerifiedOwner::new` is `pub(crate)`). Seam (a) is now
  `#[cfg(any(test, feature = "fixtures"))]`. Note the layering is what makes this
  safe: the feature gate alone would not be, since Cargo features are additive
  and any crate in a dependency graph can enable `net-mesh/fixtures`.
- **§13**: memoization is correctness-neutral, and the one fail-open that would
  have mattered was avoided — the `Err` branch is deliberately **not** memoized
  (`org_revocation.rs:1578`).
- **§6**: padding matches the prescribed fix exactly (256-byte buckets, inside
  the AEAD, length prefix authenticated); `pad_descriptor`/`unpad_descriptor`
  verified as exact inverses with a strict re-derivation and zero-tail check.
  Residual: padding is to a *multiple* of 256, so the owner envelope — which
  carries all owner-scoped tags in one payload — still discloses a bucket index,
  leaving a counting channel at roughly 7-tag granularity. Coarsened, not
  eliminated. See §23.

---

# Part II — New production findings

> **Findings below are stated AS FOUND, at `187ef4213`.** They are retained
> verbatim because they are the evidence for the changes that followed — a
> reviewer evaluating a commit needs the case that motivated it. Present-tense
> claims describe the branch at review time, not now. Disposition and commit
> for every item is in the [remediation table](#remediation-status).

## 1. High — `init()` silently resets persisted floors to empty **[verified]**

**Location:** `org_revocation.rs:1016-1029`, reached from `org_authority.rs:783`

Floor monotonicity is this module's single reason to exist. Two entry points
handle a missing `revocation-state.json` in exactly opposite ways:

```rust
// org_revocation.rs:1018 — init(), reached by NodeAuthority::adopt
Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
    let state = OrgRevocationState::empty();
    write_atomic(&path, &state.to_file_bytes()?)?;
    state
}
```

```rust
// org_revocation.rs:1065 — open_existing(), reached by NodeAuthority::open
Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
    let err = OrgRevocationError::MissingState { … };
    tracing::error!("{err}");
    return Err(err);
}
```

`init`'s own docstring at `:992-994` promises the opposite: *"re-adoption
preserves maxima — monotonicity survives even an operator re-running adopt."*
That holds only when the file exists.

**The permissive path is the destructive one**, and it is the one an operator
reaches for when something looks wrong (`net node adopt`). The strict path is
the one that merely reopens.

**Failure scenario.** An authority dir holds `owner-membership.json`,
`owner-audience.key`, and floors at generation 9. `revocation-state.json` is
lost — config-management re-provision, backup restore, selective delete: exactly
the threat the module header opens with. Operator re-runs `net node adopt`.
Ceremony step 3 gets `None` → `empty()`; step 6 `self_verify` passes against
empty; step 7 `init` writes the empty file; step 9 re-reads and self-verifies
against empty. Every membership cert for every previously-revoked member is
admissible again.

**Worse sub-case.** If a same-path core is still live, `join_or_create_core`
(`:878-885`) publishes the empty state through it and `publish`'s per-key max
keeps the *live* view at 9 — so the running node keeps enforcing 9 while the
disk is empty. Nothing logs, nothing poisons; the rollback materializes only at
the next restart.

**Why the `.lock` sidecar cannot serve as the provisioning marker today:** it
survives the state-file deletion and would be the natural evidence, but
`lock_state_file` is called at `:1008` *before* the read and unconditionally
`create(true)`s it (`open_lock_file:1826`), destroying that evidence before it
could be consulted.

**How this survived testing.** The OA-4 restart witness
(`live_two_node_owner_delegated_floor_survives_restart_denies`) proves floor
persistence by calling `NodeAuthority::open` — the branch that already refuses.
Nothing exercises re-adopt against a missing state file.

**Fix direction.** Make `init` refuse a missing state file when any *other*
authority artifact is present (membership or audience file), or record a
durable provisioning marker that `init` consults before choosing the
fresh-adopt branch. At minimum, make the two entry points agree and correct the
docstring.

## 2. High — Owner audience key silently inherited across an ownership change **[verified]**

**Location:** `org_authority.rs:713` (step 1), `:790` (step 8), `:307` (codec)

Three pieces compose into a cross-org confidentiality break:

```rust
// :713 — the AlreadyOwned org-transfer gate is INSIDE the Some arm
if let Some(existing) = read_optional(&membership_path)? {
    …
    if existing.owner_org != owner_cert.org_id {
        return Err(OrgAuthorityError::AlreadyOwned { … });
    }
}
```

```rust
// :790 — an existing audience key is preserved unconditionally
if !have_audience {
    let audience = OwnerAudienceCredential::generate();
    …
}
```

```rust
// :307 — the credential carries NO org binding
pub fn encode_config(&self) -> [u8; Self::ENCODED_SIZE] {
    buf[0] = OWNER_AUDIENCE_KEY_VERSION;
    buf[1..33].copy_from_slice(&self.audience_handle);
    buf[33..65].copy_from_slice(&self.discovery_key);
    buf
}
```

No membership file means no ownership gate. An existing `owner-audience.key` is
preserved regardless of which org is now adopting. And because the credential
has no org binding, nothing downstream can detect the mismatch.

**Failure scenario.** Node N is owned by org A. Operator transfers to org B. The
`AlreadyOwned` error text (`:540`) instructs *"remove the existing authority
explicitly to transfer"* — so the operator deletes `owner-membership.json`.
Adoption under B then succeeds with zero warnings, and `mesh.rs:8081-8082`
passes `authority.audience.{audience_handle, discovery_key()}` into
`ScopedCapabilityAnnouncement::build_owner`. N now encrypts **org B's private
capability catalog under org A's symmetric key and audience handle**. That key
is explicitly org-wide and operator-redistributed
(`org_scoped_ingest.rs:158`: *"redistribute `owner-audience.key` to every node
in the org"*), so every current and former A node holding the file can decrypt
B's owner-scoped announcements off the wire. `NotForThisOwner`
(`org_scoped_ingest.rs:290`) is an ingest *policy* check on honest nodes — it
does not stop a raw key holder from decrypting captured frames, as that module's
own §9 doc concedes.

**The documented remediation is the exploit path**, which is what makes this
more than theoretical.

**Mirror case, same root cause.** If the operator instead loses
`owner-audience.key` and re-runs `adopt` for a routine same-org renewal,
`have_audience = false` and step 8 mints a brand-new random handle+key, silently
partitioning that node from its own org's scoped discovery (every peer ingest
hits `HandleMismatch`).

**Fix direction.** Bind the credential to an org (add `owner_org` to
`encode_config` and check it at load), and hoist the ownership gate so it also
runs when the membership file is absent but other authority artifacts are
present. Rewrite the `AlreadyOwned` guidance to name an explicit transfer verb
rather than "remove the authority".

## 3. High — Ingest never clamps attacker-chosen `expires_at` **[verified]**

**Location:** `org_scoped_ingest.rs:596-600`; records built at `:444-457`, `:527-539`

```rust
// org_scoped_ingest.rs:599 — the ONLY freshness test on an envelope
fn is_expired(envelope: &ScopedCapabilityAnnouncement, ctx: &ScopedIngestContext<'_>) -> bool {
    ctx.now_secs >= envelope.expires_at().saturating_add(ctx.skew_secs)
}
```

Nothing bounds `expires_at` against `owner_cert.not_after`, `grant.not_after`,
or any maximum announcement TTL, and the record stores the value verbatim
(`expires_at: envelope.expires_at()` at `:452` and `:535`).

The **publisher** enforces exactly that invariant:

```rust
// mesh.rs:8181
let expires_at = base_expiry.min(grant.not_after).min(owner_cert.not_after);
```

This is the classic inversion — the rule is enforced on the honest sender and
not on the untrusted receiver.

**Why query time cannot rescue it.** `VerifiedScopedCapability`
(`org_scoped_ingest.rs:174-198`) retains `provider_cert_generation` and
`grant_signature` but **not the certificate window**, so a cert/grant validity
re-check at read time is structurally impossible. Query-time currentness
(`org_scoped_store.rs:249-251`) re-checks only floors.

**Failure scenario.** A provider with a currently-valid membership cert — an org
insider, or any provider under a malicious grantor org B — publishes one
envelope with `expires_at = u64::MAX`. The consumer stores it with
`expires_at = u64::MAX`. Its cert lapses the following week; **expiry is not a
revocation floor**, so nothing raises one. The private capability of a provider
whose org membership (or whose whole grant) has expired stays discoverable
forever. `saturating_add(skew)` makes `u64::MAX` permanently unexpired. Not
caught by the relay (`org_scoped_relay.rs:292` also compares only against
`expires_at`), the store, either query surface, or any sweep.

**Fix direction.** Clamp at ingest to `min(expires_at, cert.not_after,
grant.not_after)` and additionally retain the binding window in
`VerifiedScopedCapability` so queries can re-validate. The two together also
close §4.

## 4. High/Medium — Permanent capacity wedge of the global scoped store **[reported]**

**Location:** `org_scoped_store.rs:91`, `:134-139`, `:145`

`MAX_ENTRIES = 8192` is one global budget over a single
`BTreeMap<(scope, provider)>` shared by the owner partition **and every
installed grant**. At capacity the only reclaim is `sweep_expired`, which frees
a key only once `now >= tombstone_until` — and `tombstone_until` derives from
the same unbounded attacker-chosen `expires_at` as §3.

**Failure scenario.** A malicious or compromised grantor org B holds a DISCOVER
grant to A with an org-wide `target_scope`. B mints 8192 provider certs (free —
it owns the org key), each publishing one envelope under the grant's discovery
key with `expires_at = u64::MAX`. All 8192 pass `verify_granted_ingest` and
occupy A's store permanently; no sweep can ever reclaim them. A's private
discovery is `AtCapacity` forever, **including for A's own owner-scoped
capabilities**, which share the same budget.

This is specifically a regression surface of the OA3-5 fail-closed change: the
earlier evict-based version self-healed. Fail-closed plus an unbounded,
attacker-controlled retention horizon does not.

**Fix direction.** Close §3, and budget per scope (owner vs each `grant_id`)
rather than one global cap, so one grantor cannot consume the owner partition.

## 5. Medium — 16 colluding identities deny the entire protected surface **[verified]**

**Location:** `org_admission_replay.rs:53`, `:63`, `:313-319`; guard constructed
`with_defaults()` at `mesh.rs:6115` (not configurable)

```rust
pub const DEFAULT_MAX_REPLAY_ENTRIES: usize = 65_536;
pub const DEFAULT_MAX_REPLAY_ENTRIES_PER_CALLER: usize = 4_096;
```

65,536 ÷ 4,096 = **exactly 16 callers saturate the global map**. Once
`st.total >= max_entries` with nothing reclaimable, every *other* caller gets
`CapacityExhausted` → `AdmissionDenied::ReplayCapacity`.

**Failure scenario.** One grantee org mints 16 member entities — 16 membership
certs plus 16 dispatcher grants, a single org-admin action — under one valid
cross-org INVOKE grant. Each issues valid protected calls with novel `call_id`s.
Retention is `proof_expiry + MAX_TOKEN_CLOCK_SKEW_SECS` (`org_admission.rs:569-571`),
up to ~330 s, so holding 4,096 live slots needs only ~12.4 calls/s per identity.
After ~5 minutes the guard is full of unexpired entries and **the provider's own
owner-org callers are denied too**. Denials are `Unavailable`, so honest clients
retry and never recover.

The per-caller ceiling (2026-07-20 §E1.5) is correctly enforced *before* the
global check and correctly reclaims-then-denies — single-caller starvation is
closed. It simply does not compose. `validate()`'s invariant
`per_caller < global` permits the 16× ratio by construction.

**Fix direction.** Either raise the global cap relative to the per-caller cap by
an order of magnitude, or make the per-caller cap a function of observed distinct
callers, or add per-peer admission rate limiting (which also addresses §6/§7).

## 6. Medium — Unmetered signature work before any rate limit, zero credentials required **[reported]**

**Location:** `org_admission.rs:466-488`; denial publish at `mesh_rpc.rs:1160-1171`

A peer needs only a TOFU-pinned direct session — **no org credentials at all**.
It self-mints an `OrgKeypair` X, issues itself a genuinely valid membership cert
and dispatcher grant under X, and attaches a garbage capability grant whose
`issuer_org` field is set to the provider's (public) owner `OrgId`,
`target_scope = ExactNode(P)`, `rights = INVOKE`, `capability =` the invoked
one. Every cheap plaintext check (steps 5, 6, 7) passes — none of them verify a
signature. Step 8 then performs **three `ed25519 verify_strict` operations**
(membership ✓, dispatcher ✓, capability grant ✗) before denying
`CapabilityGrantInvalid`.

Failed admissions consume **no** replay slot by design (`org_admission.rs:572-586`
runs after), so nothing bounds this. There is no per-peer admission rate limiter
in the protected bridge; contrast the public path, which at least has
`may_execute`.

## 7. Medium — Denials became an awaited publish on the serialized bridge task **[reported]**

**Location:** `mesh_rpc.rs:3424-3493` (bridge loop), `:966-1173`
(`admit_and_dispatch_protected`); denial awaits at `:1008`, `:1029`, `:1044`,
`:1056`, `:1083`, `:1162`

Each serve bridge is one `tokio::spawn`ed task running
`while let Some(inbound) = rx.recv().await { … .await }`, so every inbound
frame's gate work — and now a full `publish_response_to_caller().await` per
denial — is strictly serialized behind a 1024-slot `try_send` mpsc that silently
drops on overflow (`:3314`, `:3662`, `:3864`, `:4198`).

**Regression evidence.** On `master` the denial was
`(emit_for_bridge)(meta.origin_hash, meta.seq_or_ts, resp)` — the *sync*
`RpcResponseEmitter` that `try_send`s to the per-service response drainer and
returns immediately. The branch replaced it with `emit_admission_denial(…).await`
directly in the loop. The unary public bridge is a strict regression; the three
streaming bridges gained the gate on this branch, so their await is
new-but-inherent.

Combined with §6, a single session-holding peer imposes ~100–300 µs of serial
cost per packet on a service whose effective concurrency is 1, and legitimate
protected calls are `try_send`-dropped with no diagnostic.

Not an unbounded wedge: `try_publish_to_peer` (`mesh.rs:19309`) is genuinely
non-blocking on backpressure (`WindowFull → SendFailed` rather than parking), so
this is throughput starvation.

**Fix direction.** Route denials back through the existing `resp_tx` drainer as
`master` did, so the gate loop never awaits a publish; consider moving
`verify_org_admission` off the bridge task behind a bounded semaphore.

## 8. Medium — One relay can truncate the flood **[reported]**

**Location:** `org_scoped_relay.rs:307-314`, gate at `:302`

`hop_count` is outside the provider signature by design, and the same `admit()`
decides both "forward" and "ingest locally". A malicious relay re-emits a
legitimate envelope verbatim with `hop_count = 15`. Each victim decodes,
outer-verifies, admits (the gate now holds that identity for
`RETENTION_SECS = 600`), ingests locally — and forwards nothing, because
forwarding requires `hop_count < MAX_CAPABILITY_HOPS - 1`. When the honest copy
arrives over a real path it is a dedup duplicate and is dropped, so it is not
forwarded either. One well-connected relay suppresses propagation to every
subtree behind its victims for the announcement's whole generation.

The origin bind at `:288` only guards `hop_count == 0`, and nothing records the
minimum hop seen or re-forwards on a lower-hop duplicate.

**Fix direction.** Track the minimum `hop_count` seen per dedup key and
re-forward when a strictly lower-hop copy arrives.

## 9. Medium — `sweep_expired` is never driven in production **[reported]**

**Location:** `org_scoped_store.rs:216-226`; only non-test call site is the
at-capacity branch at `:135`

Three consequences: decrypted descriptors of expired entries — plaintext private
capability names — are retained in memory indefinitely past their authorized
lifetime; the 8192 budget silently fills with dead entries, converting into §4
without any attacker; and the generation high-water outlives its designed
tombstone horizon.

On that last point: `capability_version` (`mesh.rs:20095`) is a process-local
counter that resets to 0 on restart, so a provider that restarts re-announces at
generation 1 while consumers hold, say, 500 — every announcement is `Stale`
(`:115`) and the key is never forgotten because nothing sweeps. That provider's
private capabilities are undiscoverable at every prior consumer until its
counter climbs past the old high-water.

## 10. Medium — `GrantedAudience` existence oracle **[reported]**

**Location:** `mesh.rs:2966-2969` (`all_snapshot`), consumed at `mesh.rs:19743`
and `:20200-20206`; gate at `behavior/sensing/scope.rs:99-107`

`all_snapshot()` returns every registered service regardless of visibility, so
`GrantedAudience` tags land in the self-fold entry beside `OwnerScoped` ones —
but the sensing plane's audience gate is keyed on **owner root**, not grant
scope.

**Failure scenario.** Provider P registers `payroll-export` as `GrantedAudience`,
granted only to org B. Peer Q, in P's **own** owner root but holding no grant,
sends a `SensingInterestFrame` with `capability_id = "nrpc:payroll-export"`.
`validate_subscriber_scope` passes because `session_root == local_root`;
`extract_declarers` (`sensing/snapshot.rs:135-155`) matches P's self-entry;
`resolve_candidates` (`sensing/controller.rs:175`) admits it; P registers a
branch and Q receives a warm-start it would not otherwise get. Q behaviorally
confirms the private service exists.

This is scope coarsening — grant scope → owner scope — for **existence only**.
The tag *string* never ships: `CandidateProvider`/`TagAssertion` are not
`Serialize`, no sensing frame variant carries a tag field, and
`resolve_candidates` returns bare node ids. A *foreign*-root peer is refused at
`scope.rs:103` before any observable difference. So this is an oracle bounded to
same-owner-root peers, not a tag disclosure — but it is new on this branch; on
`master` no private tag existed in the self-fold.

## 11. Medium — Windows: the audience key file's own descriptor is never validated **[reported]**

**Location:** `org_authority.rs:1905-1915`

`read_audience_checked`'s `#[cfg(not(unix))]` branch is an `eprintln!` and
nothing else. The Unix side has an explicit re-check gate
(`PermissiveAudienceFile`, `:1898`) whose stated rationale is that
*"creation-time 0600 is insufficient — config management, copying, or manual
edits can weaken it later."*

Windows has no analog. `validate_existing_dir_dacl` (`:1714`) reads the
**directory's** descriptor only; the only `read_object_security` call sites
targeting a *file* (`:2733`, `:3155`) are inside `#[cfg(test)]`.

An explicit, non-inherited ACE on `owner-audience.key` granting
`Everyone: FILE_GENERIC_READ` — from an `icacls` mistake, a restore tool, or the
file being copied in from a share during the org-wide key distribution the
design *requires* — is invisible to every check. This is the residual of
2026-07-20 §20, which closed inheritable ACEs on the *directory*; the machinery
to close it already exists in this file and is applied only to the directory.

## 12. Medium — Windows: no ancestor-chain validation for a pre-existing directory **[reported]**

**Location:** `org_authority.rs:1653-1702`

Unix runs `validate_unix_ancestor_chain` on both the existing-directory path
(`:1614`) and again after creation (`:1643`). The `#[cfg(not(unix))]` branch has
no equivalent: for a **pre-existing** directory it validates only the leaf's
owner and DACL. The module doc at `:1596` claims coverage, but
`create_missing_components_owner_only` only runs when components are *missing*.

Partially mitigated and worth stating precisely: the obvious
plant-a-replacement-directory attack **does** fail closed, because an
attacker-created directory is owned by the attacker and `validate_dacl_view`'s
owner check (`:1760`) refuses it, and a junction/symlink is refused because
Rust's Windows `FileType::is_dir()` is false for name-surrogate reparse points.
The residual is *deletion*, not substitution: a parent-owner who deletes only
`owner-membership.json` leaves the directory intact and passing validation,
downgrading the node to "no owner installed" — which chains directly into §2.

## 13. Medium — Windows: no durability boundary; poison machinery inert **[reported]**

**Location:** `org_revocation.rs:1865-1879`, `:1986-1999`

`fsync_parent_dir` is a no-op on non-Unix, so `write_atomic_phased` can never
return `WritePhase::PostRename`, so `DurabilityUncertain`/poison never arm — and
`std::fs::rename` (`MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`, no
write-through) does not commit the directory entry.

On Windows, `apply_bundle` raising a floor to 9 syncs the temp file's *data*
(`:1965`), lands the rename, publishes, retracts ownership through subscribers,
and returns `Ok`. Power loss seconds later can lose the unflushed metadata
transaction; on restart `open_existing` reads the pre-rename file (floor 5),
publishes it, and reports nothing. Precisely the rollback the module exists to
prevent, with no poison bit and no recovery step. `prove_entry_durable`
(`:1664`) is correspondingly vacuous on Windows.

The comment at `:1980-1985` acknowledges the gap, but the module header at
`:37-40` states the unqualified opposite (*"the parent-directory fsync here is
**not** best-effort: it is part of the durability boundary"*), and no caller
compensates. Fixable: `CreateFileW` on the parent with
`FILE_FLAG_BACKUP_SEMANTICS` + `FlushFileBuffers`; the file already hand-rolls
one `extern "system"` call (`windows_file_identity:759`), so the idiom is
established.

## 14. Medium/Low — Cross-org floors are "absent means zero" **[reported]**

**Location:** `org_admission.rs:471-476`; `org_revocation.rs:100-105`
(`floor_for` → `unwrap_or(0)`)

In `CrossOrgGranted`, `acting_org` is a **foreign** org A, and the floor is
looked up in the provider's own `OrgRevocationState`. Floors are populated only
by `merge_bundle` from operator-distributed, org-signed bundle files. If B's
operators never import A's bundles — the default state — `floor_for(A, member)`
is `0` and `generation < 0` is unsatisfiable, so the revocation check is a
**permanent no-op for every cross-org caller**. There is no distinguishable "no
revocation data for this issuer" state to deny on.

A compromised A member keeps invoking B's protected capability for the life of
its membership cert — up to `MAX_ORG_CERT_TTL_SECS` = 2 years (`org.rs:86`) — and
B has no mechanism to revoke the capability grant it issued either (grants are
explicitly exempt from floors, `org_grant.rs:576-580`). The only live
provider-side kill switch is the `provider_policy` closure.

This is sharper than the 2026-07-20 §D1 documentation note: §D1 says there is no
revocation channel; this says the check that looks like it provides one silently
evaluates to "permit". Either wire cross-org bundle distribution, or make the
absent-floor state explicit and deniable, or document the `provider_policy`
closure as the sole cross-org revocation mechanism.

---

# Part III — New low-severity findings

> **Findings below are stated AS FOUND, at `187ef4213`.** They are retained
> verbatim because they are the evidence for the changes that followed — a
> reviewer evaluating a commit needs the case that motivated it. Present-tense
> claims describe the branch at review time, not now. Disposition and commit
> for every item is in the [remediation table](#remediation-status).

## 15. Low — Nothing enforces `disk ≥ live` **[reported]**
`org_revocation.rs:455-462`, `:1390-1397`. `publish` unconditionally maxes the
incoming state with the outgoing live view, so a persisted state *weaker* than
the enforced view is silently absorbed rather than detected; `apply_bundle` then
builds `merged` from `disk` alone and re-persists that weaker base. Live 9, disk
5, bundle 7 → file rewritten to **7**, below the enforced 9, with no log and no
poison. Restart → floor 7. The store has no path that ever serializes the live
view.

## 16. Low — Poison is process-local **[reported]**
`org_revocation.rs:293-295`, `:1487-1512`. `PATH_POISON` is a
`OnceLock<Mutex<PoisonRegistry>>` — pure process memory — yet the doc claims *"A
restart is one route to that recovery, not the contract."* A restart is in fact
the one action that guarantees the uncertainty is discarded unexamined.

## 17. Low — Poison tombstone launderable by deleting the state file **[reported]**
`org_revocation.rs:1561-1580`, `:1594-1598`. `poison_path_key` falls back to the
non-canonical normalized path when `canonicalize` fails, but `mark_poisoned`
recorded the canonical key — so with both the state file and its `.lock` removed,
`init` sees `was_poisoned == false` and skips `prove_entry_durable`.

## 18. Low — `init` writes through a helper that never poisons **[reported]**
`org_revocation.rs:1020`, `:2008-2019`. `write_atomic` maps `PostRename` to
`DurabilityUncertain` but omits the `mark_poisoned` call `apply_bundle` makes at
`:1418`. `write_atomic`'s own docstring scopes it to *"callers whose files carry
no published live view"* — the state file is exactly the file that does.

## 19. Low — `#[doc(hidden)] pub` test seams ship in release **[reported]**
`org_revocation.rs:1127-1130`, `:1187-1198`, `:480-485`.
`mark_poisoned_for_test` and `arm_publish_pause_for_test` are `pub`, not
`#[cfg(test)]` and not feature-gated. The latter installs a hook that `recv()`s
on an mpsc channel **while `live.write()` is held**, so arming it and never
sending the resume token permanently blocks `barriered_generation()` and
`snapshot_with_generation()` — every admission decision in
`verify_provider_authority`. Not attacker-controlled input, hence Low. Compare
the RED-witness seam, which is correctly `#[cfg(test)]`.

## 20. Low — 2026-07-20 §14's zero-floor fix is enforced at 1 of 3 entry points **[reported]**
`org_revocation.rs:203-228`, `:147-149`, `:458`. `merge_bundle` now skips zero
floors, but `from_file_bytes` still accepts `floor: 0` rows from disk, neither
merge nor publish prunes them, and `publish`'s `or_insert(0)` can itself
materialize one. The install-sweep pathology §14 describes is re-openable.

## 21. Low — Floors are never pruned, capped, or evicted **[reported]**
`org_revocation.rs:129-160`, `:1390-1397`. No capacity bound and no expiry;
each `apply_bundle` is a full O(N) pretty-JSON serialize + write under the
cross-process lock. Not network-reachable (production callers are the adopt
ceremony only), so availability/scaling rather than attacker-driven.

## 22. Low — `seal_descriptor_with_nonce` is public API **[reported]**
`org_scoped_ann.rs:330`. The module chain is fully public, so any downstream
crate can seal under a discovery key with a nonce it chooses. The
envelope-level deterministic builders were correctly gated `#[cfg(test)]`
(`:547`, `:589`, `:618`); this one was not, and its own doc says it exists only
for golden vectors. Two seals sharing `(key, nonce)` give keystream reuse plus
Poly1305 one-time-key recovery — plaintext recovery *and* forgery under an
org-wide owner-audience key. No production caller today; the defect is the
exposed footgun. Should be `pub(crate)` or `#[cfg(test)]`.

## 23. Low — Cleartext framing discloses private-discovery topology **[reported]**
`org_scoped_ann.rs:698-713`, AD at `:181-197`. Beyond the padded ciphertext,
`provider`, `owner_org`, `grant_id`, `audience_handle`, `generation` and
`expires_at` all ship in clear. `grant_id` and `audience_handle` are stable for
the life of a grant, so a relay builds a permanent map of which providers serve
which cross-org grant; the all-zero owner sentinel explicitly labels
owner-scoped envelopes, letting a relay count an org's internal private-service
announcements and track their re-announce cadence. Noted because the module docs
claim relays *"learn nothing"*. `grant_id` is redundant with the handle on the
wire.

## 24. Low — A fail-closed ingest refusal still consumes the dedup identity **[reported]**
`org_scoped_relay.rs:302` primes the gate before `mesh.rs:17435` runs, so the
poison refusal (`mesh.rs:17296`), the publication-race recheck (`:17427`) and
`AtCapacity` all return without storing while that identity stays deduped for
600 s. Self-healing only because the next periodic emission bumps the
generation — incidental recovery, which disappears if generation ever becomes
change-triggered.

## 25. Low — Grant registry sweep is skew-blind **[verified]**
`org_grant_registry.rs:224`: `next.retain(|_, r| r.grant().not_after > now_secs)`
— a bare compare, while every other validity decision in the family goes through
`is_valid_at_with_skew`. `now_secs` is `current_timestamp()`
(`mesh.rs:7883`), so a single forward NTP jump coinciding with one install at
capacity permanently deletes **all 256** records; `granted_envelopes`
(`mesh.rs:8149`) then iterates an empty set with no error surfaced.

## 26. Low — `decode_config` does not reject the reserved zero `grant_id` **[verified]**
`org_grant.rs:396` validates length and version byte only, unlike
`try_issue:816` / `verify:947` / `from_bytes:1008`, which all reject all-zero.
Inert today (a zero-id secret cannot pass `matches_grant`), but zero is
`OWNER_AUDIENCE_GRANT_SENTINEL` (`org_scoped_ann.rs:58`), so the type system
currently permits constructing an owner-credential-shaped grant secret. One line
makes the invariant structural.

## 27. Low — Install moves the secret by value **[reported]**
`org_grant_registry.rs:288-309`. `validate_common` takes the secret by value and
moves it into `GrantAudienceRecord`, which `mesh.rs:7897`/`:7974` moves again
into `Arc::new`. A Rust move is a memcpy that does not run `Drop` on the source,
so `OrgAudienceSecret::Drop` fires only on the final `Arc` release, stranding ≥2
un-scrubbed copies per install. Contradicts the module doc's *"Arc bumps only —
never secret bytes"* (`:31-36`), which is accurate for the map mutation and not
for the pre-`Arc` construction hops. Returning `Box<OrgAudienceSecret>` from
`mint`/`decode_config` would move only a pointer.

## 28. Low — No hardened in-crate loader for `OrgAudienceSecret` **[reported]**
`org_grant.rs:377-404`. `decode_config` is SDK-public (`sdk/src/org.rs:49-54`)
and takes a caller-owned `&[u8]`; the only in-tree pattern an operator can copy
is `cli/tests/org_grant.rs:339`, whose `Vec<u8>` holds the raw key and drops
un-zeroed. Nothing checks the file's mode or that it is a regular file. The
owner-side equivalent *is* closed — `NodeAuthority::open` wraps in
`ScrubbedBytes` and reads through `read_audience_checked`, which uses
`open_regular_nofollow` and gates the mode on the opened descriptor. There is no
production loader at all for the grant side, and the obligation is stated in
neither codec's doc comment.

## 29. Low — Key material via unscrubbed by-value returns **[reported]**
`org_authority.rs:271`, `:307`, `:321`. `generate()`, `encode_config()` and
`decode_config()` all return key-bearing data by value; the callee's local is
never volatile-scrubbed, only the caller's copy. Without guaranteed RVO the
callee stack slot retains the discovery key. Below the bar the module sets for
itself everywhere else.

## 30. Low — Baseline-residue suppression un-suppresses on deregistration **[reported]**
`mesh.rs:19963-19986`. The plaintext strip iterates the *currently registered*
private names, so a `nrpc:X` tag in the operator's own `user_caps` baseline is
suppressed only while `X` is registered private. Drop the `ServeHandle` and the
next re-announce finds nothing to strip and ships `nrpc:X` in the clear. Only
fires when the operator explicitly pre-tagged (merged `nrpc:` tags are never
written back into `user_caps`), and arguably intended — but the
suppression/un-suppression asymmetry is silent and deserves an explicit
decision.

## 31. Low — `call_duplex` missing a deadlock guard its sibling has **[reported]**
`mesh_rpc.rs:4318-4327` guards only `request_window_initial`, then emits
`nrpc-stream-window-initial: 0` verbatim at `:4357-4362`. `call_streaming`
rejects exactly this at `:4451-4457`. Caller-side foot-gun only (a hung duplex
call plus a server-side pump and semaphore held to the deadline), but it is a
genuine call-shape asymmetry.

## 32. Low — Frame body sliced without a local length guard **[reported]**
`mesh_rpc.rs:1042`: `RpcRequestPayload::decode(inbound.payload.slice(RPC_FRAME_BODY_OFFSET..))`
with no preceding length check; `Bytes::slice` panics on an out-of-range start.
Both sibling helpers check (`:933`, `:1222`). Currently unreachable — mesh
ingress requires `decode_rpc_route` to succeed, which needs
`len >= RPC_FRAME_BODY_OFFSET` — but the safety rests on a constant relationship
two modules away, and a panic here would kill the bridge task permanently
(`ServeHandle._bridge` is never joined or restarted), silently retiring the
service.

## 33. Low — Pre-epoch fail-safe doc is wrong about why it is safe **[reported]**
`admission_clock.rs:40-49` says a pre-epoch clock saturating `wall_ns` to 0
means *"admission then treats every finite expiry as in the future, which is
fine."* Traced: with `wall_ns == 0`, `check_expiry_at(0, skew)`
(`org_call.rs:301`) can never expire a proof and its TTL ceiling collapses to
`MAX_ORG_PROOF_TTL + skew`, so proof freshness is entirely defeated. What
actually denies the call is unrelated — `is_valid_at_with_skew(0, skew)` returns
`NotYetValid` for any real certificate. Fail-closed today, for a reason the
comment does not state; any future `ClockSample` consumer checking only an upper
bound would fail open.

## 34. Low — Provider security-posture oracle before any credential check **[reported]**
`mesh_rpc.rs:1078-1094`, `org_admission.rs:249-252`. `verify_provider_authority`
runs before `verify_org_admission`, and its failures map to coarse wire byte `2`
(`Unavailable`) while every credential/binding/replay failure maps to `0`
(`Denied`). Any TOFU-pinned peer with zero org credentials can probe whether the
provider has an authority installed, whether its owner cert is temporally valid
and above its floor, and whether its revocation store is poisoned — useful
attack-timing signal, disclosed pre-credential. The coarse mapping itself is
sound and exhaustive; the issue is the ordering.

---

# Part IV — Test quality (§T1–§T9)

> **Findings below are stated AS FOUND, at `187ef4213`.** They are retained
> verbatim because they are the evidence for the changes that followed — a
> reviewer evaluating a commit needs the case that motivated it. Present-tense
> claims describe the branch at review time, not now. Disposition and commit
> for every item is in the [remediation table](#remediation-status).

The six suites audited here are the ones the 2026-07-20 pass never examined:
`integration_nrpc_protected.rs` (3,744 lines — the branch's main live witness
suite), `nrpc_registration_order.rs`, `nrpc_route_discriminator.rs`,
`nrpc_service_equality.rs`, `org_admission_gate.rs`, `org_admission_wire.rs`.

**Overall: a well-above-average security suite that substantially substantiates
the feature's core claims.** Its strongest asset is genuine positive controls —
the denial matrix is not all-deny, so a coarse `0x0009` really does mean the
gate fired rather than the fixture being dead. Deadlines are set explicitly on
every `call()`, so nothing hangs instead of failing. The §T1 binding section
(`:3542-3650`) is exemplary: it names the exact mutation that left 38 tests
green and builds the test that goes red for it.

The weaknesses are narrower than the volume suggests, but §T1 and §T2 below
should close before this suite is cited as the evidence that the feature is
secure.

## §T1 — `a_proof_ttl_outside_the_ceiling_fails_locally` is non-falsifiable **[verified]**
`tests/integration_nrpc_protected.rs:3671`, assertions `:3712`, `:3718`

Claims the TTL ceiling *"is enforced LOCALLY, before anything leaves the node"*
and that *"no frame reached the provider."* The only assertion on the error is:

```rust
assert!(!matches!(err, RpcError::Timeout { .. }), …);
```

Delete the caller-side check at `mesh_rpc.rs:5518` and the proof is minted with
`ttl = 0`/`31`, shipped over the wire, and the **provider** rejects it
(`org_call.rs:301` → `Expired` for ttl=0; `:307` → `TtlTooLong` for ttl=31 — these
tests run with `verification_skew_secs = 0`). The caller then sees
`RpcError::ServerError { status: 0x0009 }`, not `Timeout`, so `:3712` passes;
the handler is dark, so `:3718` passes. **Fully green with the property under
test removed.** Neither "locally" nor "no frame emitted" is verified; `calls == 0`
is equally satisfied by a server-side denial.

Fix is one line: assert
`matches!(err, RpcError::Codec { direction: CodecDirection::Encode, .. })`.

## §T2 — Handler-darkness counters have no settling window (systemic) **[reported]**
`integration_nrpc_protected.rs:430`, `:501`, `:567`, `:2655`, `:2809`, `:2886`,
`:2969`, `:3079`, `:3323`, `:3414`, `:3502`, `:3618`

Each asserts "the handler stayed dark"; each actually proves the handler had not
*yet* incremented at the instant the denial was observed. On the admit path the
gate calls `fold.lock().apply_inbound_admitted(…)` (`mesh_rpc.rs:1156`) and the
handler body runs on a separate task; on the deny path it calls
`emit_admission_denial(…).await` and returns. A regression doing **both** — apply
then emit — would deliver the denial first and `calls.load()` would read 0 before
the handler task was ever scheduled. There is no happens-before edge.

This leaves **"denies to the caller but still executes the handler"** — the worst
failure mode an admission gate has — outside the suite's reach. The comment at
`:2592` (*"`call` blocks on the denial response, so the witness is race-free"*)
is true only of the correct implementation, which is the thing under test.

Fix: after asserting the denial, poll a bounded window and require the counter
*stays* 0.

## §T3 — `live_two_node_missing_proof_denied` matches the reason byte over its whole range **[verified]**
`integration_nrpc_protected.rs:379`, assertion `:420-423`

```rust
assert!(matches!(message.as_bytes()[0], 0..=2), …);
```

accepts any of the three coarse reasons. A missing proof should deterministically
be `Denied` (0). As written, a regression making a missing-proof denial report
`Unavailable` (2) or `NotSupported` (1) — bytes that disclose provider state a
credential-less caller should not learn (cf. §34) — passes. The sibling poison
test pins exactly `&[2u8]` at `:493-497`, so the precision is available.

## §T4 — `live_two_node_protected_missing_local_capability_denies` cannot prove its ordering claim **[reported]**
`integration_nrpc_protected.rs:2594`, assertion `:2648`. The docstring claims
denial at the possession precheck `has_local_capability`, **not** `may_execute`,
and **before** the admission engine runs. The only assertion is
`status == 0x0009`, which every denial reason produces, and the test carries no
in-test positive control. Would not catch moving the possession check after
credential verification, nor the injected empty v100 announcement (`:2618`)
breaking something else that denies first.

## §T5 — `nrpc_route_discriminator.rs` negatives have no delivery evidence **[reported]**
`tests/nrpc_route_discriminator.rs:259`, `:280`. Both assert "nothing fired"
after a fixed 120 ms sleep in `deliver` (`:181`) plus a further 150 ms, without
establishing the frame reached node B at all. If the subscription, publish
roster or session silently regressed, both pass vacuously — the classic
setup-no-op. The positive control exists only in a different test against a
different fixture instance. Additionally `malformed_route_is_dropped` sends a
24-byte frame that any length check could drop long before the route
discriminator the name credits.

## §T6 — `converge_scoped_count(p, 0)` is satisfied by the initial state **[reported]**
Helper at `integration_nrpc_protected.rs:1056`, used at `:1213`. For `n > 0` the
helper is self-validating; for `n == 0` the target state equals the initial
state, so it returns `true` on iteration 1 if the rebuild has not landed. Rests
entirely on `announce_capabilities().await` having synchronously rebuilt the
scoped emission. Fix: converge to 1 with a matching grant first, then swap and
converge to 0.

## §T7 — `live_two_node_public_capability_unchanged_beside_protected` restates its precondition **[reported]**
`integration_nrpc_protected.rs:2898`, assertion `:2947-2950`. "The public handler
saw no org-admission proof header" — but the call at `:2929` supplies no
`org_proof_intent`, so no header is ever minted. Passes with the public-bridge
stripper deleted outright. Harmless redundancy (the real witness is
`live_two_node_public_handler_never_sees_proof_header` at `:608`, which is
genuinely falsifiable), but contributes no evidence.

## §T8 — `owner_scoped_residue_is_stripped_from_the_plaintext_announcement` proves convergence, not stability **[reported]**
`integration_nrpc_protected.rs:768`, assertion `:792-803`. `wait_until` returns
on the first instant the condition holds, and the comment at `:789` acknowledges
*two* independent re-announce paths. If one regressed to republish
`nrpc:secret`, the test can catch the other's good state and pass while the leak
lands after the assertion. Directly relevant to §30.

## §T9 — `org_admission_gate.rs` collapses four causes to one verdict **[reported]**
`tests/org_admission_gate.rs:186`, `:213`, `:242`, `:304` all assert
`Err(AdmissionDenied::ProviderAuthorityUnavailable)`. Each opens with an
`is_ok()` positive control, which is what makes them falsifiable — this is a
precision limit, not a soundness hole, and the coarseness is by design. Noting
only that a regression crossing these branches is invisible. Separately,
`provider_with_expired_cert_cannot_admit` sleeps 2500 ms against a 2 s cert
(`:229`) — a 500 ms margin on a second-granularity clock under parallel load is
flake-prone.

## Files that are solid

- **`org_admission_wire.rs`** — genuinely strong.
  `encoded_admission_header_carries_no_discovery_key` (`:66`) scans the real
  encoded header for the raw key *and* positively asserts the safe commitment is
  present (`:110-114`), so it cannot pass by the header being empty or malformed.
- **`nrpc_registration_order.rs`** — every assertion synchronous immediately
  after `serve_rpc*` returns, no sleeps, and `assert_published` (`:117`) checks
  both invariants across all four serving shapes.
- **`nrpc_service_equality.rs`** — the best file in the set. Both tests pair the
  confused-deputy negative with a positive control *in the same test against the
  same fixture* (`:236-250`, `:283-297`), and the `request_frame` doc at
  `:103-108` explicitly records a prior version that passed vacuously and fixes
  it by threading the publisher's real `origin_hash`. **This is the discipline
  the rest of the suite should adopt.**

---

# Part V — Build and CI

> **Findings below are stated AS FOUND, at `187ef4213`.** They are retained
> verbatim because they are the evidence for the changes that followed — a
> reviewer evaluating a commit needs the case that motivated it. Present-tense
> claims describe the branch at review time, not now. Disposition and commit
> for every item is in the [remediation table](#remediation-status).

## §C1 — Medium — The branch fails its own clippy gate; the new Windows security job is red **[verified — reproduced]**

**Location:** `capability_bridge.rs:37-38`; `.github/workflows/ci.yml:1052`, `:1746`

```
$ cargo clippy --lib --features cortex -- -D warnings
error: unused import: `super::state::FoldError`
  --> src\adapter\net\behavior\fold\capability_bridge.rs:37:5
error: unused import: `ApplyOutcome`
  --> src\adapter\net\behavior\fold\capability_bridge.rs:38:13
error: could not compile `net-mesh` (lib) due to 2 previous errors
```

**Cause.** `6ec11f81e` (the §12 residual) gated `apply_legacy_announcement`
behind `#[cfg(any(test, feature = "fixtures"))]` at `capability_bridge.rs:289`.
Those two imports exist solely to type its signature
(`Result<ApplyOutcome, FoldError>`). With `fixtures` off and outside `cfg(test)`
the function vanishes and the imports dangle. On `master` the function is
unconditional, so `master` lints clean — this is branch-introduced.

**Why CI does not catch it.** The main clippy job runs
`cargo clippy --all-features --lib --bins -- -D warnings` (`ci.yml:1052`), and
`--all-features` turns `fixtures` on. The feature table confirms `default`,
`net` and `cortex` all exclude `fixtures`; only `--all-features` includes it. CI
therefore lints exactly one configuration and is blind to every configuration
anyone actually builds, including a plain `cargo build`.

**The consequence that matters.** The `windows-security-tests` job — added on
this branch, in `353115c72` — runs
`cargo clippy --features net --lib --bins -- -D warnings` (`ci.yml:1746`), which
excludes `fixtures`. Reproduced:

```
$ cargo clippy --features net --lib --bins -- -D warnings
error: unused import: `super::state::FoldError`
error: unused import: `ApplyOutcome`
error: could not compile `net-mesh` (lib) due to 2 previous errors
```

So the job fails on its clippy step and **never reaches any of its three test
steps** — the `validate_dacl_view` witnesses, the CLI Windows witnesses, and the
`warn_secret_permissions` coverage that the job's own comment calls *"the entire
Windows substitute for the 0600 guarantee."* Two remediation commits interacting;
the branch's newest security coverage is currently inert. Directly relevant to
§11, §12 and §13, all of which are Windows-only and all of which this job exists
to gate.

**Fix.** Gate the imports to match the function, and add a non-`--all-features`
clippy step so the common configurations are linted:

```rust
#[cfg(any(test, feature = "fixtures"))]
use super::state::FoldError;
#[cfg(any(test, feature = "fixtures"))]
use super::ApplyOutcome;
```

## §C2 — Low — The Windows job's test filter silently excludes two security modules **[verified]**

`.github/workflows/ci.yml:1743`:
`cargo test --features net --lib -- adapter::net::behavior::org`

That is a prefix match, and two security-relevant lib test modules live outside
it: `adapter::net::org_admission_gate` (`src/adapter/net/org_admission_gate.rs`)
and the denial-matrix + RED-witness modules in `adapter::net::mesh_rpc`.
Arguably in scope for a job scoped to *Windows-specific* code — but
`cargo test -- <filter>` exits **0** when the filter matches nothing, so a module
rename silently converts this job into a no-op.

That is the same silent-coverage failure mode the sibling
"every `tests/*.rs` is pinned to a step" guard was added in this very diff to
prevent. The rigor is not applied to its own neighbor. Assert a minimum test
count, or pin the module paths the way `tests/*.rs` are pinned.

## §C3 — Low — The Windows job bypasses the new nextest safeguards **[verified]**

The same job uses `cargo test`, not `cargo nextest run`, so it receives neither
the new `slow-timeout = { period = "60s", terminate-after = 3 }` hang protection
nor the `retries = 0` security-suite override — both added in this branch's
`.config/nextest.toml` with well-argued rationales. A hang there burns the
40-minute budget with no named test, which is precisely what the `slow-timeout`
comment says it exists to prevent.

---

# Part VI — Documentation accuracy

> **Findings below are stated AS FOUND, at `187ef4213`.** They are retained
> verbatim because they are the evidence for the changes that followed — a
> reviewer evaluating a commit needs the case that motivated it. Present-tense
> claims describe the branch at review time, not now. Disposition and commit
> for every item is in the [remediation table](#remediation-status).

## §D1 — The 2026-07-20 production claim is an overclaim by six

`docs/misc/CODE_REVIEW_2026_07_20_ORG_CAPABILITY_AUTH.md:3` read
**"20 production findings resolved."** Part I finds six INCOMPLETE — §2
(Critical), §20 (High), §5 (High), §10 (Medium), §17 (Low), §19 (Low).

The header's existing retraction is honest in direction and correctly refuses to
quietly fix what it got wrong; it has now been extended to the production claim.

> **Correction to this document.** An earlier draft of this section also
> asserted that the 2026-07-20 TEST table was stale — that `187ef4213` had
> closed §T2/§T4 without updating it. **That was wrong.** The table reads DONE
> for §T1–§T8, and the commits it cites (`6d52f02e8`, `087a90d07`, `187ef4213`)
> all exist and carry the described work. The claim came from a remediation
> audit that read the table at an earlier revision, and it was relayed here
> without being checked against the file at HEAD — the same failure mode this
> review is elsewhere criticising. It is recorded rather than deleted.
>
> The test-quality findings in Part IV are unaffected: they were derived by
> reading the test code directly, not the table, and §T1/§T2/§T3 were each
> reproduced before being fixed.

Minor, in the same area: `capability_bridge.rs:370-415` — the §12 fix duplicated
a 23-line comment block **verbatim**, back to back, before the
`ann.entity_id.node_id() != ann.node_id` check. No behavioral effect, but it
makes the most security-load-bearing function in the file read as if two checks
exist where there is one. Two log strings also carry collapsed multi-line
whitespace (`capability_bridge.rs`, `mesh.rs:19901`) — these are the
operator-visible strings for the revocation-bind refusal and the
corrective-announce exhaustion. And `cli/src/commands/org.rs:1038`'s doc for
`warn_secret_permissions` still says `--insecure-permissions` suppresses it,
whereas the gate is `args.accept_windows_dacl` — doc residue from the §16 flag
split, now pointing operators at the flag the split existed to steer them away
from.

---

# Verified-clean register

Recorded so a future pass can re-check rather than re-derive. Each of these was
an active hypothesis that a deep-reader tried and failed to break.

**`may_execute` is byte-for-byte unchanged.** Verified two independent ways:
SHA-256 of the brace-matched function body extracted from
`git show master:` and `git show HEAD:` (`f68895147478a11d`, 2,586 bytes, both
sides), and confirmation that the branch diff contains no `+`/`-` line touching
`fn may_execute` or `fn may_execute_with_caller` — the only function added to
`capability_bridge.rs` is `has_local_capability`. `may_execute_batch`,
`membership_passes_post_filter` and `find_nodes_matching` are also identical.

**The `#[cfg(test)]` RED-witness seam is unreachable in release.** Confirmed
independently by three readers. Every symbol — `red_witness_disabled` (field),
`red_witness_admission_disabled()`, `with_red_witness_disabled()`,
`UnaryAdmission::ProtectedRedWitnessDisabled` and all its match arms,
`serve_rpc_protected_red_witness_disabled`, and the bypass `if` at
`mesh_rpc.rs:1122` — carries `#[cfg(test)]` and is `pub(crate)`. No cargo feature
reaches it (`Cargo.toml` has exactly one bypass feature, `fixtures`, unrelated
and default-off). A release build referencing the accessor would not compile.
Integration tests in `tests/` link the non-test-cfg lib, so it is unreachable
there too.

**No scoped tag can reach a plaintext announcement.** Exhaustive enumeration:
exactly three `CapabilityAnnouncement::new` sites exist in `mesh.rs` —
`broadcast_ann` (`:20163`, public-only), `self_ann` (`:20200`) and
`index_self_with_local_services` (`:19748`); only `broadcast_ann` is stored into
`local_emission` (`:20324`). The only serialization of an announcement is
`cached.public.to_bytes()` at `:8381`. `send_emission_to` (`:20449`) is the
single per-peer emit chokepoint, and all four send paths (immediate broadcast,
`broadcast_emission`, late-join unicast `push_local_announcement`, deferred
flush) route through it via `announcement_bytes_for_send_probed`.
`proximity_graph.set_local_capabilities` receives the post-strip set.
Cross-node fold propagation is structurally impossible:
`translate_announcement` returns `SignedAnnouncement::placeholder(…)` — unsigned
— so even a leaked self-fold entry could not be re-broadcast as a verifiable
frame, and `CapabilityMembership::owner` is `#[serde(skip)]`. The aggregator's
`CapabilityFoldSummarizer` emits per-`NodeState` counts only, never tags.

**No authorization bypass in `verify_org_admission`.** `acting_org` comes from
the org-signed membership and is cross-checked against the dispatcher grant
(`org_admission.rs:400-403`); `provider_owner_org`, `provider`, `call_id`,
`invoked_capability` and `request_digest` are all provider-supplied into
`binding_for_verify` and never read from the proof (`org_call.rs:256-281`).
`AnyNodeOwnedBy(X)` can only be issued with `X == issuer` (`org_grant.rs:764-774`),
and `covers()` receives the provider's *proven* owner. `ExactNode` compares the
32-byte `EntityId`, not the grindable 64-bit `node_id`. All three credentials go
through `verify_strict`.

**No new bridge asymmetry.** All four bridges enumerated against each invariant.
`bridge_origin_check`, `may_execute`, proof-header stripping and the
response-route cache are all reached through the single shared
`bridge_preflight` (`mesh_rpc.rs:736-776`), which hands back the *stripped* frame
by value so no bridge can fold an unstripped one.
`reject_relayed_flow_controlled_request` is present on exactly the two bridges
with an upload direction. Cache retirement is unconditional on the two
single-terminal-response bridges and `streaming_response_is_terminal`-gated on
the two multi-fire ones — correct per shape.

**Cryptographic fundamentals.** The five org-family signing domains
(`net-org-cert-v1`, `net-org-floors-v1`, `net-org-dispatcher-grant-v1`,
`net-org-capability-grant-v1`, `net-org-scoped-ann-v1`) are mutually prefix-free,
so unlength-prefixed `domain ‖ payload` is unambiguous. The scoped AD's six
fields are all fixed-width (32/32/32/32/8/8 = 144 bytes), so unprefixed
concatenation cannot alias, and `associated_data()` is recomputed from the same
struct fields that feed the downstream decision — no decoded-vs-used mismatch.
AEAD nonces are 24 bytes of fresh `getrandom` per envelope with `abort()` on
failure, so the org-shared key carries no reuse risk in production paths (cf.
§22 for the public footgun). `ScopedCapabilityAnnouncement::from_bytes` is
verified-by-construction: bounds, version, fixed-offset decode, checked
`prefix + ct_len + 64 == len`, then signature **last**, with no public
constructor yielding an unverified value. Codec offsets were verified
byte-exact against the encoder.

**Replay-guard bookkeeping.** `total` stays in step across the expired-overwrite,
`reclaim_caller` and `reclaim_all` paths (no underflow reachable); unexpired
entries are never evicted; retention strictly dominates the widest acceptance
window `check_expiry_at` can apply; the guard is node-owned so
`(caller, call_id)` is unique provider-wide across services and
re-registrations; cross-provider replay is closed by `callee` in the binding.
(The capacity *composition* issue is §5; the bookkeeping itself is sound.)

**Concurrency in the revocation store.** Canonical lock order (interprocess
sidecar OUTER → `core.reload` INNER) obeyed identically at `:1346-1347` and
`:878-883`; the registry lock is released before `reload` is taken;
`publish_guard_pair:930` dedups same-core via `Arc::ptr_eq` and otherwise orders
by normalized path, so cross-core installs cannot ABBA; `notify` runs outside
both locks. The one structural self-deadlock hazard (holding a `PublishGuard`
then calling `apply_bundle` on the same core) is handled by the `pin_held` flag
threaded through `install_org_revocation_store_locked`.

**Emission coherence.** Public and scoped live in one
`ArcSwapOption<LocalCapabilityEmission>`; `SendEmission` is built from a single
validated load; visibility generation is re-checked *beside* the final stamp
comparison (`mesh.rs:8391-8397`). `scoped_authority` and
`provider_grant_snapshot` hold the original `Arc`s alive, so the pointer
comparison at `:8402-8425` cannot be defeated by allocator reuse — ABA-safe. The
same pinning discipline is correctly applied on the scoped-ingest side
(`:17394-17423`). The visibility epoch is read *before* the projection is
derived (`:19949` vs `:19963`), which is the fail-safe direction.

**Panics from hostile network input.** No reachable panic in production code
across the reviewed surface. `unwrap()` in the wire decoders is uniformly behind
an exact-length check; oversize rejection precedes allocation; no length field
drives an allocation; `getrandom` failure aborts rather than proceeding with weak
material. The one unguarded slice is §32, and it is currently unreachable.

---

# Recommendation

**Remediation is complete; sign-off depends on review, not on more work.**

Every finding is closed across 23 commits, each red-witnessed. Nothing here has
been independently checked, and the process that produced it hit two
verification traps that yielded a false PASS (see
[Corrections](#corrections-to-this-document-and-to-the-work)). Both were caught
by accident rather than by design, which is the strongest argument for a second
pair of eyes on the result.

**Review these first — they are judgements about the deployment, not
properties of the code:**

1. **§5 — the replay-budget partition.** `65_536` total / `16_384` owner
   reserve / `4_096` per external org / `4_096` per caller, keyed on the
   VERIFIED acting org. The rejected alternatives matter as much as the choice:
   raising the global cap only changes the coalition size, and adaptive
   per-org shares cannot be made safe because shrinking a share would require
   evicting unexpired ADMITTED entries — the one thing this structure exists to
   prevent. Note the shipped per-caller and per-org values are EQUAL, so for a
   single identity the caller ceiling binds first; the org quota is
   specifically the coalition bound and engages at ≥2 identities.
2. **§6 — the failed-admission throttle.** `64` burst / `8` per second /
   `4_096` tracked peers. The load-bearing decision is that it charges on
   FAILURE rather than per attempt: an honest caller whose admissions succeed
   is untouched however fast it calls, because the attacker's distinguishing
   property is not its rate but that its admissions fail. Zero refill is
   refused at validation rather than clamped — it would make the first burst
   permanent.

Both ship safe defaults and are operator-configurable
(`MeshNodeConfig::with_admission_replay_config`,
`::with_admission_rate_limit`). Neither is a universal workload limit.

**Then review the three dispositions that are not code changes** — §14, §23,
§16/§17, §27–§29 (BOUNDED) and §30 (DECIDED). Each is a place where the honest
answer was to state a boundary rather than manufacture a fix, and each is
therefore a place where a reviewer might legitimately disagree with the
judgement.

**Two structural checks worth repeating independently**, because they are the
ones that would invalidate the most work if wrong: that `may_execute` is still
byte-identical to `master`, and that no `#[cfg(test)]`/`fixtures` seam is
reachable in a release build. Both are recorded with their method in the
[Verified-clean register](#verified-clean-register) so they can be re-derived
rather than taken on trust.

**Two patterns worth carrying into the next pass**, because between them they
predicted most of what this one found:

- **The propagation gap** (the first review's own finding, which then recurred
  in its remediation): five of six reopened items, and §1, §2, §3, §11, §12,
  §20, §32 among the new ones. *When a fix lands, grep for every sibling call
  site before closing.*
- **Bounds correct in isolation that do not compose**: §4 (one global 8192
  across owner + every grant), §5 (4,096 per caller × 16 = the global cap), §8
  (relay gate), §21 (unbounded floors). *When adding a per-X bound, state what
  N distinct X's do to the global.*

**Two patterns worth carrying into the next pass**, because between them they
predict where the following defect will be:

- **The propagation gap** (the first review's own finding, now recurring in its
  remediation): five of six reopened items, and §1, §2, §3, §11, §12, §20 among
  the new ones. *When a fix lands, grep for every sibling call site before
  closing.*
- **Bounds correct in isolation that do not compose**: §4 (one global 8192 across
  owner + every grant), §5 (4,096 per caller × 16 = the global cap), §8 (relay
  gate), §21 (unbounded floors). *When adding a per-X bound, state what N
  distinct X's do to the global.*
