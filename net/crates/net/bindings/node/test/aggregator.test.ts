// Error-classification smoke tests for the aggregator surface.
//
// The round-trip test against a daemon subprocess lives at
// `aggregator_registry_roundtrip.test.ts` (RUN_INTEGRATION_TESTS
// gate). This file pins the prefix → typed-error mapping every
// SDK consumer relies on; it does NOT require the napi `.node`
// (the classifier is pure TS) so vitest can run it on any host.

import { describe, expect, it } from 'vitest'

import {
  classifyAggregatorError,
  FoldQueryClientError,
  parseAggregatorError,
  RegistryClientError,
} from '../aggregator'

function fakeErr(msg: string): Error {
  return new Error(msg)
}

describe('parseAggregatorError', () => {
  it('extracts kind + detail from a well-formed agg: prefix', () => {
    const parsed = parseAggregatorError(
      fakeErr('agg:unknown-template: reservation-v2'),
    )
    expect(parsed).toEqual({
      kind: 'unknown-template',
      detail: 'reservation-v2',
    })
  })

  it('preserves colons inside the detail body', () => {
    const parsed = parseAggregatorError(
      fakeErr('agg:transport: timed out: elapsed_ms=5000'),
    )
    expect(parsed?.kind).toBe('transport')
    expect(parsed?.detail).toBe('timed out: elapsed_ms=5000')
  })

  it('returns null when the prefix is missing', () => {
    expect(parseAggregatorError(fakeErr('nrpc:no_route: x'))).toBeNull()
    expect(parseAggregatorError(fakeErr('plain old error'))).toBeNull()
    expect(parseAggregatorError(null)).toBeNull()
    expect(parseAggregatorError(undefined)).toBeNull()
    expect(parseAggregatorError(42)).toBeNull()
  })

  it('accepts string-thrown values (not just Error instances)', () => {
    const parsed = parseAggregatorError('agg:codec: bad bytes')
    expect(parsed).toEqual({ kind: 'codec', detail: 'bad bytes' })
  })
})

describe('classifyAggregatorError — registry kinds', () => {
  it.each([
    ['unknown-template', 'reservation'],
    ['duplicate-group-name', 'res-1'],
    ['spawn-rejected', 'no_capacity'],
    ['spawn-not-supported', 'daemon is read-only'],
  ] as const)('routes %s → RegistryClientError', (kind, detail) => {
    const typed = classifyAggregatorError(fakeErr(`agg:${kind}: ${detail}`))
    expect(typed).toBeInstanceOf(RegistryClientError)
    expect(typed).toBeInstanceOf(Error)
    const e = typed as RegistryClientError
    expect(e.kind).toBe(kind)
    expect(e.serverDetail).toBe(detail)
    expect(e.name).toBe('RegistryClientError')
  })
})

describe('classifyAggregatorError — fold-query-specific kinds', () => {
  it('routes unknown-kind → FoldQueryClientError', () => {
    const typed = classifyAggregatorError(
      fakeErr('agg:unknown-kind: 0x0042'),
    )
    expect(typed).toBeInstanceOf(FoldQueryClientError)
    const e = typed as FoldQueryClientError
    expect(e.kind).toBe('unknown-kind')
    expect(e.serverDetail).toBe('0x0042')
  })
})

describe('classifyAggregatorError — shared kinds (transport / codec / invalid-args)', () => {
  it('defaults shared kinds to RegistryClientError', () => {
    const t = classifyAggregatorError(fakeErr('agg:transport: nope'))
    expect(t).toBeInstanceOf(RegistryClientError)
    expect((t as RegistryClientError).kind).toBe('transport')

    const c = classifyAggregatorError(fakeErr('agg:codec: bad framing'))
    expect(c).toBeInstanceOf(RegistryClientError)
    expect((c as RegistryClientError).kind).toBe('codec')

    const i = classifyAggregatorError(
      fakeErr('agg:invalid-args: targetNodeId must be a u64'),
    )
    expect(i).toBeInstanceOf(RegistryClientError)
    expect((i as RegistryClientError).kind).toBe('invalid-args')
  })

  it('routes shared kinds to FoldQueryClientError when surface hint is fold-query', () => {
    const t = classifyAggregatorError(
      fakeErr('agg:transport: nope'),
      'fold-query',
    )
    expect(t).toBeInstanceOf(FoldQueryClientError)
    expect((t as FoldQueryClientError).kind).toBe('transport')
  })
})

describe('classifyAggregatorError — non-aggregator errors pass through', () => {
  it('returns the original error when prefix is missing', () => {
    const e = fakeErr('plain error not from the agg surface')
    expect(classifyAggregatorError(e)).toBe(e)
  })

  it('returns the original error for unknown agg kinds', () => {
    // Defensive: an unrecognized kind under the agg: umbrella should
    // not be wrapped — wrapping would drop the original Error
    // identity and confuse callers depending on stack traces.
    const e = fakeErr('agg:future-kind-we-do-not-know: detail')
    expect(classifyAggregatorError(e)).toBe(e)
  })

  it('classifies string-thrown values when kind is recognized', () => {
    const typed = classifyAggregatorError('agg:unknown-template: foo')
    expect(typed).toBeInstanceOf(RegistryClientError)
  })
})

describe('RegistryClientError / FoldQueryClientError class shape', () => {
  it('preserves prototype across throw / catch boundary', () => {
    const err = new RegistryClientError('unknown-template', 'foo')
    try {
      throw err
    } catch (caught) {
      expect(caught).toBeInstanceOf(RegistryClientError)
      expect(caught).toBeInstanceOf(Error)
    }
  })

  it('error subclasses are independent', () => {
    const r = new RegistryClientError('transport', 'x')
    const f = new FoldQueryClientError('transport', 'x')
    expect(r instanceof FoldQueryClientError).toBe(false)
    expect(f instanceof RegistryClientError).toBe(false)
  })

  it('message body includes both kind and detail', () => {
    const r = new RegistryClientError('duplicate-group-name', 'res-1')
    expect(r.message).toBe('duplicate-group-name: res-1')
  })
})
