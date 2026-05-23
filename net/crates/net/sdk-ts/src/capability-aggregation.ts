/**
 * Capability aggregation surface — Phase 6c of
 * `MULTIFOLD_PHASE_6C_CAPACITY_AGGREGATION.md`.
 *
 * Three composable primitives map onto the Rust core's
 * `Fold::aggregate` and `Fold::capacity_ranking`:
 *
 *  - {@link TagMatcher} — picks which entries the aggregation
 *    walks. Tagged-union: `kind: 'exact' | 'prefix' | 'axis' |
 *    'axisKey' | 'regex' | 'versionRange'`.
 *  - {@link GroupBy} — buckets matching entries.
 *  - {@link Aggregation} — reduces each bucket to a number.
 *
 * {@link CapacityQuery} composes a matcher + groupBy + optional RTT
 * filter + optional summed-capacity axis into a single
 * `capacityRanking(...)` call; the materialized view returns
 * per-bucket state breakdown sorted by available capacity.
 *
 * The Rust core takes JSON-encoded tagged unions over the napi
 * boundary; these helpers handle the conversion so TS callers work
 * with idiomatic discriminated unions.
 */

/** Taxonomy axis — matches the Rust core's `TaxonomyAxis` enum. */
export type TaxonomyAxis = 'hardware' | 'software' | 'devices' | 'dataforts';

/**
 * Pre-grouping filter. An entry is included if any of its tags
 * matches the matcher.
 */
export type TagMatcher =
  | { kind: 'exact'; value: string }
  | { kind: 'prefix'; value: string }
  | { kind: 'axis'; axis: TaxonomyAxis }
  | { kind: 'axisKey'; axis: TaxonomyAxis; key: string }
  | { kind: 'regex'; pattern: string }
  | {
      kind: 'versionRange';
      axisKey: string;
      min?: string;
      max?: string;
    };

/** Bucket-key derivation. */
export type GroupBy =
  | { kind: 'class' }
  | { kind: 'state' }
  | { kind: 'region' }
  | { kind: 'publisher' }
  | { kind: 'tagStem'; prefix: string }
  | { kind: 'tagValue'; axis: TaxonomyAxis; key: string };

/** Per-bucket reduction. */
export type Aggregation =
  | { kind: 'count' }
  | { kind: 'distinctPublishers' }
  | { kind: 'distinctValues'; axis: TaxonomyAxis; key: string }
  | { kind: 'sumNumericTag'; axisKey: string }
  | { kind: 'minNumericTag'; axisKey: string }
  | { kind: 'maxNumericTag'; axisKey: string };

/** Composed capacity-ranking query. */
export interface CapacityQuery {
  /** Optional pre-filter. */
  matcher?: TagMatcher;
  /** How to bucket matching entries. */
  groupBy: GroupBy;
  /**
   * Drop entries whose publisher's RTT exceeds this. `undefined`
   * disables the RTT filter (the `rttEntries` argument to
   * {@link MeshNode.capabilityCapacityRanking} is unused regardless).
   */
  maxRttMs?: number;
  /**
   * Canonical `<axis>.<key>` of a numeric tag to sum across each
   * bucket (e.g. `"hardware.gpu.count"`).
   */
  sumAxisKey?: string;
  /** Top-N buckets by `available` descending. `0` = no truncation. */
  limit: number;
}

/** One row of an aggregate result. */
export interface AggregateRow {
  bucket: string;
  value: bigint;
}

/** One row of a capacity-ranking result. */
export interface CapacityRow {
  /** Bucket key. */
  bucket: string;
  /** Entries in `Idle` that pass the matcher + RTT gates. */
  idle: bigint;
  /** Entries in `Busy` that pass. */
  busy: bigint;
  /** Entries in `Reserved` that pass. */
  reserved: bigint;
  /** `idle + busy + reserved`. Faulty entries are always excluded. */
  available: bigint;
  /**
   * Sum of the `sumAxisKey` numeric tag across the bucket;
   * `undefined` when no `sumAxisKey` was requested.
   */
  summedCapacity?: bigint;
}

// =====================================================
// JSON encoding for the napi boundary
// =====================================================
//
// The Rust core's serde uses snake_case kind values + snake_case
// field names. The TS surface above uses camelCase per ergonomics;
// these helpers translate.

/** @internal */
export function tagMatcherToJson(matcher: TagMatcher): string {
  switch (matcher.kind) {
    case 'exact':
      return JSON.stringify({ kind: 'exact', value: matcher.value });
    case 'prefix':
      return JSON.stringify({ kind: 'prefix', value: matcher.value });
    case 'axis':
      return JSON.stringify({ kind: 'axis', axis: matcher.axis });
    case 'axisKey':
      return JSON.stringify({
        kind: 'axis_key',
        axis: matcher.axis,
        key: matcher.key,
      });
    case 'regex':
      return JSON.stringify({ kind: 'regex', pattern: matcher.pattern });
    case 'versionRange':
      return JSON.stringify({
        kind: 'version_range',
        axis_key: matcher.axisKey,
        min: matcher.min ?? null,
        max: matcher.max ?? null,
      });
  }
}

/** @internal */
export function groupByToJson(groupBy: GroupBy): string {
  switch (groupBy.kind) {
    case 'class':
      return JSON.stringify({ kind: 'class' });
    case 'state':
      return JSON.stringify({ kind: 'state' });
    case 'region':
      return JSON.stringify({ kind: 'region' });
    case 'publisher':
      return JSON.stringify({ kind: 'publisher' });
    case 'tagStem':
      return JSON.stringify({ kind: 'tag_stem', prefix: groupBy.prefix });
    case 'tagValue':
      return JSON.stringify({
        kind: 'tag_value',
        axis: groupBy.axis,
        key: groupBy.key,
      });
  }
}

/** @internal */
export function aggregationToJson(agg: Aggregation): string {
  switch (agg.kind) {
    case 'count':
      return JSON.stringify({ kind: 'count' });
    case 'distinctPublishers':
      return JSON.stringify({ kind: 'distinct_publishers' });
    case 'distinctValues':
      return JSON.stringify({
        kind: 'distinct_values',
        axis: agg.axis,
        key: agg.key,
      });
    case 'sumNumericTag':
      return JSON.stringify({
        kind: 'sum_numeric_tag',
        axis_key: agg.axisKey,
      });
    case 'minNumericTag':
      return JSON.stringify({
        kind: 'min_numeric_tag',
        axis_key: agg.axisKey,
      });
    case 'maxNumericTag':
      return JSON.stringify({
        kind: 'max_numeric_tag',
        axis_key: agg.axisKey,
      });
  }
}

/** @internal */
export function capacityQueryToJson(query: CapacityQuery): string {
  return JSON.stringify({
    matcher: query.matcher
      ? JSON.parse(tagMatcherToJson(query.matcher))
      : null,
    group_by: JSON.parse(groupByToJson(query.groupBy)),
    max_rtt_ms: query.maxRttMs ?? null,
    sum_axis_key: query.sumAxisKey ?? null,
    limit: query.limit,
  });
}
