# Changelog

Notable changes to `net-cli`, the crate that installs the **`net-mesh`**
binary.

This file records what an operator or a CI script has to do differently. The
full per-release story for the whole system lives in the release notes; this
is the subset that reaches this binary's command surface — flags, exit codes,
and output shape.

## Unreleased — managed nodes, join links, relations and leave

**Browsers for your games (`anchor serve`)**
- New `--game <id>[:<per-minute>]` (repeatable) with `--issuer-identity`:
  the anchor serves enrollment and issues anonymous visitor credentials at
  `POST <url>/credential`. Each game has its own enrollment root derived
  from the issuer key; issuance is limited per game and per source IP
  (`--credentials-per-minute`); `--game-stats-secs` prints per-game
  counters. The start report gains `games` and `credential_endpoint`.
- `--credential-issuer` is now optional when `--issuer-identity` is given.
- `--inspect-target` omits `credential_issuer_fingerprint` when only
  `--issuer-identity` names the issuer (inspection reads no key files).

**Nodes and links**
- New `up` / `down` / `node status`: one long-lived node per profile, a
  lifetime lock and an authenticated local control endpoint.
- `up` generates and keeps the mesh PSK on first start, or takes it from
  `--psk-from file:<path>` / `stdin`. A literal PSK is never accepted on the
  command line.
- New `up --enroll`, `invite create|inspect|status|revoke|approve|deny`,
  `join` and `leave`.
  - A `netmesh-join_` link joins a clean device without hand-installed
    secrets. Direct attach is tried first, then the relay (`relay serve`,
    `up --relay`).
  - Links are bearer secrets unless bound with `--for`.
  - `--require-approval` holds issuance for `invite approve`.

**Relations one link can carry**
- A subnet attachment: `--subnet`, with `up --subnet-issuer-grant/--subnet-issuer-key`.
- Org membership: `--org`, always approved with `org approve --org-key`.
- A channel credential: `--channel` / `--channel-rights`, with
  `up --channel-grant` from the offline `channel issue-grant`.
- A device already on the mesh adds one relation with `subnet invite|join`
  or `org invite|join`.

**Channels**
- New `channel serve` (persisted gating), `channel status`,
  `channel publish` and `channel leave [<name>]`.
- New `channel invite` / `channel join`: add a channel to an already-joined
  device with a standalone link.
- Subscription and publish readiness are reported from the live session and
  the node's own gate, never implied by holding a credential.

**Removal and leave**
- New `subnet remove`, `org remove`, and `subnet members` / `org members`.
  - Removal is reported per named node from its own signed attestation;
    `complete` holds only when all of them persisted it.
  - Members are listed as "issued" versus "observed here", never a global
    roster.
- One active subnet attachment per verifier. `subnet join --switch` and
  `subnet activate <scope>` switch it explicitly.
- New `subnet leave`, `org leave` and `channel leave`. Each is local and
  durable, and survives restart. None of them revokes.

**Enrolled consumers**
- New `wrap --joined <state-dir>` / `mcp serve --joined <state-dir>`: run as
  the enrolled device.
  - It refuses while `up` owns the join, and refuses `--psk-hex`.
  - An explicit `--node-addr/--node-pubkey/--node-id` names the peer.

**Help text**
- The top-level help for `org`, `node`, `subnet` and `channel` now
  describes these verbs.

## Unreleased — one bind/PSK validation for every verb

- A malformed `--bind` / profile `bind` literal is now one exit-code class on
  every verb: `wrap --listen` and the attach verbs (`wrap`, `mcp serve`) both
  reject it with exit 2 (invalid arguments) during argument resolution. A
  broadcast literal such as `--bind 255.255.255.255:0` previously exited 2 on
  `wrap --listen` but slipped through attach-side validation and exited 6
  (connection failure). Scripts matching exit 6 for a malformed bind literal
  on an attach verb must now match exit 2. Multicast and broadcast binds are
  refused on both paths, and PSK hex parse failures now surface their cause
  on `wrap --listen` as they already did on the attach path.

## Unreleased — standalone capability publisher

- `wrap --listen` starts a publisher without a bootstrap peer. It requires a
  PSK, defaults to loopback, and rejects remote peer settings from flags or
  profiles. Explicit/profile bind selection and startup deadlines still apply.
- The `wrapped` event now includes `connection` with the live bind address,
  hex-string node ID, public Noise key and origin hash in both startup modes.
  No secret is included. Consumers must refresh these details after restart.
- A two-node subprocess harness covers local consent, provider owner-scope
  denial, one authorized invocation and cleanup; it is not off-host evidence.

## Unreleased — bounded large RPC responses

- Updated native peers negotiate unary responses up to 1 MiB including encoded
  status and headers. The packet cap remains 8 KiB. Live typegen capture and
  offline regeneration now cover roughly 22 KB metadata; incomplete responses
  cannot publish output. Older peers retain their single-packet limit.

## Unreleased — explicit oversized RPC failure

- An oversized metadata response now returns a small RPC error naming the
  single-packet limit instead of silently waiting for the CLI deadline.
  The handler may have completed; the CLI does not retry. Snapshot/generated
  output remains unpublished. This remains the compatibility behavior for
  callers that do not negotiate large responses; the packet limit is unchanged.

## Unreleased — provider-bound live typegen metadata

- Missing live input schemas are fetched from the advertising provider;
  missing output schemas are fetched when it advertises the metadata service.
  Returned identity/version, tags and inline contracts must match. Output
  schemas remain optional; snapshot v1 and offline skip behavior are unchanged.
- Conflicting advertisements fail; identical replicas choose the lowest node
  ID without fallback. Acquisition shares the explicit remaining timeout, or
  one 30-second post-attachment limit when omitted. Unusable schemas and fetch
  failures refuse before publication. Existing unary transport size limits apply.

## Unreleased — selection-aware live typegen discovery

- Live `typegen generate/snapshot --tool` waits for every requested ID after
  tag filtering, rather than stopping on unrelated tools. Missing IDs exit 7
  before writing output. An explicit global timeout can end the wait sooner.
- Tag-only/unfiltered discovery observes the full five-second window; this
  is not a complete inventory. Offline snapshot filtering is unchanged.
- Provider-bound hydration is covered above; offline incomplete or unsupported
  schemas can still be skipped during generation.

## Unreleased — wrapped-provider startup budget

- `wrap --timeout` shares one budget across configuration, identity loading,
  attachment and child MCP initialization/discovery/publication. Startup expiry
  exits 7 without a `wrapped` result and drops the managed direct child.
- After publication, the provider lifetime and later tool-list refreshes are
  not bounded by this startup flag. NDJSON lifecycle events remain unchanged.

## Unreleased — MCP startup budget

- `mcp serve --timeout` bounds configuration, identity loading and mesh
  attachment. After startup, it continues serving until stdin closes or the
  operator stops it; the startup budget is not a session lifetime limit.
- Startup expiry exits 7 with no protocol stdout. `--output json` still does
  not add status/result envelopes to MCP JSON-RPC traffic.

## Unreleased — blob receive acquisition budgets

- `transfer recv-blob --timeout` bounds configuration, attachment and network
  waits with one absolute deadline. Disk writes consume elapsed budget but are
  not cancelled; final publication runs outside cancellation after acquisition.
  Expiry exits 7 without success output and preserves the destination. An
  `<out>.partial` staging file may remain, as with other receive failures.
- Directory receive still rejects explicit timeouts: its SDK reconstruction
  tasks and blocking install need a cancellation-safe boundary first.

## Unreleased — transfer administration and live typegen budgets

- Transfer `ls/status/cancel` and live typegen `generate/snapshot` now honor
  explicit `--timeout` across configuration, attachment and RPC/discovery.
  Typegen rendering and file publication happen outside cancellation after
  acquisition succeeds; existing output is untouched on acquisition timeout.
- Transfer receive/staging and offline typegen still reject explicit timeouts.
  Live schema hydration and discovery semantics are unchanged by this slice.

## Unreleased — explicit aggregator deadline budgets

- Remote aggregator ls/query/spawn/scale honor `--timeout` as one total budget,
  returning exit 7 on expiry without retrying the operation or claiming remote
  cancellation. Zero budget refuses before effects.
- Unsupported commands/modes now reject explicit timeouts before work instead
  of silently ignoring them. Omit the flag to retain existing command limits;
  the previously advertised but unused global 30-second default is removed.

## Unreleased — ICE automation framing and confirmation

- ICE commits emit one result with `preview` and `commit`, not two consecutive
  JSON values. Move scripts from `jq -s '.[1].commit_id'` to
  `jq '.commit.commit_id'`. Dry-run remains preview-only.
- Commit previews go to stderr; refusal and failure leave no success payload
  on stdout. `--yes` now skips the prompt on TTY as well as non-TTY input;
  identity, signature and policy gates remain in effect.

## Unreleased — remaining temporary and keychain inspection

- Remaining temporary-supervisor commands accept `--local --inspect-target`,
  including streams and admin/ICE. Inspection does not start, simulate, prompt
  or commit. Admin/ICE distinguish required identities from ephemeral fallback
  and reject combining inspection with dry-run.
- Temporary reports consistently include supervisor node ID and identity
  requirement; keychain builds can inspect forwarding service/account selection
  without reading stdin or accessing the credential store.

## Unreleased — temporary read-context inspection

- Capability show/query/nodes, subnet show/ls/tree and gateway stats/exports
  accept `--local --inspect-target` without starting a supervisor or generating
  an identity. Reports the supervisor node ID and configured public fingerprint.
- Inspection now discloses ignored profile bind defaults even when no remote
  target tuple is configured.

## Unreleased — bootstrap credential target inspection

- `anchor credential mint/inspect --inspect-target` reports input/output
  selection without PSK/credential reads or minting. Mint reports its actual
  signer and warns that normal execution includes the credential on stdout,
  even when `--out` also writes a file.

## Unreleased — subnet artifact inspection

- Offline subnet key generation, issuance, all four control-fact types and
  artifact inspection accept `--inspect-target`. Resolve the signer and paths
  without generating keys, signing credentials or decoding issuer grants.

## Unreleased — organization artifact inspection

- All five `org` verbs accept `--inspect-target`. Keygen does not generate
  a key; issuance reports the actual signer's fingerprint and output paths
  without signing or minting discovery audience secrets.

## Unreleased — adoption target inspection

- `node adopt --inspect-target` reports authority and input paths plus the
  public subject fingerprint without reading certificates/floors or installing
  ownership. Normal adoption and inspection share authority-path resolution.

## Unreleased — identity and announcement artifact inspection

- Identity generate/show/fingerprint/revoke accept `--inspect-target` without
  generating keys, reading identity payloads or raising revocation floors.
  A default generation destination is a runtime filename pattern, not a
  fabricated identity or path. Explicit destinations and store precedence
  retain their existing meaning.
- `cap announce --inspect-target` reports the actual signer fingerprint and
  file/stdout destination without signing or emitting announcement bytes.
  The selected key is read through the existing permission/parse gate;
  conflicting node-ID confirmation fails before inspection as in execution.

## Unreleased — standalone anchor inspection

- `anchor serve --inspect-target` (`rtc-bootstrap`) resolves listener/RTC/TLS
  selections and issuer fingerprint without reading PSK/TLS files, opening
  sockets, ordering certificates or minting an identity. Execution consumes
  the same resolution; malformed addresses fail before mesh startup.
- Existing binds and ephemeral identity behavior are unchanged. Profile
  identity/remote/bind defaults remain unused; runtime-assigned endpoints,
  TLS validity, authorization and reachability are not inspection claims.

## Unreleased — policy, pin and staging inspection

- Forwarding policy and MCP pin commands accept `--inspect-target`, resolving
  their actual explicit/default store without reading it, creating locks,
  or changing policy/consent. Profile `netdb` does not redirect these stores.
- Transfer send-blob/send-dir inspect their source and optional staging store
  without reading files/stdin or walking directories. Staging is not hosting.
- Inspection is resolution-only, not policy/content validation or approval.
  Keychain-backed `forwarding set-value` remains outside this surface.

## Unreleased — local target inspection and explicit selector validation

- Every dispatched command validates explicit config/profile selections,
  including environment selections, before work. Offline commands no longer
  silently ignore missing/malformed explicit config or unknown profiles.
  Remove obsolete selectors; implicit config is still not a new dependency
  for commands that do not use it. Parser-only help/version remain available.
- All NetDB commands accept `--inspect-target`, reporting the actual store
  and snapshot input/output paths without opening, creating or clearing them.
  Store precedence is unchanged: flag, profile, then data-directory default.
- Saved typegen input now supports `--inspect-target`; explicit remote
  target/bind flags remain invalid. Local inspection reports unused signing
  identity/remote defaults and does not read artifact payloads. It is not
  content validation, restore preflight or a writability guarantee.

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
