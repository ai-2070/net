---
title: "v0.45.1 — Breaking the Law"
description: "Release notes for Net v0.45.1 — Breaking the Law — what shipped, what changed, and what it means for compatibility."
---
# Net v0.45.1 — "Breaking the Law"

*Same Judas Priest song as [v0.45](/docs/releases/release-v0.45-breaking-the-law). A netcode player that drops out can come back.*

## What's in it

v0.45.1 is a **patch release for browser games** built on `@net-mesh/browser`:

- **Closing a stream no longer silences its peer.** After a page closed its half of a stream, the node dropped every later frame the peer sent on that label until the page reopened it.
- **A timed-out netcode player can rejoin.** Because of the bug above, a player the host timed out could never be admitted again.
- **A player cut off from its host stays quiet.** It used to upload about 700 kB/s for as long as the page stayed open.

Nothing changes on the wire. One behaviour changes, deliberately: see [Breaking changes](#breaking-changes).

---

## Fix: closing a stream no longer silences its peer

**The symptom.** Reported from [Net Shooter](https://github.com/ai-2070/net-fps): two pages, A sending to B on a lossy stream. While B had never opened the stream, B received every frame. Once B opened its own half and closed it again, B received nothing more from A on that label. Every frame was counted `drops.stream_closed` in `node.counters()` until B opened the stream again. A reopening its own half did not help.

**The cause.** Closing a stream marked it closed in the leaf, and the receive path dropped every later arrival for a closed stream. The stream's own consumers were already detached by `close()`, so the only listener left was the node-wide `node.onEvent`, which a page that never opened the stream does receive frames on. Closing a stream was therefore worse than never opening it.

**The fix.** The closed mark is gone. After `close()`, frames the peer still sends on that label reach `node.onEvent` as `stream_data`, exactly as for a stream the page never opened. The receive position is still kept across a close, so a page that reopens the stream resumes at the peer's next frame, as before. `drops.stream_closed` keeps its place in `node.counters()` and reads 0.

---

## Fix: a timed-out netcode player can rejoin

`hostNetcode` closes a player's stream when it times the player out (`playerTimeoutMs`, and after a failed send). That stream shares its label with the one the player sends on, and the host learns about a player only from the player's own frames. With the bug above, those frames were dropped, so any gap of `playerTimeoutMs` removed the player for good, and a 1.5-second outage that made the host's sends fail was enough to start it. The host's code for a returning player, which carries on from where the player was applied, never ran.

With the leaf fixed, a timed-out player is admitted again as soon as it is heard and carries on from where it was applied. `hostNetcode` itself is unchanged.

---

## Fix: a player cut off from its host stays quiet

`joinNetcode` kept every input its host had not acknowledged and, once that backlog passed `inputRedundancy`, sent 64 of them with every input. The frames grew past one event and rode as fragments on the lossy carrier, so a player whose host had gone uploaded about 700 kB/s for as long as the page stayed open. Now:

- it keeps at most the newest 64 unacknowledged inputs (older ones were never sent again anyway);
- every input frame fits in one event (8,000 bytes), so it never rides as fragments; a backlog too big for one frame sends its newest inputs;
- while the host has said nothing for a second, it repeats its inputs only at `pingRate` (4 per second by default) instead of with every input.

---

## Tests

- `net-mesh-leaf` gains `a_closed_fire_and_forget_stream_delivers_like_one_never_opened`, the report's own shape: small and fragmented payloads sent before the receiver opens its half, while it is open, and after it closes it. It fails on 0.45.0 at the first round after the close. The close-and-reopen test now asserts that frames sent while the stream is closed are delivered.
- `@net-mesh/browser` gains `test/netcode/cut-off.test.ts`. One test checks the player's backlog cap, frame size and silent-host rate, and fails on 0.45.0. The other checks that the host re-admits a timed-out player after closing its stream.

---

## Smaller changes

- The cross-org relay live test (`a_caller_relays_its_orgs_floor_and_the_provider_then_refuses_the_removed_member`) waits for the relayed bundle to be retained, not only for its floor. It failed twice on this release's CI.
- The website's `/games` page has a section on sharded matches, the open-source Net Shooter demo, and a reworked Rose & Blade section over a gameplay reel.
- Dependency updates: `arc-swap`.

---

## Version bump

Everything published moves to **0.45.1**: every manifest and pin, the `net-mesh` Python bound (now `>=0.45.1,<0.46.0`), the skills' `net-version`, the Hermes integration pin and the lockfiles.

---

## Breaking changes

No API changes. One behaviour change in `@net-mesh/browser`: after a stream's `close()`, frames its peer still sends on that label now reach `node.onEvent` as `stream_data`. Before, they were dropped until the stream was reopened.

---

## How to upgrade

- **Browser pages:** upgrade `@net-mesh/browser` to 0.45.1. If a page relied on `close()` to stop `node.onEvent` receiving a peer's frames on a label, ignore those frames in its `onEvent` handler instead.
- **Netcode games:** nothing to change. If you worked around the bug by keeping streams open or raising `playerTimeoutMs`, you can undo that.
- **Everything else:** bump to 0.45.1. No code changes are needed.

---

Released 2026-10-18.

## License

See [LICENSE](https://github.com/ai-2070/net/blob/master/net/crates/net/LICENSE-APACHE).
