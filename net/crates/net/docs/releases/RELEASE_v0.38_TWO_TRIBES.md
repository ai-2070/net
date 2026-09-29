# Net v0.38 — "Two Tribes"

*Frankie Goes to Hollywood, 1984: two sides with nothing in common, meeting in one arena. An IPv4-only player and an IPv6-only player now meet through one anchor, and any game on any site can use it.*

## What's in it

v0.38 makes the browser anchor **dual-stack**. Until now, an anchor had one IPv4 RTC socket, so a player on an IPv6-only network could not reach it at all. Worse, that player was told `udp-blocked`, which was not true. An anchor can now have an RTC socket and a STUN socket in each family. It serves both kinds of player from one process, one identity and one relay.

The work is in five parts: the driver, the CLI and listener, honest diagnostics, a real IPv6-only browser in the network simulator, and docs. The full design and evidence are in [`ANCHOR_DUAL_STACK_PLAN.md`](../../../../../docs/internal/plans/ANCHOR_DUAL_STACK_PLAN.md).

v0.38 also adds **open games**: `anchor serve --open-games` turns an anchor into a public one. Any game on any site can use it, with no per-game setup by the operator.

---

## Dual-stack anchors

```sh
net-mesh anchor serve ... \
  --listen 0.0.0.0:443 --listen '[::]:443' \
  --acme-challenge-addr 0.0.0.0:80 --acme-challenge-addr '[::]:80' \
  --rtc-bind 0.0.0.0:7101 --rtc-bind '[::]:7101' \
  --rtc-public-addr 203.0.113.7:7101 --rtc-public-addr '[2001:db8::7]:7101' \
  --rtc-stun-bind 0.0.0.0:3478 --rtc-stun-bind '[::]:3478' \
  --rtc-stun-public-addr 203.0.113.7:3478 \
  --rtc-stun-public-addr '[2001:db8::7]:3478'
```

- **Two sockets per role.** The IPv4 socket is the primary. The IPv6 socket is bound `IPV6_V6ONLY`, so both can share a port number.
  - Every session offers one host candidate per family, and ICE picks whichever works.
  - A received datagram is stamped with the advertised address of the socket it arrived on.
  - A transmit leaves by the socket of its destination's family.
- **A STUN endpoint per family is required, not optional.** A browser with no camera or microphone permission (every data-only game) does not list its interfaces. On a given network, its only usable candidate is the server-reflexive one from a STUN server of that network's family. Without an IPv6 STUN endpoint, an IPv6-only Chromium player gathers nothing and never sends a check. The network simulator found this; unit tests could not have.
- **`--rtc-bind`, `--rtc-public-addr`, `--rtc-stun-bind` and `--rtc-stun-public-addr`** each take at most one address per family, in any order. One bind of either family is the single-socket anchor it always was.
- **`--listen` and `--acme-challenge-addr` are repeatable.** Every listener serves one router state, so the rate limits are one budget whichever family a request arrives on. An ACME directory validates over IPv6 once the name has an `AAAA` record, so an anchor that publishes one needs an IPv6 challenge listener too.
- **`GET /rtc/anchor` gains `rtc_addrs` and `stun_addrs`**, listing every published endpoint, primary first. They appear only when there is more than one family, so a single-stack anchor's output is byte-for-byte unchanged. The browser SDK's default ICE servers use every STUN endpoint.
- **The start report and `--inspect-target`** show the extra listeners and the IPv6 sockets (`also_listening_on`, `rtc_addrs`, `listen_additional`, `rtc_bind_v6`, …).
- **The anchor trickles one candidate frame per family.**
- **Configuration conflicts are refused at startup**: two binds of one family, a public address whose family has no socket, or an IPv6 STUN endpoint equal to the IPv6 RTC endpoint.

---

## Open games: a public anchor

Until now, an anchor admitted only the games its operator listed (`--game`), and only from the sites listed with `--allow-origin`. `--open-games <state-file>` lifts both:

```sh
net-mesh anchor serve --psk-file psk.hex --url https://anchor.example.com \
  --issuer-identity issuer.json --open-games /var/lib/net-anchor/open-games.txt \
  --rtc-bind 0.0.0.0:7101 --rtc-public-addr 203.0.113.7:7101
```

A page calls `requestCredential({ anchorUrl, game })` with any game id and gets a credential, the same as for a registered game.

- **Each site gets its own games.** An open game is keyed on the page's `Origin` **and** the game id. Its root and tenant are derived from the issuer key under their own domain separation, the same stateless way registered games are. Two sites that both call their game `chess` get two isolated games, with separate roots, lobbies and discovery.
- **Registered games stay the operator's.** `--game` and `--allow-origin` work beside open games. A listed origin asking for a registered game gets that game. Any other origin naming it gets its own open game of the same name, never the registered one.
- **Any origin may call the endpoints, in open mode only.** CORS echoes the caller's origin: never `*`, never with credentials allowed. Nothing on the anchor is ambient authority (no cookies, no HTTP authentication), so a page can only spend a credential it fetched itself. The trickle socket still requires an `Origin` and the attempt token.
- **Bounded:**
  - `--open-games-max` sets how many open games are held at once (default 8192). A new game is refused while the table is full, until one has had no credential issued for the invite lifetime. A game in use is never evicted.
  - `--open-games-per-minute` is one issuance ceiling across all open games (default 9000), on top of each game's own 600, so inventing game names cannot multiply the budget.
  - `--open-game-max-players` optionally caps one game's connected players across all its lobbies and matches. By default there is no cap, and one game may use the whole anchor.
- **Survives restarts.** The state file lists the open games held, one `<origin> <game>` line each. A restarted anchor therefore still admits a page reconnecting with a credential issued before the restart. The file holds no secrets.
- **Counters.** `--game-stats-secs` lines carry each open game's `origin`, plus an `open_games` summary (games held, capacity, state-file write errors).

**`--rtc-max-peers`** is new too. It is the number of browser sessions the anchor holds at once, whatever their game. `anchor serve` now defaults to 1024 (the core's own default stays 256), and the flag lets a larger host raise it.

A public anchor relays for players whose networks cannot connect directly, and that bandwidth is the operator's. The session and player caps bound how many players an anchor serves, not how many bytes it forwards.

Tests: 6 registry tests cover keying, refusals, capacity with idle reclamation, the shared ceiling, restart through the state file, and the player cap. 2 end-to-end tests against the real HTTP listener are pinned in CI, plus CLI parse and default tests.

---

## Prebuilt anchor binaries

Until now an anchor meant `cargo install net-cli --features rtc-bootstrap`: a Rust toolchain, cmake and a C compiler. That was a steep first step for a game developer. From v0.38 the GitHub release carries the anchor prebuilt, beside the regular CLI archives:

- `net-mesh-anchor-v0.38.0-x86_64-unknown-linux-gnu.tar.gz`, for a server. It is built on Ubuntu 22.04 and runs on any host with glibc 2.35 or newer.
- `net-mesh-anchor-v0.38.0-x86_64-pc-windows-msvc.zip`, for a local anchor while developing.

The binary inside is the same `net-mesh`, built with the anchor feature. The regular `net-cli-v…` archives are unchanged: they still carry no WebRTC stack and no HTTPS server. Both kinds come from the same tag in the same workflow run, so a page and its anchor are always one release. Each anchor archive is smoke-tested before it is attached: `anchor serve` must exist and carry this release's flags. Other platforms still build from source. This is shape B of [`ANCHOR_PREBUILT_BINARIES_PLAN.md`](../../../../../docs/internal/plans/ANCHOR_PREBUILT_BINARIES_PLAN.md), for two targets. The container image and the remaining targets are still to come.

---

## Rate limiting by /64

- An IPv6 client is charged by its **/64 prefix**, since one subscriber can rotate through a whole /64. IPv4 is charged per address, and IPv4-mapped IPv6 addresses are unmapped first.
- The limiter holds at most 65,536 sources and reclaims idle ones. When it is full, a **new source is refused; an existing restriction is never evicted**, so flooding the table cannot erase a limit.
- `anchor stats` reports `bootstrap_rate_limited` and `bootstrap_rate_table_full`.

---

## `udp-blocked` is stricter and says less

The old probe checked one endpoint. On a dual-stack anchor, an IPv6-only player's IPv4 probe goes unanswered, and that alone would have been reported as blocked UDP.

- The leaf now probes **every** endpoint the anchor published, concurrently, under **one deadline**.
- It reports `udp-blocked` only when the HTTPS bootstrap succeeded **and every probe went unanswered**. One answered family, a probe that never ran, or no endpoints stays `ice-timeout`.
- **A probe that never ran reads `notRun`, not `unanswered`.** If the engine fails the probe connection before gathering a single candidate, nothing was sent, so nothing went unanswered. This was the path by which a browser that could not gather at all was told UDP was blocked.
- **The message states the observation, not a cause:** `no UDP response from the anchor's advertised endpoints: its HTTPS bootstrap succeeded but STUN bindings to … went unanswered`. Blocked UDP, a stopped UDP listener, a wrong advertised address and loss all look the same from the page. The `kind` is unchanged. Match on `kind`, never on the message.
- **The Rust (wasm) and TypeScript probes agree** on what "answered" means: a server-reflexive candidate, or a STUN error response (code below 700). A shared vector file (`browser-ts/test/fixtures/stun-probe-verdicts.json`) holds both to the same verdicts. The wasm probe used to ignore error responses.
- New in `@net-mesh/browser`:
  - `UdpBlockedEvidence.probedAll` and `ConnectedEvent.rtcAddrs`;
  - `classifyRtcFailureAll`, `probeStunBindings`, `probeEventAnswers`, `stunProbeAnswered` and `candidateLineIsReflexive`;
  - the `EndpointProbe` and `ProbeEvent` types.

---

## Proved in real browsers on real IPv6-only networks

The network simulator (natsim: Linux network namespaces and nftables) gains IPv6. New modes:

- an **IPv4-only** player;
- an **IPv6-only** player behind a routing gateway with a stateful firewall and no IPv4 at all;
- an IPv6-only player on a **464XLAT** network: a CLAT on the device and a NAT64 on its gateway, both tayga, as mobile IPv6-only networks are deployed.

Three rows, each run permission-free:

| Row | Engines | Result |
|---|---|---|
| IPv4-only player meets IPv6-only player | Chromium | **Relayed through the anchor.** Payloads were observed in both directions, the anchor's forwarding counters rose in step, and each player's selected pair was on its own family. |
| Pure IPv6-only player | Firefox | **Unreachable, pinned.** B's connect fails typed `ice-timeout`, never `udp-blocked`. |
| IPv6-only player on 464XLAT | Firefox | **Direct.** |

Each row checks the address families from the network namespaces themselves. A pass therefore cannot come from a pair the families would not allow.

**Support decision: Firefox on a pure IPv6-only network is unsupported.** Without a media permission, Firefox looks for its default local address over IPv4 only. Its own ICE log shows it failing, then gathering nothing, even though it had seen the IPv6 address. A page cannot change that, and asking for a microphone to play a data-only game is not a fix. The row is pinned rather than dropped, so a Firefox that starts working fails it loudly. Mobile IPv6-only networks give the device an IPv4 route through 464XLAT, and there Firefox connects. Chromium works on both. Safari/WebKit is not covered by the matrix.

---

## Docs

- The CLI reference has a new **"Serve IPv4 and IPv6 players"** section. It covers the flags, DNS, `rtc_addrs` and `stun_addrs`, the /64 limits, the Firefox decision, and "no `AAAA` record until the anchor is dual-stack".
- Hosting notes:
  - a plain VM;
  - a DigitalOcean Reserved IP;
  - Fly.io, whose UDP is IPv4-only and whose proxy hides client addresses;
  - platforms without UDP.
- The transport concepts page, the browser errors page and the `net-browser` / `net-event-bus` skills describe the every-endpoint rule and the new fields.

---

## Version bump

`0.37.1 → 0.38.0`, applied to:

- every manifest: crate, wire, leaf, CLI, deck, SDK, payments, and the Go, Node and Python bindings;
- the `@net-mesh/*` pins and the `net-mesh` Python bound (now `>=0.38.0,<0.39.0`);
- the skills' `net-version`;
- the Hermes integration pin;
- the lockfiles.

---

## Breaking changes

None for code. A dual-stack anchor's `udp-blocked` message text changed, which matters only to code that parsed it, and it never should have. Single-stack anchors behave and serialize exactly as before.

Two SDK signatures moved, both on `net_sdk::game_anchor::GameRegistry`:

- `root_of` and `game_of_root` now return owned values (`Option<EntityId>`, `Option<String>`) instead of references, since the game table can now grow at run time.
- `games()` returns `Vec<String>`.

`anchor serve` holds up to 1024 browser sessions by default, up from 256.

---

## How to upgrade

Bump to 0.38.0 and rebuild. To serve IPv6-only players:

1. Give the anchor an `--rtc-bind` **and** an `--rtc-stun-bind` in each family, plus the matching public addresses.
2. Add an IPv6 `--listen` and an IPv6 `--acme-challenge-addr`.
3. Then publish an `AAAA` record.

To run an anchor, download `net-mesh-anchor-v0.38.0-*` from the release. To run a public one, add `--open-games <state-file>` (and drop `--allow-origin` if you list no registered games).

Pages and anchors ship together: upgrade `@net-mesh/browser` with the anchor to get the every-endpoint probe.

---

Released 2026-09-29.

## License

See [LICENSE](../../LICENSE-APACHE).
