// Blob types through @net-mesh/sdk (NODE_SDK_GAPS_PLAN.md S4).
//
// MeshNode.serveBlobTransfer / storeDir / fetchDir took a MeshBlobAdapter,
// but the SDK exported no way to build one: a caller had to import from
// @net-mesh/core. The witness here is that nothing below does — every import
// is from the SDK root (`../src/index`).

import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { afterAll, describe, expect, it } from 'vitest';

import {
  MeshBlobAdapter,
  MeshNode,
  Redex,
  blobAdapterRegistered,
  createMeshBlobAdapter,
  registerFilesystemBlobAdapter,
  unregisterBlobAdapter,
} from '../src/index';

const PSK = '42'.repeat(32);
const tmp = mkdtempSync(join(tmpdir(), 'net-sdk-blob-'));
afterAll(() => rmSync(tmp, { recursive: true, force: true }));

async function connectedPair(): Promise<[MeshNode, MeshNode]> {
  const a = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK });
  const b = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK });
  await Promise.all([
    b.accept(a.nodeId()),
    (async () => {
      await new Promise((r) => setTimeout(r, 50));
      await a.connect(b.localAddr(), b.publicKey(), b.nodeId());
    })(),
  ]);
  await a.start();
  await b.start();
  return [a, b];
}

describe('blob surface', () => {
  it('createMeshBlobAdapter takes the SDK Redex and yields a MeshBlobAdapter', () => {
    const adapter = createMeshBlobAdapter(new Redex(), 'sdk-blob-shape');
    expect(adapter).toBeInstanceOf(MeshBlobAdapter);
    expect(adapter.adapterId).toBe('sdk-blob-shape');
  });

  it('storeDir on one node, fetchDir from the other, files round-trip', async () => {
    const [a, b] = await connectedPair();
    try {
      const src = join(tmp, 'src');
      mkdirSync(join(src, 'sub'), { recursive: true });
      writeFileSync(join(src, 'hello.txt'), 'hello');
      const data = Buffer.alloc(2048, 7);
      writeFileSync(join(src, 'sub', 'data.bin'), data);

      const holder = createMeshBlobAdapter(new Redex(), 'sdk-blob-b');
      b.serveBlobTransfer(holder);
      // The fetching node needs the transfer engine too.
      a.serveBlobTransfer(createMeshBlobAdapter(new Redex(), 'sdk-blob-a'));

      const manifest = await b.storeDir(holder, src);
      const dest = join(tmp, 'dest');
      const stats = await a.fetchDir(b.nodeId(), manifest, dest);

      expect(stats.files).toBe(2n);
      expect(stats.bytes).toBe(BigInt(5 + data.length));
      expect(readFileSync(join(dest, 'hello.txt'), 'utf8')).toBe('hello');
      expect(readFileSync(join(dest, 'sub', 'data.bin')).equals(data)).toBe(true);
    } finally {
      // The fetcher leaves first and the holder shuts down at once. Before
      // the core fix (serve_chunk now holds the node weakly across its
      // graceful close), the holder stayed pinned for ~7 s and this shutdown
      // threw "cannot shutdown: outstanding references exist".
      await a.shutdown();
      await b.shutdown();
    }
  }, 30_000);

  it('the filesystem adapter registry is separate from MeshBlobAdapter', () => {
    // registerFilesystemBlobAdapter registers a GLOBAL BlobAdapter by id; it
    // does not build the MeshBlobAdapter storeDir / fetchDir take.
    const id = `sdk-fs-${process.pid}`;
    expect(blobAdapterRegistered(id)).toBe(false);
    registerFilesystemBlobAdapter(id, join(tmp, 'fs-store'));
    expect(blobAdapterRegistered(id)).toBe(true);
    expect(unregisterBlobAdapter(id)).toBe(true);
    expect(blobAdapterRegistered(id)).toBe(false);
  });
});
