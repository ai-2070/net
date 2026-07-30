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
import { TypedMeshRpc } from '@net-mesh/core/mesh_rpc';
import { serveTool, descriptorFrom, addToolCapabilitiesToAnnounce } from '@net-mesh/sdk';

// The whole tool surface — serve, call, list, watch — lives on the NATIVE mesh
// handle, not on the ergonomic `MeshNode` that wraps it. `@net-mesh/sdk` has no
// public accessor for it yet, so it is reached through `_native`. This is the
// one private field the docs touch; expect a public accessor to replace it.
const native = (node as unknown as { _native: object })._native;
const rpc = TypedMeshRpc.fromMesh(native);

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

const agentNative = (agent as unknown as { _native: object })._native;

const deadline = Date.now() + 3000;
while (Date.now() < deadline && listTools(agentNative).length === 0) {
  await new Promise((r) => setTimeout(r, 20));
}
if (listTools(agentNative).length === 0) {
  throw new Error('the announcement did not fold');
}
```

Three surfaces, and the difference is not cosmetic: `announceCapabilities` is a
method on the **`MeshNode`**, `listTools` takes the **native handle**, and
`serveTool` takes a **`TypedMeshRpc`** built over that handle. Reach for the wrong
one and TypeScript will tell you, which is the good case — the same mistake in
Python surfaces as an `AttributeError` at call time.

Next: [Discover a capability](/docs/sdk/typescript/discover).
