# @net-mesh/core

napi-rs binding to the `libnet` cdylib — the Node-facing surface for the Net mesh, RedEX, CortEX, NetDB, MeshDB, and MeshOS.

## Install

```bash
npm install @net-mesh/core
```

The package publishes pre-built `.node` artifacts for every platform listed in `package.json -> napi.targets`. Pulling the package from npm requires no Rust toolchain.

## Build from source

```bash
npm install
npm run build          # release; full feature set
npm run build:debug    # debug; full feature set
```

The canonical feature list lives in `package.json -> scripts.build` so CI artifacts and local builds stay aligned.

## Cargo features

The five feature flags that gate the storage / query / OS surfaces on this binding. Artifacts published to npm ship with every feature enabled; `napi build` invocations without a feature silently omit its symbols and the build never warns. The TypeScript wrapper destructures the napi exports lazily, so a missing feature surfaces as `undefined` at the import site rather than a load-time error.

| Feature | Surface enabled in the napi module |
|---|---|
| `cortex` | `Redex`, `RedexFile`, `TasksAdapter`, `MemoriesAdapter`, `NetDb`, `Task`, `Memory`, watch iterators, `RedexError`, `CortexError`, `NetDbError` |
| `redex-disk` | Disk-backed RedEX persistence — the `persistentDir` ctor option and `persistent: true` on `openFile`. Without it the persistent path rejects with `RedexError`. |
| `netdb` | `NetDb` composition (requires `cortex`); the `net_netdb_*` FFI entry points ship with this feature. |
| `meshdb` | `MeshQuery`, `MeshQueryRunner`, `QueryBuilder`, `Predicate`, `InMemoryChainReader`, plus the `libnet_meshdb` cdylib. |
| `meshos` | `MeshOsDaemonSdk`, `MeshOsDaemonHandle`, plus the `libnet_meshos` cdylib. |

Enable at build time:

```bash
napi build --platform --release --features "cortex netdb redex-disk meshdb meshos"
```

The repo's `npm run build` script already passes a superset of these flags (`redis,net,cortex,compute,groups,meshos,deck,meshdb`) — see `package.json -> scripts.build` for the exact invocation. Slim the list by editing that script or invoking `napi build` directly with the features you actually need.

## License

Apache-2.0
