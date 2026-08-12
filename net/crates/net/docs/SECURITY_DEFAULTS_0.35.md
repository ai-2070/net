# Fail-closed defaults in 0.35

0.35 closes a set of authorization and file-permission findings. Eight
defaults changed from permissive to fail-closed, and the Python wheel's
crash behaviour changed (§9). **Each one can stop a
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

**Was:** any admitted peer could `List`, `Spawn`, `Scale` and
`Unregister`.

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

## 3a. Aggregator status no longer emits `group_seed`

`List` returned the raw 32-byte `group_seed`, which deterministically
derives every replica keypair. It is replaced by a 16-hex-char
`group_seed_fingerprint` — `BLAKE3("net-aggregator-seed-fingerprint-v1"
|| 0x00 || seed)` truncated — which answers "are these two groups
running the same seed?" without answering "what is it?".

Field renames, by surface:

| Surface | Was | Now |
|---|---|---|
| Rust (`RegistryGroupSummary`) | `group_seed: [u8; 32]` | `group_seed_fingerprint: SeedFingerprint` |
| C FFI JSON | `group_seed_hex` (64 ch) | `group_seed_fingerprint_hex` (16 ch) |
| Node | `groupSeedHex` | `groupSeedFingerprintHex` |
| Python dict | `group_seed_hex` | `group_seed_fingerprint_hex` |
| `net-mesh aggregator ls/status/spawn` | `group_seed` | `group_seed_fingerprint` |

**This was not an exploitable disclosure**, and the change is hygiene
rather than a fix. The derived replica keypairs authorize nothing in
the aggregator path: replicas publish through the host `MeshNode`,
`SummaryAnnouncement` carries no replica signature, and aggregator
replicas are not `DaemonHost`s in the compute registry, so the
migration identity-envelope path cannot select one. Moreover a
dynamically spawned group — and a static one that omits an explicit
seed — derives the seed from the group *name*, so those replica keys
were already reconstructible by anyone who knew the name. `List` was
not what made them recoverable.

It is removed anyway because an explicitly configured seed *is* meant
to be secret, and a status API should not carry private key material
regardless of what it currently authorizes.

If cross-node aggregator replicas ever need real signing identities,
that needs a randomly generated seed persisted with explicit ownership
and rotation semantics — not this derivation, and **not** a public
deployment salt over it, which would only be another secret seed
wearing a different name.

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

## 8. A2A tasks are bound to their submitter

**Was:** `status` and `cancel` keyed on the caller-supplied task id
alone, and the registry stored no submitter. Any in-root peer that
learned an id could read the task's full brief — prompt and context
refs — or cancel the work.

**Now:** each task is bound at submission to the AEAD-authenticated
peer that submitted it. Status and cancel only see that peer's own
tasks; another submitter's task reads as *unknown* rather than
forbidden, so a denial is not an existence oracle.

Submission stays open to every in-root peer. That part was deliberate
and is unchanged — same-root reachability means permission to submit
work, not permission to inspect and cancel everyone else's.

A task id is a name, not a bearer capability: it is client-generated
and travels through logs, dashboards and polling loops as an ordinary
identifier. Third-party observation, if it is ever wanted, needs an
explicit delegated capability rather than a leaked id.

**API change** for direct `TaskRegistry` users — `submit`, `status`,
`record`, `cancel` and `forget` take a `TaskOwner` (`Local` for
in-process use, `Peer(node_id)` from the wire), and `submit` returns
`Result`: reusing an id for a *different* brief is now
`SubmitRejection::IdReusedForDifferentBrief` rather than a silent
hand-back of the earlier task. Identical re-submits stay idempotent
(the nRPC retransmit case). Entries are keyed by `(owner, task_id)`,
so two peers may use the same id without interfering — and no peer can
squat obvious ids to deny another.

The wire format is unchanged; this is entirely server-side, so a peer
on an older build still interoperates.

## 9. The Python wheel unwinds instead of aborting

Not a fail-closed change — the opposite, and it belongs here because it
changes what a failure does to your process.

**Was:** the published wheel was built `--release`, which the
workspace sets to `panic = "abort"`. Under an effective abort profile,
an internal Rust panic calls `abort()` and takes the host process with
it — no traceback, no chance to handle it. In a Jupyter kernel or a web
worker, everything else in that process dies too.

And it was untested: CI installs the binding with `maturin develop`,
which builds **debug** and therefore unwinds. The configuration under
test and the configuration that shipped disagreed about whether a panic
is recoverable, and only the recoverable one was ever exercised. That
is how a tokio runtime being dropped in an async context sat there
while 884 tests passed.

One caveat on the history, because an earlier version of this note
overstated it. When that runtime-drop panic was actually observed on a
`--release` wheel, the process *survived* — which an effective abort
profile cannot do. So whether that artifact really used abort, and
whether the panic relates to the separate `0xC0000409` termination that
prompted the investigation, are both unestablished. The change below is
made on the policy above, not on that chain of events.

**Now:** wheels are built with a dedicated `python-release` profile —
identical to `release`, but `panic = "unwind"`. A panic crossing a
pyo3-invoked boundary can be contained and surfaced as a Python
exception rather than ending the process.

The CLI, the daemon, and the Node, Go and C artifacts are unchanged and
still abort. They own their processes; a Python extension is a guest in
one.

**What this does not do.** It is not a promise that every panic becomes
a catchable exception. It makes `catch_unwind` work at the pyo3
boundary and turns a panicking background tokio task into a `JoinError`
instead of an abort — but a detached task nobody joins still fails
silently. Any task whose result matters needs its `JoinHandle` observed
and turned into a real error. This is a release-policy repair, not a
substitute for not panicking.

CI now builds a wheel with that exact profile, installs it into a clean
virtualenv, and runs the suite against the installed artifact, so the
shipped configuration is tested rather than approximated.

---

## What "authenticated peer" means everywhere above

Items 1–3 and 8 authorize on the **AEAD-authenticated session peer** —
the node whose session the frame decrypted under. A sender cannot
choose what that says, which is what makes it usable where
`caller_origin` is not (that field is routing metadata the sender
picks, and this tree's own docs say not to authorize on it).

Its limit, which applies to every gate in this document:

> It authenticates the **deliverer**, not an end-to-end origin.

If a deployment terminates and reissues nRPC through a relay, the relay
becomes the authenticated peer. An operator allowlist then authorizes
the relay and everything the relay chooses to forward; an A2A task
submitted through one is owned by the relay, not by the agent behind
it. That is the documented limit of public nRPC attribution rather than
a gap in any of these repairs — end-to-end provenance needs a PROTECTED
service (`RpcContext::org_admission`, a verified four-party identity)
or an application-level signature over a transcript that binds the
destination.

Direct authenticated sessions — the ordinary case, and the one the
acceptance tests exercise — carry the boundary you would expect.

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
| A2A status / cancel | another submitter's task reads as unknown |

The MeshDB and transfer refusals deliberately do not name the chain or
resource that was refused: doing so would turn a denial into an
existence oracle for a caller not allowed to know.
