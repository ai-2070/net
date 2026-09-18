# Reading the source

This skill is the model and the verified templates. For an exact signature, a
serialized field name, a state transition, or *why* observed behaviour differs
from what you expected, read the tree — it is ground truth.

## Get the tree

```bash
npx -y opensrc@latest path ai-2070/net
```

That resolves the repository into a local cache. A shallow clone is equivalent:

```bash
git clone --depth 1 https://github.com/ai-2070/net /tmp/net
```

## Where the browser surfaces live

| What | Path |
|---|---|
| Package entry (public exports) | `net/crates/net/browser-ts/src/index.ts` |
| `connect` / `BrowserNode` / `ConnectOptions` | `net/crates/net/browser-ts/src/node.ts` |
| `LeafStream`, `OpenStreamOptions` | `net/crates/net/browser-ts/src/stream.ts` |
| Event union + parser | `net/crates/net/browser-ts/src/events.ts` |
| Error taxonomy | `net/crates/net/browser-ts/src/errors.ts` |
| UDP-blocked classification | `net/crates/net/browser-ts/src/udp-probe.ts` |
| `openSession` / `MeshSession` (leader surface) | `net/crates/net/browser-ts/src/leader/` |
| `defineStore` | `net/crates/net/browser-ts/src/store/definition.ts` |
| `hostStore` + `StoreTransport` | `net/crates/net/browser-ts/src/store/host.ts` |
| `joinStore` + `JoinedStoreHandle` | `net/crates/net/browser-ts/src/store/join.ts` |
| Store types, errors, bounds | `net/crates/net/browser-ts/src/store/types.ts`, `errors.ts`, `wire.ts`, `chunker.ts`, `owner.ts` |
| `bindEntities` (the `/three` subpath) | `net/crates/net/browser-ts/src/three/index.ts` |
| The wasm leaf (Rust) | `net/crates/net/leaf/` |
| Anchor CLI | `net/crates/net/cli/src/commands/anchor.rs` |
| Real-browser witness runner | `net/crates/net/tests/rtc_browser/` |
| Direct-path demo | `net/crates/net/examples/browser-demo/` |
| Package README (the long-form contract) | `net/crates/net/browser-ts/README.md` |

## What a checkout will not have

- **The built package** (the dist directory). It is `tsc` plus bundler output,
  produced by `npm run build`; a checkout has `src/` only.
- **The wasm-bindgen output package** (the `pkg` directory the leaf build
  writes beside its source). It is produced by the Rust build plus
  `wasm-bindgen` at the version the leaf pins. The `src/wasm.ts` module declares
  that boundary in one place, and `test/fake-wasm.ts` satisfies it.
- **`bindings/node/index.d.ts`** — napi-generated and gitignored. Read the
  `#[napi]` attributes in `bindings/node/src/*.rs` instead. (That is the Node
  SDK's surface, not this package's.)

## How to read a citation

The browser package's own root is `net/crates/net/browser-ts/`, so a citation
written from inside the package (`src/store/host.ts`, `test/store/hosted.test.ts`)
roots there. A path like `net/crates/net/browser-ts/src/store/host.ts` is
repo-rooted and unambiguous. A line anchor is a **hint**, not an address: the file has moved
since it was written and the region is what to look for, not the number. When a
citation does not resolve, search the file by symbol name — the declarations in
`browser-ts/src/index.ts` name every public symbol, and `src/store/index.ts`
names every store export.
