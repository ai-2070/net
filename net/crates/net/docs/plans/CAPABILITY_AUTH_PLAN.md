# Capability Announcement + Execution Auth Plan

**Status:** Draft for review.
**Scope:** add operator-controlled gating around two operations on the
mesh-wide capability surface: (1) who is allowed to **announce** a
given capability, and (2) who is allowed to **execute** it. Permissive
defaults preserve v0.x ergonomics; tightening lands as opt-in signed
policy events stamped through `RedEX → CortEX` so every decision is
auditable, replayable, and survives restart.

This plan is a *codified-policy* layer on top of the existing
acceptance discipline in `mesh::handle_capability_announcement`
(signature verification, TOFU `node_id ↔ entity_id` binding, hop-count
+ dedup). It does NOT replace any of that — it adds a second pass
that consults a folded policy view.

---

## Goals

1. **Permissive by default.** A cluster with no policy events emitted
   behaves identically to today: any signed peer may announce any
   capability, any caller may invoke any capability they can resolve.
2. **Gate-by-identity, three axes.** Allow / deny rules keyed on
   `NodeId`, `SubnetId`, or `CapabilityGroupId`. Per-capability,
   per-axis, independently configurable for announce-side and
   execute-side.
3. **Signed events end-to-end.** Policy itself is a signed event
   (ed25519, mirroring `CapabilityAnnouncement::sign`), stamped to a
   reserved RedEX channel so the audit log is the operational source
   of truth.
4. **CortEX-folded view.** Policy decisions read from a folded
   in-memory view rebuilt by replaying the policy channel — same
   pattern as the existing CortEX adapters. Hot-path lookups stay
   O(1) under the fold.
5. **Replayable.** A node that crashes and restarts re-folds the
   policy channel from disk; no out-of-band config.
6. **Layer-disambiguated.** Announce-side gate runs in
   `handle_capability_announcement` *after* sig + TOFU. Execute-side
   gate runs at the caller's resolution point (nRPC `call_service`,
   blob `fetch_range` for tag-gated content, future per-cap surfaces)
   *before* the wire op leaves the local node.

## Non-goals

- **Not a general ACL system.** Scope is capability-announce +
  capability-execute. Existing channel auth (`AuthGuard` /
  `WriteToken`) stays as it is; tokens and bloom filters are
  orthogonal and continue to gate channel pub/sub.
- **Not encrypting capability announcements.** They're already on a
  broadcast channel and operators rely on signatures + topology
  filters for routing. Adding confidentiality is a separate plan.
- **Not changing the existing wire format of `CapabilityAnnouncement`.**
  New fields, if any, ride alongside as `#[serde(default)]` (same
  pattern as `reflex_addr` and `hop_count`) — pre-this-plan peers
  continue to round-trip cleanly.
- **Not auto-revoking past announcements.** A new policy that denies
  a capability does not retroactively delete the index entry; it just
  prevents the *next* announcement from being accepted. Operators run
  an explicit `purge` step (see §8) if they want immediate cleanup.

---

## Existing surface (what's already in place)

The plan builds on these existing pieces (no churn to either):

- **`CapabilityAnnouncement`** (`src/adapter/net/behavior/capability.rs`):
  signed envelope with `node_id`, `entity_id`, `version`,
  `timestamp_ns`, `ttl_secs`, `capabilities: CapabilitySet`,
  optional `signature: Signature64`, `hop_count: u8`,
  optional `reflex_addr`.
- **`handle_capability_announcement`** (`src/adapter/net/mesh.rs:5399`):
  decode → enforce `node_id ↔ from_node` for direct peers →
  origin self-check → dedup on `(node_id, version, is_direct)` →
  optional `require_signed_capabilities` enforcement →
  ed25519 verify → cryptographic TOFU bind
  `EntityId::node_id() == ann.node_id` → fold into index.
- **`TopologyScope`** (`src/adapter/net/behavior/dataforts_capabilities.rs`):
  `Node | Zone | Region | Mesh` — the existing topology partition.
- **`CapabilitySet` / `CapabilityFilter`**: the discovery surface
  callers consult after the fold.
- **RedEX** (`src/adapter/net/redex/`): durable append-only event
  log per channel, with replication + retention. CortEX folds
  RedEX-channel events into queryable state.
- **`AuthGuard` / `WriteToken`**: the existing channel auth surface.
  This plan does NOT touch them; the new policy channel uses the
  same surface for its own publish discipline.

## New surface (what this plan ships)

1. **`SubnetId`** — 16-byte stable identifier for a topology
   partition. Operators assign nodes to subnets via cluster config
   (initial implementation reads from a `subnet:<id>` tag in
   `CapabilityAnnouncement.capabilities`). The lookup direction is
   `NodeId → SubnetId` and lives on the CortEX-folded peer view.
2. **`CapabilityGroupId`** — 32-byte identifier for an operator-
   defined named collection of `EntityId`s. Membership is itself a
   signed event on the policy channel (`MemberAdd` / `MemberRemove`),
   so a group's membership is replayable and auditable. Existing
   compute-layer `replica_group` / `standby_group` are unaffected —
   this is a separate, network-layer concept.
3. **`CapabilityAuthPolicy`** — the declarative spec. Per
   capability-tag, two rule sets (announce, execute), each a
   priority-ordered list of `(matcher, verdict)` pairs:
   - `matcher ∈ { Node(NodeId) | Subnet(SubnetId) | Group(CapabilityGroupId) | Any }`
   - `verdict ∈ { Allow | Deny }`
   - First-match-wins on the priority order (operator-controlled);
     no match = the permissive default (`Allow`).
4. **`CapabilityPolicyEvent`** — signed event ride on the new
   `capability/auth/policy/v1` RedEX channel. Variants:
   - `RuleSet { capability_tag, scope: Announce | Execute, rules: Vec<Rule>, version, signer, signature }`
   - `RuleClear { capability_tag, scope, version, signer, signature }`
   - `MemberAdd { group_id, member: EntityId, version, signer, signature }`
   - `MemberRemove { group_id, member: EntityId, version, signer, signature }`
   Every variant signed by an **operator key** (see §"Locked decisions"
   for the trust model).
5. **`CapabilityAuthView`** — CortEX-folded read view. Two methods
   on the hot path:
   - `may_announce(entity: &EntityId, node: NodeId, capability_tag: &str) -> AuthVerdict`
   - `may_execute(target: &EntityId, target_node: NodeId, caller: &EntityId, capability_tag: &str) -> AuthVerdict`
   Both O(1) HashMap probes + a short priority scan.

## Trust model

Three principals on the mesh:

| Principal | Key | Allowed actions |
|---|---|---|
| **Peer** | `EntityId` (ed25519 pubkey from `identity::EntityKeypair`) | Announce own capabilities, invoke capabilities they're permitted to execute. |
| **Operator** | Operator ed25519 keypair (new) | Publish `CapabilityPolicyEvent`s. The operator key set is fixed at substrate construction via a new `MeshConfig::policy_signers: Vec<EntityId>` field. |
| **Cluster** | (future) Threshold-signed cluster key | Out of scope for this plan; documented as forward-compat. |

Every policy event is signed by an operator key. Receivers reject
policy events whose signer is not in the configured `policy_signers`
set. The set itself is **not signed** — it's local config — so
operator key rotation is a config redeploy, same trust model as
existing daemon keypairs.

---

## Phases

Each phase ships independently, gated behind the `capability-auth`
Cargo feature (off by default initially, on by default once Phase E
lands). Phases keep wire compat with pre-this-plan peers throughout
— legacy peers see the new policy events as opaque bytes on a channel
they don't subscribe to.

### Phase A — primitives (no behavior change)

Adds the types + serialization with no enforcement wired:

- `pub struct SubnetId(pub [u8; 16]);` in
  `src/adapter/net/behavior/subnet.rs` with `from_tag(&str)`
  parser, `as_bytes()`, `Debug + Display + Hash`.
- `pub struct CapabilityGroupId(pub [u8; 32]);` in
  `src/adapter/net/behavior/capability_group.rs`.
- `pub struct CapabilityAuthPolicy`, `pub enum AuthMatcher`,
  `pub enum AuthVerdict`, `pub enum PolicyScope` in
  `src/adapter/net/behavior/capability_auth/policy.rs`.
- Postcard + serde-JSON round-trip tests.
- Operator-key + signer-set fields added to `MeshConfig` with
  defaulted empty values (permissive: no signers → fully open).

**Wire impact:** zero. New types not yet referenced from any hot
path.

### Phase B — signed policy event + RedEX channel

- Reserve channel name `capability/auth/policy/v1`. Add a
  `RESERVED_CHANNELS` constant in `src/adapter/net/redex/mod.rs`.
- Define `CapabilityPolicyEvent` enum + variant payload structs.
- Signing: `CapabilityPolicyEvent::sign(&signing_key) ->
  CapabilityPolicyEvent`. Mirrors `CapabilityAnnouncement::sign`
  exactly — same canonical serialization helper, same Ed25519
  curve, same 64-byte `Signature64` type re-exported.
- Verification: `CapabilityPolicyEvent::verify(&allowed_signers:
  &[EntityId]) -> Result<(), PolicyVerifyError>`. Rejects unknown
  signers + bad signatures.
- Publishing: operator-side helper
  `Mesh::publish_capability_policy(event)` that bumps the local
  `RedExFile` for the policy channel + cross-node-replicates via
  the existing replication runtime.
- 6+ unit tests including: forge-without-key fails, signer-not-
  in-set fails, valid round-trip succeeds, malformed wire bytes
  reject cleanly.

**Wire impact:** new RedEX channel. Legacy peers don't subscribe
and never see the bytes. Subscribers ignore unknown event variants
(serde `untagged` fallback).

### Phase C — CortEX fold builds the policy view

- New CortEX adapter `CapabilityAuthAdapter` in
  `src/adapter/net/cortex/capability_auth.rs`. Subscribes to the
  reserved policy channel; folds events into:

  ```rust
  struct CapabilityAuthView {
      // Per-capability rules: tag → (announce_rules, execute_rules).
      // Each rule list is operator-priority-ordered.
      rules: HashMap<String, (Vec<Rule>, Vec<Rule>)>,
      // Group membership: group_id → EntityId set.
      groups: HashMap<CapabilityGroupId, HashSet<EntityId>>,
      // Last-seen version per (capability_tag, scope, group_id) so
      // out-of-order delivery doesn't clobber a newer rule with an
      // older one.
      versions: HashMap<PolicyKey, u64>,
  }
  ```
- Pure-function fold methods unit-tested in isolation against
  hand-crafted event sequences (out-of-order, duplicate, version-
  rollback rejected).
- Snapshot accessor for diagnostics:
  `Mesh::capability_auth_snapshot() -> CapabilityAuthSnapshot`.

**Wire impact:** none. Pure local fold.

### Phase D — announce-side gate

- Add `auth_view: Option<Arc<CapabilityAuthView>>` to
  `DispatchCtx`. `None` = permissive (no policy substrate wired,
  legacy behavior).
- In `handle_capability_announcement`, immediately *after* the
  existing `node_id ↔ entity_id` TOFU bind and *before* the index
  fold, call:

  ```rust
  if let Some(view) = ctx.auth_view.as_ref() {
      for tag in ann.capabilities.tags() {
          if let AuthVerdict::Deny =
              view.may_announce(&ann.entity_id, ann.node_id, tag)
          {
              tracing::trace!(
                  entity = ?ann.entity_id,
                  tag = %tag,
                  "capability: announce denied by policy"
              );
              metrics.incr_announce_denied();
              return;
          }
      }
  }
  ```
- Bumps a new metric `capability_announce_denied_total{tag=...}`
  surfaced via the existing CortEX adapter Prometheus emit.
- Tests: an operator publishes a deny rule against `entity X` for
  tag `Y` → entity X's subsequent announcement carrying tag Y is
  dropped at the gate, log line emitted, metric incremented.
  Other tags on the same announcement still admit.
- Special case: an announcement that hits Deny on *some* tags but
  Allow on others is rewritten — the offending tags stripped — and
  the rewritten set folded. The rewrite is deterministic and
  documented (announce-side filter, not all-or-nothing rejection,
  so an operator can deny `kubernetes:cluster-admin` without
  nuking every other capability the entity advertises). Open
  question §1 below covers the alternative (all-or-nothing).

**Wire impact:** still none — gate is local to the receiver.

### Phase E — execute-side gate

- New trait `CapabilityExecutionGate` in
  `src/adapter/net/behavior/capability_auth/execute.rs`:

  ```rust
  pub trait CapabilityExecutionGate: Send + Sync {
      fn may_execute(
          &self,
          target: &EntityId,
          target_node: NodeId,
          caller: &EntityId,
          capability_tag: &str,
      ) -> AuthVerdict;
  }
  ```
- Default implementation `CortexExecutionGate` wraps the
  `CapabilityAuthView` from Phase C and exposes `may_execute`
  as-is.
- Wire points for the first round of integration:
  - **nRPC** — `Mesh::call_service` / `call_service_typed`:
    before issuing the wire frame, the caller side resolves the
    target's `EntityId` from `peer_entity_ids` and consults the
    gate with the service's capability tag (e.g.
    `nrpc:my-service`). Verdict `Deny` → return
    `CallError::CapabilityDenied` with the offending tag.
  - **Blob fetch** — `MeshBlobAdapter::fetch_range` (and the new
    `fetch_chunk` doc-hidden helper). Gate consulted on
    `dataforts:blob-*` tags when those tags identify content that
    requires authorization. Default: no blob tags require it,
    backwards-compatible.
- Receiver-side enforcement (defense in depth): the service
  callee independently consults the same view and rejects the
  request. A misbehaving caller that bypasses the local gate is
  still caught at the boundary. Reject is wire-typed
  (`RpcRejectError::CapabilityDenied`).
- Tests: cross-node integration covering both
  caller-side-blocks-call and callee-side-rejects-call paths.

**Wire impact:** one new error variant on the nRPC reject surface
(backward-compat: unknown variants decode as `Other` on legacy
clients).

### Phase F — operator CLI surface

New `net-blob` siblings under `net-policy`:

- `net policy allow <capability> <scope> <matcher>` — emit a signed
  `RuleSet` event.
- `net policy deny <capability> <scope> <matcher>` — same, with
  `Verdict::Deny`.
- `net policy group create <name>` → assigns a fresh
  `CapabilityGroupId`, prints it.
- `net policy group add <group> <entity>` — emit `MemberAdd`.
- `net policy group remove <group> <entity>` — emit `MemberRemove`.
- `net policy view` — dump the current folded view as JSON.
- `net policy verify <event-bytes>` — round-trip parse + verify.
- `net policy purge --capability <tag>` — re-walk the capability
  index and evict every entry that *would* be denied under the
  current policy. Use to enforce a new deny rule retroactively.

Each subcommand reads the operator key from
`--operator-key <file>` or `$NET_OPERATOR_KEY`. CLI integration
tests follow the existing `net_blob_cli` pattern (spawn the bin,
assert exit + JSON output).

### Phase G — bindings (Node + Python)

Mirrors B3 / C8 pattern. Declarative surface only — the policy
event types + capability gate enum are wrapped; the actual
publish/lookup happens by passing built events through binding-
facing helpers that round-trip into Rust.

- Node: `AuthMatcher`, `AuthVerdict`, `CapabilityPolicyEvent`
  classes with factory methods. Plus `publishCapabilityPolicy(event,
  operatorKeyPath)`.
- Python: `PyCapabilityPolicyEvent`, `PyAuthMatcher`,
  `PyAuthVerdict` pyclasses. Plus
  `publish_capability_policy(event, operator_key_path)`.
- Capability tag constants for the announce + execute capability
  surfaces exposed as binding-level strings.

### Phase H — conformance integration test

`tests/capability_auth_conformance.rs`:

1. **Permissive baseline:** no policy events emitted; every
   announce + execute path admits (matches today's behavior).
2. **Deny-by-node:** operator publishes a deny rule against
   NodeId X for tag Y; X's announcements carrying Y get dropped;
   X's executions against tag Y get
   `CapabilityDenied`.
3. **Deny-by-subnet:** operator publishes deny rule against
   `SubnetId(...)` for tag Y; every node in that subnet's
   announcements / executions denied.
4. **Allow-overrides-deny-by-priority:** rule list has Allow on
   `Group(G)` ahead of Deny on `Subnet(S)`; node in both denied
   except when member of G — the Allow wins.
5. **Replay survives restart:** Node restart re-folds the policy
   channel from RedEX; all rules + group memberships restored.
6. **Operator-key rotation:** rotate the configured signer set →
   pre-rotation events still verify (signed by old key still in
   the set during overlap), post-rotation old-key events reject
   after the operator removes that signer from config.

---

## Locked decisions to confirm with operators before implementation

These are the load-bearing design questions where the wrong answer
becomes expensive to revisit. Each gets a default; flagged for
operator sign-off.

1. **Announce-side rejection: per-tag rewrite vs all-or-nothing?**
   Default: per-tag rewrite (drop the offending tags, fold the
   remainder). Alternative: all-or-nothing (any deny → reject the
   whole announcement). Rewrite is more useful day-to-day; all-or-
   nothing is more obvious in audit logs. Going with rewrite.

2. **Conflict resolution: priority-list first-match-wins vs
   explicit precedence (Deny-wins / Allow-wins)?**
   Default: priority list, first-match-wins, operator-controlled.
   Alternative ("Deny always wins") is simpler reasoning but loses
   the ability to express "this specific entity is allowed inside
   an otherwise-denied group." Going with priority list.

3. **Fail-mode when the CortEX fold isn't ready (cold start before
   replay completes)?**
   Default: **permissive** (allow) until fold is ready. Tradeoff:
   a deny rule isn't enforced until replay reaches that event. The
   closed alternative (deny everything until ready) breaks startup
   for any cluster with any policy — operators couldn't bootstrap.
   Going with permissive + a `capability_auth_fold_ready` metric +
   structured log line for ops visibility.

4. **Operator-key source: single key vs threshold-set?**
   Default: a set of N ed25519 keys, any one of which can sign.
   Operators rotate by adding the new key, propagating, then
   removing the old. Threshold signatures (M-of-N) are future
   work and require a separate plan.

5. **Subnet membership source: tag-on-announcement vs config-file?**
   Default: `subnet:<id>` tag on the announcement, validated by
   the existing TOFU + signature path. Means a peer self-declares
   its subnet. Operators who don't trust peer self-declaration can
   layer a policy rule `Allow Subnet(X) → Deny Node(Y)` to override.
   Config-file alternative would require out-of-band membership
   distribution and is heavier.

6. **Group membership: signed events vs config-file?**
   Default: signed events on the policy channel, replayable +
   auditable like the rules themselves. Config-file alternative is
   simpler but bypasses the audit log.

7. **Hot-path cost budget:** at most one HashMap probe + one
   priority-scan (≤ 8 rules typical) per announcement, per
   execution. Measure in benchmarks; target < 1 μs per call.
   Caching the verdict across repeated lookups for the same
   `(entity, capability)` pair is allowed if the cache invalidates
   on fold updates.

8. **Retention on the policy channel:** unbounded (keep every
   event forever) vs operator-tunable retention. Default:
   unbounded for the v1 ship — the channel's natural throughput is
   "operator publishes a rule" which is rare. Add tunable
   retention as a follow-up if any cluster outgrows the default.

9. **Per-capability vs per-service granularity:** the gate keys on
   the capability tag string. A service that wants finer-grained
   auth (e.g. "tag X, method Y") layers its own logic above this;
   the gate provides the coarse boundary. Documented as a
   non-goal.

10. **Backward-compat with `require_signed_capabilities`:**
    the existing flag stays. A node with
    `require_signed_capabilities = false` AND
    `capability-auth` feature off behaves exactly as today. A node
    with the feature on but no policy events admitted folds an
    empty view → permissive default → also identical to today.

---

## Wire format reference

### `CapabilityPolicyEvent::RuleSet` (postcard-encoded body)

```text
+-------------------+----------+
| field             | bytes    |
+-------------------+----------+
| event_kind        | u8 = 0x01|  // 0x01=RuleSet, 0x02=RuleClear, 0x03=MemberAdd, 0x04=MemberRemove
| capability_tag    | len-prefixed UTF-8 (u16 LE len + bytes), ≤ 128 bytes
| scope             | u8 (0=Announce, 1=Execute)
| version           | u64 LE
| rule_count        | u8 (≤ 16 rules per RuleSet — operator splits past that)
| rules[N]          | per-rule struct: matcher (1+16/32 bytes) + verdict (u8)
| signer            | EntityId (32 bytes)
| signature         | Signature64 (64 bytes)
+-------------------+----------+
```

Total bounded at ~700 bytes per event, well under the existing
`SYNC_RESPONSE` per-event ceiling.

### Reserved channel name

`capability/auth/policy/v1` — the `/v1` suffix is the schema
version. A v2 schema lands on `capability/auth/policy/v2` with
parallel publishing during migration; CortEX folds both during
overlap, then operators retire v1.

---

## Risks

1. **Operator-key compromise.** A stolen operator key can publish
   arbitrary `RuleSet` events including granting itself
   `Allow Any` for sensitive capabilities. Mitigations: rotate by
   removing the compromised key from the configured signer set
   (immediate); replay the policy channel and `RuleClear` every
   event signed by the compromised key (recovery). Documented in
   the runbook.

2. **Fold-replay latency on cold-start clusters.** A 10K-event
   policy channel takes wall time to replay. During replay, every
   gate consult sees the permissive default → temporarily lax
   behavior. Mitigations: the `capability_auth_fold_ready` metric +
   structured log gate operators' "this node is policy-enforcing"
   monitoring; the alternative (deny until replay) trades off
   startup behavior for stricter behavior and is rejected per
   §3 above.

3. **Self-deny lockout.** An operator publishes
   `Deny Group(operators)` for the `net:policy-publish` capability →
   no operator can publish further events. Mitigation: a hardcoded
   reserved capability `net:policy-publish` whose acceptance gate
   ignores deny rules. The lock-out failure mode would otherwise
   require recovering the operator key + a fresh genesis policy
   event from a config-only path. Documented in `purge` semantics.

4. **Pre-this-plan announcement re-broadcast.** A legacy node that
   doesn't enforce policy can forward an announcement that this
   node would deny if it arrived directly. The forward arrives as
   `hop_count > 0` and still hits the announce-side gate; the
   verdict still applies. Verified by Phase H test case 4.

5. **Group membership churn.** Frequent `MemberAdd/Remove` events
   bloat the channel. Mitigation: per-group event compaction at
   `version` rollover (every 1000 events, the operator publishes a
   `MemberSnapshot` event that supersedes prior membership; the
   fold treats it as a reset). Compaction is operator-driven, not
   automatic.

6. **Sig-verification cost amplification.** Every policy event
   requires an ed25519 verify. A flood of events could DoS the fold.
   Mitigation: rate-limit fold ingestion at the CortEX adapter
   layer (existing pattern); operator-key signers are expected to be
   single-digit count, so the rate ceiling is low in practice.

---

## Test plan

### Unit (per-phase)

- Phase A: type round-trip, `SubnetId::from_tag` parses + rejects
  bad input, `CapabilityAuthPolicy` round-trips through serde.
- Phase B: signature verify + reject paths (wrong key, malformed
  bytes, missing signer in configured set), reserved-channel name
  pin.
- Phase C: fold semantics — out-of-order delivery preserves
  highest version, duplicate events idempotent, group
  add-then-remove leaves empty set.
- Phase D: announce-side gate denies, allows, per-tag rewrite,
  metrics increment.
- Phase E: execute-side gate caller + callee enforcement.

### Integration

- `tests/capability_auth_conformance.rs` per §Phase H above.
- Cross-binding: Node + Python publish a policy event, Rust core
  folds + enforces. Pins binding wire-format parity.

### Performance

- New criterion bench `auth_gate_throughput`: measures
  `may_announce` + `may_execute` call rate under a populated view
  (100 rules, 10 groups, 1K members). Target ≥ 5M calls/sec/core.

---

## Out-of-scope (explicit non-deliverables)

- Threshold signatures (M-of-N operator keys).
- Time-bounded rules (`Allow until <timestamp>`). Defer; operators
  use external scheduling to publish `RuleClear` at the right
  moment.
- Per-method nRPC granularity (Q9 above).
- Encrypted policy channel (operators rely on signatures +
  topology).
- Auto-revocation of past index entries on new deny rule (operators
  run `net policy purge` explicitly).
- Cluster-key signed events (vs operator-key). Documented as
  forward-compat header reserved in the wire format (signer field
  is a generic `EntityId`).

---

## Effort estimate

Roughly the same scope as the v0.3 Phase A work: ~3 weeks of
incremental commits across A → H. Largest risks: getting the
fold semantics right (Phase C) and the wire-format pinning
(Phase B). Both are well-precedented by the existing capability +
RedEX surfaces.

---

## Open questions for the next review pass

1. Should we surface a "policy advisory" mode where the gate logs
   what it would deny without actually denying — for operators
   staging a new rule before flipping it?
2. Is `Mesh` the right home for `publish_capability_policy`, or
   should it live on a separate `MeshOperator` handle that takes
   the operator key at construction and isn't routinely held?
   (Same question for the CLI — does the operator key live in the
   running daemon's memory at all?)
3. The "audit log" pitch implies the channel is read by ops
   dashboards. Do we want a separate `NetDb` query view for
   policy events, or is "subscribe to the channel" enough?
4. Should we ship a default `Allow operators net:policy-publish`
   policy event at substrate first-boot, or always-allow via the
   reserved-capability mechanism in Risk §3?

These don't block Phase A — they shape Phases F/G.
