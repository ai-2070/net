# NAT Traversal V3 Notes

Working notes on where NAT traversal should go after [`NAT_TRAVERSAL_V2_PLAN.md`](NAT_TRAVERSAL_V2_PLAN.md). **Not a plan** — no stages, estimates, or commitments. It records a ranked list of improvements and the code-level facts behind each, checked against the tree on 2026-09-25, so a future plan starts from verified ground rather than re-deriving it.

> **Framing.** Unchanged from the parent plans: NAT traversal is a **latency / throughput optimization**, not a correctness requirement. The routed-handshake path (and, since R2, the blind relay) is the correctness guarantee. Nothing below changes the fallback contract.

Line references are current as of writing; they will drift.

## Where things stand

V2 stages 1–5 are done; stage 6 (surface completion) is deferred. Since V2, the blind UDP relay landed (`traversal/blind_relay.rs`: stateless-challenge registration, TCP splice for enrollment, TCP/443 last-resort tunnel), plus direct-first/relay-fallback enrollment and the natsim rows that witness it. `natsim.yml` runs on traversal-touching PRs and nightly with a 16-scenario floor.

`auto_direct_upgrade` defaults to `true` (v0.34), but the background upgrade only acts on `Direct` pairs — see item 1. In practice that means the default-on upgrade mostly helps peers that were already reachable at their reflex.

---

## 1. Background upgrade for `SinglePunch` pairs — gated on end-to-end `punch_id`

**The single largest gap.** Pair matrix (`traversal/classify.rs:171`): Cone×Cone, Cone×Symmetric, Symmetric×Open, Cone×Unknown and Unknown×Cone all resolve to `SinglePunch`. That is the common home/office population, and none of it is upgraded automatically — it stays on the relay unless the application calls `connect_direct` itself.

### Where the block actually is

Not in the loop predicates. `upgrade_is_loop_candidate_at` (`mesh.rs:49296`) never consults the pair action, and `upgrade_initiates_for` (`mesh.rs:49321`) consults it only inside the `higher_claims` special case. The scan loop already visits `SinglePunch` pairs; they are turned away inside `attempt_direct_upgrade` (`mesh.rs:49106`):

```rust
// `SinglePunch` upgrades aren't wired yet, but the pair may
// become `Direct` after a reclassification. Defer instead
// of marking terminal so the scan revisits it.
PairAction::SinglePunch => {
    self.upgrade_record_defer(peer_id, upgrade_jitter(..., SINGLEPUNCH_RECHECK));
    return;
}
```

`SINGLEPUNCH_RECHECK` is 30 s (`mesh.rs:49112`), so every such pair is re-evaluated — and re-deferred — twice a minute forever.

Past that arm, the attempt dials the peer's reflex directly with `connect_via_cas(target_addr, …, direct: true)` (`mesh.rs:49245`). That is correct for `Direct` and useless for `SinglePunch`: two NATed peers cannot reach each other without the coordinated simultaneous open. **Simply admitting `SinglePunch` further down would not upgrade anything.**

### What the arm needs

The same sequence `connect_direct`'s `SinglePunch` arm already runs (`mesh.rs:46747`):

1. Pick a coordinator — the V2 stage-3a auto-selection (routing next-hop → relay-capable → any mutual peer) already exists.
2. `request_punch` and await the `PunchIntroduce`.
3. Keep-alive train at the introduced reflex.
4. `connect_via_cas` install (C2 CAS; C3 busy gate and C5 failure atomicity apply unchanged).

Plus one design decision: **which end initiates a punch-based upgrade.** The current C1 rule (lower id initiates, with the `higher_claims` exception for a `Direct` pair where only the lower end announced) was written for dial-the-reflex. The V2 stage-4 notes record that a relay session whose lower-id end is the unreachable-direction peer never upgrades; a punch-aware initiator rule — not the predicates — is what dissolves that. Both ends evaluate the same announcements, so any deterministic rule keeps the "exactly one initiator" property, and the CAS settles in-flight races.

Effort: medium, not small.

### Why `punch_id` goes first

Today `connect_direct` and the upgrade loop can't both punch the same peer, because the loop never punches. Once it does, overlapping punches toward one target become ordinary, and correlation by `(coordinator, peer)` tuple can't tell them apart.

State of `punch_id` — half-wired, not absent:

- A generator exists: `next_punch_id` (`mesh.rs:12832`, initialised to 1 at `:14820`), used on the request path (`:50537`).
- Reject correlation checks it (`mesh.rs:31233`: `*pid == rej.punch_id`).
- Introduces and acks still hardcode `0` (`mesh.rs:41859` "reserved; no generator wiring yet", `:41956`, `:51167`, `:63827`, `:63852`). The comment at `:31233` says introduces deliberately stay id-free ("conflation is benign there") and defers full ack/introduce correlation to "stage-6 punch_id work".

The "benign conflation" argument holds for one caller per target; it should be re-examined once two independent callers (upgrade loop + `connect_direct`) can hold pending punches to the same peer. Carry the request's id through coordinator → introduce → ack, and tighten the waiter match.

### Verification

- Loopback matrix test: a relay-routed Cone×Cone pair upgrades with no caller orchestration; `upgrades_attempted`/`upgrades_succeeded` move, `punches_attempted` moves.
- New natsim scenario: cone×cone relay session → direct upgrade (reuse the existing `natsim_relay_session_upgrades_to_direct` topology with both sides behind the cone gateway).
- Symmetric×cone upgrade attempts exactly once per backoff window (mirror of the existing exactly-once `connect_direct` test).
- Concurrent `connect_direct` + background upgrade to the same peer: each waiter gets its own answer.

---

## 2. Per-NAT-class telemetry

`TraversalStatsSnapshot` (`traversal/mod.rs`) has 13 fields of punch/upgrade/port-mapping outcomes, but no dimension by NAT class. Every "revisit when telemetry shows…" gate in V2 is therefore unfalsifiable:

- the port-prediction non-goal ("revisit only if stage-5 telemetry shows a meaningful symmetric population with punch demand");
- the responder-budget defaults open question (4 trains / 10 s / 8 concurrent);
- the permanently-busy-session open question.

Candidates (local counters; `nat_class()` and `pair_action_for` are already computed at the decision points):

- relayed sessions by `(local class, remote class)` or by `PairAction`;
- `SkipPunch` decisions, split Symmetric×Symmetric vs Symmetric×Unknown (the latter is often a startup transient, not a real population);
- **time-in-deferral for the C3 busy gate.** `upgrades_deferred_busy` is monotonic and counts deferral *events*, so one session deferred a thousand times reads the same as a thousand sessions deferred once. What's needed is a gauge of sessions currently deferred for longer than some threshold — that's the number that decides whether same-session path migration (item 5) is worth building.

ABI note: the FFI v1 3-out-param stats call must stay stable. Either keep the new counters out of `NetTraversalStatsV2` (Rust SDK / tracing / metrics only) or add them deliberately with a v3 struct. Don't grow v2 in place.

---

## 3. Port-mapping install should announce

A successful mapping publishes `NatClass::Open` + the external address (`PortMapperTask::apply_install`, `traversal/portmap/mod.rs:288`). But unlike `MeshNode::set_reflex_override` (`mesh.rs:50270`), which resets the announce rate-limit floor via `invalidate_broadcast_window()` so the caller's next announce is guaranteed to broadcast, `apply_install` neither invalidates the window nor triggers an announce. Peers learn the new class only at the next periodic re-announce. Not a correctness bug — a latency one, and cheap to fix. `apply_renewal`'s address-change branch (router reboot / WAN flap) has the same shape.

Also worth documenting, because it's easy to overstate:

- Port mapping is opt-in: `MeshNodeConfig::try_port_mapping` defaults to `false` (`mesh.rs:3576`). Defensible — it's router control — but the gain is invisible until enabled.
- A mapping turns a mapping-capable **symmetric** NAT into `Open`. That helps symmetric×symmetric only if **both** ends map: Open×Open is `Direct`, but Open×Symmetric is `SinglePunch` — which, until item 1 lands, the background upgrade doesn't act on. So one-sided port mapping pays off only after item 1.
- CGNAT and enterprise NATs generally won't honor UPnP/NAT-PMP/PCP; those pairs stay `SkipPunch` → relay. Correct behavior.

PCP is already implemented — `portmap/natpmp.rs` is a combined NAT-PMP/PCP codec (version-2 header at `natpmp.rs:867`). The V2 non-goal "PCP (RFC 6887) still out of scope" is stale.

---

## 4. Documentation / status hygiene

- `traversal/mod.rs:41` staging table still lists stage 4b (UPnP / NAT-PMP / PCP) as "deferred (needs `igd-next` + `rust-natpmp` deps…)". It ships (`portmap/{upnp,natpmp,sequential,gateway}.rs`), in-repo, without those crates.
- `NAT_TRAVERSAL_V2_PLAN.md` implementation-status table: stage 4 still reads "landed, pending first CI run". natsim is live and floored at 16 scenarios. Its non-goals still exclude PCP (see item 3).
- `include/net.go.h` vs hand-maintained `go/net.h` drift (flagged in V2 stage-5 notes) — a single-source header generator, if/when the ABI is next touched.

---

## 5. Data-gated — revisit once item 2 has numbers

- **Same-session path migration (QUIC-style).** The escape hatch for sessions the C3 busy gate defers indefinitely (long-lived streams, e.g. an inference feed). V2 decision 9's rejected alternative; the natural V3 headline if the time-in-deferral gauge shows a real population. Dual-session drain is the lighter alternative.
- **Multi-candidate announcements for IPv6 and LAN.** As far as the announcement shape goes, a node advertises a single reflex. Advertising several candidates (LAN host, global IPv6, port-mapped, reflex) and racing them — ICE-lite / happy-eyeballs — would give direct paths to same-LAN peers and to IPv6 peers, which usually need only a simultaneous firewall open, not NAT prediction. The deferred V2 dual-stack and NAT64/464XLAT natsim rows belong with this work.
- **Interface-change re-classification.** Wi-Fi ↔ cellular switches currently wait for the reflex-diff check at the next re-announce (`reclassify_if_reflex_drifted`, which is also skipped while an override is active). Needs netlink / NLM / `ConnectivityManager` plumbing per platform.

---

## 6. Deliberate non-goal: port prediction for symmetric×symmetric

Keep deferred, but record the technique and cost so the decision is auditable rather than an absence.

- **Technique.** Both ends open N local ports and fire from each; the coordinator relays the observed external ports; each side predicts the peer's allocation window (only viable for sequential or port-preserving symmetric NATs — random-allocating ones are out of reach) and cross-fires. Hit probability ≈ N·P / 65536 against a P-port predicted window.
- **Prerequisite.** The 4-class classifier (`Open | Cone | Symmetric | Unknown`) cannot tell sequential from random allocation, so a finer classification probe comes first.
- **Cost and risk.** The traffic is reflection-shaped toward a third party. It must ride V2 decision 2's rendezvous budgets and be restricted to authenticated session peers, like the existing keep-alive train. High abuse surface for a population that, since R2, already has a working relay path.
- **Revisit trigger.** Item 2 shows a meaningful Symmetric×Symmetric population (CGNAT-heavy deployments) with sustained relay load.

---

## Not in scope here

Stage-6 surface tail (sdk-ts / sdk-py NAT wrappers, CLI `peer nat` / `peer reflex` / `port` verbs) — still worth doing, but it doesn't make traversal succeed more often. Tracked in V2 stage 6.
