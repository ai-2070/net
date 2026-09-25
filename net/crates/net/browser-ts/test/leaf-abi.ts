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
import orgFixture from './fixtures/org-abi.json';

import { toBase64 } from '../src/events.js';
import type { LeafWasmOrgByteItem } from '../src/wasm.js';

/** One vector from `test/fixtures/leaf-abi.json`. */
export interface StreamDataVector {
  /** Why this vector is in the set. */
  readonly note: string;
  /** The peer the frame came from, exact decimal. */
  readonly peerNode: string;
  /** The session incarnation that carried it, exact decimal. */
  readonly incarnation: string;
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

/** Hex, as the bytes a consumer must end up with. */
function hexBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

/** A vector's payload as the bytes a consumer must end up with. */
export function vectorPayload(vector: StreamDataVector): Uint8Array {
  return hexBytes(vector.payloadHex);
}

/** The identifying fields of one `stream_data` event. */
export interface StreamDataFields {
  /** The sending peer, exact decimal. */
  readonly peerNode: string;
  /** The session incarnation, exact decimal. */
  readonly incarnation: string;
  /** The stream's id, exact decimal. */
  readonly streamId: string;
  /** The sequence, exact decimal. */
  readonly seq: string;
  readonly payload: Uint8Array;
}

/**
 * The `stream_data` event JSON Rust emits for one inbound payload.
 *
 * Every id is a decimal string because each is a `u64` on the wire
 * and `JSON.parse` would round it. The key ORDER is part of the
 * pin: `LeafEvent::to_json` writes this arm with a hand-written
 * `format!`, and `abi.test.ts` asserts byte identity against the
 * Rust-generated vectors, so this is the emitted text rather than an
 * equivalent object.
 */
export function streamDataEvent(fields: StreamDataFields): string {
  return (
    `{"type":"stream_data","peer_node":"${fields.peerNode}",` +
    `"incarnation":"${fields.incarnation}","stream_id":"${fields.streamId}",` +
    `"seq":"${fields.seq}","payload":"${toBase64(fields.payload)}"}`
  );
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

// ── The org byte-stream terminal items ──────────────────────────────────
//
// The second half of the same pin: what `OrgByteStreamHandle.next()`
// resolves at the end of a stream. `fixtures/org-abi.json` is the
// Rust-generated fixture — `frameHex` is `RpcResponsePayload::encode_into`'s
// exact output for a completion frame carrying `bodyHex`, and `itemKeys` are
// the property names `org_stream_end_value`/`org_stream_end` set on the item
// object in `leaf/src/wasm.rs`, extracted from that source. A double that
// invents the done shape — or drops the terminal's final body — disagrees
// with the fixture instead of with whatever this package believes.

/** One vector from `test/fixtures/org-abi.json`. */
export interface OrgEndItemVector {
  readonly note: string;
  /** The completion frame's exact wire bytes, hex. */
  readonly frameHex: string;
  /** The terminal's final body — what the seam must surface, hex. */
  readonly bodyHex: string;
  /** The item object's property names, in the leaf's construction order. */
  readonly itemKeys: readonly string[];
}

/** Every pinned org stream end item, generated from Rust. */
export const ORG_END_ITEMS: readonly OrgEndItemVector[] = orgFixture.endItems;

/**
 * The `{ done: true }` / `{ done: true, value }` item the leaf hands
 * `OrgByteStreamHandle.next()` for one pinned completion frame — the
 * fixture's shape assembled, never a double's own invention.
 */
export function orgEndItem(vector: OrgEndItemVector): LeafWasmOrgByteItem {
  const body = hexBytes(vector.bodyHex);
  return body.length === 0 ? { done: true } : { done: true, value: body };
}

/**
 * The terminal body decoded back out of a vector's `frameHex` — the
 * completion frame's `status u16le ‖ headers ‖ body_len u32le ‖ body`
 * at `RpcResponsePayload`, so a pin can prove the frame carries the
 * bytes the seam surfaces. Header-free `Ok` frames only.
 */
export function orgCompletionBody(vector: OrgEndItemVector): Uint8Array {
  const frame = hexBytes(vector.frameHex);
  const status = frame[0]! | (frame[1]! << 8);
  const headerCount = frame[2]!;
  const length = (frame[3]! | (frame[4]! << 8) | (frame[5]! << 16) | (frame[6]! << 24)) >>> 0;
  if (status !== 0 || headerCount !== 0 || frame.length - 7 !== length) {
    throw new Error(`the fixture frame is not a header-free Ok completion: ${vector.note}`);
  }
  return frame.slice(7);
}
