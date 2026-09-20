# Two-node capability journey

From a source checkout, install the pinned Rust toolchain (currently 1.98.1),
Python 3 (`python` on Windows, `python3` elsewhere), and cargo-nextest. No Python
packages, running mesh, credentials, or extra CLI features are required.
Run from `net/crates/net/`:

```sh
cargo nextest run -p net-cli --test capability_workflow --no-tests=fail --retries 0
```

This is the runnable harness in `../capability_workflow.rs`, with its MCP client
in `capability_client.rs` and stdio server in `capability_server.py`. CI's Net CLI
job also runs it through `cargo test -p net-cli`, in default and rtc-bootstrap
configurations. The optional feature does not change this UDP topology.

## What runs where

There are exactly two mesh participants, each a separate CLI process:

1. The harness uses `identity generate` to create two fresh identities in an
   isolated temporary directory. It supplies an isolated config and pin store,
   never your normal profile. The fixed test PSK is disposable fixture data,
   not a deployment credential.
2. `wrap journey --listen --identity <provider> --psk-hex <test-psk> -- ...`
   starts the Python server. It defaults to loopback with an OS-assigned port.
   The server appends to `invocations.jsonl` only when its handler is invoked.
   The positive test also supplies `--allow <consumer-origin>`; the negative
   test deliberately keeps owner-only policy. No blanket admission is enabled.
3. The harness reads the `wrapped` event, verifies the provider's node ID, and
   uses its live address and Noise key to launch `mcp serve --identity
   <consumer> --pin-store <isolated-store> --node-addr <bind> --node-id <id>
   --node-pubkey <key> --psk-hex <test-psk> --bind 127.0.0.1:0`.
4. The supplied client initializes MCP over that process's stdin/stdout. In
   the positive case it polls **discovery only**, bounded to 25 seconds, for
   the exact provider's `journey_echo` capability. An invocation without local
   approval is refused and creates no provider record. A separate CLI
   `mcp pin approve <decimal-provider-node-id>/journey_echo --pin-store <store>`
   grants consumer consent. One subsequent invocation returns the deterministic
   message, matching exactly one provider record. No invocation is retried.
5. The negative case knows the capability ID from publisher readiness: owner
   scope also gates the catalog, so an unauthorized consumer cannot discover
   it. Even after explicit local pin approval, invocation is denied by the
   provider and creates no handler record. A pin is not provider authority.
6. The harness stops the Python server through a local control socket, observes
   `server_exited` from `wrap`, and waits for the publisher to exit. Closing the
   MCP input ends the consumer. Child guards kill surviving direct CLI children
   on failure; temporary identities, logs and pins are removed with the fixture.

## Failure and evidence limits

Every protocol request and subprocess wait has a ceiling. A readiness/discovery
timeout means the journey did not establish availability; an invocation timeout
does **not** prove the handler had no effect. Inspect the provider record rather
than automatically retrying an uncertain operation. CLI startup timeout does
not bound later MCP invocation lifetime; the harness supplies its own ceiling.

This proves two-process, two-node loopback behavior—not two computers, arbitrary
packet-loss recovery, production credential management, or process-tree cleanup
for arbitrary wrapped programs. The Python fixture has no descendants.
The native contract leg below complements this MCP policy witness; it does not
inherit MCP's authorization rules.

## Native typed calls and contract reuse

The second harness uses the public Rust SDK `serve_tool` / `call_typed` path,
CLI live capture, and a generated Python client. Install Pydantic v2 into the
Python interpreter on PATH, then run from `net/crates/net/`:

```sh
python -m pip install "pydantic>=2,<3"
cargo nextest run -p net-cli --test native_contract_workflow --no-tests=fail --retries 0
```

Use `python3` for the installation command on Unix. Missing Python/Pydantic is
a failure, not a skipped witness. CI supplies Python 3.12 and Pydantic v2.

The runnable source is `../native_contract_workflow.rs`; generated consumption
is in `native_consumer.py`. It performs these bounded stages:

1. Start one SDK provider, then a separate `typegen snapshot` CLI process with
   its explicit peer address, Noise key, node ID and test PSK. Observe attachment
   before registering and announcing `native_echo`. This sequencing avoids
   racing the default announcement coalescer against the CLI's five-second
   discovery window; it does not claim arbitrary startup order succeeds.
2. Capture input **and** output schemas and compare them to the served
   descriptor. Capture must leave the business-handler record empty. The small
   schemas may arrive inline; this is not another large-metadata hydration test.
3. Generate Python and TypeScript from that snapshot while the provider is
   alive. The capture CLI has exited before the SDK caller starts, so no stage
   has more than two live mesh nodes. The Rust provider and caller run within
   the harness process with separate identities, UDP binds and sessions.
4. A malformed native request gets the exact typed bad-request status before
   handler effect. A valid Rust typed call returns the expected message and
   provider node ID. The generated Python request rejects invalid input locally.
   Its call helper then sends one valid request through a one-connection local
   TCP adapter that forwards to the Rust SDK caller's real `call_typed` method.
   The adapter never synthesizes the provider response. Compare both successful
   responses, in order, with exactly two provider-side records; do not retry.
5. Stop caller and provider. Regenerate **every** Python and TypeScript file
   from the same saved snapshot into fresh directories and compare bytes.

This executes the generated Python models and call helper, **not** the native
Python SDK binding. TypeScript is regenerated and compared, not invoked in this
leg. The local adapter is fixture plumbing, not a shipped CLI RPC command.
Offline regeneration proves contract reuse, never offline invocation.

**Authorization limit:** this native example deliberately uses a public
`serve_tool` service on an isolated, disposable-PSK mesh. Typed validation is
not caller authorization; it does not demonstrate org grants, private native
capabilities, or an identity-based native denial. The MCP leg above proves its
own owner/consent boundary. The separate protected leg below supplies native
authorization evidence; do not deploy this public echo as a sensitive capability.

## Protected native authorization

From `net/crates/net/`:

```sh
cargo nextest run -p net-cli --test protected_native_workflow --no-tests=fail --retries 0
```

The runnable source is `../protected_native_workflow.rs`. It creates two SDK
nodes with distinct identities and UDP binds, adopts both into one disposable
organization, and pre-stages the same owner-discovery audience in their secured
authority files. `install_org_authority` loads those files through the production
SDK path. The offline dispatcher grant covers exactly `nrpc:protected.echo`,
not all capabilities. Test-only keys and authority files are never printed.

The provider registers `serve_org(..., OrgAccess::SameOrg, ...)`; discovery stays
encrypted/private. The harness polls discovery and authenticated peer pins with
a 25-second ceiling, without shortening production timers or invoking handlers.
A typed request to the exact provider **without** an org proof receives wire
status `0x0009` (admission denied), and the provider record must remain empty.
The same caller then uses `mesh.org(credentials).call(...)` exactly once. Its
typed response matches the one provider record, including all five verified
`OrgCaller` fields: caller entity, acting org, provider org, provider entity and
capability. Each RPC has a five-second ceiling; ambiguous calls are never retried.

Both nodes shut down before temporary authority files are removed. This binary
contains one test so no subsequent adoption can recycle a deleted revocation
lock inode within the process. This is same-org native admission evidence on
loopback, not cross-org grant, revocation, generated protected-client, or
two-computer acceptance evidence. It adds no CLI RPC command or SDK API.
