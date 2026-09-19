# Demo — 60 Hz positions between two browser tabs, direct

Two tabs, two leaf nodes, one anchor. Each tab discovers the other
through a capability query, reaches a **direct** leaf ↔ leaf session
over real ICE, and then streams its cube's position to the other at
**60 Hz over a fire-and-forget stream** — while the anchor's own
per-pair forwarding counter is on screen, going **flat** the moment
the pair stops needing it.

A third tab is open beside them and sends no positions at all: the
**signalling prober**. It exists because the pair cannot prove the
anchor is still alive on the signalling path once it stops using it
— see [The third tab](#the-third-tab).

## Run it

```powershell
net\crates\net\examples\browser-demo\run.ps1          # Windows
```

```sh
net/crates/net/examples/browser-demo/run.sh           # Linux, macOS
```

One command, from a clean checkout. It is the documented build and
then the demo: the wasm leaf → `@net-mesh/browser` → the demo's two
npm dependencies (three.js, playwright-core) → the host. Three
Chromium windows open, one per tab; Ctrl-C stops everything.

Prerequisites, all of which the script assumes rather than installs:
Rust with the `wasm32-unknown-unknown` target, `wasm-bindgen-cli`
**0.2.128** (the version the leaf pins — a mismatch is a hard error at
bindgen time), and Node ≥ 20. Playwright's Chromium is fetched on
first run by the host itself.

The same command with `--check` (`-Check` on PowerShell) runs it
**headless and asserts the counter's shape**; see
[Is it actually true?](#is-it-actually-true) below.

## What you are looking at

```
anchor forwarded_app_packets
(this pair, both directions)     41 → · ← 39
flat for                         6.4 s
pair session                     DIRECT — leaf ↔ leaf, no anchor in the path
positions sent / received        2 391 / 2 388 (3 dropped)
measured send / receive rate     60 Hz / 60 Hz
signalling forwarded (0x0D02)    18 (excluded from the counter above)
signalling probe (tab c)         tab c only — the leaf that keeps the counter above moving
announcement tick the anchor …   23 / 22 (this leaf sent 24; last climb 180 ms ago)
```

The top number is the anchor's, read on the live `MeshNode` in the
host process and served to the page — not a number the page made up.
It is `forwarded_app_packets(src, dest)`: the packets this anchor
**forwarded** for one ordered pair, and it **excludes `0x0D02`
signalling** (`mesh.rs`, the `inner_sub != SUBPROTOCOL_RTC_SIGNAL`
arm).

That exclusion is the whole reason "flat once direct" means anything:

* it cannot go flat because signalling stopped — signalling was never
  in it, its own counter is displayed beside it, and tab C keeps that
  counter moving for as long as the window lasts;
* it cannot go flat because the page stopped — the positions actually
  arriving at the *other* tab are displayed beside it too, and so is
  the announcement tick this anchor keeps resolving for each leaf,
  with the age of its last climb printed next to it. The HUD's flat
  sentence is gated on that age, so a stale tick reads as a stale
  tick rather than as a healthy anchor.

**Flat, with those numbers moving, is a direct path and nothing
else.** A flat counter on its own would be indistinguishable from
traffic the anchor dropped.

The three phases are visible as they happen:

1. **routed** — the pair has a session through the anchor. The routed
   Noise handshake and a short burst of routed position frames go
   through it, and the counter **moves**.
2. **direct** — ICE completes, the direct session replaces the routed
   one, and the counter **stops**. The 60 Hz loop starts. Thousands of
   position frames later it has not moved.
3. the peer's cube keeps moving, drawn *only* from payloads that
   arrived. Fire-and-forget means a dropped frame stays dropped — the
   trail shows the gap rather than hiding it.

## The third tab

`0x0D02` signalling is excluded from the pair counter — but the pair
cannot keep the signalling counter moving either, and that is not a
detail. At the direct install the leaf **clears its relay entry** for
its peer (`leaf/src/wasm.rs`'s `direct_installed` →
`clear_peer_relay`), so every signalling frame A signs for B rides the
DataChannel and the anchor never sees it. Two tabs and a direct pair
produce exactly **zero** signal transit, so a two-tab demo could only
ever display signalling that moved while the pair was being set
**up** — a number that had already stopped, beside a window it was
being read as evidence for.

So tab C runs a third leaf whose entire job is public signalling. It
is handed a **tag** and never an id, like every other page here: it
discovers tab B by `demo.probe.target`, calls the public
`connectPeer` on it every second, and **nothing in the demo ever
answers it** — so that pair stays routed for its whole life, every
offer it signs transits the anchor as `0x0D02`, and
`note_signal_forwarded` keeps climbing inside the very window the
A↔B pair counter is asserted flat in.

It never announces `demo.positions`. `discoverPeer` takes the first
peer that is not itself, so a third leaf announcing the pair's tag
could be picked as tab A's peer and the demo would pair the wrong two
leaves.

The same mechanism is what the merged Stage 6 runner's part 2 already
proves (`tests/rtc_browser/runner/src/stage6.rs`, its
`signalling_moved` term); this is that argument in a window you can
watch.

## Is it actually true?

```sh
net/crates/net/examples/browser-demo/run.sh --check --seconds 6
```

Headless Chromium, driven by Playwright, three isolated browsing
contexts, and five assertions made **on the anchor** — not on a
screenshot and not on the HUD text (the HUD renders the same numbers
the host asserts on, which is why they cannot drift apart):

| witness | what has to hold |
|---|---|
| `demo_the_pair_counter_moves_while_the_anchor_carries_the_pair` | the per-pair counter is strictly higher once the pair is direct than it was before either leaf had a peer session |
| `demo_the_pair_counter_is_flat_while_the_pair_is_direct` | across the whole direct window it does not change by one, while both leaves report direct and both tabs receive at least half the expected frames |
| `demo_positions_sustain_60_hz_over_the_direct_path` | each tab's measured average send rate is ≥ 57 Hz, with the measured value, the worst single-frame gap and the drop count printed either way |
| `demo_announcements_keep_arriving_while_the_counter_is_flat` | the anchor resolves a **higher** announcement tick for each leaf at the end of the flat window than at its start |
| `demo_public_signalling_moves_the_anchor_signal_counter_in_the_flat_window` | across that same window the anchor's own `0x0D02` counter moved, the prober really called the public `connectPeer` inside it, and the pair counter was flat — the pair itself can contribute no signal transit at all once direct |

Each prints one `DEMO PASS <name>` / `DEMO FAIL <name>` line with its
numbers; any failure exits non-zero. The roster is a `const` in
`host/src/main.rs`, so a dropped row is a missing line rather than a
smaller green count.

`--seconds N` lengthens the flat window; at 60 Hz the default 6 s puts
about 360 frames per direction across the direct path while the
counter must not move once.

## The public API, and what it could not do

The page uses `@net-mesh/browser` and nothing else — no
`node.inner`, no `#[wasm_bindgen]` method called directly. The demo
is the readability test for the public surface, so anything it cannot
express is a finding about the package:

```javascript
const node = await connect({ credentialB64, bootstrapUrl });
await node.announce([PEER_TAG]);
const peers = await node.query(PEER_TAG);            // discovery only: no id, key or SDP is passed in
const outcome = await node.connectPeer(peerHex);     // or acceptPeer on the other tab
const stream = node.openStream({ reliability: 'fireAndForget', peer: peerHex, streamId });
await stream.send(frame);                            // 60 Hz
stream.onMessage((payload) => render(decode(payload)));
```

That `peer` option did not exist when this demo was written:
`openStream` pinned the stream to the anchor, so a page could
establish a direct peer session and then had **nothing that put a
byte on it**. It was added for Stage 6 (and blocks the §10 witness
just as hard as it blocked this demo).

Three things to know if you copy this page:

* **`connect()`, not `openSession()`.** A leader-proxied
  `openStream` refuses `peer` by name — a follower's stream is opened
  by the leader tab's node, and the Stage 5 leader request protocol
  carries no peer.
* **A `NodeDescriptor.nodeId` is an exact decimal string; `peer`
  wants 16 hex digits.** `JSON.parse` rounds integers above 2⁵³, so
  the wrapper never hands a page a number. `decimalToHex16` in
  `page/demo.js` is the one conversion.
* **One stream id per PEER, if you ever address two.** A stream id
  is derived from the label alone and the inbound filter keys on the
  id alone, so two peer-addressed streams opened under one label on
  one leaf share an id — and every payload from either peer is
  delivered to both consumers, with no error. This demo has exactly
  one peer per tab, so its pinned id is safe; a page addressing two
  peers must vary the `label` or the `streamId` per peer.

## Layout

```
browser-demo/
  run.sh / run.ps1   the one command: documented build, then the demo
  package.json       three.js (the renderer) + playwright-core (the three contexts)
  driver.mjs         Playwright over NDJSON on stdio — launch, three contexts, a page each
  page/
    index.html       the scene and the HUD
    demo.js          the leaf, discovery, connectPeer/acceptPeer, the 60 Hz loop,
                     and the prober's public signalling
  host/src/main.rs   the anchor, the bootstrap listener, the page server,
                     /pair (the live counter), /probe (the prober's gate),
                     and --check's five assertions
```

## TLS, and why no security dialog appears

The page talks to the **real** HTTPS bootstrap listener. The host
mints a CA and a `localhost` leaf for this run and tells Chromium
about exactly that one public key
(`--ignore-certificate-errors-spki-list=base64(SHA-256(SPKI))`).
Verification stays on, every other certificate is still verified, and
no platform trust store is written or read — so running this by hand
never puts a security prompt on your desktop. There is no
`--ignore-certificate-errors` and no `ignoreHTTPSErrors` anywhere in
the demo.

Everything binds `127.0.0.1`, so Windows Firewall has nothing to
prompt about either.
