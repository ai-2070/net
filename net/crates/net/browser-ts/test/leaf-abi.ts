/**
 * The Rust stream ABI, as the test doubles must speak it.
 *
 * Stage 5's fakes handed `LeafStream` a `Uint8Array` because the
 * hand-written declaration said so. Rust hands it the node's
 * `stream_data` event JSON. The two agreed with each other, so the
 * package's whole unit suite was green against a boundary that did
 * not exist, and `onMessage` gave pages a string.
 *
 * So the doubles no longer invent a shape. {@link streamDataEvent}
 * formats exactly what `net_leaf::node::LeafEvent::to_json` emits,
 * and {@link STREAM_DATA_VECTORS} is the Rust-generated fixture that
 * proves it: `abi.test.ts` asserts this formatter reproduces every
 * committed vector, and `leaf/tests/ts_abi_fixture.rs` asserts the
 * committed vectors are what production Rust emits. Change the Rust
 * shape and the Rust test goes red; change it and regenerate without
 * teaching this formatter, and the TypeScript test goes red.
 */

import fixture from './fixtures/leaf-abi.json';

import { toBase64 } from '../src/events.js';

/** One vector from `test/fixtures/leaf-abi.json`. */
export interface StreamDataVector {
  /** Why this vector is in the set. */
  readonly note: string;
  /** The wire stream id, exact decimal. */
  readonly streamId: string;
  /** The sequence, exact decimal. */
  readonly seq: string;
  /** The payload, hex. */
  readonly payloadHex: string;
  /** The exact string Rust emits for it. */
  readonly json: string;
}

/** Every pinned `stream_data` event, generated from Rust. */
export const STREAM_DATA_VECTORS: readonly StreamDataVector[] = fixture.streamData;

/** A vector's payload as the bytes a consumer must end up with. */
export function vectorPayload(vector: StreamDataVector): Uint8Array {
  const out = new Uint8Array(vector.payloadHex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(vector.payloadHex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

/**
 * The `stream_data` event JSON Rust emits for one inbound payload.
 *
 * `streamId` and `seq` are decimal strings because they are `u64` on
 * the wire and `JSON.parse` would round them.
 */
export function streamDataEvent(streamId: string, seq: string, payload: Uint8Array): string {
  return `{"type":"stream_data","stream_id":"${streamId}","seq":"${seq}","payload":"${toBase64(payload)}"}`;
}

/**
 * The exact `LeafError` Display text the leaf refuses an outbound
 * operation on a **closed** node with — `Inner::admit` in
 * `leaf/src/wasm.rs`, which is what makes `openStream` after
 * `close()` a typed `SessionError` instead of a dead handle.
 *
 * Here for the reason {@link streamDataEvent} is here: a double that
 * invents its own refusal text agrees with whatever this package
 * believes, which is the thing under test. The string is extracted
 * from the Rust source and compared with this constant by
 * `tests/abi_real_package.mjs`, so drifting either side goes red.
 */
export const NODE_CLOSED_REFUSAL =
  "session: the node is closed: it no longer holds this origin's identity";
