# Net v0.26 — "Monkey Business"

*Named after Skid Row's 1991 single — the opening track and lead-off shot from Slave to the Grind, the record that blew up the band's bubblegum-metal reputation and, in the same swing, became the first hard-rock album to debut at number one on the Billboard 200 in the SoundScan era. Their 1989 debut had floated on power ballads — "18 and Life," "I Remember You" — and the label wanted more of the same; the band handed back a heavier, meaner, downtuned record and put "Monkey Business" first, Rachel Bolan and Snake Sabo's swampy, menacing strut, all swagger and trouble grinning in the doorway.*

## A full-surface security pass, and the eight places code drifted from its own safety protocols

v0.26 is a security hardening release. It is the result of a full-surface review across the parts of the crate where a mistake costs the most: wire-protocol parsing, the crypto primitives, the C-ABI FFI boundary, identity / token / auth, on-disk storage, and the client SDKs.

Most of the classic traps — the off-by-one slice, the unchecked length prefix, the malleable signature, the path-traversal write — already carry an explicit guard and a regression test pinning it. The eight issues that came out of the pass cluster in one place: where a single piece of code diverged from a safety protocol the rest of the codebase already follows. A blob handle that skipped the quiescing dance every other handle does. An inbound length cast the wide way in one binding and the narrow way in another. A token expiry that had a saturating add but no ceiling. The fixes mostly amount to making the outlier match the rule.

**A blob handle that didn't play by the handle rules.** The crate documents a per-handle quiescing protocol for exactly one hazard: a foreign thread (a Go cgo callback, a Python thread, a Node worker) sitting inside an FFI call while another thread frees the same handle. Every mesh / cortex / redis handle embeds a small guard, gates each operation on it, and on free *leaks the handle box* rather than deallocating it — so a racing call always lands on valid memory, sees the "freeing" flag, decrements, and bails. The mesh blob-adapter handle was the one that never got the treatment: it carried only the inner pointer, and its free did an unconditional deallocation. A store / fetch / exists racing a free read freed memory; a second free was a double-free. v0.26.0 embeds the guard, gates every operation on it, and makes free leak the box and drop only the inner — the adapter now follows the same recipe as every handle around it. A regression test pins both properties: an operation on a freed handle returns the null-pointer code instead of corrupting memory, and a double-free is a no-op.

**An inbound length cast the narrow way.** Inbound nRPC request bodies and the MeshOS causal-event / snapshot-restore payloads were copied from the native buffer with a 64-bit size cast down to a 32-bit signed int. A length with the high bit set went negative and crashed the copy before the handler's panic recovery could catch it; a length at or past 4 GiB modulo 2³² produced a short copy — a truncated body whose framing still claimed the original size, a clean parse-desync primitive. Both are reachable from whatever a peer puts on the wire. One binding file already did this correctly — checking the length against the platform-int maximum and copying through a wide slice — but the inbound trampolines had not been updated, in two separate binding copies. v0.26.0 routes every inbound site through one guarded helper that rejects an over-range length and copies through a wide slice, applied to both copies.

**Tokens that could outlive the heat death.** A permission token's expiry was a saturating add of issue-time plus requested duration, with no cap on the duration — a caller could mint a token with a TTL of `u64::MAX`, whose expiry saturated into a timestamp that never arrives. The only way to retire such a token is an advisory revocation floor that has to be distributed out of band and that a given node might never learn to bump. v0.26.0 rejects any TTL past a one-year ceiling at issue time with a typed `TtlTooLong` error. Delegation only ever copies a parent's expiry, so the bound holds transitively down the whole chain. Long-lived grants now have to be periodically re-issued — which re-checks the issuer's signing key and current policy — and the blast radius of any single leaked token is capped at a year.

**Constructors that skipped the guard.** The registry-client, fold-query-client, and channel-registration entry points, plus the blob-adapter constructor, dereferenced the inner mesh / redex node after only a null check, with no free-race guard. A concurrent free that won its race left them reading a dropped pointer. Same class as H1, narrower blast radius — these run before the handle is widely shared. v0.26.0 gates each on the relevant handle's guard; the node-clone accessors now hold the guard across the clone and return an `Option`, and every caller surfaces a null / error result when the handle is being torn down.

**Clock skew with no ceiling.** The token cache's clock-skew tolerance — a knob for absorbing NTP and container-clock drift — accepted any value. A large skew symmetrically widens every token's validity window: an expired token stays accepted for that many extra seconds, across the whole cache. The default is strict (zero), so this was misconfiguration-gated rather than on by default, but there was no guardrail. v0.26.0 clamps the tolerance to five minutes, which comfortably covers real drift while keeping a fat-fingered config from turning the expiry check into a rubber stamp.

---

## Test hygiene

- **Every fix that could carry a regression test does.** The H1 fix pins that an operation on a freed handle bails with the null-pointer code and that a double-free is a no-op. The H3 fix pins rejection at and past the TTL ceiling and a valid, non-saturating token at exactly the ceiling. The M2 fix pins the skew clamp on both the constructor and the setter. The L3 fix plants a symlink to an out-of-root secret and asserts that fetch, exists, and stream all refuse it.
- **A follow-up review caught two things the fixes themselves introduced.** Bounding the TTL turned the SDK's infallible token-issue helper — which unwraps the fallible path — into a panic on an over-long TTL; it now soft-clamps to the ceiling instead, matching the existing zero-TTL soft-clamp, with its own release / debug / fallible test trio. The new read-path symlink test was gated to the platforms that can plant a unix symlink, and the blob existence probe re-applied its regular-file contract so a directory sitting at a blob slot is not reported as present.
- **The full library test suite passes**, including the new regression tests.

---

## Breaking changes

### `TokenError` has a new `TtlTooLong` variant

Additive, but `TokenError` is a plain enum — downstream code that matches it exhaustively without a wildcard arm will need a new arm for the variant. The binding error-string maps were updated in lockstep (`ttl_too_long`).

### Token TTL is capped at one year

`try_issue` returns `TtlTooLong` for any duration past the one-year ceiling; the infallible `issue` wrapper panics on it (use `try_issue` for untrusted input). The SDK's infallible `issue_token` soft-clamps to the ceiling rather than panicking. Callers that were minting multi-year or never-expiring tokens must re-issue inside the bound or move to a periodic re-issue.

### Clock-skew tolerance is capped at five minutes

`TokenCache::with_clock_skew` / `set_clock_skew` clamp any larger value to five minutes. A config that set a larger skew silently receives the clamp.

### New public constants

`MAX_TOKEN_TTL_SECS` (one year) and `MAX_TOKEN_CLOCK_SKEW_SECS` (five minutes) are exported from the identity module for callers that want to check before they call.

---

## How to upgrade

1. **Most consumers — bump the dependency.** The fixes are on by default and need no source changes unless you mint tokens with very long TTLs, configure a large clock skew, or match `TokenError` exhaustively.

2. **Token issuers — check your TTLs.** Anything past one year is now rejected on the fallible path and clamped on the SDK's infallible path. If you were relying on a never-expiring token, switch to a periodic re-issue — that is the point of the cap. `MAX_TOKEN_TTL_SECS` is the ceiling to check against.

3. **Anyone matching `TokenError` — add the `TtlTooLong` arm.** Exhaustive matches without a wildcard will not compile until you do.

4. **Operators who tuned clock skew — confirm your value.** Anything above five minutes is now clamped to it. If you genuinely needed a wider window you were papering over a clock problem; fix the clock instead.

5. **Foreign-language callers sharing handles — no API change, but the race is now safe.** Sharing a blob-adapter handle across threads and racing a free against an in-flight call no longer corrupts memory — the racing call bails with the null-pointer code. No code change required.

6. Wire format is unchanged; v0.25 and v0.26.0 peers handshake cleanly.

---

Released 2026-05-28.

## License

See [LICENSE](../../LICENSE-APACHE).
