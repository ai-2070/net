# Changelog

Notable changes to `net-cli`, the crate that installs the **`net-mesh`**
binary.

This file records what an operator or a CI script has to do differently. The
full per-release story for the whole system lives in the release notes; this
is the subset that reaches this binary's command surface — flags, exit codes,
and output shape.

## Unreleased — remote inspection and explicit client binding

- Extend `--inspect-target` to aggregator query/spawn/scale, transfer
  receive/admin, live typegen, wrap, MCP serve, and optional anchor ls/stats.
  Inspection reads configuration but performs no network, child-process or
  output-file work. MCP inspection exits before starting protocol traffic.
- Remote clients accept `--bind`, overriding profile `bind`. Existing
  loopback client and wildcard hosted-service defaults remain unchanged.
  Loopback-to-non-loopback targets and incompatible address families fail
  early with actionable errors; execution consumes the inspected bind.
- Offline typegen refuses remote-only target/bind/inspection flags.
  Offline/persistent-wide inspection remains follow-up work.

## Unreleased — aggregator list target resolution

- `aggregator ls` now selects remote RPC from a complete profile target,
  just as it does from flags. Scripts that intended a development-only
  snapshot must explicitly add `--local`; incomplete target tuples fail.
  Explicit flags override individual profile values. Local mode can ignore
  profile remote defaults, with disclosure, but conflicts with remote flags.
- `aggregator ls --inspect-target` reports resolved mode/target, public
  fingerprints, identity availability, current bind and provenance without
  connecting, minting an identity or starting a supervisor. It does not
  verify authorization. No keys or PSKs are emitted. The same resolved
  target is passed to execution, with no local fallback on remote failure.
- Profile loading now rejects an explicitly selected missing config or an
  unknown named profile. Remove an obsolete `--config` only when you mean
  to use the optional implicit default. Commands that do not load profiles
  retain their existing behavior; CLI-wide inspection/config validation
  and non-loopback client binding are not part of this sub-slice.

## Unreleased — explicit temporary-supervisor scope

- Admin commits, ICE simulation/commit, audit/log/failure streams, capability
  reads, peer/daemon listings, subnet topology reads, gateway/channel reads,
  and local aggregator inspect/list now require `--local`, like snapshot.
  Without it they exit 2 without a result payload. This is a deliberate
  script compatibility change: `net-mesh peer ls` becomes
  `net-mesh peer ls --local` only for intentional development use.
- Scope: "Starts a temporary supervisor for this command; does not inspect
  a running node." Successful opt-in emits this notice on stderr even with
  `--quiet` or logging disabled. Existing JSON result shapes are unchanged.
  This does not implement remote Deck administration.
- Admin `--dry-run` remains an offline preview without `--local` or an
  identity; ICE dry-run still starts a supervisor and requires the opt-in.
  Offline issuance, persistent stores, and real remote clients are not gated.
  Gateway export remains unsupported. Local/explicit-remote aggregator
  selections conflict; profile-only remote list selection is separate work.

## Unreleased — NetDB restore preflight

- Successful `netdb restore` now persists the restored adapter state for
  subsequent CLI/SDK opens. Previously it existed only in the restore
  process, so a later read returned an empty store after replacement.
  Local `cortex.snapshot` checkpoints live alongside the adapter logs;
  retain them when copying a restored store. The portable snapshot format
  is unchanged. Older binaries do not understand these checkpoints and
  must not be used to open restored stores.
- Continue using the origin supplied at restore time: reopening a restored
  adapter under another origin now refuses instead of reusing its counter.
  Post-restore writes replay from the destination log's position, including
  when that log is shorter than the snapshot's original source log.
- Embedded task/memory snapshot payloads are validated before `--clear`.
  Checkpoint publication or restore flush failures return nonzero without a
  restore-success payload. This does not make replacement crash-atomic or
  permit concurrent writers.

- NetDB commands now propagate configuration read, permission, and parse
  errors instead of silently falling back to another store. Optional absent
  configuration still uses the existing defaults; valid store precedence is
  unchanged.
- `netdb restore` loads the snapshot once, checks its byte ceiling and decodes
  its envelope, and rejects an envelope without adapters before creating or
  clearing the destination. Snapshots inside the destination are captured
  before `--clear` removes their original path. Actual bytes read are bounded
  even if the file grows after its metadata check.
- Restore advice distinguishes `--force` (merge) from `--clear` (remove the
  existing store before restoration). This is preflight protection, not
  transactional replacement: use an offline store, and expect storage or
  adapter replay failures after preflight to be able to leave incomplete state.

## Unreleased — targets 0.35.0

> **Security defaults changed.** Seven defaults went fail-closed in this
> release, several of which will stop a working remote-administration
> setup on upgrade. See
> [`docs/SECURITY_DEFAULTS_0.35.md`](../docs/SECURITY_DEFAULTS_0.35.md) —
> in particular the operator-identity workflow, which is what the new
> node-id allowlists key on.

### Breaking

- **`--identity` now also fixes the CLI's mesh `node_id`.** Previously it
  set only the operator identity used for signing; the attached mesh came
  up anonymous with a fresh id every run. Remote-administration surfaces
  now authorize on that id, so an allowlisted operator must pass
  `--identity`.

  Two consequences worth knowing:

  - Without `--identity` the CLI stays anonymous, and any node-id
    allowlist will refuse it. That is a non-zero exit saying
    `not authorized`, not a hang.
  - Two concurrent invocations sharing one identity now share a
    `node_id`, and the daemon's peer map is keyed on it — the second
    attach displaces the first. Give unattended automation its own
    identity rather than reusing a human operator's.

- **`net-mesh identity show` gained `node_id_hex`.** Additive to the JSON
  shape, but scripts asserting on an exact key set will need updating. It
  is the value that goes in an operator allowlist.

- **`net-mesh snapshot get` and `net-mesh snapshot status` now require
  `--local`.** Without it they exit 2 with an explanation instead of printing
  a snapshot.

  ```sh
  net-mesh snapshot get --local
  net-mesh snapshot status --local
  ```

  Scripts that call either verb will start failing at argument parsing. That
  is the intent, and the reason is worth reading before adding the flag.

  These commands never read a running deployment. The Deck client is built
  from a `MeshOsDaemonSdk` that the invocation itself starts, and there is no
  attach path — so the only snapshot they can produce is of a supervisor
  created milliseconds earlier, which is empty by construction.

  Before, that came back as exit 0 and entirely plausible JSON:

  ```json
  { "daemons": {}, "replicas": {}, "peers": {}, "avoid_list": {},
    "local_maintenance": "Active", "recently_emitted": [] }
  ```

  An empty snapshot and a healthy idle cluster are the same document. The one
  line on stderr concerned an ephemeral identity, which points at identity as
  the missing prerequisite and quietly implies everything else worked. A
  monitoring script built on that reported a healthy cluster having inspected
  nothing.

  **What to do:**

  - Checking output shape, or smoke-testing in CI? Add `--local`. It still
    reports the fresh in-process runtime, and now says on stderr that it is
    not a view of a running deployment, so a result pasted into a report
    carries its own caveat.
  - Actually observing a node? Use a surface that attaches to one —
    `net-mesh aggregator`, `net-mesh peer`, or `net-deck`.

### Changed

- **Help text no longer points at repository-internal files.** The root
  `--help` ended with "See NET_CLI_PLAN.md for the full surface", a file that
  ships in no package — so the single pointer offered to someone who had just
  run `pip install net-mesh-cli` named something they could not open. It now
  names `net-mesh help <command>` and the online CLI reference.

- **`--help` spells the binary `net-mesh` throughout.** 112 places across the
  command modules wrote `net <verb>`; the crate is `net-cli` but the
  installed executable is `net-mesh`, so every one of those was a
  copy-and-paste failure.

- **`--no-color`'s help entry no longer prints its own maintenance history.**
  clap renders `///` doc comments as long help, and a comment written for
  maintainers had become a user interface.

### Fixed

- **The README's Quick start commands all exist.** It told operators to run
  `net-mesh snapshot show`, which has never been a subcommand under that
  spelling — so the first read-only operation a new user copied out of the
  README failed at argument parsing. The `admin drain` and `ice` examples
  named flags and verbs that did not match the clap tree either. A test now
  runs every command the README publishes against the real binary.
