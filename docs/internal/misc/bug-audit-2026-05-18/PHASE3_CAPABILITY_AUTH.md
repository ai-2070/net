# Phase 3 — Capability / Identity / Auth Surface Audit (2026-05-18)

**Crate:** `ai2070-net v0.18.0`
**Scope:** `src/adapter/net/identity/{entity,token,envelope,origin,mod}.rs`,
`src/adapter/net/behavior/capability.rs`,
`src/adapter/net/channel/{name,config,guard}.rs`,
`src/adapter/net/mesh.rs` (handle_capability_announcement, authorize_subscribe,
publish_many).
**Test oracles read:** `capability_broadcast.rs`, `capability_multihop.rs`,
`capability_scope.rs`, `capability_schema_doc_guard.rs`, `channel_auth.rs`,
`channel_auth_hardening.rs`.

The prior `CODE_REVIEW_2026_05_10_CAPABILITY_SYSTEM_2*.md` passes focused on
`Tag` canonicalization, schema validators, placement scoring and binding
parity. They did not deeply audit the cryptographic token surface, the
multi-hop dispatch ordering, or the channel-hash collision threat model.
The findings below are new.

## Findings (severity-ordered)

### F-1 — Token channel binding uses non-cryptographic `xxh3 → u32`; attacker-controlled name registration grants cross-channel access
- **File:line:** `src/adapter/net/channel/name.rs:31,144` (`ChannelHash = u32`,
  `channel_hash` via xxh3 truncated to 32 bits);
  `src/adapter/net/identity/token.rs:328-336` (`PermissionToken::authorizes`
  compares `self.channel_hash == channel`);
  `src/adapter/net/channel/config.rs:111-153`
  (`can_publish`/`can_subscribe` call
  `token_cache.check(entity_id, _, self.channel_id.hash())`).
- **Severity:** high
- **Bug class:** scope confusion (1) + canonicalization (8)
- **What:** `PermissionToken` is bound to `channel_hash: u32` derived from
  `xxh3_64(name) as u32`. Two channel names that collide at u32 are
  authorization-equivalent in the token path. xxh3 is **non-cryptographic**;
  finding a name that hashes to a target channel's u32 is ~2^32 work on a
  modern CPU (seconds-to-minutes). `AuthGuard::is_authorized_full` has the
  exact-name backstop, but `TokenCache::check` does not — the publish/
  subscribe paths consult `token_cache.check(..., channel_hash)` and accept
  any token whose `channel_hash` collides at u32 with the target. Name-level
  comments at `name.rs:9-10` acknowledge ~65 K birthday risk but assume
  random names; under adversarial channel-name registration that assumption
  is wrong.
- **Attack scenario:** Attacker registers (or convinces a victim to register)
  a channel `evil.<suffix>` whose name xxh3-truncates to the same u32 as a
  high-value channel `prod.deploy-keys`. Attacker holds a valid
  `PermissionToken(scope = SUBSCRIBE, channel_hash = H)` issued for
  `evil.<suffix>`. Subscribing to `prod.deploy-keys` invokes
  `cfg.can_subscribe → token_cache.check(entity, SUBSCRIBE, H)` which finds
  the colliding-hash slot and authorizes.
- **Fix sketch:** Widen `ChannelHash` to a cryptographic 128-bit digest
  (BLAKE2s of the name) in `PermissionToken`, or bind tokens to the full
  `ChannelName` string (mirror `AuthGuard::exact`). A surgical fix: in
  `ChannelConfig::can_{publish,subscribe}`, after `token_cache.check`
  passes, cross-verify the cached token's stored `channel_hash` against a
  freshly-rehashed name and refuse on mismatch — but the underlying token
  signature still covers only `u32`, so the colliding token is genuinely
  valid; the only durable fix is to bind tokens to the name (or a wider
  digest).

### F-2 — No token revocation; delegated child outlives any "revoke" intent
- **File:line:** `src/adapter/net/identity/token.rs:353-422`
  (`PermissionToken::delegate` copies `parent.not_after` into child;
  `child.delegation_depth = parent - 1`); `TokenCache` (`:551-732`) exposes
  only `evict_expired`, no revocation list, no parent→child link.
- **Severity:** high
- **Bug class:** token lifetime / parent-child delegation (6)
- **What:** Once a parent token is delegated, the child carries its own
  signature from the delegator and its own `not_after` copied verbatim from
  parent. If the original issuer decides to "revoke" the parent (e.g., key
  rotation, compromise), nothing in the substrate notifies anything to drop
  the child. The cache only drops on natural expiry. `is_valid()` checks
  `verify()` against `self.issuer` (the delegator, not the root issuer) —
  no chain walk, no CRL.
- **Attack scenario:** Root issues a 1-year token to A with `DELEGATE`. A
  delegates to attacker B with 1-year lifetime. Operator detects A is
  compromised, rotates A's key. B's token still verifies (signed by A's
  *previous* key — every cache that holds A's old `EntityId` still treats B
  as valid). No surface to invalidate B except waiting for natural expiry.
- **Fix sketch:** Add a `revoked_nonces: HashSet<u64>` per cache (or a
  per-issuer epoch counter cross-checked at `check()`); have
  `PermissionToken` carry a `parent_nonce: Option<u64>` so a CRL entry on a
  parent drops every descendant in O(chain_depth) at check time.
  Alternatively, cap `not_after` at delegation time to `min(parent.not_after,
  now + DELEGATION_MAX_TTL)` so the operator window stays bounded.

### F-3 — Forwarded announcement DoSes legitimate TOFU pin via shared `(node_id, version)` dedup key
- **File:line:** `src/adapter/net/mesh.rs:5145-5153` (dedup early-return)
  + `5221-5233` (TOFU pin gated on `signature_verified && hop_count == 0`)
  + `5270-5271` (dedup INSERT after all checks).
- **Severity:** medium
- **Bug class:** capability announcement broadcast (7) + multihop trust (3)
- **What:** Dedup keys on `(node_id, version)` only. A forwarded
  (`hop_count > 0`) announcement carrying a victim's signature lands first
  and inserts dedup. When the victim's own direct (`hop_count == 0`)
  announcement with the *same* `(node_id, version)` arrives, it hits the
  dedup check at line 5151 and returns BEFORE reaching the TOFU pin at
  line 5221. The victim's `from_node → entity_id` binding is never written,
  so `peer_entity_ids.get(victim_node_id)` returns `None` and any
  `require_token` channel-auth keyed on that binding fails closed
  (mesh.rs:5906-5913). The test
  `forwarded_announcement_does_not_tofu_pin_forwarder_to_victim_entity`
  pins that the **forwarder** isn't bound; it does not pin that the
  **victim** still gets bound after a forwarded race.
- **Attack scenario:** Attacker harvests a victim's signed announcement
  bytes (e.g., from an earlier multi-hop forward), bumps `hop_count` and
  ships it to a relay R *before* the victim handshakes directly with R.
  Dedup is poisoned. Victim then handshakes with R and emits the same
  signed bytes (same `(node_id, version)` since version monotonicity holds
  during one process lifetime) — R drops the direct ann silently. Victim
  cannot subscribe to any `require_token` channel on R until version
  increments and the next announce.
- **Fix sketch:** Either (a) move the dedup insert to AFTER the TOFU pin
  and special-case "this dedup hit had `hop_count > 0` and the incoming
  arrival has `hop_count == 0`" to upgrade the binding state, or
  (b) widen the dedup key to `(node_id, version, hop_count == 0)` so a
  direct ann is never short-circuited by a prior forwarded copy.

### F-4 — `CapabilityAnnouncement::signed_payload` swallows serialization errors and signs empty bytes
- **File:line:** `src/adapter/net/behavior/capability.rs:2072-2086`
  (`signed_payload` → `to_bytes` → `serde_json::to_vec(self).unwrap_or_default()`
  at line 2104-2106).
- **Severity:** medium
- **Bug class:** signature verification / construction (5)
- **What:** `signed_payload` calls `to_bytes`, which uses
  `serde_json::to_vec(self).unwrap_or_default()`. On serialization failure
  the empty `Vec` is returned and the signer signs `b""`. Both signer and
  verifier hit the same empty payload, so the signature still "verifies"
  on the receiver — but every announcement that hits the failure path
  signs the **same constant transcript**, making the signature
  meaningless for that announcement: a single captured signature replays
  across every other announcement that triggers the same failure mode.
- **Attack scenario:** Reachable today only via OOM (serde_json on
  `String`/`u64`/`BTreeMap<String,String>` is otherwise infallible). Latent
  hazard: any future addition of a type with a fallible `Serialize` impl
  (e.g., a `Tag` variant that rejects on `to_string`) would expand the
  failure surface into something a peer could trigger by crafting a
  metadata value.
- **Fix sketch:** Propagate the error: change `signed_payload` to return
  `Result`; have `sign`/`verify` surface a typed `SerializationError` and
  refuse to sign/verify on serialization failure. Even simpler: switch the
  signed transcript to a length-prefixed canonical encoding (`bincode`
  with a stable schema) so the bytes are not JSON-driven.

### F-5 — User-author `with_metadata` does not gate exact-match reserved keys; inbound peers can also stamp them
- **File:line:** `src/adapter/net/behavior/capability.rs:1156-1166`
  (only `metadata_reserved_prefixes` checked, not `metadata_reserved`);
  `src/adapter/net/behavior/schema.rs:364`
  (`METADATA_RESERVED_KEYS = {"intent","colocate-with","priority","owner"}`);
  consumer: `src/adapter/net/dataforts/greedy/admission.rs:168`.
- **Severity:** medium
- **Bug class:** scope confusion (1)
- **What:** `with_metadata` enforces only the *prefix* reserved list. The
  exact-match list (`intent`, `colocate-with`, `priority`, `owner`) is
  intentionally writable by user code (per doc-comment), but the same
  metadata field is **also populated by deserializing an inbound peer's
  `CapabilityAnnouncement`**, and that path runs no gate at all. Greedy
  admission then reads `chain_caps.metadata.get("intent")` to choose
  placement. A peer announces a fabricated `intent` value to steer
  admission to itself.
- **Attack scenario:** Attacker announces caps with
  `metadata.intent = "high-priority-tenant-X"`. The receiving node's
  greedy-admission picks attacker as the placement target for X's
  workloads.
- **Fix sketch:** Either treat the exact-match reserved keys as
  substrate-only (drop them on inbound deserialize from non-self peers),
  or add a per-key trust policy (read `intent` only from peers whose
  `entity_id` matches a configured allowlist).

### F-6 — Channel-name `ChannelName::new` rejects path-traversal but admits trailing `.` / repeated `.` / case-folded duplicates
- **File:line:** `src/adapter/net/channel/name.rs:88-122`.
- **Severity:** low-medium
- **Bug class:** canonicalization (8)
- **What:** Validation rejects `.` and `..` *segments* (split by `/`), but
  `foo.bar` and `foo..bar` are both accepted (the `.` is allowed
  *within* a segment). `foo.bar` and `FOO.BAR` are both accepted and hash
  to **different** xxh3 outputs — they are distinct channels for the
  registry, the auth guard, and the token cache. Combined with F-1, an
  operator who registers `prod.deploy` and forgets to also register
  `Prod.Deploy` opens a parallel channel namespace under the same wire
  prefix once a subscriber happens to address it case-shifted.
- **Attack scenario:** Operator registers `prod.deploy` with
  `subscribe_caps = require_tag("admin")`. Attacker subscribes to
  `Prod.deploy`: registry miss → falls into prefix table; if no prefix
  matches it falls through to the permissive "no ACL" branch at
  authorize_subscribe:5855-5858. The attacker bypasses the cap filter.
- **Fix sketch:** Canonicalize on construction — lowercase or reject
  ambiguous variants, and reject empty/dot-only segments. Either drop
  case-folding entirely (mirror DNS) or normalize to lowercase before
  hashing.

### F-7 — `TokenCache::check` does NOT cross-check token's stored `subject` matches the lookup-key bytes
- **File:line:** `src/adapter/net/identity/token.rs:649-676`
  (`TokenCache::check` walks slot keyed `(subject_bytes, channel_hash)` and
  applies `t.authorizes(action, channel_hash)` — does not re-confirm
  `t.subject.as_bytes() == subject.as_bytes()`).
- **Severity:** low
- **Bug class:** signature verification / construction (5)
- **What:** Inserts always key by `token.subject.as_bytes()`, so the
  invariant holds today. But there is no defensive re-check — if a future
  refactor ever indexes by hash-of-subject (e.g., for memory savings) or
  permits a `replace_unchecked` that lets a caller key a token under a
  different subject than the token's `subject` field, every check would
  silently authorize the wrong entity. The signature does cover `subject`
  so a tampered token wouldn't pass `is_valid()`, but the cross-check
  itself is missing.
- **Fix sketch:** Add `&& *t.subject.as_bytes() == *subject.as_bytes()` to
  the predicate at lines 660 and 670. Cost is one memcmp; benefit is a
  durable invariant that survives future refactors.

### F-8 — `delegate` verifies parent via `self.is_valid()` (which calls `current_timestamp()`) without clock-skew bound
- **File:line:** `src/adapter/net/identity/token.rs:353-364`
  (`delegate` calls `self.is_valid()?` at top); `:286-296` (`is_valid` does
  raw `now < not_before` / `now >= not_after`); `:799-804`
  (`current_timestamp` reads `SystemTime::now()`).
- **Severity:** low
- **Bug class:** replay / clock-skew (4)
- **What:** A node whose system clock drifts forward by N seconds will
  consider a freshly-issued parent token "Expired" and refuse to delegate
  it. A node whose clock drifts backward will consider a not-yet-valid
  parent token "NotYetValid" and refuse. No skew tolerance.
  More concerning: `is_valid` is the same call the channel-auth fast path
  in `publish_many` line 6284 makes. A node with a clock that ran 30
  seconds slow accepts tokens that the rest of the mesh treats as expired.
- **Attack scenario:** Attacker influences NTP / sets a faulty container
  clock on the victim node; victim continues to honour a token the rest
  of the mesh considers expired. (Real exposure depends on operational
  controls.)
- **Fix sketch:** Add a configurable `CLOCK_SKEW_TOLERANCE_SECS` (e.g.,
  60 s) and apply it to both ends: `now + skew < not_before` /
  `now - skew >= not_after`. Document the source-of-truth assumption.

### F-9 — Wildcard-slot scan is `O(slot_size)` per check; an attacker can fill the wildcard slot to `MAX_TOKENS_PER_SLOT`
- **File:line:** `src/adapter/net/identity/token.rs:596-639`
  (`insert_unchecked` always routes WILDCARD tokens to slot
  `channel_hash = 0`); `:665-674` (`check` falls through to the wildcard
  slot every time the exact slot misses).
- **Severity:** low
- **Bug class:** capability announcement broadcast (7)
  (DoS by token-slot exhaustion)
- **What:** Every WILDCARD token lands in the slot `(subject_bytes, 0)`.
  An attacker holding a valid signing key and the `DELEGATE` scope can
  mint up to `MAX_TOKENS_PER_SLOT = 32` distinct-scope WILDCARD tokens
  under the same subject (one per scope-bitfield permutation) and force
  every `check()` for that subject to walk all 32 each time. With 32 slots
  per subject and a u64 nonce diversifying, the cap is effective; but the
  fallback to the wildcard slot on every exact-slot miss (lines 665-674)
  means every legitimate `check()` for an unauthorized channel pays the
  full wildcard walk too. Mostly a latency issue; not exploitable to a
  privilege gain.
- **Fix sketch:** Add a fast-path `bool` cached on the slot — "any token
  here has WILDCARD set" — and skip the iter when false.

## Null findings (explicitly clean)

- **Schema-doc-guard (test class 2):** No bypass found. The test
  `capability_schema_doc_guard.rs` is purely a CI doc-drift check on the
  `AXIS_SCHEMA` const; it does not gate runtime decisions. The runtime
  validator in `schema.rs:485-530` walks both exact-match and prefix lists
  and applies to deserialized peer caps. NUL-injection in axis keys is
  blocked at `Tag::parse` level by the strict character set.
- **Envelope signature/construction (5):** `IdentityEnvelope`'s sealed-box
  + attestation construction is sound. Field set signed
  (`target_static_pub || chain_link.to_bytes()`) matches verifier.
  `verify_strict` rejects malleated signatures. AAD binds `chain_link` to
  AEAD tag. Wire version byte prevents v0/v1 downgrade. Tests cover
  retarget, replay-at-different-chain-link, tampered ciphertext, and
  substituted signer.
- **`unsafe impl Send/Sync` or panic surface on network-arriving messages
  (9):** `handle_capability_announcement` uses no `unwrap`/`expect` on
  any field; `from_bytes` returns `Option` and the handler returns early
  on `None`. `to_bytes` uses `unwrap_or_default` (see F-4 instead).
- **Replay of `IdentityEnvelope` at different `chain_link` (4):** caught by
  attestation transcript binding.
- **Scope confusion across tenants (1):** `matches_scope` uses byte-exact
  string equality; no case-folding, no prefix-match, no NUL collapsing.
- **Multihop binding-mismatch bypass:** `ann.entity_id.node_id() ==
  ann.node_id` check at `mesh.rs:5187` fires on every code path (direct
  and forwarded), pinned by
  `signed_announcement_with_mismatched_node_id_entity_id_is_rejected`.

## Dead invariants

- `capability_broadcast.rs:553-616` pins that forwarded announcements
  must not write `peer_subnet`; production gate at `mesh.rs:5259-5264`
  matches. ✓
- `channel_auth.rs:184-210`: token TTL=1s test relies on inclusive-expiry
  (`is_valid` returns `Expired` at `now >= not_after`); production code
  agrees (`token.rs:292-294`). ✓
- `capability_broadcast.rs:309-345`: `require_signed_capabilities` drops
  unsigned at the receiver; gate at `mesh.rs:5155-5161`. ✓
- **F-3 reveals a dead invariant on the other side**: every test for TOFU
  pin uses a fresh node-pair where no forwarder has primed dedup, so the
  victim-side direct pin is implicitly assumed to fire. No regression
  test guards against the forwarded-poisoning-then-direct-arrives race
  exposed in F-3.

## Suggested action order

1. **F-1** — channel-hash collision is the highest-yield finding because
   it lets a single bad token cross channels. Either widen `ChannelHash`
   or add a name-level cross-check in `can_publish/can_subscribe`. ~half
   a day.
2. **F-2** — no revocation. At minimum, cap delegated `not_after` so the
   operator-recovery window is bounded. Designing a real CRL is a larger
   effort; the cap is one-line + a constant. ~1 hour for the cap, ~1
   week for a full CRL.
3. **F-3** — re-order dedup vs TOFU-pin in `handle_capability_announcement`
   or widen the dedup key. ~1 hour + new regression test.
4. **F-4** — switch signed transcript away from `unwrap_or_default()`.
   Latent today; defence-in-depth.
5. **F-5**/**F-6** — both manifest at write-path / construction-path
   boundaries; small fixes.
6. **F-7**/**F-8**/**F-9** — defence-in-depth.

## Coverage gaps

- Cross-language conformance for `PermissionToken` wire format under
  channel-hash collision — Rust agrees with itself; what TS/Py/Go SDKs
  do at the `channel_hash → token` binding wasn't audited (deferred to
  Phase 4).
- The `nrpc:` reserved-tag and prefix-channel auth interaction
  (`<service>.replies.<caller_origin>` → token for prefix hash, not for
  the specific reply channel) has the documented behaviour but tested
  only through happy-path; no negative test asserts that a prefix-issued
  token cannot subscribe to a sibling prefix outside the registered
  prefix scope. Worth a follow-up.
- FFI `net_identity_install_token` length validation — covered by the
  umbrella's H-1 (`isize::MAX` guard); not duplicated here.
