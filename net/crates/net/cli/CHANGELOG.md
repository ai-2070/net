# Changelog

Notable changes to `net-cli`, the crate that installs the **`net-mesh`**
binary.

This file records what an operator or a CI script has to do differently. The
full per-release story for the whole system lives in the release notes; this
is the subset that reaches this binary's command surface — flags, exit codes,
and output shape.

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
