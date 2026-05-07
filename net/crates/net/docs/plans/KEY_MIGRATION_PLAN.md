# Key Migration — control-plane design

## Status

Design only. Describes a CortEX-layer (compute control plane) key-orchestration protocol that runs **over** Net but is **separate from** `SUBPROTOCOL_MIGRATION`. CortEX and RedEX (the control-plane event log referenced throughout this plan) do not yet exist in the tree — this document names the pieces they need to provide and the shape key orchestration should take when they land.

The only mode shippable against the current codebase is `KeyMode::PreProvisioned`, which is already the de-facto behaviour (keys installed on targets out-of-band by operators). Modes `Derived` and `Transfer` are design-only until the CortEX infrastructure exists.

## Why this is not part of `SUBPROTOCOL_MIGRATION`

`SUBPROTOCOL_MIGRATION` moves daemon **state** between nodes: snapshot, reassembly, restore, replay, cutover, activate. It is a single-purpose subprotocol with a tight 10-message wire vocabulary and a deterministic phase machine.

Key material is a different category of object:

- It governs **identity**, not state. A daemon's signing key authorizes future events forever, across any number of migrations.
- Its lifecycle is **not tied to a single migration**. Keys may be created long before the first migration, rotated independently of state moves, held in HSMs that never release bytes, or derived deterministically from a group root that never travels.
- Policy for "may this key leave this node" is a per-deployment concern (regulatory, threat-model, HSM-backed, air-gapped) that doesn't fit inside a message loop dedicated to state transfer.

Piggy-backing a `KeyTransfer` message onto `SUBPROTOCOL_MIGRATION` conflates the two lifecycles and hard-codes one policy ("always transfer keys at migration time, over the wire") into a protocol that should take key presence as an input, not an output. The right shape is: migration asks whether the target already has the key; if yes, proceed; if no, defer to the key-orchestration plane and wait.

## Scope

This plan covers the control-plane protocol for establishing that a given node holds a given daemon's `EntityKeypair` (or can produce signatures under it, for HSM-backed modes). It does **not** modify `SUBPROTOCOL_MIGRATION`. The only addition on the migration side is a precondition check: "target is key-ready for origin X" before the orchestrator fires `start_migration`.

## Key modes

```rust
pub enum KeyMode {
    /// Keys installed on every potential target out-of-band by operators.
    /// Migration moves state only. The current shipping behavior.
    PreProvisioned,

    /// Keys are derived deterministically from a shared root (per group,
    /// per fleet, or per capability domain) via KDF. No private key
    /// bytes ever travel on the wire. Target nodes that are in the
    /// derivation domain can produce the same keypair locally.
    Derived,

    /// Keys can be transferred at migration time, but only via the
    /// dedicated key-orchestration flow below — never as a field in a
    /// migration message.
    Transfer,
}
```

A deployment picks a mode per daemon or per fleet. The migration orchestrator reads the mode before starting a move and checks the appropriate precondition:

| Mode | Precondition the orchestrator checks |
|---|---|
| `PreProvisioned` | Target advertises the daemon's `entity_id` in its local factory registry. |
| `Derived` | Target is in the derivation domain for this daemon's group (i.e., has the root key and the derivation salt). |
| `Transfer` | A `KeyInstalled(entity_id, target_node_id)` event is present in the RedEX log for this target. |

If the precondition fails, migration does not fire. The orchestrator surfaces the reason to its caller ("target not key-ready: missing derived root") so the caller can decide whether to provision, pick a different target, or abort.

## Control-plane actors

Two new daemon types on the CortEX layer, both registered like any other `MeshDaemon` and scheduled by the existing placement logic.

### Key Orchestrator (one per CortEX cluster, typically few)

Central policy authority for key movement. Receives `KeyTransferRequested` events from RedEX and decides whether to permit them based on deployment policy.

Inputs (RedEX events it subscribes to):

- `KeyTransferRequested { entity_id, target_node_id, requester, reason }` — "someone wants target X to hold the signing key for entity Y, because reason Z (e.g., pre-migration staging, emergency re-placement)."

Outputs (RedEX events it emits):

- `KeyTransferApproved { entity_id, target_node_id, transport_hint }` — decision ticket. `transport_hint` names the cryptographic vehicle (today: routed Noise session between source and target; tomorrow: other).
- `KeyTransferDenied { entity_id, target_node_id, reason }` — policy rejection, with reason surfaced to the requester.

Policy is out of scope for the protocol — the orchestrator is a pluggable decision engine. Reference implementations might:

- Allow transfer to any node in a given subnet.
- Allow only N transfers per entity per time window (containment).
- Deny transfer to nodes that have not attested to a particular hardware profile.
- Require multi-party approval for high-value keys.

The orchestrator is **not** the sender of the key. It issues the approval; the actual transfer is source→target direct.

### Key Agent (one per node)

Local agent on every node that may hold keys. Listens for events addressed to its node and interacts with the local key store.

Inputs (RedEX events it subscribes to, filtered to its `node_id`):

- `KeyTransferApproved { entity_id, target_node_id = self, transport_hint }` — "we are cleared to receive the key for `entity_id`."

Outputs:

- `KeyInstalled { entity_id, target_node_id = self, mode, pub_fingerprint }` — emitted after the key is usable locally. `mode` tells downstream consumers whether it's `Transfer`-delivered, `Derived`-locally-computed, or `PreProvisioned`-already-present.
- `KeyRotated { entity_id, old_fingerprint, new_fingerprint }` — independent event; not tied to migration.
- `KeyRevoked { entity_id }` — key removed from this node.

Interactions:

- With the local `DaemonFactoryRegistry`: installs the key into a slot so that a subsequent `SUBPROTOCOL_MIGRATION` restore can find it. For `Derived` mode, this happens autonomously on startup or on capability change. For `Transfer` mode, it happens after receipt over the direct Noise session.
- With local key stores / HSMs: the Key Agent is the only component that ever touches raw key bytes on a node. Everything else asks it for "can you sign as entity_id" or "do you hold entity_id." HSM-backed modes surface as `KeyAgent::can_sign(entity_id) = true, key_is_extractable = false` — the daemon can run there but cannot be migrated out of that node.

## Transfer flow (`KeyMode::Transfer`)

```
Requester (e.g., migration              Key             Key Agent       Key Agent
 orchestrator, pre-migration            Orchestrator    (on source)     (on target)
 staging pass)
      │                                       │                │               │
      │ RedEX: KeyTransferRequested ────────► │                │               │
      │   (entity_id, target_node_id)         │                │               │
      │                                       │ (policy check) │               │
      │                                       │                │               │
      │   RedEX: KeyTransferApproved          │                │               │
      │   (entity_id, target, hint)           │                │               │
      │                                       │ ── observes ──►│               │
      │                                       │ ── observes ─────────────────► │
      │                                       │                │               │
      │                                       │                │   (source's agent
      │                                       │                │    opens direct
      │                                       │                │    Noise session
      │                                       │                │    to target via
      │                                       │                │    connect_routed)
      │                                       │                │               │
      │                                       │                │ ── key over
      │                                       │                │    Noise ───► │
      │                                       │                │               │
      │                                       │                │               │ verify
      │                                       │                │               │ entity_id
      │                                       │                │               │ matches
      │                                       │                │               │ install in
      │                                       │                │               │ local
      │                                       │                │               │ factory
      │                                       │                │               │
      │                                       │                │ RedEX: KeyInstalled
      │                                       │ ◄──────────────────────────────
      │◄──── observes KeyInstalled ───────────                 │               │
      │                                                                        │
      │ proceed with migration                                                 │
```

Properties:

- **Key bytes never touch the RedEX log.** RedEX carries events *about* key movement, not key material. The only bytes on the wire are the `KeyTransferApproved` ticket (no secret material) and the actual key traveling source→target over a direct Noise session (encrypted end-to-end between those two peers; no other node has the session keys).
- **Policy is in one place.** The Key Orchestrator is the single decision point. Requests come in through one channel; approvals come out through one channel. Audit and containment are just log queries.
- **Multiple requesters can share the mechanism.** Migration isn't the only reason to transfer a key — key rotation, recovery from a decommissioned node, emergency re-placement — all use the same flow.

## Crypto details carried over from earlier draft

These ideas were correct; they just belong in the key-orchestration flow, not in `SUBPROTOCOL_MIGRATION`:

- **Confidentiality:** source and target bring up a direct routed Noise session (via `connect_routed`, which the handshake-rewrite and multi-hop-routing plans enable). The key travels inside that session. Relays on the path see only Noise ciphertext.
- **Authenticity via identity check:** target's Key Agent verifies that the received key's derived `EntityId` equals the expected `entity_id` from the `KeyTransferApproved` ticket. Forging a matching private key requires breaking ed25519. No separate signature layer needed.
- **Zeroization:** `EntityKeypair` implements `Zeroize + ZeroizeOnDrop`. Source's Key Agent erases the key locally once the orchestrator observes the `KeyInstalled` event (if policy allows eviction) or holds on to it (if policy requires both nodes to retain for redundancy).
- **HSM/non-extractable keys:** `KeyMode::Transfer` is not available for entities whose local Key Agent reports `key_is_extractable = false`. Migration for those entities is constrained to nodes that already hold the key (PreProvisioned only) or can derive it (Derived mode, if the group topology allows).

## What this plan does NOT touch

- `SUBPROTOCOL_MIGRATION` message set, codec, handler, or state machine. Zero changes.
- `DaemonFactoryRegistry` public API, except the addition of a `register_transient(config, factory)` variant (no keypair at registration time) that a `KeyInstalled` event can later fill in. This is additive.
- The 6-phase migration lifecycle, which keeps running the same way it does today.

## Relationship to `SUBPROTOCOL_MIGRATION`

Migration reads `KeyMode` and asks "is the target key-ready?" as a precondition. Key readiness is one of:

- The target's factory registry has a `register_local` entry with the matching `entity_id` (PreProvisioned).
- The target is in the derivation domain and has the group root (Derived — the target's Key Agent can derive on demand).
- RedEX shows a `KeyInstalled { entity_id, target_node_id }` event from the target's Key Agent (Transfer).

If not ready and the mode is `Transfer`, the migration orchestrator can (a) emit a `KeyTransferRequested` event and wait for the resulting `KeyInstalled`, or (b) fail fast and let a higher-level scheduler handle staging. Both are policy choices for the migration orchestrator, not the key orchestrator.

If mode is `PreProvisioned` and the key is missing, migration fails with a clear "target not key-ready" error — no silent fallback to over-the-wire transfer.

## Actionable subset today

- **Document `KeyMode` as a concept** in `docs/COMPUTE.md`, with `PreProvisioned` as the only implemented mode and a pointer to this plan for the others.
- **`register_transient` on `DaemonFactoryRegistry`.** Additive API. Handler code that waits for a keypair to be installed is the interesting change; that landing is gated on RedEX being available so Key Agent has something to listen to.
- Nothing else is buildable until CortEX and RedEX land. This plan is the spec against which those are built, so the key-orchestration control plane drops in cleanly.

## Non-goals

- **Master key or certificate authority.** No hierarchical CA for entity identities. Each entity identity is self-certifying (ed25519 pub key = identity); the Key Orchestrator is a policy gate over movement, not a certificate signer.
- **In-protocol rotation.** Rotation is a Key Agent concern that emits `KeyRotated`; migration doesn't participate.
- **Multi-signature / threshold keys.** Out of scope; a future mode `KeyMode::Threshold` could be added without changing this plan's structure.
- **Federation across CortEX clusters.** One Key Orchestrator domain per CortEX cluster. Cross-cluster key movement is a separate problem (it would involve two orchestrators negotiating).

## Open questions

- **Where does `KeyTransferApproved` get published?** RedEX is the assumed transport for control-plane events, but we haven't specified its delivery semantics (at-least-once? fan-out? per-node subscription filter?). The shape of this plan is correct under any reasonable choice; specifics need to land with RedEX.
- **Does a Key Agent need its own keypair?** Almost certainly yes (it signs `KeyInstalled` events to prove the install happened on *this* node, not an impostor). That keypair itself is bootstrapped by ops — the one keypair we assume is always pre-provisioned is the Key Agent's own.
- **Revocation propagation.** `KeyRevoked` is easy to emit but hard to enforce globally. We assume RedEX eventually delivers to all Key Agents; tighter enforcement (e.g., "every node learns within N seconds") is a deployment tuning parameter.

These are questions about the surrounding control plane, not about the key-orchestration protocol itself.
