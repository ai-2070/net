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
The native typed provider/caller and live-contract capture/offline-reuse journey
remain subsequent acceptance slices; passing this harness does not close them.
