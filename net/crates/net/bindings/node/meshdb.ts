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
//   import "@ai2070/net/meshdb";  // augments MeshQueryStream
//   import { MeshQuery, MeshQueryRunner } from "@ai2070/net";
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

const native = require("./index") as { MeshQueryStream?: { prototype: object } };

if (
  native.MeshQueryStream &&
  typeof native.MeshQueryStream === "function" &&
  !(Symbol.asyncIterator in native.MeshQueryStream.prototype)
) {
  Object.defineProperty(native.MeshQueryStream.prototype, Symbol.asyncIterator, {
    value(this: { next(): Promise<unknown | null> }) {
      const stream = this;
      return {
        async next(): Promise<{ value: unknown; done: boolean }> {
          const row = await stream.next();
          if (row === null || row === undefined) {
            return { value: undefined, done: true };
          }
          return { value: row, done: false };
        },
      };
    },
    enumerable: false,
    configurable: true,
    writable: true,
  });
}

// Re-export the shimmed types so consumers can rely on a single
// import path. The named re-exports come straight from the native
// binding; the shim above wires the prototype before they're used.
export const { MeshQuery, MeshQueryRunner, MeshQueryStream, InMemoryChainReader } =
  native as Record<string, unknown>;
