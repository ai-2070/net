/**
 * A simulated network for netcode tests: one-way latency per direction,
 * deterministic loss of lossy frames, delivery on timers (drive it with
 * vitest fake timers). Implements the structural `NetcodeTransport`.
 */

import type { NetcodeTransport } from '../../src/netcode/wire.js';

type Handler = Parameters<NetcodeTransport['onEvent']>[0];

export interface SimOptions {
  /** One-way latency, ms. */
  readonly latencyMs: number;
  /** Fraction of LOSSY frames dropped, 0..1. Reliable frames are never dropped. */
  readonly lossRate?: number;
  /** Extra per-frame delay spread, ms (uniform 0..jitterMs) — reorders. */
  readonly jitterMs?: number;
  readonly seed?: number;
  /**
   * How events spell the sender: `'decimal'` (the default — what the
   * browser node reports) or `'hex'` (what `meshStoreTransport` reports).
   */
  readonly peerSpelling?: 'decimal' | 'hex';
}

export interface SimStats {
  sent: number;
  dropped: number;
  delivered: number;
}

export function simNetwork(options: SimOptions) {
  let seed = options.seed ?? 42;
  // mulberry32: deterministic across runs.
  const random = () => {
    seed |= 0;
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  const handlers = new Map<string, Set<Handler>>();
  // Session incarnation per node: a stream is fenced to the incarnation
  // it was opened under, like the leaf's stale-handle refusal.
  const incarnation = new Map<string, number>();
  const stats: SimStats = { sent: 0, dropped: 0, delivered: 0 };

  function node(id: string): NetcodeTransport {
    handlers.set(id, handlers.get(id) ?? new Set());
    return {
      nodeIdHex: () => id,
      openStream: ({ peer, label, lossy }) => {
        let open = true;
        const openedUnder = incarnation.get(id) ?? 0;
        return {
          send: (payload: Uint8Array) => {
            if ((incarnation.get(id) ?? 0) !== openedUnder) {
              throw new Error('session: stale stream handle; reopen the stream');
            }
            if (!open || peer === undefined) return;
            stats.sent += 1;
            if (lossy && random() < (options.lossRate ?? 0)) {
              stats.dropped += 1;
              return;
            }
            const delay = options.latencyMs + random() * (options.jitterMs ?? 0);
            const copy = payload.slice();
            setTimeout(() => {
              stats.delivered += 1;
              for (const handler of [...(handlers.get(peer) ?? [])]) {
                const peerNode = options.peerSpelling === 'hex' ? id : BigInt(`0x${id}`).toString(10);
                handler({ type: 'stream_data', streamId: label, peerNode, payload: copy });
              }
            }, delay);
          },
          close: () => {
            open = false;
          },
        };
      },
      onEvent: handler => {
        handlers.get(id)!.add(handler);
        return () => {
          handlers.get(id)!.delete(handler);
        };
      },
    };
  }
  /** Replace `id`'s session: every stream it opened before now fails. */
  const replaceSession = (id: string) => {
    incarnation.set(id, (incarnation.get(id) ?? 0) + 1);
  };
  return { node, stats, replaceSession };
}
