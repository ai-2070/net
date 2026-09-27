/**
 * Everything a node receives, as searchable text — snapshots included.
 *
 * A snapshot travels as base64 chunks (`snap.d`), so a substring search
 * over raw frames cannot see into it: a secret in a join snapshot would
 * pass a naive "not on the wire" check. This reassembles each
 * snapshot's chunks in order and appends the decoded document, so a
 * leak anywhere in the stream is found.
 */

import type { LocalNode } from '../../src/local.js';

export interface Sniffed {
  /** Every frame, plus every decoded snapshot, joined. */
  text(): string;
}

export function sniff(node: LocalNode): Sniffed {
  const frames: string[] = [];
  const decoder = new TextDecoder();
  node.onEvent(event => {
    if (event.payload !== undefined) frames.push(decoder.decode(event.payload));
  });
  return {
    text: () => {
      const chunks = new Map<string, { i: number; d: string }[]>();
      for (const frame of frames) {
        let message: { k?: string; h?: string; g?: string; i?: number; d?: string };
        try {
          message = JSON.parse(frame) as typeof message;
        } catch {
          continue;
        }
        if (message.k !== 'snap' || typeof message.d !== 'string') continue;
        const key = `${String(message.h)}/${String(message.g)}`;
        const list = chunks.get(key) ?? [];
        list.push({ i: Number(message.i), d: message.d });
        chunks.set(key, list);
      }
      const snapshots = [...chunks.values()].map(list => {
        const bytes = list
          .sort((a, b) => a.i - b.i)
          .flatMap(chunk => [...Uint8Array.from(atob(chunk.d), c => c.charCodeAt(0))]);
        return decoder.decode(new Uint8Array(bytes));
      });
      return [...frames, ...snapshots].join('\n');
    },
  };
}
