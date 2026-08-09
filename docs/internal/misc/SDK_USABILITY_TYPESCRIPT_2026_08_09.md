# TypeScript / Node black-box usability scan — 2026-08-09

## Scope and frozen test state

This is a discovery-only black-box scan. No product code was changed. Reproductions used only the live public site, npm, supplied candidate tarballs, package metadata/README files, generated `.d.ts` files, and ordinary Node/TypeScript execution. I did not inspect implementation source, internal tests, prior review packets, or private plans.

- Source revision represented by the candidate: `300deeee6b2ceb55507cb8101289a9c992b0a37f` (`master`).
- Public docs checked live on 2026-08-09 and matched the public-doc files at that revision:
  - <https://ai2070.net/docs/sdk/typescript>
  - <https://ai2070.net/docs/sdk/typescript/quickstart>
  - <https://ai2070.net/docs/sdk/typescript/announce>
- Host: Microsoft Windows 10.0.26100.8875, AMD64; Git Bash/MSYS `3.6.9-b4195d69.x86_64`.
- Node.js `v24.18.0`; npm `12.0.1`; stable TypeScript `5.9.3`; `@types/node` `24.13.3` where explicitly noted.
- Released npm packages tested: `@net-mesh/sdk@0.34.0`, `@net-mesh/core@0.34.0`, and `@net-mesh/cli@0.34.0`.
- Supplied candidate packages and SHA-256 witnesses:
  - `@net-mesh/cli@0.35.0`: `f43843bcea67fac097493fbef7e1f82078e45f70583fd70e28013821233fa247`
  - `@net-mesh/core@0.35.0`: `bbee8a1a20926f1b1b4bc2d6369eef370bedfa475943d761f335b85fa41accef`
  - `@net-mesh/deck@0.35.0`: `cce9970f527918854e4dd4a2eeb0928d34995233bbb190abbbdd86c18b8ea9dc`
  - `@net-mesh/sdk@0.35.0`: `ab9bd1d7fc96249f50a563a8e9be7a54b9de65ddb204de1286ee3f0c254dd1f9`
- Disposable workspace: `C:/Users/chief/AppData/Local/Temp/net-ts-usability-300deeee-20260809`.
- The resumed workspace contained separate repro directories. Every frozen result below was rerun serially from the named directory with an explicit `cd`; prior parallel/workdir-collapsed output was discarded. Installs that claim a clean package state first removed `node_modules` and `package-lock.json`. Timing used Bash `SECONDS`, not `/usr/bin/time`.
- The earlier self-selected `typescript@6.0.0` and `/usr/bin/time` failures are excluded as investigator noise. No finding depends on either.

## Findings

### TS-01 — The candidate version named by the package set is not obtainable from npm

- Severity: **MAJOR**
- Category: **installation**
- Versions/environment: candidate `@net-mesh/sdk@0.35.0` and `@net-mesh/core@0.35.0`; npm `12.0.1`; Windows/Node environment above.
- User intent: install the SDK/core pair at the version represented by the current docs/source candidate.
- Public path followed: the docs say `npm install @net-mesh/sdk @net-mesh/core` and explicitly say to pin both to the same version. The candidate package metadata identifies that version as `0.35.0`.
- First divergence: npm cannot resolve `@net-mesh/core@0.35.0`; the registry currently ends at `0.34.0` for `sdk`, `core`, `cli`, and `deck`.
- Minimal runnable reproduction (`repros/registry-pinned/package.json`):

```json
{"name":"registry-pinned","private":true,"dependencies":{"@net-mesh/sdk":"0.35.0","@net-mesh/core":"0.35.0"}}
```

```bash
cd 'C:/Users/chief/AppData/Local/Temp/net-ts-usability-300deeee-20260809/repros/registry-pinned' && rm -rf node_modules package-lock.json && SECONDS=0; npm install --ignore-scripts; code=$?; printf 'exit=%s elapsed_seconds=%s\n' "$code" "$SECONDS"; exit "$code"
```

Observed witness (exit `1`, 2 seconds):

```text
npm error code ETARGET
npm error notarget No matching version found for @net-mesh/core@0.35.0.
exit=1 elapsed_seconds=2
```

`npm view` independently listed matching public versions for all four aggregate packages only through `0.34.0`.

- Expected: the version represented by the candidate can be installed from the ordinary registry path, or is clearly labeled unavailable with a complete candidate-install path.
- Actual: a pinned install is impossible from npm. An unpinned install silently selects `0.34.0`, which is not the candidate tested by the current docs/source revision.
- Workaround (separate from defect): pin `sdk` and `core` to `0.34.0` to use the prior release, or use the explicitly supplied local tarballs for candidate testing. The former does not exercise `0.35.0`; the latter is not a public npm install.
- Implementation-source inspection required: **No**.

### TS-02 — A trivial strict TypeScript consumer cannot type-check against the paired candidate packages

- Severity: **MAJOR**
- Category: **interoperability**
- Versions/environment: local candidate tarballs `@net-mesh/sdk@0.35.0` and `@net-mesh/core@0.35.0`; TypeScript `5.9.3`; `@types/node@24.13.3`; Node `v24.18.0`.
- User intent: import `NetNode` in a normal strict NodeNext TypeScript project while checking dependency declarations.
- Public path followed: install the two matching packages as instructed, add ordinary Node typings, import the documented root entry point, and run `tsc --noEmit`.
- First divergence: TypeScript enters the shipped declarations and reports undefined generated names plus SDK declarations importing members that the paired core package does not export.
- Minimal runnable reproduction (`repros/candidate/trivial.ts`):

```typescript
import { NetNode } from '@net-mesh/sdk';

const node = await NetNode.create({ shards: 1 });
await node.shutdown();
```

Exact package set:

```text
@net-mesh/core@0.35.0
@net-mesh/sdk@0.35.0
@types/node@24.13.3
typescript@5.9.3
```

```bash
cd 'C:/Users/chief/AppData/Local/Temp/net-ts-usability-300deeee-20260809/repros/candidate' && SECONDS=0; ./node_modules/.bin/tsc --noEmit --strict --target ESNext --module NodeNext --moduleResolution NodeNext --skipLibCheck false trivial.ts; code=$?; printf 'exit=%s elapsed_seconds=%s\n' "$code" "$SECONDS"; exit "$code"
```

Observed witness (exit `2`, 2 seconds) included:

```text
@net-mesh/core/index.d.ts(127,48): error TS2304: Cannot find name 'DaemonBridgeTsfns'.
@net-mesh/core/index.d.ts(772,47): error TS2304: Cannot find name 'DuplexHandlerArgs'.
@net-mesh/core/index.d.ts(1532,49): error TS2304: Cannot find name 'GreedyConfigJs'.
@net-mesh/sdk/dist/meshdb.d.ts(67,10): error TS2305: Module '"@net-mesh/core"' has no exported member 'InMemoryChainReader'.
@net-mesh/sdk/dist/meshos.d.ts(45,10): error TS2724: '"@net-mesh/core"' has no exported member named 'MeshOsDaemonHandle'. Did you mean 'DaemonHandle'?
@net-mesh/sdk/dist/transport.d.ts(1,10): error TS2305: Module '"@net-mesh/core"' has no exported member 'TransferControl'.
exit=2 elapsed_seconds=2
```

- Expected: a trivial documented import type-checks against the matched `0.35.0` pair.
- Actual: unrelated shipped declarations make the project fail before application code can be checked.
- Workaround (separate from defect): `skipLibCheck: true` suppresses dependency declaration checking, but hides package-contract defects and is not equivalent to valid declarations.
- Implementation-source inspection required: **No**; package metadata and shipped `.d.ts` files were sufficient.

### TS-03 — The live TypeScript quickstart gives the wrong PSK type and fails both compile-time and runtime

- Severity: **MAJOR**
- Category: **documentation**
- Versions/environment: live quickstart on 2026-08-09; candidate `@net-mesh/sdk@0.35.0`/`core@0.35.0`; TypeScript `5.9.3`; Node `v24.18.0`.
- User intent: create the first documented mesh node.
- Public path followed: copied the quickstart's `new Uint8Array(32).fill(0x42)` PSK and `MeshNode.create(...)` call literally.
- First divergence: the shipped TypeScript contract requires `string`, not `Uint8Array`; direct execution reaches the native boundary and rejects the value as `StringExpected`.
- Minimal runnable reproduction (`repros/candidate/mesh-bytes.ts`):

```typescript
import { MeshNode } from '@net-mesh/sdk';
const psk = new Uint8Array(32).fill(0x42);
const node = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk });
await node.shutdown();
```

Compile witness:

```text
mesh-bytes.ts(3,63): error TS2322: Type 'Uint8Array<ArrayBuffer>' is not assignable to type 'string'.
```

Runtime witness:

```bash
cd 'C:/Users/chief/AppData/Local/Temp/net-ts-usability-300deeee-20260809/repros/candidate' && SECONDS=0; node mesh-bytes.ts; code=$?; printf 'exit=%s elapsed_seconds=%s\n' "$code" "$SECONDS"; exit "$code"
```

Exit `1`, 0 seconds:

```text
Error: Failed to convert JavaScript value `Object {...}` into rust type `String` on MeshOptions.psk
code: 'StringExpected'
```

- Expected: the first mesh-node snippet compiles and creates/shuts down a node.
- Actual: the exact documented value is rejected by both the public type contract and runtime.
- Workaround (separate from defect): a 64-character hex string, `psk: '42'.repeat(32)`, created and shut down the candidate node successfully (exit `0`). The candidate npm README itself says string, directly contradicting the live quickstart.
- Implementation-source inspection required: **No**.

### TS-04 — The documented tool-serving bridge uses a private field and still fails before a tool is served

- Severity: **MAJOR**
- Category: **API ergonomics**
- Versions/environment: live announce page on 2026-08-09; candidate `sdk/core@0.35.0`; Node `v24.18.0`.
- User intent: serve the first TypeScript tool through the documented `MeshNode` path.
- Public path followed: the page explicitly says the SDK has no public native accessor, casts the node to reach `_native`, then calls `TypedMeshRpc.fromMesh(native)`. The minimal repro copied that bridge and used the string PSK required by the package.
- First divergence: an ordinary documented task already requires private-field access; executing that exact bridge then fails with `InvalidArg` before `serveTool` runs.
- Minimal runnable reproduction (`repros/candidate/private-tool.ts`, reduced to the failing bridge):

```typescript
import { MeshNode } from '@net-mesh/sdk';
import { TypedMeshRpc } from '@net-mesh/core/mesh_rpc';

const node = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: '42'.repeat(32) });
const native = (node as unknown as { _native: object })._native;
const rpc = TypedMeshRpc.fromMesh(native);
```

```bash
cd 'C:/Users/chief/AppData/Local/Temp/net-ts-usability-300deeee-20260809/repros/candidate' && SECONDS=0; node private-tool.ts; code=$?; printf 'exit=%s elapsed_seconds=%s\n' "$code" "$SECONDS"; exit "$code"
```

Observed witness (exit `1`, 1 second):

```text
Error: Failed to recover `NetMesh` type from napi value
    at TypedMeshRpc.fromMesh (.../@net-mesh/core/mesh_rpc.js:94:48)
code: 'InvalidArg'
```

- Expected: a public SDK path exposes a supported RPC handle and the documented serve operation runs end to end.
- Actual: the docs normalize private-field access, and the documented conversion rejects that private value. No tool is served, announced, discovered, or invoked.
- Workaround (separate from defect): none found within public surfaces. Determining a different private object shape would require implementation inspection, which is outside an ordinary SDK task and was not attempted.
- Implementation-source inspection required: **No to establish the defect**. It would be required to guess beyond the failed public path.

### TS-05 — Candidate CLI and Deck aggregate packages report successful installation without a runnable platform binary

- Severity: **MAJOR**
- Category: **false success**
- Versions/environment: supplied `@net-mesh/cli@0.35.0` and `@net-mesh/deck@0.35.0` aggregate tarballs; Windows x64; npm `12.0.1`.
- User intent: install each Node-distributed executable and inspect its public help.
- Public path followed: install the supplied aggregate tarball through npm, then invoke the generated `.bin` command.
- First divergence: npm exits `0`, reports the install and audit as successful, and does not surface that the matching optional platform package was not installed. The first command exits `127`.
- Minimal CLI package (`repros/cli-candidate/package.json`):

```json
{"name":"candidate-cli","private":true,"dependencies":{"@net-mesh/cli":"file:C:/Users/chief/AppData/Local/Temp/net-ts-usability-300deeee-20260809/packs/net-mesh-cli-0.35.0.tgz"}}
```

```bash
cd 'C:/Users/chief/AppData/Local/Temp/net-ts-usability-300deeee-20260809/repros/cli-candidate' && rm -rf node_modules package-lock.json && SECONDS=0; npm install; install_code=$?; printf 'install_exit=%s elapsed_seconds=%s\n' "$install_code" "$SECONDS"; ./node_modules/.bin/net-mesh --help; help_code=$?; printf 'help_exit=%s\n' "$help_code"; exit "$help_code"
```

Observed:

```text
added 1 package, and audited 2 packages in 2s
found 0 vulnerabilities
install_exit=0 elapsed_seconds=2
net-mesh: Failed to locate @net-mesh/cli-win32-x64.
Underlying error: Cannot find module '@net-mesh/cli-win32-x64/package.json'
help_exit=127
```

`npm view '@net-mesh/cli-win32-x64@0.35.0' version` independently returned exit `1`, `E404 No match found for version 0.35.0`.

The exact Deck twin reproduced from `repros/deck-candidate`: npm install exit `0` in 2 seconds, followed by `net-deck --help` exit `127` because `@net-mesh/deck-win32-x64` was absent.

- Expected: either the matching platform package is installed and the command runs, or installation fails and names the unavailable artifact.
- Actual: installation is a false success; only first execution reveals that the product is absent.
- Workaround (separate from defect): `@net-mesh/cli@0.34.0` installed its Windows platform package and `net-mesh --help` exited `0`. No candidate `0.35.0` workaround was available from the supplied tarballs/public registry. A Deck workaround was not claimed: its released TUI could not be validly exercised through this noninteractive pipe.
- Implementation-source inspection required: **No**; aggregate metadata, npm behavior, registry lookup, and launcher diagnostics were sufficient.

### TS-06 — The candidate npm README advertises a mesh accessor and subpaths the package does not export

- Severity: **MODERATE**
- Category: **documentation**
- Versions/environment: candidate `@net-mesh/sdk@0.35.0`; Node `v24.18.0`.
- User intent: follow the README that ships on the npm package surface.
- Public path followed: the README's first tool loop calls `node.localAddr()`, and its submodule table names imports such as `@net-mesh/sdk/mesh`.
- First divergence A: the README loop fails at its first post-construction observation.

```text
TypeError: node.localAddr is not a function
exit=1 elapsed_seconds=0
```

Minimal command:

```bash
cd 'C:/Users/chief/AppData/Local/Temp/net-ts-usability-300deeee-20260809/repros/candidate' && node readme-loop.ts
```

- First divergence B: the package's `exports` contains only `.` and `./tool`; a two-line `import { MeshNode } from '@net-mesh/sdk/mesh'` fails.

```bash
cd 'C:/Users/chief/AppData/Local/Temp/net-ts-usability-300deeee-20260809/repros/candidate' && node subpath.mjs
```

```text
Error [ERR_PACKAGE_PATH_NOT_EXPORTED]: Package subpath './mesh' is not defined by "exports"
exit=1 elapsed_seconds=0
```

- Expected: the shipped README's first loop and named public submodule paths match the package contract.
- Actual: the README is stale relative to both runtime and package exports. The live web quickstart correctly warns that no `localAddr()` accessor exists, so the two public documentation surfaces also contradict each other.
- Workaround (separate from defect): retain the explicitly bound address instead of calling `localAddr()`; import `MeshNode` from the package root. Other README-named subpaths have no public export workaround unless their symbols are also re-exported from the root.
- Implementation-source inspection required: **No**.

## Clean areas and non-findings

These are bounded observations, not claims about unexercised surfaces.

1. **Released SDK install and bus runtime work.** A clean install of `sdk/core@0.34.0` plus TypeScript `5.9.3` exited `0` in 29 seconds. The documented bus program executed end to end, printed `accepted: ingested=1`, shut down, and exited `0` in 2 seconds.
2. **Candidate local tarball installation itself works.** The matched local `sdk/core@0.35.0` tarballs installed with exit `0` in 3 seconds.
3. **Candidate bus runtime works.** A `NetNode` accepted one event, reported `eventsIngested=1` and `eventsDropped=0`, shut down, and exited `0`.
4. **Candidate CommonJS root loading works.** `require('@net-mesh/sdk')` exposed `NetNode` as a function and `require('@net-mesh/core')` exposed `Net` as a function; exit `0`.
5. **Candidate string-PSK construction/lifecycle works.** `psk: '42'.repeat(32)` created and shut down a `MeshNode`; `nodeId()` was `bigint`, and the absence of `localAddr` matched the live web quickstart's handshake warning.
6. **Prior released CLI remains runnable.** `@net-mesh/cli@0.34.0` installed with its platform package and `net-mesh --help` exited `0`.
7. **Excluded investigator noise.** No severity or conclusion is assigned to unavailable `/usr/bin/time`, prior parallel workdir collapse, the earlier arbitrary TypeScript version choice, or a released Deck TUI run through a noninteractive output pipe.

## Overall result

The event-bus happy path is usable at runtime, including on the supplied candidate. The first real TypeScript mesh/tool journey is not: the live quickstart supplies a rejected PSK type, the announce guide requires an unsupported private bridge that fails at runtime, and the matched candidate declarations fail a trivial strict consumer. In parallel, the candidate CLI/Deck aggregate packages create a major false-success installation path because their required Windows artifacts are unavailable. These are package-contract/public-path findings; none required implementation-source inspection.
