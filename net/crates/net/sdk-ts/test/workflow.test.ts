// Integration tests for the task-lifecycle (WorkflowAdapter) wrapper in
// sdk-ts/src/cortex.ts. Exercises the lifecycle, the terminal-state
// guard, and reads through the napi boundary. Mirrors the Rust-side
// surface test in `net/crates/net/sdk/tests/workflow_surface.rs`.

import { describe, expect, it } from 'vitest';

import { Redex, WorkflowAdapter } from '../src/cortex';

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
