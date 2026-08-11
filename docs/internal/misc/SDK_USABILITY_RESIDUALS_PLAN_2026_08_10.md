# SDK Usability Residuals Implementation Plan

> **For Hermes:** Use subagent-driven-development skill to implement this plan task-by-task.

**Goal:** Close the four residual usability defects left after the 0.35 SDK round: the TypeScript tool-serving shutdown leak, the stale TypeScript `localAddr()` statement, the obsolete five-library C overview, and install pages that do not track the release that actually shipped.

**Architecture:** Keep the repair narrow. Tool registration owns the lazy metadata service only while at least one tool is registered, so closing the final tool must close that service and release its mesh reference. Documentation claims become mechanically checked: topology text must describe one library, and install versions must derive from an actual unified release tag rather than whichever release-note files happen to exist.

**Tech Stack:** TypeScript, napi-rs Node bindings, Vitest, Python 3 checker scripts, Git tags, Markdown/Next.js documentation, GitHub Actions.

---

## 1. Status and evidence boundary

**Plan base:** `43b66dbc740381cf97e6cc1e19fa52fb7bf9c99a` (`master`, 2026-08-10)

**Usability audit base:** `300deeee6b2ceb55507cb8101289a9c992b0a37f`

**Usability repair branch head:** `a9679e2b85644af73d8446e7178d1cd284ed9971`

**Published release containing the repair round:** `v0.35.0`, commit `75d158b5a1b5965c8e463518678e1cc404e56719`

The 0.35 packages are published. This plan is not a publication plan and must not reopen findings already closed by that release. It addresses only the residuals reproduced or source-confirmed after publication.

| ID | Severity | Finding | Current evidence | Closure criterion |
|---|---:|---|---|---|
| U-1 | Major | TypeScript `serveTool` leaves the lazy `tool.metadata.fetch` `ServeHandle` alive after the last tool handle closes. Even after `rpc.raw.close()`, `MeshNode.shutdown()` fails with `cannot shutdown: outstanding references exist`. | `net/crates/net/bindings/node/tool.ts:246-310`; fresh public 0.35 lifecycle reproduction | The documented `serveTool → handle.close → rpc.raw.close → shutdown` path exits cleanly, with a live test that fails on 0.35 behavior. |
| U-2 | Minor | The TypeScript quickstart says there is no `localAddr()` accessor although `MeshNode.localAddr()` exists. | `web/src/content/docs/sdk/quickstart/typescript.md:83-87`; implementation at `net/crates/net/sdk-ts/src/mesh.ts:503-513` | The quickstart uses or accurately describes `localAddr()`, and a symbol/doc gate prevents regression. |
| U-3 | Moderate | The C overview still says “eleven headers and five shared libraries,” contradicting the one-`libnet` topology. The existing checker misses prose counts. | `web/src/content/docs/sdk/c/README.md:19-25`; `.github/scripts/check-one-library-docs.py:58-64` | Every current consumer page says one library, and the checker rejects positive claims of multiple shared libraries. |
| U-4 | Moderate | Install pages still name 0.34 after 0.35 shipped. The checker derives “latest” from release-note filenames, but no 0.35 release-note file exists. | `.github/scripts/check-install-version.py:60-84`; `web/src/content/docs/start/install/*.md` | Install pages name 0.35 and the checker derives the latest shipped release from an actual unified `vX.Y.Z` tag reachable from HEAD. |

## 2. Non-goals

- Do not redesign `TypedMeshRpc`, napi ownership, or Rust `ensure_tool_metadata_fetch_installed`.
- Do not make Python handshakes async; their concurrent blocking contract is documented and working.
- Do not change Go’s cgo architecture or checkout-relative fallback linker directive.
- Do not add registry network calls to ordinary CI. Public registries are release evidence, not a deterministic PR dependency.
- Do not create 0.35 release notes merely to make the old checker pass. Release-note completeness is a separate release-process question.
- Do not sweep historical release notes; dated descriptions of the old multi-library topology remain historical facts.
- Do not alter SDK parity or security policy in this plan.

---

### Task 1: Freeze the TypeScript tool-serving shutdown failure

**Objective:** Add unit and live witnesses that prove the metadata service’s lifetime is bounded by active tool registrations and that the public cleanup journey releases every mesh reference.

**Files:**
- Modify: `net/crates/net/bindings/node/test/tool.test.ts`
- Modify: `net/crates/net/bindings/node/test/mesh_rpc_live.test.ts` or create a focused live test beside it if separation is clearer
- Modify: `.github/workflows/ci.yml`

**Step 1: Add a failing unit witness for final-handle cleanup**

Extend the fake `TypedMeshRpc` used by `tool.test.ts` so every `serve()` call returns a numbered fake `ServeHandle` with observable `close()` count. Register one tool, close its `ToolServeHandle`, and assert:

```typescript
expect(toolHandleCloseCount).toBe(1);
expect(metadataFetchCloseCount).toBe(1);
```

Expected before the implementation repair: the tool handler closes, but the metadata-fetch handler remains at `0` closes.

**Step 2: Add a shared-lifetime inverse**

Register two different tools against one RPC handle. Close the first and assert the metadata-fetch handler remains open. Close the second and assert it closes exactly once. Call both tool handles’ `close()` methods again and assert every count is unchanged.

This prevents the tempting but wrong repair of closing the shared metadata service whenever any tool closes.

**Step 3: Add a registration-failure rollback witness**

Make the fake RPC throw while registering the requested tool after the metadata service has been installed. Assert the failed registration leaves no descriptor and no live metadata-fetch handle. This pins cleanup on partial construction rather than only the happy close path.

**Step 4: Add the live public lifecycle witness**

Using a real `MeshNode`/`TypedMeshRpc` built with the existing integration-test feature set:

```typescript
const node = await MeshNode.create({
  bindAddr: '127.0.0.1:0',
  psk: '42'.repeat(32),
});
const rpc = node.rpc();
const handle = serveTool(rpc, { name: 'echo' }, async (req) => req);

handle.close();
rpc.raw.close();
await expect(node.shutdown()).resolves.toBeUndefined();
```

Do not use sleeps or V8 finalization. The test must pass by explicit ownership release.

**Step 5: Run the RED tests**

Run from `net/crates/net/bindings/node`:

```bash
npm test -- --run test/tool.test.ts
RUN_INTEGRATION_TESTS=1 npm test -- --run test/mesh_rpc_live.test.ts
```

Expected: the unit final-cleanup/rollback witnesses and the live shutdown witness fail on the current implementation for the retained metadata handle.

**Step 6: Wire the live witness into the existing Node integration job**

Use the existing built test binary and `RUN_INTEGRATION_TESTS=1` path. Do not create a second native build solely for this test. Confirm `.github/workflows/ci.yml` triggers when `bindings/node/tool.ts`, the live test, or the relevant workflow changes.

**Step 7: Commit the witnesses**

```bash
git add net/crates/net/bindings/node/test/tool.test.ts \
        net/crates/net/bindings/node/test/mesh_rpc_live.test.ts \
        .github/workflows/ci.yml
git commit -m "test(node): reproduce tool metadata shutdown leak"
```

---

### Task 2: Bound the metadata service to active tool registrations

**Objective:** Close the lazy metadata-fetch handler when the final unary or streaming tool is deregistered, including rollback after failed registration.

**Files:**
- Modify: `net/crates/net/bindings/node/tool.ts:246-410`
- Test: `net/crates/net/bindings/node/test/tool.test.ts`
- Test: live witness from Task 1

**Step 1: Add one cleanup helper**

Add a private helper that owns the invariant:

```typescript
function _closeRegistryIfEmpty(
  rpc: TypedMeshRpc,
  entry: ToolRegistryEntry,
): void {
  if (entry.registry.size !== 0) return;
  entry.fetchHandle?.close();
  entry.fetchHandle = null;
  _toolRegistries.delete(rpc);
}
```

Deleting the WeakMap entry is load-bearing: a later `serveTool` call must install a fresh metadata service rather than reuse a closed entry.

**Step 2: Make unary registration transactional**

Keep installation order, but wrap the requested `rpc.serve(...)` in `try/catch`. If requested-tool registration throws:

1. remove the descriptor inserted for that attempt;
2. call `_closeRegistryIfEmpty(rpc, entry)`;
3. rethrow the original error.

Do not swallow registration failures.

**Step 3: Release shared metadata on unary close**

After `entry.registry.delete(descriptor.toolId)` and `inner.close()`, call `_closeRegistryIfEmpty(rpc, entry)`. Preserve idempotence with the existing `closed` guard.

**Step 4: Apply identical semantics to streaming registration**

`serveToolStreaming` uses the same registry and metadata handler. Give its constructor rollback and final-handle cleanup the same code path. Do not create a second registry or a second cleanup policy.

**Step 5: Correct the lifetime comments**

Replace comments saying the metadata handler lives until process/RPC exit. State the actual invariant:

- one metadata service per RPC while at least one tool is registered;
- closing the last tool closes metadata service and deletes the WeakMap entry;
- later registration installs a new metadata service;
- unary and streaming tools share that count/lifetime.

**Step 6: Run GREEN verification**

```bash
cd net/crates/net/bindings/node
npm run build:ts
npm test -- --run test/tool.test.ts
RUN_INTEGRATION_TESTS=1 npm test -- --run test/mesh_rpc_live.test.ts
```

Expected: all focused tests pass; the live shutdown path does not require GC or a delay.

**Step 7: Run the external TypeScript consumer gate**

From repository root, after building the native binding and SDK as CI does:

```bash
.github/scripts/check-ts-consumer.sh
```

Expected: exit `0`; public declarations and the ordinary `MeshNode.rpc()` cleanup path remain valid.

**Step 8: Commit the repair**

```bash
git add net/crates/net/bindings/node/tool.ts
git commit -m "fix(node): release tool metadata service on final close"
```

---

### Task 3: Remove the two contradictory SDK claims and strengthen the topology checker

**Objective:** Make the TypeScript quickstart and C overview agree with the public APIs/artifacts, then ensure the known contradiction class cannot return.

**Files:**
- Modify: `web/src/content/docs/sdk/quickstart/typescript.md:83-87`
- Modify: `web/src/content/docs/sdk/c/README.md:12-25`
- Modify: `.github/scripts/check-one-library-docs.py`
- Modify: `.github/workflows/ci.yml` only if trigger coverage does not already include all modified paths

**Step 1: Correct the TypeScript address guidance**

Replace the false “there is no `localAddr()` accessor” statement with concrete guidance:

```typescript
const hostAddr = host.localAddr();
await agent.connect(hostAddr, host.publicKey(), host.nodeId());
```

Explain that `localAddr()` is specifically how a `:0` bind becomes connectable. Keep the warning that `start()` is async.

**Step 2: Correct the C build command and topology statement**

Change the C overview to say the ABI is split across eleven headers but compiled into one shared/static library, `libnet`. The command comment must say to include the required headers and link `-lnet`; remove “five shared libraries” and “libraries required by your application.”

**Step 3: Add a failing checker self-test for prose counts**

Extend `.github/scripts/check-one-library-docs.py` with a narrow positive-claim pattern for multiple shared libraries, covering both word and numeric counts. It should reject examples such as:

```text
The ABI is split across eleven headers and five shared libraries.
Link the three shared libraries required by your application.
```

It must allow:

```text
Eleven headers, one shared library.
Everything is compiled into libnet.
```

Historical release-note directories remain exempt.

**Step 4: Make the checker’s output match its scope**

The current success line says all files “build and link one library,” although prose-count checking is now included. Change it to describe both guarantees: no per-surface link/build instructions and no current positive claim of multiple libraries.

**Step 5: Run the focused checks**

```bash
python .github/scripts/check-one-library-docs.py --self-test
python .github/scripts/check-one-library-docs.py
python .github/scripts/check-spine-symbols.py
```

Expected: all exit `0`; the self-test proves the former five-library sentence would fail.

**Step 6: Run web validation**

```bash
cd web
npm run check:docs
npm run check:types
npm run build
```

Expected: all exit `0`.

**Step 7: Commit the documentation/checker repair**

```bash
git add web/src/content/docs/sdk/quickstart/typescript.md \
        web/src/content/docs/sdk/c/README.md \
        .github/scripts/check-one-library-docs.py \
        .github/workflows/ci.yml
git commit -m "docs(sdk): close address and library topology contradictions"
```

---

### Task 4: Make install-version truth follow a shipped release

**Objective:** Update the install pages to 0.35 and make the checker use the newest unified release tag reachable from HEAD rather than release-note filename coverage.

**Files:**
- Modify: `.github/scripts/check-install-version.py`
- Modify: `.github/workflows/web.yml`
- Modify: `web/src/content/docs/start/install/_shared.md`
- Modify: `web/src/content/docs/start/install/python.md`
- Modify: `web/src/content/docs/start/install/rust.md`
- Modify: `web/src/content/docs/start/install/typescript.md`
- Inspect and modify any additional current install page reported by the checker

**Step 1: Freeze the source-of-truth failure in the self-test**

Extract a pure parser for unified release tags. Add cases proving:

- `v0.35.0` is accepted;
- `v0.35.0-rc.1`, `cli-v0.35.0`, `deck-v0.35.0`, and malformed tags are ignored;
- `v0.35.1` outranks `v0.35.0`;
- a tag not reachable from the checked HEAD is not treated as shipped for that checkout.

Expected before implementation: there is no tag parser and the checker still reports 0.34 from release-note files.

**Step 2: Replace release-note discovery with reachable unified tags**

Implement `latest_released_version()` using local Git metadata:

1. enumerate exact stable tags matching `^v(\d+)\.(\d+)\.(\d+)$`;
2. retain only tags merged into/reachable from `HEAD`;
3. choose the maximum semantic `(major, minor, patch)` tuple;
4. fail closed if no qualifying tag is available.

Do not query GitHub, crates.io, npm, or PyPI in ordinary CI. Do not use workspace package versions because those may describe the next candidate before publication.

**Step 3: Ensure tags exist in Web CI**

Change the Web workflow checkout to fetch tags/history sufficient for the reachability test, for example `fetch-depth: 0`. Add `net/crates/net/Cargo.toml` and release-workflow/tag-relevant paths to trigger coverage only if required by the final implementation. A shallow checkout without tags must fail with a clear prerequisite error, never silently fall back to release notes.

**Step 4: Update current install pages to 0.35**

Change prose and copy-paste dependency pins from 0.34 to 0.35 across every page reported by the checker. Keep same-version pairing language intact.

**Step 5: Update checker documentation**

Remove comments claiming release notes are authoritative. State the release doctrine explicitly:

> Candidate manifests may lead publication; install pages name only the newest stable unified `vX.Y.Z` tag reachable from the deployed docs commit.

This means a future release updates “current release” documentation only once the unified release tag exists. Candidate PRs must not call an unpublished version current.

**Step 6: Run focused checks**

```bash
python .github/scripts/check-install-version.py --self-test
python .github/scripts/check-install-version.py
```

Expected output identifies `v0.35.0` as the newest reachable unified release and reports all current Net-version references as 0.35.

**Step 7: Run Web CI locally**

```bash
cd web
npm run check
npm run build
```

Expected: both exit `0`.

**Step 8: Commit the release-truth repair**

```bash
git add .github/scripts/check-install-version.py \
        .github/workflows/web.yml \
        web/src/content/docs/start/install
git commit -m "docs: derive install version from the shipped release tag"
```

---

### Task 5: Execute the complete residual-closure gate

**Objective:** Prove all four residuals are closed together without weakening existing package or SDK guarantees.

**Files:**
- No new files expected
- Verify all files modified in Tasks 1-4

**Step 1: Run formatting and static checks**

```bash
npm --prefix net/crates/net/bindings/node run build:ts
npm --prefix net/crates/net/sdk-ts run build
python .github/scripts/check-one-library-docs.py
python .github/scripts/check-one-library-docs.py --self-test
python .github/scripts/check-install-version.py
python .github/scripts/check-install-version.py --self-test
python .github/scripts/check-spine-symbols.py
```

Expected: all exit `0`.

**Step 2: Run focused Node tests including the live lifecycle**

```bash
cd net/crates/net/bindings/node
npm test -- --run test/tool.test.ts
RUN_INTEGRATION_TESTS=1 npm test -- --run test/mesh_rpc_live.test.ts
```

Expected: the final tool close releases metadata, `rpc.raw.close()` releases the RPC reference, and mesh shutdown resolves.

**Step 3: Run the public TypeScript consumer check**

```bash
cd <repo-root>
.github/scripts/check-ts-consumer.sh
```

Expected: exit `0` with `skipLibCheck: false`.

**Step 4: Run Web checks**

```bash
cd web
npm run check
npm run build
```

Expected: exit `0`.

**Step 5: Check repository hygiene**

```bash
git diff --check
git status --short
git diff --stat origin/master...HEAD
git diff origin/master...HEAD -- \
  net/crates/net/bindings/node/tool.ts \
  net/crates/net/bindings/node/test/tool.test.ts \
  net/crates/net/bindings/node/test/mesh_rpc_live.test.ts \
  web/src/content/docs/sdk/quickstart/typescript.md \
  web/src/content/docs/sdk/c/README.md \
  web/src/content/docs/start/install \
  .github/scripts/check-one-library-docs.py \
  .github/scripts/check-install-version.py \
  .github/workflows/ci.yml \
  .github/workflows/web.yml
```

Expected: no whitespace errors and no unrelated files.

**Step 6: Require exact-head CI before closure**

At the pushed implementation head, require:

- CI green, including Node unit/live integration and one-library checker;
- Web green, including install-version and documentation checks;
- Coverage green or any unrelated failure explicitly reproduced and attributed rather than waved through;
- exact head SHA recorded in the implementation review.

A rerun is evidence only at the same SHA. Do not call the plan closed from branch-local tests alone.

**Step 7: Final commit if verification-only corrections were needed**

Commit only concrete corrections discovered by the gate. Do not squash or rewrite already reviewed task commits unless explicitly requested.

---

## 3. Acceptance matrix

| Residual | Required positive witness | Required inverse |
|---|---|---|
| U-1 tool lifecycle | Last tool close + RPC close permits mesh shutdown | First of two tool closes does not close shared metadata; repeated close is idempotent |
| U-2 `localAddr()` docs | Quickstart obtains an OS-selected address through `localAddr()` | Spine-symbol check fails if the accessor disappears |
| U-3 one-library prose | All current consumer docs describe one `libnet` | Checker self-test rejects “five shared libraries” and per-surface link flags |
| U-4 current version | Reachable `v0.35.0` makes all install pages require 0.35 | CLI/Deck tags, RC tags, unreachable tags, and candidate manifest versions do not become “current” |

## 4. Public claim after closure

The bounded claim is:

> Net 0.35’s Rust, TypeScript, Python, Go, C, CLI, and Deck entry points install and execute their audited first-use paths. TypeScript tool servers can explicitly release all registrations and shut down without relying on garbage collection. Current SDK documentation matches the public address API, the one-`libnet` topology, and the newest stable unified release tag.

Do not expand that into full cross-language feature parity or proof of every advanced distributed workflow.