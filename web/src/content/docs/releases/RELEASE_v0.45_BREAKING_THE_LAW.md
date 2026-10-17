---
title: "v0.45.0 — Breaking the Law"
description: "Release notes for Net v0.45.0 — Breaking the Law — what shipped, what changed, and what it means for compatibility."
---
# Net v0.45 — "Breaking the Law"

*Judas Priest, 1980. Until now an org could not take a grant back before it expired, and a member removed from one org could keep calling another. v0.45 lets the issuing org revoke a grant, and carries an org's revocations to the providers its members call.*

## What's in it

- **Revoke a grant before it expires.** An org can withdraw a capability grant or a dispatcher grant it issued with `net-mesh org revoke-grant`. New calls under it are refused, and calls and streams already running under it end at once.
- **Cross-org revocations reach the provider.** A caller carries its org's signed revocations on its cross-org calls. A provider can also require recent revocations from an org (`up --require-fresh-revocations`) and refuse that org's callers until it has them.
- **A refused start is an error.** `Mesh::start()` used to report success while starting nothing when an `accept()` was in flight. Every SDK and binding now reports the refusal.
- **All three known limitations listed in the [v0.37 notes](/docs/releases/release-v0.37-turbo-lover) are closed:** the silent start, its Python twin, and cross-org floors that read as zero.
- **Tested across processes and languages:** every caller language against every provider language (Rust, Go, Python, Node), unary and server-streaming, 32 cells, in CI.
- **Breaking, in a few places:** `Mesh::start()` returns `Result`, a refused start raises in Python and Node, and the revocation state file has a version 2. See "Breaking changes".

---

## Revoking a grant

A capability grant (B → A: org A's members may call B's capability) or a dispatcher grant (A → S: a dispatcher acts for org A) was bounded only by its `not_after`, up to 30 days. Revocation floors covered membership certificates, not grants. A grant withdrawn on day one stayed usable for the rest of its life.

The issuing org now signs an `OrgGrantRevocationBundle` that names the grants it withdraws:

| Kind | Named by | Revoked by |
|---|---|---|
| Capability grant (B → A) | its `grant_id` | B, the provider org that issued it |
| Dispatcher grant (A → S) | an id derived from the grant's bytes | A, the org the dispatcher acts for |

An org can revoke only grants it issued. A bundle signed by any other org changes nothing.

```
net-mesh org revoke-grant --org-key <PATH> [--grant <PATH> …] [--dispatcher-grant <PATH> …] \
                          [--verifier <NODE> …] [--out <PATH>]
```

`revoke-grant` checks that each named grant verifies and was issued by the `--org-key` org, signs one bundle, and has each `--verifier` apply it. Each verifier answers with a signed attestation giving every grant's state there: `revoked`, `not_revoked` or `expired`. `--out` writes the bundle for nodes reached another way. A capability grant is enforced by the providers of the org that issued it, so name those providers.

At a node that holds the revocation:

- **New calls** under a revoked grant are refused (`CapabilityGrantRevoked`, `DispatcherGrantRevoked`), before the call's replay slot is used. An admission already in flight when the revocation lands fails its re-check instead of admitting under the old view.
- **Live calls and streams** under the revoked grant end with the `Revoked` terminal, the same one a floor raise produces. Calls under other grants keep running.
- **The revocation persists** in `revocation-state.json` with the floors, under the same locked verify → merge → persist order, so it survives a restart and an older bundle never re-admits the grant. Each entry is dropped once the grant's own `not_after` has passed (plus the five-minute skew allowance), so the list stays bounded.

The browser leaf (`net-mesh-leaf`) verifies and applies grant-revocation bundles fed through its control plane, and retires its served calls the same way.

---

## Cross-org revocations: relay and freshness

A provider in org B checks a caller from org A against A's membership floors and A's dispatcher-grant revocations. Both are signed by A's root, and until now they reached B only if B's operators imported them by hand. Without them the floor read as zero, so every A certificate passed at B for its whole life, including a member A had removed. The v0.37 notes listed this as a known limitation.

**Relay.** Every node keeps the signed revocation bundles it has applied, as a small covering set per org (at most 64 KiB per org). When an A node makes a cross-org protected call, it attaches A's retained bundles in a `net-org-revocations` request header: at most 4 KiB and 16 bundles, and only when the set changed or five minutes have passed since it last went to that provider. The header is covered by the request's signature, and the provider removes it before the handler runs.

The provider verifies each relayed bundle and merges it in the background, off the service's request path. A revocation therefore applies to admissions shortly after the call that carried it. Material the call's own admission needs (below) is applied before that call is admitted. The provider takes material only from its own org, orgs it has granted to, and orgs its freshness policy names, and caps each foreign org at 4,096 floors and 4,096 grant revocations. The floor service accepts bundles pushed directly from the same orgs under the same rule, so A's operator can also deliver to B's providers.

A removed member relays nothing useful, but it cannot stop the rest of its org, or its operator, from delivering the bundle that removes it.

**Freshness, opt-in.** Relay is best-effort: if no A node calls B, A's newest revocations never get there. A provider that must not depend on that can require recent material from an org:

- CLI: `net-mesh up --require-fresh-revocations <ORG_HEX>=<DURATION>` (repeatable);
- Rust: `MeshNode::require_fresh_org_revocations`, SDK `Mesh::require_fresh_org_revocations`;
- Python and Node: `require_fresh_org_revocations` / `requireFreshOrgRevocations`.

A caller acting for that org is then refused (`RevocationsStale`) unless the provider holds signed material from the org issued within the age. An org with nothing new to revoke proves itself current with `net-mesh org heartbeat --org-key <PATH> --verifier <NODE> …`, which signs an empty floor bundle. Sent to the org's own nodes, it rides their next calls to every provider. Sent to the providers, it lands directly.

**The default stays permissive.** Without a requirement, a cross-org call admitted while the provider holds no material at all from the caller's org is counted (`foreign_revocation_unverified_admissions`, also on the SDK, Python and Node) and logged once per org.

Not in this release: relay and freshness in the browser leaf, and the freshness policy over the Go and C ABI.

On the wire, a caller refused for any of the three new reasons sees the same `Denied` outcome as for every other admission refusal; the reason stays with the provider.

---

## A refused start is an error

`MeshNode::start()` refuses to start while an `accept()` is mid-handshake, because the receive loop would race the responder for the peer's first message. The refusal was logged and nothing else: `start()` returned normally, every binding reported success, and the node could send but never received. It cost a full diagnostic round in the org-streaming test harness.

The refusal is now reported everywhere a node is started:

| Surface | A refused start |
|---|---|
| Rust core | new `MeshNode::try_start() -> Result<(), StartRefused>`; `start()` is unchanged |
| Rust SDK | `Mesh::start()` returns `Result<()>`, with `SdkError::StartRefused` |
| C | `net_mesh_start` returns `NET_ERR_MESH_START_REFUSED` (-129) |
| Go | `MeshNode.Start()` returns `ErrStartRefused` |
| Python | `NetMesh.start()` raises `RuntimeError` |
| Node, TypeScript SDK | `start()` rejects |
| CLI, Deck | the command fails with the reason |

Starting a node that is already running still succeeds. The refusal is retryable: start again once the `accept()` returns.

---

## Tested across processes and languages

- **The org matrix.** `tests/cross_lang_org/matrix/` runs every caller language against every provider language (Rust, Go, Python, Node) as two OS processes over a real mesh, with one cross-org granted call per shape (unary, server-streaming). The new `org-matrix` CI job fails if any of the 32 cells does not run or does not pass. Before, the only cross-process cell was a Go caller against a Python provider, and CI skipped it.
- **A CLI cross-org journey.** `cli/tests/cross_org_protected_workflow.rs` runs two orgs and a B → A grant: an uncredentialed call is denied before the handler runs, then the CLI-generated Python client is denied and admitted exactly once each, and every record is attributed to A acting under B's grant.
- An independent review of the relay path found ten defects, all fixed before the merge. Among them: relayed bundles were verified and persisted inline, so one caller sending many could stall every other caller of the service.

---

## Smaller changes

- `ORGANIZATIONS.md` has new sections on grant revocation and cross-org revocations; the `net-event-bus` skill and the web CLI reference cover the new verbs.
- Dependency updates, including `num_threads` and the web site's packages.

---

## Version bump

Everything published moves to **0.45.0**: every manifest and pin, the `net-mesh` Python bound (now `>=0.45.0,<0.46.0`), the skills' `net-version`, the Hermes integration pin and the lockfiles.

---

## Breaking changes

Nothing was removed from the mesh wire. A 0.44 provider verifies a 0.45 caller's requests as before; its handlers see the `net-org-revocations` header as an ordinary request header, because only 0.45 removes it.

**Starting a node**

- Rust SDK: `Mesh::start()` returns `Result<()>`. A call written as a statement still compiles, with an unused-`Result` warning.
- Python `NetMesh.start()` can raise `RuntimeError`, and Node / TypeScript `start()` can reject, where they used to return.
- C `net_mesh_start` can return `NET_ERR_MESH_START_REFUSED` (-129), and Go `Start()` can return `ErrStartRefused`. The signatures are unchanged.

**Org admission**

- `AdmissionDenied` has three new variants: `CapabilityGrantRevoked`, `DispatcherGrantRevoked` and `RevocationsStale`. The enum is `#[non_exhaustive]`, so a `match` with a wildcard arm still compiles.

**The revocation state file**

- A node that has applied a grant revocation writes `revocation-state.json` as version 2. 0.44 and older refuse to load it rather than run without the revocations, so such a node cannot be downgraded with its state. A state file with no grant revocations stays version 1.

---

## How to upgrade

Bump to 0.45.0 and rebuild.

- **Rust:** handle `Mesh::start()`'s result, with `?` or `.expect(...)`.
- **Every binding:** treat a refused start as retryable. Start again once the in-flight `accept()` returns.
- **To revoke grants,** upgrade the verifiers you name to 0.45. A 0.44 node cannot apply a grant-revocation request and answers with an error.
- **For cross-org relay,** upgrade both sides: callers on 0.45 carry their org's revocations, and providers on 0.45 apply them.
- **To require fresh revocations,** pick an age that matches how often the caller org's operator already signs (grants are re-issued at least every 30 days), and have that org run `net-mesh org heartbeat` more often than that. A requirement shorter than the org's heartbeat cadence refuses its callers.

---

Released 2026-10-17.

## License

See [LICENSE](https://github.com/ai-2070/net/blob/master/net/crates/net/LICENSE-APACHE).
