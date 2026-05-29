# Locally-held publish-chain credentials

## Context

The channel token-auth root-of-trust fix
([`docs/misc/SECURITY_AUDIT_2026_05_29_CHANNEL_AUTH.md`](../misc/SECURITY_AUDIT_2026_05_29_CHANNEL_AUTH.md))
replaced the bare-token credential with a root-anchored
[`TokenChain`](../src/adapter/net/identity/token.rs). A presented
credential is now only honored if it roots at one of the channel's
`token_roots`.

The **subscribe** path carries the full chain over the wire
(`subscribe_channel_with_chain`), so a delegated subscriber (owner → …
→ subscriber) works end to end. The **publish self-check** does not.
`publish_many` builds the publisher's credential from the local token
cache as a *single link*:

```rust
// mesh.rs, publish_many auth gate
let chain = cache
    .get(&self_entity, cfg.channel_id.hash())
    .map(TokenChain::single);
```

`TokenCache` stores individual `PermissionToken`s keyed by
`(subject, channel_hash)`; it cannot reconstruct the chain a delegated
grant came down. So a node that holds a **delegated** publish grant
(`owner → org → this_node`) wraps only its leaf token, whose issuer is
the immediate delegator, not the channel owner — the root-anchor check
fails and the publish is denied.

This is not a missing feature so much as a **behavior regression** for
delegated publishers: pre-fix, the unanchored `cache.check` accepted
any self-consistent token in the cache, including a delegated one.
Post-fix it fails closed. Backwards compatibility was explicitly
waived for the security fix, but a deployment that grants publish
rights *by delegation* (rather than the channel owner issuing every
publisher directly) silently loses the ability to publish.

**Threat framing.** The publish self-check gates a node against
*itself* — a malicious node bypasses it trivially. So this work is
about **correctness for honest delegated publishers**, not closing an
attack surface. It is only worth doing if a deployment actually uses
delegated publish grants; direct-issued publishers (the common case)
already work via the single-link wrap and are unaffected.

## Status — core implemented 2026-05-30

Phases 1–2 and the core of phase 4 shipped in commit `4666d639f`
(`feat(mesh): let delegated publishers present a held publish chain`).
The as-built API differs from the sketch below; reconciled here (the
Design section is left as the original proposal — this section is the
source of truth for what exists):

- **Store** — `MeshNode.published_chains: Arc<DashMap<ChannelHash,
  TokenChain>>`, exactly as designed. Not threaded into `DispatchCtx`
  (the publish gate runs on `&self` in `publish_many`).
- **Install API** — shipped as an **infallible setter keyed by an
  explicit channel name**:
  `MeshNode::set_publish_chain(&self, channel: &ChannelName, chain: TokenChain)`
  — not the `install_publish_chain(chain) -> Result` that derived the
  key from `chain.tokens.last().channel_hash`. Taking the channel from
  the caller **resolves the WILDCARD-leaf open question by
  construction**: a WILDCARD publish chain is stored under whichever
  channel(s) the operator names, so there is no leaf-`channel_hash`
  ambiguity to special-case.
- **Leaf-subject validation** — not enforced at install. The
  publish-time `verify_authorizes` already binds the chain leaf to this
  node's entity, so a mis-installed chain fails *closed* at publish
  (denied), never silently honored. Install stays a cheap setter,
  matching the plan's "authoritative check at publish time" philosophy.
- **`publish_many` wiring** — store-first, falling back to a single-link
  chain from the cache, as designed — except the fallback uses the
  scope-aware `TokenCache::get_for_action(.., PUBLISH, ..)` (added in
  `d52b8186d`) instead of scope-agnostic `get`, so a SUBSCRIBE token
  sharing the slot can't shadow the PUBLISH grant.
- **Test** — `delegated_publish_chain_authorizes_publish` (mesh.rs)
  covers delegated-accept and the no-chain denial.

**Deferred** (gated on a real consumer, per the threat framing):

- `remove_publish_chain` — re-issue is already covered by overwriting
  via `set_publish_chain`; explicit removal is cleanup only.
- Rust SDK `Mesh::set_publish_chain` pass-through.
- FFI / language-binding install entry point — still the binding
  follow-up in the security audit doc.
- Extra publish-path tests (root-revoke-kills-publish, direct-grant
  fallback, self-issued-rejected). The verification logic is the shared
  `verify_authorizes`, already covered on the subscribe side in
  `channel/config.rs`.

## Scope

**In scope:**

- A per-node held-chain store for the node's own publish credentials.
- An install / remove API on `MeshNode` and the Rust SDK `Mesh`.
- Wire `publish_many` to consult the store, falling back to the
  existing single-link-from-cache path for direct grants.
- Unit + integration tests covering delegated-publish accept,
  root-revoke-kills-publish, and the direct-grant fallback.

**Out of scope:**

- FFI / language-binding entry points for installing a publish chain.
  Those depend on the chain-bytes plumbing tracked as the second
  follow-up in the security audit doc; once a binding consumer needs
  delegated publish, add `net_mesh_install_publish_chain` alongside
  the subscribe-chain entry point in the same pass.
- Any change to the subscribe path — it already carries full chains.
- Persisting held chains across process restarts. The store is
  in-memory; a restarted node re-installs its credentials, same as it
  re-presents on subscribe.

## Design

### Held-chain store

Add to `MeshNode` (and thread through `DispatchCtx` only if the
publish path that reads it runs off `ctx` — it currently runs on
`&self` in `publish_many`, so `MeshNode` alone suffices):

```rust
/// The node's own publish credentials, keyed by channel hash. Each
/// value is a root-to-leaf TokenChain whose leaf subject is this
/// node's entity and which authorizes PUBLISH, rooted at the
/// channel's owner. Consulted by `publish_many` so a delegated
/// publish grant can anchor; absent entries fall back to the
/// single-link-from-cache path for direct grants.
published_chains: Arc<DashMap<ChannelHash, TokenChain>>,
```

Keyed by `ChannelHash` alone — a node holds at most one publish chain
per channel. (Subscribe credentials are presented per-request over the
wire and are not stored here.)

### Install / remove API

```rust
impl MeshNode {
    /// Install a publish credential chain for the channel its leaf is
    /// bound to. Rejects a chain whose leaf subject is not this node's
    /// entity (a node may only install credentials granting itself).
    /// Does NOT verify the root anchor here — that depends on the
    /// channel's `token_roots`, which are checked at publish time
    /// against the live config + revocation registry.
    pub fn install_publish_chain(&self, chain: TokenChain) -> Result<(), AdapterError>;

    /// Drop a previously-installed publish chain.
    pub fn remove_publish_chain(&self, channel_hash: ChannelHash);
}
```

Install-time validation is deliberately minimal (leaf-subject ==
self): the authoritative check is at publish time, so install stays a
cheap setter and a later `token_roots` change or revocation is honored
without re-installing. The channel hash to key on is
`chain.tokens.last().channel_hash` — but note a WILDCARD leaf has no
single channel; reject WILDCARD publish chains from the store (they'd
need a different keying scheme) or key them under a reserved sentinel
and special-case the lookup. **Open question** — see below.

Rust SDK `Mesh` gets thin pass-throughs:
`Mesh::install_publish_chain` / `remove_publish_chain`.

### `publish_many` wiring

Replace the single-link construction with a store-first lookup:

```rust
let chain = self
    .published_chains
    .get(&cfg.channel_id.hash())
    .map(|c| c.clone())
    .or_else(|| {
        // Direct-grant fallback: a token the channel owner issued to
        // this node directly, sitting in the local cache.
        self.token_cache
            .as_ref()
            .and_then(|cache| cache.get(&self_entity, cfg.channel_id.hash()))
            .map(TokenChain::single)
    });
if !cfg.can_publish(&self_caps, &self_entity, chain.as_ref(), revocation, skew) {
    return Err(/* denied */);
}
```

Verification (root anchor, leaf binding, per-link revocation, monotonic
authority) is unchanged — it already runs inside
`ChannelConfig::can_publish` → `TokenChain::verify_authorizes`. The
only change is *where the chain comes from*.

## Phases

1. **[done]** **Store + API.** `published_chains` on `MeshNode` +
   construction; `set_publish_chain(channel, chain)` (infallible setter,
   keyed by the caller-supplied channel — see Status). WILDCARD-leaf
   keying question is moot under this API.
2. **[done]** **Wire `publish_many`.** Store-first lookup with the
   single-link fallback (`get_for_action`). No signature change to
   `can_publish`.
3. **[deferred]** **SDK surface.** `Mesh::set_publish_chain`
   pass-through (+ `remove_publish_chain` if added).
4. **[partial]** **Tests.** `delegated_publish_chain_authorizes_publish`
   done; the rest (below) deferred.
5. **[done]** **Docs.** "Multi-hop locally-held publish" follow-up in
   the security audit doc flipped to resolved; FFI/binding follow-up
   left open.

## Testing

- **Unit (`channel/config.rs` already covers `verify_authorizes`)** —
  no new verification logic, so the store tests live at the mesh layer.
- **[done] Delegated publish accepted** — `mesh.rs`
  `delegated_publish_chain_authorizes_publish`: owner → mid
  (PUBLISH+DELEGATE) → this node (PUBLISH); without the held chain the
  publish is denied, after `set_publish_chain` it is admitted.
- **[deferred] Direct-grant fallback** with no installed chain (owner
  issues PUBLISH directly; token in cache; publish succeeds) — guards
  the common path against regression.
- **[deferred] Root revoke kills publish** — `revoke_below(owner, …)`
  after a successful delegated publish → next publish denied.
- **[deferred] Self-issued publish chain rejected** (issuer ∉
  `token_roots`), the publish-path mirror of the subscribe-side C1 test.

The setter is infallible (no leaf-subject check at install), so the
planned "rejects a chain whose leaf isn't self" install test no longer
applies — that binding is enforced at publish time by `verify_authorizes`
and covered by the subscribe-side `leaf_subject_must_match_presenter`.

## Risks / open questions

- **WILDCARD-leaf publish chains.** *Resolved* by the as-built API: the
  setter keys on the caller-supplied `ChannelName`, not on a hash
  derived from the leaf, so a WILDCARD publish chain is simply stored
  under each channel the operator wants it used on. No special-case or
  separate wildcard list needed. (A node holding one WILDCARD publish
  credential it wants to use across many channels must call
  `set_publish_chain` once per channel; acceptable given how rare
  cross-channel publish grants are.)
- **Low value if delegated publish is unused.** Gate the work on a real
  need — direct publishers are already fine. If no deployment delegates
  publish rights, this plan can sit until one does.
- **Overlap with the FFI/binding follow-up.** `install_publish_chain`
  is itself a chain entry point; doing the bindings here would pull in
  the whole cross-language chain-bytes surface. Kept out of scope so
  this stays a contained Rust-core change.
