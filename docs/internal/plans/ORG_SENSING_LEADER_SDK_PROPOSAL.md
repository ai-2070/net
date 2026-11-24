# LS-7 proposal: an SDK surface for leader-backed organization sensing

**Status: PROPOSAL FOR AN OWNER DECISION (2026-11-24). Nothing here is
authorized for build.** This is the "separate public-API decision" that
[`ORG_SENSING_LEADER_SUBSTRATE_PLAN.md`](ORG_SENSING_LEADER_SUBSTRATE_PLAN.md)
§0 defers as LS-7. It builds on the leader track as it stands on PR #1471
(LS-1..LS-6 plus D-LS13, head `1f6278630`).

It asks five questions in §6. Everything before that is the evidence they
rest on.

---

## 1. Correcting the framing first: the leader does not widen what a consumer can see

The intuitive pitch for a provider-free selector is "sense providers you
cannot see yourself". **The leader track does not do that, by design**, and
an SDK surface must not imply it.

- **D-LS6** (`MeshNode::org_watch_admits_signer`, `mesh.rs`) accepts a
  leader-delivered proof only when its signer is a current owner-private
  provider of the capability in the CONSUMER'S OWN scoped store: unexpired,
  floor-current, under the consumer's own owner org.
- The three-node proof pins this. In
  `a_consumer_that_cannot_see_the_provider_refuses_its_proofs`
  (`tests/sensing_org_leader_three_node.rs`), the leader senses P and P's
  proofs reach A. A cannot see P, so it refuses every proof and never
  projects.

So the set of providers a leader-backed watch can ever report is the same
set the shipped exact-provider watch already reports: the consumer's own
verified owner-private discovery. What the leader changes is **who opens the
streams, and how many**.

| | Shipped exact-provider watch | Leader-backed watch (LS-7) |
|---|---|---|
| Streams opened by the consumer | One `ExactProvider` lease per provider, up to `MAX_SENSED_POPULATION` (32) | One registration, to the leader |
| Streams each provider serves | One per consumer watching it | One per leader, shared by every consumer of the same interest |
| Who chooses which providers are sensed | The consumer: all visible providers, lowest-id prefix of 32 | The leader: `ResultMode` (`Any`, `TopK(k)`) over its own projection, ranked by `(route_estimate, node_id)` from the LEADER's vantage |
| Population ceiling | 32 per watch | The leader's projection: no 32 cap on candidates, only on what is returned |
| Trust | Own discovery | Own discovery (D-LS6), plus the leader's |

The value of LS-7 is therefore **fan-in and fan-out reduction at scale**:
- many consumers of one capability stop multiplying provider streams;
- a consumer that wants one good provider stops sensing 32 of them.

It is not a reach feature. If neither of those costs is a real problem today,
the correct answer to §6 Q1 may be "not yet".

---

## 2. What is missing in the core before any SDK can sit on it

LS-6 lit the wire arm and the leader intake. Three core pieces an SDK needs
are still unbuilt. Each is verified against the tree at `1f6278630`.

1. **Nobody holds the leader role in production.**
   - `MeshNode::assume_sensing_leader` is called only from tests.
   - The pure election `sensing::rendezvous::sensing_leader(members,
     rtt_between, health_of)` exists, but nothing calls it outside its own
     unit tests. Its doc says election wiring is "a later slice".
   - Without it, every lit registration lands on a node with no role and is
     counted `NoLeader`.
   - An SDK watch needs the core to:
     - compute the org-scope leader from the live member projection plus
       the shared proximity view;
     - install or drop the role on the winner as membership moves;
     - tell consumers when the leader moved, so they re-register (failover
       is soft-state re-registration, plan §4.1).
2. **A provider-free lease has no refresh owner.**
   - `SensingLeaseKey::ProviderFree` exists in `behavior/sensing/lease.rs`.
   - The refresh path returns `SensingRefreshOutcome::Absent` for it
     ("Provider-free demand is not lit; it has no refresh owner here").
   - Release is local bookkeeping only.
   - `register_org_capability_interest` is a one-shot send that the caller
     must repeat at ttl/2.
   - The SDK's ownership contract (the watch owns its demand, and the last
     release deregisters) needs one of two homes:
     - **a node-owned ProviderFree refresh record**, the same worker that
       already renews exact-provider leases. Recommended: no task per watch;
     - or an SDK-owned timer.
3. **The consumer read needs the exact path's requalification.**
   - The per-interest rows already exist: `sensing_branch_projections(interest)`.
   - The shipped snapshot additionally requalifies on every read: the
     observer's own membership is live, and rows are clamped to current
     owner-private discovery.
   - It also applies the consumer's own route economics to the `within`
     budget.
   - A leader-backed snapshot must do the same, or it reports a provider
     the consumer stopped being entitled to see. D-LS6 guards admission,
     not reads after a floor raise.

Proposed sequencing: **LS-7a** (core: 1 + 2 + 3) ships and is reviewed
before **LS-7b** (Rust SDK), then **LS-7c** (bindings). This follows the
repo rule that a new capability lands in `net-mesh-sdk` first and the
bindings forward to it.

---

## 3. Constraints any shape must keep

These come from `CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md` and the
shipped SDK. They are not up for decision here.

- **No leader ids, audiences, digests or selectors in the public API.** "The
  SDK never accepts a caller-supplied leader for its common path" (§1.4).
  The audience is derived from installed authority, as in the shipped watch.
- **Refused, not faked.** A query form that is not implemented is refused
  loudly, never silently ignored. In particular, a leader-backed watch never
  silently falls back to exact-provider, and vice versa. The two answer
  different questions (§1's table), and the consumer docs promise "a
  caller is never silently observing a different question".
- **A snapshot is advisory.** It is not a reservation, admission or
  authority. Unchanged.
- **Error classification is owned once in the Rust SDK**
  (`SensingError::kind`, `classify_sensing_wire`). New refusals get one SDK
  kind each and flow to every binding through that classifier.
- **The guard test.**
  `the_public_surface_is_provider_lifecycle_plus_exact_consumer_observation`
  (`sdk/src/sensing.rs`) forbids leader vocabulary today. LS-7b changes it
  deliberately, in the same commit, to admit exactly the new names and
  nothing else.

---

## 4. Three API shapes

### A. No new API: the SDK switches paths internally

`watch(SensingQuery::new(cap))` keeps its signature. The SDK chooses the
leader path when a leader exists, or above some population.

- **For:** zero new public surface.
- **Against:** it breaks "never silently a different question". The same
  call would return 32 providers ranked by the consumer, or 1 provider
  ranked by the leader, depending on mesh state the caller cannot see.
  **Not recommended.**

### B. An explicit opt-in on the existing query (recommended)

```rust
let watch = mesh.sensing()?.watch(
    SensingQuery::new("nrpc:infer")
        .start_within(Duration::from_millis(500))
        .within(Duration::from_millis(800))
        // NEW: ask the organization's sensing leader to choose, and say how many.
        .coalesced(Coalesced::Best)          // ResultMode::Any
     // .coalesced(Coalesced::Top(3))        // ResultMode::TopK(3), k <= 32
)?;
```

- **New public names:** one builder method and one two-variant enum.
- **Unchanged:** `SensingWatch` / `SensingSnapshot` / `SensedProvider`.
- **Additions:**
  - a `SensingSnapshot::coalesced() -> bool`, so a reader can tell which
    question a snapshot answers;
  - new error kinds (§5).
- **What it maps to:**
  - `AnyAuthorized` selector and the owner-org audience, both fixed;
  - empty constraints, as today;
  - the ProviderFree lease from §2.2.
- **`Each` and `Quorum` are not offered:**
  - `Each` is guard-railed surveillance (plan §4.7) and adds nothing over
    the exact watch;
  - `Quorum` has no consumer-side meaning in a snapshot yet.
- **For:**
  - smallest surface;
  - the opt-in is visible at the call site;
  - every binding gets it as one extra option on an existing call.
- **Against:** one snapshot type now carries two provenances. The
  `coalesced()` flag mitigates this, but the docs must say clearly that
  ranking and the subset come from the leader.

### C. A separate watch type

`mesh.sensing()?.watch_coalesced(CoalescedQuery::new(cap).best())` returns
a `CoalescedWatch` / `CoalescedSnapshot`.

- **For:** the strongest semantic boundary; the two provenances can never
  be confused.
- **Against:**
  - it roughly doubles the consumer surface in four languages;
  - most of it would duplicate the shipped types field for field, and the
    copies would drift.

**Recommendation: B.** It keeps the call-site opt-in that A lacks, at a
fraction of C's surface. If review finds the two-provenance snapshot
confusing in practice, B can still be split into C later. Merging C back
into B would break callers.

---

## 5. Semantics B commits to

- **Population:** rows are what the leader returned. Each row is then
  clamped at read time to the consumer's own current owner-private
  discovery (§2.3). A row the leader chose but the consumer cannot see is
  absent, never `NotReady`.
- **Ranking:** `ranked()` / `preferred()` keep the leader's order among the
  surviving rows. The consumer's own `within` budget still filters on the
  consumer's route plus the provider's signed start estimate, exactly as
  today. Order is the leader's; viability is the consumer's.
- **Leader loss or move:** the watch stays open and re-registers at the new
  leader (§2.1). Until proofs arrive, rows are `Unknown`, the same
  "never a permanent verdict" meaning as today. No error and no fallback.
- **No leader at all** (an island with no eligible member, or the role is
  not wired): `snapshot()` returns a new typed `SensingError::NoSensingLeader`.
  The caller can then decide to use the exact watch. Proposed kind:
  `no_sensing_leader`.
- **Leader refusals** surface as typed errors: over capacity
  (`MAX_ORG_LEADER_INTERESTS` = 512), all branches refused, preflight
  refused. Proposed mapping:
  - `OverCap` → `WatchesAtCapacity`, reusing the existing kind;
  - `NotAdmitted` / `Refused` / `PreflightRefused` → one new
    `LeaderRefused { retryable }`.
  - The wire reason stays inside the classifier.
- **Authority:** observer qualification is identical to the exact watch.
  It is checked on every `snapshot()` and refused with the existing
  `ObserverNotQualified` / `NoOrganizationAuthority`.
- **Cleanup:** `close()` / `Drop` releases exactly this watch's
  ProviderFree lease. The last release stops refresh, and the leader's row
  expires by soft-state TTL. That is the existing leader contract, so no
  leader-side deregistration frame is needed, and the wire stays frozen.

---

## 6. Decisions for the owner

1. **Do we want LS-7 now?** §1 is the honest value: fan-in and fan-out
   reduction, not reach. If there is no consumer count or provider load
   where the exact watch's per-consumer streams hurt, deferring is
   reasonable. LS-7a (election wiring) is useful on its own either way,
   because it is the only thing that makes the lit arm reachable in
   production.
2. **Shape:** A, B or C (§4)? Recommended: B.
3. **Result modes:** `Best` + `Top(k ≤ 32)` only? Recommended: yes.
4. **No-leader behavior:** a typed error (recommended), or an empty
   `Unknown` snapshot?
5. **Binding rollout:** all of TypeScript, Python, Go and Node in LS-7c in
   one change with the shared cross-language fixtures (recommended, matching
   how the exact watch shipped), or Rust first and bindings later?

---

## 7. Phases (if authorized)

| Phase | Scope | Exit witnesses (named, RED-coupled as on the leader track) |
|---|---|---|
| LS-7a | Core: org-scope election wiring (role install/drop, leader-change wake); ProviderFree refresh owner on the node's existing refresh worker; read-time requalification for leader-delivered rows | A real-transport proof with NO `assume_sensing_leader` call; leader failover re-registers within one ttl/2; a floor raise drops a leader-delivered row on the next read |
| LS-7b | Rust SDK: `SensingQuery::coalesced`, `Coalesced`, `SensingSnapshot::coalesced`, new error kinds; guard test amended in the same commit | SDK watch end-to-end through a leader; close releases only its own lease; refusal kinds classified; no silent exact↔leader fallback |
| LS-7c | Node/Python bindings forward to the SDK; TS/Python/Go SDK option; cross-language error-kind fixtures | Per-language watch smoke tests; the shared classifier fixture; ABI stability (Go) |

The wire format does not change in any phase.
