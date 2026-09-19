/**
 * A development transport: one page, no mesh.
 *
 * It satisfies the same structural type `BrowserNode` and `MeshSession`
 * satisfy, so `hostStore` and `joinStore` are the real ones and every
 * frame is really encoded, parsed, chunked, assembled and dispatched.
 * What is NOT real is the network: delivery is a function call, and the
 * authenticated peer is assigned by this file rather than proved by a
 * handshake.
 *
 * That distinction is the whole reason this module is named and
 * documented rather than inlined. `?mode=local` is for developing the
 * game without an anchor. It is not evidence about the mesh, and a
 * screenshot of it is not evidence that two browsers can play.
 */

const handlers = new Map();

function deliver(to, from, bytes, streamId) {
  for (const handler of handlers.get(to) ?? []) {
    handler({ type: 'stream_data', streamId, peerNode: from, payload: bytes });
  }
}

/** A node on the local bus, addressed by a 16-hex id you choose. */
export function localNode(self) {
  return {
    nodeIdHex: () => self,
    openStream: options => ({
      send: bytes => {
        // Asynchronous on purpose: a real `send` is a promise, and a
        // demo that only worked with synchronous delivery would be
        // hiding an ordering assumption.
        return Promise.resolve().then(() => {
          deliver(options.peer, self, bytes, options.streamId);
        });
      },
      close: () => {},
    }),
    onEvent: handler => {
      const list = handlers.get(self) ?? [];
      list.push(handler);
      handlers.set(self, list);
      return () => {
        handlers.set(self, (handlers.get(self) ?? []).filter(entry => entry !== handler));
      };
    },
  };
}

/** Forget every local node. Only useful between demo restarts. */
export function resetLocalBus() {
  handlers.clear();
}
