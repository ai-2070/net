// An abandoned JS Promise is reported as one, across the binding's
// JS-Promise bridges (`src/js_promise.rs`; found during the
// NODE_A2A_PAID_ADMISSION_PLAN.md PR review).
//
// A pending Promise that nothing references can never settle; V8 may collect
// it, and napi then reports a dropped channel that the bridges used to render
// as a rejection — or, on the unary nRPC path, which awaits the handler's
// Promise with no deadline, could only ever surface as a generic rejection.
// Every bridge now names it: "<what> returned a Promise that can never
// settle". The A2A executor and preflight are witnessed in a2a_paid.test.ts;
// this file witnesses the nRPC unary and server-streaming handlers.
//
// Collection is only deterministic with an explicit `gc()`, so each scenario
// runs in a child `node --expose-gc` that calls it while the bridge awaits.

import { execFileSync } from 'node:child_process'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const binding: any = await import('../index')
const HAS_RPC = typeof binding.MeshRpc?.fromMesh === 'function'

const here = dirname(fileURLToPath(import.meta.url))
const INDEX = resolve(here, '..', 'index.js')

function scenario(): { unary: string; streaming: string } {
  const script = `
    const { NetMesh, MeshRpc } = require(${JSON.stringify(INDEX)});
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    (async () => {
      const psk = 'c3'.repeat(32);
      const mk = () => NetMesh.create({ bindAddr: '127.0.0.1:0', psk, permissiveChannels: true });
      const server = await mk(); const client = await mk();
      const accepted = server.accept(client.nodeId()); await sleep(50);
      await client.connect(server.localAddr(), server.publicKey(), server.nodeId()); await accepted;
      server.start(); client.start();
      await Promise.all([server.announceCapabilities({}), client.announceCapabilities({})]);
      await sleep(300);
      const serverRpc = MeshRpc.fromMesh(server); const clientRpc = MeshRpc.fromMesh(client);
      const gcLoop = setInterval(() => global.gc(), 20);

      // Handlers returning a Promise nothing else references.
      const unaryHandle = serverRpc.serve('gc.unary', () => new Promise(() => {}));
      const streamHandle = serverRpc.serveStreaming('gc.streaming', () => new Promise(() => {}));

      // The callee's capability-auth gate treats a caller as untrusted until
      // the signed announcements have been exchanged (TOFU-pinned); on a busy
      // machine that can outlast a fixed settle time. Retry only that denial,
      // re-announcing each time — any other outcome is the one under test.
      const NOT_YET_PINNED = 'capability-auth gate denied';
      async function attempt(fn) {
        let msg = 'NO_ATTEMPT';
        for (let i = 0; i < 40; i++) {
          msg = await fn();
          if (!msg.includes(NOT_YET_PINNED)) return msg;
          await Promise.all([server.announceCapabilities({}), client.announceCapabilities({})]);
          await sleep(250);
        }
        return msg;
      }

      const unary = await attempt(async () => {
        try { await clientRpc.call(server.nodeId(), 'gc.unary', Buffer.from('x')); return 'RESOLVED'; }
        catch (e) { return String(e.message); }
      });

      const streaming = await attempt(async () => {
        try {
          const stream = await clientRpc.callStreaming(server.nodeId(), 'gc.streaming', Buffer.from('x'));
          for (let c = await stream.next(); c !== null; c = await stream.next()) {}
          stream.close();
          return 'ENDED';
        } catch (e) { return String(e.message); }
      });

      clearInterval(gcLoop);
      unaryHandle.close(); streamHandle.close();
      serverRpc.close(); clientRpc.close();
      console.log('RESULT ' + JSON.stringify({ unary, streaming }));
      process.exit(0);
    })().catch((e) => { console.log('ERROR ' + e.message); process.exit(1); });`
  const out = execFileSync(process.execPath, ['--expose-gc', '-e', script], {
    encoding: 'utf8',
    timeout: 120000,
  })
  const line = out.split('\n').find((l) => l.startsWith('RESULT '))
  if (!line) throw new Error(`scenario produced no result:\n${out}`)
  return JSON.parse(line.slice('RESULT '.length))
}

describe.skipIf(!HAS_RPC)('abandoned JS Promises are named, not called rejections', () => {
  it('nRPC unary and server-streaming handlers report a Promise that can never settle', () => {
    const r = scenario()
    expect(r.unary).toMatch(/JS handler returned a Promise that can never settle/)
    expect(r.streaming).toMatch(/JS streaming handler returned a Promise that can never settle/)
  }, 180000)
})
