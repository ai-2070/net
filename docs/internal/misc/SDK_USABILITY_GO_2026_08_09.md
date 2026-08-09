# Go SDK and Go/C-bridge black-box developer-usability scan — 2026-08-09

## Result

Discovery-only scan. No package source, header, example, documentation, build configuration, commit, issue, or external system was changed. The only repository write is this report.

Audited checkout:

- repository: `C:/Users/chief/Documents/git/net`
- required and observed HEAD: `300deeee6b2ceb55507cb8101289a9c992b0a37f`
- commit time: `2026-08-09T06:46:59-07:00`
- subject: `Merge pull request #797 from ai-2070/sdk-bugs-2`
- initial `git status --short`: empty
- public Go candidate resolved by the Go proxy: `github.com/ai-2070/net/go v0.0.0-20260809134659-300deeee6b2c`
- candidate checksum: `h1:gYCDEN0zHIYSC6uDcDlHgGzxf4LFj/XLEe/aZgMzPl4=`
- candidate `go.mod` checksum: `h1:P2rM3mqPutXSVTA/s0juQcrc7LVqfTDnyKcvd9WjdtA=`
- native workspace package version: `net-ffi 0.35.0`

There are seven findings: three MAJOR and four MODERATE. The first literal public path does not reach a build on this clean Windows machine. A fully worked, source-matched workaround did reach real execution and exposed a false-success example and a stale runtime version.

Named-channel/channel-specific behavior was excluded because that scope is already frozen elsewhere. Internal audit packets, plans, tests, prior reviews, and prior sessions were not read before findings were frozen. Public examples and public headers were in scope; implementation source was inspected only after the corresponding black-box witnesses were frozen.

## Scope and public material used

In scope:

- `go/README.md`, `go/go.mod`, `go/example/go.mod`, `go/example/main.go`
- generated package reference from `go doc github.com/ai-2070/net/go`
- public Go docs at this SHA:
  - `web/src/content/docs/start/install/go.md`
  - `web/src/content/docs/sdk/go/README.md`
  - `web/src/content/docs/sdk/quickstart/go.md`
  - `web/src/content/docs/sdk/announce/go.md`
  - `web/src/content/docs/sdk/discover/go.md`
  - `web/src/content/docs/sdk/invoke/go.md`
  - `web/src/content/docs/sdk/watch/go.md`
  - `web/src/content/docs/sdk/errors/go.md`
- public C docs and headers at this SHA:
  - `net/crates/net/include/README.md`
  - `net/crates/net/include/net.h`
  - `net/crates/net/include/net.go.h`
  - the other headers listed by the public header map
  - `web/src/content/docs/sdk/c/quickstart.md`
  - `web/src/content/docs/sdk/c/headers-and-linking.md`
  - `web/src/content/docs/sdk/c/memory-and-threading.md`
- ordinary `go`, `cargo`, generated Go reference, and compiler help/error surfaces
- the repository candidate package and the native artifact built from the pinned checkout

Public URLs represented by those source pages:

- <https://pkg.go.dev/github.com/ai-2070/net/go>
- <https://ai2070.net/docs/start/install/go>
- <https://ai2070.net/docs/sdk/go>
- <https://ai2070.net/docs/sdk/go/quickstart>
- <https://ai2070.net/docs/sdk/c/quickstart>
- <https://ai2070.net/docs/sdk/c/headers-and-linking>
- <https://ai2070.net/docs/sdk/c/memory-and-threading>

The repository SHA above is the documentation revision used for all conclusions. No implementation source was used to decide the expected behavior of a witness.

## Environment and clean starting state

Audit time and host:

```text
$ date --iso-8601=seconds
2026-08-09T19:26:58+02:00

$ uname -a
MINGW64_NT-10.0-26100 varkerulet 3.6.9-b4195d69.x86_64 2026-06-06 17:49 UTC x86_64 Msys
```

Initial toolchain:

```text
$ go version
go version go1.25.5 windows/amd64

$ go env GOOS GOARCH CGO_ENABLED CC CXX GOTOOLCHAIN
windows
amd64
0
gcc
g++
auto

$ gcc --version
bash: gcc: command not found

$ clang --version
bash: clang: command not found

$ cmake --version
bash: cmake: command not found

$ rustc --version
rustc 1.97.1 (8bab26f4f 2026-07-14)

$ cargo --version
cargo 1.97.1 (c980f4866 2026-06-30)
```

No `gcc`, `clang`, or `cl` was initially available. The later workaround used a portable compiler extracted only under the disposable workspace:

```text
w64devkit v2.9.0
asset: w64devkit-x64-2.9.0.7z.exe
sha256: bff1d13fc2718eebd93548cf37f8d0332d925458d5e99506cff8f46eb5a9de5a
gcc.exe (GCC) 16.1.0
```

All consumer assets are outside the repository under:

```text
C:/Users/chief/AppData/Local/Temp/net-go-usability-300deeee-20260809/
```

Important witness locations:

```text
install-latest/main.go       literal Go install-page program
poll-default/main.go         minimal default-ingest/default-poll witness
example-copy/main.go         byte-identical copy of go/example/main.go
lifecycle/main.go            ABI and lifecycle positive control
c-quickstart/app.c           C quickstart; first exact, then one-line workaround
cargo-target/release/        native artifacts built outside the checkout
```

The copied repository example was verified before its external module wrapper was changed:

```text
go/example/main.go and example-copy/main.go
sha256: cf33e822cff784a8efcead3346095d791a5fbbcb4da83977f6f0be7f9d0ebda2

go/example/go.mod and the initial example-copy/go.mod
sha256: 5f17efdd986caa022a39e1219e18653ab902d5b3e97ddb237da784b00ea5056c
```

## Attempted journeys

1. Initialized a new empty Go module and ran the documented `go get` command.
2. Copied the documented install verification program and ran `go vet`, `go build`, and `go run` without guessing native configuration.
3. Enabled cgo and observed the next diagnostic.
4. Built the documented unified native library from pinned HEAD with `CARGO_TARGET_DIR` outside the checkout.
5. Added a temporary portable MinGW compiler, then retried vet/build.
6. Constructed the missing GNU import library and supplied an explicit library search path as a workaround; built and executed the Go verification program.
7. Ran the repository Go example end to end against the exact public candidate.
8. Reduced its ingest/poll behavior to a 20-line consumer.
9. Copied, compiled, linked, and executed the public C quickstart.
10. Exercised ABI verification, double shutdown, and post-shutdown errors as a positive lifecycle control.
11. Queried generated package documentation and compared it with the public one-library shipping model.
12. Only then inspected small implementation-source regions to localize the already-frozen findings.

## Findings

### GO-01 — MAJOR — diagnostics — cgo-disabled builds present the SDK as an API with missing methods

Confidence: HIGH.

User intent: install the Go module and run the first documented event-bus operation.

Public path followed:

1. `go mod init example.com/net-go-install`
2. `go get github.com/ai-2070/net/go`
3. copy the verification program from `web/src/content/docs/start/install/go.md`
4. run the guide-recommended `go vet`, then `go build`/`go run`

The install itself succeeded and resolved the exact pinned candidate. Go automatically downloaded the required Go toolchain:

```text
$ go get github.com/ai-2070/net/go
go: downloading github.com/ai-2070/net/go v0.0.0-20260809134659-300deeee6b2c
go: downloading github.com/ai-2070/net v0.34.0
go: github.com/ai-2070/net/go@v0.0.0-20260809134659-300deeee6b2c requires go >= 1.26; switching to go1.26.5
go: upgraded go 1.25.5 => 1.26
go: added github.com/ai-2070/net/go v0.0.0-20260809134659-300deeee6b2c
[exit=0 elapsed=35s]

$ go version
go version go1.26.0 windows/amd64

$ go env GOOS GOARCH CGO_ENABLED CC GOTOOLCHAIN
windows
amd64
0
gcc
auto
```

First divergence:

```text
$ go vet ./...
# example.com/net-go-install
# [example.com/net-go-install]
vet.exe: .\main.go:11:21: undefined: net.New
[exit=1 elapsed=7s]

$ go build ./...
# example.com/net-go-install
.\main.go:11:21: undefined: net.New
.\main.go:11:30: undefined: net.Config
[exit=1 elapsed=1s]

$ go run .
# example.com/net-go-install
.\main.go:11:21: undefined: net.New
.\main.go:11:30: undefined: net.Config
[exit=1 elapsed=0s]
```

With cgo explicitly enabled, the actual unavailable layer is finally named:

```text
$ CGO_ENABLED=1 go vet ./...
# runtime/cgo
cgo: C compiler "gcc" not found: exec: "gcc": executable file not found in %PATH%
[exit=1 elapsed=1s]
```

Expected: because the public package's useful API requires cgo, the public install path or build diagnostic should identify cgo/native toolchain unavailability. A user should not be told that the documented `net.New` and `net.Config` API simply do not exist.

Actual: a normal Windows Go environment with `CGO_ENABLED=0` imports a partial package and emits misleading undefined-identifier diagnostics at the user's code. This blames the copied program rather than the unavailable cgo layer.

Minimal runnable reproduction: `install-latest/go.mod` and `install-latest/main.go`; deterministic command `go build ./...` with the environment above.

Workaround, separate from defect: install a cgo-compatible C compiler and set `CGO_ENABLED=1`. On this machine a portable MinGW GCC made `go vet ./...` pass in 10 seconds. That only exposed GO-02 at link time.

Implementation-source inspection required: NO for the finding. Later localization confirmed that the useful package files import `C`, so cgo exclusion accounts for the partial package.

### GO-02 — MAJOR — installation — the documented Windows native build does not produce a directly consumable fresh-module link path

Confidence: HIGH for the tested Windows/MinGW path; MEDIUM for untested Windows compiler distributions.

User intent: satisfy the documented native prerequisite and link the Go quickstart.

Public path followed:

- `go/README.md` says to run `cargo build --release -p net-ffi` from `net/crates/net/` and says Windows produces `target/release/net.dll`.
- the web install page says Windows requires MSVC and that `go build` needs the real library.
- the module was fetched through ordinary `go get` in a clean external module.
- the pinned native package was built with its target directory outside the read-only checkout.

Native build succeeded:

```text
$ CARGO_TARGET_DIR=C:/Users/chief/AppData/Local/Temp/net-go-usability-300deeee-20260809/cargo-target \
    cargo build --release -p net-ffi --locked
warning: C:\Users\chief\Documents\git\net\net\crates\net\Cargo.toml: `panic` setting is ignored for `bench` profile
...
Finished `release` profile [optimized] target(s) in 7m 59s
[exit=0 elapsed=480s]
```

It produced:

```text
net.dll
net.dll.exp
net.dll.lib
net.pdb
```

After adding a cgo-compatible portable GCC, `go vet ./...` passed. The first link divergence was:

```text
$ go build ./...
... link.exe: running gcc failed: exit status 1
... gcc.exe ...
-LC:/Users/chief/go/pkg/mod/github.com/ai-2070/net/go@v0.0.0-20260809134659-300deeee6b2c/../net/crates/net/target/release -lnet
...
ld.exe: cannot find -lnet: No such file or directory
(repeated for each package cgo prelude)
collect2.exe: error: ld returned 1 exit status
[exit=1 elapsed=12s]
```

First divergence: the fetched module's linker path is relative to its module-cache source directory and resolves to a non-existent sibling checkout. The docs say to build the DLL but do not tell a fresh-module user how to connect that artifact to the fetched module. On the tested Windows path, Cargo's `net.dll.lib` is also not the GNU `libnet.dll.a` sought by `-lnet`.

Expected: after the documented unified-library build, a clean external module has a documented, direct compile/link command and a usable import artifact for the stated Windows compiler path.

Actual: the literal path cannot link. The user needs repository location knowledge, an undocumented `CGO_LDFLAGS`, and, for MinGW, an import-library conversion. The web page's “MSVC on Windows” prerequisite is not enough to make Go cgo use `cl`; the tested Go toolchain invokes its configured `CC` and defaults to `gcc`.

Minimal runnable reproduction: `install-latest/main.go`; deterministic command is `CGO_ENABLED=1 go build ./...` after the successful native build and with GCC on `PATH`.

Worked workaround, not part of the public path:

```text
$ gendef net.dll
[exit=0]
$ dlltool -d net.def -D net.dll -l libnet.dll.a
[exit=0]
$ CGO_LDFLAGS=-LC:/Users/chief/AppData/Local/Temp/net-go-usability-300deeee-20260809/cargo-target/release go build -o app.exe .
[exit=0 elapsed=7s]
```

Runtime has one more undocumented Windows edge:

```text
$ ./app.exe
[exit=127 elapsed=0s]

$ PATH=C:/Users/chief/AppData/Local/Temp/net-go-usability-300deeee-20260809/cargo-target/release:$PATH ./app.exe
accepted: ingested=1
[exit=0 elapsed=0s]
```

The missing-DLL launch emitted no stderr. The public C linking page gives `LD_LIBRARY_PATH` and `DYLD_LIBRARY_PATH`, but no Windows `PATH`/copy instruction. The Go README says only that the library must be available at runtime.

Implementation-source inspection required: NO to reproduce. Later localization found the repeated directive `#cgo LDFLAGS: -L${SRCDIR}/../net/crates/net/target/release -lnet` (for example `go/net.go:22`, `go/mesh_rpc.go:50`, `go/mcp.go:23`, and `go/compute.go:12`), explaining the module-cache path in the linker command.

### GO-03 — MAJOR — false success — the shipped Go and C examples ingest into the no-op default, poll zero events, and exit successfully

Confidence: HIGH.

User intent: perform the first useful event-bus loop promised by the Go package quickstart and C quickstart: ingest an event and poll it back.

Public path followed:

- `go/README.md` shows `net.New(nil)`, `IngestRaw`, then `Poll` and iterates returned events.
- generated package docs label the same operations “Ingest events” and “Poll events.”
- `go/example/main.go` announces that it will “Poll events,” prints every event, and ends with `Done!` without requiring a non-empty result.
- the C quickstart says “Ingest events and poll them back” and uses `net_init` with the default/no explicit adapter.

Exact repository Go example, wrapped only in an external Go 1.26 module requiring the exact candidate:

```text
$ go run .
Net version: 0.8.0
Event bus created with 32 shards
Ingested 3 events
Batch ingested 3 events
Stats: ingested=7, dropped=0
Polled 0 events (has_more=false)
Done!
[exit=0 elapsed=2s]
```

Minimal witness (`poll-default/main.go`):

```go
package main

import (
    "fmt"
    "log"
    "github.com/ai-2070/net/go"
)

func main() {
    bus, err := net.New(nil)
    if err != nil { log.Fatal(err) }
    defer bus.Shutdown()
    if err := bus.IngestRaw(`{"msg":"hello"}`); err != nil { log.Fatal(err) }
    out, err := bus.Poll(1, "")
    if err != nil { log.Fatal(err) }
    fmt.Printf("count=%d events=%d next=%q has_more=%v\n",
        out.Count, len(out.Events), out.NextID, out.HasMore)
}
```

```text
$ go run .
count=0 events=0 next="" has_more=false
[exit=0 elapsed=2s]
```

After fixing the C quickstart's independent compile defect in GO-04, its runtime result was also a clean, output-free success:

```text
$ PATH=<native release>:$PATH ./app.exe
[exit=0]
```

Expected: a page or file promising to ingest and poll events back should configure a readable adapter, or should fail/assert when the promised event is absent. If the intended result is acceptance into a no-op adapter, it should not present polling as the observable success condition.

Actual: both examples accept into the default no-op adapter, receive no event, and exit 0. The Go example prints `Done!`, making the failed user outcome look successful.

The docs do contain the missing fact, but only away from the primary examples: `web/src/content/docs/sdk/watch/go.md` says “memory counts events and discards them,” and the Rust core quickstart explicitly explains that default uses a no-op adapter and polling returns zero. That makes this example/page consistency drift, not an unknown runtime behavior.

Workaround, separate from defect: use the install-page acceptance-only check (`Stats().EventsIngested == 1`) and describe it as producer-side acceptance, or configure a readable Redis, JetStream, or mesh adapter for a real poll witness.

Implementation-source inspection required: NO. Public documentation already explains the default no-op behavior after the failure was frozen.

### GO-04 — MODERATE — documentation — the public C quickstart does not compile because it calls `free` without including `<stdlib.h>`

Confidence: HIGH.

User intent: compile the C quickstart from `web/src/content/docs/sdk/c/quickstart.md` or `net/crates/net/include/README.md`.

Public path: copy the snippet exactly, add the public include directory and the already-built unified library, and invoke the documented compiler family.

First divergence with GCC 16.1.0 occurs before linking:

```text
$ gcc -o app.exe app.c -I<public include> -L<release> -lnet
app.c: In function 'main':
app.c:24:9: error: implicit declaration of function 'free' [-Wimplicit-function-declaration]
   24 |         free(cursor);
      |         ^~~~
app.c:4:1: note: include '<stdlib.h>' or provide a declaration of 'free'
    3 | #include <string.h>
  +++ |+#include <stdlib.h>
    4 |
app.c:24:9: warning: incompatible implicit declaration of built-in function 'free' [-Wbuiltin-declaration-mismatch]
[exit=1 elapsed=0s]
```

The strict command fails identically:

```text
$ gcc -std=c11 -Wall -Wextra -Werror -o app-strict.exe app.c -I<public include> -L<release> -lnet
...
cc1.exe: all warnings being treated as errors
[exit=1]
```

Expected: the advertised standalone quickstart compiles as copied.

Actual: both public copies include `<stdio.h>` and `<string.h>` but omit the header declaring `free`.

Minimal reproduction: the initial `c-quickstart/app.c`, copied from the public page; deterministic command above.

Workaround, separate from defect: add `#include <stdlib.h>`. The same file then compiled successfully with exit 0.

Implementation-source inspection required: NO.

### GO-05 — MODERATE — documentation — generated Go reference and C header maps contradict the unified-library shipping model

Confidence: HIGH.

User intent: discover which native artifacts must be built and linked for nRPC, MCP, and the broader Go/C surface.

Public path and first divergence:

- `go/README.md:67-82` says one library deliberately serves every surface and warns that adding a second library recreates a historical hang.
- `net/crates/net/include/README.md:34-40` and the detailed linking page also say every header resolves from one `libnet`.
- `go doc github.com/ai-2070/net/go`, however, emits package documentation saying:

```text
Package net — MCP bridge ...
Compiled into the `libnet_mcp_ffi` cdylib (separate from `libnet`).
Build with `cargo build --release -p net-mcp-ffi`.

Package net — nRPC consumer wrapper ...
# Build prerequisites
- Build `libnet` as a cdylib (`cargo build --release -p net`).
- Build `libnet_rpc` as a cdylib (`cargo build --release -p net-rpc-ffi`).
- ... links against `-lnet_rpc -lnet`.
```

This is not an obscure source comment: it is emitted by the ordinary generated package-reference command and appears before the symbol list.

The C map also contradicts itself in reader-visible headings/prose:

```text
net/crates/net/include/README.md:14  "Ten headers, five libraries"
```

The table immediately below lists eleven headers and one `libnet`. The web linking page opens with “eleven, spread across five shared libraries,” then its table and later sections say every header uses the same one `libnet`.

Expected: all public discovery surfaces agree on the one-library requirement, particularly because the README describes multiple libraries as capable of causing a timing-dependent hang.

Actual: generated reference and the C entry pages direct users toward obsolete separate artifacts that the pinned `net-ffi` candidate no longer ships as the intended model.

Minimal reproduction:

```text
$ go doc github.com/ai-2070/net/go
[exit=0]
```

Workaround: follow `go/README.md` and build/link only `net-ffi`/`libnet`; disregard the generated package preamble and stale C headings.

Implementation-source inspection required: NO to find the contradiction. Later localization found the stale comments at `go/mesh_rpc.go:1-15` and `go/mcp.go:1-5`, while `net/crates/net/bindings/go/net-ffi/Cargo.toml:12-42` confirms the current single-cdylib intent.

### GO-06 — MODERATE — interoperability — `Version()` reports `0.8.0` from a 0.35.0 native candidate

Confidence: HIGH.

User intent: run the shipped example and identify the native runtime actually loaded.

Public path: execute `go/example/main.go`, whose first observable operation is `fmt.Printf("Net version: %s\n", net.Version())`.

First divergence:

```text
$ go run .
Net version: 0.8.0
...
[exit=0]
```

The native artifact was built from the required HEAD, whose unified native package manifest is version `0.35.0`. The public versioning page says bindings track the same number. The Go module itself is the exact pseudo-version for the pinned SHA.

Expected: the public native version function identifies the matching 0.35-era runtime, or clearly describes a separately versioned ABI value.

Actual: it returns the old release string `0.8.0`. This makes runtime telemetry and compatibility diagnostics report the wrong product release even though ABI verification succeeds.

Minimal reproduction: the first line of `example-copy/main.go`, or `fmt.Println(net.Version())` in any linked candidate consumer.

Positive control:

```text
$ go run <lifecycle witness>
abi=4 expected=4 second_shutdown=ok stats_is_shutting_down=true
[exit=0 elapsed=2s]
```

Thus the stale release string is distinct from the ABI guard; the loaded artifact passed `CheckABI()`.

Workaround: use the Go module pseudo-version/build metadata to identify the candidate and use `ABIVersion`/`CheckABI` for ABI compatibility. Do not rely on `Version()` for release identity on this SHA.

Implementation-source inspection required: NO to freeze. Later localization found `go/net.go:691-694` forwarding directly to the C ABI and `net/crates/net/src/ffi/mod.rs:1665-1666` hard-coding `b"0.8.0\0"`.

### GO-07 — MODERATE — installation — the checked Go example declares Go 1.21 while its replaced module requires Go 1.26

Confidence: HIGH.

User intent: run the repository's checked `go/example` exactly as supplied.

Public files:

```text
go/example/go.mod:  go 1.21
go/go.mod:          go 1.26
replace github.com/ai-2070/net/go => ..
```

First divergence on the installed Go 1.25.5 toolchain, even though `GOTOOLCHAIN=auto` was configured:

```text
$ go run .
go: module .. requires go >= 1.26 (running go 1.25.5)
[exit=1 elapsed=1s]
```

Forcing the already downloaded Go 1.26 toolchain reaches a second repository-edit requirement:

```text
$ GOTOOLCHAIN=go1.26.0 go run .
go: updates to go.mod needed; to update it:
        go mod tidy
[exit=1 elapsed=0s]
```

Expected: the checked example's manifest declares a toolchain compatible with its local replacement and runs without asking the user to modify the example.

Actual: its declared language version predates its dependency's minimum. The auto toolchain path that worked for `go get` in a fresh module does not make this local-replace example runnable as checked.

Minimal reproduction: `go/example/go.mod` plus `go/example/main.go`; deterministic command `go run .` from that directory.

Workaround: in an external copy, set `go 1.26`, require the exact candidate pseudo-version, run `go mod tidy`, and then run with the native workaround from GO-02. That produced the full output quoted under GO-03.

Implementation-source inspection required: NO.

## Clean/no-finding areas

The following tested areas behaved consistently once the installation workaround was explicit:

1. **Module resolution:** `go get github.com/ai-2070/net/go` resolved the exact pinned pseudo-version and exited 0. `go list -m -versions` returned no tagged versions, so the pinned candidate is reproducible by pseudo-version rather than a release tag.
2. **Automatic Go toolchain download for a fresh module:** the install journey moved the consumer from Go 1.25.5 to the Go 1.26 toolchain without manual installation.
3. **Native unified build:** `cargo build --release -p net-ffi --locked` succeeded from pinned HEAD with an external `CARGO_TARGET_DIR`.
4. **Type checking after a C compiler existed:** `CGO_ENABLED=1 go vet ./...` exited 0 without needing the shared library to link, matching the web install guide's intended vet-before-linking sequence.
5. **First producer-side operation:** with the documented candidate plus the explicit linker/loader workaround, the install-page acceptance check printed `accepted: ingested=1` and exited 0.
6. **ABI guard:** `CheckABI()` succeeded and runtime ABI 4 equaled `ExpectedABIVersion` 4.
7. **Lifecycle basics:** `Net.Shutdown()` succeeded twice; `Stats()` after shutdown matched `ErrShuttingDown`. No hang was observed in these bounded single-process operations.
8. **C ABI compile after the snippet correction:** adding only `<stdlib.h>` made the C quickstart compile and link against the unified library.
9. **Go error affordance checked on lifecycle path:** `errors.Is(err, net.ErrShuttingDown)` worked as documented.
10. **Public docs acknowledge important asymmetries:** the Go pages explicitly document cursor polling, structured-versus-string RPC error loss, context cancellation, the tool metadata announcement seam, and the no-op default adapter on the watch page. Those disclosures were not reported as new defects.

Not tested deeply enough for a clean claim: live two-node mesh handshake, live Go-to-Go tool invocation, streaming/cancellation over a peer connection, organization/subnet authority, external Redis/JetStream adapters, and named channels. The public docs themselves say the Go live-mesh tool round trip lacks a live test; this scan did not convert that disclosure into an unsupported finding.

## Source-informed localization (after black-box freeze)

This section is explanatory only and did not supply expected outcomes:

- GO-01: useful entry-point files are cgo files, explaining why `CGO_ENABLED=0` leaves a partial package.
- GO-02: repeated `#cgo LDFLAGS: -L${SRCDIR}/../net/crates/net/target/release -lnet` directives explain the impossible module-cache-relative search path.
- GO-05: stale package comments in `go/mesh_rpc.go` and `go/mcp.go` still describe the removed multi-cdylib model; the `net-ffi` manifest explicitly describes one cdylib.
- GO-06: the C ABI implementation hard-codes `0.8.0`, while Go simply returns that value.

Implementation-source inspection was not required to reproduce or freeze any finding. It was required only for the four localization statements above.

## Severity summary

| ID | Severity | Category | Confidence | First public-path divergence |
|---|---|---|---|---|
| GO-01 | MAJOR | diagnostics | HIGH | `go vet`/`go build` says documented API is undefined when cgo is disabled |
| GO-02 | MAJOR | installation | HIGH (tested path) | unified DLL exists, but fresh module links against a non-existent module-cache-relative directory and lacks a GNU import artifact |
| GO-03 | MAJOR | false success | HIGH | shipped examples poll zero events and exit 0 / print `Done!` |
| GO-04 | MODERATE | documentation | HIGH | copied C quickstart fails to compile at undeclared `free` |
| GO-05 | MODERATE | documentation | HIGH | generated reference and C headings direct readers to obsolete multi-library model |
| GO-06 | MODERATE | interoperability | HIGH | `Version()` reports `0.8.0` from the pinned 0.35.0 native candidate |
| GO-07 | MODERATE | installation | HIGH | checked example's Go 1.21 manifest cannot consume its Go 1.26 local replacement |
