## Invoke it — TypeScript

### Call a tool

```typescript
import { TypedMeshRpc } from '@net-mesh/core/mesh_rpc';
import { callTool } from '@net-mesh/sdk';

const native = (node as unknown as { _native: object })._native;
const rpc = TypedMeshRpc.fromMesh(native);

const resp = await callTool<{ query: string }, { results: string[] }>(
  rpc,
  'web_search',
  { query: 'how does the capability fold work' },
  { deadlineMs: 500 },
);
```

`callTool(rpc, toolId, req, opts)` — the first argument is the **`TypedMeshRpc`**,
not the `MeshNode`. This is the same handle `serveTool` takes on the
[announce](/docs/sdk/typescript/announce) page, and the same asymmetry:
`listTools` wants the native handle, the tool call wants the typed RPC over it.

### Serve and call over nRPC directly

```typescript
interface SummarizeReq  { text: string }
interface SummarizeResp { summary: string }

const serverRpc = TypedMeshRpc.fromMesh((server as unknown as { _native: object })._native);
const handle = serverRpc.serve<SummarizeReq, SummarizeResp>(
  'summarize',
  async (req) => ({ summary: req.text.slice(0, 40) }),
);

const clientRpc = TypedMeshRpc.fromMesh((client as unknown as { _native: object })._native);
const reply = await clientRpc.call<SummarizeReq, SummarizeResp>(
  server.nodeId(),
  'summarize',
  { text: '…' },
  { deadlineMs: 500 },
);

await handle.close();   // MUST close — Node finalizers are non-deterministic
```

`call` pins a node id (a `bigint`). `callService(service, req, opts)` lets the mesh
resolve the service through the capability index, which is what makes failover to
a standby possible.

The deadline is `deadlineMs` — plain milliseconds, unlike Rust's absolute
`Instant`. `opts.signal` accepts an `AbortSignal` for caller-side cancellation.

### A request that will not decode

The handler's request decode failure is not your exception to catch — it is
converted into a typed bad-request status the *caller* sees:

```typescript
import { classifyError, RpcServerError } from '@net-mesh/core/errors';
import { NRPC_TYPED_BAD_REQUEST } from '@net-mesh/core/mesh_rpc';

try {
  await clientRpc.call(nodeId, 'summarize', { wrong: 'shape' }, { deadlineMs: 500 });
} catch (e) {
  const typed = classifyError(e);
  if (typed instanceof RpcServerError && typed.status === NRPC_TYPED_BAD_REQUEST) {
    // the provider rejected the request shape — a bug in the caller, not a retry
  }
}
```

### Verify it worked

```typescript
const resp = await callTool<{ query: string }, { results: string[] }>(
  rpc, 'web_search', { query: 'ping' },
);
if (resp.results.length === 0) throw new Error('the provider answered, but with nothing');
console.log(`invoked: ${resp.results.length} result(s)`);
```

Next: [Watch the event stream](/docs/sdk/typescript/watch).
