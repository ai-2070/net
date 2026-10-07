# Net v0.41.1 — "Thunderstruck"

*Same AC/DC stadium as [v0.41](RELEASE_v0.41_THUNDERSTRUCK.md). Browser fixes from a real multiplayer game: two players who reach for each other at once now connect directly, and a lobby that closes no longer erases the next one.*

## What's in it

v0.41.1 is a **patch release of `@net-mesh/browser` fixes**, found by a multiplayer game running on the public anchor. Nothing changes for native nodes, the anchor or the other bindings.

---

## Fix: two pages connecting to each other at once

**The symptom.** In a game where every player connects to every other, two players who called `connectPeer` on each other at the same moment never went direct. The pair stayed relayed through the anchor, and each attempt ran its full ICE deadline before giving up.

**The cause.** Glare. Every offer replaces a pair's link, and answering an offer retires the answerer's own. Each page answered the other's offer, so each cancelled its own attempt; both then waited out a deadline for attempts that no longer existed. Games worked around it by hand-coding a tie-break (only the lower id re-offers).

**The fix.** The SDK breaks the tie, on `BrowserNode` and `MeshSession` alike:

- The page with the **lower** node id offers.
- The higher one, instead of offering back, answers an offer from that peer that arrived within the offerer's ICE deadline. It also answers one that crosses its own offer while that is still under way. Once an answer has started, its outcome is the call's.
- `acceptPeer` while this surface's own `connectPeer` for the peer is in flight takes that call's outcome.
- Offers are learned from the surface's verified signal events. With none, or with the peer's node id unknown, `connectPeer` behaves exactly as in 0.41.0. A stale offer older than the ICE deadline is ignored, so it cannot take over a later call.

One offerer per pair is an attempt at a direct link, not a guarantee of one: a network that cannot go direct still ends relayed. Read the outcome from `peerAttempt()`.

**Tests.** The peer-driver tests pin each case: the lower id offers whatever the peer sent; the higher id answers a fresh offer, offers over a stale one, and answers an offer that crosses its own, taking that answer's outcome even when its own offer settles first; and an `acceptPeer` during the node's own `connectPeer` takes the connect's outcome.

---

## Fix: closing one lobby no longer erases the next

**The symptom.** A page that left a lobby search (`joinLobby` then `close()`) and immediately created its own lobby (`createLobby`) sometimes published no listing at all: nobody could find the new lobby.

**The cause.** An announcement replaces a node's whole tag set. `close()` fired the withdrawal of the seek tag and returned at once, so a withdrawal still in flight could land after the new lobby's listing and replace it with nothing.

**The fix.** `close()` resolves only once the withdrawal has settled, and the withdrawal itself waits for any seeking announcement still in flight. A failed withdrawal is swallowed, as before. A test holds the withdrawal in flight and fails without the fix.

---

## Fix: `counters()` reports each drop reason

`counters()` promised one key per drop reason, but the leaf's nested `drops` object came back as the string `"[object Object]"`. Nested groups now flatten to dotted keys (`drops.<reason>`), each still an exact decimal string.

---

## Docs

- **The public anchor.** `https://anchor.ai2070.net` is documented as a ready anchor, with a `?anchor=` override, plus a local development anchor set up with mkcert.
- **Background tabs.** A tab the browser has throttled can lose its session silently. The docs show a liveness probe: a session counts as gone only after a disconnect event or two unanswered probes in a row, since one timed-out probe is inconclusive.
- **Lobby takeover** and a full-mesh, no-host netcode pattern.
- **Two tabs are one player only with one identity.** Both tabs must load the same identity through `rememberedIdentity()`, which shares one only once it is stored. Without it, each tab's `connect()` is a separate node.

---

## Version bump

Everything published moves to **0.41.1**: every manifest and pin, the `net-mesh` Python bound (now `>=0.41.1,<0.42.0`), the skills' `net-version`, the Hermes integration pin and the lockfiles.

---

## Breaking changes

None.

---

## How to upgrade

Bump `@net-mesh/browser` to 0.41.1. A game that hand-codes a tie-break for simultaneous `connectPeer` calls can drop it; leaving it in place still works.

---

Released 2026-10-07.

## License

See [LICENSE](../../LICENSE-APACHE).
