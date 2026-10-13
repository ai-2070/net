# CODE REVIEW 2026-10-13 — sharded-match branch (`LZL0/threejs-scale`, PR #1252)

**Scope:** the branch diff `master...e6d1474fd`. 32 commits, 59 files,
+9802/−239. Covers the leaf (`leaf/src/`), the wire core
(`wire/src/session.rs`), the anchor (`sdk/src/rtc_bootstrap.rs`,
`cli/src/commands/anchor.rs`) and the browser netcode
(`browser-ts/src/netcode/`, `src/lobby.ts`, `src/world/regions.ts`,
`demo/sharded-arena/`), through N2 (NPCs on paths).

**Method:** the coordinator reviewed the wire and anchor changes directly.
Three parallel lanes covered the rest: LEAF (`leaf/src/`), NET
(netcode core: codec, client, host, wire, interpolate, lobby, regions) and
SHARD (`shards.ts`, `assignment.ts`, `paths.ts`, `demo/sharded-arena/`).
The coordinator re-read LEAF-1, SHARD-1 and NET-1 in source at `e6d1474fd`;
all hold.

**Receipts:** `npx vitest run test/netcode` passes (93 tests, 13 files).
No `cargo` / wasm / CI-script run backed this pass. Findings marked
CONFIRMED were re-read in source by the coordinator; TRACED findings were
traced by a lane and not independently re-read. No finding below has a
test that covers it.

**Verdict: HOLD.** 4 must-fix (LEAF-1, LEAF-2, SHARD-1, NET-1): two of them
(LEAF-1, LEAF-2) reopen the half-direct and ghost-session failure modes the
PR set out to close.

---

## Must-fix

### LEAF-1 — the answerer's deadline only waits for the proof while the channel is open (CONFIRMED)

`leaf/src/wasm.rs:3612-3636`. The new branch that waits out a pending proof
requires `open && admissions[peer] == attempt`. Otherwise it falls through
to `settle_peer_deadline`, and `settle` takes its destructive branch
(`drop_session` of the routed session).

**Scenario:** a slow answerer handles message 1 late. The offerer passes its
deadline, settles `failed`, closes its channel and keeps its routed session.
On the answerer `is_open` is now false. Its attempt deadline then expires
inside the 5 s `PROOF_DEADLINE_MS` window, before
`retire_unproven_admissions` runs.

**Result:** the answerer drops the routed session. The offerer keeps sending
relayed packets, which the answerer refuses as `no_session`. The orphan
close doesn't fire, because it only acts on direct packets, so the pair stays
broken. The STUN probe also mislabels the attempt `iceTimeout` /
`udpBlocked`, although ICE connected. The witness covers only the
open-channel path.

### LEAF-2 — the ghost-session close can kill this leaf's own fresh re-attempt (TRACED)

`leaf/src/wasm.rs:1238` (`orphaned`), `:1216` (`close_orphaned_link`).
`orphaned` exempts a peer that is handshaking or has a provisional or pending
verified admission. It doesn't exempt an attempt this leaf offered whose
channel is open but whose `peer_handshake` hasn't started.

**Scenario:** loss detection is asymmetric. A sees ICE `failed` first, drops
its session and re-offers. B accepts; `accept_offer`'s insert replaces B's
old link, whose `Drop` detaches its close handler and ICE watcher, so B
keeps its old direct session and never learns the old channel died. B's
20 Hz traffic under the old session rides the new channel (the transport
sends by peer). A has no session and no handshake yet, so it settles its own
re-attempt `Failed` and closes. B's close handler names the new channel,
but its `channel_installations` entry names the old one, so
`drop_installed_over` drops nothing and files no trigger.

**Result:** B holds a ghost session indefinitely, and A's later attempts can
lose the same race. Before this PR, A refused those packets and finished the
handshake.

### SHARD-1 — the recipe uses two clocks, so cross-host hits always fail outside the tests (CONFIRMED)

`browser-ts/demo/sharded-arena/shard.js:82`. `now` defaults to
`Date.now`, but `hostNetcode`, the spectator `joinNetcode`s and `npcModes`
default to `performance.now()` (`wire.ts:237` `defaultNow`, `paths.ts:167`).

- `hitAction` (`:219-221`): `at` is `Date.now()` (~1.7e12) while
  `claim.seenB` is on the performance clock, so every forwarded hit is
  refused `beyond-cap`.
- `npcFire` (`:184`): `seenA` is on `Date.now`, so `netcode.rewind(seenA)`
  rewinds against the wrong timeline.
- The path driver (`:274-277`) evaluates `pathAt(points, Date.now())`, while
  clients evaluate the same path at `hostNow()`. Host and clients put the NPC
  in different places.

Every test passes `now: () => Date.now()`, which hides it. `main.js` passes
no `now` but uses no combat or paths. **Fix:** default to `defaultNow`.

### NET-1 — a client without `local` sends 64-input frames forever (CONFIRMED)

`browser-ts/src/netcode/client.ts:335`, trim at `:594`. `inputs.pending` is
only trimmed on ack inside `if (options.local && …)`. On master it was never
trimmed either, but every send was capped by `slice(-8)`. Now `sendInputs`
switches to `MAX_INPUT_BACKLOG` (64) once more than `inputRedundancy` inputs
are pending, and never switches back.

**Result:** a non-predicting client, or a `joinShards` client that doesn't
own the player, sends 64 inputs on every `input()` call and every redundancy
beat. With large input data the frame can pass the transport's frame limit;
the failed send closes the stream, along with its pings, joins and `w`
frames. **Fix:** trim pending on ack regardless of `local`.

---

## Should-fix

### SHARD-2 — path NPCs fall out of the rewind history (TRACED)

`shard.js:286-291` with `host.ts:496-498`. History records `snapshot()`,
which stops sending a path NPC after `announceMs`. After that:

- shots at it miss locally, and cross-host claims on it are refused
  `no-history` (`:230`);
- its own shots are refused at the shooter's host
  (`refused-here:no-history`, `:148`);
- if its host is lost, the `setAssignment` restore (`:387`) can't find it and
  counts it `unrecovered`, while its lobby record still says `path`. Clients
  keep drawing a ghost walking its route and then standing at the last point.
  The same ghost appears when a path NPC dies.

### WIRE-1 — the write-off only fires on a full stall, not on steady loss (CONFIRMED)

`wire/src/session.rs`, `reclaim_lost_fire_and_forget_credit`. Every grant
that advances `max_consumed_seen` restamps `tx_progress_ns`. As long as any
grant arrives within `FIRE_AND_FORGET_RECLAIM_AFTER` (2 s), lost bytes are
never written off. A lossy stream still drains its whole window, freezes for
2 s, then recovers, and repeats: at 1–5 % loss on a 64 KiB window, a 2 s
blackout every few minutes on a game stream.

**Suggested fix:** record `(tx_bytes_sent, t)` checkpoints. At
`t + horizon`, write off
`max(0, sent_at_checkpoint − consumed_now − already_written_off)`. That is
sound because every byte sent by `t` has had the full horizon to arrive, and
it works while traffic flows.

### WIRE-2 — wrongly written-off bytes add up, contrary to the doc comment (CONFIRMED)

Same function, and `apply_authoritative_grant`. Grants are lossy (a dropped
grant is subsumed by the next one). If grants are lost on the reverse path
for 2 s while data still arrives, bytes the receiver actually consumed are
written off. Every later grant is then overstated by `tx_written_off`, and
each such event adds to it, so the sender can drift arbitrarily far past
the window. The doc says it is "at worst one write-off ahead of the wire —
bounded". The receiver doesn't enforce the window, so the effect is weaker
backpressure, not drops. Either fix the comment or bound the drift, for
example by reducing `tx_written_off` when a grant overshoots the sent
watermark.

### WIRE-3 — concurrent write-off double-counts; wall-clock horizon (CONFIRMED)

Two threads in `try_acquire_tx_credit` can both pass the horizon check and
both `fetch_add(outstanding)`, double-counting `tx_written_off` (the same
inflation as WIRE-2). A CAS on `tx_progress_ns` before the `fetch_add` would
make the write-off single-winner. Separately, `current_timestamp` is wall
clock (`SystemTime`): a backward clock step holds off the horizon for that
long, and a forward step writes off at once.

### LEAF-3 — "keeps the routed session" fails once the offerer has received message 2 (TRACED)

`leaf/src/wasm.rs:1295-1314`. The initiator installs its session on
message 2, replacing its routed keys. If the proof is late or lost,
`retire_unproven_admissions` restores the relay over the answerer's old
routed keys, which the offerer no longer holds, settles `Relayed` and closes
the channel. The offerer's close handler then drops its direct session. A
relayed message 1 is refused while `has_session(from)`, so where UDP is
blocked the pair stays dead.

### ANCHOR-1 — the per-credential table fills about 120× faster than the per-IP table (CONFIRMED)

`sdk/src/rtc_bootstrap.rs`, `credential_rate`. It shares
`RATE_MAX_BUCKETS` (65,536), but one source prefix can create up to 120
`(prefix, nonce)` entries a minute (bounded by the per-IP backstop) instead
of 1. About 546 IPv6 /64s (under a /54) plus 120 credentials from an open
game (`DEFAULT_ISSUE_PER_MINUTE` = 600) fill it, and every new visitor
anchor-wide is then refused `TableFull`. Before, it took a /48. The three
pinned CI witnesses don't cover this. **Fix:** cap entries per source
prefix in the credential table.

The rest of the anchor change holds: the per-pair charge comes after the
issuer signature verifies, the per-IP check runs first, and
`--offers-per-credential-per-minute` is wired through.

### NET-2 — a restarted host can show entities under the wrong ids (TRACED)

`client.ts:520-526` with `host.ts:338-349, 392-397`. Indexes are never
reused only within one `hostNetcode` instance. If the host re-creates
`hostNetcode` with the same label while the client lives on, the client sees
an index conflict and drops the frame without sending `u`. The host has
already marked those names sent, so later frames carry no names and decode
under the client's old map: entity data drawn under the wrong id, with no
repair. **Fix:** a per-instance nonce in the frame, or clear `idOf` on a
conflict.

### NET-3 — the 30 s announce cadence also delays retrying a failed announcement (TRACED)

`lobby.ts:438-441, 606-613`, `regions.ts:87-91`. A change of players is
still announced at once (`lobby.ts:451-461`). But failed announcements are
swallowed and retried on the next tick, now 30 s away. If a `joinLobby`
seek announcement fails, `connectPeer` gives up at the 20 s `timeoutMs` with
`LobbyError('not-found')` before the re-announce. Same for a lobby's first
announcement and for region hosts. **Fix:** retry a failed announcement on a
short backoff, separate from the lease-refresh interval.

### SHARD-3 — combat timing also applies to planned moves (TRACED)

`shard.js:336`. With combat on, `timing: { retryMs: 50, giveUpMs: 500,
handledRetentionMs: 10_000 }` goes to the single `regionHandoffs` instance,
which uses it for entity handoffs too (`link.ts:495-496`). A move unanswered
for 500 ms leaves the player frozen and live nowhere. A re-offer after 10 s
with the same id can be admitted a second time, breaking "never live on two
hosts". Hit forwards and moves need separate timing.

### SHARD-4 — a refused path write is never rolled back (TRACED)

`shard.js:404-412`. `routes.set` and the `mv` bump happen before
`options.paths.write(...)`, and its rejection is ignored. If `pathAction`
refuses, the host still stops sending the NPC after `announceMs`; clients
hold it frozen while the host keeps moving it.

### SHARD-5 — every rejoined shard blinks out in `joinShards` (TRACED)

`shards.ts:288-297`. When this player's owner or a host changes, the old and
new owner shards are rejoined with `drawn: {}`, and the new client's
`view()` is empty until its first snapshot. Lingering covers only ids whose
owner changed, so on every planned move all other entities on both shards
vanish for about one join round trip. `ready()` also keeps referring to the
closed clients.

### SHARD-6 — the lease bound ignores delivery delay (TRACED)

`assignment.ts:182, 209-229`. `leaseWatch` starts its timer when the counter
changes on receipt; `mayReassign` measures from the lobby's renewal plus a
clock-drift margin. A renewal delivered `d` ms late keeps the old host live
until `T + d + leaseMs`, past the lobby's reassignment at
`T + leaseMs + drift` when `d > drift`. The first sighting of a stale
replica value also grants a full lease. The margin has to cover replication
delay, not only drift.

### NET-4 — an adoption is lost if the player is slow to arrive (TRACED)

`host.ts:479-484` with `:563-574`. `adoptPlayer` stamps `lastHeard = now()`.
If the moved player's first frame takes longer than `playerTimeoutMs`
(5 s), the record is deleted with its `lastApplied` boundary and `resuming`
flag. The next frame creates a fresh player with `lastApplied = 0`: a
planned move re-applies inputs at or below the boundary; a resume applies
inputs made against the rolled-back state. If `authorize` refuses the peer,
`adoptPlayer` also drops the adoption silently.

### NET-5 — `connectPeer` rejecting skips the direct-retry throttle (TRACED)

`client.ts:361-365, 375-376`. A peer that answers `Reject` makes
`connectPeer` reject rather than return an outcome, so `connectOutcome`
stays `null` and `shouldOffer()` is always true: every stream reopen offers
again, the loop `directRetryMs` was added to stop.

---

## Low

- **NET-6** `angle()` fields interpolate the long way round across 0 / 2π
  (`codec.ts:129-133`, `interpolate.ts:28-31`), including in the correction
  blend after a reconcile. The module's own example uses `yaw: angle()`.
- **NET-7** A peer can force names to be re-sent on every tick by spamming
  `u` frames (`host.ts:423-425`). Bounded by `MAX_NAME_REQUESTS` and that
  peer's view; hurts only that peer's frames.
- **NET-8** Id tables never shrink (`host.ts:338-349`, per-player `named`,
  client `idOf`): transient unique ids (projectiles) grow them for the whole
  match.
- **NET-9** The host accepts non-safe-integer seqs (`Number.isInteger`). A
  seq like `1e300` becomes `lastApplied`, encodes as an oversize varint that
  `getVarint` refuses, and that player gets no binary snapshots.
- **NET-10** `Math.max(...pending.keys())` (`host.ts:472`) throws past ~100k
  entries and would wedge the tick. Not realistic within one tick.
- **NET-11** `float32()` can deliver NaN / Infinity; the decoder doesn't
  check finiteness. Matters only with an untrusted host.
- **NET-12** `SnapshotBuffer.sample` at an exact interior snapshot time drops
  entities that first appear in that snapshot.
- **SHARD-7** `shardHost.setAssignment` (`shard.js:372-375`) doesn't
  validate the epoch (an `undefined` / `NaN` epoch accepts every later
  assignment, including a rollback), doesn't check `next.hosts[shard] ===
  self`, and never refreshes the `hosts` set or spectators. Input comes from
  a validated lobby record today, so this is defence in depth.
- **SHARD-8** `assignmentProblems` never checks that `squads`, `npcs` or
  `members` are arrays (a string `npcs` makes `pathAction`'s `includes` a
  substring match, `paths.ts:108`); squad names like `toString` resolve to
  prototype members (`assignment.ts:92`); `renewals` is unvalidated. Only
  the trusted lobby writes these records.
- **SHARD-9** `npcModes` never prunes `tracks` (`paths.ts:171`); a null
  owner clock counts as `carried` with the record present (`:211`); in the
  announce window the path pose is built on the old live state (`:213`), so
  fields like hp are stale.
- **LEAF-4** Retiring an admission when no relay was displaced
  (`wasm.rs:1309`, a re-attempt over an existing direct session) settles
  `Relayed`, closes the transport and calls `retry.forget`, leaving a
  session with no relay, no link and no trigger.
- **LEAF-5** One loss can file two `IceFailed` triggers (channel close, then
  ICE `failed`). `RetryPolicy::note` coalesces them, so there is no double
  re-attempt; only `ledger.ice_failed` double-counts, and channel closes are
  counted as `IceFailed`.

---

## Checked and sound

- **Stream-data routing (leaf):** exact `(peer, stream id)` keys on both
  dispatchers, parsed from `to_json`'s fixed prefix; other event types still
  reach every listener; several listeners per key all receive; removal takes
  effect on the next dispatch. (Unchanged from before:
  `ProxyStream::on_message` still uses `Closure::forget`.)
- **Close handler (leaf):** detached on every local close path (`close`,
  `close_all`, insert-replacement, dropped bootstrap); owned, not forgotten;
  fenced by channel identity.
- **Orphan close (leaf):** only the peer at the far end of the DTLS channel
  can trigger it; relayed packets and the anchor are excluded; re-checked
  after `on_datagram`.
- **Half-direct fix, other parts (leaf):** supersession and node close stay
  destructive; `installed()` requires no pending admission.
- **Codec decoder:** varints capped at 8 bytes and safe integers; every loop
  advances and is length-checked; trailing bytes, duplicate indexes and
  invalid UTF-8 refused; quantized values clamped; fingerprint mismatch or
  write error falls back to JSON.
- **`ready()`:** timer cleared on every settle; settles once; `close()`
  rejects only a waiter.
- **Clock pings:** settle from 4/s to 1/s at 8 samples.
- **Hits:** can't be forged (the handoff link checks the sending region's
  host; `hitAction` checks shooter and target ownership) or replayed (random
  ids, 10 s retention, 500 ms cap).
- **Assignment and path writes:** lobby-only / owning-host-only, with
  strictly rising, safe-integer `epoch` and `mv`.
- **Planned moves:** the input boundary holds; a refused move unwraps;
  resume drops pre-crash inputs.
- **Ownership:** a host drops unowned entities on every new epoch; the
  validator refuses an NPC on two shards.
- **Types:** `shard.d.ts` matches `shard.js`; `index.ts` exports every new
  value and type.

## Recommended order

1. LEAF-1, LEAF-2 — witnesses first (headless Chrome, as for the existing
   half-direct and ghost-session witnesses), then the fixes.
2. SHARD-1, NET-1 — one-line fixes, each with a test that uses the default
   clock / a non-`local` client.
3. WIRE-1, SHARD-2, LEAF-3, ANCHOR-1.
4. The rest.
