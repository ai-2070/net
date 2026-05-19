/**
 * Redis Streams consumer-side dedup helper — re-export of the NAPI
 * class from `@net-mesh/core`.
 *
 * The Redis adapter writes a stable `dedup_id` field on every XADD
 * entry — see the producer-side contract in `sdk-ts/README.md`.
 * The dedup helper filters producer-retry-induced duplicates at
 * consume time. Default capacity is 4096; production callers should
 * size to roughly `events_per_sec * dedup_window_seconds`.
 *
 * This shim re-exports the underlying NAPI class so users can
 * `import { RedisStreamDedup } from '@net-mesh/sdk'` directly
 * instead of reaching into `@net-mesh/core`.
 *
 * @example
 * ```typescript
 * import { RedisStreamDedup } from '@net-mesh/sdk';
 * import { createClient } from 'redis';
 *
 * // Sizing: ~10k events/sec * 1 min dedup window → ~600,000.
 * const dedup = new RedisStreamDedup(600_000);
 *
 * const r = createClient();
 * await r.connect();
 *
 * let cursor = '0';
 * while (true) {
 *   // After the first page, use the exclusive form `(<id>` so we
 *   // don't re-read the entry the cursor points at.
 *   const start = cursor === '0' ? cursor : `(${cursor}`;
 *   const entries = await r.xRange('net:shard:0', start, '+', { COUNT: 100 });
 *   if (entries.length === 0) break;
 *   for (const entry of entries) {
 *     // Advance the cursor BEFORE the duplicate check. Skipping
 *     // the `cursor = entry.id` assignment on duplicate entries
 *     // (via `continue`) makes the loop spin forever re-reading
 *     // the same range when a window is full of duplicates.
 *     cursor = entry.id;
 *     const dedupId = entry.message.dedup_id;
 *     if (dedupId && dedup.isDuplicate(dedupId)) continue;
 *     await process(entry);
 *   }
 * }
 * ```
 *
 * @packageDocumentation
 */

export { RedisStreamDedup } from '@net-mesh/core';
