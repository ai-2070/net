// Cross-binding nRPC wire-format compat — Phase B7.
//
// Loads the shared `tests/cross_lang_nrpc/golden_vectors.json`
// fixture (the same one driving the Rust + Python tests) and
// asserts the canonical `cross_lang_echo_sum` service round-trips
// correctly through the Node binding's TypedMeshRpc surface.
//
// The handler is implemented inline (no live mesh required); the
// test exercises the JSON codec + typed-error mapping by piping
// the encoded request through a stub raw MeshRpc that runs the
// handler in-process and returns the encoded response. This is
// the same wire-format-compat pattern documented in
// `net/crates/net/README.md#nrpc`.

import { existsSync, readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

import {
  classifyError,
  RpcServerError,
} from '../errors'
import {
  NRPC_TYPED_BAD_REQUEST,
  TypedMeshRpc,
} from '../mesh_rpc'

// ---------------------------------------------------------------
// Fixture loader
// ---------------------------------------------------------------

interface OkCase {
  name: string
  request_json: unknown
  expected_response_json: { echo: string; sum: number }
}
interface ErrorCase {
  name: string
  request_json: unknown
  expected_status: number
}
interface Fixture {
  service: string
  abi_version_expected: number
  ok_cases: OkCase[]
  error_cases: ErrorCase[]
}

const FIXTURE_PATH = resolve(
  __dirname,
  '../../../tests/cross_lang_nrpc/golden_vectors.json',
)
if (!existsSync(FIXTURE_PATH)) {
  // Surface a clear error pointing at the missing fixture, not a
  // generic JSON parse failure. The path is brittle relative to
  // __dirname; if the test gets moved or vitest runs from a
  // different cwd, the failure mode should be diagnosable
  // without diff-spelunking. Pinned by `fixture_present` test
  // below.
  throw new Error(
    `cross_lang nRPC fixture not found at ${FIXTURE_PATH} — has the test ` +
      `or fixture moved? The contract expects the fixture under ` +
      `net/crates/net/tests/cross_lang_nrpc/golden_vectors.json relative ` +
      `to the workspace root.`,
  )
}
const fixture: Fixture = JSON.parse(readFileSync(FIXTURE_PATH, 'utf-8'))

// ---------------------------------------------------------------
// Canonical handler (JS implementation of the contract)
// ---------------------------------------------------------------

interface EchoSumRequest {
  text: string
  numbers: number[]
}
interface EchoSumResponse {
  echo: string
  sum: number
}

function isValidRequest(v: unknown): v is EchoSumRequest {
  if (typeof v !== 'object' || v === null) return false
  const r = v as Record<string, unknown>
  if (typeof r.text !== 'string') return false
  if (!Array.isArray(r.numbers)) return false
  return r.numbers.every((n) => typeof n === 'number' && Number.isFinite(n))
}

function handleEchoSum(req: EchoSumRequest): EchoSumResponse {
  // Plain reduce; the fixture's sums all fit comfortably in Number.
  return { echo: req.text, sum: req.numbers.reduce((a, b) => a + b, 0) }
}

// ---------------------------------------------------------------
// Stub raw MeshRpc that loops back through the canonical handler.
// On a malformed request the stub emits the typed-bad-request
// status as a thrown nrpc:server_error: status=0x8000 — matches
// what the real native binding does when a handler signals
// RpcStatus::Application(NRPC_TYPED_BAD_REQUEST).
// ---------------------------------------------------------------

class LoopbackHandlerRpc {
  async call(_target: bigint, service: string, req: Buffer): Promise<Buffer> {
    return this.dispatch(service, req)
  }
  async callService(service: string, req: Buffer): Promise<Buffer> {
    return this.dispatch(service, req)
  }
  async callStreaming(): Promise<never> {
    throw new Error('streaming not exercised by cross-lang compat')
  }
  serve(): never {
    throw new Error('serve not exercised by cross-lang compat')
  }
  findServiceNodes(): bigint[] {
    return []
  }
  private dispatch(service: string, reqBytes: Buffer): Buffer {
    if (service !== fixture.service) {
      throw new Error(`nrpc:no_route: unknown service ${service}`)
    }
    let parsed: unknown
    try {
      parsed = JSON.parse(reqBytes.toString('utf-8'))
    } catch (e) {
      throw new Error(
        `nrpc:server_error: status=0x${NRPC_TYPED_BAD_REQUEST.toString(16)} message=invalid_json: ${
          (e as Error).message
        }`,
      )
    }
    if (!isValidRequest(parsed)) {
      throw new Error(
        `nrpc:server_error: status=0x${NRPC_TYPED_BAD_REQUEST.toString(16)} message=invalid_request_shape`,
      )
    }
    const resp = handleEchoSum(parsed)
    return Buffer.from(JSON.stringify(resp), 'utf-8')
  }
}

// ---------------------------------------------------------------
// Tests
// ---------------------------------------------------------------

describe('Cross-language nRPC wire-format compat (Node side)', () => {
  it('fixture file is present at the expected location', () => {
    // Pinned because the path is brittle relative to __dirname;
    // a future test refactor could silently break the loader and
    // the only signal would be a JSON parse error. The
    // top-of-file existsSync guard already throws — this test
    // makes the invariant visible in the suite output.
    expect(existsSync(FIXTURE_PATH), `fixture missing at ${FIXTURE_PATH}`).toBe(true)
  })

  it('fixture metadata matches the canonical contract constants', () => {
    expect(fixture.service).toBe('cross_lang_echo_sum')
    expect(fixture.abi_version_expected).toBe(0x0001)
    expect(NRPC_TYPED_BAD_REQUEST).toBe(0x8000)
    expect(fixture.ok_cases.length).toBeGreaterThan(0)
    expect(fixture.error_cases.length).toBeGreaterThan(0)
  })

  it('all ok cases round-trip via TypedMeshRpc.call', async () => {
    const rpc = new TypedMeshRpc(new LoopbackHandlerRpc() as unknown)
    for (const oc of fixture.ok_cases) {
      const reply = await rpc.call(0n, fixture.service, oc.request_json)
      expect(reply, `ok-case '${oc.name}'`).toEqual(oc.expected_response_json)
    }
  })

  it('all ok cases also round-trip via TypedMeshRpc.callService', async () => {
    const rpc = new TypedMeshRpc(new LoopbackHandlerRpc() as unknown)
    for (const oc of fixture.ok_cases) {
      const reply = await rpc.callService(fixture.service, oc.request_json)
      expect(reply, `ok-case '${oc.name}' via callService`).toEqual(
        oc.expected_response_json,
      )
    }
  })

  it('error cases surface as nrpc:server_error with the documented status', async () => {
    const rpc = new TypedMeshRpc(new LoopbackHandlerRpc() as unknown)
    for (const ec of fixture.error_cases) {
      let caught: Error | null = null
      try {
        await rpc.call(0n, fixture.service, ec.request_json)
      } catch (e) {
        caught = e as Error
      }
      expect(caught, `error-case '${ec.name}' must throw`).not.toBeNull()
      const typed = classifyError(caught) as RpcServerError
      expect(typed, `error-case '${ec.name}' classifies as RpcServerError`).toBeInstanceOf(
        RpcServerError,
      )
      expect(typed.status, `error-case '${ec.name}' status`).toBe(ec.expected_status)
    }
  })
})
