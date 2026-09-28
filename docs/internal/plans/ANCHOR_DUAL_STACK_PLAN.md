# Dual-stack anchor: IPv4 and IPv6 players on one anchor (0.38)

## Status

**In progress. Target: 0.38.** Written 2026-09-28, after 0.37.1, while
working out how to deploy a game anchor. Amended the same day after Kyra's
review (see [Review](#review-kyra-2026-09-28)); slice 1 started. Companion to
[`ANCHOR_PREBUILT_BINARIES_PLAN.md`](ANCHOR_PREBUILT_BINARIES_PLAN.md); the two
can land in either order.

## The gap

An anchor offers browsers exactly **one** address to reach it over UDP, so a
player whose network cannot reach that address's family cannot play. In
practice the address is IPv4, and the players locked out are those on networks
with no IPv4 path: some enterprise, campus and lab networks, and any
IPv6-only network that has NAT64 but whose device has no CLAT.

Verified against the code (paths under `net/crates/net/`):

- **One socket.** `RtcDriver::start` binds a single
  `UdpSocket::bind(config.resolved_bind_addr(..))` and derives one `advertised`
  address from it (`src/adapter/net/rtc/driver.rs:1177-1179`). `RtcConfig` has
  one `bind_addr` and one `public_addr` (`src/adapter/net/rtc/config.rs:55-58`).
- **One candidate per session.** `new_session` adds a single
  `Candidate::host(advertised, "udp")` (`driver.rs:2361`), and
  `trickle_local_candidate` trickles the same single address over the signalling
  WebSocket (`src/adapter/net/mesh.rs:28061-28080`).
- **Every inbound datagram is stamped with that one address** as its
  destination (`receive`, `driver.rs:2004-2008`), and every transmit leaves
  through the one socket (`driver.rs:1828`).
- **One address to diagnose against.** `GET /rtc/anchor` reports one `rtc_addr`
  (`AnchorInfo`, `sdk/src/rtc_bootstrap.rs:514`). The leaf stores it
  (`leaf/src/wasm.rs:1881`), and the `udp-blocked` classification probes only it
  (`leaf/src/bootstrap.rs:483`, `classify_ice_failure`;
  `browser-ts/src/udp-probe.ts`, `probeStunBinding`).
- **No IPv6 RTC session is exercised anywhere.** The only IPv6 coverage in the
  RTC code is the STUN `XOR-MAPPED-ADDRESS` encoder
  (`src/adapter/net/rtc/stun.rs`,
  `ipv6_round_trips_through_the_transaction_id_mask`). No integration test,
  natsim scenario or browser witness binds `[::1]` or a v6 namespace.
  (Searched: `rtc*` tests, `sdk/tests/rtc*`, `tests/rtc_browser/`,
  `tests/natsim/`.)

Three defects sit next to the gap and must be fixed with it:

1. **A false `udp-blocked` diagnosis.** The rule is: the anchor answered over
   HTTPS, **and** a STUN probe to its published `rtc_addr` went unanswered
   (`UdpBlockedEvidence`, `browser-ts/src/errors.ts:72`). A player whose network
   reaches the HTTPS listener but not the `rtc_addr`'s family satisfies both
   observations while UDP is not blocked at all. This can happen **today**, on
   a NAT64 network without CLAT: DNS64 synthesises an `AAAA` for the anchor's
   name, so HTTPS succeeds, but the IPv4 literal in the ICE candidate and in
   the probe is unreachable. It becomes the common case as soon as an operator
   gives the HTTPS listener a real IPv6 address. That is why "just add an
   `AAAA` record" is not a safe interim fix. (This is inferred from the rule
   and how NAT64 behaves; it has not been reproduced.)
2. **Per-IP limits are evadable over IPv6, and the limiter is unbounded
   already.** Both ceilings, `--credentials-per-minute` and
   `--offers-per-minute`, are keyed by the full `IpAddr` (`RateLimiter`,
   `sdk/src/rtc_bootstrap.rs:703-729`). An IPv6 subscriber normally controls a
   whole /64 and can rotate through it freely, so opening the listener to IPv6
   disables both limits for those clients. Independently of IPv6, the limiter
   **never removes an entry and has no cap**: `allow` inserts, and resets an
   existing entry's window when that address returns. Every distinct source
   address the anchor has ever seen stays in memory. IPv6 makes the supply of
   distinct addresses effectively unlimited.
3. **The two probes disagree on what "answered" means.** The TypeScript probe
   counts a STUN error response (code below 700) as proof the endpoint is
   reachable (`browser-ts/src/udp-probe.ts:26-30`). The Rust WASM probe counts
   only a server-reflexive candidate (`leaf/src/bootstrap.rs`, `failed =
   !probe.saw_srflx.get()`). The same network can therefore be classified
   differently depending on which side ran the probe.

## The design

### Decision 1: how the anchor receives both families

**Recommended: A.**

- **A. Two sockets per role, one per family.** An IPv4 RTC socket (today's)
  plus an optional IPv6 RTC socket. The IPv6 socket sets `IPV6_V6ONLY`
  explicitly (via `socket2`, already a dependency at `Cargo.toml:325`), so both
  can bind the same port on Linux and so behaviour does not depend on the
  platform default. Each socket carries its own advertised address:
  - `receive` stamps a datagram with the advertised address of the socket it
    arrived on;
  - a `Transmit` leaves through the socket whose family matches its
    destination.

  Nothing else in the driver changes shape: sessions, the SCTP/DTLS state and
  the stats are shared.
- **B. One dual-stack `[::]` socket.** IPv4 peers then arrive as IPv4-mapped
  addresses (`::ffff:a.b.c.d`), which never equal the candidate a browser
  signalled. Every comparison in str0m's pair matching would need the address
  normalised in both directions. `IPV6_V6ONLY` also defaults differently per
  platform: off on Linux, on on Windows. It saves one socket at the cost of a
  class of subtle mismatches. Rejected.
- **C. Two anchors, one per family.** It works with no code change, but it
  splits the player population. A lobby on the IPv4 anchor is invisible to a
  player on the IPv6 one unless the two anchors mesh and relay, which is a
  bigger unknown than this plan. It also doubles operator setup. Rejected.

The IPv6 socket is **off unless configured**, the same discipline as the
Stage 6 STUN socket. An anchor started with today's flags binds, announces and
behaves exactly as it does now.

### Decision 2: what gets published, and where

Browser connectivity needs **no change to the signed announcement**. A browser
learns the anchor's candidates from the session's answer (`new_session`) and
from the trickled `Candidate` frame, not from `CapabilityAnnouncement.rtc_addr`.
So:

- **Session and trickle:** both advertise every configured family: two host
  candidates.
- **`GET /rtc/anchor` (`AnchorInfo`):** add `rtc_addrs: [..]`, listing every
  advertised address. Keep `rtc_addr` as the IPv4 (primary) address so leaves
  from 0.37 keep working unchanged. This is unsigned HTTPS JSON, so the change
  is additive and cheap.
  - **Serialization rule:** `rtc_addrs` is **omitted unless more than one
    family is configured**. An IPv4-only anchor's JSON is therefore unchanged,
    field for field. A consumer reads `rtc_addrs` when present, otherwise
    `[rtc_addr]`, otherwise nothing. The same rule applies to `ServeReport`.
- **The leaf's STUN/peer collision guard covers every address.** The leaf
  stores one `peer_rtc_addr` and refuses an `iceServers` STUN entry that
  names it (`check_ice_servers_against_peer`, `leaf/src/bootstrap.rs:547`).
  With two anchor addresses it must refuse a STUN entry matching **either**.
  Otherwise the IPv6 address reopens the configured-STUN-source collision
  Stage 6 diagnosed. This lands with the addresses themselves (slice 2), not
  as later cleanup.
- **Signed `CapabilityAnnouncement`:** unchanged. `rtc_addr` stays the one
  primary address.
  - **Why:** adding `rtc_stun_addr` in Stage 6 touched about 40 files. That
    covered the hand-written serializer (`behavior/capability.rs:2585-2605`),
    the folds and the capability bridge, the deck and aggregator,
    `tests/cross_lang_wire/capability_announcement_rtc.json` and
    `leaf/src/test_vectors/`.
  - **Who reads it:** browsers do not use it for connectivity. Native peers use
    it, and none dials an anchor over IPv6 today.
  - **When to revisit:** when something needs it (see Not in scope).

### Decision 3: `udp-blocked` with more than one address

A leaf probes **every** published address, all under **one bounded
diagnostic deadline** (not one deadline per address). It reports
`udp-blocked` only when HTTPS answered **and every** probe came back
`unanswered`. Everything else stays `ice-timeout`, the honest, weaker claim:

- one probe answered, including with a STUN error response;
- any probe `notRun` or `unsupported`, because an unrun probe is no evidence;
- an address that does not parse (`malformed`);
- an empty address list.

**Even "every probe unanswered" does not prove UDP is blocked.** It cannot
tell filtering apart from a dead UDP listener, a wrong advertised address, a
routing failure or loss. So the observation is reported as what it is. The
`udp-blocked` kind stays for compatibility, but its message and docs say
"no UDP response from the anchor's advertised endpoints", not "your network
blocks UDP". (Strictly, the single-address rule already overclaimed a cause;
two addresses make that visible.)

**Both probes use one definition of "answered"** (defect 3): a
server-reflexive candidate **or** a STUN error response below 700. The Rust
WASM probe gains the `icecandidateerror` path the TypeScript probe has. A test
drives each probe with the same observations; mirroring the classification
tests alone would not catch a probe that differs.

The evidence names everything it probed. To keep the published TypeScript type
compatible, `UdpBlockedEvidence.probed` stays a string (the first address
probed), and a new `probedAll: readonly string[]` holds every address. Rust
(`UdpBlockedEvidence::new`, `classify_ice_failure`) and TypeScript
(`udpBlockedEvidence`) change together, as the existing mirror comment
requires.

This also fixes the NAT64 case in defect 1 for any anchor that publishes an
IPv6 address: the v6 probe answers, so the misdiagnosis cannot happen.

### Decision 4: the HTTPS listener

- **Flags:** `--listen` becomes repeatable, so the operator names both
  addresses, for example `--listen 0.0.0.0:443 --listen '[::]:443'`. The v6
  listener also sets `IPV6_V6ONLY` explicitly. This is chosen over a single
  `[::]` listener relying on the platform default, which is dual-stack on
  Linux and IPv6-only on Windows.
- **Limiter:** the current `RateLimiter` has no bound and no reclamation
  (defect 2), so this is new scope, not a key change:
  - **Key:** IPv6 clients by their /64 prefix, IPv4 clients by the full
    address as today.
  - **Hard bound** on retained buckets.
  - **Reclamation:** expired entries (older than the window) are reclaimed,
    first on insert and periodically.
  - **When full:** after reclamation, if every retained bucket is still
    active, a **new** source is refused. An active restriction is never
    evicted to admit a newcomer, because eviction is exactly how a flood would
    erase a limit.
  - **Decision recorded:** refusing when full means someone controlling
    enough prefixes can fill the table and lock out *new* players for up to
    one window. We accept that. It is bounded in time and memory, the
    per-game ceiling (`--game ID:N`) still caps total issuance, and the
    alternative silently disables the limit for everyone. The bound is set
    generously (tens of thousands of buckets), and a refusal because the
    table is full is counted separately from an ordinary per-source refusal,
    so it is visible in the anchor's stats.
  - **Shared state:** both HTTPS listeners share **one** limiter state per
    ceiling. `bootstrap_router` builds that state today (`rtc_bootstrap.rs:755,
    768`), so the listeners must share one router, or the state must be built
    once and passed in. Calling it once per listener would give each family
    its own budget and double every ceiling.
  - **What /64 does not buy:** it closes rotation *within* a prefix. It does
    not make the limiter resistant to a client controlling many prefixes.

### CLI shape

- `--rtc-bind` and `--rtc-public-addr` become repeatable, **at most one per
  family**.
- The bind and public address of one family must match in family.
  `RtcConfig::validate` and `resolved_endpoint_conflict` apply per family.
- `ServeReport` gains `rtc_addrs`, and keeps `rtc_addr` (the report's
  documented shape is a consumer contract; see the comment on
  `ServeReport.rtc_stun_addr`).
- `RtcConfig` gains explicit per-family fields (`bind_addr_v6`,
  `public_addr_v6`) rather than a list. Two named slots make "at most one per
  family" a type fact, not a validation rule.

### What the anchor gets for free

A v4-only player and a v6-only player cannot pair directly. The anchor already
relays pairs that ICE cannot connect, so once it is dual-stack it **bridges the
two families** with no new code. Slice 4 witnesses it rather than assuming it.

## The slices

### Slice 1: the driver speaks both families

**Done 2026-09-28** (branch `anchor-dual-stack`). What landed:

- `RtcConfig::{bind_addr_v6, public_addr_v6}` with `dual_stack_conflict`,
  `dual_stack_primary_conflict` (the resolved primary must be IPv4) and the
  Stage 6 STUN-collision rule extended to the IPv6 endpoint, pre-bind and
  resolved (`resolved_v6_endpoint_conflict`);
- the driver's `RtcSockets`: the IPv6 socket bound with `IPV6_V6ONLY` set
  through `socket2`, per-socket advertised address, transmit routed by
  destination family, a readiness wait across both sockets;
- `MeshNode::{rtc_advertised_addrs, bootstrap_host_candidates}`, used by
  `trickle_local_candidate` and by the bootstrap listener's trickle socket,
  which now send one candidate frame per family, primary first.
  `bootstrap_host_candidate` remains, returning the primary.

**Evidence:**

- `tests/rtc_dual_stack.rs`, 5 tests, pinned in `ci.yml` (RTC harnesses,
  floor 5, two required names);
- 4 new config unit tests in `rtc/config.rs` (7 in the module), among the 71
  `rtc::` lib tests, all green;
- CI's whole RTC harness set, 215 tests including the new file, and
  `net-mesh-sdk --test rtc_bootstrap_listener` (30 tests), all green locally
  on Windows.

**Mutations run:**

- **Caught.** Routing every transmit through the primary socket, and offering
  only the primary candidate, each fail the flagship test.
- **Not caught.** Stamping IPv6 arrivals with the primary's address
  **survived**: str0m accepted the IPv6 checks anyway. The per-socket stamp
  stays because it is the correct local address, and the test says it does
  not witness it.
- **Linux only.** Dropping `set_only_v6(true)` is invisible on Windows, whose
  default is IPv6-only. It is caught on Linux (CI), where the read-back and
  the wildcard same-port bind both fail.

- `RtcConfig` per-family fields; a second socket with `IPV6_V6ONLY`.
- `receive` stamps the arriving socket's advertised address; `Transmit` picks
  its socket by destination family.
- `new_session` and `trickle_local_candidate` add every family's candidate.
- The driver loop waits on both sockets with the same bounded poll it uses
  today. The loop's read is also its pacing (`driver.rs:1082`), so the second
  read must not starve queue servicing. The Windows `WSAECONNRESET` handling
  (`driver.rs:24`) applies to both sockets.
- **Proved by** new tests, which fail if the slice is wrong:
  - **`IPV6_V6ONLY` is checked, not inferred.** Binding `127.0.0.1:P` and
    `[::1]:P` never conflicts whatever the option says, so it proves nothing.
    The test reads the option off the IPv6 socket, and separately binds the
    **wildcards** `0.0.0.0:P` and `[::]:P` together, the one combination that
    fails without it on Linux.
  - An anchor bound on both families serves a native str0m client over each
    **at the same time**. Each session's `selected_pair` reports the family it
    dialled, and application data flows on both.
  - **IPv4-only is unchanged in behaviour.** An anchor configured without IPv6
    offers exactly one host candidate, of the configured address, in both the
    session answer and the trickle frame. (Not "byte for byte": the answer
    carries per-session ICE credentials and a DTLS fingerprint, so no two
    answers are identical.)

### Slice 2: the CLI, the listener and the limiter

**Done 2026-09-28**, in three commits on `anchor-dual-stack`. What landed:

- **2a, the limiter.**
  - /64 keying, with IPv4-mapped sources unmapped first. Masking
    `::ffff:a.b.c.d` to /64 would have put every IPv4 client on a dual-stack
    listener in one bucket. This was found while writing it; the plan did not
    name it.
  - A 65,536-bucket bound and reclamation once per window. When full,
    reclamation runs at most once a second, so a flood of new sources cannot
    buy an O(n) sweep per request.
  - Refuse-when-full.
  - Two new `RtcStats` counters, reported by `anchor stats` as optional
    fields.
- **2b, the listeners.**
  - `BootstrapConfig::additional_bind_addrs`, served from one router.
  - `IPV6_V6ONLY` on IPv6 listeners whenever there is more than one.
  - `additional_acme_challenge_addrs`. This is new scope: a directory
    validates over IPv6 once the name has `AAAA`, so a v4-only challenge
    listener would fail issuance and renewal.
- **2c, the CLI and published addresses.**
  - Repeatable `--rtc-bind`/`--rtc-public-addr` (one per family),
    `--listen` and `--acme-challenge-addr`.
  - `rtc_addrs` on `GET /rtc/anchor` and in the serve report, under the
    serialization rule.
  - **Refinement:** `rtc_addrs` lists **published** endpoints only, never a
    bound one, because that is what `rtc_addr` already meant (the probe's
    only legitimate target).
- **2d, the leaf.** `AnchorInfo::rtc_addrs`, resolved once by the documented
  rule, and `check_ice_servers_against_peers` over every endpoint at the
  anchor-connect site.

**Evidence:**

- SDK:
  - 5 limiter unit tests, all green;
  - `rtc_bootstrap_listener`: 32 tests, including
    `a_dual_stack_listener_serves_both_families_from_one_state` and
    `the_anchor_endpoint_lists_every_published_family_only_when_dual_stack`.
- CLI: 3 new unit tests (9 in `anchor::`); `anchor_stats_live` and
  `remote_inspection`, 10 tests.
- Leaf: 2 new tests; 285 lib tests; `wasm32` check and clippy clean.

**Mutations run on the limiter:**

- Dropping the IPv4-mapped unmapping fails the mapped-source test.
- Disabling reclamation fails the expiry and table-full tests.

**Not witnessed:**

- The shared-state listener test was not mutation-run. It rests on each
  router state starting its dialog counter at 1.
- The extra ACME challenge listeners have no test. ACME is exercised only by
  the Pebble CI job, on one listener.

**Known limit, by design (Decision 2).** The leaf's own anchor is checked
against every published endpoint. A *different* anchor reached later as a
peer (`ice_servers_for`) is checked against its signed announcement, which
carries the primary `rtc_addr` only.

- Repeatable, per-family `--rtc-bind` / `--rtc-public-addr`, and repeatable
  `--listen` sharing one limiter state.
- `AnchorInfo.rtc_addrs` and `ServeReport.rtc_addrs`, under the
  serialization rule.
- The leaf learns every address and its STUN/peer collision guard checks all
  of them.
- The limiter rework: /64 key, hard bound, reclamation, refuse-when-full.
- **Proved by:**
  - limiter unit tests:
    - two addresses in one /64 share a bucket; two /64s do not; IPv4
      behaviour is unchanged;
    - expired buckets are reclaimed;
    - at the bound, with every bucket active, a new source is refused **and**
      every existing restriction still holds (a flood cannot erase one);
    - a table-full refusal is counted separately;
  - a listener test: a request over IPv4 and one over IPv6 draw from **one**
    budget;
  - collision-guard tests: a STUN entry naming the anchor's IPv6 address is
    refused exactly as one naming its IPv4 address is, including the
    equivalent-spelling cases the IPv4 guard already covers;
  - CLI tests in `cli/tests/`: mixed families in one pair, and two binds of
    one family, are both refused with typed errors;
  - `GET /rtc/anchor` on a dual-stack anchor lists both addresses; on an
    IPv4-only anchor it carries no `rtc_addrs` key and its other fields are
    unchanged;
  - `--inspect-target` reports both listeners.

### Slice 3: honest diagnostics

**Done 2026-09-29.** What landed:

- **Rust (leaf).**
  - `probe_event_answers` / `probe_verdict`: a reflexive candidate (the
    `typ` token then `srflx`, not a substring), or a STUN error below 700.
  - `stun_probe_outcomes`: every endpoint, concurrently, under one
    `STUN_PROBE_MS`. Empty or unbuildable probes are `NotRun`.
  - `classify_ice_failure_all`.
  - `UdpBlockedEvidence::{probed_all, for_endpoints}`.
  - The observation-not-cause `Display`.
  - `probed_all` across the leader/follower channel, with `[probed]` from
    an older sender.
  - `LeafEvent::Connected::rtc_addrs`, serialized only when there is more
    than one.
- **TypeScript.**
  - `probeEventAnswers` / `stunProbeAnswered` / `candidateLineIsReflexive`,
    used by the live probe.
  - `probeStunBindings` and `classifyRtcFailureAll`.
  - `UdpBlockedEvidence.probedAll` and `ConnectedEvent.rtcAddrs`.
  - The new message, formatted and parsed byte-identically to Rust.
  - `refineIceFailure` over every endpoint.
  - The existing exports are unchanged; the new ones are additive.
- **Harness.** The stage-5 browser runner matches the new prefix. This is a
  string change in a standalone crate that was not compiled locally.

**Evidence:**

- The shared vector file `browser-ts/test/fixtures/stun-probe-verdicts.json`
  has 14 cases, run by `leaf/tests/stun_probe_parity.rs` and by
  `classification.test.ts` ("the shared STUN probe rule").
- Classification tests on both sides cover the IPv6-only player (v4 silent,
  v6 answered → `ice-timeout`), both silent (→ `udp-blocked` naming both),
  and `notRun`/`unsupported`/`stunError`/empty/no-bootstrap (→
  `ice-timeout`).
- A TypeScript deadline test: 3 silent endpoints, a 150 ms deadline, done in
  under 300 ms.
- Leaf, with CI's exact command: 485 native tests (floor raised from 469 in
  the same commit), `wasm32 --all-targets` check, and clippy on both
  targets.
- `@net-mesh/browser`: 943 tests.

**Mutations, all caught:**

- The Rust rule ignoring STUN errors fails the parity test.
- Rust `all`→`any` fails the classifier test.
- The TypeScript rule ignoring STUN errors fails the vector cases.
- TypeScript `some`→`every` fails 4 classification tests.

**Defect found on the way:** slice 2d had broken a wasm-only test's
`AnchorInfo` literal (`tests/wasm_leaf.rs`), because that slice's
`wasm32` check covered `--lib` only. It was caught here by CI's
`--all-targets` command and fixed.

**Not witnessed:** the probes' behaviour in a real browser against a real
dual-stack anchor. That is slice 4.

- The leaf probes every published address under one deadline; `udp-blocked`
  requires every probe `unanswered`; `probedAll` is added; the message states
  the observation, not a cause.
- The Rust probe gains the STUN-error-response path (defect 3).
- **Proved by:**
  - mirrored Rust and TypeScript classification tests:
    - HTTPS OK, v4 unanswered, v6 answered → `ice-timeout`, **not**
      `udp-blocked` (the witness for defect 1);
    - HTTPS OK, both unanswered → `udp-blocked`, naming both addresses;
    - any probe `notRun`, `unsupported` or `malformed`, or an empty list →
      `ice-timeout`;
    - an anchor from 0.37 with only `rtc_addr` → today's behaviour, unchanged;
  - a probe-parity test: the Rust and TypeScript probes, fed the same
    observations (a reflexive candidate; a STUN error below 700; code 701; a
    silent deadline), return the same verdict;
  - a deadline test: with every address silent, the whole diagnosis finishes
    within the one bound, not the bound times the number of addresses.

### Slice 4: a real IPv6-only player

- **Where:** a natsim scenario that puts a browser leaf in an IPv6-only
  network namespace (`tests/natsim/browser/` already drives a browser inside
  natsim), alongside a v4-only leaf, against one dual-stack anchor.
- **Supported engines:** Chromium and Firefox, the engines the CI browser
  matrix runs. Safari/WebKit is not covered today, and the docs say so rather
  than leave it to inference.
- **Proved by, for each supported engine**, running permission-free (no
  prompts, no flags that grant what a real player's browser would not have):
  - both leaves connect, and the v6 leaf's selected pair is IPv6;
  - **the leaves really lack a common usable family:** the test asserts the
    v6 leaf has no IPv4 route and the v4 leaf has no IPv6 route, so a direct
    pair is impossible and a pass cannot come from one;
  - **application payloads observed by the receiver, in both directions,**
    not merely sent;
  - **the anchor's application-forwarding counters** rise in step with those
    payloads, correlated per direction, proving the anchor is the bridge;
  - **old leaf, new anchor:** a 0.37.1 leaf connects to the dual-stack
    anchor over IPv4 and exchanges payloads.
- **A native str0m pass is not a substitute for a browser pass.** If a
  supported engine cannot run the scenario, the slice is not done. The outcome
  is a **named support decision** recorded here, for example "Firefox is
  unsupported on IPv6-only networks in 0.38, because …", made deliberately.
  It is never an automatic fallback to whichever engine passed.

### Slice 5: docs and operators

- CLI reference, `concepts/webrtc-transport.md`, the browser quickstart and
  the "run an anchor" guide (prebuilt-binaries plan, slice 4) show the
  dual-stack flags and the need for an `AAAA` record.
- Hosting notes:
  - Fly.io's UDP service is IPv4-only, so an anchor there cannot serve
    IPv6-only players whatever the flags;
  - a plain VM (DigitalOcean Droplet and similar) needs IPv6 enabled on the
    interface.
- Release notes of 0.38.

## Risks

- **CI has no global IPv6.** Tests run on `::1` loopback and in network
  namespaces only. Some container environments disable IPv6 entirely; if the
  runner does, slices 1 and 4 need a job-level check that fails loudly rather
  than skipping.
- **Broken IPv6 on the client side.** A device with an IPv6 route that
  blackholes traffic still completes over IPv4. ICE checks every pair, so the
  cost is only extra checks. Nothing in this plan prefers a family; ICE
  nominates whichever pair works.
- **A /64 can be shared** (some hosting providers, campus networks). Those
  users share one bucket. That is the conservative failure: it throttles, it
  does not admit extra.
- **The announcement stays single-family.** A native peer reading
  `rtc_addr` sees only IPv4. This is deliberate (Decision 2) and must be said
  in the field's rustdoc, so nobody reads the announcement as the complete
  list.
- **Old leaves against a new anchor** see two candidates in the answer. That
  is standard ICE, and 0.37 leaves pass the SDP through to the browser, but
  slice 1's byte-for-byte check covers only the IPv4-only configuration. Run
  one 0.37.1 leaf against a dual-stack anchor as part of slice 4.

## Until this lands

For operators on 0.37:

- Keep the anchor on IPv4 and **do not add an `AAAA` record** for it yet. It
  does not help connectivity, because ICE still targets IPv4, and it turns
  failures into false `udp-blocked` claims (defect 1). It also exposes the
  /64 evasion (defect 2).
- If IPv6-only players matter before 0.38, a dual-stack TURN server passed in
  `connect({ iceServers })`, locked with `allowed-peer-ip` to relay only to
  the anchor, can carry them. Two conditions:
  - passing `iceServers` replaces the default, so the anchor's announced STUN
    endpoint must be listed again by hand;
  - this has not been tested against an anchor.

## Not in scope

- **An IPv6 STUN-only endpoint** (Stage 6's second socket, per family). IPv6
  hosts are usually globally addressed, so server-reflexive candidates matter
  less. It is a follow-up if slice 4 shows leaf-to-leaf pairs over IPv6
  failing where a reflexive candidate would have helped.
- **A second address in the signed announcement.** It comes back into scope
  when a native peer needs to dial an anchor over IPv6, or `anchor ls` needs
  to show both families from the directory.
- **TURN in the product.** The browser plan defers it to P5.
- **PROXY-protocol support** for hosts that terminate TCP in front of the
  anchor (the Fly.io per-IP problem). Related, but a separate change.

## Review (Kyra, 2026-09-28)

Kyra reviewed the first draft and agreed with the architecture: two explicit
sockets sharing one anchor, IPv6 opt-in with IPv4 defaults unchanged, the
signed announcement left alone, the v4-player-meets-v6-player witness as the
flagship, and the IPv6 STUN endpoint deferred behind a real evidence gate.
She corrected the acceptance criteria. Each claim was checked against the code
before the plan was amended:

1. **The limiter had no bound or pruning to extend.** The draft said the IPv6
   buckets would get "the same bound and pruning the IPv4 entries have". None
   exists (`rtc_bootstrap.rs:703-729`); the draft asserted it without looking.
   Now explicit scope in Decision 4 and slice 2: /64 key, hard bound,
   reclamation, refuse-when-full (with the trade-off recorded), and one shared
   limiter state across both listeners.
2. **"Every probe unanswered" is an observation, not a cause.** The draft
   treated it as proof of blocking. Decision 3 now reports it as "no UDP
   response from the advertised endpoints", and defines `notRun`,
   `unsupported`, `malformed` and an empty list as non-evidence, under one
   diagnostic deadline. Kyra also found the Rust/TypeScript probe disagreement
   (defect 3), which mirrored classification tests would not have caught.
3. **The STUN/peer collision guard must cover both addresses** and lands
   with them (Decision 2, slice 2), not as deferred cleanup.
4. **A native witness cannot stand in for the browser.** The draft let slice 4
   ship on whichever engine worked. It now requires each supported engine, or
   a named support decision, plus receiver-observed payloads both ways,
   correlated anchor forwarding counters, proof the leaves share no family,
   and the old-leaf case.
5. **Two test promises were wrong.** Binding `127.0.0.1:P` and `[::1]:P` does
   not prove `IPV6_V6ONLY`; only an explicit option check and the wildcard
   pair do. And a live session answer is never byte-identical (per-session ICE
   credentials, DTLS fingerprint). Both replaced in slice 1. The `rtc_addrs`
   serialization rule is now explicit (Decision 2).
