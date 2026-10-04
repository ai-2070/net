# C SDK — verify the shipped header/library pair from a real consumer

## Status

Planned, 2026-10-04. Targets the release after 0.39. Branch
`LZL0/c-consumer-plan`. Follows
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
   (`bindings/go/net-ffi/exports.baseline`) is generated from that build,
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
  `bindings/go/net-ffi/exports.shipped.baseline`, so a change to the
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
  `run-c-consumers.sh`, which reuses `run-skill-examples.sh`'s run
  contract.
- Each assertion is a named check printed on success. CI fails if the
  number of named checks drops below the recorded floor; that's the
  roster pattern.

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
| Windows | Application Verifier with full PageHeap, on the consumer and the release `net.dll` | heap corruption and wrong-heap frees across the module boundary | leaks |
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

### C5b: unexercised surfaces (record only)

The audit (C1) covers all eleven headers, but C2–C4 exercise only
transport, blobs, the registry, callbacks, nRPC and (C3b) write tokens.
For the rest (`net_meshdb.h`, `net_meshos.h`, `net_deck.h`, `net_mcp.h`,
`net_subnet.h`, and the cortex surface other than write tokens), C5b
records, per header, whether any C program now exercises it. The matrix in
C6 must not claim more than that.

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
