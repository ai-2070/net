// The Node.js driver for the cross-process, cross-language org matrix.
//
// Implements `PROTOCOL.md` (this directory) for both roles and both shapes:
//
//   node driver.mjs provider --manifest <dir> --shape <unary|server-streaming> --caller-node-id <u64>
//   node driver.mjs caller   --manifest <dir> --shape <unary|server-streaming>
//
// stdout carries ONLY protocol lines (`ID`, `READY`, `FACTS`, `RESULT`);
// everything else goes to stderr.
//
// The binding: `@net-mesh/core` is a CommonJS package (the napi-generated
// `index.js` loader plus `tsc -p tsconfig.build.json` CJS output such as
// `errors.js`), so it is loaded with `createRequire`. The package directory is
// `$NET_NODE_BINDING` when set, else `../../../bindings/node` relative to this
// file. It must already be built:
//
//   cd net/crates/net/bindings/node
//   npm ci
//   npx napi build --platform --features <list incl. net,org>   # -> net.<triple>.node + index.js
//   npm run build:ts                                           # -> errors.js, org.js, ...
//
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createRequire } from 'node:module'
import { readFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { createInterface } from 'node:readline'
import { fileURLToPath } from 'node:url'

const HERE = dirname(fileURLToPath(import.meta.url))
const PKG_DIR = process.env.NET_NODE_BINDING
  ? resolve(process.env.NET_NODE_BINDING)
  : resolve(HERE, '..', '..', '..', 'bindings', 'node')

const ACCEPT_TIMEOUT_MS = 30_000
const DISCOVERY_BUDGET_MS = 60_000
const DISCOVERY_RETRY_MS = 500
const CALLBACK_WATCHDOG_MS = 120_000
const DRAINED_TIMEOUT_MS = 120_000
const STREAM_DRAIN_TIMEOUT_MS = 60_000
const SHUTDOWN_TIMEOUT_MS = 15_000
const ANNOUNCE_EVERY_MS = 1_000

const REQUEST = Buffer.from('{"n":7}', 'utf8')
const UNARY_RESPONSE = Buffer.from('{"n":8}', 'utf8')
const STREAM_ITEMS = [0, 1, 2].map((i) => Buffer.from(`{"i":${i}}`, 'utf8'))

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

// ---------------------------------------------------------------------------
// I/O
// ---------------------------------------------------------------------------

function log(...parts) {
  process.stderr.write(`driver[node]: ${parts.join(' ')}\n`)
}

/** One protocol line on stdout; resolves once the write is flushed. */
function emit(line) {
  return new Promise((r) => process.stdout.write(`${line}\n`, () => r()))
}

/** A single-line description of anything thrown. */
function oneLine(e) {
  const s = e instanceof Error ? `${e.name}: ${e.message}` : String(e)
  return s.replace(/[\r\n]+/g, ' ').trim()
}

/** Flush, then exit with `code` (napi handles would otherwise keep the loop alive). */
async function finish(code) {
  await new Promise((r) => process.stderr.write('', () => r()))
  process.exit(code)
}

/** Lines from stdin, pulled one at a time with a deadline; `null` at EOF. */
class StdinLines {
  constructor() {
    this.queue = []
    this.waiters = []
    this.eof = false
    const rl = createInterface({ input: process.stdin, crlfDelay: Infinity })
    rl.on('line', (line) => this.push(line.trim()))
    rl.on('close', () => {
      this.eof = true
      this.push(null)
    })
  }

  push(v) {
    const w = this.waiters.shift()
    if (w) w(v)
    else this.queue.push(v)
  }

  /** Resolves the next line, `null` on EOF; rejects after `timeoutMs`. */
  next(timeoutMs, what) {
    if (this.queue.length > 0) return Promise.resolve(this.queue.shift())
    if (this.eof) return Promise.resolve(null)
    return new Promise((res, rej) => {
      const waiter = (v) => {
        clearTimeout(t)
        res(v)
      }
      const t = setTimeout(() => {
        const i = this.waiters.indexOf(waiter)
        if (i >= 0) this.waiters.splice(i, 1)
        rej(new Error(`timed out after ${timeoutMs} ms waiting for ${what}`))
      }, timeoutMs)
      this.waiters.push(waiter)
    })
  }
}

/** `p`, or a rejection naming `what` after `ms`. */
function withTimeout(p, ms, what) {
  let t
  const timer = new Promise((_, rej) => {
    t = setTimeout(() => rej(new Error(`timed out after ${ms} ms: ${what}`)), ms)
  })
  return Promise.race([p, timer]).finally(() => clearTimeout(t))
}

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

class UsageError extends Error {}

function parseArgs(argv) {
  const [role, ...rest] = argv
  if (role !== 'provider' && role !== 'caller') {
    throw new UsageError(`first argument must be 'provider' or 'caller', got ${JSON.stringify(role)}`)
  }
  const allowed = role === 'provider' ? ['--manifest', '--shape', '--caller-node-id'] : ['--manifest', '--shape']
  const opts = {}
  for (let i = 0; i < rest.length; i += 2) {
    const flag = rest[i]
    if (!allowed.includes(flag)) throw new UsageError(`unexpected argument ${JSON.stringify(flag)} for ${role}`)
    if (flag in opts) throw new UsageError(`duplicate ${flag}`)
    if (i + 1 >= rest.length) throw new UsageError(`${flag} needs a value`)
    opts[flag] = rest[i + 1]
  }
  for (const flag of allowed) {
    if (!(flag in opts)) throw new UsageError(`missing ${flag}`)
  }
  const shape = opts['--shape']
  if (shape !== 'unary' && shape !== 'server-streaming') {
    throw new UsageError(`--shape must be unary or server-streaming, got ${JSON.stringify(shape)}`)
  }
  let callerNodeId
  if (role === 'provider') {
    const raw = opts['--caller-node-id']
    if (!/^\d+$/.test(raw) || BigInt(raw) > 0xffff_ffff_ffff_ffffn) {
      throw new UsageError(`--caller-node-id must be a decimal u64, got ${JSON.stringify(raw)}`)
    }
    callerNodeId = BigInt(raw)
  }
  return { role, manifestDir: resolve(opts['--manifest']), shape, callerNodeId }
}

// ---------------------------------------------------------------------------
// Binding + scenario
// ---------------------------------------------------------------------------

function loadBinding() {
  const require = createRequire(join(PKG_DIR, 'package.json'))
  const native = require(join(PKG_DIR, 'index.js'))
  const errors = require(join(PKG_DIR, 'errors.js'))
  for (const name of [
    'NetMesh',
    'OrgCredentials',
    'OrgClient',
    'serveOrg',
    'serveOrgStreaming',
    'installOrgAuthority',
    'installProviderGrantAudience',
  ]) {
    if (native[name] === undefined) {
      throw new Error(`binding at ${PKG_DIR} lacks ${name} (built without the org feature, or stale?)`)
    }
  }
  if (typeof errors.classifyOrgError !== 'function') {
    throw new Error(`binding at ${PKG_DIR} lacks errors.js classifyOrgError (run npm run build:ts)`)
  }
  return { native, classifyOrgError: errors.classifyOrgError }
}

function loadManifest(dir) {
  const m = JSON.parse(readFileSync(join(dir, 'manifest.json'), 'utf8'))
  return { m, p: (rel) => join(dir, rel) }
}

async function meshFromSeed(native, seedHex, pskHex) {
  return native.NetMesh.create({
    bindAddr: '127.0.0.1:0',
    psk: pskHex,
    identitySeed: Buffer.from(seedHex, 'hex'),
    heartbeatIntervalMs: 200,
    permissiveChannels: true,
  })
}

const hex = (b) => Buffer.from(b).toString('hex')

/** Structural JSON equality (key order irrelevant). */
function jsonEqual(a, b) {
  if (a === b) return true
  if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null) return false
  if (Array.isArray(a) !== Array.isArray(b)) return false
  const ka = Object.keys(a)
  const kb = Object.keys(b)
  if (ka.length !== kb.length) return false
  return ka.every((k) => Object.prototype.hasOwnProperty.call(b, k) && jsonEqual(a[k], b[k]))
}

function parseJson(buf, what) {
  try {
    return JSON.parse(Buffer.from(buf).toString('utf8'))
  } catch (e) {
    throw new Error(`${what} is not JSON (${oneLine(e)}): ${hex(buf).slice(0, 64)}`)
  }
}

async function shutdownMesh(mesh) {
  if (!mesh) return
  try {
    await withTimeout(mesh.shutdown(), SHUTDOWN_TIMEOUT_MS, 'mesh.shutdown()')
  } catch (e) {
    log(`shutdown: ${oneLine(e)}`)
  }
}

// ---------------------------------------------------------------------------
// Caller
// ---------------------------------------------------------------------------

async function runCaller(args) {
  const { native, classifyOrgError } = loadBinding()
  const { m, p } = loadManifest(args.manifestDir)
  const stdin = new StdinLines()

  // 1-2. Build (NOT started), print the id.
  const mesh = await meshFromSeed(native, m.caller.seed_hex, m.psk_hex)
  let client
  let verdict
  try {
    await emit(`ID ${mesh.nodeId().toString()}`)

    // 3. The provider's coordinates.
    const line = await stdin.next(CALLBACK_WATCHDOG_MS, 'the PROVIDER line')
    if (line === null) throw new Error('stdin closed before the PROVIDER line')
    const f = line.split(/\s+/)
    if (f.length !== 4 || f[0] !== 'PROVIDER' || !/^[0-9a-fA-F]{64}$/.test(f[2]) || !/^\d+$/.test(f[3])) {
      throw new Error(`bad PROVIDER line: ${JSON.stringify(line)}`)
    }
    const [, addr, pubHex, nodeIdStr] = f

    // 4. Connect only (never accept: start() rejects with an accept in
    //    flight), start, then install the authority.
    await mesh.connect(addr, pubHex.toLowerCase(), BigInt(nodeIdStr))
    log('connected to provider')
    await mesh.start()
    native.installOrgAuthority(mesh, p(m.caller.authority_dir))

    // 5. Credentials (audience secret by PATH).
    const credentials = native.OrgCredentials.create({
      membership: readFileSync(p(m.caller.membership_path)),
      dispatcher: readFileSync(p(m.caller.dispatcher_path)),
      grants: [readFileSync(p(m.caller.grant_path))],
      audienceSecretPaths: [p(m.caller.grant_secret_path)],
    })
    client = native.OrgClient.bind(mesh, credentials)

    // 6. Call, retrying ONLY local discovery failures.
    const open =
      args.shape === 'unary'
        ? () => client.callBytes(m.granted_service, REQUEST)
        : () => client.callStreamingBytes(m.granted_service, REQUEST)
    const deadline = Date.now() + DISCOVERY_BUDGET_MS
    let opened
    let attempts = 0
    for (;;) {
      attempts += 1
      try {
        opened = await open()
        break
      } catch (e) {
        const c = classifyOrgError(e)
        const domain = c && typeof c === 'object' ? c.domain : undefined
        if (domain !== 'discovery') {
          throw new Error(`call refused (attempt ${attempts}, not retried): ${oneLine(c)}`)
        }
        if (Date.now() + DISCOVERY_RETRY_MS > deadline) {
          throw new Error(`discovery never converged in ${DISCOVERY_BUDGET_MS} ms (${attempts} attempts): ${oneLine(c)}`)
        }
        if (attempts === 1 || attempts % 10 === 0) log(`discovery pending (attempt ${attempts}): ${oneLine(c)}`)
        // Best-effort; the provider's re-announce loop is what drives discovery.
        await mesh.announceCapabilities({}).catch(() => {})
        await sleep(DISCOVERY_RETRY_MS)
      }
    }
    log(`call opened after ${attempts} attempt(s)`)

    // 7. Check the response.
    if (args.shape === 'unary') {
      const got = parseJson(opened, 'unary response')
      const want = parseJson(UNARY_RESPONSE, 'expected')
      verdict = jsonEqual(got, want) ? null : `unary response ${JSON.stringify(got)} != ${JSON.stringify(want)}`
    } else {
      const stream = opened
      const items = []
      try {
        await withTimeout(
          (async () => {
            for (;;) {
              let item
              try {
                item = await stream.next()
              } catch (e) {
                throw new Error(`stream terminal error after ${items.length} item(s): ${oneLine(classifyOrgError(e))}`)
              }
              if (item === null || item === undefined) return
              items.push(parseJson(item, `stream item ${items.length}`))
              if (items.length > 64) throw new Error('stream produced more than 64 items')
            }
          })(),
          STREAM_DRAIN_TIMEOUT_MS,
          'draining the response stream',
        )
      } finally {
        // Bounded: after a drain timeout a parked `next()` may still hold the
        // stream's lock, and `close()` waits on it.
        await withTimeout(stream.close(), 5_000, 'stream.close()').catch((e) => log(oneLine(e)))
      }
      const want = STREAM_ITEMS.map((b) => parseJson(b, 'expected'))
      verdict =
        items.length === want.length && items.every((it, i) => jsonEqual(it, want[i]))
          ? null
          : `stream items ${JSON.stringify(items)} != ${JSON.stringify(want)}`
    }
  } catch (e) {
    verdict = oneLine(e)
  }

  // 8. Verdict, then teardown (client before shutdown).
  await emit(verdict === null ? 'RESULT ok' : `RESULT fail ${verdict}`)
  if (verdict !== null) log(`fail: ${verdict}`)
  try {
    client?.close()
  } catch (e) {
    log(`client.close: ${oneLine(e)}`)
  }
  await shutdownMesh(mesh)
  return verdict === null ? 0 : 1
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

async function runProvider(args) {
  const { native } = loadBinding()
  const { m, p } = loadManifest(args.manifestDir)
  const stdin = new StdinLines()
  const access = native.OrgAccess?.Granted ?? 'Granted'

  // 1-2. Build, install the authority.
  const mesh = await meshFromSeed(native, m.provider.seed_hex, m.psk_hex)
  let handle
  let announcing = false
  let announceLoop
  try {
    native.installOrgAuthority(mesh, p(m.provider.authority_dir))

    // 3-4. Arm accept in the background, then advertise.
    let acceptErr
    const accepted = mesh.accept(args.callerNodeId).then(
      (from) => from,
      (e) => {
        acceptErr = e
        throw e
      },
    )
    accepted.catch(() => {}) // observed below; never an unhandled rejection
    await emit(`READY ${mesh.localAddr()} ${mesh.publicKey().toLowerCase()} ${mesh.nodeId().toString()}`)

    // 5. Handshake, then start.
    try {
      const from = await withTimeout(accepted, ACCEPT_TIMEOUT_MS, 'accept(caller)')
      log(`accepted caller from ${from}`)
    } catch (e) {
      throw new Error(`accept: ${oneLine(acceptErr ?? e)}`)
    }
    await mesh.start()

    // 6. Serve (handle kept until DRAINED), THEN the grant audience.
    const state = { calls: 0, facts: null, problems: [] }
    let fire
    const fired = new Promise((r) => (fire = r))
    const onCall = (caller, request) => {
      state.calls += 1
      if (state.calls === 1) {
        state.facts = {
          actingOrg: hex(caller.actingOrg),
          providerOrg: hex(caller.providerOrg),
          isSameOrg: caller.isSameOrg,
          entity: hex(caller.entity),
        }
      }
      let n
      try {
        n = parseJson(request, 'request').n
      } catch (e) {
        state.problems.push(oneLine(e))
      }
      if (n !== 7) state.problems.push(`request n=${JSON.stringify(n)} want 7`)
      fire()
      if (n !== 7) throw new Error(`bad request: n=${JSON.stringify(n)}`)
    }

    if (args.shape === 'unary') {
      handle = native.serveOrg(mesh, m.granted_service, access, async (req) => {
        onCall(req.caller, req.request)
        return UNARY_RESPONSE
      })
    } else {
      handle = native.serveOrgStreaming(mesh, m.granted_service, access, async (callArgs) => {
        const [caller, request, sink] = callArgs
        onCall(caller, request)
        for (const item of STREAM_ITEMS) {
          if (!sink.send(item)) {
            state.problems.push('sink.send returned false (sink closed mid-stream)')
            break
          }
        }
        return Buffer.alloc(0)
      })
    }
    native.installProviderGrantAudience(mesh, readFileSync(p(m.provider.grant_path)), p(m.provider.grant_secret_path))
    log(`serving ${m.granted_service} (${args.shape}) as Granted`)

    // 7. Re-announce about once a second until the handler has run.
    announcing = true
    announceLoop = (async () => {
      while (announcing) {
        try {
          await mesh.announceCapabilities({})
        } catch (e) {
          log(`announce: ${oneLine(e)}`)
        }
        await sleep(ANNOUNCE_EVERY_MS)
      }
    })()

    const ran = await Promise.race([fired.then(() => true), sleep(CALLBACK_WATCHDOG_MS).then(() => false)])
    announcing = false
    if (!ran) {
      await emit('RESULT fail callback-loss')
      log('the handler never ran')
      try {
        handle.close()
      } catch {
        /* best effort */
      }
      await shutdownMesh(mesh)
      return 2
    }

    // 8. The verified caller facts.
    const facts = state.facts
    const factProblems = []
    if (facts.actingOrg !== m.caller.org_id_hex.toLowerCase()) {
      factProblems.push(`acting_org=${facts.actingOrg} want ${m.caller.org_id_hex}`)
    }
    if (facts.providerOrg !== m.provider.org_id_hex.toLowerCase()) {
      factProblems.push(`provider_org=${facts.providerOrg} want ${m.provider.org_id_hex}`)
    }
    if (facts.isSameOrg !== false) factProblems.push(`is_same_org=${facts.isSameOrg} want false`)
    log(`handler facts: entity=${facts.entity} acting_org=${facts.actingOrg} provider_org=${facts.providerOrg} same_org=${facts.isSameOrg}`)
    await emit(factProblems.length === 0 ? 'FACTS ok' : `FACTS fail ${factProblems.join('; ')}`)

    // 9. Hold the serve handle until the caller has drained.
    let drainedProblem
    try {
      const line = await stdin.next(DRAINED_TIMEOUT_MS, 'DRAINED')
      if (line === null) drainedProblem = 'stdin closed before DRAINED'
      else if (line !== 'DRAINED') drainedProblem = `unexpected stdin line ${JSON.stringify(line)} (want DRAINED)`
    } catch (e) {
      drainedProblem = oneLine(e)
    }
    try {
      handle.close()
    } catch (e) {
      log(`handle.close: ${oneLine(e)}`)
    }
    handle = undefined

    // 10. Verdict.
    const problems = [...factProblems, ...state.problems]
    if (state.calls !== 1) problems.push(`calls=${state.calls} want 1`)
    if (drainedProblem) problems.push(drainedProblem)
    await shutdownMesh(mesh)
    await emit(problems.length === 0 ? 'RESULT ok calls=1' : `RESULT fail ${problems.join('; ')}`)
    return problems.length === 0 ? 0 : 1
  } catch (e) {
    announcing = false
    await emit(`RESULT fail ${oneLine(e)}`)
    log(`fail: ${oneLine(e)}`)
    try {
      handle?.close()
    } catch {
      /* best effort */
    }
    await shutdownMesh(mesh)
    return 1
  } finally {
    announcing = false
    void announceLoop
  }
}

// ---------------------------------------------------------------------------

async function main() {
  let args
  try {
    args = parseArgs(process.argv.slice(2))
  } catch (e) {
    log(`usage: node driver.mjs provider --manifest <dir> --shape <unary|server-streaming> --caller-node-id <u64>`)
    log(`       node driver.mjs caller   --manifest <dir> --shape <unary|server-streaming>`)
    await emit(`RESULT fail usage: ${oneLine(e)}`)
    return finish(1)
  }
  log(`role=${args.role} shape=${args.shape} binding=${PKG_DIR}`)
  let code
  try {
    code = args.role === 'provider' ? await runProvider(args) : await runCaller(args)
  } catch (e) {
    // Setup failures before the role's own error handling (binding load,
    // manifest read, mesh construction).
    await emit(`RESULT fail ${oneLine(e)}`)
    log(`fatal: ${e instanceof Error && e.stack ? e.stack : String(e)}`)
    code = 1
  }
  return finish(code)
}

process.on('unhandledRejection', (e) => log(`unhandled rejection: ${oneLine(e)}`))
await main()
