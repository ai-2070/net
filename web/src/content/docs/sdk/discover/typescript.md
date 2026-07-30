## Discover it — TypeScript

### Filter nodes by capability

```typescript
const peers: bigint[] = node.findNodes({
  requireTags: ['gpu'],
  minVramMb: 16_384,
});
```

`findNodes` is a method on the **`MeshNode`** and is **synchronous** — it reads the
local index. Node ids come back as `bigint[]` because they are 64-bit; `===`
against a number literal is always false.

`findNodesScoped(filter, scope)` narrows to a tenant, region or subnet pool.

### List tools

`listTools` and `watchTools` take the **native** mesh handle, not the `MeshNode`:

```typescript
import { listTools, watchTools } from '@net-mesh/sdk';

const native = (node as unknown as { _native: object })._native;

for (const t of listTools(native)) {
  console.log(`${t.toolId} v${t.version}  tags=${t.tags}`);   // baseline
}
```

That asymmetry — `findNodes` on the node, `listTools` on the handle underneath it —
is the current state of the binding rather than a convention. There is no public
accessor for the native handle yet.

Schemas arrive as **JSON-encoded strings** on `descriptor.inputSchema` and
`descriptor.outputSchema`. Call `JSON.parse` before handing them to anything that
expects an object.

### Watch for changes

```typescript
const controller = new AbortController();

for await (const change of watchTools(native, { signal: controller.signal })) {
  console.log(change);   // pushed on fold mutation — no timer, no re-diff
}
```

`options.intervalMs` is the staleness ceiling; leave it unset for pure event-driven
behaviour. Abort the signal to end the loop — the iterator subscribes eagerly at
call time, so a change published before you start iterating is still observed.

### Lower a descriptor for an LLM

```typescript
import { openai } from '@net-mesh/sdk';

const lowered = listTools(native).map(openai.toOpenaiTool);
```

`anthropic`, `mcp` and `gemini` are namespaced objects beside it, each with a
`lower*` counterpart that parses a model's reply into a call spec.

### Verify it worked

```typescript
const found = listTools(native).some((t) => t.toolId === 'web_search');
if (!found) {
  throw new Error('web_search did not fold — is the pair handshaked and started?');
}
```

Next: [Invoke a capability](/docs/sdk/typescript/invoke).
