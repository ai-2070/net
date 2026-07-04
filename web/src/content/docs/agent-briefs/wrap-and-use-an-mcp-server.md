# Brief: Wrap and Use an MCP Server

**Goal.** Take an existing stdio MCP server, put its tools on a Net mesh as
discoverable capabilities, and invoke one from a local MCP host — proving the
[MCP wedge](/docs/worldview/mcp-vs-net) end-to-end.

## Prerequisites

- The `net-mesh` binary built with the CLI feature:
  `cargo install net-mesh --features cli` (or `cargo build --release --features cli`).
- A stable operator identity file (`operator.toml` with `seed_hex = "..."`). Owner-only
  scoping keys on it, so it must not be ephemeral.
- **A running mesh peer to join.** `net wrap` builds a node and *joins* the mesh via
  a peer — it does not bootstrap the first node. If you don't have one, stand up a
  bootstrap node via the SDK ([Discover and Invoke](/docs/guides/discover-and-invoke))
  and note its `--node-addr` / `--node-pubkey` / `--psk-hex`. There is no `net up`
  one-liner yet — do not invent one.

## Steps

1. **Wrap the server** (supply side). On the node that owns the tool:
   ```
   net wrap github \
     --node-addr <peer-ip:port> --node-pubkey 0x<peer-hex> --psk-hex <psk> \
     --identity ./operator.toml \
     --env GITHUB_TOKEN=ghp_xxx \
     -- npx -y @modelcontextprotocol/server-github
   ```
   Credentials in `--env` stay in the child process on this machine; they never
   transit the mesh.

2. **Serve the mesh to a host** (demand side). On the node with your MCP host, run
   the shim under the **same identity** so owner-only tools are invocable without an
   explicit allow:
   ```
   net mcp serve --node-addr <peer> --node-pubkey 0x<hex> --psk-hex <psk> --identity ./operator.toml
   ```
   Register this command as a stdio MCP server in your host's config.

3. **Discover and invoke** from the host: call `net_search_capabilities`, then
   `net_describe_capability`, then `net_invoke_capability` with the tool's id
   (`provider/tool`).

## Expected output

- Step 1 emits a `wrapped` event listing the served tools (e.g. `github`'s tool
  set), the announced scope (owner identity), and empty `allowed_origins`.
- Step 3: `net_search_capabilities` returns a row with `compat_tier: "mcp_bridge"`;
  `net_invoke_capability` returns the tool's typed result.

## Verify (acceptance)

- [ ] The wrapped tool appears in `net_search_capabilities` output from the *other*
      node — discovery crossed the mesh.
- [ ] `net_invoke_capability` returns a real result (not a "not permitted" row).
- [ ] A node with a **different** root identity that runs `net mcp serve` can
      *search/describe* the tool but its `net_invoke_capability` is refused —
      owner-only holds, and display did not imply invocation.

## Pitfalls

- **`mcp_bridge` is request/response only** — no streams, artifacts, or migration.
  If the task needs those, it needs a native capability, not a wrapped one.
- A **spicy** capability (credentialed / external / unknown) is search/describe-only
  until pinned. Approve it out-of-band: `net mcp pin approve provider/tool`. The
  model cannot approve its own pin.
- If invoke is refused unexpectedly, check that the serve side runs under the *same
  identity* as the wrap side, or widen with `net wrap --allow 0x<caller-origin>`.

See [Wrap an MCP Server](/docs/guides/wrap-mcp-server) and
[Expose Net as MCP](/docs/guides/expose-net-as-mcp) for the full command surface.
