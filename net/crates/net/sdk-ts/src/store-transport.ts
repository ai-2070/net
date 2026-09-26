/**
 * `meshStoreTransport` — serve (or join) the browser package's networked
 * store from a native Node node.
 *
 * ```ts
 * import { MeshNode, meshStoreTransport } from '@net-mesh/sdk';
 * import { hostStore } from '@net-mesh/browser';
 *
 * const mesh = await MeshNode.create({ … });
 * const transport = meshStoreTransport(mesh, { listen: ['store/my-game.world'] });
 * const host = hostStore({ definition: world, transport, … });   // a dedicated host
 * ```
 *
 * It satisfies the store's `StoreTransport` structurally — this package
 * does not depend on `@net-mesh/browser`, and the store does not depend on
 * this one — over three native calls:
 *
 * - `openStream` → {@link MeshNode.openStream} on the id
 *   {@link streamIdFromLabel} derives from the store's label, the same id a
 *   browser page derives, so both ends name one stream;
 * - `send` → {@link MeshNode.sendWithRetry};
 * - `onEvent` → {@link MeshNode.onStreamData}, which delivers each event
 *   WITH the peer whose session authenticated it. That peer is the store's
 *   only source of identity: every `authorize` decision and every
 *   `context.peer` rests on it, so a transport over `recv` (which has no
 *   sender) could not be built.
 *
 * **Listen before anyone writes.** A native node receives a stream only
 * once it has subscribed to it, and a host must hear a player's `join`
 * before it opens anything. So a host names its store labels in `listen`
 * (`store/<definition id>` unless the store sets `streamId`); a joiner is
 * covered by opening its stream, which subscribes too.
 *
 * Sessions are the mesh's: a peer must already be connected
 * (`connect` / `accept`) before the store can reach it.
 */

import type { MeshNode, MeshStream, StreamDataSubscription } from './mesh';
import { streamIdFromLabel } from './identity';

/** The frame shape the store's `onEvent` consumes. */
export interface StoreTransportFrame {
  readonly type: 'stream_data';
  readonly streamId: string;
  /** The authenticated sender, 16 lowercase hex. */
  readonly peerNode: string;
  readonly payload: Uint8Array;
}

/** Options for {@link meshStoreTransport}. */
export interface MeshStoreTransportOptions {
  /**
   * Store stream labels to receive from the start — what a HOST needs,
   * since players write first. `store/<definition id>` for a store that
   * does not set its own `streamId`.
   */
  readonly listen?: readonly string[];
}

/** The transport, plus a way to release its subscriptions. */
export interface MeshStoreTransport {
  nodeIdHex(): string;
  openStream(options: {
    reliability: 'reliable' | 'fireAndForget';
    peer?: string;
    label?: string;
  }): { send(payload: Uint8Array): Promise<void>; close(): void };
  onEvent(handler: (event: StoreTransportFrame) => void): () => void;
  /** Stop receiving on every stream this transport subscribed to. */
  close(): void;
}

function hex(id: bigint): string {
  return id.toString(16).padStart(16, '0');
}

function peerId(peer: string): bigint {
  const raw = peer.startsWith('0x') ? peer.slice(2) : peer;
  if (!/^[0-9a-fA-F]{1,16}$/.test(raw)) throw new TypeError(`not a 16-hex peer id: ${JSON.stringify(peer)}`);
  return BigInt(`0x${raw}`);
}

/** A `StoreTransport` over a native {@link MeshNode}. */
export function meshStoreTransport(mesh: MeshNode, options: MeshStoreTransportOptions = {}): MeshStoreTransport {
  const handlers = new Set<(event: StoreTransportFrame) => void>();
  const subscriptions = new Map<bigint, StreamDataSubscription>();
  let closed = false;

  function listen(label: string): bigint {
    const id = streamIdFromLabel(label);
    if (!subscriptions.has(id) && !closed) {
      subscriptions.set(
        id,
        mesh.onStreamData(id, data => {
          const frame: StoreTransportFrame = {
            type: 'stream_data',
            streamId: data.streamId.toString(),
            peerNode: hex(data.peerNodeId),
            payload: new Uint8Array(data.payload.buffer, data.payload.byteOffset, data.payload.byteLength),
          };
          for (const handler of [...handlers]) handler(frame);
        }),
      );
    }
    return id;
  }

  for (const label of options.listen ?? []) listen(label);

  return {
    nodeIdHex: () => hex(mesh.nodeId()),
    openStream: ({ reliability, peer, label }) => {
      if (closed) throw new Error('this store transport is closed');
      if (peer === undefined || label === undefined) {
        throw new TypeError('a native store stream needs both a peer and a label');
      }
      const target = peerId(peer);
      // Opening a stream subscribes to it, so a joiner hears the host's
      // replies on the stream it writes.
      const streamId = listen(label);
      const stream: MeshStream = mesh.openStream(target, {
        streamId,
        reliability: reliability === 'reliable' ? 'reliable' : 'fire_and_forget',
      });
      let open = true;
      return {
        send: async payload => {
          if (!open) throw new Error('this store stream is closed');
          await mesh.sendWithRetry(stream, [Buffer.from(payload.buffer, payload.byteOffset, payload.byteLength)]);
        },
        close: () => {
          open = false;
        },
      };
    },
    onEvent: handler => {
      handlers.add(handler);
      return () => {
        handlers.delete(handler);
      };
    },
    close: () => {
      closed = true;
      for (const subscription of subscriptions.values()) subscription.close();
      subscriptions.clear();
      handlers.clear();
    },
  };
}
