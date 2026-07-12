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

Announcements propagate multi-hop (bounded by a hop count), so a match can be a
node several hops away, not just a direct neighbor. Discovery is **advisory** — it
tells you who *can*, with no exclusivity.

## List tools

After a peer serves tools, they fold into your local index. Folding is
asynchronous — don't poll for it: take a `listTools` baseline, then subscribe with
`watchTools` and react to pushed changes. The watch is event-driven off the
capability fold's change signal — a `ToolListChange` arrives the moment a tool is
added, removed, or its publisher count changes, and an idle mesh costs zero
periodic work:

```typescript
import { listTools, watchTools } from '@net-mesh/sdk';

for (const t of listTools(node)) {
  console.log(`${t.toolId} v${t.version}  tags=${t.tags}`); // baseline
}

const controller = new AbortController();
for await (const change of watchTools(node, { signal: controller.signal })) {
  console.log(change); // pushed on fold mutation — no timer, no re-diff
}
```

`options.intervalMs` is a client-side staleness ceiling (a safety-net re-diff at
least that often), **not** a poll rate — leave it unset for pure event-driven
behavior.

Tool descriptors lower to provider tool-call formats (e.g. an OpenAI `tools` array
entry) via the `openai` helpers, so a discovered tool feeds straight into a
chat-completion call.
