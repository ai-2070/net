// Cross-binding wire-format compat for the TS PredicateDebugReport
// aggregator. Drives the same JSON fixture the Rust integration
// test consumes.

import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

import type {
  ClauseStats,
  EvalContextWire,
  PredicateDebugReport,
  PredicateWire,
} from '../src/capability-enhancements';
import {
  p,
  predicateDebugReport,
  predicateFromWire,
  renderDebugReport,
  tagKey,
} from '../src/capability-enhancements';

const REPORT_FIXTURE = resolve(
  __dirname,
  '../../tests/cross_lang_capability/predicate_debug_report.json',
);

interface ReportCase {
  name: string;
  summary: string;
  wire: PredicateWire;
  contexts: EvalContextWire[];
  expected_total_candidates: number;
  expected_matched: number;
  expected_clause_stats: ClauseStats[];
}

interface ReportFixture {
  description: string;
  abi_version_expected: number;
  cases: ReportCase[];
}

function loadFixture(): ReportFixture {
  if (!existsSync(REPORT_FIXTURE)) {
    throw new Error(
      `report fixture missing at ${REPORT_FIXTURE}; cross-binding test cannot run`,
    );
  }
  return JSON.parse(readFileSync(REPORT_FIXTURE, 'utf-8')) as ReportFixture;
}

describe('predicateDebugReport (cross-binding fixture)', () => {
  const fx = loadFixture();
  for (const c of fx.cases) {
    it(`matches ${c.name}`, () => {
      const pred = predicateFromWire(c.wire);
      const got: PredicateDebugReport = predicateDebugReport(pred, c.contexts);
      expect(got.total_candidates).toBe(c.expected_total_candidates);
      expect(got.matched).toBe(c.expected_matched);
      expect(got.clause_stats).toEqual(c.expected_clause_stats);
    });
  }
});

describe('predicateDebugReport local properties', () => {
  it('clause_stats sorted alphabetically by label', () => {
    const pred = p.and(
      p.exists(tagKey('hardware', 'gpu')),
      p.metadataEquals('intent', 'ml-training'),
    );
    const report = predicateDebugReport(pred, [
      { tags: ['hardware.gpu'], metadata: { intent: 'ml-training' } },
      { tags: [], metadata: {} },
    ]);
    const labels = report.clause_stats.map((s) => s.label);
    const sorted = [...labels].sort();
    expect(labels).toEqual(sorted);
  });

  it('structurally-equal clauses merge by label', () => {
    // Two `Exists(hardware.gpu)` clauses inside an Or — should
    // collapse to a single ClauseStats entry summing both branches.
    const pred = p.or(
      p.exists(tagKey('hardware', 'gpu')),
      p.exists(tagKey('hardware', 'gpu')),
    );
    const report = predicateDebugReport(pred, [
      { tags: [], metadata: {} },
    ]);
    const existsEntry = report.clause_stats.find(
      (s) => s.label === 'Exists(hardware.gpu)',
    );
    expect(existsEntry).toBeDefined();
    // Both children evaluated when first returned false (Or with no
    // short-circuit on the cheap clause).
    expect(existsEntry!.evaluated).toBe(2);
    expect(existsEntry!.matched).toBe(0);
  });

  it('renderDebugReport produces a multi-line summary', () => {
    const pred = p.exists(tagKey('hardware', 'gpu'));
    const report = predicateDebugReport(pred, [
      { tags: ['hardware.gpu'], metadata: {} },
      { tags: [], metadata: {} },
    ]);
    const text = renderDebugReport(report);
    expect(text).toContain('Predicate evaluation report');
    expect(text).toContain('Total candidates: 2');
    expect(text).toContain('Matched:          1');
    expect(text).toContain('Exists(hardware.gpu)');
  });

  it('empty corpus yields zero everything', () => {
    const pred = p.exists(tagKey('hardware', 'gpu'));
    const report = predicateDebugReport(pred, []);
    expect(report.total_candidates).toBe(0);
    expect(report.matched).toBe(0);
    expect(report.clause_stats).toEqual([]);
  });
});
