// Error-class smoke tests — verifies the napi binding emits stable
// `cortex:` / `netdb:` prefixes and that `classifyError()` rehydrates
// them into typed `CortexError` / `NetDbError` instances.

import { describe, expect, it } from 'vitest'

import { MemoriesAdapter, NetDb, Redex, TasksAdapter } from '../index'
import { classifyError, CortexError, NetDbError } from '../errors'

const ORIGIN = 0xabcdef01n

function nowNs(): bigint {
  return BigInt(Date.now()) * 1_000_000n
}

describe('error classification', () => {
  it('tasks operations on a closed adapter throw a cortex-prefixed error', async () => {
    const redex = new Redex()
    const tasks = await TasksAdapter.open(redex, ORIGIN)
    tasks.close()

    let caught: Error | null = null
    try {
      tasks.create(1n, 'x', nowNs())
    } catch (e) {
      caught = e as Error
    }
    expect(caught).not.toBeNull()
    expect(caught!.message.startsWith('cortex:')).toBe(true)

    const typed = classifyError(caught)
    expect(typed).toBeInstanceOf(CortexError)
    expect(typed).toBeInstanceOf(Error)
  })

  it('memories operations on a closed adapter throw a cortex-prefixed error', async () => {
    const redex = new Redex()
    const memories = await MemoriesAdapter.open(redex, ORIGIN)
    memories.close()

    let caught: Error | null = null
    try {
      memories.store(1n, 'x', ['t'], 's', nowNs())
    } catch (e) {
      caught = e as Error
    }
    expect(caught).not.toBeNull()
    expect(classifyError(caught)).toBeInstanceOf(CortexError)
  })

  it('persistent=true without persistent_dir raises a CortexError', async () => {
    const redex = new Redex() // heap-only
    let caught: Error | null = null
    try {
      await TasksAdapter.open(redex, ORIGIN, true)
    } catch (e) {
      caught = e as Error
    }
    expect(caught).not.toBeNull()
    expect(caught!.message).toMatch(/persistent/)
    expect(classifyError(caught)).toBeInstanceOf(CortexError)
  })

  it('NetDb.openFromSnapshot on garbage bytes raises a NetDbError', async () => {
    const garbage = Buffer.from([0xff, 0xff, 0xff, 0x00, 0xff])
    let caught: Error | null = null
    try {
      await NetDb.openFromSnapshot(
        { originHash: ORIGIN, withTasks: true },
        { stateBytes: garbage },
      )
    } catch (e) {
      caught = e as Error
    }
    expect(caught).not.toBeNull()
    expect(caught!.message.startsWith('netdb:')).toBe(true)
    expect(classifyError(caught)).toBeInstanceOf(NetDbError)
  })

  it('classifyError passes unknown errors through unchanged', () => {
    const unrelated = new Error('something totally different')
    expect(classifyError(unrelated)).toBe(unrelated)
  })

  it('CortexError and NetDbError are independent Error subclasses', () => {
    const c = new CortexError('x')
    const n = new NetDbError('y')
    expect(c instanceof Error).toBe(true)
    expect(n instanceof Error).toBe(true)
    expect(c instanceof NetDbError).toBe(false)
    expect(n instanceof CortexError).toBe(false)
    expect(c.name).toBe('CortexError')
    expect(n.name).toBe('NetDbError')
  })
})

describe('regression: BigInt boundary validation', () => {
  // Regression: `bigint_u64` in the napi layer used to silently drop
  // the `signed` + `lossless` flags from `BigInt::get_u64()`. A
  // negative or >u64::MAX BigInt would then be silently truncated into
  // a wrong u64 value — corrupting ids, timestamps, and sequences
  // at the FFI boundary. The fix rejects both cases with an explicit
  // throw. These tests lock the behavior in place.

  it('rejects a negative BigInt id', async () => {
    const redex = new Redex()
    const tasks = await TasksAdapter.open(redex, ORIGIN)
    expect(() => tasks.create(-1n, 'x', nowNs())).toThrow(/non-negative/)
  })

  it('rejects a BigInt id that exceeds u64::MAX', async () => {
    const redex = new Redex()
    const tasks = await TasksAdapter.open(redex, ORIGIN)
    // 2^65 > u64::MAX: `get_u64()` reports `lossless = false`.
    expect(() => tasks.create(2n ** 65n, 'x', nowNs())).toThrow(/u64 range/)
  })

  it('rejects a negative BigInt timestamp', async () => {
    const redex = new Redex()
    const tasks = await TasksAdapter.open(redex, ORIGIN)
    expect(() => tasks.create(1n, 'x', -100n)).toThrow(/non-negative/)
  })

  it('rejects an out-of-range BigInt on memories.store', async () => {
    const redex = new Redex()
    const memories = await MemoriesAdapter.open(redex, ORIGIN)
    expect(() => memories.store(-1n, 'x', ['t'], 'src', nowNs())).toThrow(
      /non-negative/,
    )
    expect(() =>
      memories.store(2n ** 70n, 'x', ['t'], 'src', nowNs()),
    ).toThrow(/u64 range/)
  })
})

// Paid A2A (NODE_A2A_PAID_ADMISSION_PLAN.md D4). Pure classification of the
// native prefixes; the live refusals that produce them are pinned in
// a2a_paid.test.ts.
describe('paid a2a error classification', async () => {
  const {
    classifyError: classify,
    PaymentRefusedError,
    JournalOwnedElsewhereError,
    A2aInvalidArgumentError,
  } = await import('../errors')

  it('splits a payment refusal into its message and its untouched schematic', () => {
    // A schematic number above 2^53 in the open `extra` map must survive
    // byte-for-byte: the classifier never re-encodes the schematic.
    const schematic =
      '{"object":"net.payment.failure@1","reason":"no_reservation","extra":{"n":9007199254740993}}'
    const typed = classify(
      new Error(`a2a:payment_refused: ${schematic}\nno reservation; prepare first`),
    ) as InstanceType<typeof PaymentRefusedError>
    expect(typed).toBeInstanceOf(PaymentRefusedError)
    expect(typed.message).toBe('no reservation; prepare first')
    expect(typed.schematic).toBe(schematic)
  })

  it('keeps a refusal with no schematic, and a malformed one whole', () => {
    const none = classify(new Error('a2a:payment_refused: null\nrefused')) as InstanceType<
      typeof PaymentRefusedError
    >
    expect(none).toBeInstanceOf(PaymentRefusedError)
    expect(none.schematic).toBeUndefined()
    expect(none.message).toBe('refused')
    const odd = classify(new Error('a2a:payment_refused: not the documented shape')) as Error
    expect(odd).toBeInstanceOf(PaymentRefusedError)
    expect(odd.message).toBe('a2a:payment_refused: not the documented shape')
  })

  it('classifies journal ownership and invalid-argument refusals', () => {
    expect(classify(new Error('a2a:journal_owned_elsewhere: held'))).toBeInstanceOf(
      JournalOwnedElsewhereError,
    )
    expect(classify(new Error('a2a:invalid_argument: bad pointer'))).toBeInstanceOf(
      A2aInvalidArgumentError,
    )
    // The plain `a2a:` prefix (a failure underneath) is not one of them: it
    // passes through unchanged.
    const transport = new Error('a2a: describeA2a: no route')
    expect(classify(transport)).toBe(transport)
  })
})
