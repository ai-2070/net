// SDK audit 2026-10-11: some u64 BigInt arguments bypassed the strict
// conversion (-5n read as 5, 2n**64n+3n as 3), and a malformed cancelToken
// was silently dropped, leaving the call uncancellable.
import { afterEach, describe, expect, it } from 'vitest'

const binding: any = await import('../index')
const { NetMesh, MeshRpc } = binding

const meshes: any[] = []
const rpcs: any[] = []
afterEach(async () => {
  rpcs.splice(0).forEach((r) => r.close())
  await Promise.all(meshes.splice(0).map((m) => m.shutdown()))
})

describe('strict u64 arguments', () => {
  it('shardForStream rejects a negative or oversized stream id', async () => {
    const mesh = await NetMesh.create({ bindAddr: '127.0.0.1:0', psk: 'ab'.repeat(32) })
    meshes.push(mesh)
    expect(() => mesh.shardForStream(-5n)).toThrow(/non-negative/)
    expect(() => mesh.shardForStream(2n ** 64n + 3n)).toThrow(/u64/)
    expect(typeof mesh.shardForStream(5n)).toBe('number')
  })

  it('a malformed cancelToken rejects the call instead of being dropped', async () => {
    const mesh = await NetMesh.create({ bindAddr: '127.0.0.1:0', psk: 'cd'.repeat(32) })
    meshes.push(mesh)
    const rpc = MeshRpc.fromMesh(mesh)
    rpcs.push(rpc)
    await expect(
      rpc.call(mesh.nodeId(), 'svc', Buffer.from('x'), { cancelToken: -1n }),
    ).rejects.toThrow(/invalid_cancel_token/)
  })
})
