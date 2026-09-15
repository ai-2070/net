# Stage 6 — Browser ↔ browser direct, NAT conformance, telemetry, demo

Plan: `docs/internal/plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md` —
Stage 6 (scope + exit criteria), §9 (browser ↔ browser is the §5
sequence with the mesh as the signalling network), §10 (the three-part
direct-path witness), §3 (the pair-type matrix), the Stage 5 report's
named Stage 6 hand-offs (`S5_REPORT.md` §8 gap 6: four
`#[wasm_bindgen]` methods `peer_offer`, `peer_accept_offer`,
`peer_candidate`, `peer_handshake` — and `peer_handshake` takes a peer
id **and nothing else**, because a page that can supply a Noise key
can supply any key), and `NAT_TRAVERSAL_V2_PLAN.md` Stage 4 (the
natsim harness). Prerequisite checked: `natsim.yml` is green on this
branch at `c40357404` and later.

Authorization: the product owner authorizes Stage 6 implementation on
`LZL0/webrtc-transport`, stacked on the Stage 5 fourth-round head
while Kyra reviews it. Additive: new page-facing leaf methods, harness
scenarios, telemetry surface, demo; **no changes to the Stage 5 leaf
protocol, the mode boundary, or any Stage 3/4 core seam** except
named fixture hooks. Stage 5's open owner questions stay open; do not
resolve them by implication. One commit per slice, prefix
`feat(net): stage 6 —`. Report `docs/internal/spikes/S6_REPORT.md`:
exit table, matrix results per row, inverse ledger with raw receipts,
named gaps. Plan docs are not yours.

## 1. Browser ↔ browser from a page (§9)

The four `#[wasm_bindgen]` methods above, over the Stage 5
`ControlPlane` (`signal(peer, envelope)` with the D1 signed envelope —
the leaf verifies every inbound envelope). Flow, from two isolated
tabs (separate origins or separate contexts so identity and lock state
are isolated): A discovers B by capability query → learns B's entity
and Noise keys from B's signed announcement → offers via the anchor
control plane → ICE (real, cross-context, real host/prflx candidates)
→ DataChannel → Noise in the offerer's role, keys from discovery only
→ a direct session installed on both sides → the routed session
replaced (the Stage 3/4a replacement fences apply unchanged). The
page-facing drive loop lives in `@net-mesh/browser`
(`connectPeer(nodeId)` returning a typed result), with typed failures
(`IceTimeout`, `NoAnnouncement`, `HandshakeFailed`, `Superseded`).

## 2. §10 three-part witness, browser ↔ browser

In the Playwright runner: (1) routed delivery A → anchor → B with the
anchor's per-pair application-data counter moving; (2) after the
direct session, application traffic flows while that counter stays
**flat**, and signalling/announcements continue (their counters move);
(3) the inverse leg — force the direct session down (close the
DataChannel from the page) and observe the counter move again on
manual routed restoration. Both engines. Assertions on the anchor's
live counters, nonce-correlated payloads at the receiver — the Stage
4b/5 evidence discipline.

## 3. Network-change retry trigger and `RtcStats`

`navigator.onLine` / `online` events and an ICE `disconnected` →
`failed` transition drive a bounded re-attempt through the same
production owner (Stage 4a's `spawn_dialog_completion` semantics on
the leaf side: one absolute deadline, retire before install). Witness:
Playwright's `context.setOffline(true/false)` around an established
direct session → typed interruption, one retry, restoration; counters
show one additional `ice_attempted`. `RtcStats` exposed on the leaf
(`node.rtcStats()`), same field names as native.

## 4. Deterministic NAT conformance (the natsim extension)

Extend `tests/natsim/` with two headless browsers behind simulated NATs
and one anchor: rows cone×cone, cone×port-restricted,
port-restricted×port-restricted, cone×symmetric,
port-restricted×symmetric, symmetric×symmetric. Expected: every row ICE
solves lands **direct**; symmetric×symmetric lands **relayed**
(routed via the anchor, typed as such); and the identity
`ice_direct + ice_relayed + ice_failed + udp_blocked == ice_attempted`
holds on every row. Chromium in the netns (Playwright's Chromium with
the CA trust from 4b; the runner already has the Firefox NSS path — use
Chromium for the matrix, Firefox for one row as a control). The plan's
warning stands: if the simulator breaks under the browser rows, that
is repair work — report it, do not paper over it with a loopback
substitute.

## 5. Field telemetry

`ice_direct / ice_attempted` through the existing stats surface and
Deck, documented as a deployment metric **with its own denominator**
(attempts, not sessions). Deck column with the ratio; CLI `net-mesh
anchor stats`.

## 6. Demo

A 60 Hz three.js position-update demo between two tabs
(`web/` or `examples/browser-demo/` — follow the repo's convention for
examples): positions over a fire-and-forget stream, the anchor's
per-pair forwarding counter displayed live and **flat once direct**
while signalling and announcements continue. A Playwright test drives
it headless and asserts the counter shape; the demo is also runnable by
hand with one command.

## Exit criteria (plan, verbatim)

- Every matrix row ICE is expected to solve lands direct; symmetric ×
  symmetric lands relayed; the counter identity holds; 100 % of
  sessions established over rows where both anchors are reachable.
- The demo shows the counter flat once direct while signalling and
  announcements continue.

## Validation

Both engines green in the merged runner with the new witnesses pinned
by name (rosters generated from source, self-checked); the natsim
browser matrix green in `natsim.yml` with the run URL; every witness
inverse applied-red-reverted with raw receipts; Stage 5's 42 reviewer
probes and all prior RTC/listener/leaf suites unchanged and green;
narrow matrix; bindings; `--lib` floors; export checker (telemetry
fields may add C constants — say so; symbols should not move);
consumer diff file by file. Full pre-push checklist every push; report
only on all-green exact-head CI. No Stage 7.
