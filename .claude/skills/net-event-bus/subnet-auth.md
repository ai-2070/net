# Subnet Authority — Exporting a Service Across a Protected Boundary

**Read this when a provider inside a protected subnet must serve callers outside it** — a factory-floor service reachable by a partner org, a tenant enclave exporting one API while the rest stays sealed, a gateway that must prove its right to carry protected traffic.

This is the *authority* plane of subnets, not the topology plane (`mesh.md` § Subnets covers `SubnetId` / `SubnetPolicy` — where a node *sits*). One sentence per layer:

```text
topology places
subnet credentials authorize local attachment/routing/export
organization credentials authorize the caller
export binding authorizes one provider-local crossing
named export keeps that binding out of application code
```

Topology is not authority: a node's `SubnetId` says where it is, never what it may do. Equal paths under two different authorities are unrelated crossings.

---

## The mental model — two ordinary verbs, everything else is provisioning

```rust
// provider — the export was configured at mesh construction, BY NAME
let _handle = mesh.serve_subnet_exported("fleet.telemetry", "factory-export", handler)?;

// caller — an ordinary org client; the caller never names a subnet
let resp: Telemetry = org.call_exported("fleet.telemetry", &request).await?;
```

The application constructs **no authority objects** — no roots, no credentials, no boundaries, no epochs. The provider names a service and a provider-local export label; the caller presents *organization* authority and calls. That is the whole ordinary surface.

- **`call_exported`, deliberately not `call_subnet`.** The caller never joins the provider's subnet, receives no subnet context, and discovers on the **public** plane through the verified ownership projection. Failures are the four **org** domains (`org.md` § Errors) — an exported call is an organization call.
- **A named export is a provider-local label.** Configured at mesh construction, resolved once into a checked binding, never announced, never accepted from a caller. An unknown name fails locally (`subnet:unknown_export_name`) before anything registers or announces.
- **Dispatch revalidates the exact crossing against the node's LIVE gateway authority on every call**, before organization admission. A revoked or epoch-stale export binding stops serving even though the registration succeeded.

## Construction — where the authority plane is configured

Trust anchors and exports are **config-time** state, validated before the node exists (a broken config means no node, not a half-authorized one):

```rust
let mesh = Mesh::builder(addr, psk)
    .identity_seed(seed)
    .subnet_authority(authority_config)          // trust anchor: authority id + roots + grant-lifetime cap
    .subnet_attachment(path)                     // this node's security attachment
    .subnet_control_channel(channel)             // where signed control facts arrive
    .subnet_export("factory-export",             // the named export the provider verb resolves
        SubnetExportAccess::Granted, subnet_ref, topology_epoch)
    .build().await?;
```

Every binding mirrors this on its constructor: `subnetAuthorities` / `subnetAttachment` / `subnetControlChannel` / `subnetExports` in Node, the snake_case kwargs in Python, `MeshConfig.SubnetAuthorities` and friends in Go, and the same four JSON keys in C's `net_mesh_new`.

## Provisioning — offline issuance, runtime installation

Nothing in any SDK signs. Every signed artifact is minted **offline** by `net-mesh subnet …` (`cli.md` § `net-mesh subnet`) and crosses as opaque canonical wire bytes:

```bash
net-mesh subnet keygen --out ~/subnets/authority-a.toml        # root or delegated-issuer key
net-mesh subnet issue-direct  …   # authority root → subject credential set
net-mesh subnet issue-issuer  …   # root → bounded delegated issuer (one hop, structural)
net-mesh subnet issue-delegated … # leaf signed by a delegated issuer, framed with its grant
net-mesh subnet issue-control-fact …  # descriptor | gateway-advertisement | export-policy | revocation-floor
net-mesh subnet inspect <file>    # decode + summarize, no private material
```

At runtime, the **admin namespace** (deliberately apart from the ordinary verbs) installs them:

- **Gateway credentials** — `install_gateway_credentials(sets)`: **wholesale replace**, pass every currently-held set. Every artifact decodes before anything installs; one malformed set refuses the whole batch with zero node-state mutation.
- **Boundaries** — `declare_boundaries(declaration)`: also wholesale.
- **Control facts** — `apply_control_fact(bytes)`: the **one door** for floors and descriptive facts alike. Returns `{kind, applied}`; `applied == false` is an authenticated stale/idempotent outcome, **not** a failure.

## Per-SDK API

| | provider verb | caller verb | admin |
|---|---|---|---|
| Rust (`net_sdk::subnet`) | `mesh.serve_subnet_exported(service, export_name, handler)` (+ `_bytes`) | `org.call_exported(service, &req)` (+ `call_exported_bytes[_deadline]`) | `net_sdk::subnet::admin::*` |
| TypeScript (`@net-mesh/sdk`) | `mesh.serveSubnetExported(service, exportName, handler)` | `org.callExported(service, req)` / `callExportedBytes` | `subnet.admin.*` |
| Node low-level (`@net-mesh/core/subnet`) | `serveSubnetExported(mesh, service, exportName, handler)` (+ `…Bytes`) | as above | `subnet.admin.*` |
| Python (`net_sdk`) | `mesh.serve_subnet_exported(service, export_name, handler)` | `client.call_exported(service, request)` | `net.subnet.admin.*` |
| Python low-level (`net.subnet`) | `serve_subnet_exported(mesh, service, export_name, handler)` (+ `…_bytes`) | as above | `net.subnet.admin.*` |
| Go (`go/subnet.go`) | `ServeSubnetExported[Req, Resp](node, service, exportName, handler)` / `ServeSubnetExportedBytes` | `CallExported[Req, Resp](ctx, client, service, req)` / `CallExportedBytes` | `InstallSubnetGatewayCredentials` / `DeclareSubnetBoundaries` / `ApplySubnetControlFact` |
| C (`net_subnet.h`, links `-lnet`) | `net_subnet_serve_exported(mesh_arc, service, export_name, …)` — Rust resolves the name against the node's own map | `net_org_call_exported` (on the org client, in `net_org.h`) | `net_subnet_install_gateway_credentials` / `_declare_boundaries` / `_apply_control_fact` |

The handler receives the same verified `OrgCaller` as `serve_org` — the serve pipeline, dispatcher, and teardown rules are shared (`org.md` § Teardown applies unchanged).

### Go and C are first-class

Both bindings serve exports, call exported services, run every runtime admin verb, and declare subnet **trust anchors**. Go sets `SubnetAuthorities` / `SubnetAttachment` / `SubnetControlChannel` / `SubnetExports` on `MeshConfig`; C supplies the same four keys in the JSON `net_mesh_new` already takes. Rust converts and validates them through the frozen DTOs before the node exists. A standalone Go or C program can stand up a gateway on its own. See `bindings/coverage.md` for the rows and anchors.

## Errors — one stable envelope, one fixture

Subnet failures are **local and startup-shaped** — configuration, decode, or install refused before (or without) any node-state mutation — and carry the stable `subnet:<kind>` envelope, single-sourced from Rust and pinned by `net/crates/net/tests/cross_lang_subnet/stable_kinds.json` (consumed by the Rust, Node, Python, and Go suites).

- **Auth kinds** (core verifier refusals): `unknown_authority`, `wrong_topology_epoch`, `scope_not_ancestor`, `revoked`, `issuer_attenuation_broadened`, `invalid_format`, … — the reason a credential set or fact was refused.
- **Local configuration kinds**: `unknown_export_name`, `duplicate_export_name`, `invalid_id_hex`, `path_too_deep`, `invalid_path_level`, `invalid_access`, ….
- Classifiers: `classifySubnetError` (Node), `net.subnet.parse_subnet_kind` / `classify_subnet_error` (Python), `ParseSubnetKind` / `errors.Is(err, ErrSubnet)` (Go), `NET_ORG_ERR_SUBNET` (C, distinct from `NET_ORG_ERR_PROVISION`).
- An unrecognized kind is passed through **verbatim as data** — never remapped onto a known kind.
- **A remote refusal of an exported call is never a `subnet:` error.** It surfaces through the org taxonomy; a caller cannot probe a provider's subnet configuration through error kinds.

## Gotchas

- **Serve-registration failures WRAP the envelope** (`… failed: subnet:unknown_export_name: …`) rather than leading with it. Use the provided classifiers, which scan; a bare-prefix parse misses these.
- **Wholesale means wholesale.** `install_gateway_credentials` and `declare_boundaries` replace the whole set — passing a delta silently drops everything you didn't pass.
- **`applied: false` is success**, not failure: the fact verified but changed nothing (stale floor, idempotent re-apply). Don't retry it.
- **The export name never crosses the wire.** If a caller "knows" a provider's export name, something is mislayered — callers name *services* only.
- **Epochs pin bindings.** An export binding declares the topology epoch it was minted under; a topology change that bumps the epoch stops the old binding at dispatch (`wrong_topology_epoch`), by design.
- **No signing key type exists in any binding.** If you want to sign a subnet artifact in-process, you're looking for the CLI ceremony instead.

## Source of truth

- Rust facade: `net/crates/net/sdk/src/subnet.rs` (DTOs, `admin`, serve; one conversion module for every binding)
- Substrate: `net/crates/net/src/adapter/net/subnet/auth.rs`
- Bindings: `bindings/node/subnet.ts`, `bindings/python/python/net/subnet.py`, `go/subnet.go`, `include/net_subnet.h`
- CLI: `net/crates/net/cli/src/commands/subnet.rs`
- Frozen kind vocabulary: `net/crates/net/tests/cross_lang_subnet/stable_kinds.json`

## Cross-references

- `org.md` — the organization layer exported calls ride on: credentials, `OrgCaller`, the four call domains, teardown.
- `mesh.md` § Subnets — the *topology* plane (`SubnetId` / `SubnetPolicy` / gateway `Visibility`); a different question.
- `cli.md` § `net-mesh subnet` — the issuance ceremonies.
- `bindings/coverage.md` — the three subnet rows and per-binding anchors.

## Further reading

- [Subnets](https://ai2070.net/docs/concepts/subnets)
- [Security model](https://ai2070.net/docs/concepts/security-model)
