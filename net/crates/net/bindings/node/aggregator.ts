// Typed error classes + classifier for the `aggregator.registry` /
// `fold.query` RPC clients (Stage 2 of `SDK_AGGREGATOR_SUBNET_PLAN.md`).
//
// The napi binding throws plain `Error` objects with the stable
// `agg:` prefix (`agg:<kind>: <detail>`). `classifyAggregatorError`
// inspects the kind segment and re-throws a typed
// `RegistryClientError` / `FoldQueryClientError`. Catch with
// `instanceof`:
//
//   import {
//     RegistryClient,
//     classifyAggregatorError,
//     RegistryClientError,
//   } from '@net-mesh/core/aggregator';
//
//   try {
//     await client.spawn(target, 'reservation', 'res-1', 3);
//   } catch (e) {
//     const typed = classifyAggregatorError(e);
//     if (typed instanceof RegistryClientError && typed.kind === 'unknown-template') {
//       // ...
//     }
//     throw typed;
//   }
//
// The prefix string is locked in lockstep with `ERR_AGG_PREFIX` in
// `bindings/node/src/aggregator.rs`. The kind segment matches the
// substrate's `RegistryClientError` / `FoldQueryClientError`
// variants per `SDK_AGGREGATOR_SUBNET_PLAN.md` § "Stage 2".

const ERR_AGG_PREFIX = 'agg:'

export type RegistryErrorKind =
  | 'transport'
  | 'codec'
  | 'unknown-template'
  | 'duplicate-group-name'
  | 'spawn-rejected'
  | 'spawn-not-supported'
  | 'invalid-args'

export type FoldQueryErrorKind =
  | 'transport'
  | 'codec'
  | 'unknown-kind'
  | 'invalid-args'

export class RegistryClientError extends Error {
  readonly kind: RegistryErrorKind
  readonly serverDetail?: string
  constructor(kind: RegistryErrorKind, detail: string) {
    super(`${kind}: ${detail}`)
    this.name = 'RegistryClientError'
    this.kind = kind
    this.serverDetail = detail
    Object.setPrototypeOf(this, RegistryClientError.prototype)
  }
}

export class FoldQueryClientError extends Error {
  readonly kind: FoldQueryErrorKind
  readonly serverDetail?: string
  constructor(kind: FoldQueryErrorKind, detail: string) {
    super(`${kind}: ${detail}`)
    this.name = 'FoldQueryClientError'
    this.kind = kind
    this.serverDetail = detail
    Object.setPrototypeOf(this, FoldQueryClientError.prototype)
  }
}

const REGISTRY_KINDS: ReadonlySet<string> = new Set([
  'transport',
  'codec',
  'unknown-template',
  'duplicate-group-name',
  'spawn-rejected',
  'spawn-not-supported',
  'invalid-args',
])

const FOLD_KINDS: ReadonlySet<string> = new Set([
  'transport',
  'codec',
  'unknown-kind',
  'invalid-args',
])

/**
 * Inspect an error message for the `agg:` prefix and return the
 * structured `{kind, detail}` if it matches. Returns `null` when
 * the message is missing the prefix or is malformed.
 */
export function parseAggregatorError(
  e: unknown,
): { kind: string; detail: string } | null {
  const msg = extractMessage(e)
  if (!msg.startsWith(ERR_AGG_PREFIX)) return null
  const after = msg.slice(ERR_AGG_PREFIX.length)
  // Find the FIRST `: ` separator — kinds are stable kebab-case
  // identifiers with no colons; the detail may contain colons.
  const sepIdx = after.indexOf(': ')
  if (sepIdx === -1) {
    // No detail, just the kind (defensive — substrate always emits
    // a detail string today).
    return { kind: after, detail: '' }
  }
  return {
    kind: after.slice(0, sepIdx),
    detail: after.slice(sepIdx + 2),
  }
}

/**
 * Re-throw a typed error for the given raw error if it carries the
 * `agg:` prefix. Non-matching errors pass through unchanged so
 * `throw classifyAggregatorError(e)` is safe at any catch site.
 *
 * Routes to `RegistryClientError` for registry-shaped kinds and
 * `FoldQueryClientError` for fold-query-shaped kinds. Both surfaces
 * share `transport` / `codec` / `invalid-args`; we route by the
 * caller's typing — pass an explicit `surface` if you know which
 * client raised it, otherwise the default routing biases to
 * `RegistryClientError` for shared kinds (the substrate's primary
 * surface).
 */
export function classifyAggregatorError(
  e: unknown,
  surface?: 'registry' | 'fold-query',
): unknown {
  const parsed = parseAggregatorError(e)
  if (!parsed) return e

  // Surface-specific kinds take priority — they are unambiguous.
  if (parsed.kind === 'unknown-kind') {
    return new FoldQueryClientError('unknown-kind', parsed.detail)
  }
  if (
    parsed.kind === 'unknown-template' ||
    parsed.kind === 'duplicate-group-name' ||
    parsed.kind === 'spawn-rejected' ||
    parsed.kind === 'spawn-not-supported'
  ) {
    return new RegistryClientError(parsed.kind, parsed.detail)
  }

  // Shared kinds — route by caller hint, default to registry.
  if (
    parsed.kind === 'transport' ||
    parsed.kind === 'codec' ||
    parsed.kind === 'invalid-args'
  ) {
    if (surface === 'fold-query') {
      return new FoldQueryClientError(
        parsed.kind as FoldQueryErrorKind,
        parsed.detail,
      )
    }
    return new RegistryClientError(
      parsed.kind as RegistryErrorKind,
      parsed.detail,
    )
  }

  // Unknown kind under the `agg:` umbrella — keep the original
  // error so callers see the full string instead of a synthetic
  // typed wrapper that drops information.
  void REGISTRY_KINDS
  void FOLD_KINDS
  return e
}

function extractMessage(e: unknown): string {
  if (e === null || e === undefined) return ''
  if (typeof e === 'string') return e
  if (typeof e !== 'object') return ''
  const msg = (e as { message?: unknown }).message
  return typeof msg === 'string' ? msg : ''
}

// ============================================================================
// Typed re-exports of the napi class shapes.
// ============================================================================
//
// The napi-generated `index.d.ts` already declares `RegistryClient`
// + `FoldQueryClient` + the POJOs. This module re-exports them
// under stable names so consumers don't have to mix imports
// (`@net-mesh/core` for classes, `@net-mesh/core/aggregator` for
// error helpers). We intentionally re-export by re-declaration
// rather than `export { ... } from './index'` because the
// generated `.d.ts` does not have a corresponding `.js` re-export
// surface for napi-class constructors that lives next to it — the
// napi loader is required.

// Mirrors `RegistryReplicaRowJs` in `src/aggregator.rs`.
export interface RegistryReplicaRow {
  generation: bigint
  healthy: boolean
  diagnostic?: string
  placementNodeId?: bigint
}

// Mirrors `RegistryGroupSummaryJs` in `src/aggregator.rs`.
export interface RegistryGroupSummary {
  name: string
  groupSeedHex: string
  replicas: RegistryReplicaRow[]
}

// Mirrors `SummaryBucketJs` in `src/aggregator.rs`.
export interface SummaryBucket {
  name: string
  count: bigint
}

// Mirrors `SummaryAnnouncementJs` in `src/aggregator.rs`.
export interface SummaryAnnouncement {
  foldKind: number
  sourceSubnet: string
  generation: bigint
  buckets: SummaryBucket[]
}
