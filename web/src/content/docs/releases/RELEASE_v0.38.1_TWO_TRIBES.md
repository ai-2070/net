---
title: "v0.38.1 — Two Tribes"
description: "Release notes for Net v0.38.1 — Two Tribes — what shipped, what changed, and what it means for compatibility."
---
# Net v0.38.1 — "Two Tribes"

*Same Frankie Goes to Hollywood arena as [v0.38](/docs/releases/release-v0.38-two-tribes). One anchor fix: an anchor that cannot get its first certificate no longer locks itself out of Let's Encrypt.*

## What's in it

v0.38.1 is a **patch release with one behavioral fix in the anchor's ACME client**, plus dependency maintenance. It matters to anyone running `net-mesh anchor serve` with `--acme-directory`. Nothing else about how a node runs changes.

---

## Fix: the anchor keeps its ACME account

**The symptom.** An anchor whose first certificate order kept failing was refused by Let's Encrypt after a few minutes: `too many new registrations (10) from this IP address in the last 3h0m0s`. The first order can fail for ordinary reasons: a DNS record not propagated yet, a firewall still closed, an IPv6 address not configured yet. Under a supervisor that restarts it, the anchor tried again every few seconds, and every attempt spent one of the ten. It then stayed locked out for up to three hours after the real problem was fixed. This happened on the first `anchor.ai2070.net` Droplet.

**The cause.** Every certificate order registered a **new** ACME account and threw its credentials away. After a certificate is cached, a restart does not order again, so the defect only bit before the first certificate, and at every renewal: each renewal registered one more account.

**The fix.** The account is created once per ACME directory and saved in the ACME cache, beside the per-domain certificate directories:

- The file is `acme-account-<hash of the directory URL>.json`, written 0600 because it holds the account's private key.
- A staging account is therefore never presented to production.
- Every later order, including renewals, restores the saved account.
- A restore that fails fails the order; it does not quietly register a new account, which would bring the churn back. An operator retires a dead account by deleting its file.
- If the new account cannot be saved, the order still completes and caches its certificate, so a restart does not order again.

**Tests.** Two unit tests cover the file: it round-trips, is private on disk, is never offered to another directory, and an unreadable file counts as none. The pebble cold-start witness (a real ACME server in CI) now orders twice from one cache, with the certificate removed in between, and asserts that the second order reused the saved account byte for byte.

**If you run an anchor.** Upgrade, and keep `--acme-cache` on persistent storage (the default is the system temp directory). The account file lives there with the certificate. An anchor that already has a certificate cached is not at risk today, but without this fix its next renewal registers another account.

**Under a supervisor.** Cap restarts, so no crash loop can reach any ACME limit. With systemd:

```ini
[Unit]
StartLimitIntervalSec=1h
StartLimitBurst=3

[Service]
Restart=on-failure
RestartSec=60
```

---

## Dependency updates

- `quanta` → `0.13`. Re-resolved lockfiles bring `xxhash-rust` `0.8.19` and `tokio-rustls` `0.26.6`.
- Every lockfile that path-depends on the core is re-resolved, so none goes stale.

---

## Docs

- The install pages name 0.38 as the published release.

---

## Version bump

`0.38.0 → 0.38.1`, applied to:

- every manifest: crate, wire, leaf, CLI, deck, SDK, payments, and the Go, Node and Python bindings;
- the `@net-mesh/*` pins and the `net-mesh` Python bound (`>=0.38.1,<0.39.0`);
- the skills' `net-version`;
- the Hermes integration pin;
- the lockfiles.

---

## Breaking changes

None.

---

## How to upgrade

Bump to 0.38.1 and rebuild. No API, configuration or wire-format change. To update an anchor, replace `net-mesh` with the `net-mesh-anchor-v0.38.1-*` archive from the release and restart it. Its saved certificate is kept, and its next order creates the one account it will keep.

---

Released 2026-09-29.

## License

See [LICENSE](https://github.com/ai-2070/net/blob/master/net/crates/net/LICENSE-APACHE).
