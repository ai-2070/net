# Fail-closed defaults in 0.35

0.35 closes a set of authorization and file-permission findings. Seven
defaults changed from permissive to fail-closed. **Each one can stop a
working deployment on upgrade**, and each is deliberate: in every case
the previous default granted something to anyone who completed the mesh
handshake, and completing the handshake proves PSK possession and
nothing about operator intent.

Read this before upgrading. The short version: if you administer a node
remotely, you now have to say who the operators are.

---

## 1. Compute migration will not accept a remote orchestrator

**Was:** any admitted mesh peer could send `TakeSnapshot` and become the
orchestrator for a daemon. Handling it returns the daemon's snapshot to
the sender and, under identity transport, seals the daemon's private
Ed25519 seed to a `target_node` the *sender* chose.

**Now:** `MigrationOrchestratorPolicy::LocalOnly`. A cross-node
migration requires the **source** to name its orchestrator.

```rust
runtime.set_migration_orchestrator_policy(
    MigrationOrchestratorPolicy::allowlist([control_plane_node]),
)?;
```

Call it before `DaemonRuntime::start()`; afterwards it errors rather
than silently not applying. Locally-orchestrated migration is
unaffected. A refused request gets a `MigrationFailed` reply naming the
reason, so this shows up as a clear failure, not a timeout.

## 2. `blob.transfers` refuses every remote request

**Was:** any admitted peer could enumerate in-flight stream ids, holder
identities and expected content hashes, and `Cancel` an arbitrary
transfer — failing the *owner's* fetch.

**Now:** `TransferAdminPolicy::Closed`.

```rust
serve_blob_transfer_rpc_with_policy(&mesh, adapter,
    TransferAdminPolicy::operators([ops_node_id]))?;
```

`serve_blob_transfer_rpc` (no policy) still exists and now serves
nothing remotely. Read and write are not separately configurable — see
the type's docs for why.

## 3. The aggregator registry refuses every remote request

**Was:** any admitted peer could `List` (including `group_seed`),
`Spawn`, `Scale` and `Unregister`.

**Now:** `RegistryAdminPolicy::Closed`, and the daemon config takes:

```toml
operators = ["0x1a2b3c4d5e6f7788"]
# or, if mesh membership IS operator membership in your deployment:
# operators_any_admitted_peer = true
```

Setting both is refused rather than resolved. Note that
`RegistryReadHandler` — despite the name — answers `Unregister`, so it
carries the same gate.

Scope: these are fold-summary reducers. An unauthorized caller could
never forge the signed source announcements, mutate canonical fold
entries, or run arbitrary workloads. The exposure was control-plane
availability, summary suppression, and resource exhaustion.

Also new: `max_groups` (64) and `max_replica_count` (16) bound how much
one caller can commit. `replica_count` is a `u8` and was validated only
as non-zero, so a single request could ask for 255.

## 4. `MeshDbServer::new` requires an access policy

**Was:** the server received `(peer, request)` and discarded the
authenticated caller before the executor ran. The per-chain ACL the
MeshDB plan specifies was not merely unenforced — it was
unimplementable.

**Now:** the policy is a required constructor argument.

```rust
MeshDbServer::new(executor, MeshDbAccessPolicy::AllPeersMayReadEveryChain)
// or
MeshDbServer::new(executor, MeshDbAccessPolicy::per_chain(MyAuthorizer))
```

There is no default: installing a server exposes every chain the reader
resolves to every admitted peer, so that has to be a decision someone
made. `AllPeersMayReadEveryChain` is the honest name for the old
behaviour and is correct when the served chains are public to the mesh.

## 5. Secret-bearing files are refused unless owner-only

Applies to the aggregator config (`psk_hex`), the CLI profile
(`psk_hex`), and device-enrollment state (`device_seed`).

**Was:** the daemon warned and booted; the CLI profile had no check at
all; enrollment read its private seed unchecked.

**Now:** the file must be a regular file, owned by the calling user,
with `mode & 0o077 == 0`. The ownership check is the substantive
addition — `0600` proves nothing about *whose* `0600` it is.

```sh
chmod 600 /etc/net/aggregator.toml
chown "$(id -u)" /etc/net/aggregator.toml
```

Escape hatch: `--insecure-permissions` on the daemon,
`ConfigFile::load_with` / `DeviceEnrollment::load_allowing_insecure`
in-process. These skip the ownership and mode checks; they never skip
the regular-file check, which is about not blocking forever on a FIFO.

**Windows is unchanged and still a gap**: `std::fs` exposes no usable
NTFS ACL view, so the gate warns rather than enforcing. Restrict the
DACL out of band.

## 6. Fetched directories are owner-only

**Was:** the reconstruction tree inherited the umask, typically `0755`,
for the whole transfer — and it lives beside the destination, often
somewhere another local user can traverse. Files landed at `0644` and
were narrowed at the end, so a file destined for `0600` was
world-readable for the length of its transfer.

**Now:** the temp tree is created `0700`, and files are created
carrying their final mode. The `0700` travels with the inode through
the final rename, so **`dest` ends up owner-only** where it previously
inherited the umask. Widen it explicitly if another local user needs
it.

## 7. Remote file modes lose setuid/setgid/sticky

`DirEntry::File.mode` comes from the manifest publisher and reached
`chmod` verbatim, so a hostile manifest could ask for `04755`. Fetch as
root and the result is a root-owned setuid binary. Ordinary permission
bits are preserved; the special bits are dropped unconditionally.

No configuration. If you were relying on a fetched directory to carry
setuid bits, that never worked safely.

---

## The operator identity workflow

Items 1–3 name **mesh node ids**. A node id is derived from an
identity, so an operator needs a stable one:

```sh
# 1. Create it (written 0600).
net-mesh identity generate --out ~/.config/net-mesh/operator.toml

# 2. Read the node id it derives.
net-mesh identity show ~/.config/net-mesh/operator.toml --output json
#   { ..., "node_id_hex": "0x1a2b3c4d5e6f7788", ... }

# 3. Name that id on the serving side (config or policy).

# 4. Attach WITH it.
net-mesh transfer ls --identity ~/.config/net-mesh/operator.toml ...
```

Without `--identity` the CLI attaches anonymously with a fresh node id
every run, and any allowlist will refuse it. Two concurrent invocations
sharing one identity share a node id and the second displaces the
first in the daemon's peer map — give unattended automation its own
identity.

## Diagnosing a refusal

Every gate answers rather than dropping, so a misconfiguration looks
like a failure and not a hang:

| Surface | What you see |
|---|---|
| Migration | `MigrationFailed` — "not an authorized orchestrator" |
| `blob.transfers` | non-zero exit, "not authorized" |
| Aggregator registry | `RegistryRpcError::Unauthorized` (C: `NET_REGISTRY_ERR_UNAUTHORIZED`) |
| MeshDB | `MeshError::Unauthorized` |
| Secret files | typed error naming the path and a category |

The MeshDB and transfer refusals deliberately do not name the chain or
resource that was refused: doing so would turn a denial into an
existence oracle for a caller not allowed to know.
