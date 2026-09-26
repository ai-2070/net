/**
 * `@net-mesh/browser/local` — a mesh in one page, for developing a game
 * without an anchor.
 *
 * ```ts
 * import { createLocalMesh } from '@net-mesh/browser/local';
 *
 * const mesh = createLocalMesh();
 * const hostNode = mesh.node();
 * const playerNode = mesh.node();
 * const host = hostStore({ definition, transport: hostNode, … });
 * const player = joinStore({ definition, transport: playerNode, host: hostNode.nodeIdHex(), … });
 * ```
 *
 * A local node satisfies the same structural type `connect()`'s node
 * does for the store, so `hostStore`, `joinStore` and `hostPlayer` are
 * the real ones and every frame is really encoded, parsed, chunked,
 * assembled and dispatched. What is NOT real is the network: delivery
 * is a function call in this page, and the peer on each frame is
 * assigned by this module rather than proved by a handshake.
 *
 * So a game that works here has working game logic and working store
 * wiring. It is not evidence that two browsers can reach each other —
 * that takes an anchor and `connect()`.
 *
 * Nothing here loads the wasm node: importing this subpath costs no
 * download.
 */

import type { StoreTransport, TransportFrame, TransportStream } from './store/host.js';

/** A node on a local mesh. Pass it wherever the store takes a `transport`. */
export interface LocalNode extends StoreTransport {
  /** This node's id: 16 lowercase hex, as `connect()`'s node reports it. */
  nodeIdHex(): string;
  /** Take this node off the mesh: frames to it fail, and it hears nothing. */
  close(): void;
}

/** One page's worth of mesh. Meshes are independent of each other. */
export interface LocalMesh {
  /**
   * Add a node. `id` is 16 hex digits; omitted, one is chosen at
   * random. Refuses an id already on this mesh.
   */
  node(id?: string): LocalNode;
  /** The ids of the nodes currently on this mesh. */
  nodes(): readonly string[];
  /** Take every node off the mesh. */
  close(): void;
}

function randomId(): string {
  const bytes = new Uint8Array(8);
  crypto.getRandomValues(bytes);
  let hex = '';
  for (const byte of bytes) hex += byte.toString(16).padStart(2, '0');
  return hex;
}

function canonicalId(id: string): string {
  const raw = id.startsWith('0x') ? id.slice(2) : id;
  if (!/^[0-9a-fA-F]{1,16}$/.test(raw)) {
    throw new TypeError(`a local node id is up to 16 hex digits, got ${JSON.stringify(id)}`);
  }
  return raw.toLowerCase().padStart(16, '0');
}

/** Create an empty local mesh. */
export function createLocalMesh(): LocalMesh {
  const listeners = new Map<string, Set<(event: TransportFrame) => void>>();

  function deliver(to: string, from: string, payload: Uint8Array, streamId: string | undefined): void {
    const targets = listeners.get(to);
    if (targets === undefined) throw new Error(`no local node ${to} on this mesh`);
    for (const handler of [...targets]) {
      // A copy per receiver, as a network would hand each its own
      // bytes: a receiver that mutates its payload must not change
      // what another receiver — or the sender — holds.
      handler({ type: 'stream_data', streamId, peerNode: from, payload: payload.slice() });
    }
  }

  function node(requested?: string): LocalNode {
    const self = requested === undefined ? randomId() : canonicalId(requested);
    if (listeners.has(self)) throw new Error(`local node ${self} is already on this mesh`);
    const own = new Set<(event: TransportFrame) => void>();
    listeners.set(self, own);
    let closed = false;

    return {
      nodeIdHex: () => self,
      openStream: options => {
        if (closed) throw new Error(`local node ${self} is closed`);
        const peer = options.peer === undefined ? null : canonicalId(options.peer);
        // The label stands in for the id a real node derives from it:
        // both ends name the same stream without exchanging one.
        const streamId = options.label;
        let open = true;
        const stream: TransportStream = {
          send: payload =>
            // Asynchronous on purpose: a real `send` is a promise, and a
            // game that only worked with synchronous delivery would be
            // hiding an ordering assumption.
            Promise.resolve().then(() => {
              if (!open || closed) throw new Error('this local stream is closed');
              if (peer === null) throw new Error('a local stream needs a peer');
              deliver(peer, self, payload, streamId);
            }),
          close: () => {
            open = false;
          },
        };
        return stream;
      },
      onEvent: handler => {
        own.add(handler);
        return () => {
          own.delete(handler);
        };
      },
      close: () => {
        if (closed) return;
        closed = true;
        own.clear();
        if (listeners.get(self) === own) listeners.delete(self);
      },
    };
  }

  return {
    node,
    nodes: () => [...listeners.keys()],
    close: () => {
      for (const handlers of listeners.values()) handlers.clear();
      listeners.clear();
    },
  };
}
