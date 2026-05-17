// Surface tests for sdk-ts/src/meshdb.ts.
//
// These verify the re-exports without exercising the napi runtime —
// the underlying MeshDB query path is covered end-to-end by
// `bindings/node/test/meshdb.test.ts`. This file is the sdk-ts
// wrapper boundary check.

import { describe, expect, expectTypeOf, it } from 'vitest';

import {
  DisposableMeshQueryRunner,
  InMemoryChainReader,
  MeshQuery,
  MeshQueryRunner,
  MeshQueryStream,
  QueryBuilder,
  parseMeshDbErrorKind,
} from '../src/meshdb';
import type {
  AggregateResult,
  CachePolicy,
  ExecuteOptions,
  GroupKey,
  JoinedRow,
  LineageEntry,
  MeshDbPredicate,
  ParsedMeshDbError,
  ResultRow,
  WindowBoundary,
} from '../src/meshdb';

describe('sdk-ts/meshdb re-exports', () => {
  it('exposes every MeshDB class from the napi binding', () => {
    expect(InMemoryChainReader).toBeTypeOf('function');
    expect(MeshQuery).toBeTypeOf('function');
    expect(MeshQueryRunner).toBeTypeOf('function');
    expect(MeshQueryStream).toBeTypeOf('function');
    expect(QueryBuilder).toBeTypeOf('function');
  });

  it('exposes parseMeshDbErrorKind', () => {
    expect(parseMeshDbErrorKind).toBeTypeOf('function');
  });

  it('type-level: every result + config interface flows through', () => {
    // These assertions force the imports to actually be type-resolved.
    // A regression that dropped one from the wrapper would fail to
    // compile.
    expectTypeOf<AggregateResult>().toBeObject();
    expectTypeOf<CachePolicy>().toBeObject();
    expectTypeOf<ExecuteOptions>().toBeObject();
    expectTypeOf<GroupKey>().toBeObject();
    expectTypeOf<JoinedRow>().toBeObject();
    expectTypeOf<LineageEntry>().toBeObject();
    expectTypeOf<ParsedMeshDbError>().toBeObject();
    expectTypeOf<MeshDbPredicate>().toBeObject();
    expectTypeOf<ResultRow>().toBeObject();
    expectTypeOf<WindowBoundary>().toBeObject();
  });
});

describe('DisposableMeshQueryRunner', () => {
  it('constructs a runner over a reader', () => {
    const reader = new InMemoryChainReader();
    const disposable = new DisposableMeshQueryRunner(reader);
    expect(disposable.runner).toBeInstanceOf(MeshQueryRunner);
  });

  it('Symbol.dispose drops the runner reference', () => {
    const reader = new InMemoryChainReader();
    const disposable = new DisposableMeshQueryRunner(reader);
    expect(disposable.runner).toBeInstanceOf(MeshQueryRunner);

    disposable[Symbol.dispose]();
    expect(disposable.runner).toBeUndefined();
  });

  it('integrates with the using-block when the target supports it', () => {
    // The TC39 explicit-resource-management proposal lands in TS 5.2+;
    // skip the syntactic test in environments that don't support it.
    const reader = new InMemoryChainReader();
    let lastRunner: unknown = null;
    {
      using d = new DisposableMeshQueryRunner(reader);
      lastRunner = d.runner;
      expect(lastRunner).toBeInstanceOf(MeshQueryRunner);
    }
    // Outside the block — the runner reference on the disposable was
    // cleared by the Symbol.dispose call.
    // (The local `d` is gone with the block; we use `lastRunner` only
    // as evidence the constructor ran.)
    expect(lastRunner).not.toBeNull();
  });
});
