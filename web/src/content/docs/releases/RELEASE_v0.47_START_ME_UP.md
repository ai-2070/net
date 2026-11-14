---
title: "v0.47.0 — Start Me Up"
description: "Release notes for Net v0.47.0 — Start Me Up — what shipped, what changed, and what it means for compatibility."
---
# Net v0.47 — "Start Me Up"

*The Rolling Stones, 1981. A WebRTC answerer behind a NAT waited for its peer to speak first, and behind an address-restricted NAT its peer never could. v0.47 makes the answerer start on its own, moves the build to Rust 1.100, and refreshes dependencies.*

## What's in it

v0.47 is a **maintenance release**:

- **An RTC answerer starts its own ICE checks.** A session no longer depends on a timer the WebRTC stack happens to set, so an anchor behind an address-restricted NAT can be reached.
- **`str0m` stays on 0.24.** 0.25 broke the case above. Renovate keeps it below 0.25 until the NAT simulator passes on it.
- **Rust 1.100.0** builds every release artifact.
- **Dependency updates,** including `async-nats` 0.52, which fixes a panic on line breaks in NATS header values.

Nothing changes on the wire or in any API.

---

## RTC: the answerer starts its own checks

**The symptom.** Two rows of the NAT simulator (`natsim_natted_anchor_publishes_a_reachable_rtc_addr` and `natsim_natted_anchor_publishes_both_mapped_endpoints`) failed every day after `str0m` moved to 0.25 on `master`. In both, a native anchor answers a WebRTC session from behind an address-restricted NAT. No released version carried 0.25.

**The cause.** The ICE agent inside `str0m` has no clock until it is first given a timeout. The RTC driver only fed it a timeout when one was due, so a freshly built answerer reported its next timeout as about ten years away and never sent a connectivity check. Behind an address-restricted NAT, the offerer's checks are dropped until the answerer has sent one of its own, so the session never opened. Under 0.24 an unrelated one-second timer set the clock by accident.

**The fix.** Every new session ticks its ICE agent once at creation (`new_rtc` in `rtc/driver.rs`), so the answerer schedules its own checks at once whatever `str0m` version runs underneath. `str0m` is back on 0.24.1 in the core crate, the SDK and every lockfile. `renovate.json` holds it below 0.25 until the NAT'd-anchor rows pass on 0.25.

Two new tests in `rtc/driver.rs` cover it: an answerer sends its own check with nothing arriving, and a session opens through an address-restricted NAT in front of the answerer. Both fail without the tick. The NAT simulator is green again on `master`: 19 scenarios and 49 table rows.

---

## Rust 1.100

The workspace's `rust-toolchain.toml` and CI move from 1.99.1 to 1.100.0, and the release workflows build inside that pin, so 0.47's binaries, wheels and npm packages are built with 1.100.0. The crates still set no `rust-version`.

Two fixes ride along:

- Test binaries live in a new place under cargo 1.100's build directory, so the NAT simulator looked for its helper binary in the wrong directory. It now searches for it.
- The doc checker for skill examples merged enums that share a name (`RpcError` in the core crate and in the leaf), so whether it passed depended on the order files were read. It now checks each definition separately.

---

## Smaller changes

- `async-nats` 0.52, behind the `jetstream` feature. A `\r` in a received header value used to panic the connection task, so any client allowed to publish on a subject could stop its subscribers' connections. Header values with line breaks now get a space in place of each one.
- Two CI tests are less flaky. `join_lifecycle` no longer reuses a taken port when it retries `up --enroll`, and the browser runner waits for both conditions of the bounds witness before reading its counter.
- Dependency updates: `toml` 1.2.1, `napi` 3.16.2, `napi-derive` 3.7.1, `arc-swap` 1.9.5, `smallvec` 1.16.5, `notify` 8.2.2, `webpki-root-certs` 1.0.12, `syn` 3.0.10, `loom` 0.7.3, and the website's packages.

---

## Version bump

Everything published moves to **0.47.0**: every manifest and pin, the `net-mesh` Python bound (now `>=0.47.0,<0.48.0`), the skills' `net-version`, the Hermes integration pin and the lockfiles.

---

## Breaking changes

None. Nothing changes on the mesh wire, the C ABI or any SDK surface.

---

## How to upgrade

Bump to 0.47.0 and rebuild. No code changes are needed.

- **WebRTC anchors behind a NAT:** upgrade the anchor. The fix is on the answering side.
- **Building from the repository:** `rust-toolchain.toml` now pins Rust 1.100.0; rustup installs it on first use.

---

Released 2026-11-14.

## License

See [LICENSE](https://github.com/ai-2070/net/blob/master/net/crates/net/LICENSE-APACHE).
