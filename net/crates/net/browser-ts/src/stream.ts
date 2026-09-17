/**
 * Streams, as a page wants them: an async iterable of payloads with a
 * `send` that rejects with a typed error.
 */

import { AsyncQueue } from './async-queue.js';
import { fromWasmError, SessionError } from './errors.js';
import { parseEvent } from './events.js';
import type { LeafWasmStreamLike, StreamCallbackPayload, StreamReliability } from './wasm.js';

/** Options for {@link BrowserNode.openStream}. */
export interface OpenStreamOptions {
  /**
   * `reliable` retransmits and reorders by `seq`; `fireAndForget`
   * neither, which is the point of it — a dropped frame stays dropped
   * and the consumer sees the gap.
   */
  reliability: StreamReliability;
  /** A human label for logs. */
  label?: string;
  /**
   * Used verbatim when present, so a stream can match a publish
   * contract a native handler dispatches on.
   */
  streamId?: string;
  /**
   * Likewise verbatim: the `u16` channel hash the stream rides.
   * A number — see {@link LeafWasmStreamOptions.channelHash} for
   * why it is not a string.
   */
  channelHash?: number;
  /**
   * The node this stream addresses — **16 hex digits**, the spelling
   * {@link BrowserNode.nodeId} hands out and
   * {@link BrowserNode.connectPeer} takes. Absent, it is the anchor,
   * which is what every caller got before this option existed.
   *
   * This is the page-facing half of §9: `connectPeer` installs a
   * direct leaf ↔ leaf session and this is what puts application
   * bytes on it. The addressing is the leaf's, not a second path —
   * a stream to a peer whose session is still ROUTED rides the
   * anchor, and needs no second call to start riding the
   * DataChannel.
   *
   * **The handle does not survive the direct upgrade, though.** §9
   * step 4 installs a *replacement* session, and a stream handle is
   * fenced to the incarnation it was opened on — so `send` on a
   * stream opened while the pair was routed rejects with
   * {@link SessionError} ("stale stream handle … reopen the
   * stream") once the direct session lands. Reopening is one call
   * with the same `peer` and the same `streamId`; a page driving a
   * peer across the upgrade should reopen on that rejection rather
   * than assume continuity.
   *
   * A peer with no session at all rejects typed from `send`.
   *
   * Accepted by {@link MeshSession.openStream} as well. A follower's
   * stream is opened by the leader tab's node, and the request
   * carries this peer, so a follower addresses a direct
   * leaf-to-leaf session exactly as the leader tab does. It used to
   * be refused by name there, because the proxy request had no peer
   * field to carry.
   */
  peer?: string;
}

/**
 * One open stream.
 *
 * Inbound payloads are delivered to `for await` consumers and to
 * {@link LeafStream.onMessage} listeners. Payloads that arrive before
 * anyone consumes them are buffered in arrival order, so a page that
 * opens a stream and awaits it a tick later loses nothing.
 */
export class LeafStream implements AsyncIterable<Uint8Array> {
  private readonly queues = new Set<AsyncQueue<Uint8Array>>();
  private readonly listeners = new Set<(payload: Uint8Array) => void>();
  private readonly pending: Uint8Array[] = [];
  private closed = false;
  /**
   * This stream's wire id as a number, or `null` when the inner
   * object's `stream_id_hex()` was not hex.
   *
   * The event JSON spells the id in **decimal** while the handle
   * spells it in **hex**; comparing the two textually would drop
   * every payload, so the comparison is numeric. Rust filters the
   * event stream too — this is the exact check behind its substring
   * one, and the reason a mismatch is a dropped payload here rather
   * than someone else's bytes.
   */
  private readonly wireId: bigint | null;
  /**
   * The peer this stream addresses, as a number, or `null` when the
   * inner object does not spell one.
   *
   * **The other half of a stream's identity, and the reason the id
   * alone was not one.** A stream id is an application label scoped
   * to a session, so two streams to two peers under the same id is
   * the ordinary case — `openStream({ peer, streamId })` invites it
   * — and the callback is handed the NODE-WIDE event vector. Keyed
   * on the id alone, two wrappers both admitted whichever peer's
   * frame arrived, so a page echoing what it received amplified one
   * peer's payload onto the other's stream (R4-10). The key is
   * `(peer, id)`.
   *
   * Read in the same hex spelling as the id and compared against the
   * event's decimal `peer_node` the same way, so there is one
   * conversion idiom in this file rather than two — a mismatch in
   * either would fail the id comparison too, loudly.
   */
  private readonly peerId: bigint | null;

  /**
   * @internal — built by `BrowserNode.openStream` (a leader-local
   * stream, whose `send` is synchronous) or by
   * `MeshSession.openStream` (a proxied one, whose `send` is a
   * promise because another tab puts the packet on the wire). A page
   * writes `await stream.send(bytes)` either way.
   *
   * `onClosed` tells the owner it may forget this stream — the same
   * shape {@link AsyncQueue} uses for a departing consumer. A node
   * has to retain the streams it handed out to end them on its own
   * close, and `BrowserNode` passes this so a page that opens and
   * closes a stream per frame does not grow that set for the node's
   * lifetime.
   */
  constructor(
    private readonly inner: LeafWasmStreamLike,
    private readonly onClosed?: () => void,
  ) {
    this.wireId = u64FromHex(() => inner.stream_id_hex());
    this.peerId = u64FromHex(() => inner.peer_node_hex?.());
    inner.on_message((event) => this.receive(event));
  }

  /**
   * Whether this stream retransmits. Read from the leaf rather than
   * from the options it was asked for, so it is the stream's own
   * answer.
   */
  get reliability(): StreamReliability {
    return this.inner.is_reliable() ? 'reliable' : 'fireAndForget';
  }

  /** The stream's wire id, hex. */
  get streamId(): string {
    return this.inner.stream_id_hex();
  }

  /** Send one payload. Fragmentation at `MAX_PAYLOAD_SIZE` is the leaf's. */
  async send(payload: Uint8Array): Promise<void> {
    if (this.closed) throw new SessionError('the stream is closed');
    try {
      await this.inner.send(payload);
    } catch (error) {
      throw fromWasmError(error);
    }
  }

  /** Listen for payloads. Returns a cancel handle. */
  onMessage(listener: (payload: Uint8Array) => void): () => void {
    this.listeners.add(listener);
    const buffered = this.pending.splice(0, this.pending.length);
    for (const payload of buffered) listener(payload);
    return () => {
      this.listeners.delete(listener);
    };
  }

  [Symbol.asyncIterator](): AsyncIterableIterator<Uint8Array> {
    const queue: AsyncQueue<Uint8Array> = new AsyncQueue(() => this.queues.delete(queue));
    this.queues.add(queue);
    const buffered = this.pending.splice(0, this.pending.length);
    for (const payload of buffered) queue.push(payload);
    if (this.closed) queue.end();
    return queue;
  }

  /** Close the stream and end every consumer. */
  close(): void {
    if (this.closed) return;
    this.closed = true;
    for (const queue of [...this.queues]) queue.end();
    this.queues.clear();
    this.listeners.clear();
    // Before the boundary call, so an owner's bookkeeping is correct
    // even if a host-supplied wrapper's `close` throws.
    this.onClosed?.();
    this.inner.close();
  }

  /**
   * One callback from the wasm boundary.
   *
   * Rust emits the node's `stream_data` event JSON; this is where
   * it becomes the `Uint8Array` the public API promises. Bytes are
   * taken as an already-decoded payload, which is what a
   * host-supplied {@link LeafWasmStreamLike} may deliver.
   */
  private receive(event: StreamCallbackPayload): void {
    const payload = this.decode(event);
    if (payload === null || this.closed) return;
    if (this.queues.size === 0 && this.listeners.size === 0) {
      this.pending.push(payload);
      return;
    }
    for (const listener of this.listeners) {
      try {
        listener(payload);
      } catch (error) {
        // Same rule as the event hub: a listener's bug must not unwind
        // through the wasm callback frame that called us.
        console.error('[@net-mesh/browser] stream listener threw', error);
      }
    }
    for (const queue of this.queues) queue.push(payload);
  }

  private decode(event: StreamCallbackPayload): Uint8Array | null {
    if (event instanceof Uint8Array) return event;
    if (typeof event !== 'string') {
      console.error('[@net-mesh/browser] stream callback received', typeof event, 'not the leaf event JSON');
      return null;
    }
    const parsed = parseEvent(event);
    // The node's event stream carries more than this stream: Rust
    // pre-filters, but the id it filters on is a substring match, so
    // the exact one lives here.
    if (parsed.type !== 'stream_data' || !/^\d+$/.test(parsed.streamId)) return null;
    if (this.wireId !== null && BigInt(parsed.streamId) !== this.wireId) return null;
    // And the peer, because the id is only half of the identity. A
    // frame whose peer is unattributable is dropped by a stream that
    // knows its own: "some peer's bytes under my id" is precisely
    // what this stream must not deliver. A stream whose HANDLE does
    // not spell a peer keeps the id-only behaviour instead of
    // dropping everything — the same disposition an unreadable id
    // gets, one line above.
    if (this.peerId !== null) {
      if (!/^\d+$/.test(parsed.peerNode)) return null;
      if (BigInt(parsed.peerNode) !== this.peerId) return null;
    }
    return parsed.payload;
  }
}

/**
 * A `u64` the inner object spells in hex, as a number — or `null`
 * when it does not spell one.
 *
 * A host-supplied wrapper is allowed to omit either accessor, and a
 * value that cannot be read must not become a filter that drops
 * everything.
 */
function u64FromHex(read: () => string | undefined): bigint | null {
  let hex: string | undefined;
  try {
    hex = read();
  } catch {
    return null;
  }
  if (hex === undefined || !/^[0-9a-fA-F]{1,16}$/.test(hex)) return null;
  return BigInt(`0x${hex}`);
}
