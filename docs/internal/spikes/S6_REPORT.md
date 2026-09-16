# Stage 6 — browser ↔ browser, direct

Brief: [`spikes/S6_BRIEF.md`](../../../spikes/S6_BRIEF.md). Plan:
[`BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`](../plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md)
§10, §11. Base: `e342007a0` (Stage 5 round 4; CI green, browser matrix
26/26 on both gate engines).

Stage 6 is **additive**. Nothing in the Stage 5 protocol, the mode
boundary or the Stage 3/4 seams changes, with one named exception
(§6.4, a surface that could not previously function). Stage 5's four
open owner questions stay open; this stage does not answer them and
does not depend on their answers.

---

## 1. What the stage claims

Two browser tabs, in isolated contexts, with no shared state and no
harness back-channel between their pages, **find each other through a
native anchor, negotiate a direct WebRTC path, and then exchange
application data that the anchor never sees** — asserted on the
anchor's own per-pair forwarding counters rather than on the
browser's opinion of its connection state.

Everything below is measured through the public
`@net-mesh/browser` surface. No witness reaches past it into the wasm
leaf, and no witness asserts on a private method.

---

## 2. Exit criteria

| # | Criterion | Where it is proven | Verdict |
|---|---|---|---|
| 1 | Two isolated browser contexts reach a direct session | `stage6_two_isolated_contexts_reach_a_direct_session` | **MET** |
| 2 | Key discovery precedes signalling, from a signed announcement | `stage6_each_leaf_learns_the_others_keys_from_a_signed_announcement` | **MET** |
| 3 | The four wasm methods drive one attempt end to end | `stage6_the_four_wasm_methods_drive_one_attempt_end_to_end` | **MET** |
| 4 | An undiscovered peer is refused before an offer exists | `stage6_an_undiscovered_peer_is_refused_before_an_offer_exists` | **MET** |
| 5 | An unanswered offer leaves the pair relayed and typed | `stage6_an_offer_nobody_answers_leaves_the_pair_relayed` | **MET** |
| 6 | A second offer supersedes the live attempt | `stage6_a_second_offer_supersedes_the_live_attempt` | **MET** |
| 7 | The ICE attempt ledger partitions on both leaves | `stage6_the_ice_attempt_ledger_partitions_on_both_leaves` | **MET** |
| 8 | Direct application data does **not** reach the anchor | `stage6_direct_peer_app_data_leaves_that_counter_flat_while_the_anchor_is_live` | **MET** |
| 9 | Losing the direct path returns the pair to the relay | `stage6_forcing_the_direct_channel_down_moves_the_counter_again` | **MET** |
| 10 | One network change drives exactly one re-attempt | `stage6_a_network_change_drives_exactly_one_re_attempt` | **MET** |
| 11 | `rtcStats()` uses native field names and names what it cannot measure | `stage6_rtc_stats_uses_the_native_field_names_and_names_what_it_cannot_measure` | **MET** |
| 12 | A runnable demo on the public surface | `browser-demo` job, four rows | **MET** |

Fifteen browser witnesses (floor 41 with Stages 4b and 5), four demo
rows (floor 4), six natsim topology rows.

---

## 3. The §10 witness: what "direct" is asserted on

A browser reporting `connectionState === "connected"` proves the
browser's opinion. §10 asks for something else: that the **anchor
stops carrying the traffic**. The witness therefore reads the
anchor's live per-pair forwarding counters and asserts in three
parts, each failing independently:

| Part | Path | `(a→b)` | `(b→a)` | Payload evidence |
|---|---|---|---|---|
| 1 | routed | 1 → 7 | 1 → 7 | 3 nonces sent, 3 named at B, 3 echoes back at A |
| 2 | **direct** | 7 → **7** | 7 → **7** | 3 nonces named at B and echoed, anchor untouched |
| 3 | channel down, then restored | 7 → 11 | 7 → 11 | B's inbox `[]` for 2004 ms while down; both nonces after |

Bit-identical across two serialized runs.

**The third context is what makes part 2 mean anything.** An
unrelated pair C↔B is routed through the same anchor throughout the
flat window. In that window the unrelated pair's counter moved 0 → 5,
forwarded `0x0D02` moved 26 → 28, and A's re-announcement reached C
as version 1 → 2. So the flat counter is *flat while the anchor is
live*, not flat because the anchor stopped working — the misreading
§10 explicitly warns about.

**Routed is routed by addressing, not by luck.** Parts 1 and 3 never
service an ICE candidate, so the pair is relayed because no handshake
ran, not because ICE happened to fail. Restoration in part 3 uses the
production surface with no new leaf API, and leaves the Noise session
untouched — so "the same counter moves again" is a statement about
the **path**, not about a new session.

**Counter and payload are asserted separately.** Every payload
carries a nonce the receiver names and echoes. This is not ceremony:
it is what caught §7.1, which a counting assertion would have passed.

### 3.1 The same property, behind two real NATs

The §10 witness runs on loopback. The natsim matrix runs it behind
simulated NATs in Linux network namespaces, and the cone-ar × cone-ar
row is **green on Firefox**:

```
[runner] a discovered ["9684434118755589433"] for natsim.matrix.peer
[runner] connectPeer settled as "direct" in 269 ms; acceptPeer as "direct"
[runner] anchor ice attempted=2 direct=2 relayed=0 failed=0 pending=0
[runner] row browser_cone_cone_firefox landed direct, as expected
nat_flow.json: {"a":{"udp_flows":1,"udp_replied":1},"b":{"udp_flows":1,"udp_replied":1}}
```

Two headless browsers, in separate namespaces, behind two separate
address-restricted NATs, punched a direct DataChannel — and the
**gateways' own conntrack** is the witness for it. That last line
matters more than the endpoints' agreement: a `[UNREPLIED]`-free UDP
flow on both gateways is the NAT saying packets crossed between the
two public addresses in both directions, and neither endpoint was
asked. It is the one party to the session with no stake in the
answer.

The six Chromium rows pass too, on the same three witnesses — see
§6.12, which is now closed.

---

## 4. One network change, one re-attempt

`navigator.onLine` and an ICE `disconnected → failed` transition feed
one bounded owner. The row stages the distinction rather than
assuming it:

1. the pair goes direct;
2. the trigger is armed (**opt-in** — see below);
3. the page closes the DataChannel it created, keeping index 0 (the
   anchor's), and at that instant the report must read `started=0`:
   **"the path is down" is not "the network changed", and only the
   second may act**;
4. the offerer's context goes offline for 1.2 s and returns, the page
   dispatches a *second* real `online` event for the same change, and
   exactly **one** re-attempt starts — `ice_attempted` delta 1.

The answerer stays online throughout: a re-offer nobody answers would
satisfy a counter while proving nothing about restoration.

**Arming is opt-in, by design.** Nothing arms itself, so no existing
witness changes behaviour and §3's forced-direct-failure row cannot
be fought by a background re-offer. Two lanes whose mechanisms would
otherwise race, made non-interacting by construction rather than by
scheduling. The demo lane corroborates the negative control
independently: across every clean demo run the only ICE-visible
events were the two bootstrap dialogs plus one peer dialog per
participant — no spontaneous re-attempt, measured by a harness that
was not looking for one.

---

## 5. `rtcStats()`

Thirty-nine native field names, sixteen emitted, and **twenty-four
carrying an explicit not-applicable reason** rather than a plausible
zero. A zero that means "not measured here" is a lie a dashboard will
average.

---

## 6. Findings

Eleven defects found by **building on** this surface rather than
reviewing it. Six are in the product or its witnesses; five are in
instruments, harnesses and toolchains that reported success without
doing the job asked of them. Five surfaced only on CI, which is the
argument for gating the demo and the matrix rather than shipping them
as samples.

### 6.1 Cross-talk: two peer streams under one label share an id

**Not fixed in Stage 6.** Stage 5 surface; this stage is additive by
instruction, not by difficulty.

Mechanism, four links:

1. `leaf/src/stream.rs:115` `stream_id_from_label(label)` derives the
   id from the **label alone**. The peer is not an input, by design —
   it is what lets both ends derive the same id without exchanging
   it.
2. `leaf/src/node.rs:357` classifies the **receive** side per
   `(NodeId, u64)` — peer *and* id. **The leaf core is correct.**
3. `leaf/src/node.rs:199-206` is where the peer is **lost**: the
   `StreamData` event emitted to JS carries `stream_id`, `seq`,
   `payload` and no peer. Its neighbour `StreamFailed` (`:207-214`)
   *does* carry `peer_node` — the field was available and simply is
   not on the data event.
4. Both filters above it can therefore only key on the id
   (`leaf/src/wasm.rs:2283`, `browser-ts/src/stream.ts:88`/`:192`).

**Observed.** B held one stream to A and one to C, both labelled
`s6.app`. B's inbox for the A stream reported C's payloads — and
because that page echoes what it receives, **B echoed C's payloads to
A**. No error, no typed refusal, no counter records it.

**Visible only because the row compares the receiver's nonces exactly
rather than counting them.** A counting assertion would have passed:
five payloads arrived, five expected. This is the strongest argument
in the stage for exact correlation over counts.

*Workaround (page-side, what the rows do):* one label per pair, or
pin `streamId` per peer. *Fixes, all owner decisions:* (a) fold the
peer into the derivation — changes an id both ends derive
independently, protocol-adjacent; (b) put `peer_node` on
`StreamData` and key both filters on `(peer, stream_id)` — changes a
page-facing event schema; (c) refuse a second peer under a bound
label, typed — cheapest, and narrows a working API.

*Today a page addressing two peers with `openStream({ peer, label })`
and one label gets cross-talk rather than a refusal, and a receiver
that echoes gets it amplified.*

### 6.2 An instrument that fired and hit nothing

The first loss injector reported success on every call while the
system under test was unaffected. Caught only because a row that
should have failed did not.

### 6.3 `BrowserContext.setOffline` never reaches the renderer

Playwright's `setOffline` emulates at the URL loader. The page
observed **zero** `offline` events and the trigger fired zero times.
A retry row built on it would have asserted "the page saw a network
change" while the page saw nothing. The driver now also sends CDP
`Network.emulateNetworkConditions`.

**And that CDP session must be held.** Detaching it in a `finally`
*reverts* the emulation: `offline` and `online` fired microseconds
apart, the page saw a complete and plausible event pair, and
`navigator.onLine` read `true`. **A reverted emulation is
indistinguishable from a working one unless you read the renderer's
state back** — which the op now does and the row now asserts.

Three instruments in one stage, same shape: *an instrument must be
verified against the system's own observable state, not against the
fact that the call returned.* The same discipline replaced a 250 ms
sleep with a `window.__netChangeArmed` handshake — nobody is tempted
to tune a handshake.

### 6.4 `LeafNode::signal` could not accept the leaf's own node ids

**The only behaviour change in a pre-existing path in this stage, and
it is a fix to a surface that could not previously function.**

`LeafNode::signal` parsed node ids decimal-first via `parse_u64`, so
it **refused the output of `node_id_hex()`** — the leaf's own
spelling — while the leader-proxied path re-encodes canonical 16-hex.
The proxied `signal` therefore could not work at all. It survived
because nothing exercised it. Both surfaces now parse through
`parse_peer_id`, the same reader the four peer methods use, pinned by
`stage6_one_node_id_spelling_across_both_signalling_surfaces`.

An audit found the spelling drift; closing it found the dead path
underneath.

### 6.5 A verdict string that argues its own conclusion

A demo row's detail line ended with "…so the flat pair counter is not
a quiet mesh" — printed on failing rows too, where it was false.
Details now state numbers only; PASS/FAIL carries the claim. **A
verdict string that argues its own conclusion is a small lie waiting
to be quoted**, and other witnesses in this repo are still written
the old way.

### 6.6 A baseline window an ordering argument said could not exist

The demo's flat row asserts the pair counter is `0+0` *before* the
pair exists, rather than merely `t1 > t0`. The argument that the
baseline was provably pre-pair was **wrong at a 50 ms polling gap**:
one run in six read `1+1`, because the second page's report, its
first announcement, the peer's discovery and the routed Noise
handshake all fit inside one poll interval.

The row **failed** rather than passing on a contaminated baseline —
which is the entire return on asserting `baseline == 0` instead of a
delta. The fix removes the window rather than widening the poll: the
baseline is captured *inside* the `/report` handler that completes
both-ids knowledge, before that response returns, and a page
announces only after its own POST resolves.

*An ordering argument held with confidence, falsified by the run that
was supposed to be a formality.*


### 6.7 "Discovered" is one-directional; the handshake requires both

The demo's four rows never ran on CI: the page offered as soon as it
had discovered the peer, and the relayed handshake died with
*"discoverable but not reachable through the anchor"*.

The message was exactly right and everyone read it as vague.
`node.query` reads **this** leaf's store of signature-verified
announcements, so A discovering B proves A holds B's announcement and
says nothing about the reverse — and a leaf answers a relayed
handshake only from a node whose announcement it has verified, which
is slice 1's keys-from-discovery-only property working correctly. A
one-directional precondition on a mutual requirement.

Ruled out first, with evidence rather than argument: admission is not
the cause (`admission_refused_transit` stayed 0, admission completed
in 1–3 ms across every failing run), and neither is §6.8 (the browser
matrix hits it 26 times in the same CI run and goes 41/41).

Reproduced deterministically 2/2 — not under CPU load, but by
starting both pages simultaneously, which closed a startup skew the
demo had been relying on without knowing it. 5/5 after the fix. The
gate is now mutual discovery plus anchor-side admission, read on the
host, which already had both pages' ids and was not using them. No
library deadline was touched.

### 6.8 A control frame sent before its socket is open is discarded

`anchor_control_plane.rs:230 send_frame` calls `send_with_str`
without checking `ready_state` and without a queue, so a frame
emitted between `open_trickle` returning and the socket opening
throws `InvalidStateError: Still in CONNECTING state` and is **lost**
— not deferred. The comment above it reasons about the socket opening
before the answer is installed; opening is asynchronous, so that is a
race, not an ordering.

Observed on every CI page load, tolerated by every row. Not fixed:
Stage 5 surface, and nothing this stage asserts depends on it. The
honest fix is to buffer until `onopen` and flush.

### 6.9 An installer that reported success and installed the wrong thing

All seven natsim browser rows died in 1.1 s with no output. The
cause, once the evidence was reachable: `browserType.launch:
Executable doesn't exist at …/chromium_headless_shell-1194`. The
driver depends on `playwright-core` only, which ships no CLI, so a
bare `npx playwright install` fetched the **latest** `playwright`
(1.63.0) and installed **its** browser builds (v1243) while
`playwright-core@1.56.0` looks for v1194. The step exited 0.

The CLI version is now read from the driver's own `package.json`, so
the two numbers cannot drift apart silently.

Two evidence gaps were fixed to get there, and both are worth more
than the bug: the rows exited with **empty stderr and no cause**
(the scenario waited on a verdict file that would never be written),
and the state dirs are root-owned so `upload-artifact` failed with
`EACCES` — the artifact step ran and produced nothing. A conformance
matrix that fails without naming a cause is the one failure mode it
must not have.

### 6.10 The rows were dying in the witness, after the thing they witness

With the browsers finally launching, all seven rows still failed —
and the cause was not ICE. `run_scenario.sh`'s gateway flow witness
reads `/proc/net/nf_conntrack`, which does not exist on the GitHub
runner kernel, under `set -euo pipefail`: `cat` exits 1, the pipeline
fails, and the script dies **mid-write, after the runner had already
written a good verdict**.

Proved from a file size before any further CI cycle: every browser
state dir's `nat_flow.json` is exactly **36 bytes** —
`{"a":{"udp_flows":0,"udp_replied":0}` — truncated between the `a`
value and the `,"b":` that follows it; no browser dir contains the
gateway snapshot written later in the script while every native dir
does; and stdout stops after the pid marker with empty stderr. The
working `conntrack -L` → proc → "(no view)" ladder was already in the
same file, ten lines below.

Second, separate cause in the same run: Firefox refuses to run as
root when `$XDG_RUNTIME_DIR` is owned by another user, and Playwright
rethrows that as the generic *"Target page, context or browser has
been closed"*.

### 6.11 A log line that asserts more than its code checked

Chasing the remaining rows, the anchor appeared to contradict the
page: *"the attempt's channel is already open, so there is nothing
left to apply it to"* while the page reported ICE never connected.
There is no contradiction. The line fires whenever
`dispatch_bootstrap_candidate` returns `Err` and the attempt is live,
and that `Err` covers **any** non-`CandidateApplied` outcome — the
message states a conclusion the code never tested.

Same class as §6.5: a string that argues its own conclusion. It cost
a cycle of chasing a contradiction that did not exist. Not fixed
here; the wording is leaf surface.

A related blind spot, worth naming because the log looked complete:
the bootstrap listener lives in `net_sdk`, which the harness's
`RUST_LOG` filter did not include, so the anchor's entire dialog view
— offer accepted, trickle socket authorized, every typed refusal —
was simply absent.

### 6.12 The Chromium NAT rows: closed — the page's STUN server was the anchor it had to pair with

**Closed.** All six Chromium rows and the Firefox control pass in
natsim run
[35142933036](https://github.com/ai-2070/net/actions/runs/35142933036)
(13/13 scenarios: six native, six Chromium, one Firefox control).

There were **two** defects, in series, and the second was invisible
until the first was fixed. Both were in the harness or its
environment; neither was in the product.

**Defect 1 — interface enumeration was off.** Chromium gates
local-interface enumeration on media permission. With it denied,
`FilteringNetworkManager` withholds the network list the browser
process has already delivered, the allocator logs `Allocate ports on
any any`, every port binds the `any` address at cost 999, and inbound
datagrams are matched against a network list that is empty — so they
are dropped before any STUN parsing. Granting camera+microphone for
the page's own origin (what a real user does before a call) fixed it:
run 35138824048 logs `received permission status: granted`,
`Count of networks: 1`, `Net[eth0:192.168.102.x/24:Ethernet:id=1]`,
`Allocate ports on eth0`, and a gathered srflx. The rows still failed.

**Defect 2 — the harness named the anchor as the page's STUN
server.** One rule in libwebrtc
(`webrtc/p2p/base/stun_port.cc`, `UDPPort::OnReadPacket`, verbatim):

```c++
  // Look for a response from the STUN server.
  if (server_addresses_.find(packet.source_address()) !=
      server_addresses_.end()) {
    request_manager_.CheckResponse(packet.payload());
    return;
  }
  if (Connection* conn = GetConnection(packet.source_address())) {
```

The `return` precedes `GetConnection`. **Every** datagram arriving on
an ICE port from an address that port was configured with as a STUN
server is handed to the gathering path and consumed there — responses
and requests alike, in both directions.

`drive_sequence` gave the page exactly one `iceServers` entry,
`stun:10.99.0.10:7100`, and the anchor's ICE host candidate **is**
10.99.0.10:7100: the product answers STUN on the RTC socket, by
design. So the anchor was both the STUN server and the ICE peer, and
every packet between them was eaten before ICE saw it.

That is the whole of the symptom table above, which no STUN-level
reading had explained:

| Measured | Explained by |
|---|---|
| 197 valid, integrity-correct, transaction-matched responses arrive and **none** is credited | consumed by `request_manager_.CheckResponse` and `return`ed |
| the anchor's 7 Binding **Requests** go unanswered | same branch; a request is not a response, so it is simply dropped |
| **zero** receive-side `connection.cc` lines while the pcap shows the packets landing | the `Connection` is never reached |
| silence in **both** directions — the one symptom nothing else fitted | the rule is per-source-address, not per-direction |
| Firefox passes on the same topology, same anchor, same run | nICEr reads its sockets directly and has no such rule |

**The controlled experiment was already running in CI and had never
been read as one.** The loopback harness's `[mdns]` sweep
(`rtc_browser/runner/src/main.rs`) varies exactly one thing — whether
the page's `iceServers` names the anchor's own socket — across two
interfaces, with the STUN variant deliberately first on one and second
on the other so position is excluded. ci.yml run 35137453461,
Chromium:

```
[mdns] loopback/anchor-stun:  NO PAIR — timeout: datachannel open
[mdns] loopback/no-stun:      PAIR FORMED in 22 ms   (same anchor socket 127.0.0.1:36622)
[mdns] interface/no-stun:     PAIR FORMED in 20 ms
[mdns] interface/anchor-stun: NO PAIR — timeout: datachannel open
```

Firefox forms a pair in **both** `anchor-stun` positions. And that
sweep's `working_stun` fallback — take the first configuration that
forms a pair — is why §6.12's founding premise held: the loopback
matrix had silently dropped the STUN server, so "Chromium works
against this same anchor on loopback and fails behind the NAT" was
true for a reason that had nothing to do with the NAT. A harness that
searches for a working configuration will hide the defect it searched
around; that is a finding in its own right.

**The fix.** The run's STUN responder is a separate host from the ICE
peer — 10.99.0.11, the aux public address `setup.sh` always adds to
the wan bridge (`run_scenario.sh --stun-ip`). That is also what a real
deployment has. `serve_stun` stays on the anchor as well: the leaf's
own `UdpBlocked` evidence probes the anchor's published `rtc_addr` and
must keep being answered. No product code, no deadline widened, no
assertion weakened, no NAT flavour touched.

**Pinned, so neither defect can return quietly.** The driver counts
every `Net[…]` descriptor the allocator prints and returns the counts
in its `shutdown` reply; `run_row` **refuses** a Chromium tab that
allocated no port on an enumerated network, naming the interfaces it
did see. A row that loses interface enumeration now fails saying so
instead of timing out sixty seconds later, indistinguishably from a
real ICE failure. Defect 2 is pinned by the rows themselves: with the
anchor as STUN server again, no Chromium row can reach `direct`.

**Dead, and not to be reopened**: the four readings §6.12 already
killed (gateway/NAT setup, str0m's candidate parsing, a deaf anchor, a
refused egress, `addIceCandidate`, answer/candidate ordering), plus
two more this closure retires — mDNS obfuscation (falsified twice:
disabling it changed nothing) and the default-local-address route
(real mechanism, wrong namespace, and enumeration was already working
when the rows still failed). The multicast route and both default
routes stay as **harmless, not causal**, recorded as such in
`setup.sh`.

**What this gap never cast doubt on**: the browser matrix (41
witnesses, both engines, green), the demo (4 rows, green), and the
Firefox NAT row described in §3.1.

**Named for the owner, not fixed here (out of stage scope).** The
harness collision is a harness bug. The *configuration* that produced
it is one the plan invites, so §6.12.1 states it as a plan-level
consequence and stops short of choosing a resolution.

#### 6.12.1 Plan-level consequence: `rtc_addr` is one socket serving two roles

**(a) The rule and its scope.** `UDPPort::OnReadPacket` consumes and
`return`s any datagram whose source address is in `server_addresses_`
— the port's configured STUN servers — before it reaches
`GetConnection`. So on a **Chromium-family engine (any libwebrtc
build: Chrome, Edge, Electron, Playwright's Chromium)** a leaf handed
`stun:<anchor rtc_addr>` as an `RTCIceServer` **can never form a
candidate pair with that anchor**: its own checks go out, the anchor
answers, and every answer is eaten by the gathering path. It is
per-source-address, not per-direction, so the anchor's own checks are
dropped too. **Firefox is unaffected** (nICEr has no such rule, and
its `anchor-stun` legs form pairs — §6.12's 2×2); WebKit is untested
here and should not be assumed either way.

The plan makes that configuration the obvious one. §5 (line 841)
defines `rtc_addr` as the anchor's **"public RTC/STUN socket,
UDP-only"** — one socket for both roles — the §7 announcement-field
registry carries the same field, and §6 rules out a demux,
deliberately and for good reasons. Stage 4b's R8 repair then *proved*
the announced address is a live STUN target
(`natsim_natted_anchor_publishes_a_reachable_rtc_addr`:
`stun_probe_ok` / `stun_probe_target` / `stun_probe_mapped`, against
`RtcStats::stun_binding_requests`), and `@net-mesh/browser` ships
`stunUrl(rtcAddr)` to turn it into exactly that URL. The leaf does not
force it — `iceServers` is caller-supplied and the leaf passes it
through verbatim (`wasm.rs: parse_ice_servers`) — but nothing warns
against it, and both of this repo's own harnesses did it.

**(b) Three ways to resolve it. Not chosen here; owner's call.**

1. **A second socket.** The anchor answers STUN on its own socket/port
   and announces that separately from `rtc_addr`, so a leaf's STUN
   server is never its peer's ICE address. Costs a wire field and a
   second bound port per anchor; §6's no-demux ruling is untouched
   (two sockets, not one demuxed).
2. **The leaf refuses or strips it.** At `connect`, compare each
   `iceServers` entry against the anchor this leaf will pair with and
   drop it (or refuse the option) with a typed warning. Keeps one
   socket and one announcement; puts engine-specific knowledge in the
   leaf, and needs a rule for the peer-to-peer case where the same
   address is a legitimate STUN server for a pair that does not
   include it.
3. **Documentation only.** "Never configure an anchor's `rtc_addr` as
   an `iceServer` for a Chromium leaf." Zero code; relies on every
   integrator reading it, and the failure it prevents is a silent
   60-second ICE timeout with no diagnostic — which is what cost this
   stage five cycles.

**The `UdpBlocked` probe is unaffected by all three.** It sends one
unsolicited RFC 5389 binding request to `rtc_addr` from a **throwaway
`RTCPeerConnection` with no remote description and no ICE peer**
(`leaf/src/bootstrap.rs`, `browser-ts/src/udp-probe.ts`), so the
eaten-by-the-gathering-path branch is precisely where its answer is
*supposed* to land. Option 1 leaves it pointed at `rtc_addr`, which
still answers; option 2 touches `connect`'s `iceServers`, not the
probe; option 3 is prose. Any resolution that removed the anchor's
`serve_stun` — none of the three do — would break it, and with it the
only evidence that distinguishes `UdpBlocked` from `IceTimeout`.

**(c) Which witnesses would have to change.**

| Resolution | Stage 4b | Stage 5 | Stage 6 |
|---|---|---|---|
| 1. second STUN socket | `natsim_natted_anchor_publishes_a_reachable_rtc_addr` keeps probing `rtc_addr` unchanged; a **new** witness is owed for the announced STUN address (reachable, mapped, and *not equal* to `rtc_addr`) | `stage5_udp_blocked_surfaces_a_typed_failure` unchanged — but `udp_block.rs` installs its firewall rule scoped to `rtc_addr`, and would need the second port blocked too, or the probe passes while ICE is dead | natsim's `--stun-ip` becomes the product's announced address instead of a harness choice; the loopback `[mdns]` sweep's `anchor-stun` legs become "announced-stun" legs and should then form pairs on Chromium, so `working_stun` can go |
| 2. leaf refuses/strips | unchanged | unchanged | a new leaf witness: the stripped entry is reported (typed warning, not silence) and `effective_ice_servers` shows it gone — that method exists because Stage 5 dropped `RTCIceServer`s silently once already |
| 3. documentation only | unchanged | unchanged | unchanged; the `[mdns]` sweep's `working_stun` fallback must then be made loud (it currently hides this exact defect) and the natsim `--stun-ip` comment becomes the normative reference |

No witness is weakened under any of the three, and none of them
changes a NAT flavour, a deadline or a counter identity.

### 6.13 Two more log lines that asserted more than their code knew

`no such dialog on this anchor` renders `SignalOutcome::Ignored`,
which collapses a missing dialog AND any driver error into one
string. The registration-race reading it invited is therefore a
hypothesis, not a finding, and is recorded as such. Separately,
`add_remote_candidate` returns `()`: "our wrapper reported success"
cannot distinguish acceptance from the ICE agent's silent rejection,
so a claim that a candidate was *applied* was never established by
any run — only that the code called an API.

Both were caught by the reviewer, after both the implementer and the
harness lane had reasoned from them. The lane's own summary is the
one worth keeping: *"the same defect class I flagged on the original
line, and I walked into it one layer down — a string that names one
cause for an outcome that has several."*

---

## 7. The demo

`net/crates/net/examples/browser-demo/` — two tabs, cursors, the
public surface only.

- **60.00 / 59.96 Hz** sustained per tab over 6.2 s, 360 positions
  sent each way, 361 received. 1–2 fire-and-forget frames dropped in
  360 and **displayed rather than hidden**: fire-and-forget means
  what it says, and a demo showing 360/360 would misrepresent the
  reliability class it chose.
- **Reopen cost** (the direct install fencing the routed handle):
  0.10–0.50 ms, i.e. 0.01–0.03 frame slots at 60 Hz, against
  21.9–22.5 ms of scheduler jitter. **A page author can treat it as
  free; what they cannot do is skip it.**
- *An honest footnote:* the first measurement read 61.79 Hz because
  the rate divided a cumulative `sent` that included the routed burst
  by the 60 Hz loop's own elapsed time — flattering arithmetic, 3 %
  high. **Caught by reading a number that was better than the
  target.** Measurement bugs that flatter you are the ones that ship.

**Chromium only, stated rather than implied.** The demo's driver
needs Chromium's throttling-defeat flags and SwiftShader for WebGL,
neither with a Firefox equivalent here. Engine coverage is the
browser matrix's job; a one-engine demo is not a matrix and is not
described as one.

---

## 8. Evidence discipline

Every row in this stage has a **raw inverse receipt**: the defect
reintroduced with a bounded diff, the exact command, the exit code,
the verbatim RED output, and the restored green. A row that has never
been seen to fail is not a witness.

Two receipts are worth naming:

- **§3 part 2.** The pair is put *back on the relay* just before the
  flat window. Payloads still arrive at B and echo, the unrelated
  pair still moves, announcements still move — and the row goes red
  purely because the counter moved 7 → 13 each way. **Arriving bytes
  cannot make that row green.**
- **The demo's flat row.** `peer:` dropped from `openStream`, so the
  positions addressed the anchor: the pair counter stayed flat at
  1+1 while 0/0 arrived and the anchor's ingress went 92 → 604. **A
  flat counter with nothing arriving must not pass.**

Two receipts were applied in one run to show the rows fail
independently rather than collapsing into one assertion with three
names.

---

## 9. CI

| Gate | Floor | Pinned from |
|---|---|---|
| Browser matrix (Chromium + Firefox) | 41 | `stage6.rs` `WITNESSES` |
| Browser demo (Chromium) | 4 | `browser-demo/host/src/main.rs` |
| Leaf native | 266 | run count |
| R11 ABI probes | 12 | `abi_real_package.mjs` |
| natsim scenarios (6 native + 6 Chromium + Firefox control) | 13 | `natsim.yml`, each row pinned by name |
| Chromium interface enumeration, per NAT'd tab | ≥1 port on `Net[eth0:…]` | `run_row`, from the driver's own count |

Each roster is validated against its **source** before any log is
read, each parser proves it can find a known verdict line before a
count is believed, and the demo binary prints `[demo] NOT REACHED:
<names>` if it exits before a row ran — so an early exit cannot
present itself as a smaller green count.

The enumeration floor is the one gate that is not a count of
witnesses. It is a **precondition**: a Chromium tab that allocated
every port on `Net[any:0.0.0.x/0:Wildcard:id=0]` has enumerated no
interface and drops every inbound datagram before STUN parsing, so
any verdict a row reached would be about that and nothing else.
§6.12 spent five cycles reading such rows as ICE failures.

---

## 10. Scope boundaries

- **Firefox was never run on the implementation host**; that leg is
  CI's, and no row or detail claims otherwise.
- **The six natsim rows and the Firefox control ran in CI only.**
- **`peer` on `openStream` is refused, not ignored, on the
  leader-proxied path** — a typed refusal rather than a silent
  downgrade to the routed path.
- **Stage 5's four open owner questions remain open.** Stage 6
  neither answers nor depends on them.
