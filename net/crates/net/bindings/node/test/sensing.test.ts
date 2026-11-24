// Capability sensing through the Node binding (SENSING_OLB_COMPLETION_PLAN B1).
//
// Every behaviour asserted here is the Rust SDK's, forwarded. The binding
// adds no classification and no wait semantics of its own:
//
//   - errors: each fixture vector in `tests/cross_lang_sensing/error_vectors.json`
//     classifies through `sensingErrorClass` exactly as Rust classifies it,
//     and a live refusal carries the Rust wire string verbatim (C5);
//   - provider: `sensingProvide` is the SDK's value-backed provider, and a
//     `set()` reaches a consumer's snapshot through real signed attestations;
//   - consumer: `close()` during a pending `changed()` rejects that wait with
//     `sensing:watch_closed:`, never success (B1-pre).
//
// The live cell mints a same-org scenario from `gen_subnet_scenario`, exactly
// like the S4 cell in `org_live.test.ts`. It needs a Rust toolchain and an
// `org`-feature build, and skips cleanly otherwise.

import { execFileSync, spawnSync } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { afterAll, beforeAll, describe, expect, it } from 'vitest'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const binding: any = await import('../index')
const { NetMesh, OrgAccess, installOrgAuthority, serveOrg, sensingErrorClass } = binding

const HAS_SENSING =
  typeof sensingErrorClass === 'function' && typeof NetMesh?.prototype?.sensingWatch === 'function'

const here = dirname(fileURLToPath(import.meta.url))
const crateRoot = resolve(here, '..', '..', '..')
// The live cell mints its scenario with `cargo run` inside the Rust workspace.
// A prebuilt binding with no toolchain or no workspace skips it cleanly.
const HAS_CARGO_WORKSPACE =
  existsSync(join(crateRoot, 'Cargo.toml')) &&
  spawnSync('cargo', ['--version'], { stdio: 'ignore' }).status === 0
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))

type Vector = { kind: string; terminal: boolean; wire: string; message: string }

describe.skipIf(!HAS_SENSING)('C5 — the Rust-owned sensing error classification', () => {
  const fixture = JSON.parse(
    readFileSync(join(crateRoot, 'tests', 'cross_lang_sensing', 'error_vectors.json'), 'utf8'),
  ) as { prefix: string; vectors: Vector[] }

  it('classifies every fixture vector exactly as Rust does', () => {
    expect(fixture.vectors.length).toBeGreaterThan(0)
    for (const v of fixture.vectors) {
      expect(sensingErrorClass(v.wire), v.wire).toEqual({ kind: v.kind, terminal: v.terminal })
    }
  })

  it('refuses what is not a sensing wire string', () => {
    expect(sensingErrorClass('org:rpc:timeout: x')).toBeNull()
    expect(sensingErrorClass('sensing:not_a_kind: x')).toBeNull()
  })

  it('surfaces a live refusal as the Rust wire string', async () => {
    const mesh = await NetMesh.create({ bindAddr: '127.0.0.1:0', psk: '42'.repeat(32) })
    try {
      let message = ''
      try {
        mesh.sensingWatch('nrpc:anything')
      } catch (e) {
        message = String((e as Error).message)
      }
      // A mesh built without `enableSensing` refuses, as the SDK does.
      const disabled = fixture.vectors.find((v) => v.kind === 'disabled')!
      expect(message).toBe(disabled.wire)
      expect(sensingErrorClass(message)).toEqual({ kind: 'disabled', terminal: true })
    } finally {
      await mesh.shutdown()
    }
  })

  // SDK plan LS-7c: the coalesced option reaches the SDK and every refusal is
  // Rust's own kind. An out-of-range `top` is refused before any node state;
  // a meaningful coalescing on a mesh with no organization authority is
  // refused as `observer_not_qualified` — never served as an exact watch.
  it('refuses a coalesced watch with Rust\'s own kinds', async () => {
    const kinds = new Set(fixture.vectors.map((v) => v.kind))
    expect(kinds.has('unsatisfiable_coalescing')).toBe(true)
    expect(kinds.has('no_sensing_leader')).toBe(true)
    const incarnationDir = scenarioDir('coalesced')
    const mesh = await NetMesh.create({
      bindAddr: '127.0.0.1:0',
      psk: '42'.repeat(32),
      enableSensing: true,
      sensingIncarnationPath: join(incarnationDir, 'coalesced.incarnation'),
    })
    const refusal = (options: object) => {
      try {
        mesh.sensingWatch('nrpc:coalesced.node', options)
      } catch (e) {
        return sensingErrorClass(String((e as Error).message))
      }
      return null
    }
    try {
      for (const top of [0, 33, 70_000, 4_294_967_297, 2.5]) {
        expect(refusal({ coalesced: { top } }), `top ${top}`).toEqual({
          kind: 'unsatisfiable_coalescing',
          terminal: false,
        })
      }
      for (const coalesced of [{}, { top: 3 }]) {
        expect(refusal({ coalesced })?.kind, JSON.stringify(coalesced)).toBe('observer_not_qualified')
      }
    } finally {
      await mesh.shutdown()
      rmSync(incarnationDir, { recursive: true, force: true })
    }
  })
})

type SameOrgManifest = {
  psk_hex: string
  provider: { seed_hex: string; authority_dir: string }
  caller: { seed_hex: string; authority_dir: string }
  foreign_caller: { seed_hex: string; authority_dir: string }
}

function scenarioDir(tag: string): string {
  // Plain mkdir, not mkdtemp: the audience-secret loader refuses the
  // inherited owner-only ACL an mkdtemp directory carries on Windows (see
  // org_live.test.ts).
  const dir = join(tmpdir(), `sensing-${tag}-${process.pid}-${Date.now()}`)
  mkdirSync(dir, { recursive: true })
  return dir
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Mesh = any

describe.skipIf(!HAS_SENSING || !HAS_CARGO_WORKSPACE)('B1 — live sensing through the Node binding', () => {
  const SERVICE = 'sensing.node.live'
  const TAG = `nrpc:${SERVICE}`
  let dir: string
  let provider: Mesh
  let consumer: Mesh
  let quiet: Mesh
  let serveHandle: { close(): void } | undefined

  beforeAll(async () => {
    dir = scenarioDir('live')
    execFileSync(
      'cargo',
      ['run', '-q', '-p', 'net-mesh-sdk', '--features', 'net,cortex,fixtures', '--example', 'gen_subnet_scenario', '--', dir],
      { cwd: crateRoot, stdio: 'inherit' },
    )
    const m = JSON.parse(readFileSync(join(dir, 'manifest.json'), 'utf8')) as SameOrgManifest
    const make = (seedHex: string, name: string) =>
      NetMesh.create({
        bindAddr: '127.0.0.1:0',
        psk: m.psk_hex,
        identitySeed: Buffer.from(seedHex, 'hex'),
        permissiveChannels: true,
        enableSensing: true,
        sensingIncarnationPath: join(dir, `${name}.incarnation`),
      })
    provider = await make(m.provider.seed_hex, 'provider')
    consumer = await make(m.caller.seed_hex, 'consumer')
    // A third, peerless node with its own authority: nothing can move its
    // sensing generation, so a wait on it stays pending until the floor.
    quiet = await make(m.foreign_caller.seed_hex, 'quiet')
    installOrgAuthority(quiet, join(dir, m.foreign_caller.authority_dir))
    await quiet.start()
    installOrgAuthority(provider, join(dir, m.provider.authority_dir))
    writeFileSync(
      join(dir, m.caller.authority_dir, 'owner-audience.key'),
      readFileSync(join(dir, m.provider.authority_dir, 'owner-audience.key')),
    )
    installOrgAuthority(consumer, join(dir, m.caller.authority_dir))
    const accepted = provider.accept(consumer.nodeId())
    await sleep(50)
    await consumer.connect(provider.localAddr(), provider.publicKey(), provider.nodeId())
    await accepted
    await provider.start()
    await consumer.start()
    serveHandle = serveOrg(provider, SERVICE, OrgAccess.SameOrg, async () => Buffer.from('ok'))
  }, 300_000)

  afterAll(async () => {
    try {
      serveHandle?.close()
    } catch {
      /* already closed */
    }
    await provider?.shutdown().catch(() => {})
    await consumer?.shutdown().catch(() => {})
    await quiet?.shutdown().catch(() => {})
    if (dir) rmSync(dir, { recursive: true, force: true })
  })

  /** Snapshot until `predicate` holds, re-announcing so owner-scoped
   *  discovery converges (announcements are throttled). */
  async function until(
    watch: { snapshot(): Promise<{ providers: { nodeId: bigint; readiness: string; viability: string }[] }> },
    what: string,
    predicate: (rows: { nodeId: bigint; readiness: string; viability: string }[]) => boolean,
  ) {
    const deadline = Date.now() + 60_000
    let last: unknown
    while (Date.now() < deadline) {
      const snap = await watch.snapshot()
      last = snap.providers
      if (predicate(snap.providers)) return snap
      await Promise.all([provider.announceCapabilities({}), consumer.announceCapabilities({})]).catch(() => {})
      await sleep(500)
    }
    throw new Error(`${what}: last rows ${JSON.stringify(last, (_, v) => (typeof v === 'bigint' ? v.toString() : v))}`)
  }

  it('a provider set reaches the consumer snapshot, and close ends a pending wait', async () => {
    const providerId = BigInt(provider.nodeId())
    const ready = provider.sensingProvide(TAG, { state: 'ready', estimatedStartMs: 40 })
    expect(ready.get()).toEqual({ state: 'ready', estimatedStartMs: 40 })

    const watch = consumer.sensingWatch(TAG)
    const first = await until(watch, 'the provider never read ready', (rows) =>
      rows.some((r) => r.nodeId === providerId && r.readiness === 'ready'),
    )
    expect(first.ranked).toContain(providerId)

    ready.set({ state: 'not_ready', reason: 3 })
    await until(watch, 'a not_ready set never reached the consumer', (rows) =>
      rows.some((r) => r.nodeId === providerId && r.viability === 'not_viable'),
    )

    // After close(), every wait rejects as closed. (Close while a wait is
    // still PENDING is witnessed on a quiet node below: here a live provider
    // keeps producing real edges, so "still pending" is not observable.)
    expect(watch.close()).toBe(true)
    let rejection = ''
    await watch.changed().then(
      () => {
        throw new Error('a closed watch must not report a change')
      },
      (e: Error) => {
        rejection = String(e.message)
      },
    )
    expect(sensingErrorClass(rejection)).toEqual({ kind: 'watch_closed', terminal: true })
    expect(watch.isClosed).toBe(true)
    expect(watch.close()).toBe(false)

    expect(ready.close()).toBe(true)
    expect(ready.set({ state: 'ready' })).toBe(false)
  }, 120_000)

  it('close() while a changed() is pending rejects that wait as closed, never resolves it', async () => {
    // The quiet node has no peers and no providers, so the only thing that
    // could resolve the wait is the 1 s population floor. 100 ms is far inside
    // it. The snapshot acknowledges setup movement first, so the wait really
    // parks.
    const watch = quiet.sensingWatch('nrpc:quiet.capability')
    await watch.snapshot()
    let settled = false
    let outcome: { ok: boolean; message?: string } | undefined
    const pending = watch.changed().then(
      () => {
        settled = true
        outcome = { ok: true }
      },
      (e: Error) => {
        settled = true
        outcome = { ok: false, message: String(e.message) }
      },
    )
    await sleep(100)
    expect(settled, 'precondition: the wait is parked').toBe(false)
    expect(watch.close()).toBe(true)
    await pending
    expect(outcome?.ok, 'a wait pending at close() must reject, never resolve').toBe(false)
    expect(sensingErrorClass(outcome!.message!)).toEqual({ kind: 'watch_closed', terminal: true })
  }, 60_000)
})
