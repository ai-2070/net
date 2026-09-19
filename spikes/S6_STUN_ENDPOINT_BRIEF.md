# Stage 6 amendment — a separately announced STUN endpoint (Kyra, option 1)

Kyra read `S6_REPORT.md` §6.12.1 at `b689be08f` and verified natsim
35142933036 green at `cfd7aa44d` (status only; she did not replay the
packet-level diagnosis). Her ruling, verbatim in substance:

> Choose option 1: a separately announced STUN endpoint. Add fail-fast
> rejection of known endpoint collisions as a safeguard — not silent
> stripping. Reject documentation-only. … The receive-dispatch
> behaviour belongs to libwebrtc, but our product contract invites the
> incompatible configuration. Changing the harness closes the
> experiment; it does not fix that contract.

This is a **narrow plan amendment**: distinct advertised endpoints,
working defaults, explicit configuration failures. Not a second Net
node, not a mandatory third-party service, not a transport demux, not a
signalling redesign. The candidate-relabel theory stays closed.

## The design

Separate the two advertised roles:

- **`rtc_addr`** — the anchor's ICE/RTC endpoint. It **keeps answering
  the existing diagnostic STUN probe** (`UdpBlocked` evidence,
  unsolicited request from a throwaway socket).
- **A separately announced STUN endpoint** — used for gathering on
  connections that may pair with that anchor. It must be a **distinct
  externally reachable UDP endpoint**, not merely a different local
  bind that maps back to the same public socket. Two UDP ports on the
  same host in the same process is fine. **Announce the actual
  endpoint** (a new announcement field alongside `rtc_addr`, entering
  `SignedPayloadCanonical` the way Stage 4a's fields did, emission off
  unless configured); never teach clients to guess an adjacent port.

Net supplies a working configuration: an integrator should not have to
discover which external STUN service avoids its own anchor.

## Configuration behaviour (the safeguard)

For a **known collision on the particular `RTCPeerConnection` being
established** — an `iceServers` STUN entry whose endpoint equals the
peer's RTC endpoint — return a **typed configuration error before
waiting for ICE**, explaining that the configured STUN endpoint
conflicts with the peer's RTC endpoint.

- **Do not silently strip.** Stripping turns an explicit NAT-traversal
  configuration into a host-only attempt while appearing to accept the
  caller's settings.
- **Connection-specific.** An anchor can legitimately supply STUN to a
  separate browser ↔ browser connection where it is not the ICE peer;
  the check compares against *this* connection's peer only.
- **Detected equality only.** Distinguish detected endpoint equality
  from unresolved DNS aliases; do not promise exhaustive detection the
  implementation cannot provide — document the boundary.
- `stunUrl(rtcAddr)` remains legitimate for the throwaway diagnostic
  probe. What must disappear is the implication that converting
  `rtc_addr` into a STUN URL makes it suitable for that anchor's
  application connection — rename or split the helper so the two uses
  cannot be confused (`diagnosticStunUrl(rtcAddr)` vs the announced
  STUN endpoint for `iceServers`), and the leaf's default `iceServers`
  for the anchor connection comes from the announced STUN endpoint.

## Acceptance boundary (Kyra's, verbatim in substance)

1. The announced STUN endpoint is **reachable and distinct** from the
   announced RTC endpoint, **including the NAT mapping case** (the two
   public tuples differ after the gateway).
2. **Chromium and Firefox** establish the anchor connection and exchange
   application data using the **product-advertised configuration**,
   without a harness search for whichever configuration works.
3. The known conflicting configuration **fails promptly and
   descriptively** (typed error, before ICE).
4. The existing `UdpBlocked` probe still targets `rtc_addr`, with its
   classification unchanged.
5. The repaired NAT matrix passes **without changed topology
   expectations, longer deadlines, or a candidate-type-label success
   criterion**.

Two corrections from Kyra to carry into the witnesses:

- Because the diagnostic probe stays aimed at `rtc_addr`, adding the
  second STUN port does **not** require blocking that port to preserve
  the target-specific `UdpBlocked` witness. Block it too only when
  testing a broader all-UDP-blocked profile.
- The harness's camera/microphone grant is **not** permission to make
  media access a prerequisite for a data-only Net application. Keep
  that qualification explicit in the report; the product must work
  without it.

## Scope and rules

Product changes are in scope here: the announcement field (core,
canonical signer, the three tamper/absent/JSON witnesses as in 4a),
`RtcConfig` (`stun_addr` / `serve_stun` bind), the anchor's second
socket, the leaf's default `iceServers` and the collision check
(`leaf/`, `browser-ts/`), the SDK/CLI surface that announces it, Deck.
Stage 3 driver seams and 4a admission untouched except a named hook.
Consumer diff file by file; the export checker on CI's build (a new
config field may add a C symbol — say so and regenerate the baseline
in the same commit with the reason). Assertions never weakened, no
deadline widening, no retries. One commit per slice, prefix
`feat(net): stage 6 —` / `fix(net): stage 6 —`; `S6_REPORT.md` §6.12.2
with the acceptance table, raw inverse receipts, and the media-permission
qualification. Full pre-push checklist; report only on main CI **and**
natsim green at one head with both URLs. No Stage 7.
