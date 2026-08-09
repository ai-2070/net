## Announce it — TypeScript

### Tags

```typescript
import { MeshNode } from '@net-mesh/sdk';

const node = await MeshNode.create({ bindAddr: '127.0.0.1:9001', psk });

await node.announceCapabilities({
  tags: ['gpu', 'inference', 'region:eu-west'],
});
```

`announceCapabilities` takes a `CapabilitySet` — `tags` plus optional `hardware`,
`models`, `tools` and `limits` fields. It self-indexes, so `findNodes` on this same
node matches its own announcement.

### Serve a tool

The tool API takes a **`TypedMeshRpc`**, not the node:

```typescript
import { serveTool, descriptorFrom, addToolCapabilitiesToAnnounce } from '@net-mesh/sdk';

// The tool surface — serve, call, list, watch — hangs off the RPC handle
// rather than the node. `node.rpc()` is the bridge.
//
// Hold the handle rather than calling `rpc()` per tool: each call builds a
// new one with its own reference to the mesh, and an outstanding reference
// makes `shutdown()` fail. Release it with `rpc.raw.close()` when done.
const rpc = node.rpc();

const options = {
  name: 'web_search',
  description: 'Search the web for relevant pages.',
  tags: ['web', 'research'],
};

const handle = serveTool(rpc, options, async (req: { query: string }) => {
  return { results: [`first hit for '${req.query}'`] };
});
```

`serveTool(rpc, options, handler)` — first argument is the RPC surface. Passing the
node here is the most common mistake on this page, and it is a type error rather
than a runtime one, so the compiler will catch it.

### Announce the tool — the step `serveTool` does not do

```typescript
await node.announceCapabilities(
  addToolCapabilitiesToAnnounce({ tags: [] }, [descriptorFrom(options)]),
);
```

`addToolCapabilitiesToAnnounce` adds an `ai-tool:<toolId>` tag and a `tools[]`
entry to the capability set you are about to announce. **Without it the handler is
served and invisible** — `listTools` on a peer returns nothing, and a `callTool`
against the name fails to route.

`handle.close()` when you are done. Node's finalizers are non-deterministic, so an
unclosed handle keeps serving for an unspecified length of time.

### Verify it worked

From a peer that folded the announcement:

```typescript
import { listTools } from '@net-mesh/sdk';

const deadline = Date.now() + 3000;
while (Date.now() < deadline && listTools(agent).length === 0) {
  await new Promise((r) => setTimeout(r, 20));
}
if (listTools(agent).length === 0) {
  throw new Error('the announcement did not fold');
}
```

Folding is asynchronous, so the loop is the point — an immediate `listTools`
after a peer announces will usually be empty, and that is not a failure.

Two surfaces, and the difference is not cosmetic: `announceCapabilities` and
`listTools` work on the **`MeshNode`**, while `serveTool` takes the
**`TypedMeshRpc`** that `node.rpc()` returns. Reach for the wrong one and
TypeScript will tell you, which is the good case — the same mistake in Python
surfaces as an `AttributeError` at call time.

Next: [Discover a capability](/docs/sdk/typescript/discover).
