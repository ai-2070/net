// Integration tests for the task-lifecycle (WorkflowAdapter) wrapper in
// sdk-ts/src/cortex.ts. Exercises the lifecycle, the terminal-state
// guard, and reads through the napi boundary. Mirrors the Rust-side
// surface test in `net/crates/net/sdk/tests/workflow_surface.rs`.

import { describe, expect, it } from 'vitest';

import { Redex, ShardGroup, TriggerEngine, WorkflowAdapter } from '../src/cortex';

const ORIGIN = 0x0f10_5d01n;

describe('WorkflowAdapter', () => {
  it('drives a task through its lifecycle', async () => {
    const redex = new Redex();
    const wf = await WorkflowAdapter.open(redex, ORIGIN);

    wf.submit(1n);
    wf.start(1n);
    wf.advance(1n); // step 0 -> 1, attempts reset
    const seq = wf.complete(1n);
    await wf.waitForSeq(seq);

    const st = wf.get(1n);
    expect(st).not.toBeNull();
    expect(st!.status).toBe('done');
    expect(st!.step).toBe(1);
    expect(wf.statusCounts().done).toBe(1);
  });

  it('does not resurrect a terminal task', async () => {
    const redex = new Redex();
    const wf = await WorkflowAdapter.open(redex, ORIGIN);

    wf.submit(1n);
    wf.complete(1n);
    wf.start(1n); // no-op: Done is terminal
    const seq = wf.retry(1n); // no-op: Done is terminal
    await wf.waitForSeq(seq);

    expect(wf.get(1n)!.status).toBe('done');
  });

  it('deletes a task', async () => {
    const redex = new Redex();
    const wf = await WorkflowAdapter.open(redex, ORIGIN);
    wf.submit(7n);
    const seq = wf.delete(7n);
    await wf.waitForSeq(seq);
    expect(wf.get(7n)).toBeNull();
  });
});

describe('WorkflowAdapter shards + triggers (Tier 2)', () => {
  it('fans out shards then joins the reduce', async () => {
    const wf = await WorkflowAdapter.open(new Redex(), ORIGIN);
    const group = new ShardGroup([10n, 11n, 12n], 99n);

    const seq = wf.fanOut(group);
    await wf.waitForSeq(seq);
    expect(wf.tryJoin(group).kind).toBe('pending');

    let last = 0n;
    for (const s of [10n, 11n, 12n]) last = wf.complete(s);
    await wf.waitForSeq(last);

    const j = wf.tryJoin(group);
    expect(j.kind).toBe('submitted');
    if (j.seq != null) await wf.waitForSeq(j.seq);
    expect(wf.get(99n)).not.toBeNull();
    expect(wf.tryJoin(group).kind).toBe('already_submitted');
  });

  it('a trigger fires the dependent when its predecessor is done', async () => {
    const wf = await WorkflowAdapter.open(new Redex(), ORIGIN);
    const eng = new TriggerEngine(wf);
    eng.armAfterTask(1n, { kind: 'submit', id: 2n }); // B depends on A

    wf.submit(1n);
    wf.start(1n);
    await wf.waitForSeq(wf.complete(1n));

    const actions = eng.onTaskChange(1n);
    expect(actions).toEqual([{ kind: 'submit', id: 2n }]);
    expect(eng.armedCount()).toBe(0);
  });
});
