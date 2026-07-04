# Code review — `mcp-creds` branch (MCP credential forwarding)

**Date:** 2026-07-04
**Branch:** `mcp-creds`
**Base:** `master`
**Scope:** 21 files, +5,205 LOC, 28 commits ahead.
**Plan:** [`MCP_CREDENTIAL_FORWARDING_PLAN.md`](../plans/MCP_CREDENTIAL_FORWARDING_PLAN.md)

The branch adds the credential/header **forwarding** subsystem to the
`net-mesh-mcp` adapter — Phase 0/1/2-seam of the plan. It is spec + primitives,
**not yet wired into a live request path** (no destination-side injection, no
invocation-id replay cache, no mesh key distribution):

- **object + policy** (`forward/context.rs`, `forward/header.rs`,
  `forward/policy.rs`, `forward/target.rs`) — the
  `net.invoke.forwarded_context@1` object, its canonical AAD encoding, the
  secret-wrapper type, the deny-by-default caller/destination policy, and the
  never-for-stdio doctrine;
- **policy store + value backend** (`forward/store.rs`, `forward/secret.rs`,
  `forward/keychain.rs`) — the per-user locked policy store + redaction-safe
  audit, the `SecretBackend` seam, and the OS-keychain backend (non-default
  `keychain` feature);
- **sealing crypto** (`forward/seal.rs`, `forward/aead.rs`, `forward/keys.rs`,
  `forward/assemble.rs`) — the `SealedContext` wire form, the X25519 sealed-box
  sealer/opener (BLAKE2s-MAC KDF + XChaCha20-Poly1305), and forwarding-key
  derivation from the ed25519 identity seed;
- **CLI** (`cli/commands/forwarding.rs`) — `net forwarding
  enable|disable|allow|rm|audit|set-value`.

> **Status — OPEN (2026-07-04).** Findings below reflect the code as reviewed at
> branch HEAD (`6f63173a9`). File/line anchors point at the reviewed code. All
> 78 `forward::*` tests pass; the crypto core and decision logic are sound (see
> *Verified as correct*). No high-severity defect in the live surface; the
> recurring theme is the **value-entry path** (F2–F5) doing none of the
> validation/scrubbing the rest of the module assumes, plus one **write/load
> asymmetry** (F1) where the store doesn't uphold a guarantee it documents.

---

## Overall assessment

Review was five correctness angles + cleanup/altitude/conventions across the
whole diff, three independent finder passes (crypto, policy/decision,
concurrency/IO/CLI), and per-candidate verification.

### Verified as correct (not findings)

- **AAD binding is complete.** Every cleartext `SealedContext` field
  (`sealed_to`, `caller_origin`, `capability_id`, `invocation_id`, `issued_at`,
  `expires_at`, `nonce`, `declared_names`) is bound in
  `canonical_aad_bytes` (`context.rs:293`); a tampered envelope field fails the
  open. The `issued_at` binding regression test is present (`aead.rs:426`).
- **Nonce/key uniqueness holds.** A fresh ephemeral X25519 key per seal makes
  `(key, nonce)` unique without a shared counter; the object `nonce` field is an
  authenticated uniqueness token, not the AEAD nonce.
- **Length-prefix parsers fail closed.** `take` (`aead.rs:257`) compares
  `n > buf.len() - 4` (no 32-bit `4 + n` wrap), `deserialize_headers` rejects an
  oversized value before copying it, no `split_at`/underflow panic.
- **Expiry is consistent.** `is_expired` (`seal.rs:106`, `>=`) matches
  `validate`'s strict `expires_at > issued_at`.
- **Decrypted-vs-declared mismatch is caught.** `open` reconstructs with the
  authenticated `declared_names` then `validate()`s, so a decrypted header set
  disagreeing with the AAD-bound names fails (`aead.rs:211`).
- **KDF domain separation holds.** `net-mcp-forward-key`,
  `net-mcp-forward-nonce`, `net-mcp-forward-x25519-v1` are distinct; no collision.
- **Seal/open zeroize before returning** on both success and error paths
  (`aead.rs:107`, `aead.rs:190`); `StaticSecret`/`SharedSecret` carry the
  `zeroize` feature.
- **Decision logic is sound.** Kill switch checked first in both
  `decide_secret`/`decide_plain`; `permits` requires capability-glob AND exact
  provider; `ProviderScope::Any` rejected for secrets at write, load, and
  decision time; `resolve_secret_send` checks policy strictly before the backend;
  header names canonicalized before every check; `glob_match` is the standard
  linear backtracker (`github.*` does not match `github-evil.x`). No kill-switch
  bypass, glob over-match, case-smuggling, or Any-reaches-a-secret path.
- **Lock-free reads are safe.** `mutate` holds the cross-process `LockGuard`
  across load→apply→save; a lock-free `load` racing a `save` never tears because
  the write is temp-file + atomic rename.

### Refuted candidate

- **Windows rename race (REFUTED).** The claim that a lock-free `load` opens the
  store without `FILE_SHARE_DELETE`, so a concurrent `save`'s replace-rename
  fails with a sharing violation, does not hold: Rust's `File::open` defaults to
  a share mode of `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`, so a
  reader does not block the `MoveFileEx`-replace. The design is safe on both
  platforms.

---

## Findings

Ranked most-severe first. No high-severity defect; F1–F5 are the ones to fix
before this graduates from seam to a live forwarding path.

### F1 — load-time revalidation skips `validate_ref_name` (documented guarantee unmet)

**`forward/store.rs:221`** · correctness / invariant · **medium**

The load loop re-runs `SecretPolicy::validate` / `PlainHeaderPolicy::validate`
but never `validate_ref_name`, even though the module doc (`store.rs:22-25`) and
the load comment (`store.rs:214-220`) claim load enforces "the *same* rules the
write-time mutators enforce … a ref name shaped like a secret value."

```rust
for (ref_name, policy) in &file.forwarding.secrets {
    policy.validate(ref_name).map_err(|e| corrupt(e.to_string()))?;   // header/cookie/provider-any only
}
```

`set_secret` (`store.rs:341`) calls `validate_ref_name` (charset / length /
lowercase-slug); `load` does not.

**Failure scenario:** hand-edit `forwarding.json` with a secret keyed by a raw
token — `{"secrets": {"ghp_LIVETOKENvalueABC123": {"header": "Authorization",
"allow": {"providers": ["node-1"], "capabilities": ["*"]}}}}` (uppercase,
value-shaped; or empty; or >64 chars). `SecretPolicy::validate` passes, so
`load()` accepts it as active. The ref name then flows verbatim into
`audit()` → `render()` (`store.rs:405`, `store.rs:514`), leaking a token-shaped
string through the surface documented as "value-free by construction"
(`store.rs:398`), and reaches an active state the invariant says is impossible.

**Fix:** call `validate_ref_name(ref_name)` in the load loop (map to
`Corrupt`), symmetric with `set_secret`.

---

### F2 — value-backend `set` skips the validation `get` enforces

**`forward/keychain.rs:59`, `forward/secret.rs:104`** · correctness / robustness · **medium**

Both `set` paths store raw bytes; `get` wraps them in `ForwardedHeaderValue::new`
(`keychain.rs:98`, `secret.rs:140`), which rejects values over
`MAX_HEADER_VALUE_LEN` (8 KiB) or with control characters. So a value that can
never be read back is accepted at entry and errors only later at forward time
(`resolve_secret_send` → `ResolveError::Backend`).

**Failure scenario:** an operator pipes a value over 8 KiB (a large JWT / PEM) or
one with an embedded newline (`net forwarding set-value` strips only *one*
trailing `\n` at `cli/commands/forwarding.rs:141-146`, leaving an interior one).
`set` succeeds silently; every later `get` fails as `ValueTooLong` /
`ControlCharInValue`, so forwarding permanently and silently fails with no error
at entry time — violating the "validate at write time so the store can never
hold what the decision path must reject" doctrine (`store.rs:22-25`).

**Fix:** validate through `ForwardedHeaderValue::new` (or the same length /
control-char checks) in the `set` path so a bad value is rejected at entry.

---

### F3 — `set-value` stores under the raw, unvalidated ref name

**`cli/commands/forwarding.rs:152`** · correctness / robustness · **medium-low**

`backend.set(&args.ref_name, &buf)` bypasses the lowercase-slug / length rules
`validate_ref_name` (`store.rs:540`) enforces on the policy side.

**Failure scenario:** `net forwarding set-value Github-Token` writes a keychain
account `Github-Token`, but the matching policy `allow` slug is `github-token`
and `resolve_secret_send` looks the value up by the policy ref — the two keys
never coincide, so the credential silently resolves as `ValueMissing` and never
forwards, with no error at entry time.

**Fix:** run `set-value`'s `ref_name` through the same `validate_ref_name` rules
(or canonicalize to the slug) before writing to the backend.

---

### F4 — stdin secret scrubbed with a naive, elidable loop

**`cli/commands/forwarding.rs:154`** · secret hygiene · **medium-low**

```rust
buf.iter_mut().for_each(|b| *b = 0);
```

This is exactly the pattern `header.rs:245-264` documents `zeroize_vec` was
written to defeat: it overwrites only the last allocation's live `len()` bytes
with a store the optimizer may dead-store-eliminate, and misses the copies
`read_to_end` left in reallocated/freed buffers plus the tail capacity.

**Failure scenario:** `read_to_end` grows `buf` through several reallocations,
each freeing an un-scrubbed copy of the partial secret; the final loop overwrites
only the last allocation's `len()` bytes with an elidable store — so secret
residue survives in freed heap and in spare capacity.

**Fix:** use a volatile, full-capacity scrub. `zeroize_vec` is `pub(crate)` in
`net-mcp`, so either expose a zeroizing helper from the crate or pull in the
`zeroize` crate at the CLI.

---

### F5 — keychain `set`'s `value.to_vec()` copy is never zeroized

**`forward/keychain.rs:61`** · secret hygiene · **low-medium**

Unlike `contains` (`keychain.rs:111`), `get`, and `ForwardedHeaderValue`, the
fresh `Vec` copy `set` allocates and hands to `keyring` is dropped without
overwriting, leaving plaintext in freed process memory — the one un-scrubbed
write path in a module whose whole premise is scrubbing secret buffers.

**Fix:** scrub the moved copy after `set_secret` returns (wrap it, or
`zeroize_vec` it in the closure).

---

### F6 — sealer doesn't reject a low-order / identity recipient key

**`forward/aead.rs:89`** · crypto hardening / defense-in-depth · **low-medium**

`x25519-dalek` v2 does not reject non-contributory points
(`SharedSecret::was_contributory()` is never called). If a node's *published*
forwarding key is a small-order point (e.g. 32 zero bytes),
`shared = diffie_hellman(eph_sk, recipient_pk)` is the all-zero identity
regardless of `eph_sk`, so `key = derive_key([0u8;32], KDF_DOMAIN_KEY)` is a
fixed public constant and `nonce = derive_nonce(eph_pk, recipient_pk)` derives
from public values only — any passive observer of the sealed blob recomputes the
identical key+nonce and decrypts the bearer token in `ciphertext`.

**Feasibility:** requires a malicious / MITM'd announced key, which the plan
defers to the out-of-scope key-distribution layer (announcement integrity), and
standard sealed boxes skip this check too. But the fix is a cheap local check and
the payoff is full plaintext-credential disclosure — the module's primary
guarantee.

**Fix:** at the seal boundary (`aead.rs:89-91`) reject the all-zero / known
low-order recipient encodings (or check `shared.was_contributory()`) before
deriving the key.

---

### F7 — `SECURITY_SENSITIVE` covers only three names

**`forward/header.rs:40`, `forward/policy.rs:389`** · policy hardening · **low**

Only `authorization` / `cookie` / `set-cookie` are gated off the plain path.
`x-api-key`, `x-auth-token`, `x-amz-security-token`, etc. are treated as
non-sensitive, so they can be configured as a `plain_header` with
`providers: any` and forwarded to arbitrary destinations, and a destination
`AcceptPolicy` accepting one carries no `accepts_forwarded_credentials` tag
(`policy.rs:492`, `target.rs:89`) — hiding the credential surface honest-labeling
is meant to expose.

**Feasibility:** requires deliberate operator config (the plain path still needs
`enable` + provider/capability match), so lower severity, but it's a real hole in
the plain-path guarantee.

**Fix:** widen the sensitive-header set, or make plain-header classification a
denylist-plus-heuristic (`x-*-key` / `x-*-token` / `x-*-secret`) rather than a
3-item allowlist.

---

## Lower-severity / robustness

### F8 — `save` is atomic but not durable

**`forward/store.rs:280`** · robustness · **low**

`save` calls only `f.flush()` (a no-op for durability on an unbuffered `File`),
with no `f.sync_all()` and no parent-directory fsync. On power loss / crash right
after `rename` returns but before the tmp file's data blocks commit (not
guaranteed on all filesystems), `forwarding.json` can be left truncated / zero
length; the next `load` returns `StoreError::Corrupt`, which fails closed and
bricks every `net forwarding` verb until the file is manually deleted.

### F9 — header **name** bytes excluded from the size budget

**`forward/context.rs:210`, `forward/header.rs:96`** · resource accounting · **low**

`validate` sums only `ForwardedHeaderValue::len()` (values) against
`MAX_TOTAL_FORWARDED_BYTES`; `HeaderName::parse` caps neither name length. A
context with 16 headers (≤ `MAX_FORWARDED_HEADERS`) whose values are tiny but
whose names are each multi-megabyte passes `validate`, and `serialize_headers` /
`canonical_aad_bytes` then emit an unbounded-size AAD and plaintext. Names are
still AAD-bound — this is a size/accounting gap, not a binding defect.

### F10 — `ForwardingFile` lacks `deny_unknown_fields`

**`forward/store.rs:177`** · robustness · **low**

The on-disk wrapper `ForwardingFile` is not `#[serde(deny_unknown_fields)]`
(unlike the inner `ForwardingConfig`), so a typo'd top-level field is silently
ignored rather than rejected.

### F11 — temp file orphaned on a write/flush error

**`forward/store.rs:276`** · cleanup · **low**

If `write_all` / `flush` errors after the temp file is created (disk full,
quota, transient I/O), `save` returns `Err` without removing
`<store>.tmp.<pid>`; the orphan persists in the per-user data dir (cleaned only
if the same PID is later reused).

### Also noted (marginal)

- **`context.rs:327`** — `write_field` uses `u32::try_from(...).expect(...)`. The
  `#[expect]` rationale ("a field that large is a logic bug, not runtime input")
  holds on the *seal* side but not the *open* side: `open` → `sealed.aad()` runs
  `write_field` over wire-supplied envelope fields, so a >4 GiB field would panic
  the opener rather than return `OpenError`. Not reachable today (no
  `SealedContext` wire-decode with length limits exists yet); worth a bounded
  decode when that lands.
- **Dependency hygiene** — the new crypto deps add duplicate major versions to
  the workspace lock (`chacha20poly1305` 0.10 *and* 0.11; a third `getrandom`,
  0.4.3, alongside 0.2/0.3). The `Cargo.toml` comment "already resolved in the
  workspace lock via core" is inaccurate for these; reusing core's locked
  `chacha20poly1305` 0.10 would trim build time and binary size.

---

## Suggested order

F1–F5 (the value-entry path + the store write/load asymmetry) are the
ship-blockers for a live forwarding path. F6 is cheap crypto defense-in-depth
worth landing alongside. F7–F11 are hardening / robustness that can follow.
