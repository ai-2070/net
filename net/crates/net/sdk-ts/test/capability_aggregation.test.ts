// JSON-encoder pins for the Phase 6c capability-aggregation surface.
//
// The Rust core's `serde_json::to_string(&TagMatcher::...)` etc.
// produces specific byte sequences pinned by
// `serde_shapes_match_cross_binding_wire_format` in
// `capability_aggregation.rs`. The TS encoder in
// `src/capability-aggregation.ts` must produce the same JSON for the
// napi boundary to deserialize correctly. This file pins the TS
// side; if either side moves, both move together.

import { describe, expect, it } from 'vitest';

import {
  aggregationToJson,
  capacityQueryToJson,
  groupByToJson,
  tagMatcherToJson,
} from '../src/capability-aggregation';

describe('tagMatcherToJson', () => {
  it('encodes Exact as { kind, value }', () => {
    expect(
      tagMatcherToJson({ kind: 'exact', value: 'software.python=3.11' }),
    ).toBe('{"kind":"exact","value":"software.python=3.11"}');
  });

  it('encodes Prefix as { kind, value }', () => {
    expect(tagMatcherToJson({ kind: 'prefix', value: 'hardware.gpu' })).toBe(
      '{"kind":"prefix","value":"hardware.gpu"}',
    );
  });

  it('encodes Axis as { kind, axis }', () => {
    expect(tagMatcherToJson({ kind: 'axis', axis: 'hardware' })).toBe(
      '{"kind":"axis","axis":"hardware"}',
    );
  });

  it('encodes AxisKey as { kind, axis, key }', () => {
    expect(
      tagMatcherToJson({ kind: 'axisKey', axis: 'hardware', key: 'gpu.count' }),
    ).toBe('{"kind":"axis_key","axis":"hardware","key":"gpu.count"}');
  });

  it('encodes Regex as { kind, pattern }', () => {
    expect(tagMatcherToJson({ kind: 'regex', pattern: '^a$' })).toBe(
      '{"kind":"regex","pattern":"^a$"}',
    );
  });

  it('encodes VersionRange with bounds nulled when omitted', () => {
    expect(
      tagMatcherToJson({
        kind: 'versionRange',
        axisKey: 'software.python',
        min: '3.10.0',
      }),
    ).toBe(
      '{"kind":"version_range","axis_key":"software.python","min":"3.10.0","max":null}',
    );
  });
});

describe('groupByToJson', () => {
  it('encodes Class', () => {
    expect(groupByToJson({ kind: 'class' })).toBe('{"kind":"class"}');
  });

  it('encodes State', () => {
    expect(groupByToJson({ kind: 'state' })).toBe('{"kind":"state"}');
  });

  it('encodes Region', () => {
    expect(groupByToJson({ kind: 'region' })).toBe('{"kind":"region"}');
  });

  it('encodes Publisher', () => {
    expect(groupByToJson({ kind: 'publisher' })).toBe('{"kind":"publisher"}');
  });

  it('encodes TagStem as { kind, prefix }', () => {
    expect(groupByToJson({ kind: 'tagStem', prefix: 'hardware.gpu' })).toBe(
      '{"kind":"tag_stem","prefix":"hardware.gpu"}',
    );
  });

  it('encodes TagValue as { kind, axis, key }', () => {
    expect(
      groupByToJson({ kind: 'tagValue', axis: 'software', key: 'python' }),
    ).toBe('{"kind":"tag_value","axis":"software","key":"python"}');
  });
});

describe('aggregationToJson', () => {
  it('encodes Count', () => {
    expect(aggregationToJson({ kind: 'count' })).toBe('{"kind":"count"}');
  });

  it('encodes DistinctPublishers', () => {
    expect(aggregationToJson({ kind: 'distinctPublishers' })).toBe(
      '{"kind":"distinct_publishers"}',
    );
  });

  it('encodes DistinctValues as { kind, axis, key }', () => {
    expect(
      aggregationToJson({
        kind: 'distinctValues',
        axis: 'software',
        key: 'python',
      }),
    ).toBe('{"kind":"distinct_values","axis":"software","key":"python"}');
  });

  it('encodes SumNumericTag as { kind, axis_key }', () => {
    expect(
      aggregationToJson({
        kind: 'sumNumericTag',
        axisKey: 'hardware.gpu.count',
      }),
    ).toBe('{"kind":"sum_numeric_tag","axis_key":"hardware.gpu.count"}');
  });

  it('encodes MinNumericTag / MaxNumericTag with the right discriminant', () => {
    expect(
      aggregationToJson({
        kind: 'minNumericTag',
        axisKey: 'hardware.gpu.count',
      }),
    ).toBe('{"kind":"min_numeric_tag","axis_key":"hardware.gpu.count"}');
    expect(
      aggregationToJson({
        kind: 'maxNumericTag',
        axisKey: 'hardware.gpu.count',
      }),
    ).toBe('{"kind":"max_numeric_tag","axis_key":"hardware.gpu.count"}');
  });
});

describe('capacityQueryToJson', () => {
  it('round-trips through Rust core wire shape', () => {
    const json = capacityQueryToJson({
      matcher: { kind: 'prefix', value: 'hardware.gpu' },
      groupBy: { kind: 'tagStem', prefix: 'hardware.gpu' },
      maxRttMs: 50,
      sumAxisKey: 'hardware.gpu.count',
      limit: 5,
    });
    expect(JSON.parse(json)).toEqual({
      matcher: { kind: 'prefix', value: 'hardware.gpu' },
      group_by: { kind: 'tag_stem', prefix: 'hardware.gpu' },
      max_rtt_ms: 50,
      sum_axis_key: 'hardware.gpu.count',
      limit: 5,
    });
  });

  it('nulls absent fields', () => {
    const json = capacityQueryToJson({
      groupBy: { kind: 'region' },
      limit: 0,
    });
    expect(JSON.parse(json)).toEqual({
      matcher: null,
      group_by: { kind: 'region' },
      max_rtt_ms: null,
      sum_axis_key: null,
      limit: 0,
    });
  });
});
