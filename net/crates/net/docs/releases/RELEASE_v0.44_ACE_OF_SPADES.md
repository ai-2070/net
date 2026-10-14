# Net v0.44 — "Ace of Spades"

*Motörhead, 1980. v0.43 armoured the SDKs; v0.44 deals the browser a bigger table. A match hosted by its own players now holds 128 of them on one host, or splits across several hosts, and the anchor keeps every game to itself.*

## What's in it

- **Sharded matches in the browser.** `joinShards` merges one view over several shard hosts, with a match assignment and leases in the lobby store, cross-host hits, planned moves between hosts, recovery from a lost host, and NPCs that cross hosts or follow paths.
- **Compact netcode snapshots,** about 7× smaller than JSON when both ends share a codec.
- **One host past 64 players.** Measurement found the faults below, and fixing them took one host to 128 players: every player joined, every pair direct, 60 fps, on one machine.
- **Security:** a browser session that never enrolled no longer sees any game's announcements, and a departed host's announcement no longer leaks into other games.
- **The anchor** charges offers per visitor, so players behind one shared address can all join, and it catches each newcomer up on its game's announcements.
- **Breaking, in a few places:** the anchor's rate-limit defaults, `joinNetcode().ready()` now rejecting, and the 30 s announcement refresh. See "Breaking changes".

---

## Security

Two leaks between games on one anchor, found while adding announcement replay. Both are fixed in the core mesh, which the anchor runs:

- **A session that never enrolled saw every game.** A browser session that completed the handshake but never enrolled had no tenant, and "no tenant" read as the native "meets everyone". The live flood sent it every game's announcements, and the keys in them, for as long as it stayed unenrolled. Such a session now sees only untenanted announcements (`rtc::announcement_may_reach`); its own game's are replayed to it when enrollment promotes it.
- **A departed host's announcement crossed games.** Replay looked an announcement's game up in the live peer table, so once its host disconnected, an unexpired announcement resolved to "no tenant" and was replayed to every game's players. A held announcement now records its game when it is held.

---

## The anchor

`net-mesh anchor serve` and the `net-mesh-sdk` RTC bootstrap listener:

- **Visitors behind one shared address can all join.** Offers are charged per visitor, per (source IP, credential): 10 a minute by default, set with the new `--offers-per-credential-per-minute`. `--offers-per-minute` becomes the per-address backstop across all of them, and its default rises from 10 to 120. `--credentials-per-minute` rises from 30 to 120; the per-game issuance ceiling is unchanged. Before, the eleventh player behind one carrier NAT, campus or LAN party was refused `rate_limited`. A credential every visitor shares (`anchor credential mint`) is still charged per address, as before.
- One address holds at most 32 per-visitor budgets. A visitor with no room for one is admitted on the per-address backstop rather than refused, and counted as `bootstrap_visitor_budget_degraded` in `anchor stats`.
- **A newcomer is caught up.** When enrollment promotes a session, the anchor replays the announcements it holds for that session's game. Before, a lobby announced before a player arrived stayed invisible to it until the host announced again.

---

## The browser

### Sharded matches (`@net-mesh/browser/netcode`)

- **`joinShards`** runs one `joinNetcode` per shard host and draws each entity only from the shard that owns it in the installed `ShardAssignment` (`{ epoch, hosts, owner }`). Inputs go only to the shard that owns the player. A shard at another epoch is frozen at its last drawn state, minus anything it no longer owns, and the old owner's copy lingers for `lingerMs` after an owner change.
- **An optional epoch on the netcode wire:** `hostNetcode({ epoch })` stamps `ep` on every snapshot and drops input frames from another epoch. Frames without `ep` are unchanged.
- **The assignment and leases:** `MatchAssignment` in the lobby store, with `assignmentProblems`, `assignAction({ lobby })`, `renewLeases`, `leaseWatch`, `shardAssignment` and `authorizeTeams`. `hostNetcode({ live })` stops once its lease is lost; `joinShards({ live })` freezes.
- **Cross-host hits:** `NetcodeClient.sampleAt(hostTime)` reads history, never extrapolated, so a shard host can check a hit claim against where the shooter really was. The rewind cap is 500 ms.
- **Planned moves:** `releasePlayer` / `adoptPlayer` on `hostNetcode`, and `createInputSequencer()` shared across clients. An input the old host applied is never applied again.
- **A lost host:** `NetcodeClient.latest()` and `adoptPlayer(peer, { resume: true })` restore a lost host's players from the survivor's last observation.
- **NPCs:** an NPC is owned by one shard and its AI runs there. An NPC on a known route can be published once, as an `NpcPath` record in the lobby store (`pathAction`, `npcModes().resolve`, `pathAt`), instead of in every snapshot: with 50 NPCs, 47 KB/s live against 1.6 KB/s on paths, per viewer.
- **The shard host recipe** (`browser-ts/demo/sharded-arena/`) wires one shard host from these pieces, and runs 4 hosts and 32 bots on the local mesh.

### Netcode

- **Compact snapshots.** `hostNetcode` and `joinNetcode` take an optional `codec` built with `entityCodec({ x: quantized(0, 512), yaw: angle(), hp: uint(8), … })`. With the same codec on both ends, snapshots are binary and about 7× smaller; otherwise they stay JSON, so a page without a codec works unchanged.
- **`joinNetcode().ready()` says why it has not joined.** It rejects with a typed `NetcodeError` after `joinTimeoutMs` (15 s by default): `no-announcement`, `unreachable`, `timeout` or `closed`. It used to wait silently forever. `stats()` gains `connectOutcome` and `lastError`.
- **A relayed pair is no longer re-offered on every reopen.** Each stream reopen started a fresh ICE attempt, a loop that under load never delivered a snapshot. Direct is now retried at most every `directRetryMs` (30 s).
- **Fewer clock pings** once the clock has settled: 4/s until 8 samples, then 1/s. Host upload at 1 host × 32 players fell from 2.19 to 1.82 Mbit/s.
- **A backlog of unacknowledged inputs is sent whole,** up to 64; before, everything older than the newest 8 was lost.

### Transport and the leaf

- **A lossy stream no longer wedges after loss** (`net-mesh-wire`). On a fire-and-forget stream a lost packet's bytes were never credited back, so after 64 KiB of loss the stream refused every send, and a reopen inherited the drained state. Lost credit is now written off; reliable streams are unchanged.
- **A peer pair can no longer end half-direct.** When a slow answerer's reply crossed the offerer's deadline, the offerer fell back to the relay while the answerer stayed `direct` and sent on a closed channel, for good. An answerer is now direct only once the establishment is proven, and an abandoned one returns the pair to its relay.
- **A direct link one side no longer recognises now recovers.** A host that held no session for a player still sending over an open channel dropped every packet, and nothing noticed. The side without a session now closes the link, the other side treats that as a lost session, and connects again. `retryReport()` gains `channelClosed`.
- **Stream data is routed to its stream.** Every stream's listener used to read every node event to find its own: 13% of a host's main thread at 64 players. Events are now routed by peer and stream id inside the wasm.
- **Lobbies, joiners and region hosts refresh their announcements every 30 s, not 2 s.** At 64 players the 2 s refresh pinned the host's anchor link and starved every relayed pair. A change of players is still announced at once, and the anchor now catches newcomers up (above). `joinLobby` gains `announceEveryMs`.

### What one host holds

Real Chrome, a real `anchor serve`, one machine (`examples/shard-topology/`):

| 1 host × N | Joined, direct | fps | Tick p95 | Wire upload, mean / max |
|---|---|---|---|---|
| 96 (4 runs) | 96/96 | 60 | 8.2–9.2 ms | 6.89–6.95 / 7.14 Mbit/s |
| 128 (2 runs) | 128/128 | 60 | 14.3–16.8 ms | 9.10–9.72 / 9.96 Mbit/s |

Upload, not CPU, is the ceiling: at 128 players the peak reaches a 10 Mbit/s budget. Past that, shard the match.

---

## Smaller changes

- The web site has a `/games` page: Net for browser games, with the demo.
- The web site builds with Bun 1.4.3 (`bun run check`, `bun run sync:releases`); Node 24 still runs `next`, `tsc` and the check scripts.
- `cli/tests/org_join.rs` no longer flakes under CI load.
- `npm run bench:netcode` in `browser-ts` measures host CPU and upload, JSON against compact.
- Dependency updates, including `sha2`, `schemars`, `syn`, `keyring-core` and the web site's packages.

---

## Version bump

Everything published moves to **0.44.0**:

- every manifest: crate, wire, leaf, CLI, deck, SDK, payments, and the Go, Node and Python bindings;
- the `@net-mesh/*` pins and the `net-mesh` Python bound (now `>=0.44.0,<0.45.0`);
- the skills' `net-version`;
- the Hermes integration pin;
- the lockfiles.

---

## Breaking changes

Nothing changed on the mesh wire. The netcode wire only gained optional frames and fields, which a 0.43 peer ignores.

**The anchor**

- `--offers-per-minute` is now a per-address backstop (default 120, was 10). The per-visitor limit is the new `--offers-per-credential-per-minute` (default 10). `--credentials-per-minute` defaults to 120 (was 30). An anchor that set `--offers-per-minute` to cap each player now caps the whole address.
- A session that has not enrolled no longer receives game announcements.

**The browser**

- `joinNetcode().ready()` rejects with a `NetcodeError` after `joinTimeoutMs` (15 s). It used to stay pending until the first snapshot. The client keeps retrying after the rejection.
- Lobby, joiner and region announcements refresh every 30 s. A newcomer relies on the anchor's replay to see what was announced before it arrived, and an anchor older than 0.44 does not replay.

---

## How to upgrade

Bump to 0.44.0 and rebuild.

- **Anchors first.** Upgrade `anchor serve` with the pages that use it: a 0.44 page refreshes its lobby every 30 s and relies on the anchor to catch newcomers up. If you set `--offers-per-minute`, read it again as a per-address figure and set `--offers-per-credential-per-minute` for each player.
- **Browser:** handle a rejected `await net.ready()`. `error.kind === 'no-announcement'` means this node never heard the host announce. A page that awaited `ready()` indefinitely gets the rejection after 15 s, or after `joinTimeoutMs` if set.
- **Mixed versions:** a 0.43 peer has no codec, so it plays a 0.44 peer on JSON snapshots; upgrade both ends to get the binary codec.

---

Released 2026-10-13.

## License

See [LICENSE](../../LICENSE-APACHE).
