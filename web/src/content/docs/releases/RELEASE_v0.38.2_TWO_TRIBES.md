---
title: "v0.38.2 — Two Tribes"
description: "Release notes for Net v0.38.2 — Two Tribes — what shipped, what changed, and what it means for compatibility."
---
# Net v0.38.2 — "Two Tribes"

*Same Frankie Goes to Hollywood arena as [v0.38](/docs/releases/release-v0.38-two-tribes). One anchor fix: a busy anchor no longer runs itself out of sockets and locks up.*

## What's in it

v0.38.2 is a **patch release with one behavioral fix in the anchor's HTTPS listener**. It matters to anyone running `net-mesh anchor serve` on the public internet, and above all to a public anchor (`--open-games`). Nothing else about how a node runs changes.

---

## Fix: the anchor's listener survives running out of sockets

**The symptom.** The public anchor at `anchor.ai2070.net` (v0.38.1) stopped answering. New connections got a TCP handshake and nothing after it, and the process sat at 100% CPU. The host has one core. It stayed wedged until it was restarted.

**The cause.** Three gaps in the listener, which together turn ordinary internet traffic into an outage:

- **No TLS handshake deadline.** A client that connected and never completed a handshake held its socket forever.
- **No HTTP idle deadline.** hyper only enforces its header deadline when a timer is configured, and the listener never configured one. A keep-alive connection that went quiet after a request, as connection pools do, was also held forever.
- **An accept error was retried at once.** Held sockets climbed to the process's file-descriptor limit (1024). From then on every `accept` failed with "too many open files", and the loop retried immediately: 136,431 failures in three seconds. On a one-core host that spin starved the tasks that would have closed connections and freed descriptors, so the anchor never recovered. 426 sockets sat in `CLOSE-WAIT`.

**The fix.**

- A failed `accept` pauses before the next attempt: 10 ms, doubling to one second, reset on the next success. The runtime keeps running the tasks that release descriptors, so the listener recovers as soon as load drops.
- The TLS handshake has a deadline: `BootstrapConfig::tls_handshake_timeout`, default 10 s.
- hyper now has a timer, so its header deadline is enforced: `BootstrapConfig::http_idle_timeout`, default 30 s. It also closes a connection left idle between keep-alive requests.

**Tests.** Two tests run against a real TLS listener:

- a connection that never sends a ClientHello is dropped at the handshake deadline;
- an idle keep-alive connection is closed after its request is answered.

Each was shown to fail with its fix removed, and both are pinned in CI. A unit test fixes the backoff sequence.

**If you run an anchor.** Upgrade. Under systemd, also give the service room for many connections: a public anchor legitimately holds one per player.

```ini
[Service]
LimitNOFILE=65536
```

---

## Docs and CI

- The SDK's rustdoc CI step now builds with `rtc-bootstrap` as well as `full`. The anchor's listener, game registry and ACME client are public modules behind that feature, and until now none of their docs were built. That is how a link to a private item reached v0.38.1; it is fixed here.

---

## Dependency updates

- Website only: `motion` → `13.4.6`, `posthog-js` → `1.434.18`.

---

## Version bump

`0.38.1 → 0.38.2`, applied to:

- every manifest: crate, wire, leaf, CLI, deck, SDK, payments, and the Go, Node and Python bindings;
- the `@net-mesh/*` pins and the `net-mesh` Python bound (`>=0.38.2,<0.39.0`);
- the skills' `net-version`;
- the Hermes integration pin;
- the lockfiles.

---

## Breaking changes

None. `BootstrapConfig` gains two fields, `tls_handshake_timeout` and `http_idle_timeout`, set by `BootstrapConfig::new`.

---

## How to upgrade

Bump to 0.38.2 and rebuild. No wire-format change. To update an anchor, replace `net-mesh` with the `net-mesh-anchor-v0.38.2-*` archive from the release, and restart it.

---

Released 2026-09-30.

## License

See [LICENSE](https://github.com/ai-2070/net/blob/master/net/crates/net/LICENSE-APACHE).
