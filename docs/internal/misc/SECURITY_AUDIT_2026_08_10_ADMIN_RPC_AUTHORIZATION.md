# Administrative RPC and Subprotocol Authorization Audit — 2026-08-10

## Status

**Verdict: HOLD.** The service inventory found three novel confirmed authorization defects, one witness-needed ownership candidate, and reconfirmed the aggregator and transfer-administration findings.

```text
Audited commit: 43b66dbc740381cf97e6cc1e19fa52fb7bf9c99a
Branch: security-4
Upstream: origin/security-4
Divergence during audit: 0 ahead / 0 behind
Repository: C:\Users\chief\Documents\git\net
```

This report is a frozen follow-on to `SECURITY_AUDIT_2026_08_10_CROSS_SUBSYSTEM.md`. It does not amend that report.

## Scope and classification

The review inventoried remotely reachable RPC handlers and native subprotocols across core Net, Compute, Aggregator, Dataforts, MeshDB, MeshOS, Deck, MCP, Cortex, payments, sidecars, and SDK helpers. Each surface was classified as public data plane, read-only operator, resource allocation, mutating operator, destructive administration, or internal-only. Registration/channel admission and handler-local checks were traced before retaining a finding.

## AUTH-01 — Arbitrary admitted peer can self-appoint as migration orchestrator and extract daemon state and signing identity

**Severity:** Critical  
**Confidence:** High  
**Status:** Novel, confirmed by complete production-path trace; deterministic witness required

### Violated invariant

Compute migration is a destructive administrative protocol controlled by a trusted orchestrator. Ordinary mesh admission must not authorize snapshot extraction, identity transfer, restore, cutover, or migration lock acquisition.

### Production path

Starting the SDK compute runtime automatically installs the migration subprotocol handler:

```text
net/crates/net/sdk/src/compute.rs:433-468
```

Any authenticated mesh peer can send native subprotocol `0x0500`. Ingress requires only a decrypted session and forwards `from_node` directly to the handler:

```text
net/crates/net/src/adapter/net/mesh.rs:19055-19098
```

The initial `TakeSnapshot` message has no orchestrator allowlist, permission token, operator signature, or pre-existing migration binding. The sender is recorded as orchestrator:

```text
net/crates/net/src/adapter/net/subprotocol/migration_handler.rs:279-312
```

The request snapshots the named local daemon:

```text
net/crates/net/src/adapter/net/compute/migration_source.rs:86-97
net/crates/net/src/adapter/net/compute/migration_source.rs:133-177
```

Snapshot chunks are returned to the initiating peer:

```text
net/crates/net/src/adapter/net/subprotocol/migration_handler.rs:374-386
```

The attacker controls `target_node`. If it selects itself and its Noise static key is known, the source seals the daemon's private Ed25519 seed to the attacker's X25519 key:

```text
net/crates/net/src/adapter/net/subprotocol/migration_handler.rs:314-329
net/crates/net/src/adapter/net/subprotocol/migration_handler.rs:1052-1081
net/crates/net/src/adapter/net/identity/envelope.rs:183-208
net/crates/net/src/adapter/net/identity/envelope.rs:219-270
```

The compute documentation distinguishes the migration orchestrator as trusted and treats Byzantine-orchestrator risk as a deployment concern:

```text
net/crates/net/docs/COMPUTE.md:93-95
net/crates/net/docs/COMPUTE.md:145-153
net/crates/net/docs/COMPUTE.md:194
```

The wire path currently allows the first arbitrary admitted peer to become that trusted actor.

### Prerequisites and impact

The attacker is an admitted mesh peer; the victim runs a started compute runtime; the attacker knows a daemon origin, which is an operational identifier rather than a secret.

Impact:

- extraction of arbitrary daemon snapshot state;
- recovery of daemon signing material when identity transport is enabled;
- future daemon impersonation and forged events/tokens;
- denial of legitimate migration through `AlreadyMigrating`;
- potential unauthorized restore and cutover progression.

### Required inverse witness

Construct a source runtime with a stateful daemon and identity context whose peer-static lookup contains an attacker key. Send an attacker-originated encoded:

```text
TakeSnapshot {
  daemon_origin,
  target_node: attacker
}
```

Assert that current outbound chunks target the attacker, decode to the daemon state, and include an identity envelope decryptable by the attacker's X25519 private key. After repair, the same request must fail before snapshot or migration-state mutation unless the authenticated sender holds exact migration authority.

Suggested future filter:

```text
cargo test -p net-mesh --features compute migration_take_snapshot_rejects_unapproved_orchestrator -- --exact
```

### Minimal repair boundary

Require an issuer-signed, current migration entitlement binding authenticated subject, source daemon/origin, target node, permitted phase/operations, validity, and generation. Verify it before installing migration state or snapshotting. A static operator allowlist may be an interim deployment control but is not a complete transferable migration-authority design.

## AUTH-02 — MeshDB remote execution drops documented session and chain authorization context

**Severity:** High  
**Confidence:** High  
**Status:** Novel, confirmed API/production-path defect; integration witness required

### Violated invariant

Remote chain reads must be evaluated against the requesting session's current chain-level authorization. The server API must preserve caller/session authority to the `ChainReader`.

### Documented contract

The shipped MeshDB plan requires chain reads to be gated by chain-level `subscribe_caps` and `AuthGuard`, using the requesting session's current guard:

```text
docs/internal/plans/MESHDB_PLAN.md:540-551
```

### Production path

Ingress accepts any nonzero authenticated `from_node` and passes the frame to the MeshDB router without an `AuthGuard` or permission context:

```text
net/crates/net/src/adapter/net/mesh.rs:19667-19697
```

The router decodes and forwards the request to the installed server:

```text
net/crates/net/src/adapter/net/behavior/meshdb/transport.rs:295-317
```

`MeshDbServer` receives only `(peer, request)` and executes the supplied plan:

```text
net/crates/net/src/adapter/net/behavior/meshdb/transport.rs:504-516
net/crates/net/src/adapter/net/behavior/meshdb/transport.rs:557-602
```

`ChainReader` has no caller, session, or authorization parameter:

```text
net/crates/net/src/adapter/net/behavior/meshdb/executor.rs:123-145
```

The production helper makes the service remotely reachable whenever the embedder supplies `Some(server)`:

```text
net/crates/net/src/adapter/net/behavior/meshdb/transport.rs:836-872
```

### Prerequisites and impact

The provider enables a MeshDB server; the attacker is an authenticated mesh peer and knows or discovers a chain origin.

The attacker can execute arbitrary reads, joins, and aggregations over every chain exposed through the configured reader. The current API cannot enforce the documented per-session chain ACL because authority context is discarded before reaching the reader.

There is no built-in production RedEX `ChainReader` at this commit; embedders supply the reader. The defect is therefore confirmed for configured server data, while a concrete RedEX ACL-bypass witness needs an integration reader.

### Required witness

Configure peer C as denied for one chain, install a MeshDB server on B, and prove C can currently retrieve rows through `MeshDbRequest::Execute`. The repaired path must pass authenticated session authority into the reader and deny C before query execution or task allocation.

Suggested filter:

```text
cargo test -p net-mesh --features meshdb --test meshdb_subprotocol_wire unauthorized_peer_cannot_read_protected_chain -- --exact
```

### Minimal repair boundary

Carry a verified session/chain authorization context through ingress, router, server, executor, and `ChainReader`. Do not reconstruct authority from self-declared request fields. Ensure long-running query tasks bind to the exact session/authorization generation and are invalidated consistently when authority becomes stale.

## AUTH-03 — Overflow RPC trusts caller-supplied sender identity and size

**Severity:** Medium  
**Confidence:** High  
**Status:** Novel, confirmed confused-identity path; focused witness required

### Violated invariant

Admission and attribution must use the authenticated caller identity. Resource admission must use an authoritative object size, not an attacker-selected claim.

### Production path

`OverflowPush` includes caller-controlled `sender_node_id`:

```text
net/crates/net/src/adapter/net/dataforts/blob/overflow.rs:72-87
```

The handler ignores authenticated `RpcContext.caller_origin`, looks up capabilities using the claimed sender, and bases scope/opt-in admission on those capabilities:

```text
net/crates/net/src/adapter/net/dataforts/blob/overflow.rs:342-362
net/crates/net/src/adapter/net/dataforts/blob/overflow.rs:407-425
```

The attacker also controls `size_bytes`, which drives disk-space admission:

```text
net/crates/net/src/adapter/net/dataforts/blob/admission.rs:205-250
```

After admission, prefetch opens replication state using only the hash rather than binding the admitted size:

```text
net/crates/net/src/adapter/net/dataforts/blob/mesh.rs:3870-3891
```

The service is remotely installed through:

```text
net/crates/net/src/adapter/net/mesh.rs:30519-30536
```

### Prerequisites and impact

The target has opted into overflow and installed the service; the attacker is an authenticated mesh peer and knows an overflow-enabled peer identity. A useful blob hash increases impact, but arbitrary hashes may still force channel/runtime allocation.

Impact:

- bypass of sender opt-in and scope checks;
- spoofed attribution and audit records;
- attacker-induced replication or prefetch work;
- disk-headroom bypass by claiming a small size for a larger object.

### Required witness

Authenticated caller A sends an overflow request claiming sender B, where only B satisfies overflow admission. Current code should accept based on B. The repaired path must reject unless authenticated caller identity equals the claimed sender or the claim is independently signed and authorized. A second witness must prove that authoritative object size—not `size_bytes` alone—controls admission.

### Minimal repair boundary

Remove self-declared sender identity from the security decision. Bind admission to `ctx.caller_origin` and an authenticated session. Resolve or verify authoritative object size before reserving disk headroom; bind that size to the accepted replication state.

## AUTH-04 — Aggregator registry administration remains ungated

**Severity:** High/Critical impact  
**Status:** Reconfirmed SEC-01 from the cross-subsystem report

The daemon installs full Spawn/Scale capability by default:

```text
net/crates/net/aggregator-daemon/src/lib.rs:312-334
```

Configuration contains no operator identity, token, or administrative allowlist:

```text
net/crates/net/aggregator-daemon/src/lib.rs:96-118
```

The handler ignores caller identity and executes `List`, `Spawn`, `Unregister`, and `Scale`:

```text
net/crates/net/src/adapter/net/behavior/aggregator/registry_service.rs:57-103
net/crates/net/src/adapter/net/behavior/aggregator/registry_service.rs:367-385
net/crates/net/src/adapter/net/behavior/aggregator/registry_service.rs:396-470
```

`List` additionally exposes `group_seed`:

```text
net/crates/net/src/adapter/net/behavior/aggregator/registry_service.rs:128-157
net/crates/net/src/adapter/net/behavior/aggregator/registry_service.rs:500-505
```

That seed derives replica private keypairs:

```text
net/crates/net/src/adapter/net/compute/replica_group.rs:40-81
```

This expands SEC-01 beyond workload disruption to replica identity-key compromise. The repair must omit private seed material from ordinary status output even for otherwise authorized readers unless exact key-export authority exists.

## AUTH-05 — Transfer inspection and cancellation remain ungated

**Severity:** Medium  
**Status:** Reconfirmed SEC-03 from the cross-subsystem report

`blob.transfers` exposes `List`, `Get`, and destructive `Cancel`; the handler ignores caller identity:

```text
net/crates/net/src/adapter/net/dataforts/blob/transfer_rpc.rs:44-60
net/crates/net/src/adapter/net/dataforts/blob/transfer_rpc.rs:93-107
net/crates/net/src/adapter/net/dataforts/blob/transfer_rpc.rs:125-137
```

The SDK helper registers it as ordinary public nRPC:

```text
net/crates/net/sdk/src/transport.rs:194-217
```

The existing unit test proves cancellation mutates live engine state without caller ownership:

```text
net/crates/net/src/adapter/net/dataforts/blob/transfer_rpc.rs:327-374
```

## Witness-needed candidate — A2A status and cancellation are not bound to submitter

**Severity:** Medium candidate  
**Confidence:** Medium  
**Status:** Authority decision and three-peer witness required

`serve_a2a` intentionally accepts task submission from any in-root peer:

```text
net/crates/net/sdk/src/mesh_a2a.rs:7-20
```

Status and cancellation use only attacker-supplied task IDs:

```text
net/crates/net/sdk/src/mesh_a2a.rs:96-117
net/crates/net/sdk/src/mesh_a2a.rs:144-178
```

`TaskRegistry` stores no owner identity:

```text
net/crates/net/sdk/src/a2a.rs:258-270
net/crates/net/sdk/src/a2a.rs:368-389
```

Status includes the complete brief, including prompt and context references:

```text
net/crates/net/sdk/src/a2a.rs:156-165
```

Default task IDs are random, so an unrelated peer generally needs a leaked or observed ID. The broad same-root submission policy may also intend bearer-ID administration. Do not promote without deciding the ownership contract and proving that peer C can inspect/cancel peer A's task after learning the ID.

## Service inventory conclusions

| Surface | Classification | Authorization conclusion |
|---|---|---|
| Core/Cortex `serve_rpc*` | Public application data plane | Authenticated origin plus capability admission; protected modes apply organization admission. No novel framework bypass retained. |
| Compute migration | Destructive administration and secret transfer | **AUTH-01 confirmed.** |
| Aggregator registry | Status, resource allocation, mutating/destructive administration | **AUTH-04 / SEC-01 reconfirmed; seed disclosure added.** |
| MeshDB | Remote reads, joins, aggregates, task allocation | **AUTH-02 confirmed.** Per-peer cancellation keying itself is sound. |
| Dataforts overflow | Resource allocation and replication | **AUTH-03 confirmed.** |
| Dataforts transfers | Metadata and destructive cancellation | **AUTH-05 / SEC-03 reconfirmed.** |
| A2A | Submit, status, cancellation | Submission intentionally public in-root; ownership candidate remains. |
| Enrollment/renewal | Identity admission | Invite/operator approval and signed-chain validation close reviewed paths. |
| Payments quote/pay | Quote and settlement | Signed/fresh/replay-checked and provider-admitted; no finding retained. |
| Native tools/MCP | Tool invocation | Public free tools are intentional; priced paths fail closed; owner/delegation gates applied. |
| Org/subnet SDK services | Protected application-defined operations | Verified organization admission required by core protected registration. |
| MeshOS | Operator-signed RedEX admin events | No remote administrative RPC found in this inventory. |
| Deck | Local/operator dashboard | No production remote administrative handler found. |
| MCP stdio sidecars | Local child-process invocation | Remote exposure only through scoped wrapper handler; stdio itself is local. |

## Verification and limitations

No live services or network witnesses were launched. No repository files were changed by the auditor. Generic application-defined handlers cannot be classified without the embedding application, but production framework registration seams were inventoried. Cross-language wrappers delegate to the same server-side authorization paths and did not add a distinct network gate.

## Repair order and acceptance

1. AUTH-01 migration orchestrator authorization and secret transfer.
2. AUTH-04 aggregator administration and group-seed disclosure.
3. AUTH-02 MeshDB authority propagation.
4. AUTH-03 overflow authenticated identity and authoritative size.
5. AUTH-05 transfer ownership/administration.
6. Decide and witness A2A ownership semantics.

Each repair needs an inverse witness against the audited behavior, the smallest production change, positive authorized controls, exact-head focused and integration tests with nonzero counts, clean Git state, `git diff --check`, and green required CI.
