# Security audit — channel token-auth root-of-trust (2026-05-29)

Branch: `master`.
Scope: the `require_token` channel-authorization path and the `PermissionToken`
delegation/revocation model. Triggered by a trace of the inbound `Subscribe`
handler (`mesh.rs::authorize_subscribe`) against the token cache and channel
config. Both findings below are **structural** — every individual primitive
verifies correctly in isolation; the gap is that channel enforcement was never
anchored to a root of trust, and delegation rewrites the field revocation keys
on.

These are distinct from the `2026-05-28` net-crate audit. That pass flagged the
token TTL as unbounded and revocation as *advisory* (H3). This pass shows the
deeper problem: for the `require_token` gate there is **no issuer barrier at
all**, so H3's premise ("exploitation requires being a valid issuer") does not
hold — every peer is a valid issuer of its own credentials.

Line numbers reflect `master` at audit time and may drift.

| ID | Severity | Area | One-line |
|----|----------|------|----------|
| C1 | Critical | Identity / channel-auth | `require_token` channels accept self-issued tokens — no channel owner / authorizing-issuer anchor anywhere |
| H1 | High | Identity | `delegate()` rewrites `issuer` to the delegator; root revocation never reaches delegated descendants, contradicting the documented transitive-revoke guarantee |

---

## CRITICAL

### C1 — `require_token` has no root of trust: a self-issued token grants access
`net/crates/net/src/adapter/net/identity/token.rs:303` (`verify`), `:911` (`TokenCache::check`), `:754` (`is_revoked`); `net/crates/net/src/adapter/net/channel/config.rs:34` (`ChannelConfig`), `:135` (`can_subscribe`); `net/crates/net/src/adapter/net/mesh.rs:7043` (`authorize_subscribe`).

The token gate verifies a credential is *internally self-consistent* but never that the signer has any authority over the channel.

**1. `verify()` only checks the claimed issuer signed it** — `token.rs:303-309`:

```rust
pub fn verify(&self) -> Result<(), TokenError> {
    let payload = self.signed_payload();
    let sig = Signature::from_bytes(&self.signature);
    self.issuer
        .verify(&payload, &sig)
        .map_err(|_| TokenError::InvalidSignature)
}
```

This proves "the entity named in the `issuer` field signed this token." It says nothing about whether that issuer is permitted to grant on the channel.

**2. `TokenCache::check()` never constrains the issuer** — `token.rs:911-956`. A slot passes if any token satisfies:

```rust
t.is_valid_with_skew(self.clock_skew_secs).is_ok()
    && !self.revocation.is_revoked(t)
    && t.subject.as_bytes() == subject.as_bytes()
    && t.authorizes(action, channel_hash)
```

The `issuer` field is consulted *only* by `is_revoked` (`token.rs:754`: `token.issuer_generation < self.floor(&token.issuer)`) — i.e. to look up the issuer's *own* revocation floor. There is no comparison of the issuer against any expected/authorized key.

**3. `ChannelConfig` has no anchor to compare against** — `config.rs:34-51`:

```rust
pub struct ChannelConfig {
    pub channel_id: ChannelId,
    pub visibility: Visibility,
    pub publish_caps: Option<CapabilityFilter>,
    pub subscribe_caps: Option<CapabilityFilter>,
    pub require_token: bool,
    pub priority: u8,
    pub reliable: bool,
    pub max_rate_pps: Option<u32>,
}
```

No owner key, no authorizing-issuer field, no set of trusted roots. `can_subscribe` (`config.rs:135-154`) and `can_publish` (`config.rs:111-132`) both just delegate to `token_cache.check(entity_id, SCOPE, channel_hash)`.

**4. The subscribe path inserts peer-presented tokens after only `verify()`** — `mesh.rs:7126-7128`:

```rust
let presented_token = token_bytes
    .and_then(|bytes| PermissionToken::from_bytes(bytes).ok())
    .filter(|tok| tok.verify().is_ok());
```

`:7181-7185` drops the token into a scratch cache and runs `can_subscribe`; `:7198-7200` promotes it to the shared cache once the check passes. The subject is bound to the handshake identity (`mesh.rs:7154`, `peer_entity_ids.get(&from_node)`), so the attacker cannot impersonate another subject — but it does not need to. `issue()` defaults `issuer_generation: 0` (`token.rs:284`), so a self-issued token is born below no floor.

**Attack.** An admitted peer E mints, with its own key:

```text
issue(issuer = E, subject = E, scope = SUBSCRIBE, channel_hash = hash(C))
```

`verify()` passes (E signed it), `check(E, SUBSCRIBE, hash(C))` passes (subject == E, floor[E] == 0, `authorizes` matches) → E is subscribed to the `require_token` channel C it has no grant for. The identical hole exists in `can_publish` for `TokenScope::PUBLISH`.

- **Trigger**: any handshake-completed mesh peer sends a `Subscribe` (or publishes) on a `require_token` channel carrying a token it minted for itself.
- **Preconditions** (scope the blast radius, do not remove it): the attacker must (1) be an admitted mesh member — the transport PSK gate still applies; (2) pass the subnet **visibility** gate (`mesh.rs:7107-7117`); (3) if the channel *also* sets `subscribe_caps` / `publish_caps`, satisfy that capability filter separately (capabilities derive from signed announcements — a distinct gate, out of scope here). A channel relying on `require_token` *alone* among mutually-distrusting mesh members — which is the population `require_token` exists to restrict — is fully bypassable.
- **Impact**: complete bypass of the per-channel token ACL. `require_token` provides no security property against any peer admitted to the mesh.
- **Root cause**: `check()` answers "does this subject hold a self-consistent token for this channel," which everyone can satisfy for themselves. It never answers "does this token chain back to the entity that owns the channel," because no such owner is recorded anywhere.

---

## HIGH

### H1 — Delegation rewrites `issuer`; root revocation cannot reach descendants
`net/crates/net/src/adapter/net/identity/token.rs:411` (`delegate`), `:458` / `:468` (issuer + generation assignment), `:730` (`revoke_below`), `:754` (`is_revoked`).

`delegate()` sets the child's `issuer` to the **delegator**, not the root, while inheriting the parent's generation — `token.rs:457-474`:

```rust
let mut child = Self {
    issuer: signer.entity_id().clone(),   // the delegator, NOT the root
    subject: new_subject,
    scope: new_scope,
    channel_hash: self.channel_hash,
    issuer_generation: self.issuer_generation,  // inherited from parent
    not_before: now,
    not_after: self.not_after,
    delegation_depth: self.delegation_depth - 1,
    nonce,
    signature: [0u8; 64],
};
```

Pinned by the test `delegate_inherits_parent_issuer_generation` (`token.rs:1907-1935`): the child inherits the parent's `issuer_generation` but its `issuer` is the intermediate signer.

Revocation floors are keyed by issuer — `revoke_below` (`token.rs:730-731`) keys on `issuer.as_bytes()`; `is_revoked` (`:754`) reads `floor(&token.issuer)`. So for a chain `R → A → B → C` (R issues to A, A delegates to B, B delegates to C):

```text
revoke_below(R, 1)  →  floor[R] = 1
  A's grant   (issuer=R, gen 0): 0 < floor[R]=1  → revoked   ✓
  B's deleg.  (issuer=A, gen 0): 0 < floor[A]=0  → NOT revoked
  C's deleg.  (issuer=B, gen 0): 0 < floor[B]=0  → NOT revoked
```

Revoking the root kills only the root's *direct* grants. Every offline-delegated descendant has `issuer = its delegator` and survives. To revoke the subtree the operator must bump the floor for *every* delegator in it — but `delegate()` is a purely local offline operation (no registration back to the root), so the root cannot enumerate the floors it would need to bump.

The doc comment at `token.rs:462-467` claims the opposite:

> Children inherit the parent's issuer_generation. When the signer's floor is bumped … every outstanding token from that issuer — including this child — falls below the floor … That makes a single floor bump transitively invalidate the chain without a per-link revocation walk.

That holds only for a flat fan-out of *direct* grants from one issuer (where every token has `issuer == that issuer`). For an actual delegation chain it is false past the first hop: bumping a delegator's floor revokes that delegator's direct children, not grandchildren, and never lets the root revoke the subtree in one operation.

- **Trigger**: revoke a root issuer after any of its grants has been delegated onward.
- **Impact**: revocation silently under-reaches. Combined with the `2026-05-28` H3 (unbounded TTL), a delegated long-TTL credential is effectively unrevocable by the party that rooted the chain. Compounds C1: even a deployment that *did* anchor channels to a root could not reliably revoke a leaked delegated credential.
- **Fix**: anchor revocation to the **root** issuer carried across the whole chain rather than the per-link rewritten `issuer`, OR walk the presented chain at check time and test each link's root floor. Either way, requires the chain to be present at check time (see shared fix below). At minimum, correct the doc comment so it does not advertise a guarantee the code does not provide.

---

## Root cause + fix shape (shared)

Both findings are the same missing concept from two angles: **nothing roots the tree.** The credential format is complete and the mechanisms are individually hardened (NONE-scope short-circuit, `channel_hash == 0` wildcard removal, getrandom-abort on nonce failure, scratch-before-shared cache, slot caps, skew clamping), but the channel-enforcement layer was never wired to a root of trust, and delegation severs the link revocation keys on.

A coherent fix:

1. **`ChannelConfig` gains an authorizing key (or set)** — the channel owner entity, or a set of trusted root issuers.
2. **`check` / `can_subscribe` / `can_publish` verify the token's issuer is that key**, or that a presented delegation chain *terminates* at it.
3. **Delegation carries the parent chain on the wire.** Today `from_bytes` reads exactly one `WIRE_SIZE = 169`-byte token (`token.rs:167`, `:552-555`); `MembershipMsg::Subscribe` carries a single `token`. A chain needs the full ancestry transmitted and length-bounded.
4. **`check` walks the chain**: each link signed by the prior link's subject, scopes monotonically narrowing, `delegation_depth` decrementing, `subject(parent) == issuer(child)` continuity, bottoming out at the channel's authorizing key. Cap the max chain length so verify cost on the receive path stays bounded.
5. **Anchor revocation to the root issuer** across the chain instead of the rewritten per-link issuer, or the floor bump keeps missing descendants (H1).

This touches the wire format (item 3) and deserves its own design pass rather than a mid-session bolt-on.

## Confidence / containment caveat

The trace covered the authoritative receive path (`mesh.rs::authorize_subscribe`), the channel config, and the token cache. If a deployment's operational model is "operators only ever feed their caches tokens from issuers they have already vetted out-of-band," then in a fully trusted mesh C1 is contained and matches a "trusted participants" posture. But the **presented-token path inserts whatever the peer hands over after only a signature check** (`mesh.rs:7126-7128`, `:7198-7200`), so that containment evaporates the moment a channel sets `require_token` expecting it to mean something against a mutually-distrusting peer — which is the only reason the flag exists.
