// S4TsSdk — the executable consumer-program witness (Stage 4, Pure SDKs Q6).
//
// The brief's named demand: "the witness is a CONSUMER PROGRAM that calls
// AND serves all four shapes through `@net-mesh/sdk` alone (the
// `check-ts-consumer.sh` pattern but EXECUTABLE — 'Nothing here runs' is
// not evidence)". This driver IS the machinery:
//
//   1. REBUILD the consumer artifact — `tsc` over the SDK's own sources
//      into `dist/` (every execution cycle rebuilds the artifact it
//      consumes, the spot-check rule's "rebuild the consumer artifact
//      inside the cycle");
//   2. STAGE a consumer project exactly the `check-ts-consumer.sh` way —
//      fresh COPIES of the packaged `@net-mesh/core` and the shipped
//      `@net-mesh/sdk` (`files` set: `dist` + manifest) in a temp
//      `node_modules`, copy-not-symlink so declaration errors read as
//      OUR shipped files, not the source tree's;
//   3. COMPILE `test/org_consumer/org_streaming_consumer.ts` with
//      `skipLibCheck: false` (the careful-consumer check — the same
//      compile emits the executable);
//   4. MINT the two issuer fixtures (same-org via the `test-helpers`
//      minter, granted via the `gen_org_scenario` cargo example) — a
//      consumer only ever LOADS issued credentials from disk;
//   5. EXECUTE `node out/org_streaming_consumer.js <scenario> <fixture>`
//      once per witness row. Every named assertion runs inside the
//      consumer (against the staged packages); a failure surfaces the
//      assertion's name verbatim through `runScenario`.
//
// Roster (11 cells = the report's witness rows): the consumer-compile
// probe; all four shapes × call AND serve × same-org AND granted with
// verified-caller attribution (never origin) — the unary cells are the
// preserved unary; the midstream org-vocabulary pin (an `OrgError`,
// never a false clean EOF); strict disposal.

import { execFileSync } from 'node:child_process'
import {
  cpSync,
  mkdirSync,
  mkdtempSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { testMintSameOrgScenario } from '@net-mesh/core'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'

const here = dirname(fileURLToPath(import.meta.url)) // sdk-ts/test
const pkgRoot = resolve(here, '..') // sdk-ts
const crateRoot = resolve(here, '..', '..') // net/crates/net (cargo walks up)

const CONSUMER_TSCONFIG = JSON.stringify(
  {
    // The careful-consumer check, with OUT-EMIT: this consumer is
    // executed, not just checked. `moduleResolution: bundler` reads the
    // `exports` maps (the `@net-mesh/sdk/org` subpath — S0_MAPPING
    // obstacle 8 — resolves through them).
    compilerOptions: {
      module: 'CommonJS',
      moduleResolution: 'bundler',
      target: 'ES2022',
      lib: ['ES2024'],
      strict: true,
      noImplicitAny: true,
      strictNullChecks: true,
      esModuleInterop: true,
      skipLibCheck: false,
      types: ['node'],
      outDir: './out',
      rootDir: '.',
    },
    include: ['./org_streaming_consumer.ts'],
  },
  null,
  2,
)

const SCENARIOS = {
  same_org_unary_call_and_serve: 'same-org',
  same_org_streaming_call_and_serve: 'same-org',
  same_org_client_stream_call_and_serve: 'same-org',
  same_org_duplex_call_and_serve: 'same-org',
  granted_unary_call_and_serve: 'granted',
  granted_streaming_call_and_serve: 'granted',
  granted_client_stream_call_and_serve: 'granted',
  granted_duplex_call_and_serve: 'granted',
  midstream_outcomes_surface_an_org_error_never_a_false_clean_eof: 'same-org',
  closing_client_and_serve_handle_lets_both_meshes_shut_down_cleanly: 'same-org',
} as const

let work = ''
let consumerJs = ''
let compileOutput = ''
let sameDir = ''
let grantedDir = ''

/**
 * Plain `mkdir`, NOT `mkdtemp`: on Windows an mkdtemp directory carries
 * an owner-only ACL whose inheritance the audience-secret loader
 * (rightly) refuses on files created beneath it (the same note the
 * binding's live cells carry). The minters' checked writers need
 * ordinary per-user directories.
 */
function tmpScenarioDir(tag: string): string {
  const dir = join(
    tmpdir(),
    `org-q6-tssdk-${tag}-${process.pid}-${Date.now()}-${Math.random().toString(36).slice(2)}`,
  )
  mkdirSync(dir, { recursive: true })
  return dir
}

function tscBin(): string {
  return join(pkgRoot, 'node_modules', 'typescript', 'bin', 'tsc')
}

/** Copy a tree, skipping nested `node_modules` (never the root itself). */
function copyTree(src: string, dst: string): void {
  cpSync(src, dst, {
    recursive: true,
    dereference: true,
    filter: (p) => resolve(p) === resolve(src) || basename(p) !== 'node_modules',
  })
}

/**
 * Run one consumer scenario in its own process and return its stdout.
 * A failure surfaces the consumer's NAMED assertion verbatim in the
 * thrown error (that text is what the inverse receipt quotes).
 */
function runScenario(scenario: string, fixtureDir: string): string {
  try {
    return execFileSync(process.execPath, [consumerJs, scenario, fixtureDir], {
      stdio: 'pipe',
      encoding: 'utf8',
      timeout: 110_000,
    })
  } catch (e) {
    const err = e as { stdout?: string; stderr?: string; message?: string }
    throw new Error(
      `consumer scenario ${scenario} failed\n--- stdout ---\n${err.stdout ?? ''}\n--- stderr ---\n${err.stderr ?? ''}\n${err.message ?? ''}`,
    )
  }
}

beforeAll(async () => {
  // (1) Rebuild the consumer artifact: the SDK's shipped `dist`.
  execFileSync(process.execPath, [tscBin(), '-p', join(pkgRoot, 'tsconfig.json')], {
    cwd: pkgRoot,
    stdio: 'pipe',
    encoding: 'utf8',
    timeout: 180_000,
  })

  // (2) Stage the consumer project — the check-ts-consumer.sh pattern.
  work = mkdtempSync(join(tmpdir(), 'org-q6-tssdk-consumer-'))
  copyTree(join(pkgRoot, 'node_modules'), join(work, 'node_modules'))
  rmSync(join(work, 'node_modules', '@net-mesh', 'core'), { recursive: true, force: true })
  rmSync(join(work, 'node_modules', '@net-mesh', 'sdk'), { recursive: true, force: true })
  mkdirSync(join(work, 'node_modules', '@net-mesh'), { recursive: true })
  // The staged `@net-mesh/core` must be a COMPLETE package. Its
  // hand-written TS modules (`errors`, `org`, `subnet`, `mesh_rpc`,
  // `meshdb`, `aggregator`, `tool`, `transport`) are compiled IN PLACE
  // to CJS `.js` + `.d.ts` beside their sources by the core's own
  // `build:ts`, and those artifacts are untracked build outputs:
  // `napi build` emits only `index.js` / `index.d.ts`, and CI's sdk-ts
  // job never runs the core's `build:ts` (only the Node-bindings job
  // does). A fresh checkout therefore stages a package with no
  // `subnet.js`, and the consumer program dies on MODULE_NOT_FOUND.
  // Compile it here with the core's own TypeScript — the same
  // rebuild-before-consume rule (1) applies to the SDK's `dist`.
  const coreRoot = resolve(pkgRoot, '..', 'bindings', 'node')
  execFileSync(
    process.execPath,
    [join(coreRoot, 'node_modules', 'typescript', 'bin', 'tsc'), '-p', 'tsconfig.build.json'],
    { cwd: coreRoot, stdio: 'pipe', encoding: 'utf8', timeout: 180_000 },
  )
  copyTree(coreRoot, join(work, 'node_modules', '@net-mesh', 'core'))
  const sdkDst = join(work, 'node_modules', '@net-mesh', 'sdk')
  mkdirSync(sdkDst, { recursive: true })
  copyTree(join(pkgRoot, 'dist'), join(sdkDst, 'dist'))
  // The shipped manifest verbatim (the `exports` entries resolve inside
  // the staged copy — including the `/org` subpath, S0_MAPPING
  // obstacle 8).
  cpSync(join(pkgRoot, 'package.json'), join(sdkDst, 'package.json'))
  cpSync(join(here, 'org_consumer', 'org_streaming_consumer.ts'), join(work, 'org_streaming_consumer.ts'))
  writeFileSync(join(work, 'tsconfig.json'), CONSUMER_TSCONFIG)

  // (3) The careful-consumer compile (also emits the executable). A
  // non-clean compile is cell 1's RED — capture, never mask.
  try {
    compileOutput = execFileSync(
      process.execPath,
      [tscBin(), '-p', join(work, 'tsconfig.json')],
      { cwd: work, stdio: 'pipe', encoding: 'utf8', timeout: 180_000 },
    )
  } catch (e) {
    const err = e as { stdout?: string; stderr?: string }
    compileOutput = `${err.stdout ?? ''}\n${err.stderr ?? ''}`
  }
  consumerJs = join(work, 'out', 'org_streaming_consumer.js')

  // (4) Mint the issuer fixtures.
  sameDir = tmpScenarioDir('same')
  testMintSameOrgScenario(sameDir)
  grantedDir = mkdtempSync(join(tmpdir(), 'x2-ts-sdk-granted-'))
  execFileSync(
    'cargo',
    [
      'run',
      '-q',
      '-p',
      'net-mesh-sdk',
      '--features',
      'net,cortex,fixtures',
      '--example',
      'gen_org_scenario',
      '--',
      grantedDir,
    ],
    { cwd: crateRoot, stdio: 'pipe', encoding: 'utf8', timeout: 300_000 },
  )
}, 600_000)

afterAll(() => {
  for (const dir of [work, sameDir, grantedDir]) {
    if (dir) rmSync(dir, { recursive: true, force: true })
  }
})

describe('S4 — the @net-mesh/sdk consumer program (Q6: call AND serve, all four shapes)', () => {
  it('consumer_program_compiles_against_the_shipped_org_surface', () => {
    expect(
      compileOutput.trim(),
      'tsc reports a clean compile (skipLibCheck: false over the staged packaged pair)',
    ).toBe('')
  }, 180_000)

  for (const [scenario, mode] of Object.entries(SCENARIOS)) {
    it(scenario, () => {
      const out = runScenario(scenario, mode === 'granted' ? grantedDir : sameDir)
      expect(out, 'the consumer program reports its scenario').toContain(`OK ${scenario}`)
    }, 120_000)
  }
})
