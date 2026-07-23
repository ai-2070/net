# Net v0.33 — "Circus Maximus"

*Named after Rome's largest arena — where the whole city converged on one track to watch a single race resolve — and carried by two tracks that share the name: Travis Scott's Utopia-era "Circus Maximus," and JVZEL & Neon Haze's "Circus Minimus" off the Cyberpunk 2077 Night City Radio soundtrack, whose title plays the same trick in reverse. The metaphor does the release's work: many consumers converge on one rendezvous, ask one existential question, and coalesce into a single answer — the provider running the race once, no matter how large the crowd. Maximus in demand, minimus in the load it costs.*

Three tracks land, all in the network layer:

- **Capability sensing (interest coalescing)** — a mesh-wide answer to *"can **any** authorized provider currently satisfy capability Y under characteristics C and latency envelope L?"*, asked once per distinct interest instead of once per watcher. Consumers coalesce locally, then again at a rendezvous the mesh already had, so a provider evaluates once and signs one proof that fans back to everyone who resolved to it. Ships dark behind a flag — the whole plane is off unless you turn it on.
- **The rendezvous — and the review that made it safe** — the sensing plane went through a line-by-line review of its crypto, wire, and state-machine core and a deep-reader sweep of the +5.7k-line integration glue. Eight findings, all resolved, each Rust fix carrying a regression test; the one remotely-triggerable leader crash is closed at intake.
- **Real-time routing, hardened** — the v0.32 push-for-latency tracks get their RT-5/RT-3 follow-ups: pingwaves are de-duplicated before they can touch the routing table, a dead direct peer is withdrawn regardless of what the stale graph still shows, alternate-path promotion looks past the shortest path, and a change made between `start()` and `start_arc()` is no longer dropped.

The organizing observation is the mirror of last cycle's. Where v0.32 was **a fast path layered over a correctness path that never moves**, v0.33 is **one shared answer layered over an authority that never moves.** Sensing coalesces many consumers' identical question into one provider evaluation and distributes one signed proof — but every proof it distributes is *advisory*: soft state, origin-signed, converging by expiry, always deferring the final yes/no to the admission path below it. The provider stays the authority; final admission (the gang-claim / invocation recheck, targeted at the selected provider) stays the recheck. Nothing the sensing plane says is load-bearing on its own — a physical or safety-critical integration that treats advisory readiness as authorization to proceed is a bug on par with a wrong API signature. And it invents nothing new: the rendezvous is the RedEX election re-anchored, the routed half is the proven v3 readiness machinery, the wire is two frames. Two routing stages, no new coordinator, no NDN.

---

## Capability sensing — the existential question, asked once

The product-level question was never "is printer-7 online?" It is **"can *any* authorized provider currently satisfy capability Y under characteristics C and latency envelope L?"** — existential over the eligible provider set, not a health check on one preselected device. A node that needed this evaluated had three bad options: the capability fold (announce-cadence staleness, no per-(C, L) evaluation), direct probing (N·K·f load peaking at exactly the contention moment), or v3's provider-first sensing, which fragmented demand only when consumers happened to resolve differently and offered no capability-level surface at all. The **Sensing Interest Coalescing** work answers the existential question directly, and coalesces the demand behind it.

- **An interest is three orthogonal dimensions.** *What* — the capability predicate (Y, its canonical constraints C, and the provider-evaluated latency envelope L). *From where* — a `ProviderSelector`: `AnyAuthorized` (the default: a provider is itself the answer), an explicit `Node` / `Nodes` set, an owner-scoped `Group`, or exact-conjunction `Tags`. *How many* — a `ResultMode`: `Any`, `TopK`, `Each` (the un-flattened per-provider map), or `Quorum`. v3's entire model is the single `Node(X) + Each` cell of this matrix; the default `AnyAuthorized + Any` is the new existential primitive.
- **Two layers, cleanly split.** A **local capability sensing controller** owns interest identity, candidate resolution, bounded exploration, and the result-mode aggregate; the **routed provider-readiness protocol** — the proven v3 machinery — carries provider-targeted branches along `next_hop(provider)`, coalesces per-hop on `(provider, interest digest)`, and maintains hop-by-hop continuity. A provider evaluates once per distinct digest, signs an attestation, and identical signed proofs fan back down every interested branch.
- **The rendezvous already existed.** Open-population interests (`AnyAuthorized` / `Group` / `Tags`) have no `next_hop` of their own, so they are addressed to the current scope-local **sensing leader** by NodeId over ordinary Net routing. The leader is the existing RedEX deterministic, health-filtered election — re-anchored at a shared proximity-centrality key via a non-member observer, a *parameterization*, never a second election subsystem. The next-ranked healthy node wins on leader loss (the bully fallback); leader failover is soft-state re-registration, no synchronous state transfer required for correctness. The leader coalesces equivalent interests *before* provider selection, resolves bounded candidates once, opens the branches, and is the fan-out point — the provider remains the authority, each consumer remains the judge of its own path.
- **Coalescing on two honest surfaces.** All consumers on one node sharing an interest collapse to one `CapabilityInterestKey` (local coalescing); all consumers in a reachable scope that resolve to the same provider share one provider stream at fan-in (cross-node coalescing, *after* resolution). Provider sensing load therefore scales with **interested routing-tree branches × distinct interests, never raw watcher count** — the whole point. The divergent-resolution case, where two consumers resolve to different providers and don't coalesce, is a stated v1 limitation, pinned and *measured* (the SI-7 merge-miss rate), with an evidence-triggered future gate rather than a silent gap.
- **The latency budget is split, permanently.** A provider can sign "I can start within 300 ms"; it *cannot* know any given consumer's current path cost. So the provider-evaluated dimension (`work_latency`, L) is in the digest and signable, while the consumer's `end_to_end` budget is checked **locally** against the proximity plane's route estimates. Two consumers may legitimately derive different viability from the *same* signed proof — which is why the result-mode aggregate is local by definition. Relays distribute proofs, never verdicts; no relay ever globally resolves `Any`.
- **An honest continuity contract.** For each interest the consumer holds provider-signed *last attested statuses* under a requested continuity interval D, with optimistic `Ready` gated on *established* continuity, `NotReady` projecting immediately, and unknown/expired evidence projecting `Unknown` — the pinned, pessimism-safe projection table. The plane does **not** bound the age of any provider's evaluation (a named follow-up) and no attestation ever signs an end-to-end latency claim. Continuity never crosses a provider generation change; the attestation transcript is hand-rolled tamper-evident, with distinct derive-key domains for interest, constraints, and attestation, and a bounded-LRU equivocation seq-gate underneath.
- **First consumer: the gang-claim scheduler.** Sensed aggregate views join gang candidate pruning through the scheduler's projection seam (`match_islands_sensed` / `claim_island_sensed`), the claim still targeting the selected provider under its own authoritative admission recheck. The plane is advisory input to a decision it does not make.
- **Observable, and off by default.** An operator surface (`docs/SENSING.md`) exposes refusals-by-kind (including the broad-selector refusal), coalescing and delivery lifecycle counters, the divergent-resolution merge-miss rate, and a leader-load snapshot. The whole plane ships behind `enable_sensing_coalescing = false` and the leader role behind the `redex` build feature — a node that enables neither pays no wire, fold, or idle cost, and the two new frames (`0x0C02` `SensingInterestFrame`, `0x0C03` `ReadinessAttestation`) are never emitted.

---

## The rendezvous — and the review that made it safe

The sensing change is large — 57 files, +28.5k/−481, a new `behavior/sensing/` module tree and ~5.7k lines of `mesh.rs` integration — and it sits at exactly the seams an attacker wants: the wire→leader boundary and the freshly-written language bindings. It went through a focused review (`docs/misc/CODE_REVIEW_2026_07_15_SENSING_INTEREST.md`) that read the cryptographic core, wire codec, epoch/continuity state machines, and interest table **line by line**, and swept the integration glue with deep-readers, re-verifying every finding against source.

- **The core came back clean.** The injective, domain-separated digests; the tamper-evident attestation transcript (all twelve fields proven malleability-free by test); the size-capped `postcard` decode with no truncating casts or panics on peer bytes; the persist-then-participate boot ordering; the interest table's per-downstream independent expiry and refusal partitioning; and the entire `mesh.rs` integration — lock ordering consistent and never held across an `.await`, saturating time math on peer-controlled inputs, every map swept or reclaimed on branch death — all verified. The findings concentrated where predicted.
- **The one crash, closed at intake (High).** The wire gate bounded the sample interval and rejected a zero ttl but never bounded `soft_state_ttl` *above*, and — because that field is not part of `interest_digest` — it was entirely unvalidated. An authenticated peer inside the owner-root boundary could send one valid `CapabilityRegistration` with `soft_state_ttl = u64::MAX`; the leader reached `Instant + Duration` overflow and panicked. Every *local* registration path already capped it; the leader-relay leg was the one that skipped the guard. Fixed by clamping at intake to `sensing_interest_ttl`, mirroring the existing local guards — a robustness fix independent of the v1 trust assumption, since a trusted-but-buggy peer had the same effect.
- **Two silent-wrong-verdict bugs (Medium).** `Quorum(k)` with `k > maximum_fanout` was silently unsatisfiable — only `maximum_fanout` branches ever sensed, so the quorum could never be met, and unlike `Each` it raised no refusal; now it refuses like `Each`'s `SelectorTooBroad`. And a reconcile could leave a provider in *both* the active and standby sets, which a later `expand_to_standby` would then duplicate into a double-counted branch; active and standby are now kept disjoint on reconcile.
- **The Go binding, tightened (Medium + Low).** `WatchTools` snapshotted its returned baseline with a *separate, earlier* call than the substrate watch's own snapshot — a TOCTOU window in which a tool added or removed between the two would be invisible until it next changed (a permanently stale view, and a regression from the internally-consistent polling code it replaced). Fixed to a single snapshot. A sub-millisecond `WatchOptions.Interval` also truncated to `0` (interpreted as "no staleness ceiling"); it now rounds up to at least 1 ms.
- **Three low-severity edges.** A consumer left on a torn-down-only branch is re-registered onto a kept branch immediately instead of waiting for its ttl/2 soft-state refresh; an `Unestablished` continuity cell whose warm-start cadence exceeded its own interval is re-anchored against the basis its deadline actually used; and `ProviderSelector`'s derived `Eq`/`Hash` — which disagreed with the canonical digest identity, a latent footgun for external SDK code comparing specs structurally — is made canonical.

All eight are resolved on the branch. Each Rust fix ships a regression test, and the crate passes `cargo fmt`, the strict `--lib --bins` and `--all-targets` clippy gates, and the full sensing unit and integration suites. The two Go fixes are gofmt-clean and manually verified — the Go `.go` files aren't built in CI (only the Rust FFI shim is linted), a gap called out honestly rather than papered over.

---

## Real-time routing, hardened

v0.32 made propagation event-driven — change-driven announcements, event-triggered pingwaves, origin-scoped route withdrawal. This cycle's **pingwave-fixes** work ([#582](https://github.com/ai-2070/net/pull/582)) closes the RT-5 and RT-3 review items that surfaced against those tracks, all of them about not letting *stale* control traffic undo a *correct* decision.

- **De-duplicate before you mutate.** Pingwaves now pass through a `PingwaveAdmission` gate (`RejectedDuplicate` / `AcceptedNoForward` / `AcceptedAndForward`) *before* any proximity-graph or routing-table change; the receive path installs or refreshes routes only for accepted pingwaves, and `on_pingwave_from` stays a thin forward-only shim. A replayed pingwave can no longer reinstall a route that was just withdrawn.
- **Withdraw on death, regardless of the stale graph.** When the failure detector transitions a direct peer to `Failed`, the node now *always* floods a route withdrawal and removes the dead direct edge from the proximity graph — the old `has_graph_path_alternate` gate (which could suppress the withdrawal when a stale graph still showed a path) is gone, along with its tests.
- **Look past the shortest path.** Alternate promotion uses `ProximityGraph::path_to_excluding_first_hop`, so when the shortest path starts with the very peer being withdrawn, the mesh can still promote a longer *valid* route through a different neighbor instead of failing the reroute.
- **Damp the floods per recipient.** Route-withdraw floods key their damper by `(dest, exclude)` via `route_withdraw_damp_admit`, and cascades are counted under `MAX_INFLIGHT_ROUTE_WITHDRAW_CASCADES` by awaiting the flood in the cascade path — bounding fan-out without dropping a distinct withdrawal.
- **Capabilities ride the pingwave.** Merged capabilities are pushed into the graph (`ProximityGraph::set_local_capabilities`) so both origin and change-driven pingwaves piggyback the current capability hash/version, and — the RT-3 fix — the change-driven announce loop no longer drops a mutation made between `start()` and a later `start_arc()`: it resolves `self_weak` before consuming the signal and parks on a 200 ms poll until the Arc-startup is ready. A `net-node` serve-teardown race rode along too: outstanding node refs are now drained on shutdown, closing the window where a teardown could race an in-flight serve.

Each fix ships its test — duplicate rejection, alternate search, per-recipient damping, capability piggyback, an integration test proving withdrawals still propagate against a stale graph alternate, and a regression test that a mutation between `start()` and `start_arc()` still announces.

---

## The docs

The sensing plane ships with an operator-directed `docs/SENSING.md` covering the observability surface and the advisory-not-authoritative contract in the same load-bearing framing the plan makes central — a `Ready` from this plane is a materialized view to *act on subject to final admission*, never an authorization to skip it. The discovery guide's `watchTools` subscription — the streaming replacement for the old poll-until-appears loop, whose Go binding cutover v0.32 left deferred — lands with the sensing branch's `tool.watch` surface, the FFI `net_rpc_watch_tools`, and the Go `WatchTools` binding hardened by the review above.

---

## What's deferred (honestly)

- **Sensing ships dark.** `enable_sensing_coalescing = false` by default; v1 soaks behind the flag before any deployment leans on it, and the leader role additionally needs the `redex` build feature. A mesh that enables neither is byte-for-byte v0.32 on the wire.
- **Sensing SDK/FFI bindings.** Only the Rust substrate (and the `tool.watch` discovery surface it rode in with) land this cycle; the `SensingInterest` API is not yet exposed on the TS/Python/Go SDKs — a follow-up once the substrate soaks.
- **The divergent-resolution coalescing miss.** Cross-node coalescing happens only *after* resolution, so two consumers that resolve to different providers don't share a stream. This is a stated v1 limitation, measured by the SI-7 merge-miss rate, with an evidence-triggered future gate for rendezvous/reverse-announcement routing — not a bug, and not silently hidden.
- **No evidence-age bound.** The continuity contract delivers signed *last attested statuses* under a continuity interval; it does not bound the age of a provider's underlying evaluation, and no attestation signs an end-to-end latency claim. Strong-freshness guarantees are a named follow-up.
- **v1 authority is owner-root-only.** Cross-root authority propagation, arbitrary Boolean / compound selector expressions, constraint subsumption, CAS-backed large constraints, and signed-batch attestation optimizations are all out of scope for v1 by design.

---

## Breaking changes

v0.33 is **additive on the wire and on every existing transport, fold, reliability, and SDK path** — none of them changed shape. A downstream feels new surface and a version bump, not a behavior change to code it already ships.

- **New sensing wire frames, gated off:** `0x0C02` `SensingInterestFrame` and `0x0C03` `ReadinessAttestation`. Neither is ever emitted or dispatched unless `enable_sensing_coalescing = true`, so a mesh that leaves the flag off (the default) is unchanged; an un-upgraded peer that receives one under a mixed-version rollout degrades to the negotiated fallback rather than to anything wrong.
- **New sensing config surface, all defaulted to inert:** `enable_sensing_coalescing` (`false`), `sensing_interest_ttl` (30 s), `max_interests_per_peer` (512), `max_constraint_bytes` (1 KiB), `attestation_cadence_floor` (50 ms), `continuity_factor` (3), and the candidate-exploration bounds (`candidate_initial_fanout` 1, `candidate_standby_count` 1, `candidate_max_fanout` 3, `each_mode_max_providers` 32). A mesh that touches none of them behaves exactly as v0.32.
- **New build feature:** the sensing **leader** role is gated behind `redex`; a node that doesn't compile it can still register interests but never acts as a rendezvous center.
- **Hardened real-time routing behavior:** the pingwave-admission gate, the always-withdraw-on-failure change, the alternate-path search, and the per-recipient withdraw damper refine the v0.32 routing tracks — same wire, corrected control-plane decisions. A mesh already running v0.32 real-time routing gets strictly-safer route churn, no new surface to adopt.
- **New / changed binding surface:** the `tool.watch` streaming watch (`net_rpc_watch_tools` and the Go `WatchTools` cutover off its 1 s poll) closes v0.32's deferred remote-watch tail; the Go binding's baseline-snapshot and sub-ms-interval fixes ride with it.

---

## How to upgrade

1. **Pull the release** — nothing changes unless you turn on the new plane. Existing bus, stream, nRPC, payments, and persistence code behaves exactly as before, and the v0.32 real-time timers keep their current cadence as anti-entropy floors.
2. **To evaluate capability sensing**, build with the `redex` feature where you want a rendezvous leader and set `enable_sensing_coalescing = true`; register an interest with an `AnyAuthorized + Any` selector to ask the existential question, and read the result as **advisory** — always follow it with your own final admission recheck. Tune the ttl, cadence-floor, and candidate-exploration knobs only if a workload needs it.
3. **To get the routing hardening**, no action is required — pingwave de-duplication, always-withdraw-on-failure, alternate-path promotion, and per-recipient damping are on by default and degrade cleanly against un-upgraded peers.
4. **To move off the discovery poll loop**, adopt the `watchTools` streaming subscription (`tool.watch`); the Go `WatchTools` binding is now event-driven rather than polling on a 1 s tick.
5. **Everyone else** gets the new surfaces with no behavior change to existing paths.

---

## Dependency updates

The crate version bumps `0.32.0 → 0.33.0`, propagated across the CLI, deck, SDK, payments, and language-binding manifests. Like v0.32, this is a routine refresh — no first-party crypto or HTTP-client majors moved, and sensing introduces no new third-party dependency:

- **Rust crates (lockfile-level, no downstream API impact):** `xxhash-rust`, `napi`, `rustls`, `socket2`, and `toml` lockfile refreshes.
- **Docs / web (Next.js under `web/`), tooling and lockfile only, no runtime path:** `ws` 8.21.1, `fuse.js` 7.5.0, `posthog-js` / `posthog-node` / `redis` routine refreshes, and the `actions/setup-node` v7 CI bump.

---

Released 2026-07-15.

## License

See [LICENSE](../../LICENSE-APACHE).
