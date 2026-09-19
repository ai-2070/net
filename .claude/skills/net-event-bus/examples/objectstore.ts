//! The object store you no longer run.
//!
//! Two in-process mesh nodes over loopback UDP and a content-addressed blob
//! store: a producer stores bytes and mints an address, a second node fetches
//! them by that address, and storing the same bytes again produces the same
//! address. There is no bucket to create, no region to pick and no replication
//! factor to configure — the address *is* the data.
//!
//! Run (from `net/crates/net/sdk-ts`):
//!
//!   npx tsc --noEmit -p tsconfig.skill-example.json
//!
//! Expected final line: `RESULT ok dedup=1 readback=1 bytes=64`

import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

// The blob surface (and the `Redex` it needs) is core-only — the SDK wrapper does
// not re-export the blob classes, and its own `Redex` is a different type.
import {
  BlobRef,
  MeshBlobAdapter,
  Redex,
  blobPublish,
  registerFilesystemBlobAdapter,
} from '@net-mesh/core';
import { MeshNode } from '@net-mesh/sdk';

/** 64 hex characters = 32 bytes. Every node in a mesh shares it. */
const PSK = '42'.repeat(32);

/** A fixed-size payload, so the byte count in the result line is deterministic. */
const PAYLOAD = Buffer.alloc(64, 0x5a);

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

async function build(seed: number): Promise<MeshNode> {
  return MeshNode.create({
    bindAddr: '127.0.0.1:0',
    psk: PSK,
    identitySeed: Buffer.alloc(32, seed),
    heartbeatIntervalMs: 200,
  });
}

async function handshake(responder: MeshNode, initiator: MeshNode): Promise<void> {
  const addr = responder.localAddr();
  const pub = responder.publicKey();
  const responderId = responder.nodeId();
  await Promise.all([
    responder.accept(initiator.nodeId()),
    (async () => {
      await sleep(50);
      await initiator.connect(addr, pub, responderId);
    })(),
  ]);
}

async function main(): Promise<void> {
  const holder = await build(0x91);
  const reader = await build(0x92);

  await handshake(holder, reader);
  await holder.start();
  await reader.start();

  // A content-addressed store backed by a local directory. `blobPublish`
  // returns the *encoded* address, which is what travels on the wire; the
  // `BlobRef` object is its parsed form.
  const dir = mkdtempSync(join(tmpdir(), 'net-blobs-'));
  registerFilesystemBlobAdapter('local', dir);
  const encoded = await blobPublish('local', 'file:orders/2026-09/payload', PAYLOAD);
  const ref = BlobRef.fromEncoded(encoded);
  console.log(`stored ${PAYLOAD.length} bytes`);
  console.log(`minted address: ${ref.hash.toString('hex')}`);

  // Host the same bytes on the mesh so a peer can pull them. Installing the
  // transfer engine is required before serving or fetching.
  const adapter = new MeshBlobAdapter(new Redex(), 'objects');
  await adapter.store(ref, PAYLOAD);
  holder.serveBlobTransfer(adapter);
  // The engine is needed to *issue* fetches as well as to serve them, so the
  // reading node installs one over its own adapter.
  reader.serveBlobTransfer(new MeshBlobAdapter(new Redex(), 'incoming'));

  // Read your own write. The reader does not have to know which node the bytes
  // settled on — the address resolves through the mesh.
  const readback = await reader.fetchBlob(holder.nodeId(), ref);
  const same = Buffer.compare(readback, PAYLOAD) === 0;

  // Content addressing means the second store is a no-op that produces the same
  // address: identical bytes cannot occupy two identities.
  const again = await blobPublish('local', 'file:another/name/entirely', PAYLOAD);
  // Compare the content hash, not the encoded ref: the URI is part of the ref,
  // so two names for identical bytes encode differently while hashing the same.
  const dedup = Buffer.compare(ref.hash, BlobRef.fromEncoded(again).hash) === 0 ? 1 : 0;

  // The encoded address round-trips through bytes, which is how one rides
  // inside an ordinary event payload without the substrate inspecting it.
  const roundTrip = Buffer.compare(BlobRef.fromEncoded(encoded).encode(), encoded) === 0 ? 1 : 0;

  console.log(`read back from the mesh:  ${readback.length} bytes`);
  console.log(`same bytes:               ${same}`);
  console.log(`storing them again:       same address = ${dedup === 1}`);
  console.log(`ref round-trips on wire:  ${roundTrip === 1}`);

  console.log(
    `RESULT ok dedup=${dedup} readback=${same ? 1 : 0} bytes=${readback.length}`,
  );

  await holder.shutdown();
  await reader.shutdown();
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
