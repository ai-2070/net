# TypeScript — Discover Capabilities

Query the mesh by what you need. Two surfaces: filter nodes by capability, or list
tools.

## Filter nodes by capability

```typescript
const peers: bigint[] = node.findNodes({
  requireTags: ['gpu'],
  minVramMb: 16_384,
});
```

`findNodes` is **synchronous** and returns matching node ids as `bigint[]` (node
ids are 64-bit, so they're BigInt in JS — see
[Payload interop](/docs/reference/capability-schema) for the u64/BigInt edge).
`findNodesScoped(filter, scope)` narrows to a tenant/region/subnet pool.

Announcements reach every **directly-connected** peer (the announcing node also
self-indexes) — multi-hop propagation is deferred, so a match is a direct neighbour,
not a node several hops away. Discovery is **advisory** — it tells you who *can*,
with no exclusivity.

## List tools

After a peer serves tools, they fold into your local index. Folding is
asynchronous, so poll until the tool you expect appears rather than assuming it's
there on the first call:

```typescript
import { listTools } from '@net-mesh/sdk';

const deadline = Date.now() + 3_000;
while (Date.now() < deadline && listTools(node).length < 1) {
  await new Promise((r) => setTimeout(r, 20));
}
for (const t of listTools(node)) {
  console.log(`${t.toolId} v${t.version}  tags=${t.tags}`);
}
```

For a long-running agent, prefer the push-based `watchTools(node)` subscription
(from `@net-mesh/sdk`) over a poll loop — it delivers tool changes as they fold in.
The poll above is fine for a one-shot wait at startup.

Tool descriptors lower to provider tool-call formats (e.g. an OpenAI `tools` array
entry) via the `openai` helpers, so a discovered tool feeds straight into a
chat-completion call.
