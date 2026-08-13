# Cross-Subsystem Security Audit — 2026-08-10

## Status

**Verdict: HOLD pending repair and inverse witnesses.**

This report records a read-only, parallel security review of Net at the exact commit below. It is a finding packet, not a repair or acceptance report.

```text
Audited commit: 43b66dbc740381cf97e6cc1e19fa52fb7bf9c99a
Branch at final report creation: security-4
Remote tracking branch: origin/security-4
Repository: C:\Users\chief\Documents\git\net
```

The audited checkout was synchronized with its remote tracking branch and had no tracked modifications before this report was created. No product source files were edited by the auditors.

## Scope and method

Three independent review lanes inspected:

1. authentication, authorization, cryptographic identity, admission, revocation, and distributed authority;
2. unsafe Rust, C/Go/Python/Node FFI, parsers, concurrency, lifecycle, and denial of service;
3. SDKs, CLI, daemons, sidecars, filesystem/configuration behavior, secret handling, and cross-language boundaries.

A finding was retained only when the reviewer identified a reachable production path, attacker prerequisites, a violated invariant, exact source evidence, impact, and either a witness or a precise witness design. Generic hardening advice and unsupported speculation were excluded.

The authority lane found no surviving vulnerability after reviewing Noise and routed handshakes, session-to-node binding, protected organization admission, subnet grants and revocation floors, payload-origin binding, response-route caching, and protected-routing downgrade behavior.

The combined review retained:

- one high-severity finding;
- four medium, medium-to-high, or deployment-dependent findings;
- one low-severity finding;
- one medium-severity candidate requiring a load witness.

The aggregator-registry item was initially overstated as a high-severity arbitrary-workload administration defect. Source re-review established that its templates instantiate only configured capability/reservation fold-summary workers. This packet corrects that item below.

No confirmed memory-corruption, FFI use-after-free, malformed-parser panic, or deadlock vulnerability was established by the completed primary review lanes. The extended unsafe/FFI specialist run was still pending when this packet was frozen; any independent findings from that run must be recorded in a separate follow-on report rather than silently appended here.

---

## SEC-01 — Aggregator registry uses mesh admission as its only operator boundary

**Severity:** Medium, deployment-dependent
**CWE:** CWE-862 only when ordinary mesh members are outside the intended aggregation-operator trust set
**Confidence:** High on mechanism; security classification depends on deployment intent
**Status:** Corrected after source re-review; authority decision and bounded-impact witnesses required

### Corrected system boundary

The aggregator daemon is a specialized fold reducer, not an arbitrary workload host. Its built-in templates configure only:

- a source subnet;
- capability and/or reservation fold kinds;
- a summary interval;
- a replica count.

The workers read already-admitted `CapabilityFold` and `ReservationFold` state and publish bucketed `SummaryAnnouncement` rows such as `idle`, `busy`, `reserved`, and `faulty`:

```text
net/crates/net/src/adapter/net/behavior/aggregator/daemon.rs:366-411
net/crates/net/src/adapter/net/behavior/aggregator/summarizer.rs:99-160
```

Spawn, Scale, and Unregister create, resize, or stop only those operator-configured summary reducers. They do not mutate the signed source announcements, forge provider identities, or deploy arbitrary application code.

### Actual control surface

The daemon installs the registry handler, including Spawn, Scale, and Unregister:

```text
net/crates/net/aggregator-daemon/src/lib.rs:326-334
```

`RegistryHandler::call` does not evaluate caller identity or a separate operator token before invoking the configured operation:

```text
net/crates/net/src/adapter/net/behavior/aggregator/registry_service.rs:366-385
```

The operations execute at:

```text
Spawn:      net/crates/net/src/adapter/net/behavior/aggregator/registry_service.rs:405-428
Unregister: net/crates/net/src/adapter/net/behavior/aggregator/registry_service.rs:430-435
Scale:      net/crates/net/src/adapter/net/behavior/aggregator/registry_service.rs:437-472
```

### Security classification

If possession of the mesh PSK intentionally means trusted aggregation-operator membership, mesh admission is the administrative ACL and no missing-authorization vulnerability is established.

If ordinary workers or providers share the PSK, a non-operator can:

- suppress aggregate visibility by stopping summary groups;
- resize summary replicas;
- repeatedly spawn distinct configured groups and create host-resource pressure;
- inspect group status.

That is a deployment-dependent control-plane availability/resource issue. It does not compromise the authenticity of underlying capability or reservation announcements.

### Existing evidence and required witnesses

The focused test below proves that an RPC-shaped Unregister reaches group shutdown:

```text
adapter::net::behavior::aggregator::registry_service::tests::unregister_drives_group_shutdown_and_returns_existed_true
```

Required next steps:

1. Declare whether mesh admission intentionally confers aggregation-operator authority.
2. If it does not, prove a lower-trust admitted member can invoke Spawn, Scale, and Unregister.
3. Measure summary-availability impact and repeated distinct-name Spawn resource growth; do not assume unbounded impact without a quota witness.
4. Preserve the invariant that signed source announcements and canonical folds remain untouched.

### Separate witness-needed seed disclosure

`RegistryRequest::List` returns `group_seed`, which can derive replica entity keypairs:

```text
net/crates/net/src/adapter/net/behavior/aggregator/registry_service.rs:128-157
net/crates/net/src/adapter/net/behavior/aggregator/registry_service.rs:500-505
net/crates/net/src/adapter/net/compute/replica_group.rs:40-81
```

This is sensitive key material, but the audit did not prove that those replica identities can forge accepted summaries or exercise another security-bearing authority. Summary publication serializes a plain `SummaryAnnouncement` and calls the host `MeshNode::publish`:

```text
net/crates/net/src/adapter/net/behavior/aggregator/daemon.rs:433-452
```

Do not classify this as provider-key or summary-signature compromise until an end-to-end witness proves what possession of a derived replica key authorizes. Ordinary status output should nevertheless omit private seed material unless exact key-export authority is intended.

---

## SEC-02 — Pingwave admission permits unbounded persistent topology-state growth

**Severity:** High  
**CWE:** CWE-400, Uncontrolled Resource Consumption; CWE-770, Allocation of Resources Without Limits  
**Confidence:** High  
**Status:** Confirmed production path; deterministic inverse RED required

### Violated invariant

The configured topology bound must cap attacker-influenced node, edge, deduplication, learned-route, and forwarding state.

### Attacker prerequisite

A malicious directly connected mesh peer, or an attacker able to send UDP datagrams with the source tuple of a registered direct peer.

### Production path

`EnhancedPingwave` uses a fixed 72-byte unsigned format:

```text
net/crates/net/src/adapter/net/behavior/proximity.rs:21-44
net/crates/net/src/adapter/net/behavior/proximity.rs:120-212
```

`ProximityConfig` exposes a `max_nodes` limit, but admission does not enforce it:

```text
net/crates/net/src/adapter/net/behavior/proximity.rs:435-463
```

The graph maintains independently growing node, edge, and seen-pingwave maps:

```text
net/crates/net/src/adapter/net/behavior/proximity.rs:468-499
```

`admit_pingwave_from` rejects only self-origin and exact duplicate tuples before inserting fresh attacker-selected `(origin_id, seq)` keys, nodes, and edges:

```text
net/crates/net/src/adapter/net/behavior/proximity.rs:656-746
```

The UDP receive path gates the datagram using only the registered source address:

```text
net/crates/net/src/adapter/net/mesh.rs:17902-17943
```

Accepted pingwaves can install learned routes and, with positive TTL, trigger forwarding to peers:

```text
net/crates/net/src/adapter/net/mesh.rs:17989-18040
```

A cleanup method exists:

```text
net/crates/net/src/adapter/net/behavior/proximity.rs:1089-1124
```

Repository-wide production call-site inspection found it invoked only in tests. A separate older `LocalGraph` already carries explicit node and dedup caps and documents the same flood threat:

```text
net/crates/net/src/adapter/net/swarm.rs:430-455
net/crates/net/src/adapter/net/swarm.rs:531-592
```

### Attack sequence

1. Send fixed-size `EnhancedPingwave` datagrams from a registered adjacent source.
2. Select a fresh random `origin_id` and/or `seq` for each datagram.
3. Keep `hop_count` below the maximum.
4. Use `ttl = 0` for state exhaustion without rebroadcast, or a positive TTL for additional network and scheduler amplification.
5. Each novel tuple grows topology/dedup state and may install a learned route.

### Impact

Random 256-bit origins make accidental deduplication negligible. Sustained traffic can grow process-lifetime topology, deduplication, routing, and edge state until memory exhaustion terminates the node. Positive-TTL traffic additionally causes O(peer-count) sends and per-pingwave task creation.

### Required inverse witnesses

**Deterministic unit RED**

1. Construct `ProximityGraph` with `max_nodes = 1`.
2. Repeatedly call `admit_pingwave_from` from one registered sender using distinct origin IDs and sequence numbers.
3. Assert that node, deduplication, and edge counts never exceed their configured caps.
4. Prove that the current implementation violates those assertions.

**End-to-end RED**

Send equivalent wire frames from a connected peer and observe process RSS, proximity statistics, learned routing-table size, spawned forwarding work, and outbound packet counts.

The existing cleanup test passed but proves only that cleanup works when called manually:

```text
adapter::net::behavior::proximity::tests::test_cleanup
```

### Minimal repair boundary

- Enforce `config.max_nodes` before inserting a novel origin.
- Add explicit caps for seen-pingwave tuples, edges, and learned routes.
- Run cleanup from a shutdown-aware production lifecycle.
- Authenticate pingwaves under the adjacent session instead of trusting only a UDP source tuple.
- Apply per-session token-bucket admission before map mutation or forwarding.
- Replace one-task-per-forward behavior with a bounded worker/queue or load-shedding send path.

---

## SEC-03 — Any admitted mesh participant can inspect and cancel another node's transfers

**Severity:** Medium  
**CWE:** CWE-862, Missing Authorization  
**Confidence:** High  
**Status:** Confirmed production path; inverse authorization witness required

### Violated invariant

Remote transfer administration must require explicit operator authority rather than mesh membership alone.

### Attacker prerequisite

Any authenticated mesh participant able to invoke the target node's transfer service.

### Production path

`blob.transfers` exposes `List`, `Get`, and `Cancel` operations:

```text
net/crates/net/src/adapter/net/dataforts/blob/transfer_rpc.rs:35-76
```

`Cancel` removes a pending transfer and causes the target fetch to fail:

```text
net/crates/net/src/adapter/net/dataforts/blob/transfer_rpc.rs:93-107
```

`TransferRpcHandler::call` decodes the request but ignores `ctx.caller_origin` and performs the operation directly:

```text
net/crates/net/src/adapter/net/dataforts/blob/transfer_rpc.rs:125-137
```

The SDK installs this handler through the standard RPC service path:

```text
net/crates/net/sdk/src/transport.rs:194-218
```

### Impact

- Repeated cancellation can cause targeted blob and directory retrieval failures.
- `List` and `Get` disclose in-flight stream IDs, holder identities, expected content hashes, transferred byte counts, and sizes.

### Existing evidence

The following focused test passed and confirms that the handler exposes pending state and that `Cancel` removes it:

```text
adapter::net::dataforts::blob::transfer_rpc::tests::answer_reflects_engine_state_for_each_verb
```

### Required inverse witness

1. Start a transfer owned by one peer.
2. From a second admitted but unauthorized peer, call `List` and `Get` and prove metadata disclosure.
3. Invoke `Cancel` and prove the owner's fetch fails.
4. After repair, prove that unauthorized callers cannot observe or mutate the transfer while an authorized operator still can.

### Minimal repair boundary

Protect transfer administration with an operator-scoped token or signed entitlement and verify the authenticated caller in `TransferRpcHandler`. Split read-only status from cancellation if broad visibility is intended; cancellation requires the stronger authority.

---

## SEC-04 — A hostile directory manifest can request setuid or setgid file modes

**Severity:** Medium  
**CWE:** CWE-732, Incorrect Permission Assignment for Critical Resource  
**Confidence:** High from source inspection  
**Status:** Production path confirmed; Linux runtime witness required

### Violated invariant

Remote directory metadata must not create locally privileged executable files.

### Attacker prerequisite

The victim fetches a directory from an attacker-controlled source while running with sufficient privilege to set special mode bits, most significantly as root, and installs it where another local user can execute the result.

### Production path

`DirEntry::File.mode` is deserialized from the remote manifest:

```text
net/crates/net/src/adapter/net/dataforts/dir.rs:145-165
```

Reconstruction carries the value through file creation:

```text
net/crates/net/src/adapter/net/dataforts/dir.rs:576-621
```

Unix mode application passes the remote value directly to `Permissions::from_mode`:

```text
net/crates/net/src/adapter/net/dataforts/dir.rs:1021-1033
```

### Impact

An attacker can request mode `04755` or `02755` for attacker-supplied content. A privileged extraction can therefore create a root-owned setuid/setgid executable, yielding a local privilege-escalation primitive when the destination is executable by another user.

### Required Linux inverse witness

Inside a disposable root-owned namespace or VM:

1. Construct and serve a directory manifest containing a regular executable with mode `04755`.
2. Fetch the directory as root.
3. Verify that the resulting file mode is `4755`.
4. Only inside the disposable environment, execute it as an unprivileged test account and confirm whether effective identity changes.
5. After repair, prove that only ordinary permission bits survive.

### Minimal repair boundary

Mask remotely supplied regular-file modes to an explicit allowlist such as `mode & 0o777`. Always remove setuid, setgid, and sticky bits. A hardened extraction mode may additionally remove group/other write bits.

---

## SEC-05 — Credential-file permission defaults can expose mesh PSKs and identity seeds

**Severity:** Medium to High, deployment-dependent  
**CWE:** CWE-732, Incorrect Permission Assignment for Critical Resource  
**Confidence:** High for Unix continuation behavior; Medium for Windows ACL impact  
**Status:** Structural behavior confirmed; cross-platform witnesses required

### Severity boundary

- **High** when Net claims isolation from hostile local users or runs on shared infrastructure.
- **Medium** under an explicitly single-user host model.

### Violated invariant

A PSK that acts as the mesh-membership root secret, and private identity seed material, must not be accepted from or written to locations readable by unrelated local principals.

### Production path

CLI profiles store the PSK alongside remote bootstrap information:

```text
net/crates/net/cli/src/config.rs:61-83
```

The profile loader parses the file without a permission gate:

```text
net/crates/net/cli/src/config.rs:97-118
```

Aggregator configuration also embeds `psk_hex`:

```text
net/crates/net/aggregator-daemon/src/lib.rs:96-118
```

The daemon detects permissive Unix permissions but warns and continues:

```text
net/crates/net/aggregator-daemon/src/lib.rs:284-310
net/crates/net/aggregator-daemon/src/lib.rs:830-868
```

On Windows, the aggregator check is advisory/no-op:

```text
net/crates/net/aggregator-daemon/src/lib.rs:871-882
```

Secret seed writers rely on inherited Windows ACLs, and permission enforcement is a no-op there:

```text
net/crates/net/cli/src/commands/identity.rs:453-459
net/crates/net/cli/src/commands/identity.rs:531-536
```

### Impact

Reading a CLI profile provides the PSK plus bootstrap values needed to attach to the configured mesh. Reading daemon configuration exposes its PSK. Inherited permissive Windows ACLs may also expose generated Ed25519 seed material. Mesh admission then amplifies SEC-01 and SEC-03.

### Required witnesses

**Unix**

1. Create a profile or daemon configuration with mode `0644`.
2. Prove that the CLI or daemon continues.
3. From another uid, read the PSK and attach using the stored bootstrap data.
4. Attempt the administrative operations described by SEC-01 and SEC-03.

**Windows**

Repeat with a directory whose inherited DACL grants a second local account read access. Verify both configuration loading and generated seed-file ACLs.

### Minimal repair boundary

Create secret-bearing files with owner-only permissions or an owner-only security descriptor. Refuse unsafe existing permissions by default. Implement a real Windows owner/DACL check. If compatibility requires an escape hatch, make it an explicit `--insecure-permissions` override rather than warning and continuing silently.

---

## SEC-06 — Secret-bearing TOML diagnostics may reproduce PSKs or private seeds in logs

**Severity:** Low  
**CWE:** CWE-532, Insertion of Sensitive Information into Log File  
**Confidence:** Medium-High  
**Status:** Precise regression witness required

### Violated invariant

A parser failure involving a secret-bearing field must not reproduce the source text containing that secret in a less-protected output channel.

### Attacker prerequisite

A malformed secret-bearing line caused by operator error, templating failure, or configuration modification, plus access to stderr or daemon logs that is broader than access to the original secret file.

### Production path

CLI profile parsing retains and displays `toml::de::Error`, which can include a source span:

```text
net/crates/net/cli/src/config.rs:107-111
net/crates/net/cli/src/config.rs:128-142
net/crates/net/cli/src/main.rs:244-250
```

Identity commands interpolate parser errors while the input can contain `seed_hex`:

```text
net/crates/net/cli/src/commands/identity.rs:393-411
```

Aggregator parsing similarly retains and logs the full TOML error:

```text
net/crates/net/aggregator-daemon/src/lib.rs:171-174
net/crates/net/aggregator-daemon/src/lib.rs:284-302
net/crates/net/aggregator-daemon/src/main.rs:13-17
```

### Impact

A malformed `psk_hex` or `seed_hex` line may be copied into stderr, CI output, journald, or centralized logs.

### Required inverse witness

For each secret-bearing parser, place a recognizable dummy marker in a malformed secret line, run the corresponding CLI or daemon path, capture stderr/log output, and assert that the marker is currently present. After repair, assert that the marker is absent while a useful location/reason-only error remains.

### Minimal repair boundary

Map secret-file parse failures to sanitized errors that omit source excerpts and secret-bearing input. Reuse the hardened parsing style already present in context and organization/subnet key parsers.

---

## SEC-07 — Legacy route forwarding may permit unbounded per-datagram task amplification

**Severity:** Medium candidate  
**CWE:** CWE-400, Uncontrolled Resource Consumption  
**Confidence:** Medium  
**Status:** Production path confirmed; practical resource buildup requires a load witness

### Violated invariant

Unauthenticated packet ingress must not create unbounded asynchronous work or retained packet buffers under congested egress.

### Attacker prerequisite

Network reachability to a legacy-mode relay plus knowledge or discovery of a destination with a routing-table entry.

### Production path

Legacy routed packets are classified using framing magic and minimum length:

```text
net/crates/net/src/adapter/net/mesh.rs:18046-18079
```

Non-local legacy packets are forwarded without decrypting the inner packet:

```text
net/crates/net/src/adapter/net/mesh.rs:18146-18159
```

Every accepted packet spawns a task around `send_to(...).await`:

```text
net/crates/net/src/adapter/net/mesh.rs:18167-18207
```

The protected route-hop path already avoids this pattern by using `try_send_to` and shedding load:

```text
net/crates/net/src/adapter/net/mesh.rs:18489-18517
```

### Potential impact

With blocked or congested egress, spawned tasks can retain packet buffers and scheduler state. Unauthenticated packet input can therefore become heap and executor pressure.

### Required witness

1. Populate one valid legacy route.
2. Impede or saturate the egress socket.
3. Flood minimum-sized valid routed envelopes from an unknown source.
4. Measure live Tokio tasks, retained buffers, RSS, executor latency, and packet-drop behavior.
5. Compare against protected forwarding, which should remain bounded.

Do not promote this candidate to a confirmed vulnerability unless the witness demonstrates meaningful task or memory accumulation.

### Minimal repair boundary

Require an authenticated adjacent session before legacy relay or disable legacy forwarding by default. Replace per-packet spawning with `try_send_to` or a bounded egress queue.

---

## Authority and identity leads ruled out

The dedicated authority lane retained no finding after source and focused-test review. In particular:

- Noise `NKpsk0` leaves the initiator anonymous by design, but protected subnet admission compensates with a fresh Ed25519-signed presentation bound to subject, credential set, session, verifier, nonce, target, and rights.
- Protected organization calls bind claimed origin to authenticated session identity before admission or dispatch.
- Packet-origin and payload-origin splitting is rejected before admission and cache mutation.
- Self-declared subnet/group tags do not grant callee-side capability authority; only `allowed_nodes` remains security-bearing in that path.
- Protected services do not inherit unrelated legacy target-wide allow-list semantics.
- Response-route caching requires the claimed origin to equal the authenticated peer's pinned origin and includes session node, origin, and call ID in the key.
- Revocation/currentness floors invalidate compiled subnet authority.
- Protected gateways reject untagged legacy routed packets rather than silently downgrading.
- Older routed-handshake replay and key-rotation findings appeared repaired by same-static ephemeral replay detection, atomic entry-based installation, and live-session rotation refusal.

Focused witnesses reported passing:

```text
adapter::net::crypto::tests::test_noise_handshake
adapter::net::behavior::fold::capability_bridge::tests::may_admit_denies_what_may_execute_narrows_to
capability_broadcast::peer_static_x25519_returns_peer_noise_pubkey_after_handshake
mesh_rpc::...::protected_gate_binds_authenticated_identity_to_claimed_origin
subnet_session_auth::presentation_signed_by_another_entity_is_refused
subnet_session_auth::accepted_floor_invalidates_stale_contexts
```

These ruled-out leads are not security acceptance for every authority feature combination; they explain why no authority candidate was promoted in this review.

## Other false leads ruled out

- FFI handle free/use races use a leaked-outer-box guard protocol with a single-winner free and active-operation quiescence; the completed primary review established no concrete production UAF.
- Oversized FFI counts generally remain within documented unsafe foreign-caller contracts; no safe binding path exposing attacker-controlled mismatched arrays was established.
- Blob reference and manifest decoding includes caps on wire size, chunk count, total size, tree depth, bulk fetch, transfer size, and per-range allocation.
- UDP parsing checks fixed header sizes before slicing, and receive buffers are bounded by `MAX_PACKET_SIZE`.
- Heartbeats are authenticated before liveness mutation, and failure observations carry session incarnation.
- Protected-route egress snapshots and rechecks address, identity, route binding, and session ID before sealing.
- Reviewed DashMap hot paths generally release guards before asynchronous sends.
- Directory traversal and manifest symlink escape are constrained by `safe_join` and link-target validation.
- Filesystem CAS paths canonicalize/check containment and verify content hashes under the adapter's exclusive-root ownership contract.
- MCP process wrapping uses direct argv execution rather than shell interpolation.
- MCP credential classification cannot silently downgrade to an ungated state.
- Node, Python, Go, and C aggregator wrappers delegate to the same Rust client; the aggregator defect is server-side rather than cross-language drift.

## Repair and acceptance order

1. **SEC-02:** pingwave state caps, cleanup lifecycle, adjacent-session authentication/rate limiting, and bounded forwarding.
2. **SEC-03:** transfer inspection and cancellation authorization.
3. **SEC-04:** strip privileged mode bits from remote directory metadata.
4. **SEC-05:** fail-closed credential-file permission and ACL enforcement.
5. **SEC-01:** decide the aggregation-operator trust boundary, measure bounded-template resource impact, and separately witness the authority of leaked replica seeds.
6. **SEC-07:** execute the congestion witness and repair if it survives.
7. **SEC-06:** sanitize secret-bearing parser diagnostics.

Each repair chain must have one HOLD at a time and include:

- a deterministic inverse RED against this exact audited commit or an isolated equivalent base;
- the smallest production repair;
- a positive witness proving the intended path still works;
- focused regression tests with nonzero counts;
- relevant full-suite and binding-parity gates;
- a clean worktree and `git diff --check`;
- exact-head CI before final acceptance.

Do not combine unrelated repair evidence into another active audit report. Create additive repair or review packets per subsystem so this source finding report remains frozen.

## Verification and limitations

The primary audit reported:

- a clean tracked worktree at the exact audited SHA;
- passing focused authority witnesses;
- a passing proximity cleanup unit test;
- a passing aggregator unregister/shutdown test;
- a passing transfer list/get/cancel state test.

Limitations:

- no complete fuzz corpus, Miri, sanitizers, Loom, or cross-language stress harness was run;
- Linux setuid and multi-user permission witnesses were not executable on the Windows audit host;
- Windows reparse-point and DACL behavior was reviewed structurally, not fully exercised dynamically;
- published npm/Python/Go/C artifacts were traced to core implementations but not all executed end to end;
- dependency advisory resolution and every non-default feature combination were not exhaustively reviewed;
- one broad Rust integration build encountered a Rust 1.97.1 internal compiler error, so focused nonzero test targets were used instead.

This report remains **HOLD** until every retained confirmed finding has a repair and exact-head acceptance packet, and SEC-07 has either been reproduced or closed with evidence.
