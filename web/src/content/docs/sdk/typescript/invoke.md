# TypeScript — Invoke a Capability

Discovery tells you who *can*; invoking does the work and returns a typed result.
The typed request/response surface is nRPC, via `TypedMeshRpc`.

## Serve and call over nRPC

The typed surface lives in the napi binding (`@net-mesh/core/mesh_rpc`); wrap a
`MeshNode`'s native handle with `TypedMeshRpc.fromMesh`:

```typescript
import { MeshNode } from '@net-mesh/sdk';
import { TypedMeshRpc } from '@net-mesh/core/mesh_rpc';

interface SummarizeReq  { text: string }
interface SummarizeResp { summary: string }

const psk = new Uint8Array(32).fill(0x42);
const server = await MeshNode.create({ bindAddr: '127.0.0.1:9001', psk });
const client = await MeshNode.create({ bindAddr: '127.0.0.1:9000', psk });
// (handshake the two nodes — see the mesh setup guide)

const summarize = (text: string): string => text.slice(0, 40);

// Provider side. NOTE: `_native` is an internal handle — sdk-ts does not yet
// re-export TypedMeshRpc, so you reach through it. This is the one place the docs
// touch a private field; expect a public accessor to replace it.
const serverRpc = TypedMeshRpc.fromMesh((server as unknown as { _native: unknown })._native);
const handle = serverRpc.serve<SummarizeReq, SummarizeResp>(
  'summarize',
  async (req) => ({ summary: summarize(req.text) }),
);

// Caller side — typed call with a deadline.
const clientRpc = TypedMeshRpc.fromMesh((client as unknown as { _native: unknown })._native);
const reply = await clientRpc.call<SummarizeReq, SummarizeResp>(
  server.nodeId(), 'summarize',
  { text: '…' },
  { deadlineMs: 500 },
);

await handle.close();   // MUST close — Node finalizers are non-deterministic
```

`call` addresses a specific node id; `callService` lets the mesh pick any provider
advertising the service (the basis for failover — see
[Errors](/docs/sdk/typescript/errors)). Response streaming
(`callStreaming`) and the resilience helpers are in
[Typed RPC with nRPC](/docs/guides/nrpc).

## Call a tool

For a served [tool](/docs/sdk/typescript/announce), the ergonomic path is
`callTool` (re-exported from `@net-mesh/sdk`), which finds a provider for the named
tool and returns the typed result — the TypeScript counterpart of Rust's
`call_tool`:

```typescript
import { callTool } from '@net-mesh/sdk';

const resp = await callTool(node, 'web_search', { query: 'how does the fold work' });
```

## Policy: invocation is authorized, discovery is not

Seeing a capability does not grant the right to invoke it. A provider enforces
scope at call time — an owner-only capability rejects a caller outside its scope,
verified against the authenticated origin, regardless of who can see it. For
wrapped MCP tools this is the owner-scope / consent model in
[Wrap an MCP Server](/docs/guides/wrap-mcp-server).
