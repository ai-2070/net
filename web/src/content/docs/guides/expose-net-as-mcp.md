# Expose Net as MCP

The reverse of [wrapping](/docs/guides/wrap-mcp-server): `net mcp serve` runs a
stdio MCP **server** that exposes the mesh's capabilities to any local MCP host —
a desktop agent, an IDE — as a small set of meta-tools. The host doesn't need to
know Net exists; it just sees tools it can call.

## Run the shim

```
net mcp serve \
  --node-addr 203.0.113.7:4433 --node-pubkey 0x<peer-hex> --psk-hex <psk> \
  --identity ./operator.toml
```

Like `net wrap`, this builds a mesh node and joins via a peer, then speaks the MCP
stdio protocol on stdin/stdout. Register it in your MCP host's config as a stdio
server (the exact config shape is host-specific — it's the same way you'd register
any `command`-based MCP server).

**Run it under the same identity as your `net wrap` side** if you want to invoke
your own owner-only wrapped tools without an explicit allow — owner-scoped tools
admit callers by origin.

## The meta-tools the host sees

The shim exposes a fixed, small surface — the host discovers the *mesh* through
these, rather than seeing thousands of individual tools:

| Meta-tool | Does |
|---|---|
| `net_search_capabilities` | Find capabilities on the mesh by query. Returns summaries (id `provider/capability`, compat tier, credential/risk status). |
| `net_describe_capability` | Full detail for one capability: input/output schema, risk, provider. Describing never implies permission to invoke. |
| `net_invoke_capability` | Invoke a capability by id with typed arguments; returns the typed result. |
| `net_list_pinned_capabilities` | List capabilities the host has been granted permission to invoke. |
| `net_request_pin` | Request approval for a capability the model can't invoke yet — the model asks; a human approves out-of-band. |

An agent's loop through this surface is: `net_search_capabilities` →
`net_describe_capability` → `net_invoke_capability`, exactly the
discover → describe → invoke ladder from
[The Agentic Mesh](/docs/worldview/agentic-mesh).

## Consent: search is not invocation

Display never implies invocation. A **safe** capability (owner-scoped to you, no
credentials) can be invoked directly. A **spicy** one — credentialed, external, or
unknown-status — is **search/describe-only until it's pinned**. This is the
confused-deputy defense: connecting an MCP host to the mesh grants it *no* ambient
authority; every sensitive invocation is gated.

Approving a pin is the step the model **cannot** do for itself — a human does it
out-of-band:

```
net mcp pin approve provider/capability     # provider/capability is the id from a search result
net mcp pin list                            # see pending + approved pins
net mcp pin reject provider/capability      # remove one
```

The shim and the `pin` verbs share one per-user pin store, so an approval in one
terminal is honored by the running `net mcp serve` in another. To pre-approve
capabilities at startup instead, pass them on the serve command:

```
net mcp serve --allow-capability provider/capability --node-addr <peer> --node-pubkey <hex> --psk-hex <psk>
```

Without a pin or `--allow-capability`, `net_invoke_capability` on a spicy
capability returns a "not permitted — request a pin" result, and the model can
call `net_request_pin` to queue the human approval. Wrapper policy always wins:
a pin is *local client consent*, not remote authorization — if the remote
wrapper's owner scope excludes you, the pin doesn't override it.
