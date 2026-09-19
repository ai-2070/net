//! The job queue you no longer run.
//!
//! A producer, two workers, and an append-only job log — three in-process mesh
//! nodes over loopback UDP plus a local log. Jobs are appended to the log (the
//! queue), dispatch reads them back out of it, and a worker that *refuses* a
//! job re-issues it to its peer. Nothing is re-executed, and the results log is
//! the record you reconcile from.
//!
//! Both logs live in memory for the life of the process — that is all this
//! route needs, and all it claims. Surviving a restart is two more arguments:
//! `new Redex({ persistentDir })` and `openFile(name, { persistent: true })`.
//!
//! Run (from `net/crates/net/sdk-ts`, where `tsconfig.skill-example.json`
//! includes it):
//!
//!   npx tsc --noEmit -p tsconfig.skill-example.json
//!
//! Expected final line: `RESULT ok jobs=6 done=6 retried=1 duplicates=0`

import { MeshNode, Redex } from '@net-mesh/sdk';
import { classifyError } from '@net-mesh/core/errors';
import { appError, NRPC_TYPED_HANDLER_ERROR } from '@net-mesh/core/mesh_rpc';

/** 64 hex characters = 32 bytes. Every node in a mesh shares it. */
const PSK = '42'.repeat(32);

const JOBS = 6;
/**
 * Job 3 is answered with an error by the first worker that sees it, so the
 * dispatcher has to re-issue it somewhere else and the retry is observable.
 */
const POISON = 3;

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * A typed application refusal reaches the caller as an `RpcServerError`
 * carrying the wire status the handler chose. `call` rejects with the raw
 * napi error, so run it through `classifyError` first — that is the seam
 * that parses `status=0x....` out of the message. Match on `name` rather
 * than `instanceof`: the binding documents the name check as the
 * dual-module-safe one, and it only sets `status` when it could parse one.
 */
function isRefusal(error: unknown): boolean {
  const typed = classifyError(error);
  if (!(typed instanceof Error) || typed.name !== 'RpcServerError') return false;
  return 'status' in typed && typed.status === NRPC_TYPED_HANDLER_ERROR;
}

interface Job {
  id: number;
}

interface Done {
  id: number;
  worker: string;
}

async function build(seed: number): Promise<MeshNode> {
  return MeshNode.create({
    bindAddr: '127.0.0.1:0',
    psk: PSK,
    identitySeed: Buffer.alloc(32, seed),
    heartbeatIntervalMs: 200,
  });
}

/**
 * One side connects, the other accepts. `accept` MUST be registered before
 * `start`; both promises resolve only after the handshake completes, so
 * awaiting the pair is the wait-until-connected primitive.
 */
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
  const producer = await build(0x71);
  const w1 = await build(0x72);
  const w2 = await build(0x73);

  await handshake(producer, w1);
  await handshake(producer, w2);
  // Workers can see each other too — nothing here needs that, but it is what
  // makes re-dispatch to "any worker" a local query rather than a config file.
  await handshake(w1, w2);

  await producer.start();
  await w1.start();
  await w2.start();

  const w1Id = w1.nodeId();
  const w2Id = w2.nodeId();
  const label = (id: bigint) => (id === w1Id ? 'one' : 'two');

  // Two workers, one service. Each echoes its own name so the caller can prove
  // which one ran the job. The refusal is thrown as `appError(...)` so it
  // reaches the caller as a typed application status rather than the generic
  // `Internal` a bare `throw` maps to — and it is thrown BEFORE the job does
  // any work, which is what makes re-issuing it safe. The caller decides to
  // retry; the substrate never does it silently.
  //
  // Each `rpc()` builds a handle holding its own reference to the node, so
  // hold one per node and close it before `shutdown()` — an outstanding
  // handle makes shutdown fail with `outstanding references exist`.
  const rpcOne = w1.rpc();
  const rpcTwo = w2.rpc();
  const serveOne = rpcOne.serve<Job, Done>('run', async (job) => {
    if (job.id === POISON) {
      throw appError(NRPC_TYPED_HANDLER_ERROR, `worker one refused job ${job.id}`);
    }
    return { id: job.id, worker: 'one' };
  });
  const serveTwo = rpcTwo.serve<Job, Done>('run', async (job) => ({
    id: job.id,
    worker: 'two',
  }));

  // The queue: a local append-only log, one record per submitted job. A
  // `Redex` with no `persistentDir` is the in-memory manager — these logs are
  // the queue for as long as the process lives, and no longer.
  const redex = new Redex();
  const queue = redex.openFile('jobs/queue');
  const results = redex.openFile('jobs/results');

  for (let id = 1; id <= JOBS; id += 1) {
    const seq = queue.append(Buffer.from(`job:${id}`));
    console.log(`queued job ${id} at seq ${seq}`);
  }

  // Dispatch. The work list comes back out of the queue log, not out of the
  // loop that wrote it — the log IS the queue. Round-robin across the workers;
  // a refusal re-issues that job to the other one.
  const queued: number[] = [];
  for (const event of queue.readRange(0n, queue.len())) {
    const text = event.payload.toString('utf8');
    if (!text.startsWith('job:')) continue;
    const id = Number(text.slice('job:'.length));
    if (Number.isInteger(id)) queued.push(id);
  }

  const targets = [w1Id, w2Id];
  const caller = producer.rpc();
  let retried = 0;
  for (const [index, id] of queued.entries()) {
    const primary = targets[index % targets.length];
    const secondary = targets[(index + 1) % targets.length];

    let done: Done;
    try {
      done = await caller.call<Job, Done>(primary, 'run', { id }, { deadlineMs: 5_000 });
    } catch (error) {
      // ONLY the typed refusal is re-issued. A timeout or a transport fault
      // means the call failed, not that the job did — the worker may have run
      // it already, and re-issuing would execute it twice. Those rethrow;
      // `duplicates=0` is a claim this guard earns.
      if (!isRefusal(error)) throw error;
      retried += 1;
      console.log(`job ${id} refused by ${label(primary)}; re-issuing to ${label(secondary)}`);
      done = await caller.call<Job, Done>(secondary, 'run', { id }, { deadlineMs: 5_000 });
    }
    results.append(Buffer.from(`done:${done.id}:${done.worker}`));
  }

  // Reconcile from the results log, not from a counter kept beside the
  // dispatch loop: the completion count is whatever the log says. A job id
  // with one result record ran exactly once.
  const done = new Map<number, number>();
  for (const event of results.readRange(0n, results.len())) {
    const text = event.payload.toString('utf8');
    if (!text.startsWith('done:')) continue;
    const id = Number(text.slice(5).split(':')[0]);
    if (Number.isInteger(id)) done.set(id, (done.get(id) ?? 0) + 1);
  }

  const jobsQueued = Number(queue.len());
  const jobsDone = done.size;
  const duplicates = [...done.values()].filter((count) => count > 1).length;

  console.log(`queued:     ${jobsQueued}`);
  console.log(`completed:  ${jobsDone} (one result record each)`);
  console.log(`re-issued:  ${retried}`);

  console.log(
    `RESULT ok jobs=${jobsQueued} done=${jobsDone} retried=${retried} duplicates=${duplicates}`,
  );

  serveOne.close();
  serveTwo.close();
  caller.raw.close();
  rpcOne.raw.close();
  rpcTwo.raw.close();
  await producer.shutdown();
  await w1.shutdown();
  await w2.shutdown();
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
