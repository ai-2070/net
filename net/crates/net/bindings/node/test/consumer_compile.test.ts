// S4 — the consumer-compile probe (the Node row's "consumer-compile"
// evidence, the `guards/org_api_probe` discipline for this binding).
//
// Runs `tsc --noEmit` over `test/consumer/org_streaming_consumer.ts` — a
// consumer program pinning the typed org surface AND the generated native
// declarations with `skipLibCheck: false` — and asserts a CLEAN compile
// (empty output). The compile IS the witness: it rides `npm test`, so a
// signature or declaration break in the pinned surface reddens the suite
// before any live cell runs.

import { execFileSync } from 'node:child_process'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

const here = dirname(fileURLToPath(import.meta.url))
const pkgRoot = resolve(here, '..')

describe('S4 — consumer-compile probe', () => {
  it('a_consumer_program_compiles_against_the_shipped_org_surface', () => {
    const tsc = join(pkgRoot, 'node_modules', 'typescript', 'bin', 'tsc')
    const out = execFileSync(
      process.execPath,
      [tsc, '--noEmit', '-p', join(here, 'consumer', 'tsconfig.json')],
      {
        cwd: pkgRoot,
        stdio: 'pipe',
        encoding: 'utf8',
      },
    )
    expect(out.trim(), 'tsc reports a clean compile').toBe('')
  }, 120_000)
})
