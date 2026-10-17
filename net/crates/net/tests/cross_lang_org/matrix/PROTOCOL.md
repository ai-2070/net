# Cross-process, cross-language org matrix: the driver protocol

`run_matrix.py` runs every caller language against every provider language
(Rust, Go, Python, Node), each pair as **two separate OS processes**, over a
real mesh, for one cross-org **Granted** call per shape: `unary` and
`server-streaming`. Each language ships one driver program that plays either
role. This file is the contract every driver implements; the orchestrator is
the only thing that knows about more than one language.

## Scenario

Each run gets a fresh scenario directory written by
`cargo run -p net-mesh-sdk --features net,cortex --example gen_org_scenario -- <dir>`.
`<dir>/manifest.json` (see `sdk/src/org/fixtures.rs`,
`CrossOrgScenarioManifest`) gives:

- `psk_hex` — the mesh PSK.
- `granted_service` — the service both sides use (`customer.read`).
- `provider.{seed_hex, org_id_hex, authority_dir, grant_path, grant_secret_path}`
- `caller.{seed_hex, org_id_hex, authority_dir, membership_path, dispatcher_path, grant_path, grant_secret_path}`

Paths are relative to `<dir>`. Files ending in `_path` hold raw wire bytes
(grant, membership, dispatcher), except `grant_secret_path`, which is passed
**by path** to the binding (the raw audience key never enters the driver).

## Payloads (identical bytes in every language)

All payloads are UTF-8 JSON, compact, no spaces.

| Shape | Request body | Response |
|---|---|---|
| `unary` | `{"n":7}` | one body: `{"n":8}` |
| `server-streaming` | `{"n":7}` | exactly three items, in order: `{"i":0}`, `{"i":1}`, `{"i":2}`, then a clean end |

The provider handler parses the request as JSON and checks `n == 7`. The
caller compares responses after parsing JSON (not byte-for-byte), so key
order and whitespace in a binding's encoder do not matter.

## Command line

```
<driver> provider --manifest <dir> --shape <unary|server-streaming> --caller-node-id <u64>
<driver> caller   --manifest <dir> --shape <unary|server-streaming>
```

The driver is any executable the orchestrator can start: a Python script, a
`node` script, a Go binary, or a Rust example binary. Extra flags are an error.

## Lines (stdout is the protocol; stderr is free-form diagnostics)

Every protocol line is one line on **stdout**, flushed immediately. Anything
else a driver wants to say goes to stderr.

### Caller

1. Build the mesh node from `caller.seed_hex` and `psk_hex`, bound to
   `127.0.0.1:0`, heartbeat 200 ms where the binding allows. Do NOT start it.
2. Print `ID <node_id>` (the node's u64 id, decimal).
3. Read one stdin line: `PROVIDER <addr> <public_key_hex> <node_id>`.
4. `connect(addr, public_key, node_id)` (connect only; never `accept`), then
   `start()`, then install `caller.authority_dir` as the org authority.
5. Bind org credentials from `membership_path`, `dispatcher_path`,
   `[grant_path]`, `[grant_secret_path]`.
6. Call `granted_service` with the request for the shape. Discovery takes a
   few seconds: while the call fails with a LOCAL discovery error (no request
   was sent; the binding classifies it as the `discovery` domain), retry every
   500 ms for up to 60 s. Never retry any other failure: an admission denial
   or transport error is a result.
7. Check the response for the shape.
8. Print `RESULT ok` or `RESULT fail <one-line detail>`, then shut the node
   down and exit (0 for ok, 1 for fail).

### Provider

1. Build the mesh node from `provider.seed_hex` and `psk_hex`, bound to
   `127.0.0.1:0`, heartbeat 200 ms where the binding allows.
2. Install `provider.authority_dir` as the org authority.
3. Start `accept(<caller-node-id>)` in the background.
4. Print `READY <addr> <public_key_hex> <node_id>`: the bound address as
   `ip:port`, the Noise static public key as 64 lowercase hex, the u64 id.
5. Wait for the accept to complete (fail after 30 s), then `start()`.
6. Serve `granted_service` as **Granted** with the handler for the shape,
   THEN install the provider grant audience (`provider.grant_path` bytes +
   `provider.grant_secret_path` path). Registration first, audience second.
   Keep the serve handle alive until step 9.
7. Re-announce capabilities about once a second until the handler has run
   (some bindings throttle scoped emission, and the caller needs it to
   discover the provider).
8. When the handler runs, record the verified caller facts it was given and
   check them: `acting_org` hex == `caller.org_id_hex`, `provider_org` hex ==
   `provider.org_id_hex`, and the call is NOT same-org. Print
   `FACTS ok` or `FACTS fail <detail>`.
9. Read one stdin line, `DRAINED` (the orchestrator sends it after the caller
   printed its `RESULT`). Only then drop the serve handle and shut down: a
   serve handle dropped while a stream is still draining retires the stream
   with a CANCEL terminal instead of a clean end.
10. Print `RESULT ok calls=1` (the handler ran exactly once and the facts
    held) or `RESULT fail <detail>`, then exit (0 / 1).

A provider whose handler never runs within 120 s prints
`RESULT fail callback-loss` and exits 2.

## Orchestration (what `run_matrix.py` does per pair and shape)

```
gen scenario → start caller → read ID → start provider (--caller-node-id ID)
→ read READY → write "PROVIDER …" to caller stdin → read caller RESULT
→ write "DRAINED" to provider stdin → read provider FACTS and RESULT
```

Every read has a timeout. A pair passes when the caller printed `RESULT ok`,
the provider printed `FACTS ok` and `RESULT ok calls=1`, and both exited 0.
The orchestrator prints a caller × provider table per shape and exits
non-zero if any cell failed or did not run.
