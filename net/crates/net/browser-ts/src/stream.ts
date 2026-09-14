/**
 * Streams, as a page wants them: an async iterable of payloads with a
 * `send` that rejects with a typed error.
 */

import { AsyncQueue } from './async-queue.js';
import { fromWasmError, SessionError } from './errors.js';
import type { LeafWasmStreamLike, StreamReliability } from './wasm.js';

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
  /** Likewise verbatim: the channel hash the stream rides. */
  channelHash?: string;
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
   * @internal — built by `BrowserNode.openStream` (a leader-local
   * stream, whose `send` is synchronous) or by
   * `MeshSession.openStream` (a proxied one, whose `send` is a
   * promise because another tab puts the packet on the wire). A page
   * writes `await stream.send(bytes)` either way.
   */
  constructor(private readonly inner: LeafWasmStreamLike) {
    inner.on_message((payload) => this.receive(payload));
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
    this.inner.close();
  }

  private receive(payload: Uint8Array): void {
    if (this.closed) return;
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
}
