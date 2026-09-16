# Stage 6 §6.12 — the Chromium NAT rows: CLOSED (2026-09-16)

**Status: closed.** All six Chromium rows and the Firefox control pass
in natsim run
[35142933036](https://github.com/ai-2070/net/actions/runs/35142933036)
— 13/13 scenarios. The full account, with the source rule and the
symptom-by-symptom mapping, is `docs/internal/spikes/S6_REPORT.md`
§6.12. This file is kept for the dispatch it came from and for what its
own diagnosis got wrong, which is worth more than the part it got
right.

## What actually happened

Two defects, in series. Neither in the product.

1. **Interface enumeration was off** — Chromium gates it on media
   permission; `FilteringNetworkManager` withheld the network list, the
   allocator logged `Allocate ports on any any`, ports bound the `any`
   address, and inbound datagrams were dropped before STUN parsing.
   Fixed by granting camera+microphone for the page's own origin
   (`c17a1ee32`, `driver.mjs`). **This is the defect the note below
   describes**, and it was real.
2. **The harness named the anchor as the page's STUN server** —
   `stun:10.99.0.10:7100`, while the anchor's ICE host candidate *is*
   10.99.0.10:7100, because the product answers STUN on the RTC socket
   by design. `UDPPort::OnReadPacket` (`webrtc/p2p/base/stun_port.cc`)
   returns **before** `GetConnection` for any packet whose source is a
   configured STUN server, so every check response and every Binding
   Request between the browser and the anchor was consumed by the
   gathering path. Fixed by putting the STUN responder on a separate
   host, 10.99.0.11 (`run_scenario.sh --stun-ip`).

Defect 2 was invisible until defect 1 was fixed, and the note was
written between the two.

## Where this note was wrong, and why that matters

- **Its mechanism was already stale when it was written.** The note
  cites runs 35123516381 and 35126509744. The permission grant landed
  in `c17a1ee32` two runs later, and by run **35138824048** — the run
  immediately before this note's dispatch — the renderer logged
  `Allocate ports on eth0`, `Count of networks: 1`,
  `Net[eth0:192.168.102.x/24:Ethernet:id=1]`, `permission status:
  granted`, a gathered srflx and `PRECONDITION networks real=629
  wildcard=0`. Enumeration was **on**, and the rows still failed. The
  note's premise was therefore falsified before either of its two
  proposed changes could be tried.
- **The evidence of that was hidden by an instrument, not by the
  engine.** The `Chromium's ICE accounting` step piped
  `grep … | head -200` under `bash -e` with `pipefail`: `head` closed
  the pipe, `grep` died of SIGPIPE, and the step exited 2 after
  printing one row's lines. Five rows' logs were never opened and the
  step's own summary never printed. Two sessions read that fragment as
  the whole matrix. Repaired with `grep -m` (`cfd7aa44d`).
- **`network.cc` was never going to appear.** The note requires
  `network.cc` lines as its success signal. Chromium does not use
  libwebrtc's `BasicNetworkManager` at all: the network service
  enumerates and the renderer wraps the list in `IpcNetworkManager`
  behind `FilteringNetworkManager`
  (`peer_connection_dependency_factory.cc`). Absence of `network.cc` is
  not evidence either way — an operative criterion that could not be
  satisfied by a working run.
- **Its two proposed changes were therefore not made.** (1) The
  8.8.8.8 route: every browser namespace already has
  `default via <gateway>` from `setup.sh`, and the network service's
  own successful enumeration proves the probe resolved. `driver.mjs`
  now logs `ip route get 8.8.8.8` from inside each namespace so that
  precondition is a fact in the log rather than an inference, but a
  redundant `/32` route was not added. (2)
  `--webrtc-ip-handling-policy=default`: the effective policy was
  already `default` — the switch's whole purpose (enumeration on) was
  observably met, and the renderer's own
  `WebRTC routing preferences: policy: …` line is now kept as the
  record instead of scraping `chrome://policy`, which shows *configured*
  enterprise policy and renders it inside a shadow root. Note also that
  the content-level switch is spelled
  `--force-webrtc-ip-handling-policy`
  (`content/public/common/content_switches.cc`); the name in the note
  is the chrome-layer switch.

## Harmless, not causal

The multicast route and both default routes stay, recorded as such in
`setup.sh`: each removes a real error from the log or a real failure
mode from the topology, and neither was what ailed these rows. mDNS
obfuscation is dead twice over — disabling it changed nothing.

## The transferable lesson

The loopback harness had been running the controlled experiment for
this the whole time and nobody read it as one: its `[mdns]` sweep
varies exactly one thing — whether the page's `iceServers` names the
anchor's own socket — and Chromium forms a pair only when it does not
(ci.yml run 35137453461). The sweep then *silently adopts the working
configuration*, which is how "Chromium works on loopback and fails
behind the NAT" stayed true for a reason that had nothing to do with
the NAT. **A harness that searches for a working configuration hides
the defect it searched around.** That fallback should state what it
had to avoid, loudly, or not exist.
