# C SDK — verify the shipped header/library pair from a real consumer

## Status

In progress, 2026-10-04: C0 through C6 have landed (each slice's status
block below says how); C3b waits on #1167 and C7 is optional. Targets the
release after 0.39. Branch `LZL0/c-consumer-plan`. Follows
[`GO_BINDING_CONSOLIDATION_AND_BLOBS_PLAN.md`](GO_BINDING_CONSOLIDATION_AND_BLOBS_PLAN.md)
(merged as #1165). That plan reached the new C surfaces (trees, ranges,
repair, the blob registry, Go-implemented adapters) only through cgo, from
Go. This plan checks them from C.

**Reviewed twice, revised after each.** The first review (HOLD at
`8192253`) kept the direction (a default-feature bundle and real C
programs) and held on seven findings, R1–R7. The largest: the
cross-allocator repair this plan proposed already exists, so C4 and C5 now
test the existing contract instead of predicting its failure. The second
(HOLD at `491237b`) kept those corrections and held on two execution
contracts, S1–S2, plus one C4 wording fix: which library actually loaded,
and how `repair.c` gets declarations for the helper functions. Every
finding was verified against the source before revising; the ledger under
"Review" maps each one to its change.

**Prerequisite:** #1167 (channel-bound write tokens) must be merged before
C3b. At this branch's base, `net_cortex.h` still declares the four-argument
waits.

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
   passes `--features net-ffi/test-helpers` (`ci.yml:4749`) and puts it in
   the ordinary `target/release`. The export baseline
   (`net/crates/net/bindings/go/net-ffi/exports.baseline`) is generated from that build,
   so it includes test-only seams such as
   `net_mesh_blob_adapter_test_drop_data_chunk` and
   `net_blob_test_barrier_*`. Nothing checks the export set of the library
   a consumer actually gets.
3. **The header/source checks that exist are partial.**
   - `check-ffi-exports.py` compares the built DLL with the baseline.
   - The Go header-parity tests compare `net.h` with `net.go.h`, and the
     cortex pair with each other.
   - `check-rpc-abi-parity.py` (run in CI, `ci.yml:4785`) compares
     ordered argument shapes and returns for nRPC, org, MeshOS and
     compute. Its parser is coarse: `uint8_t*` and `uint8_t**` both
     shape to `ptr:uint8_t`, and a callback typedef is compared by name,
     not by its expanded prototype.
   - Nothing covers the other surfaces, the safe `extern "C"` exports
     (`net_rpc_set_callback_free`, `net_ffi_abi_version`), or the
     umbrella `net-ffi/src/lib.rs`.
   - **Returned codes are not declared.** Comment-stripped, no shipped
     header declares `NET_ERR_BLOB_INVALID_ARGUMENT`,
     `NET_ERR_BLOB_BACKEND`, `NET_ERR_WRONG_ORIGIN` or
     `NET_ERR_FEATURE_NOT_BUILT`, though functions document returning
     them. A C caller can only compare against magic numbers.
   - #1167 nearly shipped `NET_ERR_WRONG_CHANNEL = -109` on top of
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
   - callback adapters (`net_blob_register_callback_adapter_owned`);
   - nRPC handlers, and their registered-release contract (below);
   - write tokens.
5. **The crate's own C examples are stale and unbuilt.** None of the eight
   `net/crates/net/examples/*.c` files is referenced by any workflow:
   - they include headers by relative path into the source tree
     (`#include "../include/net.go.h"`);
   - `transport.c` tells the reader to build `net-mesh` and link
     `-lnet_mesh`, which hasn't existed since the single-cdylib change;
   - `transport.c` shows node bring-up only "in outline".
6. **C has never been compiled on Windows in CI**, and the callback
   allocator contract has never run from C.
   - The only Windows job is the Rust security suite (`ci.yml:6091`). The
     docs give MSVC and MinGW link recipes
     (`headers-and-linking.md:80-113`), but nothing runs them.
   - Callback buffers already go back through a consumer-registered
     release: `net_rpc_set_callback_free` (`net_rpc.h:280-305`) and
     `net_org_set_callback_free`. Rust calls it for success responses and
     error strings (`rpc-ffi/src/lib.rs:542-559`, `:682-710`), has no
     `libc::free` fallback, and refuses dispatcher registration without it
     on every platform (`:627-635`).
   - The headers contradict that. `net_rpc.h:266-269` still says Rust
     frees the buffer with `free(3)`, and the dispatcher's comment says
     the refusal is Windows-only. `net_org.h` carries similar stale text.
7. **ABI versioning is uneven, and the token break has no compatibility
   check.**
   - Only `net_rpc.h` and `net_org.h` carry an ABI-version macro with a
     runtime check. `net_ffi_abi_version` documents the bundle's
     composition, not a signature generation.
   - The v0.39.0 `net_cortex.h` declares neither token wait. A diff
     against that release alone sees the #1167 waits as additions, and
     can't see that they changed from four arguments to five in between.

## The design

### The artifact: a C SDK bundle

The consumer builds against a bundle, not the source tree:

```
net-c-sdk-<version>-<target>/
  include/     the eleven headers, copied from net/crates/net/include
  lib/         libnet.so | libnet.dylib | net.dll + net.dll.lib
  EXPORTS      the shipped export set, one symbol per line
  PROVENANCE   commit, cargo command, feature set, toolchain, checksums
```

The bundle is made by one script, `.github/scripts/make-c-bundle.py`,
from a **default-feature** `cargo build --release -p net-ffi`, the command
the docs give. It is kept apart from the existing helper build:

- **Separate directories.** `CARGO_TARGET_DIR=target/c-bundle` for the
  build and `target/c-bundle/stage/` for the bundle, so nothing in the
  ordinary `target/release` (where CI's helper library lives) can be
  picked up.
- **Exports come from the staged library**, not from a build directory.
- **Loader settings are sanitized, as setup.** Consumer runs set the
  library path to `bundle/lib` only, and the runner refuses to start if
  another `libnet`/`net.dll` is reachable on `PATH` or `LD_LIBRARY_PATH`.
  That is not proof of what loaded: Windows also searches the
  application's directory and others, and ELF loading follows
  `DT_RPATH`/`DT_RUNPATH`, explicit dependency paths and preloads.
- **`PROVENANCE` tells the two artifacts apart.** It records the exact
  cargo command, the feature set and the library's SHA-256.
- **The loaded library is identified, not assumed.** See "Loaded-library
  identity" below.
- **Windows stages the runtime DLL and its import library.** "Exactly one
  library" means one Net implementation library; `net.dll.lib` is part of
  it, not a second one.

### Loaded-library identity

Every consumer program, production or helper, starts with one identity
check, before it calls any Net function. It is a shared C file the runner
compiles in (`examples/c/support/loaded_module.c`), not a Net API.

1. **Resolve the address the program actually calls.** For each Net
   function the program uses, take the address its imports resolved to:
   - **Linux:** the function's address as the executable sees it, then
     `dladdr` for the module that contains it. An `LD_PRELOAD` or
     interposed definition resolves to the interposer, so it is the
     interposer that gets reported.
   - **Windows:** the function's entry in the executable's import address
     table, then `GetModuleHandleExW` (`FROM_ADDRESS`,
     `UNCHANGED_REFCOUNT`) and `GetModuleFileNameW`. A thunk's address
     isn't used: it belongs to the executable.

   The check never opens the expected library by path and inspects that,
   which would examine the right module whatever the program called.
2. **Report one module.** Every resolved address must fall in a single
   module. The program prints `NET-LOADED-MODULE: <path>`, and fails if
   the addresses span more than one.
3. **Compare with the staged artifact.** The runner resolves that path
   (real path, no links) and requires it to equal the staged library's
   path, and its SHA-256 to equal the one in that bundle's `PROVENANCE`.
   A missing line, a second module, or a mismatch fails the run.

Production and helper bundles are checked separately, each against its
own staged library and `PROVENANCE`.

**Negative controls, benign by construction.** Each must fail the
identity check by name, before any Net call, rather than relying on a
mismatched ABI to crash:
- **Shadowing:** the helper bundle's library (same ABI, different hash)
  placed where the loader prefers it: the application directory on
  Windows, an `RPATH` entry on Linux. The run must fail on the hash.
- **Interposition (Linux):** an `LD_PRELOAD` library defining one Net
  function the program uses. The run must fail with that function
  resolving outside the staged module. The interposed function is never
  called, because the check only takes addresses.

### Building against the bundle

CI builds the bundle on Linux and Windows; every later slice compiles
against it and nothing else. A consumer program may include headers only
by name (`#include "net_transport.h"`), with `-I bundle/include`. The
Windows compile and link commands are written for MSVC and for MinGW-w64
in the runner; the skill runner's GCC/pthread/dl line is Linux-only and
isn't reused there.

**Decision D1: does the bundle become a release asset?**
- **(a)** CI-only, as the reference for what a source build produces.
- **(b)** Also attach it to the GitHub release.

**Decided: (a), CI-only, first.** (b) is the optional slice C7 and is not
a prerequisite for proving the consumer pair. Publishing adds signing,
per-target matrices and a support promise; the checks are worth having
first and don't depend on it.

**Compatibility boundary.** The bundle is a matched pair: a header set and
the one library built with it. Source compatibility is checked by C1.
Runtime mismatch (an old binary loading a library whose same-named function
changed) is not something a commit marker can prevent. This plan's policy
is the matched pair: a breaking change requires callers to rebuild against
the matching headers and library, and the release notes say so. A runtime
handshake would protect only binaries that call it, so it can't
retroactively cover existing ones; it is out of scope here.

### The audit tool

`.github/scripts/check-c-abi.py`, with `--self-test`, as house style
requires. It **extends `check-rpc-abi-parity.py`'s parser** rather than
starting over: one parser, widened to every surface, with the
pointer-depth collapse fixed, and the existing four-surface check kept as
a caller of it. It reads the bundle and the Rust sources (`src/ffi/**`,
the seven `*-ffi` crates and `net-ffi/src/lib.rs`, both `unsafe extern
"C"` and safe `extern "C"` exports), and checks:

1. **Declared ⇒ exported, per build profile.** Each declaration is
   classified under the bundle's actual feature set:
   - exported with its real body;
   - exported as a stub (the `blob_stubs` pattern), listed as a stub;
   - not compiled in this profile. A header declaration in that class is
     an error unless the header gates it the same way.

   A stub satisfies symbol presence but provides no behaviour, so this
   check does not prove a feature works. That proof is a consumer program
   succeeding (C2–C4), or an explicit capability contract.
2. **Exported ⇒ declared.** Every exported `net_*` symbol is declared in
   some shipped header, or is on a short allowlist with a reason for each
   entry (for example, symbols only the Go mirror headers declare).
3. **Exact signatures.** For each declared function and its Rust
   definition: the ordered argument types, including width, pointer depth
   and constness where it affects the ABI, and the return type. Callback
   parameters are compared by their expanded prototypes, not by typedef
   name.
4. **Value-type layout, compiled.** A text parser can't prove struct
   layout, so this check is a C program: `abi_layout.c` asserts
   `sizeof`, `_Alignof` and `offsetof` of every public by-value struct
   and vtable (`net_blob_adapter_vtable_t`, the owned-registration
   struct, and the rest the tool lists) against values a Rust test emits
   from the `#[repr(C)]` definitions into a generated header.
5. **Constants are complete and correct.**
   - Every `NET_*` code a function returns or its comment documents is
     declared in a shipped header, with Rust's value.
   - `abi_constants.c` compiles `_Static_assert(NET_ERR_X == <rust>)` for
     each one, from the bundle headers.
   - A constant redeclared with the identical value is fine. Two distinct
     meanings with one value in a shared return domain (the −109 case)
     fail, unless the pair is on an exception list with a reason.
6. **No test seams ship.** The default bundle exports nothing matching the
   test-seam patterns (`_test_`, `net_blob_test_barrier_*`).
7. **Compatibility against two baselines.**
   - Tag `v0.39.0`'s headers, the last release.
   - A pinned pre-#1167 `net_cortex.h` fixture
     (`tests/c_abi/fixtures/net_cortex.pre1167.h`), so the four-to-five
     argument token change is seen as a change, not an addition.

   A removed declaration or a changed signature fails unless it matches
   an entry in `tests/c_abi/breaks.toml`. Each entry names one symbol or
   type, its old and new signature, and the release it ships in. One
   authorized break excuses only itself. #1167's commits use different
   wording, so no commit-message trailer is assumed or read.

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

**Each program asserts the contract its functions actually have.** There
is no universal failure-output rule. For example,
`net_mesh_blob_adapter_fetch_range` refuses a mixed-null output pair
without writing either slot (`src/ffi/blob.rs:1977-1985`), and
`net_mesh_public_key_hex` returns before touching its outputs
(`src/ffi/mesh.rs:920-934`). Each check cites its function's contract. A
program never dereferences invalid output storage, and if an old function
needs a stronger contract, that is a recorded repair, not a test that
quietly changes behaviour.

**Decision D2: what happens to the eight stale `examples/*.c`?**
**Decided: a per-topic ledger, then delete.** In the slice that lands
their replacements, this plan records, for each stale file's topic
(meshdb, meshos, deck, scheduler, transport, …):
- the API the headers make available;
- the C-runtime evidence, if any (a program that runs in CI);
- the documentation gap that remains.

Deleting an unverified example doesn't change what's implemented, and a
retired topic stays named in the ledger, not dropped.

**Decision D3: how does the repair program break a shard?** The plan's
first answer (find a chunk file through
`net_mesh_blob_adapter_tree_node_cache_stats` / `net_blob_ref_describe`)
can't work: the stats are hits, misses, bytes and entries
(`src/ffi/blob.rs:2152-2191`), and describe gives top-level ref fields,
the tree root hash and depth, and the encoding (`:2195-2251`). Neither
lists leaf data-shard hashes or paths, and the root is not the shard to
delete.

- **(a)** A disk fixture: known distinct chunk contents and a test-side
  map of the storage layout.
- **(b)** A separate test-helpers bundle for `repair.c` alone, using
  `net_mesh_blob_adapter_test_drop_data_chunk` and `_chunk_present`
  (`src/ffi/blob.rs:2275`, `:2362`).

**Decided: (b).** The on-disk layout isn't a supported contract, so (a)
would make a production-bundle claim rest on an internal detail. (b) is
labelled as helper-build evidence everywhere it's cited. Every other
consumer stays on the production bundle, and repair is never marked as
executed against the default artifact. No production inspection or
deletion surface is added to save the demo's shape.

**The helper bundle needs its own header.** Neither function is declared
in any shipped header. The Go witness declares them locally
(`go/blob_repair_testhelpers.go:22-36`), but consumer files may not carry
prototypes, so that route is closed to `repair.c`. Changing the feature
set alone supplies symbols, not declarations. So:
- **`net_test_helpers.h`** declares exactly those two functions. It lives
  outside the shipped header directory
  (`net/crates/net/tests/c_abi/helpers/`), so the production header count
  and its source of truth are unchanged.
- **Only the helper bundle stages it.** That bundle is built with
  `--features net-ffi/test-helpers` in its own target directory, and has
  its own inventory (the eleven headers plus `net_test_helpers.h`), its
  own `EXPORTS` and `PROVENANCE`, and its own identity check.
- **`repair.c` includes it by name**, like any other header.
- **C1 checks it.** Under the helper profile, its declarations are
  compared with the gated exports by the same exact-signature check.
  Under the production profile, the header and both symbols must be
  absent.

## The slices

### C0: the bundle

`make-c-bundle.py` plus a CI step on `ubuntu-latest` and `windows-latest`
that builds the default `net-ffi` library in its own target directory and
stages the bundle.

**Proves it:**
- The bundle has eleven headers (cross-checked by `check-header-count.py`)
  and one Net implementation library (plus its import library on
  Windows).
- `EXPORTS` is extracted from the staged library, then pinned as
  `net/crates/net/bindings/go/net-ffi/exports.shipped.baseline`, so a change to the
  shipped surface shows in review. Its header records the default-feature
  recipe; the update tool's provenance text, which today describes the
  helper build, is changed to match whichever baseline it writes.
- The runner refuses to start if another Net library is reachable on the
  loader path. That is setup; the proof is the next item.
- The loaded-library identity check passes for a trivial program against
  the production bundle, and both negative controls fail it with their
  named reasons.
- The helper bundle is staged separately with its own inventory and
  identity check. The production bundle's eleven-header and no-test-seam
  assertions are unchanged; the helper bundle is the one documented
  exception.
- The existing test-helpers baseline stays as it is.

**Status: built, verified on Windows, Linux pending CI (2026-10-04).**
- **What landed:**
  - `make-c-bundle.py` (`--profile production|helper`, `--self-test`:
    13 planted cases);
  - `run-c-consumers.py` (`--self-test`: 10 cases);
  - `examples/c/support/loaded_module.{c,h}`, the identity check;
  - `examples/c/smoke.c`;
  - `tests/c_abi/helpers/net_test_helpers.h`;
  - `exports.shipped.baseline`;
  - the `c-consumers` CI job (ubuntu, windows).
  - `check-ffi-exports.py` gained `--build`, so a baseline records the
    recipe that built it. It also no longer crashes on a relative
    `--baseline`.
  - `check-script-permissions.py` treats `"$PY"` (the job's per-runner
    interpreter: `python3` isn't on every Windows image's PATH) as an
    interpreter, with a self-test case that fails without the rule.
- **Bundles on Windows:**
  - production: 11 headers, `net.dll` + `net.dll.lib`, 593 exports, no
    seams;
  - helper: 12 headers, 601 exports.
  - The helper set minus the shipped set is exactly the eight test seams:
    `net_blob_test_barrier_{arm,release,wait_held}`,
    `net_compute_test_inject_synthetic_peer{,_with_tags}`,
    `net_mesh_blob_adapter_test_{chunk_present,drop_data_chunk}` and
    `net_mesh_test_send_refusal_attribution`. Nothing is shipped-only.
- **Runs on Windows:** `smoke.c` under MSVC `/W4 /WX /MD` and MinGW-w64
  UCRT (GCC 16.1, `-Werror`), against both bundles. Each loaded its own
  staged `net.dll` with a matching SHA-256. In every combination, the
  shadowing control (the other bundle's `net.dll` beside the executable)
  was refused by name before any Net call.
- **Not yet run anywhere:** the ELF identity path and the `LD_PRELOAD`
  control. This box has no Linux environment; the ubuntu leg of
  `c-consumers` is their first run.
- **Found:** `net_ffi_abi_version` is exported (`net-ffi/src/lib.rs:52`)
  but declared in no shipped header. It is the first entry for C1's
  exported ⇒ declared check.

### C1: the audit

`check-c-abi.py` runs checks 1–7 against the C0 bundle, in CI next to
`check-ffi-exports.py`. `abi_layout.c` and `abi_constants.c` compile from
the bundle on both platforms.

**Proves it:**
- `--self-test` plants one defect per check and requires each to be
  reported:
  - a declared but unexported function, and a stub reported as a stub;
  - an exported but undeclared function;
  - same-arity drift: a swapped argument order, a changed width
    (`uint32_t` to `uint64_t`), and a changed pointer depth (`uint8_t*`
    to `uint8_t**`);
  - a callback typedef whose expanded prototype changes under the same
    name;
  - a struct field change (caught by `abi_layout.c` failing to compile);
  - a returned code with no declaration, and a declared value that
    differs from Rust;
  - two distinct meanings sharing a value in one return domain;
  - a test seam in the shipped export set;
  - an authorized break in `breaks.toml` combined with an unrelated
    removed symbol: the removal must still fail.
- The first run on the real tree is recorded here with every finding.
  Each finding is fixed in this slice, or allowlisted with a reason, or
  deferred under "Defects found on the way". The four undeclared codes
  in gap 3 are expected findings and are declared in this slice.

**Status: stage 1 of 4 landed (2026-10-04): checks 1, 2, 3, 6 and the
handle rule.** Stages 2–4 are constants (check 5, with
`abi_constants.c`), layout (check 4, with `abi_layout.c`) and
compatibility (check 7).
- **What landed:**
  - `c_abi_model.py`: the type model. C goes through pycparser on `cc -E`
    output with stand-in libc headers. Rust is lexed with comments and
    strings masked, and a definition counts as compiled if its own
    `#[cfg]`s, its inline modules' and its file's (including inner
    `#![cfg]`) all hold under the features `cargo tree` resolves for the
    profile.
  - `check-c-abi.py`: audits a staged bundle. `--self-test` covers 13
    cases: same-arity swapped handles, a narrowed width, a dropped
    pointer level, a callback prototype and a callback arity changed
    under one typedef name, a stale allowlist entry, and an unknown cfg
    predicate.
  - `tests/c_abi/allowlist.toml`.
  - The CI step on the ubuntu leg of `c-consumers`.
- **Opaque handles have no alias table.** Each C type name must
  correspond to one Rust type across every function. The first run
  matched 80 C types one-to-one. 42 positions meet a `void*` on one side
  (the event-bus API's untyped handle; the transport stubs). They are
  ABI-identical, are listed by `--verbose`, and are not handle-checked.
- **Proved on the real headers, not just the self-test:** a copy of the
  production bundle with four planted changes in `net.go.h`,
  `net_cortex.h` and `net_transport.h`. A narrowed width, a dropped
  pointer level, two reordered `net_tasks_wait_for_token` arguments and
  swapped `net_serve_blob_transfer` handles were each reported, against
  the real bodies and their stubs.
- **First run, production (Windows):** 583 declared functions, all
  exported and all backed by exactly one compiled, non-stub definition.
  No signature or handle findings. Ten exports were declared nowhere:
  - **Declared now** in `net_rpc.h` (documented to users, called by Go
    through local preamble declarations): `net_rpc_watch_tools`,
    `_next`, `_close`, `_free`, `net_rpc_metrics_snapshot` and
    `net_rpc_observer_dropped_total`. Each now passes this audit and
    `check-rpc-abi-parity.py`.
  - **Allowlisted, with reasons:** `net_ffi_abi_version` (bundle
    composition, not a signature generation; C0 answers which library
    loaded) and `net_blob_register_callback_adapter` (the non-owning
    predecessor of the declared `_owned` form).
  - **Deferred to stage 3:** `net_rpc_set_observer_dispatcher` and
    `net_rpc_observer_install`. Their dispatcher takes `RpcCallEventC`,
    which would have to be published and layout-checked first.
- **After the fixes:** production has 589 declared functions, all real
  and matching. Helper has 591 (adding the two repair seams from
  `net_test_helpers.h`), with six undeclared Go-test seams listed by
  name.

**Stage 2 landed (2026-10-04): constants (check 5).**
- **What it checks:**
  - Every `NET_*` const in the Rust FFI (`src/ffi/**` and the `*-ffi`
    crates) is declared in a shipped header, with the Rust value.
  - A name declared twice has one value.
  - Every complete `NET_*` name a header comment mentions is declared.
    A family written `NET_ERR_BLOB_*` is a prefix, not a name.
  - No two codes share a value within one return domain. All of
    `NET_ERR_*` is one domain (`net_error_t`, which every surface
    returning `NetError` draws from); `NET_RPC_*`, `NET_ORG_*` and so on
    are each their own.
- **Compiled, not only parsed:** the audit generates
  `_Static_assert(NAME == value)` for every declared constant with a Rust
  value and compiles it from the bundle's headers. There is one unit for
  `net.h` and one for `net.go.h`, since they share a guard.
- **`--self-test`** adds five cases: an undeclared Rust code, a value
  mismatch, the same mismatch failing the compiled assertions, a comment
  naming an undeclared code, and a collision.
- **Proved on the real headers:** a copy of the bundle with
  `NET_ERR_QUEUE_FULL` changed to -109 was refused three ways: the value
  mismatch, the failed `_Static_assert`, and a collision with
  `NET_ERR_MESH_STREAM_OCCUPIED`. That collision is #1167's -109 incident,
  now caught by a tool rather than a reviewer.
- **First run:**
  - 30 Rust codes were declared nowhere. That covers the whole
    `NET_ERR_BLOB_*` band, the cortex read-your-writes codes,
    `NET_ERR_GANG_INVALID`, four registry codes and the observer's
    status/direction values.
  - Seven comment mentions named undeclared codes; six were codes the
    headers told callers to compare against (`NET_ERR_WRONG_ORIGIN`,
    `NET_ERR_QUEUE_FULL`, …).
  - Ten collisions: the blob band against mesh codes.
- **Fixed in the same commit, under the one-commit ABI rule:**
  - **Headers:**
    - `net.go.h` / `go/net.h`: the 12 blob codes,
      `NET_ERR_GANG_INVALID` and `NET_ERR_FEATURE_NOT_BUILT`.
    - `net_cortex.h` / `go/net_cortex.h`: `NET_ERR_TIMEOUT`,
      `_STREAM_ENDED`, `_WRONG_ORIGIN`, `_QUEUE_FULL`, `_FOLD_STOPPED`,
      `_PANIC` and `_FEATURE_NOT_BUILT`.
    - `net_transport.h`: `_FEATURE_NOT_BUILT`, which the transport stubs
      return.
    - `net.h`: registry codes 8–11.
    - `NET_ERR_FEATURE_NOT_BUILT` is `#ifndef`-guarded in its three
      headers; the three compile together.
  - **Rust:** a note at each code group names its header.
  - **Go:** the new `TestABIStabilityHeadersDeclareReturnedCodes` pins
    each declared value to Rust, and the blob band to its Go sentinel.
    `TestABIStabilityTransferCodes` pins the new transport declaration,
    and its stale "-107 lives in net.h" note is gone.
- **Allowlisted, with reasons:**
  - The ten blob/mesh pairs, as the per-surface decision of `ae651ef9b`.
    Any other collision fails.
  - `NET_ERR_DIR_SYMLINK_UNSUPPORTED`, a reserved name.
  - The six observer constants, deferred with the observer to stage 3.
- **Verified on Windows:** both bundles audit clean; 18/18 self-test;
  `smoke.c` under MSVC and MinGW; 18 Go ABI and header-parity tests;
  `check-rpc-abi-parity.py` and the header-count, one-library and
  C-snippet guards.

**Stage 3 landed (2026-10-04): layout (check 4).**
- **Two compilers meet at a reviewed fixture,**
  `tests/c_abi/layout.json`. It records size, alignment and every field
  offset of each struct the headers publish with a body.
  - **Rust:** `bindings/go/net-ffi/tests/abi_layout.rs` measures each
    `#[repr(C)]` struct with `size_of`, `align_of` and `offset_of!`, and
    requires the fixture to match. `NET_ABI_LAYOUT_WRITE=1` regenerates
    it.
  - **C:** the audit compiles `_Static_assert`s on `sizeof`, `_Alignof`
    and `offsetof` from the bundle headers.
  - **Coverage:** the audit requires the fixture to cover exactly the
    published structs, and pairs C and Rust fields in order. Field types
    go through the same comparison as signatures, so nested structs join
    the one-to-one handle map.
  - **Which Rust struct goes with which C struct** comes from that map:
    signatures first, then fields of already-paired structs.
    `--emit-layout-table` prints the test's table.
- **Where it runs:** CI runs the Rust test on both legs of `c-consumers`
  (`--release` in the bundle's target directory, reusing its libraries).
- **Plan deviation, recorded:** the fixture is JSON rather than
  generated C. Both sides read the same file, which is what the plan
  required.
- **21 structs, all clean** on the first run, after two audit fixes: array
  fields, and a comment inside a Rust `type` alias.
- **Proved on the real headers:** two swapped `RpcCallEventC` fields in a
  bundle copy were reported as a field-order mismatch, two type
  mismatches and two failed `offsetof` assertions. A narrowed `net_stats_t`
  field was reported by the type comparison alone; its offsets and size
  happen not to move, which is why both methods are kept. A wrong size
  planted in the fixture failed the Rust test.
- **`--self-test`** adds four cases: a published struct with no entry, a
  wrong offset, a changed field width, and reordered fields. 22 in all.
- **The nRPC call observer is now published** in `net_rpc.h`:
  `RpcCallEventC`, `RpcObserverFn`, `net_rpc_set_observer_dispatcher`,
  `net_rpc_observer_install` and the six status/direction constants. The
  eight allowlist entries deferring them are gone. The new Go test
  `TestABIStabilityObserverPreambleMatchesNetRpcHeader` pins Go's own
  preamble copy to the header, all 12 fields and the 6 values, and fails
  when a preamble field is widened.
- **`check-abi-commit.py` refined:** it no longer demands a Go mirror
  header for a change confined to headers that have none (`net_rpc.h`,
  `net_org.h`, …). The Rust, header and Go-ABI-test groups are still
  required. Three self-test cases cover it.
**Stage 4 landed (2026-10-04): compatibility (check 7). C1 is complete.**
- **Two baselines, compared header by header.** A declaration moved to
  another header is gone for a consumer of the first one.
  - **The release tag's headers** (`--baseline-ref`, default `v0.39.0`;
    CI fetches the tag). 581 functions, 172 constants, 19 structs.
  - **Pinned fixtures**, `tests/c_abi/fixtures/<header>.<label>.h`. The
    first is `net_cortex.pre1167.h`, `net_cortex.h` as of `b77886c22`
    (master with #1165, before #1167).
- **The rule:** a removed or changed function, constant or struct fails,
  unless `tests/c_abi/breaks.toml` has one entry with exactly its symbol,
  old form, new form, release and reason. An entry excuses only its own
  change. An entry that matches nothing fails, and so does one without a
  reason. No commit-message marker is read.
- **First run:** clean. Since v0.39.0 this branch has only added
  declarations, and it predates #1167.
- **Proved on a bundle copy:**
  - #1167's change, applied to `net_tasks_wait_for_token`, was reported
    against the pre-#1167 fixture, and only there: v0.39.0 never declared
    the waits.
  - With that change authorized in a scratch `breaks.toml`, an unrelated
    removal of `net_free_string` from `net.h` was still reported.
  - Against the clean bundle, the same entry was reported as stale.
  - An earlier draft compared across all headers and missed the
    `net_free_string` removal, because `net.go.h` still declares it.
    That is why the comparison is per header.
- **`--self-test`** adds ten cases, 32 in all.
- **#1167 needs `breaks.toml` entries when it lands** for both token
  waits. `breaks.toml`'s header carries the entry to add. Until then the
  check is red on any branch carrying #1167, which is the point.

**C1, all four stages.** Every check in the plan's list runs on both
bundles, with self-tests for each, and each was shown to fail on planted
changes to the real headers.
- **What the first runs found and fixed:**
  - six undeclared nRPC functions plus the observer surface;
  - 30 undeclared return codes;
  - a header that failed `-Wall -Werror`;
  - two `check-abi-commit.py` false positives.
- **Still allowlisted, with reasons:**
  - `net_ffi_abi_version` and `net_blob_register_callback_adapter`;
  - the ten blob/mesh value overlaps;
  - one reserved name.

- **A regression this slice caused, and fixed:** stage 2 declared
  `NET_REGISTRY_ERR_*` 8–11 in `net.h` without updating
  `tests/error_kind_mirror.rs`, which keeps its own table of that band and
  rejects an unknown `#define`. CI's net-core integration job caught it.
  The table, and the kebab-case names the bindings already raise
  (`unknown-group`, `scale-rejected`, `scale-not-supported`,
  `unauthorized`), now include them. No other test that reads the headers
  rejects additions: the transport, nRPC and `NetError` header tests, and
  the Go parity tests.
- **Unrelated CI flakes seen on this branch:**
  `mesh_rpc_hedge::hedge_loser_handler_observes_cancellation` (passed on
  retry) and
  `review_a2a_provider::the_owner_outlives_a_running_executor` (a loopback
  `handshake timeout`). Neither touches code changed here.

### C2: lifecycle and transfer programs

- **`lifecycle.c`.** Two in-process nodes go through `net_mesh_new`,
  `_start`, `_accept` / `_connect`, `_shutdown` and `_free`.
  - Shutdown and free are tested separately. After `_shutdown`, the
    program calls operations chosen from the source for having a defined
    post-shutdown result, and checks that result. A getter that stays
    valid after transport stops (an identity accessor, say) is checked as
    still valid, not as refused.
  - Each free function documented as NULL-safe is called with NULL.
  - Repeat-close is tested only on functions that document it. Nothing
    is freed twice where no such guarantee exists.
- **`transfer.c`.**
  - `net_serve_blob_transfer`, publish, `net_fetch_blob` and
    `net_fetch_blob_discovered`.
  - `net_store_dir`, `net_fetch_dir` and `net_dir_manifest_read`, with
    the tree compared byte for byte.
  - Refusals: a path that escapes its root, and an unknown ref.
  - Manifests, as two separate cases: zero bytes (malformed, refused) and
    a valid manifest of an empty directory (accepted, no entries).

**Proves it:**
- Both programs build against the bundle only and run under
  `run-c-consumers.py` (C0), which keeps `run-skill-examples.sh`'s run
  contract: bounded timeouts, no retries. It is Python rather than shell
  because one runner drives GCC, MSVC and MinGW.
- Each assertion is a named check printed on success. CI fails if the
  number of named checks drops below the recorded floor; that's the
  roster pattern.

**Status: landed (2026-10-04); verified on Windows, Linux pending CI.**
- **`lifecycle.c`, 26 checks:**
  - **Refusals from `net_mesh_new`:** NULL, malformed JSON and a zero
    heartbeat (`NET_ERR_INVALID_JSON`), a short PSK and an unparseable
    address (`NET_ERR_MESH_INIT`). None of them produces a handle.
  - **Bring-up:** two nodes, a handshake, start.
  - **Argument checks:** NULL handles.
  - **Shutdown, then the operations with a defined result after it.**
    Shutdown runs inside the handle's guard and only `net_mesh_free`
    closes it, so a second shutdown returns 0 (documented as idempotent in
    `src/ffi/mesh.rs`), and the node id and public key are unchanged.
  - **NULL frees:** `net_free_string(NULL)`, the only free these programs
    use that a header documents as NULL-safe.
  - **Nothing after `net_mesh_free`:** no header documents that as safe,
    whatever the handle tombstone does.
- **`transfer.c`, 55 checks:**
  - **Fetch before the engine is installed:** `ENGINE_NOT_INSTALLED`.
  - **Blob fetch,** from the holder and by discovery, byte for byte.
  - **Unknown address:** `NOT_FOUND` from the holder, `ALL_PEERS_FAILED`
    by discovery. A NULL hash is `NULL_POINTER`.
  - **A directory tree:** a file, a nested 5000-byte file and an empty
    directory. Stored, read as a manifest, fetched; files compared byte
    for byte, and the empty directory survives.
  - **Path escape:** a manifest built in the program as the postcard
    bytes of `DirManifest{1, [Dir "../escape"]}` and published as a plain
    blob is refused with `DIR_PATH_INVALID`, not `INVALID_MANIFEST`. So
    the encoding decoded, and the refusal is the path check. The counters
    read 0, and nothing appears outside the destination.
  - **Unknown ref:** a manifest only the reader stores is `NOT_FOUND` from
    the holder, with `(NULL, 0)` outputs per `net_transport.h`.
  - **The two empty manifests:** a zero-length ref is `INVALID_ARGUMENT`
    (`read_blob_ref` decodes it to no ref); a stored empty directory reads
    as `"entries":[]` and fetches 0 files.
- **Shared support:** `support/consumer_util.{c,h}` holds the named-check
  macros, port reservation, threads, sleep and files (Winsock and Win32
  on Windows, POSIX elsewhere), and node build and handshake. The runner
  compiles every support file and links `ws2_32` on Windows.
- **Floors:** `examples/c/FLOORS` (smoke 2, lifecycle 26, transfer 55).
  The runner fails a run below its floor, a program without a floor, and
  a floor without a program; a self-test case covers the last two.
  Raising transfer's floor to 56 failed the run with "a check stopped
  running".
- **The consumer lint from C1 landed here:** `check-c-abi.py` refuses a
  Net header included by path, a `net_*` prototype, or an `extern` Net
  declaration in any `examples/c` file. There are four self-test cases,
  36 in all, and the current programs are clean.
- **Verified:** MSVC `/W4 /WX` and MinGW `-Werror`, both bundles, every
  program, with the shadowing control. The ubuntu leg of `c-consumers`
  is the first GCC/POSIX build of `consumer_util.c`.

### C3: tree, range and repair programs

- **`tree_range.c`** (production bundle).
  - `net_mesh_blob_adapter_new_v2` and `_store_tree`, in Replicated and
    in Reed-Solomon form.
  - `_fetch_range`, covering every case of its contract:
    - a reversed range;
    - an empty range past the end;
    - a range ending past the blob;
    - the 1 GiB cap, against a ref that claims 2 GiB;
    - an exact sub-range;
    - output storage: valid outputs are initialised on a later failure,
      and a mixed-null pair is refused with both slots untouched.
- **`repair.c`** (helper bundle, D3(b)), including
  `net_test_helpers.h` by name. It keeps the Go plan's S6 shape:
  - the production-sized Reed-Solomon input the Go plan used;
  - before the drop, the stored tree is fully closed (every chunk
    present);
  - after the drop, absence is proved twice: `_chunk_present` says no,
    and a fetch that needs the shard fails;
  - the repair report's restoration counters match exactly;
  - the full range reads back byte for byte afterwards.

**Proves it:** both programs run on Linux and Windows. The cap check uses
the same re-encoded ref as Go's `bigSmallRef`, so both bindings test the
same boundary. Matrix evidence for repair says "helper build".

**Status: landed (2026-10-04); verified on Windows, Linux pending CI.**
- **`tree_range.c`, 61 checks, production bundle.** It mirrors
  `go/blob_tree_test.go` on the same bytes (`cu_distinct_chunks` is Go's
  `distinctChunks`):
  - `new_v2`: a NULL out-pointer is -1, and an unknown option key is -3
    with the handle slot left NULL. Cache stats read `null` without a
    cache and carry counters with one.
  - A 9 MiB replicated tree, described (tree, chunked, size, root and
    depth, no single hash) and read whole and across a chunk boundary. A
    whole-tree `fetch` is refused.
  - A 16 MiB Reed-Solomon(4, 2) tree, described and read whole.
  - Six encoding refusals at -150. The defaults (k = m = 0) are non-zero.
  - The range contract in core's order:
    - reversed is -150;
    - empty succeeds anywhere with `(NULL, 0)`;
    - past the end is -150;
    - Go's 2 GiB ref is refused past the 1 GiB cap and is `NOT_FOUND` at
      exactly 1 GiB;
    - an exact sub-range reads back.
  - **Output storage:** valid outputs read `(NULL, 0)` after a later
    failure, and a mixed-null pair is -1 with the non-NULL slot untouched.
- **Found here, recorded rather than changed:** an empty ref is not -150
  from C. A NULL ref pointer is -1 (checked with the handle), and a real
  empty buffer is `NET_ERR_BLOB_DECODE`. Go's `FetchRange(nil)` returns
  -150 only because `refArg` refuses it before calling in. The two
  bindings report the same case differently, and that's by design.
- **`repair.c`, 34 checks, helper bundle only** (`NET-BUNDLE: helper`; the
  runner skips it on the production bundle and says so). It keeps the Go
  S6 shape on one 16 MiB RS(4, 2) stripe:
  - full closure before the drop;
  - the dropped shard proved absent through `_test_chunk_present`;
  - repair restores exactly one chunk in one stripe, the shard is present
    again, the full range matches, and a second repair finds the stripe
    healthy;
  - three drops (m = 2) make the stripe unrecoverable, counted and not an
    error.
  - The seam refuses a real small ref at -150. Arbitrary bytes would fail
    earlier as undecodable.
- **Plan correction:** the plan said that after one drop, "a fetch that
  would need it fails". It does not: one loss is within RS(4, 2)'s
  tolerance, and a whole read reconstructs from parity, byte for byte.
  `repair.c` asserts that degraded read, and proves absence through the
  seam. A read that must fail is checked where it really fails, after
  three drops, with `(NULL, 0)` outputs.
- **Floors:** tree_range 61, repair 34.
- **Verified:** every program, both bundles, MSVC and MinGW, with the
  shadowing control.

### C3b: write tokens (after #1167)

- **`write_tokens.c`** (production bundle), against the five-argument
  waits. Tasks and Memories adapters over one Redex with the same origin:
  - both `net_{tasks,memories}_channel_hash` getters, and that the two
    hashes differ;
  - tokens built from the adapter's own origin, channel and the seq a
    write returned;
  - same-channel progress: a timed wait succeeds and the write is
    visible;
  - a zero-timeout poll, before and after the fold applies;
  - same-origin cross-channel refusal: Memories writes past the Tasks
    seq, and both a poll and a timed wait of the Tasks token on Memories
    return `NET_ERR_WRONG_CHANNEL`, by name from the header;
  - a token from another origin returns `NET_ERR_WRONG_ORIGIN`.

**Proves it:** the program runs on both platforms. C1's check 7 reports
the wait change against the pre-#1167 fixture, and `breaks.toml` carries
exactly that entry. The C docs state that callers built against the old
headers must rebuild.

### C4: callback programs

- **`blob_callbacks.c`.** A C `net_blob_adapter_vtable_t` backed by an
  in-memory map, registered with `net_blob_register_callback_adapter_owned`
  and driven through `net_blob_publish` / `net_blob_resolve`. Its buffers
  come from `malloc` and go back through its own `free_buffer`. It
  asserts that:
  - `release_fn` runs exactly once, after unregister;
  - a duplicate-id registration never calls `release_fn` and leaves the
    context with the caller;
  - a callback's error code surfaces as the public code it maps to,
    which is not always the same one (`code_to_err`,
    `src/ffi/blob.rs:697-707`, then `err_to_code`, `:141-160`). The
    witness drives these pairs, by name:

    | Callback returns | Caller sees |
    | --- | --- |
    | `NET_ERR_BLOB_NOT_FOUND` | `NET_ERR_BLOB_NOT_FOUND` |
    | `NET_ERR_BLOB_HASH_MISMATCH` | `NET_ERR_BLOB_BACKEND` |
    | an unlisted code | `NET_ERR_BLOB_BACKEND` |

    The table records current behaviour; no native change is made.
- **`rpc_callbacks.c`**, written against the existing contract:
  - it calls `net_rpc_set_callback_free` with its own counting
    deallocator before any dispatcher registration;
  - a handler returns a nonempty `malloc`'d response, and another returns
    an allocated error string;
  - the release count for each matches the number returned, and nothing
    is released twice.
- **`rpc_no_free.c`**, in a fresh process because registration is
  first-call-wins: dispatcher registration without a deallocator returns
  `-1`, on both platforms.
- **Header fix.** `net_rpc.h:266-269` and the dispatcher comment, and the
  matching text in `net_org.h`, are rewritten to the registered-release
  contract and the all-platform refusal.

**Proves it:** the release counts above. The barrier-held cases stay in
Go; they need the test seams, and the shipped bundle has none.

**Status: landed (2026-10-04); verified on Windows, Linux pending CI.**
- **`blob_callbacks.c`, 31 checks.** A C adapter backed by an in-memory
  map, with every buffer from this program's malloc and back through its
  own `free_buffer`.
  - Refused registrations never run `release_fn`, and leave the id
    unregistered: a NULL vtable or `release_fn` (-1), a NULL entry
    (-115), and a duplicate id (-111, checked on the second context).
  - Publish and resolve go through the C callbacks. Five fetches hand out
    five buffers, and all five come back.
  - The error pairs: `NOT_FOUND` stays `NOT_FOUND`; `HASH_MISMATCH` and
    an unlisted -999 both become `BACKEND`.
  - `release_fn` runs exactly once, after unregister and not before; a
    second unregister is 0. Resolving a real ref afterwards is
    `NOT_REGISTERED`.
- **`rpc_callbacks.c`, 26 checks.** The counting deallocator is registered
  first (NULL is refused at -1), then the dispatcher. Two handlers are
  served on one node and called from another: three echo responses and
  one error string, all malloc'd. The caller sees the handler's error
  text. All four buffers are released exactly once through the registered
  deallocator, and nothing is released that was not handed out.
- **`rpc_no_free.c`, 4 checks,** in its own process because registration
  is first-call-wins. The dispatcher is refused (-1) without a
  deallocator, and stays refused after a NULL one.
- **Header fix:** `net_rpc.h`'s "Rust frees with `free(3)`" and its
  "On Windows, returns -1" are rewritten to the registered-release
  contract and the all-platform refusal; `net_org.h`'s two counterparts
  likewise. Both libraries refuse on every platform (no `cfg(windows)`
  in either). `check-callback-buffer-ownership.py` still passes.
- **Floors:** blob_callbacks 31, rpc_callbacks 26, rpc_no_free 4.
- **Verified:** all eight programs, both bundles, MSVC and MinGW, with the
  shadowing control. `support/consumer_util` gained a portable mutex for
  state the callbacks share with worker threads.

### C5: compatibility lanes and memory-checker lanes

Two kinds of lane, kept apart. A compatibility lane shows the programs
build, link and pass. A checker lane names its checker and the allocation
domain it can see, and claims nothing outside that domain.

**Compatibility lanes** (every C2–C4 program):
- Linux GCC.
- Windows MSVC `/MD` (UCRT).
- Windows MinGW-w64 UCRT.
- Windows MinGW-w64 MSVCRT, measured, not predicted. With registered
  release, each buffer is freed by the allocator that made it, so this
  lane is expected to pass; if it doesn't, the failure is a finding with
  its own evidence. Earlier Application Verifier evidence was for the old
  Go/Org pairing and is not a reproduction against this program or this
  code.

**Checker lanes:**

| Lane | Checker | Domain it can see | Not claimed |
| --- | --- | --- | --- |
| Linux | ASan + UBSan on the C program, LSan | consumer memory access; library-returned buffers, through the intercepted system allocator Rust uses | accesses inside Rust code, which isn't instrumented |
| Windows | Application Verifier with full PageHeap, on the consumer and the release `net.dll` | heap corruption and double frees across the module boundary (wrong-heap frees only where the heaps differ; see C5's status) | leaks |
| Windows | consumer built `/MDd`, `_CrtSetDbgFlag` leak and heap checks | the consumer's own CRT allocations | anything the release DLL allocates |

`_CrtSetDbgFlag` is compiled out without `_DEBUG`, which `/MD` doesn't
define, so it is only used in the `/MDd` lane and only for that lane's
domain. MinGW lanes are compatibility-only unless a checker is added with
its own domain claim. No library-returned-buffer leak result is claimed
on Windows.

**Arming negatives.** Each checker lane must catch a planted defect at the
boundary it claims. Each runs as a disposable child, and the runner
requires both a failing result and the named diagnostic, not just any
non-zero exit. A crash during setup, or a leak of a buffer the C program
allocated itself, doesn't count.
- **Leak (Linux):** a real, nonempty `net_fetch_blob` output with its
  `net_transport_free_buffer` call omitted. LSan must report the leak
  with an allocation stack through `net_fetch_blob`.
- **Double free (Linux and AppVerifier):** that output's matching free
  called twice. ASan must report `attempting double-free`, and
  AppVerifier its heap stop.
- **Callback buffers:** a consumer-owned response freed by the consumer
  as well as through the registered release, caught by the lane's
  checker.

**Retained allocations.** Some allocations are kept on purpose, and are
reported separately, never hidden by a blanket `libnet` suppression:
- `net_mesh_free` keeps the outer handle box as a tombstone while
  dropping its inner resources (`src/ffi/mesh.rs:799-822`);
- the blob FFI's process-global runtime.

Each class gets one suppression keyed by its own allocation stack, and
the lane prints the count of suppressed blocks per class. Being reachable
from a static doesn't show teardown reclaims it; nothing here claims that
it does.

**Proves it:**
- Clean runs in each checker lane, within its stated domain.
- Each arming negative fails with its named diagnostic.

**Status: all four lanes built (2026-10-04).** The Application Verifier
lane needs an elevated process, so it is proved in CI, not on the dev box.
- **Linux sanitizer lane (`--sanitize`).** Every program again, built with
  ASan and UBSan with no error recovery, and LeakSanitizer at exit, against
  the uninstrumented `libnet.so`.
  - **Arming, all three caught on the first CI run:**
    - a leaked `net_fetch_blob` buffer, matched by its odd 4093-byte size
      so the report is that buffer's;
    - a double `net_transport_free_buffer`;
    - a callback buffer freed by the program after the library already
      released it through `free_buffer`.
  - **Leak accounting is a classifier, not suppressions.** LSan
    suppressions match any frame of a stack, and the handle tombstones
    share `net_mesh_new` with the defect below, so a suppression could not
    tell them apart. The runner parses every leak block and judges it
    against `tests/c_abi/lsan_policy.toml`:
    - direct blocks of 64 bytes or less from a handle constructor are
      handle tombstones (the intended keep-the-outer-box pattern of
      `net_mesh_free`, `net_redex_free` and `net_mesh_blob_adapter_free`);
    - indirect blocks were accepted only in the programs the open
      defect D-C5-1 named; since its fix, none are.
    - Anything else fails, and every accepted class is printed with its
      blocks and bytes on every run.
  - **Replayed against the first run's real reports:**
    - `transfer` and `tree_range`: only tombstones (16 to 24 bytes per
      handle);
    - `lifecycle`: tombstones plus the defect, 508 KB in 482 blocks
      (second run: 552 KB in 517);
    - `rpc_callbacks`: tombstones plus the defect, 236 KB in 259 blocks
      (second run: 557 KB in 367). The defect's size varies between runs,
      so the policy bounds the programs it may appear in, not its bytes.
  - **Correction, found by the second run:** a leak-only report was never
    judged at all. LSan's summary line reads `SUMMARY: AddressSanitizer:
    N byte(s) leaked`, and the gate excluded any output naming
    AddressSanitizer. It now recognises an ASan error by its `ERROR:` line
    (`leak_only`, with a self-test). The first replay had exercised the
    classifier and not this gate.
  - **Self-tests:** the parser and the policy, including "a leaked
    library-returned buffer is never an accepted class".
- **Windows debug CRT lane (`--debug-crt`, MSVC `/MDd`).**
  - **What it does:** the identity check enables the leak check at exit
    and heap validation every 128 allocations, with all reports on stdout
    (never a dialog).
  - **Domain:** the program's own CRT heap only.
  - **Arming:** a leaked 777-byte block, and a write past an allocation's
    end. Both are caught.
  - **Correction:** the debug CRT dumps leaks without changing the exit
    status, so for this lane the runner's verdict is the failing result.
  - **First run:** a real leak in `rpc_callbacks.c` (its mutex, 40 bytes).
    Fixed. All eight programs are clean on both bundles.
- **Pinned MinGW-w64 lanes.** `ucrt64` (UCRT) and `mingw64` (msvcrt), from
  MSYS2, replace the runner image's unpinned gcc. `--expect-crt` reads each
  binary's import table and fails a lane whose binaries do not link the
  runtime it claims. Locally, the UCRT gcc passes as `ucrt` and is refused
  as `msvcrt`. The MSVCRT lane is measured, not predicted.
- **Application Verifier lane (`--appverif`, MSVC `/MD`).**
  - **What it does:** each program runs with full PageHeap and the
    Handles, Locks and Memory layers (not Leak: the lane claims no leak
    result). The settings are per image name and machine-wide, so the
    runner enables them for one run and always removes them. It exports
    the verifier's log, and a run with no log fails, so a run that
    silently was not verified cannot pass. Each error entry becomes an
    `APPVERIFIER STOP` line, a checker report.
  - **Arming:** `double_free_fetch_blob.c` and
    `callback_buffer_double_release.c` arm this lane as well as the
    sanitizer lane (`NET-LANE: sanitize appverif`, with a per-lane
    `NET-EXPECT(appverif):`). Each must produce a heap stop.
  - **First CI run (37192704417):** every production program ran clean
    under full PageHeap, each with a verifier log. Arming caught
    `callback_buffer_double_release` (stop 0x7, "Heap block already
    freed"). Two runner defects showed, both fixed:
    - the expectation was anchored with `^`, which the runner's search
      only matches at the start of the output;
    - `double_free_fetch_blob.c` and `leak_fetch_blob.c` never called
      `cu_net_init`, so on Windows (no WSAStartup) their node setup
      failed. They had only run on Linux before.
  - **Correction to the lane table: no wrong-heap claim between the
    release UCRT and `net.dll`.** Measured: with `/MD`, the UCRT's
    `_get_heap_handle()` is the process heap, and Rust's `System`
    allocator on Windows also allocates from `GetProcessHeap()`. So a
    program that `free()`s a library buffer frees it on the heap that
    made it. That is a contract breach (the headers name the free
    function), but no heap check can see it, and this lane does not claim
    it. A real cross-heap free (a `/MDd` program, or a static CRT with its
    own heap) remains a stop the verifier can report.

### C5b: unexercised surfaces (record only)

The audit (C1) covers all eleven headers, but C2–C4 exercise only
transport, blobs, the registry, callbacks, nRPC and (C3b) write tokens.
For the rest (`net_meshdb.h`, `net_meshos.h`, `net_deck.h`, `net_mcp.h`,
`net_subnet.h`, and the cortex surface other than write tokens), C5b
records, per header, whether any C program now exercises it. The matrix in
C6 must not claim more than that.

**Status: done (2026-10-04).** The record is generated, not written:
`.github/scripts/c-surface-record.py` lists every function each header
declares (from C1's header model) and the C programs CI runs that call it:
consumer programs and their support files, arming programs, and the skill
examples. A call is the name followed by `(` in code, outside comments and
strings. `CLM_FN` import lists do not count. The output is
`tests/c_abi/SURFACE.md`, and CI regenerates it and fails on any
difference, so it cannot go stale in either direction.

At this commit:

| Header | Declared | Called by a consumer | Called by any C program |
| --- | ---: | ---: | ---: |
| `net.go.h` | 218 | 35 | 52 |
| `net.h` | 41 | 2 | 6 |
| `net_cortex.h` | 92 | 2 | 8 |
| `net_deck.h` | 84 | 0 | 0 |
| `net_mcp.h` | 22 | 0 | 0 |
| `net_meshdb.h` | 27 | 0 | 0 |
| `net_meshos.h` | 23 | 0 | 0 |
| `net_org.h` | 30 | 0 | 15 |
| `net_rpc.h` | 70 | 10 | 17 |
| `net_subnet.h` | 4 | 0 | 0 |
| `net_transport.h` | 7 | 7 | 7 |

- Five headers have no C caller at all: `net_deck.h`, `net_mcp.h`,
  `net_meshdb.h`, `net_meshos.h` and `net_subnet.h` (and C3b's
  write-token waits in `net_cortex.h` are still to come). Their C evidence
  is C1's audit only: declared, exported and type-matched, never called.
- `net_org.h` is called only by the skill examples
  (`net_org_streaming.c`), not by a consumer program, so it has no
  sanitizer, debug-CRT or verifier lane.
- `net.h` and `net.go.h` both declare the core surface, so a function in
  both is counted under both.

### C6: docs, the support matrix and the D2 ledger

- The C docs (`include/README.md`, `web/src/content/docs/sdk/c/*.md`,
  the C sections of the skill guides) are rebuilt from the C2–C4 programs:
  - snippets are taken from code that runs;
  - every link line is one that actually linked in C0;
  - the Windows guidance names the toolchains C5's compatibility lanes
    ran, and the registered-release contract.
- The D2 per-topic ledger is completed here.
- The C column of `docs/data/capabilities/*.yaml` is checked against C5b:
  - each `supported` cell names a consumer program or a skill example
    that exercises it;
  - a cell nothing exercises drops to `partial`, with the gap stated;
  - a cell backed only by helper-build evidence (repair) says so;
  - `capability_records.py --check` regenerates the tables.

**Proves it:**
- A small extension to `capability_records.py` requires a C-cell
  `evidence:` field and checks that the named file exists and runs in CI.
- The existing guards stay green: skills, header count, doc code width.

**Status: done (2026-10-04).**
- **The matrix.** Every positive C cell in `docs/data/capabilities/*.yaml`
  now carries `evidence:` (the C programs CI runs that call its anchor) or
  a `gap:`. `capability_records.py --check` requires:
  - a `supported` C cell to name evidence;
  - each evidence file to be tracked, run by CI (a consumer program in
    `examples/c/`, or a C skill example `examples.yaml` runs) and to
    **call** the anchor, by the same call finder as C5b;
  - a `partial` cell without evidence to state its gap.

  Four self-test cases plant each defect. Result: 9 of 23 positive C cells
  are backed by a running program. 13 dropped from `supported` to
  `partial`: filter DSL, streams, gang-claim, org call, the three subnet
  cells, MCP, CortEX folds, MeshDB, compute, Deck and Redis dedup. Each
  states its gap, and names the Go cgo wrapper where one exists. The
  membership-rejection cell was already `partial`. `coverage.md` gains a
  generated C evidence table.
- **The docs.**
  - `include/README.md` and `sdk/c/headers-and-linking.md` list only
    programs CI runs.
  - `headers-and-linking.md` gains the Windows toolchains C5 ran
    (`/MD`, `/MDd`, MinGW-w64 UCRT and MSVCRT) and why mixing C runtimes
    is safe.
  - `memory-and-threading.md` gains the library-buffer free functions and
    the registered-release contract for callbacks.
  - "Three memory rules" was true of the event bus only. It is corrected
    in the quickstart, the README and the skill's `bindings/c.md`. The
    skill's "no callback" line is also corrected: C has handler and
    vtable callbacks.
- **Not done here.** The plan's "snippets taken from code that runs" is
  met for the examples tables, not for every prose snippet. The quickstart
  snippet is already compile-checked by the snippet ratchet.
- **The skill's C blob section** (`dataforts.md`, defect below) now
  documents `net_blob_register_callback_adapter_owned`: its vtable, the
  `free_buffer` release, and `ctx` ownership. `blob_callbacks.c` is the
  running program behind it.
- **CR-5 moved.** A Rust unit test pinned that `examples/capability.c`
  did not include both `net.h` and `net.go.h`. The property is now a C1
  lint rule over every consumer program (with a self-test), and the test
  went with the file.

**Follow-up (2026-10-04): the partial cells closed.** Twelve new
consumer programs, each run in every C5 lane:

| Program | Covers | Named checks |
| --- | --- | ---: |
| `capabilities.c` | filter DSL and capability helpers, against six cross-language fixtures | 287 |
| `mcp.c` | MCP helpers and consent / pin store, against the MCP fixtures | 79 |
| `redis_dedup.c` | the consumer-side dedup window | 25 |
| `meshdb.c` | the MeshDB query layer (found D-C6-1) | 69 |
| `compute.c` | a C daemon behind the compute dispatcher; fork and replica groups | 58 |
| `deck.c` | the Deck operator surface (found D-C6-2) | 78 |
| `streams.c` | per-peer streams and the stream inbox | 36 |
| `islands.c` | the gang-claim scheduler | 31 |
| `aggregator.c` | registry and fold-query clients, from a `net.h`-only unit | 22 |
| `meshos.c` | the MeshOS daemon-author SDK | 39 |
| `org_call.c` | a unary protected org call, cross-org (org scenario) | 40 |
| `subnet.c` | gateway provisioning, exported serve and call (subnet scenario) | 43 |

- **Scenarios.** Org and subnet credentials are issued material, so the
  runner generates them per invocation with the in-repo generators the
  other bindings' live tests load. A program declares
  `NET-NEEDS: org-scenario | subnet-scenario`.
- **Result.** 22 of 23 positive C cells name a running program: 20
  `supported`; Deck and CortEX folds `partial` with evidence and their
  stated gaps (D-C6-2; a successful fold query needs the aggregator
  daemon, which C cannot serve); membership rejection `partial` for its
  collapsed taxonomy. Every header now has a C caller
  (`tests/c_abi/SURFACE.md`).
- **D-C5-1** was fixed in two halves along the way (see Defects).
- **Generated fixture headers.** `gen-c-fixture-cases.py` renders the
  capability and MCP fixtures as C data, with `--check` in CI.

**D2 ledger.** The eight pre-plan `examples/*.c` are deleted. None was
built or run by CI, and `capability_aggregation.c` no longer compiled: it
called three `net_meshnode_*` functions that no header declares. Per
topic:

| Stale file | API the headers provide | C-runtime evidence now | Documentation gap |
| --- | --- | --- | --- |
| `basic.c` | Event bus, `net.h` | `hello.c` and `observe.c` (skill examples, run) | none; the quickstart is compile-checked |
| `transport.c` | Blob and directory transfer, `net_transport.h` | `transfer.c` (consumer, every lane); `objectstore.c` | none |
| `capability.c` | Capability validation, predicate evaluate/trace, `net-where:` headers, debug-report aggregation, `net.go.h` | `registry.c` covers announce and discovery only | the stateless helpers (`net_validate_capabilities`, `net_predicate_*`) have no C caller; matrix: filter DSL `partial` |
| `capability_aggregation.c` | Capability aggregation and capacity ranking, `net.go.h` | none | no C caller; the old example used removed functions |
| `scheduler.c` | Task-lifecycle workflow (`net_cortex.h`) and gang-claim reserve/release (`net.go.h`) | none for either | matrix: gang-claim `partial` |
| `meshdb.c` | MeshDB factory AST, runner, iterator, `net_meshdb.h` | none | matrix: MeshDB `partial` |
| `meshos.c` | MeshOS daemon-author vtable, `net_meshos.h` | none | no matrix row of its own (compute/daemons `partial`) |
| `deck.c` | Deck operator workflow, `net_deck.h` | none | matrix: Deck `partial` |

Each retired topic stays named here and in `tests/c_abi/SURFACE.md`, whose
per-function record shows its functions with no C caller. Deleting the
files changed what is documented as run, not what is implemented.

### C7 (optional, D1(b)): the bundle as a release asset

Attach the C0 bundle to GitHub releases for Linux x86_64 and Windows
x86_64, with checksums, following
[`ANCHOR_PREBUILT_BINARIES_PLAN.md`](ANCHOR_PREBUILT_BINARIES_PLAN.md).
Not a prerequisite for any other slice.

## Risks

- **The default build differs from the tested one in more than seams.**
  `net-ffi`'s default features could drop a surface the docs promise.
  C1's per-profile classification shows what is real, stubbed or absent;
  a consumer program failing shows a promised feature missing. *Fallback:*
  a feature change in `net-ffi/Cargo.toml`, recorded here.
- **#1167 is delayed.** *Fallback:* C3b waits; every other slice is
  independent of it. The pre-#1167 fixture is still added in C1.
- **The MSVCRT lane fails.** It is expected to pass under registered
  release. *Fallback:* record the failure with its evidence, and the docs
  require UCRT until it's understood. It does not reopen the allocator
  ABI by assumption.
- **The extended parser can't express a type.** *Fallback:* that check
  moves into a compiled C assertion (as layout and constants already do),
  not into an allowlist.
- **Two-node loopback programs flake on CI runners.** *Fallback:* the
  same retry-free, bounded-timeout shape the skill runner uses. A flake is
  a defect to fix, not to retry.
- **Windows CI cost.** *Fallback:* the Windows legs run on changes to
  `include/**`, `src/ffi/**`, `bindings/go/*-ffi/**` and the C programs,
  using the existing path filter. AppVerifier runs are the slowest and
  can be split out first.
- **C1's first run finds more than this plan can fix.** *Fallback:*
  blockers (a declared but unexported function, an unsafe constant clash,
  an undeclared returned code) are fixed in C1. Everything else is
  recorded under "Defects found on the way" with an owner.

## Not in scope

- New C surfaces: A2A, payments, or anything else marked `not exposed`.
- C++ wrappers or frameworks, pkg-config or CMake packaging, and wholesale
  C parity with the other bindings.
- A runtime ABI handshake (see "Compatibility boundary").
- macOS: no CI runner is assumed. The bundle script supports `.dylib`, but
  no macOS evidence is claimed.
- Replacing the deleted meshdb, meshos, deck and scheduler examples with
  full programs. The D2 ledger and C5b record them.
- The Go binding, which has its own plan, and the browser/wasm surfaces.

## Defects found on the way

Found by C5:

- **D-C5-1 (fixed): a `MeshNode` is not reclaimed after `net_mesh_shutdown`
  and `net_mesh_free`.** In `lifecycle.c` and `rpc_callbacks.c`, the node's
  memory (allocations of `MeshNode::new`, task buffers, maps: 0.2 MB to
  0.6 MB per program, varying run to run) is still allocated at exit.
  - LSan reaches it only through the freed handle's tombstone, via the
    stale bits of the `Arc` that `net_mesh_free` moved out. So no live
    reference remains and the memory was never released: a leaked `Arc` or
    a reference cycle.
  - **Fixed (2026-10-04).** The cause was in the FFI, not the core.
    Nineteen entry points in `src/ffi/mesh.rs` (`net_mesh_start`,
    `_connect`, `_accept` and others) took their node with
    `h.inner.clone()`, where `inner` is a `ManuallyDrop<Arc<MeshNode>>`.
    `ManuallyDrop<T>: Clone` clones the wrapper, so every call leaked one
    strong count, and any started node outlived its free. `src/ffi/blob.rs`
    had already been fixed for the same pattern; `mesh.rs` had not.
    - Found by bisection: a strong count of 2 after start, constant
      through shutdown and free, and unchanged with every spawned loop
      skipped and with `start` itself a no-op, which left only
      `net_mesh_start`'s own clone.
    - `transfer.c` leaked the same way. LSan scans conservatively, and a
      stale pointer it could still reach hid that program's leak; the
      leak was in every started node.
    - Now `Arc::clone(&h.inner)` at all nineteen sites.
      `ffi::mesh::tests::node_reclaim_c_abi` requires a started node, and
      a connected pair in `lifecycle.c`'s sequence, to be reclaimed after
      free. `ffi::tests::no_clone_of_a_manually_drop_arc_field` refuses a
      `.clone()` on any `ManuallyDrop<Arc<_>>` field in `src/ffi`, with
      the field names read from the declarations. Both fail when one
      leaking clone is reintroduced.
    - `lsan_policy.toml` no longer accepts any indirect leak.
  - **Second half (fixed the same day).** With the node itself reclaimed,
    the next sanitizer run (37194875653) still found its components
    alive at exit, now in `transfer.c` too: the failure detector, the
    reroute policy, session routing, the roster, the scoped publication,
    the subnet challenge store, reachable only from one another. They
    formed a strong cycle that `MeshNode::new` builds itself: the
    detector's callbacks hold the reroute policy, and the policy's verdict
    check held an `Arc` to the detector. No drop could break it, so every
    node ever built left these behind for the life of the process.
    - The verdict check now holds the detector weakly; a detector that is
      gone has no current verdict, so the check answers false.
    - Witness: `adapter::net::mesh::component_reclaim_tests` (a connected,
      started pair, shut down and dropped, must leave none of the six
      components alive). It fails without the fix. The reroute, verdict,
      failure and peer-death suites pass with it (148 unit tests, 69
      integration).

Found by the C6 follow-up programs:

- **D-C6-1 (fixed): `net_meshdb_decode_payload_json` misread a window as
  an aggregate.** The sentinel envelopes carry no type tag, so the decoder
  tries Aggregate, Joined and Window in turn and took the first that
  parsed. `postcard::from_bytes` accepts any valid prefix, and a one-row
  window bucket starting at seq 0 prefix-parses as an Aggregate. `meshdb.c`
  saw it as `{"kind":"aggregate", ... "avg", "value":1.1e-310}` for the
  bucket [0, 2). C and Go use this decoder; Python and Node decode by an
  explicitly chosen type and were not affected.
  - Fix: each type counts only if it consumes every byte
    (`postcard::take_from_bytes` with an empty remainder).
  - Witness: `a_window_that_prefixes_as_an_aggregate_decodes_as_a_window`
    in `net-meshdb-ffi`, which asserts its own precondition (the payload
    still prefix-parses as an aggregate), and `meshdb.c`'s per-bucket
    checks.
  - Not fixed: a payload that exactly parses as two envelope types would
    still be ambiguous. The cure is a tagged envelope or typed C decoders,
    an ABI change.

- **D-C6-2 (open, an ABI gap): a C deck client cannot verify or
  attribute a signed ICE commit.** `net_deck_client_new` takes no operator
  registry, and nothing in `net_deck.h` attaches one. Without a registry
  the SDK's `IceSimulated::commit` routes every commit through the unsigned
  admin path (`deck.rs`), so the signatures a C caller passes are dropped:
  the audit ring records the freeze with no operator ids and outcome
  `Unverified`, and `by_operator` cannot find it. The commit itself
  succeeds. `deck.c` checks what holds today and says so; the matrix's
  Deck C cell stays `partial` with this gap, now with `deck.c` as its
  evidence. The fix is an ABI addition (a registry or verifier on the
  client), not a test.

Found by C1, stage 1:

- **`net.go.h` failed to compile under `-Wall -Werror`.** A doc comment
  read `*out_ref/*out_ref_len`, and the `/*` inside it trips GCC's
  `-Wcomment`. Any C consumer including `net.go.h` (or Go's `go/net.h`
  mirror) with warnings as errors stopped there. Fixed in both. Every
  shipped and mirror header now compiles alone under `-std=c11 -Wall
  -Wextra -Werror -pedantic`.
- **Six nRPC exports had no C declaration**, though the skill guide and
  the web docs tell users about `net_rpc_watch_tools` and
  `net_rpc_observer_dropped_total`. Declared in `net_rpc.h`.
- **`check-abi-commit.py` read an include guard as an ABI constant.** C0's
  `net_test_helpers.h` opens with `#define NET_TEST_HELPERS_H`, and the
  guard demanded header, Go-mirror and Go-ABI-test changes in the same
  commit, failing the `go-tests` job on C0's push. It now counts a
  `#define` only when it carries a value or is function-like, and reads
  each constant pattern only in its own language (`.rs` for Rust consts;
  `.h` and `.c` for defines and enum members). Before that, a `#define`
  line inside a Python self-test string counted. Two self-test cases pin
  this; the include-guard one fails under the old pattern.
- **The skill's C blob section is stale.**
  `.claude/skills/net-event-bus/dataforts.md:218` says no host-language
  adapter registration is exposed to C, but
  `net_blob_register_callback_adapter_owned` is declared in `net.go.h`
  since #1165. Fixed in C6.

Known before C1 runs:

- **The crate's C examples are stale** (gap 5). Fixed by C2–C4 under D2.
- **The shipped export set is unchecked** (gap 2). Fixed by C0.
- **The callback-allocator headers contradict the code** (gap 6, review
  R1). `net_rpc.h:266-269` says Rust frees with `free(3)`; the code
  releases through the registered deallocator and refuses without one on
  every platform. `net_org.h` is similar. Fixed in C4.
- **Returned codes are undeclared** (gap 3, R3):
  `NET_ERR_BLOB_INVALID_ARGUMENT`, `NET_ERR_BLOB_BACKEND`,
  `NET_ERR_WRONG_ORIGIN`, `NET_ERR_FEATURE_NOT_BUILT`, and whatever else
  C1 finds. Declared in C1.
- **`check-rpc-abi-parity.py` can't see pointer depth** (gap 3, R3).
  Fixed in C1 when its parser is extended.

## Review

Two reviews. Every finding was checked against the source before the
revision that answered it.

### First review: HOLD at `8192253`

| Finding | Verdict | Change |
| --- | --- | --- |
| R1 (high): C4/C5 targeted a cross-allocator path already repaired | Confirmed: `net_rpc_set_callback_free`, all-platform refusal, no `free` fallback | Gap 6 rewritten. C4 tests the registered release, plus refusal in a fresh process, and fixes the stale headers. The predicted MSVCRT corruption and the proposed allocator ABI are removed |
| R2 (high): Windows checker not armed; coverage must follow allocation domains | Confirmed: `_CrtSetDbgFlag` is compiled out without `_DEBUG` | C5 split into compatibility and checker lanes, each with a domain claim. Arming negatives use a real library-returned buffer and require named diagnostics. Retained allocations reported per class |
| R3 (medium): C1 needs exact types, layouts and declared constants | Confirmed: four codes undeclared; the parity parser collapses pointer depth | C1 extends the existing parser, adds exact signatures, compiled layout and constant checks, safe exports and the umbrella crate. The self-test plants same-arity, callback and layout drift |
| R4 (medium): no token consumer witness; one baseline can't see the change | Confirmed: v0.39.0 has no waits; this base still has four arguments | #1167 is a prerequisite. C3b `write_tokens.c`. A pre-#1167 fixture. Exact-entry `breaks.toml` replaces the commit trailer. Matched-pair policy stated |
| R5 (medium): the repair locator can't be built from the named APIs | Confirmed: stats and describe expose no shard hashes or paths | D3 decided as (b), the helper bundle, labelled as helper-build evidence. The Go plan's S6 assertions kept |
| R6 (medium): pin the artifact compiled and loaded; stubs aren't behaviour | Accepted | Separate target and staging directories, `PROVENANCE`, loader pinned to the bundle, Windows import library staged, per-profile classification |
| R7 (medium): the universal failure-output rule contradicts existing contracts | Confirmed: `fetch_range` mixed-null and `public_key_hex` early return | Per-function contracts. Shutdown and free tested separately. Repeat-close only where documented. Empty manifest split into two cases |
| D1 | Agreed | CI-only first; the release asset is C7 (the earlier text said C6) |
| D2 | Agreed | A per-topic ledger replaces "delete and list" |
| D3 | Agreed | (b), as R5 |

### Second review: HOLD at `491237b`

Kept every first-review correction and asked for no redesign.

| Finding | Verdict | Change |
| --- | --- | --- |
| S1 (medium): loader-path isolation and printed provenance don't identify the loaded library | Accepted: `PATH`/`LD_LIBRARY_PATH` are not the whole search order on either platform | New "Loaded-library identity": the address each Net function resolved to, its module, then path and SHA-256 against the staged artifact, per bundle. Sanitized loader settings kept as setup. Shadowing and `LD_PRELOAD` negative controls fail the check by name. C0 proves it |
| S2 (medium): `repair.c` has no declared route to the helper functions | Confirmed: neither function is in a shipped header; Go declares them locally | `net_test_helpers.h`, outside `include/`, staged only in the helper bundle with its own inventory, and checked by C1 under the helper profile. The production bundle's assertions are unchanged |
| C4 wording: callback codes don't all round-trip | Confirmed: `HASH_MISMATCH` maps to `Backend`, then `NET_ERR_BLOB_BACKEND` | C4 names the input/output pairs the witness drives. No native change |
