---
title: "v0.44.1 — Ace of Spades"
description: "Release notes for Net v0.44.1 — Ace of Spades — what shipped, what changed, and what it means for compatibility."
---
# Net v0.44.1 — "Ace of Spades"

*Same Motörhead single as [v0.44](/docs/releases/release-v0.44-ace-of-spades). An anchor no longer fills up with browsers that have left.*

## What's in it

v0.44.1 is a **patch release for anchors and long-running nodes**:

- **An anchor no longer runs out of RTC sessions.** A browser that left without a clean close kept its session on the anchor for good, so the anchor eventually refused every new player.
- **An abandoned nRPC subscribe no longer leaks** a pending-ack slot.
- The Python binding moves to pyo3 0.30, and the release is built with Rust 1.99.1.

Nothing changes on the wire or in any API.

---

## Fix: an anchor no longer fills up with browsers that have left

**The symptom.** The public anchor, restarted on 0.44.0, held 2 RTC sessions at first and 256 two and a half days later, its `--rtc-max-peers` limit. The count never fell, even overnight with nobody playing. From then on, every new player was refused with `at_capacity` (HTTP 503, "this anchor holds 256 of its 256 RTC sessions"). `"rtc": { "sessions": … }` in `anchor serve`'s `game_stats` line shows the count.

**The cause.** The RTC driver frees a session only once the session is closed, or once `str0m` reports it dead. ICE disconnection never makes `str0m` report a session dead. A browser that closes its page cleanly closes its channel, and that path was fine. A browser that vanishes (a killed tab, a sleeping laptop, a phone off the network) sends nothing more, and two paths then dropped the peer without closing its channel:

- **The failure sweep.** The mesh's dead-peer sweep removed a silent peer from every mesh table, but did not tell the RTC driver.
- **A reconnect.** A browser keeps one identity per origin, so a reloaded tab, or a page that reconnects after a network change, arrives as the same node on a new DataChannel. Installing the new session retired the old one's mesh state but left its channel open.

Either way, nothing could reach the old session again, and it held one of the anchor's `max_peers` slots until the anchor restarted. A native node's RTC peers leaked the same way.

**The fix.** Both paths now close the RTC endpoint they drop: the sweep closes a swept peer's endpoint, and the install closes the endpoint its successor replaced. The driver's close notification that follows finds no peer on that endpoint, so it cannot touch the successor.

**Tests.** `rtc_repairs` gains two tests, and each fails without its half of the fix. In `a_swept_dead_peers_rtc_session_is_closed`, the sweep evicts a silent peer and its RTC session must go too. In `a_displaced_rtc_endpoint_is_closed`, the same identity reconnects on a second channel: one session must remain, and the successor must still deliver.

---

## Fix: an abandoned nRPC subscribe no longer leaks

The first unary call to a target subscribes to its reply channel and waits for the membership ack. A call dropped while it waited, such as the losing branch of a hedged call or of a `select!`, left its pending-ack entry behind for the node's lifetime: one dead entry per abandoned subscribe. The entry now leaves with the call, however the call ends. A failed `CANCEL` publish, which is best-effort, is now logged at debug level instead of being swallowed silently.

---

## Smaller changes

- The Python binding builds against pyo3 0.30 and pyo3-async-runtimes 0.30. The Python API is unchanged.
- Built with Rust 1.99.1.
- The `hedge_loser_handler_observes_cancellation` test no longer fails on a loaded CI runner.
- Dependency updates, including `rustls` 0.23.46, `tokio` 1.53.3 and the web site's packages.

---

## Version bump

Everything published moves to **0.44.1**: every manifest and pin, the `net-mesh` Python bound (now `>=0.44.1,<0.45.0`), the skills' `net-version`, the Hermes integration pin and the lockfiles.

---

## Breaking changes

None.

---

## How to upgrade

- **Anchors:** upgrade `anchor serve` to 0.44.1. Until you do, restart a 0.44.0 or older anchor before its `"rtc"` session count reaches its `--rtc-max-peers` limit.
- **Everything else:** bump to 0.44.1. No code changes are needed.

---

Released 2026-10-17.

## License

See [LICENSE](https://github.com/ai-2070/net/blob/master/net/crates/net/LICENSE-APACHE).
