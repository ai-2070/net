---
title: "v0.47.0 — Start Me Up"
description: "Release notes for Net v0.47.0 — Start Me Up — what shipped, what changed, and what it means for compatibility."
---
# Net v0.47 — "Start Me Up"

*The Rolling Stones, 1981. A seventeen-pass bug audit of the whole tree found places where a peer could keep access it had lost, where a session key could be read by anyone holding the mesh PSK, and where a local file could be overwritten by a racing run. v0.47 is the fix round. It also makes a WebRTC answerer start its own connection checks, and moves the build to Rust 1.100.*

## What's in it

- **A new handshake, `net-handshake-v2`.** Session keys now come from the Noise handshake's shared secret instead of its public transcript. **0.47 nodes cannot handshake with 0.46 or older**, and that includes browser pages and their anchors. See "Breaking changes".
- **Access is rechecked when it is used.** Revoked org grants, subnet credentials, channel policies and expired certificates now take effect everywhere they used to linger, and updates to them commit in order and durably.
- **Anchors and enrollment.** The native mesh socket of `anchor serve` defaults to loopback, an enrollment grant promotes only the participant it names, the per-game player cap is atomic, and ACME certificates are cached per issuing directory in a private cache.
- **Payments.** A provider signs only the price and recipient it published.
- **Local files.** CLI verbs that write keys and tokens can no longer overwrite a seed or each other's output, and every store stages and commits its files safely.
- **Bindings.** Node, Python and Go fixes for buffer ownership, cancellation, error status and object lifetimes.
- **Hermes.** Each machine runs as its own node identity, derived from the root. This changes every seeded Hermes node's identity.
- **RTC.** An answerer behind an address-restricted NAT now starts its own ICE checks. `str0m` stays on 0.24.
- **Rust 1.100.0** builds every release artifact.

The findings are recorded, one entry each, in `docs/internal/misc/BUG_AUDIT_2026_11_14.md`. The IDs below (NHS-01, ORG-02, …) are its entries. Every fix has a regression test.

---

## The handshake

**NHS-01.** The four session keys (packet and route-hop, one per direction) were derived from the Noise handshake hash. That hash is computed from public material: anyone holding the mesh PSK, the responder's static public key and the two handshake messages could recompute it, so they could read the traffic, and the session had no forward secrecy.

The keys are now expanded from the Noise `Split()` outputs, the secret that the handshake's DH results feed, one output per direction under its own label (`net-packet-i2r-v2`, `net-route-hop-r2i-v2`, …). The handshake hash rides along only as channel binding. The prologue tag `net-handshake-v2` versions the change on every handshake path:

- **Mesh handshakes**, direct and routed: `net-handshake-v2 ‖ routing_id(src) ‖ routing_id(dest)`, where `routing_id(x)` is the low 32 bits of the node id, zero-extended. The routing header carries only 32-bit ids; the full node id travels inside the encrypted msg1 payload.
- **The point-to-point `NetAdapter`** (the default `NoiseHandshake` constructors): the 16-byte tag alone.

A peer on the old derivation fails at msg1 instead of opening a session it cannot decrypt. The exact KDF is in the wire-format reference and `docs/TRANSPORT.md`.

Two related fixes:

- **NHS-03.** A routed msg2 that failed verification used to consume the pending initiation, so a forged reply could cancel a real handshake. It is now checked before anything is consumed.
- **NAT-08.** The rendezvous coordinator forwarded a `PunchAck` whatever origin it claimed. It now forwards one only from the peer it names.

---

## Access is rechecked when it is used

**Organizations**

- **ORG-02.** Revoking a capability grant now also withdraws DISCOVER authority, and reaches emission, the cached send path, ingest (`ScopedIngestError::GrantRevoked`) and queries.
- **ORG-01, ORG-05.** Replacing the revocation store must keep every live grant revocation, and a corrected clock can no longer bring back one that was pruned: an expired grant revocation is marked and kept for seven more days. A state file holding such a mark is written as version 3.
- **ORG-03.** An owner audience belonging to another org is refused when an authority is loaded or installed.
- **DISC-05.** A verified owner projection carries its certificate's expiry and stops counting once it passes.

**Subnets** (SUBNET-01, 03, 04, 05, 07)

- A revoked credential can no longer pick up a new authority epoch, and admission re-reads subject floors after it installs a context, so a floor that lands mid-admission cannot leave the peer admitted.
- Revocations commit in one durable order. A floor apply on a store that is not durable reports `StateNotPersisted`, even when it changed nothing, and retrying a failed one repairs it.
- Subject floors now apply to a gateway's local export authority, judged per right by the strongest credential holding it.

**Channels**

- **CPOL-01.** Replacing a channel's admission policy re-asks every existing membership, so the old policy's readers lose access.
- **CPOL-02, CISS-01.** The publish fallback picks a cached token through the channel's trust roots, and an expired covering grant no longer hides its live replacement.
- **CHAN-04.** A delayed, superseded membership request is acknowledged but no longer undoes a later one.

---

## Anchors and enrollment

- **ANCHOR-01.** `anchor serve` binds its native mesh socket to `127.0.0.1:0` by default and warns when `--bind` is not loopback. Browsers never use that socket, and the bootstrap PSK in every browser credential also authenticates native UDP sessions, so a reachable socket would admit any credential holder. Separating the two needs a protocol change and is not in this release.
- **ANCHOR-03.** An enrollment grant promotes only the session it names. The SDK's enrollment services (`serve_game_enrollment`, `serve_enrollment`, `serve_enrollment_auto`) do this already; a custom handler registered directly on the core must call `MeshNode::set_enrollment_subject_binding(true)`.
- **ANCHOR-04.** The per-game player cap is taken atomically at promotion. A visitor refused at the cap gets a Rejected reply with code `DENIED` (7) instead of Admitted.
- **ANCHOR-06.** An idle game is reclaimed only after every invite it issued has expired, and is judged again under its issuance gate before removal.
- **ENR-03.** The final issuance check uses the time at commit, not a time sampled before the claim was stored.
- **TLS-02, TLS-06.** ACME certificates are cached per issuing directory (`<acme-cache>/directory-<id>/<domain>/`), so a staging certificate can no longer be served for production. The default cache moves from the shared temp directory to a private per-user `<local data>/net-mesh/acme`, and a cache another user could write is refused.

---

## Payments

**PAY-01.** A provider used to sign whatever price and recipient the caller put in the quote request. It now signs only the terms it published. Published offers follow the service: they are published once the serve registers, and withdrawn when it is refreshed, released or dropped.

---

## Local files

**The CLI** (CLI-02 to CLI-05):

- `identity generate`, `org audience-keygen` and `invite create --out` are no-clobber at publication, not just at their up-front check. Two concurrent runs can no longer both succeed with one replacing the other's seed; the loser exits 2.
- `channel issue-grant` and `cap announce --out` refuse an `--out` that names their own signing key, and with `--force` refuse to replace any file holding a seed, or one they cannot read.
- `transfer recv-blob` stages into a fresh `<out>.partial.<nonce>`, so concurrent receives to one `--out` no longer corrupt each other.

**Stores** (PSTORE-03, CONSENT-01 to 03):

- Every store stages under its full filename in a file created exclusively, so two stores in one directory can no longer share a staging path.
- A pin mutation holds the store lock until it lands, and its commit includes the directory sync. Device-registry and revocation-store saves report `NotDurable` when that sync fails.
- `PinStore::save` is a compare-and-swap: a stale snapshot returns `PinStoreError::Stale` and writes nothing, instead of bringing back a removed approval.
- `wrap` refuses to start when its revocation store cannot be loaded, instead of running with an empty one.

---

## Directories and blobs

- **BLOB-09.** A directory manifest with duplicate paths, or names that are not exact Unicode, is refused. Names that would alias on the destination filesystem (case, Unicode form) fail the fetch instead of overwriting each other.
- **BLOB-10.** A range read on a small blob reference checks the bytes actually present, not the advertised size.
- **GBLOB-02.** `net_dir_manifest_read` / Go `DirManifestRead` refuse an unsupported manifest version, the same as `fetch_dir`.

---

## Bindings

- **Node.** Streamed chunks and `Buffer` arguments are copied when the call is made, so reusing a `Buffer` afterwards no longer changes bytes already queued (NCALL-01). A synchronous `appError` throw keeps its status on every streaming shape (NCALL-02). An observer that throws or rejects is contained and logged, not escalated (NCALL-03).
- **Python.** Cancelling a pull whose data was already ready now cancels the underlying call (PYBR-02). A registration made after `shutdown()` comes back already closed instead of being kept forever (PYBR-01).
- **Go.** The legacy blob-adapter constructor keeps its Redex alive across the native call, so a finalizer can no longer free it mid-call (GBLOB-01).
- **MCP forwarding.** Forwarded secret bytes are zeroized on every error path (MCP-08).
- **`@net-mesh/browser` store.** Visibility paths, interest widening and deferred projections are re-authorized, and the host pins a player's arrival stream per peer, not per host (STORE-01, 02, 04).
- **Redaction.** Debug-report redaction matches keys the same way in Rust, TypeScript and Python (TSDK-06).

---

## Hermes

- **HERMES-02.** A Hermes node runs as a per-machine identity derived from the root seed and a persistent machine id, not as the root itself, so machines under one root are distinct nodes. `NET_MESH_MACHINE_ID` sets the label, and `NET_MESH_NODE_SEED` sets the node seed outright (it may not equal the root).
- **HERMES-04.** A promoted tool name that would alias another gets a digest suffix (`pc/terminal.run` can become `net_mesh__pc__terminal_run___<hex>`), and a colliding registration is refused.
- **HERMES-08.** The machine id is published together with its contents, so a racing start adopts the committed value.

---

## RTC: the answerer starts its own checks

The ICE agent inside `str0m` has no clock until it is first given a timeout, and the RTC driver only fed it one when it was due. Under `str0m` 0.25 a freshly built answerer therefore never sent a connectivity check, and behind an address-restricted NAT, where the offerer's checks are dropped until the answerer has sent one, the session never opened. Two NAT-simulator rows failed on `master` because of it; no release carried 0.25.

Every new session now ticks its ICE agent once at creation (`new_rtc` in `rtc/driver.rs`), whatever `str0m` version runs underneath. `str0m` is back on 0.24.1, and `renovate.json` holds it below 0.25 until those rows pass on 0.25.

---

## Rust 1.100

The workspace's `rust-toolchain.toml` and CI move from 1.99.1 to 1.100.0, and the release workflows build inside that pin. The crates still set no `rust-version`. The NAT simulator finds its helper binary under cargo 1.100's new build-directory layout, and the skill-example doc checker no longer depends on the order it reads files.

---

## Smaller changes

- `async-nats` 0.52, behind the `jetstream` feature, fixes a panic on a line break in a received header value.
- Two flaky CI tests are fixed: `join_lifecycle`'s port retry and the browser runner's bounds witness.
- Dependency updates: `toml` 1.2.1, `napi` 3.16.2, `napi-derive` 3.7.1, `redis` 1.8.2, `arc-swap` 1.9.5, `smallvec` 1.16.5, `notify` 8.2.2, `webpki-root-certs` 1.0.12, `syn` 3.0.10, `loom` 0.7.3, and the website's packages.

---

## Version bump

Everything published moves to **0.47.0**: every manifest and pin, the `net-mesh` Python bound (now `>=0.47.0,<0.48.0`), the skills' `net-version`, the Hermes integration pin and the lockfiles.

---

## Breaking changes

**The handshake (wire)**

- 0.47 and 0.46 nodes cannot complete a handshake, on the mesh and on the point-to-point `NetAdapter`. Browser pages (`@net-mesh/browser`) handshake with their anchor the same way, so a page and its anchor must also be on the same side.

**Identity and files**

- Every seeded Hermes node gets a new entity id, node id, origin and Noise key.
- An org revocation state file that holds an expiry mark is written as version 3, which 0.46 refuses to load.
- The ACME cache moves (see "How to upgrade").
- `transfer recv-blob` leaves `<out>.partial.<nonce>`, not `<out>.partial`.

**Behaviour**

- `anchor serve`'s native mesh socket is on loopback unless `--bind` says otherwise.
- A visitor refused at a game's player cap receives `DENIED` (7).
- `identity generate`, `org audience-keygen` and `invite create --out` exit 2 when another run published first. `channel issue-grant` and `cap announce` refuse to replace a seed.
- `wrap`, including `wrap --owner-root`, refuses to start without a loadable revocation store.
- A custom enrollment handler registered on the core must call `MeshNode::set_enrollment_subject_binding(true)`.
- A Python registration after `shutdown()` returns a closed handle. Node observer failures are logged, not thrown.
- A directory fetch whose names alias on the destination fails with `DirError::Manifest`. Inspecting a manifest of an unsupported version fails (`ErrDirInvalidManifest` in Go).

**Rust API**

- `PinStore::save` takes `&mut self` and can return `PinStoreError::Stale`.
- `PinStoreError`, `DeviceRegistryError` and `RevocationStoreError` are `#[non_exhaustive]`; the last two gain `NotDurable`.
- `VerifiedGatewayContext` has a new public field, `delegated`.

The C ABI is unchanged.

---

## How to upgrade

Bump to 0.47.0 and rebuild, **on every node at once**: a 0.47 node cannot talk to a 0.46 one. Upgrade a browser game's anchor and its pages together.

- **Hermes:** every machine's node identity changes. Update `NET_MESH_PEERS` on other machines and `net wrap --allow <origin>` lists, move same-root providers to `--owner-root`, and re-enroll devices whose operator is a Hermes node. `NET_MESH_NODE_SEED` cannot be set to the root seed, so the node cannot keep running as the root.
- **Anchors:** if a native peer reached the anchor's mesh socket from another host, pass `--bind <ip>:<port>` explicitly. The anchor warns, because the bootstrap PSK in every browser credential also opens that socket. An anchor on the default ACME cache orders one new certificate on its first start; one with an explicit `--acme-cache` reuses its certificate when the chain verifies for the domain.
- **Custom enrollment handlers** on the core: answer real grants and call `MeshNode::set_enrollment_subject_binding(true)`.
- **`wrap`:** make sure its revocation store exists and loads, or pass `--revocation-store`.
- **Scripts that cleaned up `<out>.partial`:** match `<out>.partial.*`, and remove only a staging file you know is stale; another receive may still be writing its own.
- **Rust:** add a wildcard arm where you match `PinStoreError`, `DeviceRegistryError` or `RevocationStoreError`; give `PinStore::save` a mutable store.

---

## Not in this release

These audit findings stay open, each needing a protocol or policy decision: NHS-02 (the zero-static upgrade path), ORG-04 (grant issue times), the root cause of ANCHOR-01, CONSENT-04 (revoking a pin does not recall an invoke already in flight), MCP-11 (retry safety of credential-free tools, documented policy), the CHAN-04 residual, and HERMES-01, 03, 05, 06 and 07.

---

Released 2026-11-17.

## License

See [LICENSE](https://github.com/ai-2070/net/blob/master/net/crates/net/LICENSE-APACHE).
