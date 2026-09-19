# Stage 4a — announcement fields, `0x0D02` signalling, §12 admission contract (native half)

**Authorized by the product owner (2026-09-12), stacked on the Stage 3
repair head `97815f9d9` while Kyra reviews it.** Same rule as before,
now stricter because Stage 4 touches the **wire** and the **security
contract**: every commit is additive; the three announcement fields are
**off by default** (emission and, until Stage 5, consumption gated behind
`MeshNodeConfig::rtc`); `0x0D02` is dispatched only when `webrtc` is on;
admission state is installed only for RTC sessions. A Stage 3 re-review
fix must not conflict. Stage 4 is split: **4a (this brief)** is
everything testable with native nodes; **4b** (bootstrap listener, the
credential, TLS, Chromium) follows in its own brief.

Source of truth, in this order:

1. `docs/internal/plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md` §5
   (Layers 0–3, replacement not dual session), **§12 verbatim** (the
   admission contract: allow-list A–E, five enforcement points, no
   pre-enrollment transit, session-bound promotion, eligibility not
   authority, six witnesses), §10 (three-part direct-path witness), §11
   (registry), §Stage 4 scope + exit criteria.
2. `docs/internal/spikes/S0E_BOOTSTRAP_FRAMES.md` — §2 the allow-list
   with bounds, §3 incidental work (the **rate-limit-bypassing
   corrective re-announce**, `mesh_rpc.rs:5842–5870`, is removed from the
   provisional path), §4 the seven forwarding sites F1–F7 and the
   `dest_id == local_node_id` ordering, §5 renewal, §6 (the Subscribe
   retry shares one nonce; the nRPC envelope must be decoded before
   admission; heartbeat and pingwave split at `:27554`/`:27556`).
3. `S3_REPORT.md` §11 — the repaired driver/transport/installer surfaces
   you build on (`require_live_rtc_endpoint`, `rtc_upgrade_precheck`,
   the CAS + quiescence installer, `PeerRemoval` via reap).
4. `behavior/capability.rs` — `SignedPayloadCanonical` (~`:2402`), the
   `reflex_addr` pattern (`:2319`, `:2438`).
5. `AGENTS.md`; `docs/SUBPROTOCOLS.md`; `docs/CAPABILITIES_SCHEMA.md`.

## Decisions fixed for this stage

- **`0x0D02` is `SUBPROTOCOL_RTC_SIGNAL`.** `traversal/mod.rs:451` carries
  a comment-only reservation of `0x0D02` for port-mapping metadata that
  was never allocated and is absent from `docs/SUBPROTOCOLS.md`. Retire
  that comment, reserve **`0x0D03`** for port-mapping in the same place,
  and register `0x0D02` in `SUBPROTOCOLS.md`.
- **Announcement fields** `noise_pubkey: Option<[u8; 32]>`,
  `rtc_bootstrap: Option<String>`, `rtc_addr: Option<SocketAddr>` — the
  `reflex_addr` wire-compat treatment (`#[serde(default,
  skip_serializing_if = "Option::is_none")]`) **and** the matching
  `serialize_field` / `skip_field` pair in `SignedPayloadCanonical`, in
  declaration order after `owner_cert`. Emission: `noise_pubkey` only when
  `MeshNodeConfig::rtc.is_some()`; `rtc_bootstrap`/`rtc_addr` only when
  `serve_bootstrap` / `public_addr` are set. Native nodes without `rtc`
  emit nothing new — byte-identical announcements (witnessed).
- **Capability tags** `rtc-anchor` (set iff `serve_bootstrap`),
  `transport:rtc` (set iff `rtc.is_some()`), `leaf` (Stage 5 emits it;
  Stage 4 only *reads* it: a `leaf`-tagged peer is never a forwarding
  next-hop and never re-flooded to).
- **`RtcSignalMsg`** exactly as §5 Layer 3: `Offer | Answer | Candidate |
  Reject { dialog, reason }`, postcard, riding a session-authenticated
  Net packet; origin/target are the session endpoints. `dialog: u64`
  chosen by the offerer; a `Reject` or `ice_deadline` ends it. Per-sender
  budget: at most 4 concurrent dialogs and 64 signalling frames per
  10 s per peer; over-budget frames are dropped **with a counter**
  (`RtcStats::signal_over_budget`). Forwarding of `0x0D02` for a
  provisional peer is denied (§12, F1–F7).
- **Admission state** is a new `PeerAdmission { Provisional { since,
  budget }, Admitted { promoted_at, session_id } }` field on `PeerInfo`,
  **distinct from `PeerTransport`** (§12). Installed as `Provisional`
  for every session whose endpoint is `PeerAddr::Rtc` **and** whose
  responder is `serve_bootstrap`-enabled; UDP sessions and RTC sessions on
  a non-bootstrap node install as `Admitted` (native ↔ native and the
  Stage 3 harness are unchanged). Promotion happens **only** in the
  enrollment handler's success path, binding `(node_id, session_id,
  RtcPeerId)` captured at request decode — a delayed completion whose
  session was replaced promotes nothing (witness 4).
- **Enforcement points** (the five of §12), each a named function so the
  witnesses can name them: `admission_gate_forward` at F1–F7 (all seven
  from S0e §4, **after** F1's `dest_id == local_node_id` test),
  `admission_gate_route_install` at the routed-handshake install
  (`:25460–25477` region), `admission_gate_subscribe` in
  `handle_membership_message`, `admission_gate_announce` in the
  announcement ingest, `admission_gate_deliver` before application
  delivery. Each returns a typed refusal that is **counted**
  (`RtcStats::admission_refused_{forward,route,subscribe,announce,deliver}`)
  and never silently dropped.
- **Allow-list A–E** from S0e §2 verbatim, including its bounds
  (≤ 1 in-flight enrollment call, ≤ 4 REQUEST frames, body ≤ 16 KiB,
  ≤ 1 channel membership = the reply channel
  `net.mesh.enroll.replies.<origin:016x>`, ≤ `membership_max_attempts`
  Subscribe frames sharing one nonce, ≤ 2 streams, ≤ 64 KiB tracked,
  heartbeat permitted, pingwave denied both directions, provisional
  expiry 30 s, ≤ 256 inbound frames, ≤ 256 KiB inbound). The nRPC
  envelope is decoded under strict bounds **before** the admission
  decision (S0e §6) — that is `admission_gate_deliver`'s job for the
  enrollment REQUEST. Renewal (`net.mesh.renew`) is admitted-only.
- **Global bounds:** `RtcConfig::max_provisional` (default 64) and
  `max_bootstrap_bytes_in_flight`; breach → close and reclaim (§12
  step 5), counted.
- **Corrective re-announce** (`mesh_rpc.rs:5842–5870`): not fired when
  the rejecting peer is provisional. Witness: a refused Subscribe from a
  provisional peer triggers zero announcements.
- **Direct-path replacement** is the Stage 3 installer as repaired
  (CAS + quiescence + `require_live_rtc_endpoint`); §9's steps 1–6 for
  native ↔ native-with-`webrtc` pairs are wired here: discover
  `noise_pubkey` from the announcement → `connect_via(anchor, …)` →
  `0x0D02` over that session → ICE → direct install replacing the routed
  session → on direct loss, explicit interruption and routed
  reconnection (Stage 3's carried witness, now driven by real
  signalling instead of the in-process fixture). Retry policy on ICE
  failure: never per packet; on a network-change event or the periodic
  reclassify tick.
- **`PairAction::Ice`** is now also returned from the announcement's
  `transport:rtc` tag, not only from an installed RTC endpoint.

## Target

New: `adapter/net/rtc/signal.rs` (codec + dialog state + budget),
`adapter/net/rtc/admission.rs` (state, gates, allow-list, promotion),
`tests/rtc_signalling.rs`, `tests/rtc_admission.rs` (pinned by name;
`--features "webrtc fixtures"`). Touched, feature-gated: `mesh.rs`
(dispatch of `0x0D02`, the five gates at their sites, `PeerInfo`,
emission of the fields/tags), `behavior/capability.rs` (three fields in
struct **and** canonical signer), `behavior/broadcast.rs` (no change to
`0x0C04`), `mesh_rpc.rs` (corrective re-announce guard, enrollment
success → promotion), `traversal/classify.rs`, `traversal/mod.rs`
(comment), `sdk/src/mesh_enroll.rs` (promotion hook on the operator
side is a core-side call; SDK surface unchanged), `docs/SUBPROTOCOLS.md`,
`docs/CAPABILITIES_SCHEMA.md`, `tests/cross_lang_wire/` (announcement
fixtures with and without the fields), `ci.yml` (pin the two binaries,
names + counts).

**Frozen:** everything earlier stages froze; `0x0C04` scoped
announcements (org path is out of v1); the bootstrap HTTP/WS listener,
the browser credential, TLS, and anything with a browser (4b).

## Exit criteria (4a's share of §Stage 4 + §12)

Each one → a named test → pass, in the report's table:

1. **Canonical-signer witnesses:** tampering with each of the three
   fields invalidates the signature; all three absent ⇒ byte-identical
   signed bytes to the pre-field form (fixture from `01e4b0f20`);
   encoding remains JSON. Add to `cross_lang_wire`.
2. A native node without `rtc` emits a byte-identical announcement
   (golden compare against the pre-Stage-4 fixture).
3. Signalling between two nodes sharing no direct session is delivered
   via an intermediate anchor; the anchor never reads the SDP (assert
   on the anchor's decrypted-payload path never seeing `0x0D02`
   plaintext; it may classify by `subprotocol_id`).
4. §9 end to end, native: announcement → `connect_via` → `0x0D02` → ICE
   → direct install replacing routed → forced direct loss → explicit
   interruption → routed reconnection. Three-part direct-path witness
   per §10 (positive receipt on the direct endpoint; per-pair
   application-data forward counter flat on the anchor; forced-relay
   inverse increments it).
5. Over-budget signalling dropped with the counter; a `Reject` ends the
   dialog; `ice_deadline` ends it with `RtcError::IceTimeout`.
6. **The six §12 witnesses** (native anchor with `serve_bootstrap`,
   native "browser stand-in" client over the Stage 3 loopback RTC
   path — no real browser in 4a):
   1. a PSK holder completes the permitted enrollment exchange and is
      promoted;
   2. the same provisional session is refused: announcement ingest,
      unrelated channel Subscribe, another nRPC service, forwarding a
      routed envelope to a third node, `0x0D02` — each at its named gate,
      each counted;
   3. a routed envelope addressed to the anchor itself is delivered
      locally; the same envelope with a third-party `dest_id` is refused
      at F1 **after** the local-delivery test (`connect_via` with
      `dest_node_id ≠ anchor` is the attack S0e named);
   4. promotion binds the exact live session: replace the session
      between REQUEST decode and RESPONSE → nothing promoted, the
      replacement stays provisional;
   5. `max_provisional` and the byte bound hold: the (N+1)th provisional
      connection is closed and reclaimed, counted;
   6. an enrolled (Admitted) peer without provider authority is still
      denied a protected invocation (existing org gate, unchanged —
      prove it is *reached*, not bypassed).
7. Corrective re-announce never fires for a provisional rejecter.
8. Pingwave from/to a provisional peer dropped and counted; heartbeat
   passes.
9. Stage 3's four RTC binaries, the default `--lib` count + six floors,
   and the export checker are unchanged.

Inverses, each applied-red-reverted in the report: remove one gate call
(each of the five) → its witness fails; remove the session binding from
promotion → witness 4 fails; remove the `serialize_field` for one new
field → canonical witness fails; restore the corrective re-announce →
witness 7 fails.

## Constraints

- Additive, feature-gated, emission off unless configured. No listener,
  no credential, no TLS, no browser, no `0x0C04`, no Stage 5.
- No new guards across awaits (the gates are synchronous reads of
  `PeerInfo`).
- Do not touch plan documents or earlier reports.
- Commits on `LZL0/webrtc-transport`, prefix `feat(net): stage 4a —`:
  (1) announcement fields + canonical signer + fixtures; (2) `0x0D02`
  codec/dispatch/budget; (3) admission state + the five gates +
  allow-list; (4) promotion + corrective-re-announce guard + bounds;
  (5) §9 wiring + the three-part witness; (6) tests + CI pins; (7)
  candidate: fmt, validation, `docs/internal/spikes/S4A_REPORT.md`.
- Validation: the Stage 3 list plus the two new binaries under
  `--no-tests=fail --retries 0`, every FFI member's clippy, default-
  feature `cargo doc` for both crates, the export checker on a fresh
  cdylib, and the exit-criterion table with inverses.

Reply with the candidate hash, the exit-criterion table, the inverse
results, and the validation list. Then stop — **no 4b, no Stage 5.**
