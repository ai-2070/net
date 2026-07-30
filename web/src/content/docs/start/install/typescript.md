## Install it — TypeScript / Node

```sh
npm install @net-mesh/core     # the native addon
npm install @net-mesh/sdk      # the ergonomic SDK, if you want typed channels
```

Both publish at **0.33**. `@net-mesh/core` is a native addon built with `napi-rs`;
prebuilt binaries ship for Windows, macOS and Linux on x86-64 and aarch64,
including musl. **Node 20 or newer.**

The core package also exposes subpath entries, so RPC and query code stays
tree-shakeable:

```ts
import { call } from "@net-mesh/core/mesh_rpc";
import { query } from "@net-mesh/core/meshdb";
```

### What is peculiar about TypeScript here

**The exported bus class is `Net`, not `EventBus`.** `EventBus` is the internal
Rust type the addon is built from; it is not a JS export, and reaching for it is
the first thing that goes wrong.

**Several surfaces live only on `@net-mesh/core`, never on `@net-mesh/sdk`** —
payments is the clearest case. If an import from the SDK fails, try the core
package before concluding the feature does not exist.

**Counters are `bigint`.** `stats.eventsIngested` is not a `number`; comparing it
to one with `===` is always false, and casting a value above 2^53 truncates
silently.

**Errors throw, they do not return null.** `emit` and `emitRaw` throw on failure;
the fire-and-forget variants return a boolean instead.

### Verify it worked

```ts
import { NetNode } from '@net-mesh/sdk';

interface Hello {
  msg: string;
}

async function main(): Promise<void> {
  const node = await NetNode.create({ shards: 1 });
  const ch = node.channel<Hello>('hello/world');

  const accepted = ch.publish({ msg: 'hello, mesh' });
  if (!accepted) throw new Error('the bus did not accept the event');

  // Counts at the PRODUCER boundary: accepted, not received or stored.
  const stats = node.stats();
  console.log(`accepted: ingested=${stats.eventsIngested}`);

  await node.shutdown();
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
```

Expect one line, `accepted: ingested=1`, and a clean exit. This is the example CI
executes on every commit, so if it does not behave this way for you the difference
is your environment, not the docs.

Next: [the TypeScript SDK spine](/docs/sdk/typescript/quickstart).
