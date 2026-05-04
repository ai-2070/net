// NetDB smoke tests — unified db handle bundling tasks + memories.

import { describe, expect, it } from 'vitest'

import { NetDb, TaskStatus, TasksOrderBy, MemoriesOrderBy } from '../index'

const ORIGIN = 0xabcdef01n

describe('NetDb — build + inspect', () => {
  it('opens with both models', async () => {
    const db = await NetDb.open({
      originHash: ORIGIN,
      withTasks: true,
      withMemories: true,
    })
    expect(db.tasks).not.toBeNull()
    expect(db.memories).not.toBeNull()
  })

  it('opens with only tasks', async () => {
    const db = await NetDb.open({
      originHash: ORIGIN,
      withTasks: true,
    })
    expect(db.tasks).not.toBeNull()
    expect(db.memories == null).toBe(true)
  })

  it('opens with neither model', async () => {
    const db = await NetDb.open({ originHash: ORIGIN })
    expect(db.tasks == null).toBe(true)
    expect(db.memories == null).toBe(true)
  })
})

describe('NetDb — CRUD through bundled adapters', () => {
  it('tasks: create + list + complete via db.tasks', async () => {
    const db = await NetDb.open({
      originHash: ORIGIN,
      withTasks: true,
    })
    const tasks = db.tasks!
    tasks.create(1n, 'write plan', 100n)
    tasks.create(2n, 'ship netdb', 200n)
    const seq = tasks.complete(1n, 150n)
    await tasks.waitForSeq(seq)

    const all = tasks.listTasks(null)
    expect(all).toHaveLength(2)
    expect(all.find((t) => t.id === 1n)!.status).toBe(TaskStatus.Completed)
  })

  it('memories: store + list via db.memories', async () => {
    const db = await NetDb.open({
      originHash: ORIGIN,
      withMemories: true,
    })
    const memories = db.memories!
    memories.store(1n, 'hello', ['x'], 'alice', 100n)
    const seq = memories.pin(1n, 110n)
    await memories.waitForSeq(seq)

    const all = memories.listMemories(null)
    expect(all).toHaveLength(1)
    expect(all[0].pinned).toBe(true)
  })

  it('both models coexist under one NetDb', async () => {
    const db = await NetDb.open({
      originHash: ORIGIN,
      withTasks: true,
      withMemories: true,
    })
    db.tasks!.create(1n, 'task', 100n)
    db.memories!.store(1n, 'mem', ['x'], 'alice', 100n)
    const tSeq = db.tasks!.complete(1n, 150n)
    const mSeq = db.memories!.pin(1n, 150n)

    await db.tasks!.waitForSeq(tSeq)
    await db.memories!.waitForSeq(mSeq)

    expect(db.tasks!.count()).toBe(1)
    expect(db.memories!.count()).toBe(1)
  })
})

describe('NetDb — filter queries', () => {
  it('listTasks + listMemories filters work via db.tasks / db.memories', async () => {
    const db = await NetDb.open({
      originHash: ORIGIN,
      withTasks: true,
      withMemories: true,
    })

    for (let i = 1n; i <= 5n; i++) {
      db.tasks!.create(i, `t-${i}`, 100n * i)
    }
    const last = db.tasks!.complete(2n, 999n)
    await db.tasks!.waitForSeq(last)

    db.memories!.store(1n, 'work note', ['work'], 'alice', 100n)
    db.memories!.store(2n, 'home note', ['home'], 'alice', 200n)
    const mSeq = db.memories!.pin(1n, 210n)
    await db.memories!.waitForSeq(mSeq)

    // Filter tasks.
    const pending = db.tasks!.listTasks({
      status: TaskStatus.Pending,
      orderBy: TasksOrderBy.IdAsc,
    })
    expect(pending.map((t) => t.id)).toEqual([1n, 3n, 4n, 5n])

    // Filter memories.
    const pinned = db.memories!.listMemories({
      pinned: true,
      orderBy: MemoriesOrderBy.IdAsc,
    })
    expect(pinned.map((m) => m.id)).toEqual([1n])
  })
})

describe('NetDb — whole-db snapshot / restore', () => {
  it('bundled snapshot encodes both models', async () => {
    const db = await NetDb.open({
      originHash: ORIGIN,
      withTasks: true,
      withMemories: true,
    })

    db.tasks!.create(1n, 'alpha', 100n)
    const tSeq = db.tasks!.complete(1n, 150n)
    db.memories!.store(1n, 'hello', ['x'], 'alice', 100n)
    const mSeq = db.memories!.pin(1n, 110n)
    await db.tasks!.waitForSeq(tSeq)
    await db.memories!.waitForSeq(mSeq)

    const bundle = db.snapshot()
    expect(bundle.stateBytes.byteLength).toBeGreaterThan(0)
    db.close()

    // Restore against a fresh NetDb.
    const db2 = await NetDb.openFromSnapshot(
      {
        originHash: ORIGIN,
        withTasks: true,
        withMemories: true,
      },
      bundle,
    )

    const allTasks = db2.tasks!.listTasks(null)
    expect(allTasks).toHaveLength(1)
    expect(allTasks[0].status).toBe(TaskStatus.Completed)

    const allMemories = db2.memories!.listMemories(null)
    expect(allMemories).toHaveLength(1)
    expect(allMemories[0].pinned).toBe(true)
  })

  it('openFromSnapshot opens a missing model fresh', async () => {
    // Build a snapshot that only covers tasks.
    const db = await NetDb.open({
      originHash: ORIGIN,
      withTasks: true,
    })
    db.tasks!.create(1n, 'just tasks', 100n)
    const seq = db.tasks!.complete(1n, 150n)
    await db.tasks!.waitForSeq(seq)

    const bundle = db.snapshot()
    db.close()

    // Restore with both models — memories has no snapshot entry, so
    // it opens fresh (empty).
    const db2 = await NetDb.openFromSnapshot(
      {
        originHash: ORIGIN,
        withTasks: true,
        withMemories: true,
      },
      bundle,
    )
    expect(db2.tasks!.count()).toBe(1)
    expect(db2.memories!.count()).toBe(0)
  })
})

describe('NetDb — close', () => {
  it('is idempotent', async () => {
    const db = await NetDb.open({
      originHash: ORIGIN,
      withTasks: true,
      withMemories: true,
    })
    db.close()
    db.close() // no throw
  })
})
