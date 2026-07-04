# TypeScript — Announce a Capability

A capability is a typed unit of work a node can do. You announce it; peers fold it
into their index; anyone can then discover and invoke it.

## Announce capabilities

```typescript
import { MeshNode } from '@net-mesh/sdk';

const node = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk });

await node.announceCapabilities({
  tags: ['gpu', 'inference', 'region:eu-west'],
  // hardware / models / tools are additional fields on the set
});
```

After `announceCapabilities`, peers that fold the announcement can find this node
by tag ([Discover](/docs/sdk/typescript/discover)). Re-announce to update; the mesh
diffs your last set, so steady-state changes are cheap. The default TTL is 5
minutes — re-announce before it elapses or peers GC the entry.

The full capability shape (hardware, models, tools, resource limits) is in
[Capabilities](/docs/concepts/capabilities) and
[Capability Schema](/docs/reference/capability-schema).

## Serve a callable tool

For a tool an agent can invoke, use the tool surface — the TypeScript counterpart
of Rust's `#[tool]` macro:

```typescript
import { serveTool } from '@net-mesh/sdk';

const handle = serveTool(node, {
  name: 'web_search',
  description: 'Search the web for relevant pages.',
  tags: ['web', 'research'],
}, async (req: { query: string }) => {
  return { results: [`first hit for '${req.query}'`] };
});
// handle.close() when done — always close explicitly.
```

`serveTool` registers the handler and makes the tool discoverable via `listTools`
on peer nodes. (`serveTool` / `callTool` / `listTools` are re-exported from
`@net-mesh/core` in lockstep with the binding.)

## Policy: announced ≠ invocable

Announcing makes a capability **discoverable**, not open. Visibility and invocation
are separate — a credentialed capability can be visible while invocable only by its
owner. The boundary is enforced on invoke, not announce — see
[Invoke](/docs/sdk/typescript/invoke).
