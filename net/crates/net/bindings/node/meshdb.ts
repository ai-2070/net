// MeshDB AsyncIterable shim.
//
// Augments the napi-rs–generated `MeshQueryStream` class with
// `[Symbol.asyncIterator]` so callers can write `for await (const row
// of await runner.execute(query)) { ... }`. The locked Node SDK
// decision is `Promise<AsyncIterable<Row>>`; importing this module
// once at startup makes the shape land.
//
// Usage:
//
//   import "@net-mesh/core/meshdb";  // augments MeshQueryStream
//   import { MeshQuery, MeshQueryRunner } from "@net-mesh/core";
//
//   const runner = new MeshQueryRunner(reader);
//   const stream = await runner.execute(MeshQuery.latest(0xABn));
//   for await (const row of stream) {
//     console.log(row.seq, row.payload);
//   }
//
// The shim is idempotent — re-imports are no-ops. It detaches
// cleanly under tree-shaking too (no top-level side effects beyond
// the prototype attach, which only fires when `MeshQueryStream` is
// part of the loaded native binding).
//
// Phase-F note: this shim is a wire-shape ergonomics layer; it
// doesn't change semantics. The underlying `next()` / `toArray()`
// methods stay available for callers that prefer manual iteration
// or batch drain.

// The napi-generated MeshQueryStream is a constructor function with a
// prototype, but the original cast typed it as a plain object — the
// later `typeof === "function"` narrowing then collapsed it to `never`
// and reading `.prototype` errored under `-D warnings`. Cast as a
// Function with a prototype so both type-guards line up.
const native = require("./index") as {
  MeshQueryStream?: Function & { prototype: object };
};

if (
  native.MeshQueryStream &&
  typeof native.MeshQueryStream === "function" &&
  !(Symbol.asyncIterator in native.MeshQueryStream.prototype)
) {
  Object.defineProperty(native.MeshQueryStream.prototype, Symbol.asyncIterator, {
    value(this: {
      next(): Promise<unknown | null>;
      release?(): Promise<void>;
    }) {
      const stream = this;
      return {
        async next(): Promise<{ value: unknown; done: boolean }> {
          const row = await stream.next();
          if (row === null || row === undefined) {
            return { value: undefined, done: true };
          }
          return { value: row, done: false };
        },
        // `return(value)` is invoked when a `for await (...)` loop
        // `break`s, `return`s from the enclosing function, or an
        // exception unwinds out of the loop body. Without this,
        // the backing row Vec stays pinned on the AsyncMutex
        // until JS GC eventually drops the stream — for a 10k+
        // row result that's a sizeable memory pin.
        async return(value: unknown): Promise<{ value: unknown; done: boolean }> {
          if (typeof stream.release === "function") {
            await stream.release();
          }
          return { value, done: true };
        },
        // `throw(err)` is the iteration-protocol's error path.
        // Symmetric to `return()`: free the buffer, then
        // re-surface the error to the caller.
        async throw(err: unknown): Promise<{ value: unknown; done: boolean }> {
          if (typeof stream.release === "function") {
            await stream.release();
          }
          throw err;
        },
      };
    },
    enumerable: false,
    configurable: true,
    writable: true,
  });
}

// NOTE: this module's primary purpose is the AsyncIterable
// side-effect attached above. We deliberately do NOT re-export
// the typed classes from here — going through `native as
// Record<string, unknown>` would downgrade them to `unknown`,
// silently weakening callers that imported them from
// `@net-mesh/core/meshdb` versus `@net-mesh/core`. Use the typed
// re-exports from `@net-mesh/core` instead; this module only
// needs `import "@net-mesh/core/meshdb"` for the iterator shim.

/**
 * Result of {@link parseMeshDbErrorKind}: extracted structured
 * discriminator + the human-readable message stripped of the
 * `<<meshdb-kind:...>>` prefix.
 */
export interface ParsedMeshDbError {
  kind: string;
  message: string;
}

/**
 * Pull the structured error kind out of a MeshDB error message.
 *
 * The Rust binding embeds the kind discriminator (one of the
 * `MeshError` variant tags such as `planner_error`,
 * `executor_error`, `query_cancelled`, `historical_range_unavailable`,
 * `ambiguous_discovery`, etc.) at the start of the error message
 * as `<<meshdb-kind:KIND>>MSG`. This helper parses it back.
 *
 * Returns `null` for errors that don't carry a kind prefix
 * (SDK-side validation failures, factory rejections) — those
 * surface with the plain message intact.
 */
export function parseMeshDbErrorKind(err: unknown): ParsedMeshDbError | null {
  if (!(err instanceof Error)) return null;
  // Accept digits too (`protocol_v2_mismatch`-style future kinds).
  const m = err.message.match(/^<<meshdb-kind:([a-z0-9_]+)>>(.*)$/s);
  if (!m) return null;
  return { kind: m[1], message: m[2] };
}
