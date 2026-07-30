## The helper in TypeScript

```typescript
import { RedisStreamDedup } from '@net-mesh/sdk';

const dedup = new RedisStreamDedup(600_000);

for (const entry of entries) {
  const dedupId = entry.fields.dedup_id;
  if (dedupId && dedup.isDuplicate(dedupId)) continue;
  process(entry);
}
```

`new RedisStreamDedup(capacity?)` — omitted defaults to 4096, `0` is clamped
to 1. Then `isDuplicate(dedupId): boolean`, and the getters `len`, `capacity`,
`isEmpty`, plus `clear()`.
