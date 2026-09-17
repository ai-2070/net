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

The six Chromium rows pass too — see §6.12, which is now closed.

**What a NAT row does and does not witness, exactly.** The rows read
the typed disposition on both halves of the dialog, the §10 ICE
ledgers on both leaves and the anchor, and the gateways' own
conntrack. Until this round they did **not** read an application
payload, and the sentence that used to stand here — "the same three
witnesses" — read as though §3's loopback payload proof applied to
every NAT row. It did not, and it could not: conntrack reply traffic
on a direct row can be ICE or Noise, and on a relayed row the typed
`iceTimeout` says the routed session was KEPT, never that a byte
crossed it. Kyra's E1 was right to refuse the transfer.

Each row now carries its own **fourth** witness: a nonce-correlated
bidirectional exchange on the public peer-addressed stream surface,
plus the anchor's own per-pair `forwarded_app_packets` read in the
anchor's process either side of it — **flat both ways on a direct
row, moving both ways on a relayed one**
(`tests/natsim/rows.rs`, `AppExchange`). The runner mints both
nonces; neither page chooses one, so "B decoded exactly what A sent"
cannot be satisfied by one side. Delivery and the counter disposition
fail independently, and the inverse receipt for the direct row's half
(`spikes/S6_RECEIPTS/S6A-01-direct-row-flat-forwarding.log`) shows
why it had to exist: with only that one assertion disabled, a row
whose payload the anchor carried still satisfies its typed outcome,
both leaves' exact ledgers, the anchor's ledger and a two-way replied
flow on both gateways.

Three more gaps of that shape closed with it:

- **A NAT'd anchor with two announced endpoints.** The matrix's
  anchor sits on the simulated internet, outside both NATs, so its
  two local sockets say nothing about two externally reachable
  mappings — and the older `rtc_anchor_direct` row never exercised
  the second endpoint §6.12.2 added. `rtc_anchor_stun_endpoint` puts
  the anchor **inside** the cone NAT with both ports pinned 1:1 and
  reads both announced addresses back out of the anchor's own
  announcement; the client outside gets a STUN reply from the
  announced `rtc_stun_addr` carrying its own public tuple, and takes
  its session onto a DataChannel at the announced `rtc_addr`. Two
  mappings, two replies, reachable by deliberately different means
  (the ICE port stays address-restricted; the STUN port is
  forwarded, because an endpoint that drops a stranger's first
  request could never serve the peers it is announced to).
- **A permission-free Chromium leg.** The six rows grant the page
  camera+microphone, because Chromium withholds interface
  enumeration from WebRTC until a media permission exists (§6.12).
  That the product calls no media API is source evidence about the
  product, not a measurement of the ungranted context.
  `browser_cone_cone_nomedia` is row 1 with one variable removed, and
  the grant is now part of the row table and asserted against what
  the DRIVERS report doing.
- **Measurement failure is no longer observed absence.** A gateway
  whose conntrack table could be read by neither reader used to
  yield `{"udp_flows":0,"udp_replied":0}` — which is exactly a
  relayed row's confirmation. Each side now reports the reader that
  produced its numbers and an unreadable side is a refusal
  (`S6A-06-unreadable-gateway-is-not-absence.log`).

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

#### 6.12.2 The resolution, built: a separately announced STUN endpoint

Kyra ruled **option 1** plus fail-fast rejection of a known
collision, and rejected documentation-only, in
[`spikes/S6_STUN_ENDPOINT_BRIEF.md`](../../../spikes/S6_STUN_ENDPOINT_BRIEF.md):
*"The receive-dispatch behaviour belongs to libwebrtc, but our
product contract invites the incompatible configuration. Changing the
harness closes the experiment; it does not fix that contract."*

**What shipped.** `rtc_addr` is unchanged and keeps both of its
roles. A new announcement field `rtc_stun_addr` carries a distinct
endpoint, signed in `SignedPayloadCanonical` immediately after
`rtc_addr` (field count 15 → 16), emitted only when configured. The
anchor binds a second UDP socket that answers STUN and nothing else.
The leaf reads the announced endpoint from `GET /rtc/anchor` and uses
it as its own default `iceServers`, so an integrator does not have to
discover which STUN service avoids its own anchor.

| Kyra's acceptance boundary | Evidence |
|---|---|
| 1. Announced STUN reachable **and distinct** from `rtc_addr`, NAT case included | `the_two_announced_endpoints_are_different_sockets` + `the_stun_only_socket_answers_a_binding_request`; distinctness is structural, not observed — two independent binds, and a NAT cannot map two internal tuples to one external tuple for a protocol whose reverse translation keys on the external port. natsim asserts the public tuples after the gateway. |
| 2. **Chromium and Firefox** connect on the **product-advertised** configuration, no harness search | the matrix's separate STUN host is DELETED; the anchor announces its own endpoint and `matrix.js` configures no `iceServers` at all |
| 3. The conflicting configuration fails **promptly and descriptively** | `LeafError::IceServerConflictsWithPeer`, returned after `attach` and **before** `create_offer` |
| 4. `UdpBlocked` still targets `rtc_addr`, classification unchanged | proven, not assumed: both call sites still read `anchor_rtc_addr()`, the new accessor appears only inside `connect`'s default, and a browser-ts test fails if a second entry reaches the probe's configuration |
| 5. NAT matrix passes with **no** topology change, longer deadline, or candidate-type criterion | no NAT flavour, deadline, floor or disposition changed; `--stun-ip` is vestigial rather than repurposed |

The typed refusal, verbatim — pinned by `assert_eq!` on **both**
sides, because `@net-mesh/browser` reconstructs the variant by
parsing this prefix and a reworded message would silently demote it
to `unknown` with nothing red:

```
ice configuration: the iceServers entry stun:198.51.100.7:4433 names
this connection's peer RTC endpoint 198.51.100.7:4433; a peer cannot
be its own STUN server. Omit iceServers to use the STUN endpoint the
anchor announces (the stun_addr field of GET /rtc/anchor), or name a
STUN server that is not this peer
```

**Decisions worth keeping.**

- **Absence, not emptiness.** An explicit `iceServers: []` is a
  caller saying *none* and is honoured; only an omitted option takes
  the default. A default that also fired on `[]` would override an
  intent rather than supply a missing one.
- **`effective_ice_servers({})` still reports `[]`.** The leaf uses
  the announced endpoint without pretending the caller configured it.
- **Detection is equality-only**, and DNS aliases are documented out
  of scope rather than implied to work.
- **A second fail-fast, unasked and then commissioned.** Two
  correctly distinct sockets announced under one endpoint reproduces
  §6.12.1 exactly, and neither the driver nor the leaf could see it.
  `RtcDriver::spawn` now refuses before binding when the announced
  endpoints collide, or when the two binds do. The bind arm exempts
  port 0 — two `ip:0` binds compare equal and are nonetheless two
  sockets — and that exemption was found by a negative witness going
  red on the ordinary `127.0.0.1:0` configuration, not by reasoning.
  The best receipt in the slice: inverting the bind comparison yields
  `AddrInUse`, which is *literally the confusing failure the check
  replaces*.
- **`stunUrl` → `diagnosticStunUrl`**, clean cutover, no alias. The
  implication that it also builds an anchor connection's `iceServers`
  is what cost this stage five cycles. §6.12.1 keeps the old name
  because it describes the pre-fix state.

**Two qualifications Kyra required, stated rather than buried.**

1. **The harness's camera/microphone grant is not permission to make
   media access a prerequisite for a data-only Net application.** It
   is a harness fact about Chromium's interface-enumeration gate.
   Verified: there is no `getUserMedia`, `getDisplayMedia` or
   `MediaStream` anywhere on the leaf or `browser-ts` path — every
   connection and the probe are `RTCPeerConnection` + `DataChannel`.
   The product must work without it, and does.
2. **Reachability is the second socket's weak point, not
   distinctness.** `stun_only_loop` only ever replies, so it cannot
   bootstrap its own NAT mapping: the announced endpoint is reachable
   only via a static or forwarded mapping, exactly like `rtc_addr`.
   natsim's anchor is not NAT'd so this does not arise there; an
   operator running a NAT'd anchor must forward both ports.

**A named gap found on the way, not fixed here.** The leaf vendors
byte copies of the cross-language fixtures, because `include_str!`
cannot cross a package boundary and the wasm runner must replay them
inside Chromium. The copies are load-bearing; their *guard* is not
well placed. Editing a fixture in the core workspace goes green where
the editor is working and reddens only a separately-invoked
`cargo test -p net-mesh-leaf` — which is exactly what happened during
this slice. The repo already solves it correctly for
`aead_vector.json`, where the core asserts the mirror byte-equal so
the red lands at the edit. Fix shape: one test in
`tests/cross_lang_wire.rs` comparing against
`net_leaf::test_vectors::ALL`. This is the second guard in this stage
that existed but sat on the far side of the boundary it guards — the
first was natsim's table-vs-script cross-check, which only fires in a
job that has to get that far.

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

`net/crates/net/examples/browser-demo/` — three tabs, cursors, the
public surface only. Two of them are the pair; the third is a
signalling prober (below).

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

**The flat window now asserts signalling inside itself.** The demo
already asserted bidirectional receipt, flat ordered-pair counters,
maintained direct status, the send-rate floor and advancing
announcement ticks — and it SAMPLED the anchor's `0x0D02` counter
while only ever reporting setup-time movement. Kyra's E2 refused
that, correctly: a number that had already stopped moving was
displayed beside a window the demo claimed liveness over.

The reason it had stopped is worth recording, because it also rules
out the obvious fix. §9 step 4 clears the leaf's relay entry for its
peer at the direct install, so every `0x0D02` frame A signs for B
rides the DataChannel and never reaches the anchor to be counted:
**two leaves and a direct pair have exactly zero signal transit.**
And the public `signal()` method cannot substitute —
`AnchorControlPlane::signal` is a typed refusal (R14), pinned green
by its own witness. A demo that drove it would have asserted a
refusal.

So the demo runs a **third leaf whose whole job is public
signalling**: tab C discovers tab B by a capability tag, is never
answered, and therefore stays RELAYED for life while every
`connectPeer` it calls signs an offer that transits the anchor. That
is the mechanism the merged runner's part 2 already proves, in a demo
you can watch. The fifth witness,
`demo_public_signalling_moves_the_anchor_signal_counter_in_the_flat_window`,
asserts the anchor's own signalling counter moved, that the prober
really called the public API inside the window (floor 1, measured
count printed), and REUSES the flat and still-direct booleans of the
existing flat row so the two provably speak about one window. The
four existing witnesses keep their names and predicates verbatim.

The manual HUD's "fresh announcements" sentence is now gated on
measured tick freshness — the age of the last observed climb in the
anchor's resolved tick, against a bound derived from the announce
interval — instead of on receiving positions and a flat counter. When
the counter is flat and positions arrive but the tick is stale, the
HUD says so and names the age.

---

## 8. Evidence discipline — and the receipt index

This section used to claim that **every** row in the stage had a raw
inverse receipt: the defect reintroduced with a bounded diff, the
exact command, the exit code, the verbatim RED output and the
restored green. **That claim was not traceable and is withdrawn.**
It described two chains and retained neither; no Stage 6 chain
existed anywhere in the tree, which is what the reviewer's filename
search found and what an inventory of this repository confirms
(`spikes/S6_RECEIPTS/README.md` §1: the only captured inverse
receipts in the tree were Stage 5's, in `spikes/S5_R3_NATIVE_RECEIPTS`
and `spikes/s5r3_inverse`). A green CI log establishes that a witness
**passed**; it can never establish that it **can fail**.

What exists now, per witness, is
**[`spikes/S6_RECEIPTS/README.md`](../../../spikes/S6_RECEIPTS/README.md)**:
an index with one row per Stage 6 witness, naming for each the source
SHA, the selective diff, the executed witness and engine, the command
and exit, the intended assertion failure, the restoration identity
and the restored positive output — or saying plainly that no receipt
was taken, and why. A row marked *no receipt* is a witness whose
ability to fail has not been demonstrated; that is worth seeing, and
averaging it into a stage-wide sentence is what went wrong here.

**Nine raw receipts** were taken for this round's new acceptance
arithmetic, all in that directory, each with the mutated file's
`sha256` before / during / after, both exit codes (101 mutated, 0
restored) and both runs verbatim. Two are worth naming:

- **`S6A-01`.** The direct row's flat-forwarding assertion is
  disabled and nothing else. Both nonces still arrive, both leaves'
  ledgers are exact, the anchor's ledger is exact, both gateways show
  a two-way replied flow — and the anchor carried the payload.
  **Nothing else in the row notices**, which is the argument for the
  witness existing.
- **`S6A-06`.** `GatewayFlows::measured()` returns `true`
  unconditionally, and a relayed row then passes on
  `{"udp_flows":0,"udp_replied":0,"source":"unreadable"}` — a
  gateway whose conntrack table was never read, confirming that
  nothing crossed it.

The two chains this section used to describe are recorded in the
index as **described, not captured**, with the §3 part 2 relay re-pin
and the demo's dropped `peer:` named as prose observations rather
than as receipts. The forced-channel-down phase remains earned credit
and is not relabelled a receipt.

---

## 9. CI

| Gate | Floor | Pinned from |
|---|---|---|
| Browser matrix (Chromium + Firefox) | 41 | `stage6.rs` `WITNESSES` |
| Browser demo (Chromium) | **5** | `browser-demo/host/src/main.rs` |
| Leaf native | 266 | run count |
| R11 ABI probes | 12 | `abi_real_package.mjs` |
| natsim scenarios (7 native + 7 Chromium + Firefox control) | **15** | `natsim.yml`, each row pinned by name |
| natsim row table + checker (ungated, every platform) | **38** | `tests/natsim_browser.rs` |
| Chromium interface enumeration, per NAT'd tab | ≥1 port on `Net[eth0:…]` | `run_row`, from the driver's own count |

The floors this round moves, and the names behind them: the demo
gains `demo_public_signalling_moves_the_anchor_signal_counter_in_the_flat_window`;
natsim gains `natsim_browser_cone_cone_is_direct_without_media_permission`
(the permission-free Chromium leg) and
`natsim_natted_anchor_publishes_both_mapped_endpoints` (the NAT'd
anchor's two announced endpoints); the ungated checker goes 22 → 38
with the application-delivery and measurement-failure rows.

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
- **The two new natsim legs have never been executed anywhere.** The
  permission-free Chromium leg and the two-endpoint NAT'd anchor need
  Linux netns and root; their first run is CI's, and their results
  are claims until it happens. `spikes/S6_RECEIPTS/README.md` §2.2
  records them as unexecuted rather than implying otherwise.
- **`tests/natsim.rs` is not type-checked on the implementation
  host.** It is `#![cfg(target_os = "linux")]` and the cross check
  fails in `cc-rs` for want of `x86_64-linux-gnu-gcc`; the ungated
  `tests/natsim_browser.rs` carries every assertion that can live
  outside the gate, which is why the row table and its checker are
  there and not in the gated file.
- **The three-context demo was not executed on the implementation
  host** either: it needs Playwright Chromium and a built wasm
  bundle. Its fifth witness has no runtime receipt yet.
- **`peer` on `openStream` is refused, not ignored, on the
  leader-proxied path** — a typed refusal rather than a silent
  downgrade to the routed path.
- **Stage 5's four open owner questions remain open.** Stage 6
  neither answers nor depends on them.


## 11. First repair round — answering the Stage 6 HOLD

Kyra's Stage 6 review (`KYRA_STAGE6_REVIEW.md`, pinned at
`9ef5b6a03`) returned eight required repairs, three missing acceptance
items and five secondary corrections. This section states what each
became. **Nothing here closes a Stage 5 item**, and the Stage 5
fourth-repair HOLD remains separate and open.

**Read this first: the tree moved under the review.** She pinned
`9ea5bc8`…`9ef5b6a03`; this round lands on top of Stage 5 rounds 4 and
5, which she has not reviewed. Line numbers in her report no longer
resolve. Every finding below was located by symbol and re-verified
against current source before being repaired — none was taken as read
from a line reference.

### 11.1 The eight repairs

| item | what it became |
|---|---|
| **S6-01** | A signed establishment proof on its own subprotocol, verified against the initiator's announced identity, bounding the responder to a provisional admission until it verifies. §11.2 |
| **S6-02** | An internal `Attempt{peer,dialog}` captured before every await and compared before every mutation, settlement, removal, install or publication |
| **S6-03** | One terminal transition spanning ICE, Noise and installation, retiring the exact owned resources — including S6-01's provisional admission — and excluding late installation |
| **S6-04** | Bounded retention under the authorizing dialog across both phases that discarded candidates, with the bound itself witnessed |
| **S6-05** | Arming and pair ownership moved into the single retry owner; the answerer install now revokes, which is the role-reversal half |
| **S6-06** | Completion claims the attempt before installing or charging; `PeerLink`'s destructor performs the discard accounting replacement used to skip |
| **S6-07** | All five configurations: bound-before-override, resolved-pair validation, the effective server list, the per-peer PC check, and parsed-address comparison |
| **S6-08** | Durable release published with `send_replace`, read before waiting, with a bounded post-abort wait added after CI found the unbounded one |

### 11.2 S6-01, because it is the one that is not mechanical

The owner ruled the mechanism: retain Noise, add a domain-separated
signed proof verified against the initiator's already-verified
announcement identity. Two implementation facts are worth a reviewer's
attention because they were forced rather than chosen.

**The proof cannot ride a Noise payload.** Message 1 is written before
the final handshake hash exists and message 2 is the responder's, so
no Noise payload can carry a signature over the final transcript — it
could only bind a partial hash, which is a *different value on the two
sides*. It rides its own subprotocol frame on the established session,
which additionally seals it under the keys that transcript produced.

**The transcript binds roles, not just identities.** Both node ids in
role order *plus* an explicit role byte, because ordering alone leaves
the two roles' statements identical for a symmetric pair — an
initiator's proof could then be presented as a responder's. The
handshake hash is what makes a proof non-transferable to any other
establishment; the peer id, the announcement and the signalling dialog
are all stable across attempts, which is exactly why none of them is
sufficient.

Kyra's own impostor reproduction is now a pinned witness, with an
honest control beside it so the refusal cannot pass by breaking
everything.

### 11.3 The preserved reviewer probes are no longer byte-identical

Stated plainly because she verified the property herself and recorded
it as evidence: her retained `prior-probe-preservation.diff` is empty
and **will not be empty again**. The four files gained 12 lines each
inside their `connect()` helper — 48 insertions, **0 deletions**, no
test name, assertion, expected value or message changed, all 42 still
passing, verified here rather than taken on a lane's report.

The old helper handshook and then let every probe treat the responder
as holding a session immediately after message 1, which is precisely
the PSK-only attribution S6-01 removes. The helper now completes the
protocol. **Adding discovery restores the premise those probes always
meant** — two mutually discovered leaves with a real session — which
the old form could skip only because the responder never checked who
it was talking to.

### 11.4 What the acceptance work withdrew

Two claims in this report were overstated and are withdrawn rather
than quietly adjusted:

- the "same three witnesses" claim transferred the strong loopback
  payload proof into every NAT row; the rows never contained a payload
  exchange at all, and now do;
- §8's claim that every Stage 6 row had a mutation → RED → revert →
  GREEN chain was untraceable. Inventory first: the repository held
  **zero** captured Stage 6 receipts. Two were described in prose and
  the rest rested on green CI logs, which establish that a witness
  passed and never that it can fail. `spikes/S6_RECEIPTS/` now holds
  the receipts taken this round and an index that records *no receipt*
  where none was taken.

### 11.5 Two witnesses that would have run zero times

Both caught before push, both the same shape, and the reason this
round pins counts rather than presence:

- `wasm_rtc_conservation` (S6-06's leaf half) is a new `--test`
  binary, and the leaf job's suite list is hardcoded — a fourth suite
  runs nothing until it is named. Added with `floor=2`.
- the 13 S6-02/03/04 witnesses are **lib** tests on the wasm32 target,
  because they must reach the private `Inner`. The job's only wasm
  invocation passes `--test <name>` and cannot reach a `#[cfg(test)]`
  child module of `wasm.rs`. A `--lib` step with `floor=13` now runs
  them; the target was previously uninvoked in that job.

### 11.6 What is witnessed only in CI, and what is not witnessed at all

- **S6-06's leaf half has no native witness and will not get one.**
  `leaf/src/rtc.rs` is `#![cfg(target_arch = "wasm32")]` in its
  entirety, so a native test could only re-implement `PeerLink` and
  assert the copy — while the claim under test is *which production
  paths perform the accounting*, which a fake link cannot carry. Its
  two assertions had never been observed to hold at the time they were
  written: the author could compile for wasm32 but not execute.
- **The Firefox leg of the browser matrix does not run on the
  implementation workstation** — the harness refuses to start without
  NSS `certutil` rather than fall back to a root store it never
  writes. Chromium ran 42/42 locally; Firefox is CI's.
- **The NAT and demo acceptance legs require Linux netns and root**
  and therefore execute only in CI.

### 11.7 A hang CI found that no local run reproduces

`a_frame_captured_under_a_retired_incarnation_cannot_revive_its_reassembly`
hit nextest's 180 s terminate on the first repaired head. It passes
alone in ~3.2 s and the binary is 49/49 under nextest on the
workstation, so the cause needs the contention of 193 tests on a small
runner. The obvious explanation was checked and **killed**: the test's
synchronous dispatch pause runs on the mesh's ingress task, not on a
task `TaskRelease` owns, and `driver.close()` awaits no reply.

Two unbounded waits on the shutdown path were found from source and
bounded — one new in this round (`TaskRelease::join`'s post-abort
await) and one pre-existing (`MeshNode::shutdown`'s task drain, a bare
`handle.await` with no abort and no deadline). `abort()` is
cooperative: it lands at an await point, so a task wedged in a
synchronous call never reaches one and an unbounded wait converts "a
task that cannot be cancelled" into "shutdown never returns" — in
production, not only under test. Neither bound is deadline padding: no
test deadline moved, no retry was added, and the timing-out witness is
untouched. Neither is claimed as the confirmed cause.

### 11.8 The permission-free Chromium leg: what was measured, and what §6.12 over-claimed

Kyra's E1 third item asked for an ordinary permission-free Chromium
NAT leg and noted that the required no-grant SUCCESS was unproven.
The leg was built, it ran, and **it succeeded** — including on the
direct path. The failing row in
[35182320241](https://github.com/ai-2070/net/actions/runs/35182320241)
was the harness refusing its own successful measurement.

Nothing below re-derives §6.12's mechanism. The gate is real and is
not in dispute: `FilteringNetworkManager` withholds the network list
until a media permission exists, and both permission-free tabs logged
`received permission status: denied`, `Allocate ports on any any`,
and ports created on `Net[any:0.0.0.x/0:Wildcard:id=0]` at cost 999.

#### Three questions, answered separately

Deliberately not collapsed, because Net's anchor-routed fallback is
**not TURN** — it rides each leaf's authenticated session with the
anchor rather than a relay allocation, so "it falls back to the
anchor" is itself a claim about leaf-to-anchor application delivery.
If the anchor hop failed, there would be no fallback to fall back to.

**1. Can the permission-free leaf establish its authenticated anchor
session, with an application request/reply? YES.** Not "ICE
succeeded", not "a DataChannel opened": tab A issued a **capability
query** over the session and got back the peer set containing B's
node id, which only the anchor could supply and only from B's signed
announcement. Both tabs completed `Connect` (distinct node ids
`4ad9c8926e4cbc99`, `ed508e02ebdd68b6`) and both completed
`Announce`. Every one of those steps is a `require` in
`drive_sequence`, so reaching the later steps is proof the earlier
ones returned. Leaf drop counters were zero across the board —
including `establishment_unproven`, `no_session` and `unparsable`.

**2. Can two permission-free leaves exchange application data over
the ROUTED path?** **OPEN. Instrumented, not yet run.** Q1 makes this
a real question rather than a formality: Net's routed path rides each
leaf's authenticated anchor session, hop 1 is now demonstrated for a
permission-free tab, and hop 2 is not.

That run could not have answered it. On a row that solves direct the
anchor's per-pair application counter is flat *by assertion* — that
flatness is the direct row's own witness — so a direct row is
structurally the one shape that cannot observe forwarding. **Direct
success does not substitute for this.** What the run does show is
that the anchor had already forwarded application-class packets for
this pair before the nonce exchange began (`forwarded_pre_ab=2`,
`forwarded_pre_ba=1`, on a counter that excludes `0x0D02`
signalling) — consistent with the routed path working, but not a
receiver-observed payload, so it is not an answer.

`browser_symmetric_symmetric_nomedia` is the instrument: the relayed
row with the grant removed, on the pair ICE cannot solve. It requires
all three of:

1. **receiver-observed nonces in BOTH directions** — the same
   instrument as the direct row, no weaker. Each side must decode the
   exact nonce the *other* side was given by the runner
   (`seen_at_b == nonce_a`, `seen_at_a == nonce_b`), and each
   direction must show a non-zero send and a non-zero receive.
2. **increases in the APPLICATION-ONLY per-pair forwarding counters,
   in both directions, as deltas** — `forwarded_delta()` subtracts
   pre from post with a checked subtraction (a counter that went down
   is an unusable reading, not a flat path), on
   `forwarded_app_packets(src32, dest)`, which excludes `0x0D02`
   signalling and therefore cannot move because a candidate was
   trickled.
3. **an accounted routed disposition, not a timeout plus a counter.**
   This is the one that changed. `delta != 0` says the anchor
   forwarded *something* for the pair while the exchange happened,
   which one unrelated application packet satisfies — so it cannot
   separate "the routed session carried these payloads" from "the
   payloads arrived by some other route and the counter moved
   anyway". `Forwarding::Carried` now requires the anchor to have
   forwarded **at least as many application packets as the sender
   itself reports handing to the transport, in each direction**
   (`delta >= sent`; `>=` because the counter is packets and the
   sender counts frames, so fragmentation can only make the anchor's
   number larger). The send step is a bounded retry loop on a
   `fireAndForget` stream, so `sent` is whatever actually went out
   and the requirement scales with the exchange instead of collapsing
   back to presence. Pinned by
   `a_relayed_row_the_anchor_only_partly_carried_fails`, which is a
   shape the previous check accepted.

Beside that, and independently: both halves of the dialog must type
the disposition themselves and agree (`connectPeer` on the offerer,
`acceptPeer` on the answerer), `udpBlocked` is refused as a
diagnosis the evidence does not support, the ledgers are exact
(`attempted=2 direct=1 relayed=1 failed=0 udp_blocked=0`,
`pending=0`), and **both gateways' conntrack must show no replied
flow between the two public addresses** — which is what makes the row
able to fail when the payloads arrived directly after all.

**The instrument gap, stated rather than papered over.** The leaf's
public surface exposes no per-peer route accessor: `openStream`,
`counters`, `announce`, `query`, `connectPeer`, `acceptPeer`, and the
`signal` event are the whole of it, and none of the leaf's counters
is route-typed (`packets_out` increments identically whether a frame
went to the peer or to the anchor). So "from the leaf's own view" is
carried here by the typed outcome on both halves plus the exact
ledger, and "the application path in use is the anchor's" is carried
by the anchor's per-pair accounting plus both gateways' conntrack —
three parties, none of them alone. A per-peer route accessor on the
leaf (or the relayed `PeerConnectOutcome` carrying its live
`dialog`/`peer` the way the direct one does) would let the row state
it in one place from the endpoint itself. That is a `leaf/` change,
it is not this slice's to make, and it is named here rather than
implemented.

**3. Can the same pair establish DIRECT connectivity? YES, with
application delivery.** Measured, from the run's own verdict:

| witness | permission-free `cone-ar × cone-ar` |
|---|---|
| `page_type` / `peer_page_type` | `direct` / `direct`, and they agree |
| `connect_peer_ms` | 273 |
| leaf ICE ledgers (A and B) | `attempted=2 direct=2 relayed=0 failed=0 udp_blocked=0`, `pending=0` |
| anchor ICE ledger | `attempted=2 direct=2` |
| application payloads | `seen_at_a` = B's nonce `1cd3a6cc73980c73`; `seen_at_b` = A's nonce `b04608cf1698adee` — each side decoded a nonce it could not have minted |
| anchor per-pair app forwarding | `a→b 2 → 2`, `b→a 1 → 1` — **flat both ways** across the exchange |
| `nsim_gwa` conntrack | `192.168.101.2:54230 → 10.99.0.3:39308`, reply tuple present, `[ASSURED]`, **not** `[UNREPLIED]` |
| `nsim_gwb` conntrack | the mirror image, also replied |
| enumeration | tab a `real=0 wildcard=117`, tab b `real=0 wildcard=157`, `nets=[any:0.0.0.x/0:Wildcard, any:0:0:0:x…/0:Wildcard]` |

Direct-versus-forwarded is therefore carried by two independent
witnesses that are not the endpoints' own opinion: the anchor's
counter did not move while the payloads crossed, and both gateways
saw a two-way flow between the public addresses.

#### Why it works with enumeration off

From the permission-free tabs' own renderer log
(`/tmp/natsim.o3TSU0/runner.log`), and this is the part §6.12 got
wrong. The wildcard IPv4 port binds `0.0.0.0:60339` (tab b) /
`0.0.0.0:53892` (tab a) and **still reaches the announced STUN
endpoint and still learns its mapping**:

```
Gathered candidate: Cand[…:1677729535:10.99.0.x:60339:srflx:192.168.102.x:60339:…:0:999:0]
New selected connection: Conn[…Net[any:0.0.0.x/0:Wildcard:id=0]…srflx:udp:10.99.0.x:60339->…host:udp:10.99.0.x:7100|CRWS|S…]
```

What the denial actually costs, all of it visible in that log:

- the real **host** candidate, replaced by an mDNS name
  (`68136645-…-634e5139be50.local:60339`) because the renderer does
  not know its own address;
- the **IPv6** leg — its wildcard port logs `STUN server address is
  incompatible` (the STUN endpoint is IPv4) and its host candidate is
  `Discarding candidate because it doesn't match filter`;
- priority and cost: type preference `1677729535` instead of
  `1686052607`, network cost `999` instead of `0`.

None of those three is what decided this pair: the run solved on an
srflx-versus-srflx pair, which the denial leaves intact. That is a
statement about THIS row, and it is deliberately not promoted into a
rule. "Srflx decides NAT success" would be a new oracle of exactly
the kind this section is retracting — the previous one was
"enumeration decides it". What remains decisive is what was measured:
authenticated application delivery and the forwarding counters, not
any candidate-level theory about why they came out that way.

#### The claim that is withdrawn, and the observation that is kept

§6.12 concluded that with enumeration denied "inbound datagrams are
matched against a network list that is empty — so they are dropped
before any STUN parsing." **That causal claim is withdrawn.** It is
falsified by an end-to-end result on the same topology: a
wildcard-allocated pair parsed STUN responses, formed pairs and
delivered application payloads in both directions. The drop observed
in the runs that produced the claim is explained by §6.12's *second*
defect — the anchor was the page's configured STUN server, so
`UDPPort::OnReadPacket` consumed every datagram from it before
`GetConnection` — which was present in those runs and was never
separated from the first. That is the CONFOUNDER, and naming it is
not the same as accounting for every historical dropped packet: the
successful run falsifies the claimed *inevitability* of failure under
denied enumeration, and does no more than that. Runs from that period
carried both defects at once and cannot now be attributed
retrospectively to either one alone.

The **observation** is not withdrawn and is not erased: the grant was
a material part of the environment the six matrix rows were measured
in, `real=0` is what a permission-free Chromium tab does here, and
the counts are now written into every row's `browser_outcome.json`
under `enumeration`, tagged `required` or `observed`, on the passing
path as well as the failing one.

#### Scope, stated narrowly on purpose

This licenses exactly one thing: **on the tested Chromium build, the
tested IP-handling policy (`policy: default, multiple_routes: 1,
nonproxied_udp: 1`) and this topology, a camera/microphone grant is
not a prerequisite for a data-only Net application to reach its peer
directly behind two address-restricted cone NATs.** It is not a
universal statement about Chromium data-only WebRTC, it says nothing
about the narrower NAT flavours on the permission-free axis (only
`cone-ar × cone-ar` was run ungranted), and question 2 above is open.

#### Firefox

No difference to report, and that was already established: the
control has **always** run permission-free. Firefox has no media gate
on interface enumeration and Playwright cannot grant it camera or
microphone, so the driver never asked — `browser_cone_cone_firefox`
records `Media::None` because that is the environment it ran in.
`natsim_browser_cone_cone_is_direct_on_firefox` passed in the same
run at 04:43:01.

#### The harness change

Narrow, and in one direction only.

- The permissioned rows' `real > 0` refusal is **unchanged as a
  criterion**. Its message no longer asserts the withdrawn drop
  mechanism; it names the observation and the environment the row was
  built for.
- On a permission-free row the counts are **recorded, not asserted**
  (`Row::enumeration()` → `Enumeration::Observed`). Requiring them
  would pin one browser's gating policy as though it were a promise
  of this product — a future Chromium that stopped gating would
  surface as a regression here — and would refuse the working
  measurement above.
- `browser_symmetric_symmetric_nomedia` added, to answer question 2
  with the instrument that already existed.
- **`Forwarding::Carried` strengthened from presence to accounting**
  for every relayed row, granted and ungranted alike: the anchor must
  have forwarded at least as many application packets as the sender
  reports having sent, per direction, rather than merely having
  forwarded something. A timeout plus a counter increment is not a
  routed session, and the previous check could be satisfied by one
  unrelated forwarded packet. This is the only assertion in this
  round that moved, and it moved in the strict direction; the
  existing granted relayed rows satisfy it (their fixture and the
  measured exchange both have `delta >= sent`).
- No deadline widened, no retry added, no assertion weakened, no row
  deleted or skipped, and no media permission granted anywhere it was
  not already granted.

#### What is NOT closed

**Question 2, and `browser_symmetric_symmetric_nomedia` has never
executed.** Not "expected to pass", not "should pass given the
granted relayed rows pass" — it has never run, on any machine, in any
run of this suite. The netns rows require Linux and root; the
implementation workstation has neither, and the row was added after
the CI run this section is written from. No routed result is reported
here, and none is inferred from the direct success, which is the
substitution the reviewer ruled out.

A row that has never run has also **not demonstrated its own ability
to fail.** That is the same standard §11.6 holds S6-06's leaf half
to, and it applies here for the same reason: green is evidence a
witness passed and never evidence it can fail, and a witness that has
not run is not even that. What HAS been demonstrated to fail is the
checker the row will be decided by — `Forwarding::Carried`'s
accounting arm is pinned by
`a_relayed_row_the_anchor_only_partly_carried_fails` in both
directions, on a shape the previous check accepted — but that is the
acceptance arithmetic exercised against a synthetic verdict, not the
row exercised against two browsers behind two symmetric NATs. The two
are not interchangeable and are not counted as one here.

So the state of Q2 is: instrumented, pinned by name at the CI floor,
unrun. It closes when that row runs and passes, or it becomes a
finding when it runs and does not.

#### Adjacent, same round: the NAT'd anchor's second endpoint served, and the probe was lying about who asked

A separate row — `natsim_natted_anchor_publishes_both_mapped_endpoints`,
the first user of S6-07.1 — failed in run
[35185332604](https://github.com/ai-2070/net/actions/runs/35185332604)
at `tests/natsim.rs:541`. Recorded here because the plausible reading
was a deployment-envelope limitation and the measurement **refutes**
it.

What the verdict actually says, in the order the row asserts it:

- `stun_endpoint_target: "10.99.0.2:7103"` equals `anchor_stun_addr`
  — the probe was aimed at the address read back out of the
  announcement, not at a flag.
- `stun_endpoint_probe_ok: true` — line 533 PASSED. An unsolicited
  binding request from a stranger crossed the gateway's
  `dnat to 192.168.101.2:7103`, reached a private socket, was served,
  and the reply came back well-formed enough to parse an
  XOR-MAPPED-ADDRESS out of. **A port-forwarded STUN endpoint on a
  NAT'd anchor works, and serves strangers.** That is the deployment
  shape the row exists to establish, and it held.
- `stun_endpoint_mapped: "10.99.0.1:60141"` — line 541 failed. The
  reply named a real address that was not the client's `10.99.0.12`.

**It was source selection in the harness probe, not the DNAT and not
the reply path.** Three facts separate them:

1. `setup.sh` puts four addresses on `nsim_wan`'s one `br0` —
   `10.99.0.1/24` FIRST (line 165), then `.10`, `.11`, and `.12`
   under `--public-b`. `10.99.0.1` is therefore the device's primary.
2. The client is a joiner launched inside `nsim_wan` with
   `--bind 10.99.0.12:7002`, but `stun_probe` bound its socket
   `0.0.0.0:0`. The product's own sockets are bound explicitly; only
   the probe delegated source selection to the kernel, which picks
   the outgoing device's primary — `10.99.0.1` — with an ephemeral
   port, which `60141` is.
3. Every source-rewriting rule in `one_side` is scoped
   `oifname "gw$L-wan"` (LAN → WAN). The inbound rule is
   `iifname "gw$L-wan" udp dport $STUN dnat to $LAN.2:$STUN` —
   destination only. Nothing rewrote the source on the inbound path,
   and had anything done so the anchor would have seen the gateway's
   LAN address `192.168.101.1`, not a wan-bridge address.

So the anchor reported the source it genuinely observed. The reply
was a faithful mapping statement about the wrong socket — which is
the failure mode the assertion's own wording names ("about a mapping
rather than about a local socket"), arriving from the direction
nobody was watching: the probe, not the responder.

**The envelope claim is refuted, not confirmed.** "A DNAT'd STUN
endpoint reports a post-prerouting source and so cannot be a
reflexive oracle for its peers" would have been a real constraint. It
is not what happened: prerouting rewrote only the destination, and
the endpoint reported the client's true public tuple. Scoped as
usual — one anchor behind one `cone`-mode gateway with a single
forwarded UDP port, one un-NAT'd client on the same simulated
internet.

**The fix, and the assertion got stronger rather than weaker.**
`stun_probe` now takes the source address and binds it (`bind.ip()`,
the address the node's own mesh socket uses); a bind that fails is
reported as `ok: false` rather than silently downgraded to a wildcard
socket, because a probe that could not use this node's address cannot
make a statement about this node's mapping. And the probe now records
the tuple it actually bound, read back off the socket, as
`stun_endpoint_local` — so the row asserts
`stun_endpoint_mapped == stun_endpoint_local` **exactly**, on top of
the `10.99.0.12:` prefix check rather than instead of it. The prefix
pins which address was used; the equality pins that the anchor echoed
the source it really saw. The client is un-NAT'd on this topology, so
the mapping is the identity and exact equality is the statement
available; a rewrite in the path or a responder echoing a constructed
tuple now fails where a prefix check passed. No deadline moved and no
nftables rule changed.

Not yet re-run in CI, for the same reason Q2 is not: netns needs
Linux and root. The source-addressing invariant the row now asserts
was smoke-tested directly — a responder echoing its observed source,
probed from an explicitly bound socket, returns exactly that socket's
`getsockname()`, while an unbound socket's own `local_addr` is
`0.0.0.0` and the process cannot state which source the kernel will
choose. That is the mechanism, not the topology; the four-address
selection itself is not reproducible off Linux.
