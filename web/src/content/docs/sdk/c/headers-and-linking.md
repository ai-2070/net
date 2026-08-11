---
title: Headers and Linking
description: The eleven headers, the one shared library they all resolve against, and the net.h / net.go.h choice you have to make per translation unit.
---

# C — Headers and Linking

The C surface is not one header. It is eleven — and they all resolve out of a
single shared library, `libnet`. One of the header pairings is mutually
exclusive. This page is the map.

## The one decision that bites

`net.h` and `net.go.h` **share the `NET_SDK_H` include guard**, so only one of
them can be active in a translation unit. `net.go.h` is the broader surface —
but it is *not* a strict superset: `net_ingest_raw_ex`, `net_poll_ex` and
`net_stats_ex` exist only in `net.h`.

Pick one per translation unit based on the symbols you need. If you need both
surfaces in one program, split them across separate translation units.

Every other header has its own guard and composes freely.

## The headers

| Header | Guard | Surface | Library |
|---|---|---|---|
| `net.h` | `NET_SDK_H` | Narrow event bus — init / ingest / poll / stats / shutdown | `libnet` |
| `net.go.h` | `NET_SDK_H` | Mesh + compute — sessions, channels, capabilities, NAT, daemon dispatch, placement filters, predicate helpers | `libnet` |
| `net_cortex.h` | `NET_CORTEX_H` | RedEX logs, CortEX Tasks/Memories adapters, NetDb bundle + snapshot | `libnet` |
| `net_transport.h` | `NET_TRANSPORT_H` | Blob and directory transfer over the stream transport | `libnet` |
| `net_rpc.h` | `NET_RPC_H` | nRPC request/response | `libnet` |
| `net_meshdb.h` | `NET_MESHDB_H` | Federated query layer over capability queries + CortEX folds | `libnet` |
| `net_meshos.h` | `NET_MESHOS_H` | Daemon-author SDK — operator handle + control-event channel | `libnet` |
| `net_deck.h` | `NET_DECK_H` | Deck operator-side SDK | `libnet` |
| `net_org.h` | `NET_ORG_H` | Organization capability auth | `libnet` |
| `net_subnet.h` | `NET_SUBNET_H` | Subnet authority — exported serve + gateway provisioning | `libnet` |
| `net_mcp.h` | `NET_MCP_H` | MCP bridge helpers, graduated consent / pin surface | `libnet` |

`net_cortex.h` is deliberately self-contained — it depends only on `<stdint.h>`
and `<stddef.h>`, so a consumer who wants just the storage slice can include it
without dragging in the mesh and compute surface from `net.go.h`. Its symbols
resolve when the cdylib is built with `--features "netdb redex-disk"`.

`net.go.h` is the in-crate mirror of `go/net.h` at the repo root, and pulls in
`net_cortex.h` for its storage half.

Every header resolves out of the same `libnet`. That is not a simplification
of the table — it is the whole shipping model, and the reason is in
[One library, on purpose](#one-library-on-purpose) below.

`net_subnet.h` shares `net_org.h`'s error namespace (`NET_ORG_ERR_SUBNET`) and
handle model, and `#include`s it. The subnet-exported *caller* verb, `net_org_call_exported`, is declared
in `net_org.h` because it takes the org client handle.

## Building the library

```bash
cargo build --release -p net-ffi
```

One command, one library, every header served.

Output lands in `target/release/`:

| Platform | Filename | Also produced |
|---|---|---|
| Linux | `libnet.so` | — |
| macOS | `libnet.dylib` | — |
| Windows | `net.dll` | `net.dll.lib` (import library), `net.pdb` |

On Windows the DLL is not the thing you link against. `net.dll.lib` is the
import library, and it is what the linker needs; `net.dll` is what the
*loader* needs, later, at run time. Missing either produces a different
failure, and they are covered separately below.

## Compiling against it

```bash
gcc -o app app.c -L target/release -lnet -lpthread -ldl -lm
```

`-lnet` regardless of which headers you included. There is no second `-l` to
add.

On Windows, pick the form that matches your toolchain:

```bat
:: MSVC — link the import library by name
cl /I include app.c target\release\net.dll.lib
```

```bash
# MinGW / GCC — `-lnet` resolves against net.dll directly.
gcc -o app.exe app.c -Iinclude -Ltarget/release -lnet
```

`-lpthread -ldl -lm` are not Windows libraries; leave them off. If your `ld`
is old enough that `-lnet` does not find `net.dll`, generate a GNU import
library once and link that instead:

```bash
gendef net.dll                              # writes net.def
dlltool -d net.def -D net.dll -l libnet.dll.a
```

## One library, on purpose

Net used to ship a cdylib per surface — `libnet_rpc`, `libnet_org`,
`libnet_meshdb` and the rest — and this page used to tell you to link the ones
you needed alongside `-lnet`. **Do not do that**, on any version. If you have
a build script carrying those flags, drop them.

Each of those libraries statically embedded its own copy of the core, and
re-exported the core's symbols. Linking two therefore put two copies of Net's
internal state in one process. The exported *functions* deduplicate — the
dynamic linker resolves each name once — which is why the arrangement worked
for a long time and why it was documented here in good faith.

`static`s do not deduplicate. Each copy keeps its own. One of them is the
registry `parking_lot` uses to track threads blocked on a lock: a thread that
blocks through one copy is recorded in that copy's registry, and an unlock
that runs through the other consults a registry the waiter was never in. The
lock is released; the waiter is never woken.

It surfaces as a hang with no visible cause. The case that found it sat ten
minutes inside a capability announcement while every worker thread in the
process was idle — a held lock owned by nobody. It is timing-dependent, so it
passes far more often than it fails.

One library makes that unreachable. The per-surface libraries are no longer
built.

## Finding it at run time

Linking is not the last step. The executable records the library by name, and
the loader has to find it when the process starts:

```bash
LD_LIBRARY_PATH=target/release ./app       # Linux
DYLD_LIBRARY_PATH=target/release ./app     # macOS
```

Windows has no equivalent variable — it searches the executable's own
directory first, then `PATH`. So do one of these:

```bat
:: Either put the DLL beside the executable — what you ship
copy target\release\net.dll .
app.exe

:: Or point PATH at the build output — what you do while developing
set PATH=%CD%\target\release;%PATH%
app.exe
```

Skip it and the program dies before `main`. The failure is quiet and easy to
misread — under MSYS/MinGW it looks like this, and a plain `cmd.exe` shows a
dialog or nothing at all:

```text
$ ./app.exe
app.exe: error while loading shared libraries: net.dll: cannot open shared
object file: No such file or directory
[exit=127]
```

Nothing there says the link succeeded and only the loader failed, so it is
worth recognizing: exit 127 naming `net.dll` means the DLL is not on the
search path, not that anything is wrong with your build.

## Examples in the repo

| File | What it shows |
|---|---|
| `include/examples/basic.c` | The event-bus quickstart loop |
| `include/examples/capability.c` | Stateless capability, predicate and where-header helpers |
| `include/examples/meshdb.c` | MeshDB factory AST, runner, iterator, sentinel-envelope decoder |

## Next

- [Quickstart](/docs/sdk/c/quickstart) — ingest and poll
- [Memory and Threading](/docs/sdk/c/memory-and-threading) — the ownership rules
- [Errors](/docs/sdk/c/errors) — the return-code table
