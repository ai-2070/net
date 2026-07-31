# Security audit — channel auth (2026-07-31)

Branch: `master` (`fb0f5803e`).

Scope: the L2 channel authorization surface —
`adapter/net/channel/{config,guard,membership,name,roster,publisher}.rs`,
`adapter/net/identity/token.rs`, and the enforcement sites in
`adapter/net/mesh.rs` (inbound `Subscribe` handling, publish fan-out, the
periodic token sweep), plus the nRPC glue in `adapter/net/mesh_rpc.rs` /
`sdk/src/mesh_rpc.rs` that rides on channel auth.

Method: manual trace of the authoritative paths — wire `Subscribe` →
`authorize_subscribe` → `ChannelConfig::can_subscribe` →
`TokenChain::verify_authorizes`, and `publish_many` → subscriber `retain` →
`reverify_subscribe*` — followed by a reverse sweep for *unreached* enforcement
(who reads `AuthGuard`, who calls `can_publish`, what the ingress path checks).

Line numbers reflect `master` at audit time and may drift.

> **Revision (rev 2).** The first draft of this document was revised after
> review. Changes: added H2 (`serve_rpc*` config overwrite), which the first
> pass missed; added M3 and M4; replaced H1's fix sketch with a design decision
> after the proposed ingress identity was shown to be underivable; narrowed H3
> (was H2) to the public/legacy admission route; rewrote M1 as an
> acceptance/delivery inconsistency plus a persistent queue-group DoS, and added
> the symmetric `set_publish_chain` breakage; rewrote M2 as an
> integrity/availability finding rather than confidentiality; demoted the
> `AuthGuard`-namespace finding to Info (I1); narrowed the registry-less finding
> to direct `MeshNode` embedders (L1). ID map from rev 1: `H2 → H3`,
> `M1 → M1`, `M2 → M2`, `L1 → I1`, `L2 → L1`, `I1 → I2`.

## Summary

**The token core is sound.** The root-anchored `TokenChain` introduced for
[C1/H1 (2026-05-29)](SECURITY_AUDIT_2026_05_29_CHANNEL_AUTH.md) holds up under
re-trace: chains must anchor at `ChannelConfig::token_roots`, bind their leaf to
the TOFU-pinned presenter, and authorize the action at *every* link (chain
authority = intersection); `require_token` with empty roots fails closed
(`config.rs:246-257`); the entity pin only happens on a signature-verified,
direct (`hop_count == 0`) announcement whose `entity_id.node_id()` matches the
claimed `node_id` (`mesh.rs:22333-22426`); both wire codecs reject trailing
bytes; `TokenScope::contains(NONE)` is short-circuited; the
`channel_hash == 0` implicit-wildcard overload is gone. None of the findings
below are defects in that machinery.

They are gaps **around** it. Three themes:

1. **One direction of the ACL is never enforced where it matters** (H1). Publish
   authority is checked on the sending node, and one whole class of sender
   (nRPC direct unicast) skips even that.
2. **The operator hardening story for RPC channels does not work** (H2, H3). The
   documented "register strict ACLs before `serve_rpc`" path is silently undone
   by `serve_rpc*` itself, and the API it names does not exist.
3. **Prefix-configured channels are keyed on the wrong channel throughout**
   (M1), producing inconsistent accept-then-deny behaviour and a persistent
   queue-group denial of service.

| ID | Severity | Area | One-line |
|----|----------|------|----------|
| H1 | High | Channel auth / integrity | Publish authority is emitter-side only, and nRPC direct sends bypass even that |
| H2 | High | nRPC / config | `serve_rpc*` overwrites operator-installed RPC ACLs with permissive defaults; the documented escape hatch does not exist |
| H3 | High | nRPC / confidentiality | Public-mode reply channels are world-subscribable; the roster fallback discloses response bodies to raw event-plane readers |
| M1 | Medium | Channel auth | Prefix configs gate and retain on the sentinel hash — accept-then-fail-closed, a persistent queue-group DoS, and unusable `set_publish_chain` |
| M2 | Medium | Channel auth / availability | Queue-group membership is unauthenticated — a subscriber can steal another group member's work |
| M3 | Medium | Identity | `MAX_TOKEN_TTL_SECS` is issuance-only; receivers accept arbitrarily long remote TTLs |
| M4 | Medium | Channel auth | Registry without `TokenCache`: subscribe accepts, first publish always denies; the docs say it always rejects |
| L1 | Low | Defaults | A registry-less `MeshNode` accepts every `Subscribe` and publishes every channel as open |
| I1 | Info | Channel auth / storage | The `AuthGuard` exact ACL is one namespace shared by four readers under unconstrained `u64` keys |
| I2 | Info | Docs | The "presented tokens are installed into `TokenCache`" contract is stale in several places |

---

## HIGH

### H1 — Publish authority is emitter-side only, and nRPC direct sends bypass even that

`net/crates/net/src/adapter/net/mesh.rs:23787-23842` (the only production
`can_publish` call site), `:16490-16870` (ingress), `:24179-24240`
(`try_publish_to_peer`), `:23961` (`check_fast`), `:24132`
(`publish_stream_id`); `net/crates/net/src/adapter/net/protocol.rs:172-174`
(wire `channel_hash`).

**Emitter-side only.** `ChannelConfig::can_publish` is invoked exactly once in
the runtime: inside `publish_many`, on the *publishing* node, against its *own*
config, before its own fan-out (`mesh.rs:23787-23842`). Nothing on the receiving
side re-establishes the property. The ingress path resolves the stream, does
credit accounting, optionally diverts to a blob-transfer stream or an nRPC
dispatcher, and otherwise pushes straight onto the per-shard queue —
`mesh.rs:16858-16866`:

```rust
let queue = inbound.entry(shard_id).or_default();
let seq = parsed.header.sequence;
for (i, event_data) in events.into_iter().enumerate() {
    ...
    queue.push(StoredEvent::new(event_id, event_data, seq, shard_id));
}
```

`StoredEvent` carries `(event_id, data, seq, shard_id)` — not the channel, not
the sender. `AuthGuard`, the structure holding the answer, is consulted only on
the **egress** path (`mesh.rs:23961`, filtering which *subscribers* may
receive). There is no `check_fast` / `is_authorized_full` call on any receive
path.

**And one class of sender skips the emitter gate too.** `publish_many` is not
the only way onto the event plane. `try_publish_to_peer` (`mesh.rs:24189-24240`)
takes a `channel_hash` and `stream_id` and sends: session lookup, partition
filter, `open_stream_with`, credit acquire, build, `send_to`. No config lookup,
no `can_publish`, no `AuthGuard`. Every nRPC direct send routes through it —
requests (`mesh_rpc.rs:1961-1979`), grants (`:1525-1550`), responses
(`:2885-2898`). So for those paths the channel ACL is not merely
self-attested; it is absent on both ends.

- **Trigger**: any handshake-completed peer builds a packet on the channel's
  stream id and sends it to a subscriber. No token, no capability, no roster
  entry.
- **Impact**: events forged by an unauthorized peer are delivered to the
  subscriber's consumer indistinguishably from the authorized publisher's.
  `token_roots` + `TokenScope::PUBLISH` and `publish_caps` protect a channel's
  integrity only against nodes running unmodified code — the check lives on the
  machine with the incentive to skip it. Subscribe-side auth (who may *read*) is
  genuinely enforced end-to-end; publish-side auth (who may *write*) is not
  enforced at all.
- **Root cause**: the design mirrors subscribe — where the enforcing node is the
  publisher, i.e. the party being *protected* — onto publish, where the
  enforcing node is the sender, i.e. the party being *restricted*.
  `ChannelConfig`'s rustdoc (`config.rs:26-47`) presents the two directions as
  symmetric.

#### The fix requires a design decision, not a patch

Rev 1 of this document proposed reverse-mapping the packet to its channel via
"the `u16` header hint plus the stream-id low bits, disambiguated via
`ChannelConfigRegistry`." **That is not derivable.** The generic event wire
carries no canonical channel identity:

- `NetHeader.channel_hash` is a `u16` (`protocol.rs:172-174`) — the sender
  deliberately narrows the canonical `u64` when stamping it (`mesh.rs:24239-24258`).
- The stream id is `0x0001_0000_0000_0000 | channel.hash()` (`mesh.rs:24132`).
  That is a bitwise OR, so it *destroys* bit 48 of the hash: the stream id is
  not a reversible encoding of the canonical hash.
- nRPC gets around this with an in-payload `RpcRouteV1` discriminator carrying
  the canonical hash. Generic channel events have no equivalent.
- Prefix-configured channels are strictly worse: dynamically-named channels
  appear in neither `by_hash` nor `by_wire_hash` — prefix entries live only in
  `prefix_configs` and are reachable only by the full requested name, which the
  wire never carries.

So the receive-side gate needs one of:

1. **An authenticated canonical channel discriminator on every event-plane
   publish** — generalize the `RpcRouteV1` approach to all channel traffic, or
   widen the header field. Wire-format change; affects every publisher.
2. **Receiver-owned `(authenticated session, stream_id) → canonical ChannelName`
   state**, established by an explicit publisher authorization/presentation
   handshake — the mirror of `Subscribe`, where the publisher presents a chain
   once and the receiver binds it to the stream for the session's lifetime.

Requirements either way: ambiguity, absent mapping, and unresolvable prefix
names must all fail **closed**; the gate must cover both `publish_caps` and
token PUBLISH authority (H1 is not token-specific); and it must evaluate against
the AEAD-authenticated peer identity, not any wire-claimed origin. Until this
lands, `require_token` should be documented as a **read** ACL.

### H2 — `serve_rpc*` overwrites operator-installed RPC ACLs with permissive defaults

`net/crates/net/sdk/src/mesh_rpc.rs:253-258` (the documented hardening path),
`:267`, `:365`, `:436`, `:496`, `:572`, `:614`, `:654`, `:692` (the eight call
sites), `:276-294` (`auto_register_rpc_channels`);
`net/crates/net/sdk/src/mesh.rs:637-641` (`register_channel` replaces);
`net/crates/net/src/adapter/net/channel/config.rs:405-407`, `:417-424`
(both registry inserts replace).

The SDK tells operators how to harden RPC channels — `sdk/src/mesh_rpc.rs:253-258`:

> Both entries default to permissive (no `publish_caps`, no `require_token`) —
> channel-level ACLs on RPC traffic are a Phase 3 concern […]. Operators who
> need RPC ACLs today can call `register_channel` / `register_channel_prefix`
> themselves **before `serve_rpc`** to override.

Two things are wrong with that.

**1. `serve_rpc*` undoes it.** All eight variants call
`auto_register_rpc_channels(service)` as their first act — `serve_rpc:267`,
`serve_rpc_typed:365`, `serve_rpc_streaming:436`,
`serve_rpc_streaming_typed:496`, `serve_rpc_client_stream:572`,
`serve_rpc_client_stream_typed:614`, `serve_rpc_duplex:654`,
`serve_rpc_duplex_typed:692`. That helper registers unconditionally
(`:279-293`), and both underlying operations *replace*:

- `Mesh::register_channel` — "Idempotent: re-registering the same channel
  replaces the prior config" (`sdk/src/mesh.rs:637-640`);
- `ChannelConfigRegistry::insert_prefix` — plain `DashMap::insert`
  (`config.rs:405-407`);
- `ChannelConfigRegistry::insert` — replaces by name (`config.rs:417-424`).

So the documented sequence produces the opposite of its stated effect:

```text
operator installs strict request/reply ACL
  → serve_rpc(service, handler)
    → auto_register_rpc_channels replaces BOTH entries with open defaults
      → service runs wide open, silently
```

This hits the **request** channel (`<service>.requests`) as well as the reply
family, so it is broader than H3 — an operator who token-gated who may *invoke*
a service loses that gate the moment they serve it.

**2. `register_channel_prefix` does not exist.** It appears exactly once in the
tree — in that doc comment. There is no such method on `Mesh`. The lower-level
`ChannelConfigRegistry::insert_prefix` is reachable via `channel_configs_arc()`,
but the advertised SDK surface is fictitious, so an operator following the
documentation cannot pre-register a reply-prefix ACL at all.

- **Impact**: every RPC ACL an operator installs through the documented path is
  discarded at `serve_rpc` time, with no error and no log. The failure is
  silent and the resulting posture (fully open request + reply channels) looks
  identical to the default.
- **Fix**: `auto_register_rpc_channels` must preserve existing entries —
  an atomic entry-if-absent on both the exact and prefix tables (the registry
  currently exposes no such operation; `DashMap::entry(..).or_insert(..)` is the
  natural addition) — or fail loudly when an incompatible registration exists.
  Every one of the eight `serve_rpc*` variants needs a regression witness
  proving a strict pre-installed config survives the call unchanged; a single
  test on `serve_rpc` would not have caught this, since the other seven call the
  helper independently. Fixing the doc comment to name a real API is part of the
  same change.

### H3 — Public-mode reply channels are world-subscribable; the roster fallback discloses response bodies

`net/crates/net/sdk/src/mesh_rpc.rs:284-293` (permissive reply prefix),
`net/crates/net/src/adapter/net/channel/config.rs:480-504` (prefix resolution),
`net/crates/net/src/adapter/net/mesh.rs:23471-23513` (`authorize_subscribe`),
`net/crates/net/src/adapter/net/mesh_rpc.rs:5860-5893`
(`response_route_fallback`), `:2917-2928` (the fallback),
`net/crates/net/src/adapter/net/cortex/rpc.rs:4045-4070` (client-fold binding).

A `Subscribe` for `<service>.replies.<victim_origin>` finds no exact config,
falls through to longest-prefix match (`config.rs:494-503`), and lands on the
permissive auto-registered entry. `authorize_subscribe` then short-circuits
before any identity work — `mesh.rs:23509-23513`:

```rust
let has_auth_gates =
    cfg.publish_caps.is_some() || cfg.subscribe_caps.is_some() || cfg.token_required();
if !has_auth_gates {
    return (true, None);
}
```

The peer joins the roster for a channel named after **someone else's** origin.
The name grammar admits it: the suffix is lowercase hex and `origin_hash` is
derivable from any peer's announced `EntityId`.

The response then leaks when the direct route misses — `mesh_rpc.rs:2923-2928`:

```rust
// Fallback: roster fan-out. Reached only for `RosterOnStaleDirect`
// when the caller's origin is unknown to both the bridge cache AND
// the global reverse index, OR the resolved node had no live session
// at send time (nothing was sent).
let publisher = ChannelPublisher::new(reply_channel.clone(), PublishConfig::default());
mesh.publish(&publisher, payload).await.map(|_| ())
```

#### Scope — narrower than it first appears

Rev 1 called this "cross-tenant on any multi-tenant mesh." That is too broad.
Three qualifications, all load-bearing:

- **Only the public/legacy route roster-fans.** `response_route_fallback`
  (`mesh_rpc.rs:5882-5893`) maps `UnaryAdmission::Public` to
  `RosterOnStaleDirect` and `Protected` / `OwnerScoped` / `Granted` to
  `DirectOnly`. Org-protected modes are already contained.
- **The risk is already documented at that site.** The rustdoc at
  `mesh_rpc.rs:5860-5880` names the permissive reply prefix and the missing
  origin binding explicitly, as the *reason* protected modes do not roster-fan.
  Rev 1's claim that the consequence was undocumented was wrong. What is
  undocumented is that the public route still carries it.
- **An ordinary nRPC client will not accept the stolen frame.** The client fold
  binds each pending call to the node the request was dispatched to and drops a
  RESPONSE arriving from any other session peer (`cortex/rpc.rs:4045-4070`,
  the S-4 gate). So an attacker cannot subscribe under a victim origin and have
  its own pending call complete with the victim's response.

The defect is nonetheless real: the bytes reach the attacker's **event plane**,
and a malicious peer reads them via the raw shard API or a custom inbound
dispatcher — neither of which consults the pending-call binding. So the accurate
statement is: *a public-mode RPC service can disclose full response bodies to
any mesh peer willing to read its own event plane directly, whenever the
server's direct route to the caller misses.* Both triggers are ordinary
operation, not attack preconditions — route-cache eviction under concurrency,
and `NoSession` when the caller reconnects under a new NodeId (the case the
fallback exists to serve).

- **Fix**: (a) scope the prefix — admit a `<service>.replies.<X>` subscribe only
  when `X` equals the subscribing peer's **pinned** `origin_hash`, the value
  `emit_capability_denial` already insists on (`mesh_rpc.rs:820-823`); or
  (b) make public RESPONSE `DirectOnly` too, accepting the reconnect-window drop
  AV-5 traded away. (a) is better — it fixes the channel rather than one
  consumer, and a reply channel named for an origin has exactly one legitimate
  subscriber.
- **(a) needs an ordering rule.** Today a reply subscribe succeeds *before* any
  peer-identity lookup. Binding the suffix to the pinned origin makes
  subscribe-before-announcement fail, which is reachable on a first call and on
  every reconnect. The design must require identity publication/pinning before
  reply subscription and define the retry/backoff behaviour for that window —
  otherwise the fix converts a confidentiality bug into a first-call
  availability bug.

---

## MEDIUM

### M1 — Prefix configs gate and retain on the sentinel hash, not the requested channel

`net/crates/net/src/adapter/net/channel/config.rs:238-268` (`token_gate`),
`net/crates/net/src/adapter/net/mesh.rs:23584-23588` (chain retention),
`:23932-23938` / `:23989-24024` (publish revalidation), `:2768` (sweep),
`:19328` (unsubscribe), `:19121-19123` (`set_publish_chain`), `:23814-23827`
(prefix publish lookup); `net/crates/net/src/adapter/net/channel/roster.rs:205-235`.

`token_gate` verifies the chain against `self.channel_id.hash()`
(`config.rs:261`). For an exact config those coincide with the requested
channel. For a **prefix** config `channel_id` is the sentinel
(`<svc>.replies.prefix`) — which `insert_prefix`'s own rustdoc describes as "not
used for hash lookups" (`config.rs:395-397`) — yet `token_gate` uses it as the
authoritative channel binding. Four consequences:

**1. Acceptance and delivery disagree.** A sentinel-bound token passes the
initial `Subscribe` gate for *every* matching requested name
(`config.rs:258-267`, `mesh.rs:23563-23587`), and the peer is added to the
roster with a successful `Ack`. But `authorize_subscribe` retains the chain
under the config's hash (`mesh.rs:23584-23587`) while publish revalidation looks
it up under the **requested** channel's hash (`mesh.rs:23932-23938`,
`:23989-24011`) and the sweep uses `name.hash()` (`:2768`). The lookup misses,
`chain_ok` is `false`, and the subscriber is revoked before a single event is
delivered. So this is *not* a completed cross-channel read — it is broken
authorization semantics: accept-then-fail-closed, reported to the peer as a
successful subscription that silently never delivers.

**2. It creates a persistent queue-group denial of service.** Queue-group
selection happens in `dispatch_recipients` (`roster.rs:205-235`) **before** the
auth `retain` filter runs, and there is no alternate-member retry — the sharp
edge is acknowledged at `mesh.rs:23856-23863`. A sentinel-authorized peer that
joins a legitimate group is therefore selectable; when selected, its
requested-hash lookup fails and *that group's copy of the event is dropped
entirely*. Worse, the publish-time failure revokes only the `AuthGuard` entry
(`mesh.rs:24021-24024`) — it does **not** remove the peer from the roster or
delete the sentinel-keyed chain. The peer stays selectable until the periodic
sweep evicts it, or indefinitely if the sweep is disabled
(`token_sweep_interval = Duration::MAX`, a supported configuration).

**3. `set_publish_chain` cannot satisfy a token-gated prefix publish.** The
public API stores under the requested channel's hash (`mesh.rs:19121-19123`,
`self.published_chains.insert(channel.hash(), chain)`), while the prefix publish
path retrieves under `cfg.channel_id.hash()` (`mesh.rs:23814-23817`) and asks
`TokenCache::get_for_action` and `can_publish` about the sentinel too. The
delegated-publish escape hatch is unreachable for prefix channels.

**4. Unsubscribe leaks the retained chain.** It removes `(from_node, id.hash())`
(`mesh.rs:19328`) — the real hash — so the sentinel-keyed entry survives until
peer failure clears it (`mesh.rs:8143`).

- **Fix**: thread the *requested* channel identity through the whole path rather
  than deriving it from `&self`. Call sites: `can_subscribe`, `can_publish`,
  `reverify_subscribe`, `reverify_subscribe_presigned`, subscriber-chain
  insertion and removal, the `published_chains` lookup, and
  `TokenCache::get_for_action`. Taking the channel as an explicit argument also
  removes the standing temptation to reuse one config across many channels.
  Separately, the publish-time auth failure should evict from the roster (not
  only the `AuthGuard`) so a denied subscriber stops consuming queue-group
  selections.

### M2 — Queue-group membership is unauthenticated within a channel

`net/crates/net/src/adapter/net/channel/membership.rs:53-64` (wire field),
`net/crates/net/src/adapter/net/mesh.rs:19282-19303` (mode construction),
`net/crates/net/src/adapter/net/channel/roster.rs:205-236`.

`Subscribe.queue_group` is taken verbatim from the wire and turned into a
`SubscriptionMode` with no authorization step — `mesh.rs:19296-19303`:

```rust
let id = ChannelId::new(channel);
let mode = match queue_group {
    None => SubscriptionMode::Broadcast,
    Some(name) => SubscriptionMode::QueueGroup(QueueGroupName::new(name)),
};
ctx.roster.add_with_mode(id, from_node, mode);
```

Auth is deliberately mode-agnostic (`:19289-19293`). But dispatch picks exactly
**one** member per group (`roster.rs:215-218`), so any peer that can subscribe
to a channel can join the same group name as the production workers and be
selected in their place.

**This is not a confidentiality finding.** A peer with channel-level `SUBSCRIBE`
authority can already subscribe in `Broadcast` mode and receive *every* event;
joining a victim's queue group exposes nothing that the same channel grant did
not already expose. The distinct impact is **integrity and availability**:
stealing work from legitimate consumers, and — because the attacker will not
process it — causing legitimate processing not to occur at all. On an open
channel the attacker is any mesh member; on a token-gated one it is any holder
of a `SUBSCRIBE` grant, including a read-only auditor never meant to consume
work.

- **Share**: with `L` legitimate and `A` attacker identities in a group,
  attackers collectively receive `A/(L+A)` of selections and each individual
  identity receives `1/(L+A)`. The attacker scales its share by joining under
  more identities.
- **Fix**: an allowlist of permitted group names is **not** sufficient — an
  attacker who knows a production group name (they are operational constants,
  not secrets) simply joins an allowed one. A generic "may join queue groups"
  scope bit is likewise only partial: it separates broadcast readers from
  workers but still lets any worker join any group on the channel. The
  authorization must bind the **peer to the specific group**: a group name/hash
  carried inside the signed grant, per-group roots or capability policy in
  `ChannelConfig`, or a single configured group per channel with an explicit
  worker-only grant. Whichever shape, the check must run *before*
  `roster.add_with_mode`.

### M3 — `MAX_TOKEN_TTL_SECS` is enforced at issuance only, never on receipt

`net/crates/net/src/adapter/net/identity/token.rs:249-257` (`try_issue`),
`:562-592` (`from_bytes`), `:796-805` (`verify_inner` per-link checks),
`:886-900` (the constant's rationale).

`try_issue` rejects `duration_secs > MAX_TOKEN_TTL_SECS` (1 year) with
`TtlTooLong`. The stated purpose is to bound the blast radius of a leaked
credential, since revocation is only the advisory per-issuer floor
(`token.rs:886-900`). But the bound never travels with the token:

- `from_bytes` (`:562-592`) reads `not_before` / `not_after` as opaque `u64`s
  with no relational check;
- `verify_inner` checks only that each link is *currently inside* its window
  (`:801-805`, via `is_valid_with_skew` / `check_time_bounds`) — never that the
  window is of bounded width.

So a compromised root, a rolled-back build, or any alternate implementation can
sign a token with a century-long lifetime and every receiver accepts it. The
one-year ceiling constrains only tokens this process mints for itself.

- **Fix**: enforce `not_after - not_before <= MAX_TOKEN_TTL_SECS` (saturating)
  during `verify_inner`, per link — consistent with the constant's stated
  purpose, and the only version of the check that binds a remote issuer. If that
  is judged too strict for existing deployments, the audit wording must instead
  say "locally-minted TTL is bounded" and the constant's rustdoc must stop
  claiming it caps leaked-credential lifetime.

### M4 — Registry without `TokenCache`: subscribe accepts, first publish always denies

`net/crates/net/src/adapter/net/mesh.rs:23527-23539` and `:23563-23589`
(subscribe), `:2729-2739` (sweep), `:23989-24024` (publish), `:19033-19042`
(`set_token_cache` rustdoc).

With a channel registry installed but no `TokenCache`, the three enforcement
points disagree about what a token-gated channel means:

1. **Subscribe accepts.** `authorize_subscribe` falls back to a transient empty
   `RevocationRegistry` and strict skew (`:23532-23539`) and verifies the
   presented chain normally — a valid chain passes and the peer gets an
   accepting `Ack` (`:23563-23589`).
2. **The sweep is a no-op.** It returns early without a cache (`:2737-2739`).
3. **The first publish denies.** The token-gated branch requires
   `Some(cache)`; the `_ => false` arm treats its absence as unauthorized,
   revokes the `AuthGuard` entry, and drops the subscriber (`:23990-24024`).

The public documentation states the opposite of (1) — `set_token_cache`
(`mesh.rs:19038-19041`):

> When unset, `require_token` channels always reject — without a cache there's
> no way to validate presented tokens or find pre-cached ones.

A peer therefore receives a successful membership `Ack` for a subscription that
cannot deliver a single event, and the operator's mental model (from the docs)
does not match either observed behaviour.

- **Fix**: pick a policy and apply it at all three sites. Either reject a
  token-gated `Subscribe` immediately when no cache is installed — which
  matches the existing documentation and makes the `Ack` honest — or support
  cache-less verification consistently, in which case the publish path's
  `_ => false` must accept the transient-registry construction the subscribe
  path already builds, and the sweep must stop being a no-op. The first is
  simpler and fails closed earlier.

---

## LOW

### L1 — A registry-less `MeshNode` accepts every `Subscribe`

`net/crates/net/src/adapter/net/mesh.rs:23467-23470`, `:23783-23786`.

```rust
let Some(ref configs) = ctx.channel_configs else {
    // No registry → no ACL (test / permissive deployments).
    return (true, None);
};
```

`publish_many` mirrors it — a `None` `cfg_snapshot` means no gate runs and
`require_token` is `false` for the fan-out. A bare `MeshNode` therefore accepts
every subscribe from any session peer and treats every channel as open.

**Scope is narrower than rev 1 claimed.** Every shipped construction path
installs a registry by default: the Rust SDK (`sdk/src/mesh.rs:330-331`), the C
FFI (`ffi/mesh.rs:616-617`), Node (`bindings/node/src/lib.rs:1576-1578`), and
Python (`bindings/python/src/lib.rs:1377-1379`). Node and Python omit it only
behind an explicit permissive/test-oriented option. The exposure is limited to
embedders using `MeshNode` directly.

- **Fix**: make the permissive branch an explicit opt-in
  (`MeshNodeConfig::with_open_channels(true)`) rather than a consequence of an
  unset field, so "I forgot to install a registry" and "I want an open mesh" are
  distinguishable.

---

## INFO

### I1 — The `AuthGuard` exact ACL is one namespace shared by four readers under unconstrained `u64` keys

`net/crates/net/src/adapter/net/channel/guard.rs:361-386`,
`net/crates/net/src/adapter/net/mesh.rs:19294-19295` (sole production writer),
`:2425-2427` (`subscriber_origin_hash`),
`net/crates/net/src/adapter/net/redex/manager.rs:161`,
`net/crates/net/src/adapter/net/dataforts/blob/mesh.rs:964`, `:984`, `:1009`,
`:2899-2910`.

`AuthGuard::exact` — the collision-free `(origin_hash, ChannelName)` ACL —
backs publish admission (`mesh.rs:23962`), `Redex::open_file`
(`redex/manager.rs:161`), and four blob operations: `pin_authorized`,
`unpin_authorized`, `delete_chunk_authorized`, and `repair_blob_authorized`
(`blob/mesh.rs:964`, `:984`, `:1009`, `:2899`). Its only production writer is
the subscribe handler, on **every** accepted `Subscribe` including
gate-less channels (`mesh.rs:19294-19295`).

**Reclassified from Low to Info.** Rev 1 asserted the collision was contained
because subscribe writes a node id (`subscriber_origin_hash` is the identity
function, `mesh.rs:2425-2427`) while blob ops read an entity origin hash
(`entity.rs:47` vs `:61`, different blake2s domains). That containment is not
actually enforced by the source: `Redex::with_auth` and every
`*_authorized` blob entry point take a bare, unconstrained `u64`, and the
peer-facing blob paths have no production callers at all
(`bin/net-blob.rs:46` notes they are "reserved for the chain-fold"). So there is
no present privilege escalation — and no structural guarantee either. The
accurate finding is a **future wiring and type-safety hazard**: both keys are
`u64`, both are spelled `origin_hash` at the boundary, and the guard's rustdoc
advertises the exact tier as "the control-plane / storage authorization path"
(`guard.rs:22-31`) — inviting exactly the wiring that would collapse them. The
day a peer-facing blob op resolves its caller by node id, "subscribed to an open
channel" becomes "may pin, unpin, delete, and repair that channel's blobs."

- **Fix**: newtype the key so `NodeId` and `OriginHash` are not
  interchangeable, or split the data-plane grant from the storage grant into
  separate maps. At minimum, document at `allow_channel` that the subscribe
  path is its sole writer and what that implies for anything reading `exact`.

### I2 — The "presented tokens are installed into `TokenCache`" contract is stale in several places

`net/crates/net/src/adapter/net/mesh.rs:23416-23420` (`authorize_subscribe`),
`:19033-19042` (`set_token_cache`), and the corresponding constructor
documentation on the SDK, C FFI, Node, and Python surfaces.

The `TokenChain` migration removed that step. The presented credential is parsed
(`mesh.rs:23504`), verified inline inside `can_subscribe` →
`TokenChain::verify_authorizes`, and retained in `subscriber_chains`
(`:23584`) — it never enters the `TokenCache`, which on this path supplies only
the `RevocationRegistry` and clock skew (`:23533-23539`). The stale text points
the next reader at the wrong place for where peer-supplied credentials are
validated, which is the single most important question on this path. Sweep all
sites together; `set_token_cache`'s second stale claim is M4.

---

## What was checked and found sound

Recorded so a future pass does not re-derive it:

- **Chain verification** (`token.rs:770-845`): root anchor, leaf-to-presenter
  binding, per-link signature + time bounds + revocation floor, link continuity
  (`child.issuer == parent.subject`, parent carries `DELEGATE`, strictly
  decreasing depth, nested validity windows), and monotonic authority on every
  link. `verify_authorizes_presigned` skips only the ed25519 verifies on an
  immutable chain and re-checks everything else. (Width of the validity window
  is *not* checked — see M3.)
- **Entity binding** (`mesh.rs:22333-22426`): TOFU pin gated on
  `signature_verified && hop_count == 0`, with `entity_id.node_id() == node_id`
  enforced so a forwarder cannot pose as the origin, and rebind rejected.
  `peer_subnets` is gated on the same pair for the same reason.
- **Wire codecs**: `PermissionToken::from_bytes` requires exactly `WIRE_SIZE`;
  `TokenChain::from_bytes` bounds the count at `MAX_CHAIN_DEPTH` and requires
  exact length; `membership::decode` rejects trailing bytes after the outermost
  optional field and rejects a half-written token length prefix.
- **Scope algebra**: `contains(NONE)` short-circuits to `false`; `WILDCARD` must
  be set explicitly (`channel_hash == 0` is no longer an implicit wildcard);
  `ALL` excludes `WILDCARD`.
- **Bounds**: `MAX_TOKEN_CLOCK_SKEW_SECS` (5 min) is clamped on the receiving
  cache; `MAX_TOKEN_SLOTS` / `MAX_TOKENS_PER_SLOT` reject only *novel* keys at
  the cap so refreshes survive flood pressure. `MAX_TOKEN_TTL_SECS` is
  issuance-only — see M3.
- **Name validation** (`name.rs:106-154`): lowercase-only (no split-namespace
  footgun), restricted charset, no leading/trailing/double `/`, `.` and `..`
  segments rejected before any on-disk use.
- **Membership `Ack`** is bound to the node the request was sent to
  (`mesh.rs:23345-23347`), so a session peer cannot forge an ack by guessing a
  nonce.
- **nRPC client-fold response binding** (`cortex/rpc.rs:4045-4070`): a pending
  call is bound to the dispatched target node and a RESPONSE from any other
  session peer is dropped without touching the waiter.
- **Org-protected RPC response routing** (`mesh_rpc.rs:5882-5893`):
  `Protected` / `OwnerScoped` / `Granted` are `DirectOnly`, with the permissive
  reply prefix named as the reason.
- **Revocation reaches the data plane**: the publish path re-verifies the
  retained chain per fan-out and revokes inline; the sweep re-verifies with full
  signature checks rather than trusting the publish path's cached flag.
- **Auth-failure throttling** is per-peer and excludes resource-limit
  rejections, so it cannot be used to lock out a third party.

## Verification

Existing suites pass against `master` at audit time:

```text
channel_auth:           9 passed
channel_auth_hardening: 11 passed
Total:                 20 passed, 0 failed
```

They cover the intended paths only. No existing test covers:

- hostile ingress publishing (H1);
- a strict RPC config surviving any `serve_rpc*` variant (H2);
- cross-origin reply-channel subscription (H3);
- requested-hash vs sentinel-hash token behaviour on prefix channels, including
  the queue-group selection stall (M1);
- peer-to-specific-queue-group authority (M2);
- an overlong remote token TTL accepted on receipt (M3);
- registry-present / cache-absent subscribe-then-publish behaviour (M4).

## Suggested order

1. **H2** — smallest fix, largest blast radius. Every operator ACL on RPC
   channels is currently discarded silently; entry-if-absent plus eight
   regression witnesses. Also unblocks the documented mitigation for H3.
2. **H3** — scope the reply prefix to the pinned caller origin, with the
   identity-publication ordering rule worked out first.
3. **M1** — mechanical once the requested channel is threaded through; fixes
   the accept-then-deny inconsistency, the queue-group stall, and
   `set_publish_chain`. Prerequisite if H3 is fixed with a token gate rather
   than a name check.
4. **M4**, **M3**, **I2** — small, self-contained, and they make the remaining
   documentation trustworthy.
5. **H1** — the real work. Requires the wire-identity decision above and a
   publisher-side presentation handshake; scope it as its own design pass.
   Until it lands, document `require_token` as a read ACL.
6. **M2**, **L1**, **I1** — group-bound authority, opt-in permissiveness, and
   key type-safety.
