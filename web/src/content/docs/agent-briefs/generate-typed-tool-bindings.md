# Brief: Generate Typed Tool Bindings

**Goal.** Turn a tool your node discovered at runtime into typed, compile-checked
client code — and gate CI on breaking schema changes — using `net-mesh typegen`.

## Prerequisites

- The `net-cli` crate installed (it provides the `net-mesh typegen` binary): `cargo install net-cli`.
- A node whose capability fold has discovered at least one `ai-tool:*` tool (i.e.
  a peer served a tool — see [Announce](/docs/sdk/rust/announce)).

## Steps

1. **Generate bindings** for the discovered tools you want, filtered by tag or id:
   ```
   net-mesh typegen generate --language ts --tag weather --out ./generated
   # or Python:
   net-mesh typegen generate --language python --tool acme/web-search --out ./generated
   ```
   `--tag` and `--tool` are repeatable and compose as OR; with neither, every
   discovered tool is emitted.

2. **Pin a snapshot** so regeneration is deterministic and CI is hermetic (no live
   mesh query):
   ```
   net-mesh typegen snapshot --tag weather --out tools.snapshot.json
   net-mesh typegen generate --language ts --from-snapshot tools.snapshot.json --out ./generated
   ```

3. **Gate CI on breaking changes.** In CI, capture a fresh snapshot from the live
   mesh, then diff the committed one against it:
   ```
   net-mesh typegen snapshot --tag weather --out new.snapshot.json
   net-mesh typegen diff --from tools.snapshot.json --to new.snapshot.json --exit-code
   ```

## Expected output

- Step 1 writes one module per tool under `./generated`: TypeScript interfaces (or
  Pydantic v2 models), a typed call helper (`callAcmeWebSearch(mesh, request)` /
  `call_acme_web_search(mesh, request)`), and a `…Meta` constant with the
  descriptor metadata.
- Step 3 prints added/removed tools and field-level schema deltas, marking breaking
  ones `[BREAKING]`.

## Verify (acceptance)

- [ ] The generated module compiles against `@net-mesh/core` (TS) or with
      `net-mesh` installed (Python), and the call helper's request/response types
      match the tool's schema.
- [ ] `net-mesh typegen diff --exit-code` **exits 14** when you introduce a breaking
      change to a tool's schema, and **exits 0** otherwise — so CI fails loudly on a
      contract break.
- [ ] Regenerating `--from-snapshot` with no mesh reachable still succeeds
      (hermetic).

## Pitfalls

- **Codegen is convenience, not a contract.** The wire stays schemaless JSON; the
  generated types are a compile-time aid, not an IDL the mesh enforces. Two sides
  still just need to agree on the shape.
- **Commit the snapshot.** Generating from a live mesh in CI is non-deterministic;
  `--from-snapshot` is what makes builds reproducible.
- Exit code `14` is specifically the `diff --exit-code` breaking-change signal —
  don't confuse it with the generic `1` (arg/parse error). See the
  [CLI Reference](/docs/reference/cli) exit-code table.

Background: [Expose Net as MCP](/docs/guides/expose-net-as-mcp) and the typed-tools
section of [Typed RPC with nRPC](/docs/guides/nrpc).
