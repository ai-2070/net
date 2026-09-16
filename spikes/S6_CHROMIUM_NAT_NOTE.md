# Stage 6 §6.12 — the Chromium NAT rows, diagnosed (reviewer, 2026-09-16)

Status: the six Chromium natsim rows fail at browser ↔ anchor ICE while
the Firefox row passes. Every layer above the socket has been measured
and is correct; the cause is below STUN and is now named from Chromium's
own renderer log. Not a product defect. The Stage 6 agent's session
ended before this landed; it is the next dispatch.

## What is established (do not re-check)

From `browser_b.pcap` of run 35118618599, parsed by the reviewer
(SLL2, Python): anchor → Chromium 7 Binding Requests, USERNAME
`offer-ufrag:answer-ufrag` exact, MESSAGE-INTEGRITY valid under
Chromium's offered `ice-pwd`, ICE-CONTROLLED set, backoff 16 ms →
9.8 s; **Chromium answered none**. Chromium → anchor 193 requests
(USE-CANDIDATE + ICE-CONTROLLING), 193 responses, integrity valid under
the anchor's `ice-pwd`, response source == request destination,
XOR-MAPPED == Chromium's srflx; **Chromium credited none**. No DTLS.
Credentials, bytes, addresses, role, nomination, mDNS: all dead.

From the renderer log of runs 35123516381 and 35126509744
(`[chromium-ice]` in the job log):

- `basic_port_allocator.cc:802  Allocate ports on any any`
- every `Port[…]` is on `Net[any:0.0.0.x/0:Wildcard:id=0]`, cost 999
- **no `network.cc` line at all** with `--vmodule=network=3`
- 47/43/42 × `connection.cc:1019/1118/1648` (ping sent) and **zero
  receive-side lines** — no `OnReadPacket`, no request handling, no
  response handling — while the pcap shows the packets landing on the
  socket.

## Mechanism

"Allocate ports on any any" is libwebrtc's path for
`PORTALLOCATOR_DISABLE_ADAPTER_ENUMERATION`: the interface enumerator is
never consulted (hence no `network.cc` output), ports bind the `any`
address, the host candidate can only be named as `.local`, and inbound
datagrams on the any-address port are matched against the **default
local address** libwebrtc learns by `connect()`ing a throwaway UDP
socket to a public IP. Inside the browser netns that probe fails
(`ip -n nsim_b route get 8.8.8.8` is the exact test), so the port has
no default local address and every inbound datagram is dropped before
STUN parsing — in both directions, which is the one symptom no
STUN-level rule explains. Firefox's nICEr reads sockets directly and has
no such mode.

Why enumeration is disabled: Chromium sets it under the WebRTC
IP-handling policy `default_public_interface_only` /
`disable_non_proxied_udp` — from the `WebRtcIPHandling` enterprise
policy, the `webrtc.ip_handling_policy` pref, or the
`--webrtc-ip-handling-policy` switch. The headed CDP launch the driver
now owns may be picking up a policy file in the profile directory or
under `/etc/chromium/policies`. The gateway default route added in
`43c994a66` could not affect this (right layer, wrong namespace) and is
correctly recorded as insufficient.

## The fix to run (one cycle)

1. In **each browser netns**, a route that gives the probe a source
   address — reachability is not needed:
   `ip -n nsim_b route add 8.8.8.8/32 via 192.168.102.1` (and `nsim_a`
   via its gateway). Log `ip -n nsim_b route get 8.8.8.8` in the driver
   before launch.
2. Launch Chromium with `--webrtc-ip-handling-policy=default` so
   enumeration is on regardless of any inherited policy; log
   `chrome://policy`'s `WebRtcIPHandling` value once for the record.

Expected renderer output after the fix: `network.cc` lines and
`Port[…:Net[eth0:192.168.102.x/24:Ethernet:id=1]]` with cost 10/50, no
"Allocate ports on any any"; then the pair forms and the row goes
direct. **Pin that `Net[eth0…]` line as the Chromium-behind-NAT
precondition** once a run satisfies it (the agent rightly refused to
pin it before) so the row cannot regress into the same red silently.

## Cleanup owed after green

Remove the `--vmodule` plumbing that is no longer needed; keep the
`[chromium-ice]` job-log print (it earned its place); keep the
per-namespace pcap capture; record §6.12 as closed with this mechanism;
the multicast route and gateway route stay as "harmless, not causal".
