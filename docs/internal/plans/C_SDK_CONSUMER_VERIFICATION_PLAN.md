# C SDK — verify the shipped header/library pair from a real consumer

## Status

Planned, 2026-10-04. Targets the release after 0.39. Branch
`LZL0/c-consumer-plan`. Follows
[`GO_BINDING_CONSOLIDATION_AND_BLOBS_PLAN.md`](GO_BINDING_CONSOLIDATION_AND_BLOBS_PLAN.md)
(merged as #1165). That plan reached the new C surfaces (trees, ranges,
repair, the blob registry, Go-implemented adapters) only through cgo, from
Go. This plan checks them from C.

## The gap

C is listed as `supported` for blobs and RedEX in the coverage matrix
(`.claude/skills/net-event-bus/bindings/coverage.md:106-107`). The
evidence behind the C column is much thinner than for the other bindings.
Checked on `master` at `4656690`:

1. **There is no distributable artifact.** No release workflow packages
   `libnet` with its headers. The `release-*.yml` files cover crates.io,
   npm, PyPI and the CLI/deck binaries. A C consumer builds from source
   (`web/src/content/docs/sdk/c/headers-and-linking.md:69`,
   `cargo build --release -p net-ffi`), so the pair they use is whatever
   that default build produces.
2. **CI never builds that default library.** Every CI build of `libnet`
   passes `--features net-ffi/test-helpers` (`ci.yml:4749`). The export
   baseline (`bindings/go/net-ffi/exports.baseline`, 612 lines) is
   generated from that build, so it includes test-only seams such as
   `net_mesh_blob_adapter_test_drop_data_chunk` and
   `net_blob_test_barrier_*`. Nothing checks the export set of the library
   a consumer actually gets.
3. **Nothing checks headers against the library.** `check-ffi-exports.py`
   compares the built DLL with the baseline. The Go header-parity tests
   compare `net.h` with `net.go.h`, and the cortex pair with each other.
   No check asserts that every function declared in the eleven shipped
   headers is exported, that every exported `net_*` symbol is declared
   somewhere, or that a constant isn't shared across surfaces. #1167 nearly
   shipped `NET_ERR_WRONG_CHANNEL = -109` on top of
   `NET_ERR_MESH_STREAM_OCCUPIED`; a reviewer caught it, not a tool.
4. **The C programs that run cover almost none of the new surface.** CI
   links and runs the ten skill examples on Linux
   (`run-skill-examples.sh --lang c`, `ci.yml:5170-5177`). Only
   `objectstore.c` touches blobs: `net_mesh_blob_adapter_new` / `_publish`
   and `net_fetch_blob`. Nothing in C exercises:
   - directory transfer;
   - `net_fetch_blob_discovered`;
   - trees, range fetches or repair (`net_mesh_blob_adapter_new_v2`,
     `_store_tree`, `_fetch_range`, `_repair_blob`);
   - the process-wide registry (`net_blob_register_fs_adapter`,
     `net_blob_publish` / `_resolve`);
   - callback adapters (`net_blob_register_callback_adapter_owned`).
5. **The crate's own C examples are stale and unbuilt.** None of the eight
   `net/crates/net/examples/*.c` files is referenced by any workflow:
   - they include headers by relative path into the source tree
     (`#include "../include/net.go.h"`);
   - `transport.c` tells the reader to build `net-mesh` and link
     `-lnet_mesh`, which hasn't existed since the single-cdylib change;
   - `transport.c` shows node bring-up only "in outline".
6. **C has never been compiled on Windows in CI.** The only Windows job is
   the Rust security suite (`ci.yml:6091`). The docs give MSVC and MinGW
   link recipes (`headers-and-linking.md:80-113`), but nothing runs them.
   One contract makes this matter: in nRPC handlers the consumer
   `malloc(3)`s a response buffer and Rust frees it with `free(3)`
   (`net_rpc.h:266-268`). That is only safe when the C program and
   `libnet` share a C runtime. The Rust host target here is
   `x86_64-pc-windows-msvc` (UCRT), and a MinGW toolchain built against
   `msvcrt.dll` would free across heaps.
7. **ABI versioning is uneven.** Only `net_rpc.h` and `net_org.h` carry an
   ABI-version macro with a runtime check. The blob, transport and cortex
   surfaces have none. Two surfaces lack any release-to-release
   compatibility check:
   - the additive S6/G-B functions;
   - #1167's breaking token change.

## The design

### The artifact: a C SDK bundle

The consumer builds against a bundle, not the source tree:

```
net-c-sdk-<version>-<target>/
  include/   the eleven headers, copied from net/crates/net/include
  lib/       libnet.so | libnet.dylib | net.dll + net.dll.lib
  EXPORTS    the shipped export set, one symbol per line
```

The bundle is made by one script, `.github/scripts/make-c-bundle.py`,
from a **default-feature** `cargo build --release -p net-ffi`, the command
the docs give. CI builds it on Linux and Windows; every later slice
compiles against it and nothing else. A consumer program may include
headers only by name (`#include "net_transport.h"`), with `-I
bundle/include`.

**Decision D1: does the bundle become a release asset?**
- **(a)** CI-only, as the reference for what a source build produces.
- **(b)** Also attach it to the GitHub release.

**Recommendation: (a) now**, with (b) as the optional slice C6.
Publishing adds signing, per-target matrices and a support promise; the
checks are worth having first and don't depend on it.

### The audit tool

`.github/scripts/check-c-abi.py`, with `--self-test`, as house style
requires. It reads the bundle and the Rust sources, and checks:

1. **Declared ⇒ exported.** Every function declared in `include/*.h`
   appears in the bundle's export set. A feature-gated function must be
   exported anyway, through its stub. The `blob_stubs` pattern makes that
   the existing contract.
2. **Exported ⇒ declared.** Every exported `net_*` symbol is declared in
   some shipped header, or appears on a short allowlist with a reason for
   each entry (for example, symbols only the Go mirror headers declare).
3. **Arity and return type match Rust.** For each declared function, its
   `pub unsafe extern "C" fn` in `src/ffi/**` and the seven `*-ffi`
   crates has the same parameter count and a compatible return type. This
   generalises the Go arity witnesses, which pin a handful of functions,
   to all of them.
4. **Constants are unique.** A `NET_*` error value defined by two shared
   headers, or by two surfaces a single call can return, is an error unless
   allowlisted. This is the −109 lesson.
5. **No test seams ship.** The default bundle exports nothing matching the
   test-seam patterns (`_test_`, `net_blob_test_barrier_*`).
6. **Additive compatibility against the last release.** The tool diffs
   declarations against tag `v0.39.0`'s headers. A removed function or a
   changed signature fails unless the commit range declares it breaking (a
   `BREAKING-C-ABI:` line in a commit message, which #1167's would carry).

**Alternative considered: extend the Go header-parity tests.** Rejected.
They only run with cgo, only see the Go mirror headers, and would leave a
C-only consumer with nothing.

### Consumer programs

The new C programs live in `net/crates/net/examples/c/` and replace the
stale `examples/*.c`. Each is a complete, runnable program that exits
non-zero on any failed assertion. Two rules are enforced by a lint in the
audit tool:
- headers are included by name only;
- no `net_*` prototype or `extern` declaration appears in an example file.

**Decision D2: what happens to the eight stale `examples/*.c`?**
**Recommendation: delete them** in the slice that lands their
replacements, and record each one's fate in this plan, so their topics
(meshdb, meshos, deck, scheduler) aren't silently dropped. Topics this
plan doesn't replace are listed under "Not in scope", with their gap left
open in the matrix.

**Decision D3: how does the repair program break a shard without test
seams?** The shipped bundle has no `_test_drop_data_chunk`.
- **(a)** Use a persistent adapter: store a Reed-Solomon tree, shut down,
  delete one data-chunk file from the adapter's directory, reopen, then
  repair. This is the fallback the Go plan named.
- **(b)** Build a second, test-helpers bundle just for this program.

**Recommendation: (a).** It's what a real operator would hit, and it
keeps the "shipped artifact only" rule unbroken. It depends on the
on-disk chunk layout, so the program finds the file through
`net_mesh_blob_adapter_tree_node_cache_stats` / `net_blob_ref_describe`
rather than a hard-coded path. If that proves impossible, fall back to
(b) and say so here.

## The slices

### C0: the bundle

`make-c-bundle.py` plus a CI step on `ubuntu-latest` and `windows-latest`
that builds the default `net-ffi` library and stages the bundle.

**Proves it:**
- The bundle has eleven headers (cross-checked by `check-header-count.py`)
  and exactly one library.
- `EXPORTS` is generated, then pinned as
  `bindings/go/net-ffi/exports.shipped.baseline`, so a change to the
  shipped surface shows in review.
- The existing test-helpers baseline stays as it is.

### C1: the audit

`check-c-abi.py` runs checks 1–6 against the C0 bundle, in CI next to
`check-ffi-exports.py`.

**Proves it:**
- `--self-test` plants one defect per check and requires each to be
  reported:
  - a declared but unexported function;
  - an exported but undeclared function;
  - an arity mismatch;
  - a duplicated constant value;
  - a test seam in the shipped export set;
  - a removed declaration with no `BREAKING-C-ABI:` line.
- The first run on the real tree is recorded here with every finding.
  Each finding is fixed in this slice, or allowlisted with a reason, or
  deferred under "Defects found on the way".

### C2: lifecycle and transfer programs

- **`lifecycle.c`.** Two in-process nodes go through `net_mesh_new`,
  `_start`, `_accept` / `_connect`, `_shutdown` and `_free`. It also
  covers the error paths:
  - a call after shutdown returns the shutting-down code;
  - each free function is NULL-safe;
  - a double close returns a code instead of crashing.
- **`transfer.c`.**
  - `net_serve_blob_transfer`, publish, `net_fetch_blob` and
    `net_fetch_blob_discovered`.
  - `net_store_dir`, `net_fetch_dir` and `net_dir_manifest_read`, with
    the tree compared byte for byte.
  - Refusals: a path that escapes its root, an unknown ref, an empty
    manifest.

**Proves it:**
- Both programs build against the bundle only and run under
  `run-c-consumers.sh`, which reuses `run-skill-examples.sh`'s run
  contract.
- Each assertion is a named check printed on success. CI fails if the
  number of named checks drops below the recorded floor; that's the
  roster pattern.

### C3: tree, range and repair programs

- **`tree_range.c`.**
  - `net_mesh_blob_adapter_new_v2` and `_store_tree`, in Replicated and
    in Reed-Solomon form.
  - `_fetch_range`, covering every case of its contract:
    - a reversed range;
    - an empty range past the end;
    - a range ending past the blob;
    - the 1 GiB cap, against a ref that claims 2 GiB;
    - an exact sub-range.
- **`repair.c`.** The D3 flow. The program asserts:
  - the shard really is gone (a fetch that would need it fails) before
    repair runs;
  - the repair report's counts are correct;
  - the blob reads back intact afterwards.

**Proves it:** both programs run on Linux and Windows. The cap check uses
the same re-encoded ref as Go's `bigSmallRef`, so both bindings test the
same boundary.

### C4: callback programs

- **`blob_callbacks.c`.** A C `net_blob_adapter_vtable_t` backed by an
  in-memory map, registered with `net_blob_register_callback_adapter_owned`
  and driven through `net_blob_publish` / `net_blob_resolve`. Its buffers
  come from `malloc` and go back through its own `free_buffer`. It
  asserts that:
  - `release_fn` runs exactly once, after unregister;
  - a duplicate-id registration never calls `release_fn` and leaves the
    context with the caller;
  - a callback that returns an error surfaces as the matching
    `NET_ERR_BLOB_*` code.
- **`rpc_callbacks.c`.** An nRPC handler whose response comes from
  `malloc`, the `net_rpc.h:266` contract. Run on Windows, this is where a
  mismatched C runtime between consumer and library would show up.

**Proves it:** the release counts above. The barrier-held cases stay in
Go; they need the test seams, and the shipped bundle has none.

### C5: allocation and teardown

Every C2–C4 program runs again under memory checkers:

- **Linux:** built with `-fsanitize=address,undefined` and run with
  LeakSanitizer. Rust uses the system allocator, so LeakSanitizer sees
  leaks of buffers that `libnet` returns and the program never frees.
- **Windows:** two toolchains, kept as two matrix entries:
  - MSVC with `/MD` (UCRT), running against the debug CRT heap checks
    (`_CrtSetDbgFlag`);
  - MinGW-w64 **UCRT**.

  A third run uses a MinGW-w64 **MSVCRT** toolchain against `rpc_callbacks.c`
  alone. It is expected to fail. It shows whether the cross-runtime hazard
  is real; the docs are then fixed to require UCRT, or the contract is
  changed (see Risks).
- **Error paths:** every function that fails leaves its out-parameters at
  `(NULL, 0)`, the S6 contract, checked on the refusal paths C2–C4 already
  drive.

**Proves it:**
- Sanitizer-clean runs on Linux and debug-heap-clean runs on Windows.
- A planted leak and a planted double free in a throwaway variant of
  `transfer.c` must each be caught. That proves the checkers are actually
  armed.

### C5b: unexercised surfaces (record only)

The audit (C1) covers all eleven headers, but C2–C4 exercise only
transport, blobs, the registry, callbacks and nRPC. For the rest
(`net_meshdb.h`, `net_meshos.h`, `net_deck.h`, `net_mcp.h`,
`net_subnet.h`, and the cortex surface beyond tokens), C5b records, per
header, whether any C program now exercises it. The matrix in C6 must not
claim more than that.

### C6: docs and the support matrix

- The C docs (`include/README.md`, `web/src/content/docs/sdk/c/*.md`,
  the C sections of the skill guides) are rebuilt from the C2–C4 programs:
  - snippets are taken from code that runs;
  - every link line is one that actually linked in C0;
  - the Windows guidance names the toolchains C5 ran.
- The C column of `docs/data/capabilities/*.yaml` is checked against C5b:
  - each `supported` cell names a consumer program or a skill example
    that exercises it;
  - a cell nothing exercises drops to `partial`, with the gap stated;
  - `capability_records.py --check` regenerates the tables.

**Proves it:**
- A small extension to `capability_records.py` requires a C-cell
  `evidence:` field and checks that the named file exists and runs in CI.
- The existing guards stay green: skills, header count, doc code width.

### C7 (optional, D1(b)): the bundle as a release asset

Attach the C0 bundle to GitHub releases for Linux x86_64 and Windows
x86_64, with checksums, following
[`ANCHOR_PREBUILT_BINARIES_PLAN.md`](ANCHOR_PREBUILT_BINARIES_PLAN.md).
This slice is only planned if D1 says (b).

## Risks

- **The default build differs from the tested one in more than seams.**
  `net-ffi`'s default features could drop a surface the docs promise.
  *Fallback:* C1's declared ⇒ exported check catches it. The fix is a
  feature change in `net-ffi/Cargo.toml`, recorded here.
- **The repair program can't locate a chunk file without the seam.**
  *Fallback:* D3(b), a test-helpers bundle for `repair.c` alone, stated as
  such in the matrix evidence.
- **The MSVCRT check shows the `free(3)` contract is unsafe.**
  *Fallback:* the docs require a UCRT toolchain on Windows immediately. An
  ABI fix (a library-side allocator, or a `free_fn` passed alongside the
  buffer) becomes its own plan, because it changes `net_rpc.h`.
- **Two-node loopback programs flake on CI runners.** *Fallback:* the
  same retry-free, bounded-timeout shape the skill runner uses. A flake is
  a defect to fix, not to retry.
- **Windows CI cost.** *Fallback:* the Windows leg runs on changes to
  `include/**`, `src/ffi/**`, `bindings/go/*-ffi/**` and the C programs,
  using the existing path filter.
- **C1's first run finds more than this plan can fix.** *Fallback:*
  blockers (a declared but unexported function, an unsafe constant clash)
  are fixed in C1. Everything else is recorded under "Defects found on the
  way" with an owner.

## Not in scope

- New C surfaces: A2A, payments, or anything else marked `not exposed`.
- C++ wrappers, pkg-config or CMake packaging.
- macOS: no CI runner is assumed. The bundle script supports `.dylib`, but
  no macOS evidence is claimed.
- Replacing the deleted meshdb, meshos, deck and scheduler examples with
  full programs. C5b records them as unexercised.
- The Go binding, which has its own plan, and the browser/wasm surfaces.

## Defects found on the way

Known before C1 runs:

- **The crate's C examples are stale** (gap 5). Fixed by C2–C4 under D2.
- **The shipped export set is unchecked** (gap 2). Fixed by C0.
- **The Windows `free(3)` pairing is unverified** (gap 6). Settled by C5.
