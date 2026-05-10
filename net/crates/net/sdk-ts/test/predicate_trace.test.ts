// Cross-binding wire-format compat for the trace evaluator. Drives
// the same JSON fixture the Rust integration test consumes
// (`tests/cross_lang_capability/predicate_trace.json`) so all
// bindings agree byte-for-byte on the trace tree.

import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

import type {
  ClauseTrace,
  Predicate,
  PredicateWire,
} from '../src/capability-enhancements';
import {
  evaluatePredicateWithTrace,
  p,
  predicateFromWire,
  predicateToWire,
  tagKey,
} from '../src/capability-enhancements';

const TRACE_FIXTURE = resolve(
  __dirname,
  '../../tests/cross_lang_capability/predicate_trace.json',
);

interface TraceCase {
  name: string;
  summary: string;
  wire: PredicateWire;
  tags: string[];
  metadata: Record<string, string>;
  expected_result: boolean;
  expected_trace: ClauseTrace;
}

interface TraceFixture {
  description: string;
  abi_version_expected: number;
  cases: TraceCase[];
}

function loadFixture(): TraceFixture {
  if (!existsSync(TRACE_FIXTURE)) {
    throw new Error(
      `trace fixture missing at ${TRACE_FIXTURE}; cross-binding test cannot run`,
    );
  }
  return JSON.parse(readFileSync(TRACE_FIXTURE, 'utf-8')) as TraceFixture;
}

describe('predicate trace evaluator (cross-binding fixture)', () => {
  const fx = loadFixture();

  for (const c of fx.cases) {
    it(`matches ${c.name}`, () => {
      const pred = predicateFromWire(c.wire);
      const { result, trace } = evaluatePredicateWithTrace(
        pred,
        c.tags,
        c.metadata,
      );
      expect(result).toBe(c.expected_result);
      expect(trace).toEqual(c.expected_trace);
    });
  }
});

describe('trace evaluator local properties', () => {
  it('and short-circuits on the cheapest false leaf', () => {
    const pred: Predicate = p.and(
      p.semverCompatible(tagKey('software', 'runtime.python'), '3.11.0'),
      p.metadataEquals('intent', 'no-match'),
    );
    const { result, trace } = evaluatePredicateWithTrace(
      pred,
      ['software.runtime.python=3.11.5'],
      {},
    );
    expect(result).toBe(false);
    // Cost-ordered: metadata_equals (cost 11) runs first, fails, semver (60) skipped.
    expect(trace.children).toHaveLength(1);
    expect(trace.children[0].label).toMatch(/^MetadataEquals/);
  });

  it('not flips inner result and keeps it as the single child', () => {
    const { result, trace } = evaluatePredicateWithTrace(
      p.not(p.exists(tagKey('hardware', 'gpu'))),
      [],
      {},
    );
    expect(result).toBe(true);
    expect(trace.label).toBe('Not');
    expect(trace.children).toHaveLength(1);
    expect(trace.children[0].label).toBe('Exists(hardware.gpu)');
    expect(trace.children[0].result).toBe(false);
  });

  it('label format matches the substrate for every leaf variant', () => {
    const cases: { p: Predicate; label: string }[] = [
      { p: p.exists(tagKey('hardware', 'gpu')), label: 'Exists(hardware.gpu)' },
      {
        p: p.equals(tagKey('hardware', 'gpu.vendor'), 'nvidia'),
        label: 'Equals(hardware.gpu.vendor=nvidia)',
      },
      {
        p: p.numericAtLeast(tagKey('hardware', 'memory_mb'), 65536),
        label: 'NumericAtLeast(hardware.memory_mb >= 65536)',
      },
      {
        p: p.stringPrefix(tagKey('software', 'os'), 'linux'),
        label: 'StringPrefix(software.os starts with "linux")',
      },
      {
        p: p.metadataExists('intent'),
        label: 'MetadataExists(intent)',
      },
    ];
    for (const c of cases) {
      const { trace } = evaluatePredicateWithTrace(c.p, [], {});
      expect(trace.label).toBe(c.label);
    }
  });

  it('roundtrips a predicate built via predicateToWire / predicateFromWire', () => {
    const pred = p.and(
      p.exists(tagKey('hardware', 'gpu')),
      p.metadataEquals('intent', 'ml-training'),
    );
    const wire = predicateToWire(pred);
    const decoded = predicateFromWire(wire);
    const { result } = evaluatePredicateWithTrace(
      decoded,
      ['hardware.gpu'],
      { intent: 'ml-training' },
    );
    expect(result).toBe(true);
  });
});
