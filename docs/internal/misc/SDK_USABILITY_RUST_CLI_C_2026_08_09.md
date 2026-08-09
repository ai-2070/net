# Rust SDK, CLI distributions, and C ABI developer-usability scan — 2026-08-09

## Verdict

Discovery-only black-box scan at pinned HEAD `300deeee6b2ceb55507cb8101289a9c992b0a37f`.

I found two major false-success paths, five moderate onboarding/documentation defects, and one minor public-surface contradiction:

1. **MAJOR / false success:** `net-mesh snapshot get` with the documented optional/no-config setup creates and reports a fresh empty in-process runtime instead of attaching to an existing node. It exits 0 and emits plausible snapshot JSON; the only warning concerns an ephemeral identity, not the absence of a target.
2. **MAJOR / false success:** the C quickstart says it ingests and polls an event back, but the default adapter accepts and discards it. Ingest, poll, shutdown, and process all report success while the poll count is zero.
3. **MODERATE / installation:** the Rust capability/tool path cannot compile after the documented install command because the guide imports the opt-in `macros` surface and direct `schemars` crate without installing either.
4. **MODERATE / documentation:** the CLI quickstart's `net-mesh snapshot show` command does not exist in either released 0.34.0 or candidate 0.35.0; both exit 2.
5. **MODERATE / installation:** the C quickstart omits `<stdlib.h>` while calling `free`, so the published program fails a strict C11 compile.
6. **MODERATE / installation/configuration:** the C linking guide claims Windows support but documents loader setup only for Linux/macOS. The linked Windows executable exits 127 until `net.dll` is put on `PATH` or beside the executable.
7. **MODERATE / documentation:** the pinned install pages call 0.33 the current published release while all three queried CLI registries and crates.io's Rust SDK expose 0.34.0; HEAD itself is the 0.35.0 candidate.
8. **MINOR / discovery:** the public C header calls itself “one header ... the entire C SDK,” while the C overview exposes eleven headers; adjacent public headings also say “ten headers, five libraries” and “eleven ... five shared libraries” even though their own tables and prose say every symbol resolves from one `libnet`.

No production/package source was repaired. No commit, push, issue, PR, release, publication, deployment, or external communication was performed.

## Audit identity and boundary

- Audited repository: `C:/Users/chief/Documents/git/net`
- Required/audited HEAD: `300deeee6b2ceb55507cb8101289a9c992b0a37f`
- Branch: `master`
- Candidate package versions at HEAD:
  - `net-mesh` 0.35.0
  - `net-mesh-sdk` 0.35.0
  - `net-cli` / `net-mesh` 0.35.0
  - `@net-mesh/cli` candidate manifest 0.35.0
  - `net-mesh-cli` candidate manifest 0.35.0
  - `net-ffi` 0.35.0
- Released versions independently resolved on 2026-08-09:
  - crates.io `net-mesh-sdk`: 0.34.0
  - crates.io `net-cli`: 0.34.0
  - npm `@net-mesh/cli`: 0.34.0
  - PyPI `net-mesh-cli`: 0.34.0
- Clean starting state: `git status --short` returned no entries before the scan.
- Repository write boundary: only this report was authored. Builds and consumer projects used external temporary directories and external `CARGO_TARGET_DIR` locations.
- Named-channel functionality was excluded because that scope is already frozen elsewhere. Incidental command names in top-level help were not investigated.
- I did not inspect prior review packets, Hermes memories, private plans, implementation source, or internal tests before freezing the black-box witnesses.
- After witnesses were frozen, implementation source inspection was limited to CLI localization described in finding CLI-2.

During the scan, three unrelated untracked report files appeared in the working tree:

```text
?? docs/internal/misc/SDK_USABILITY_GO_2026_08_09.md
?? docs/internal/misc/SDK_USABILITY_PYTHON_2026_08_09.md
?? docs/internal/misc/SDK_USABILITY_TYPESCRIPT_2026_08_09.md
```

They were not present in the initial status, were not opened, and were not modified by this scan.

## Environment

```text
OS: Microsoft Windows [Version 10.0.26100.8875]
Shell layer: MINGW64_NT-10.0-26100, bash/MSYS
CPU target: x86_64-pc-windows-msvc
rustc: 1.97.1 (8bab26f4f 2026-07-14)
cargo: 1.97.1 (c980f4866 2026-06-30)
Python: 3.11.15
pip: 26.1.2
Node.js: v24.18.0
npm: 12.0.1
Zig/Clang C frontend used for the external C consumer: ziglang 0.16.0
Native gcc/clang/cmake on initial PATH: absent
```

Zig was installed into the disposable venv at `C:/Users/chief/AppData/Local/Temp/net-usability-300deeee-v2/c-toolchain`; it was not installed system-wide.

## Public material followed before source inspection

Repository-public material at the pinned SHA:

- `README.md`
- `net/crates/net/README.md`
- `net/crates/net/Cargo.toml`
- `net/crates/net/sdk/README.md`
- `net/crates/net/sdk/Cargo.toml`
- `net/crates/net/sdk/examples/hello.rs`
- `net/crates/net/sdk/examples/tool_calling.rs`
- `net/crates/net/cli/README.md`
- `net/crates/net/cli/Cargo.toml`
- `net/crates/net/cli/npm/{README.md,package.json,bin/net-mesh.js}`
- `net/crates/net/cli/python/{README.md,pyproject.toml}`
- `net/crates/net/include/{README.md,net.h,net.go.h}` and the remaining public headers
- `net/crates/net/examples/basic.c`
- rendered-site sources under:
  - `web/src/content/docs/start/install/`
  - `web/src/content/docs/start/quickstart.md`
  - `web/src/content/docs/sdk/{rust,quickstart,announce,discover,invoke,c}/`
  - `web/src/content/docs/reference/cli.md`
- CLI `--help` at released 0.34.0 and candidate 0.35.0
- crates.io/npm/PyPI ordinary registry surfaces

Public URLs represented by those sources:

- <https://ai2070.net/docs/start/install/rust>
- <https://ai2070.net/docs/sdk/rust/quickstart>
- <https://ai2070.net/docs/sdk/rust/announce>
- <https://ai2070.net/docs/sdk/rust/discover>
- <https://ai2070.net/docs/sdk/rust/invoke>
- <https://ai2070.net/docs/sdk/c/quickstart>
- <https://ai2070.net/docs/sdk/c/headers-and-linking>
- <https://ai2070.net/docs/sdk/c/memory-and-threading>
- <https://ai2070.net/docs/reference/cli>
- <https://docs.rs/net-mesh-sdk>

## Attempted journeys

| Journey | Artifact | End-to-end result |
|---|---|---|
| Rust bus install → build → emit → shutdown | HEAD candidate 0.35.0 path package | Passed; one receipt printed, exit 0 in 156.778 s cold build/run. |
| Rust tool announce after documented install | HEAD candidate 0.35.0 | Failed to compile: missing `macros`, `schemars`, and generated register function; exit 101. |
| Rust two-node announce → fold → discover → invoke | HEAD candidate 0.35.0 with explicit workaround deps/features | Passed; one tool discovered and invoked, exit 0 in 25.810 s incremental build/run. |
| npm distribution install → help/version | released 0.34.0 | Passed; two packages installed, help/version exit 0. |
| PyPI wheel install → help/version | released 0.34.0 `py3-none-win_amd64` wheel | Passed; 4.0 MB wheel, help/version exit 0. |
| Cargo candidate install → help/version | HEAD candidate 0.35.0 | Passed; installed external `net-mesh.exe`, help/version exit 0. |
| CLI identity generation | released npm 0.34.0 | Passed to an explicit disposable output path, artifact existed, exit 0. |
| CLI quickstart snapshot command | released 0.34.0 and candidate 0.35.0 | `snapshot show` failed at argument parsing, exit 2. |
| CLI nearest valid snapshot command with no config | candidate 0.35.0 | Returned plausible empty snapshot from a new runtime, exit 0: false success. |
| C candidate library build | `net-ffi` 0.35.0 | Passed in external target dir; `net.dll` and `net.dll.lib` produced. |
| C quickstart strict compile | public quickstart + candidate header | Failed on undeclared `free`, exit 1. |
| C link | quickstart plus `<stdlib.h>` workaround | Passed with `-lnet`, exit 0. |
| C run without undocumented Windows loader setup | candidate `net.dll` | Loader failed to find `net.dll`, exit 127. |
| C lifecycle after PATH workaround | candidate 0.35.0 | init/ingest/poll/shutdown all returned success, but poll count was 0; process exit 0. |

## Findings

### RUST-1 — documented capability onboarding omits two required install choices

- **Severity:** MODERATE
- **Confidence:** HIGH
- **Category:** installation / documentation
- **Versions/environment:** HEAD `300deeee6b2ceb55507cb8101289a9c992b0a37f`; `net-mesh-sdk` 0.35.0 candidate; Rust 1.97.1; Windows 10 x86_64.
- **User intent:** follow the Rust SDK spine from install/quickstart to the first useful headline operation: register and announce a callable tool.
- **Public path followed:** `cargo add net-mesh-sdk tokio serde serde_json` from the Rust quickstart, then the Rust announce fragment importing `net_sdk::macros::tool` and `schemars::JsonSchema`.
- **First divergence:** `cargo add` explicitly reported `macros` disabled, and the next public page required that module and a direct `schemars` dependency without adding either.
- **Minimal runnable reproduction:** `C:/Users/chief/AppData/Local/Temp/net-usability-300deeee-v2/rust-announce`
- **Evidence files:** `check.stdout.txt`, `check.stderr.txt`, `Cargo.toml`, `src/main.rs` in that directory.

Commands:

```sh
cargo init --bin --name rust_announce C:/Users/chief/AppData/Local/Temp/net-usability-300deeee-v2/rust-announce
cargo add net-mesh-sdk --path C:/Users/chief/Documents/git/net/net/crates/net/sdk
cargo add tokio serde serde_json
cargo check
```

Observed terminal result after 69.869 s:

```text
error[E0432]: unresolved import `net_sdk::macros`
note: ... the item is gated behind the `macros` feature
error[E0432]: unresolved import `schemars`
help: ... use `cargo add schemars`
error[E0425]: cannot find function `web_search_register` in this scope
COMMAND_EXIT=101
```

- **Expected:** the language-specific install command should install everything used by the next first-party capability step, or that step should begin with the additional feature/dependency command.
- **Actual:** the local bus quickstart works, but the first advertised tool operation cannot compile from the documented install state.
- **Diagnostics quality:** the compiler correctly names both missing pieces. This keeps the issue moderate rather than major, but the user must infer a feature/dependency recipe not stated on the path.
- **Workaround (separate from defect):** install `net-mesh-sdk` with `--features macros` and add `schemars`. The verified two-node workaround at `.../rust-tool-calling` discovered and invoked one tool:

```text
tools=1 response=WebSearchResp { results: ["first hit for 'capability folds'"] }
COMMAND_EXIT=0
```

- **Implementation-source inspection required:** No. The public package manifest, `cargo add` feature listing, and compiler diagnostics fully establish the defect.

### CLI-1 — quickstart uses a nonexistent `snapshot show` subcommand

- **Severity:** MODERATE
- **Confidence:** HIGH
- **Category:** documentation / diagnostics
- **Versions/environment:** released npm/PyPI CLI 0.34.0 and HEAD candidate CLI 0.35.0; Windows 10 x86_64.
- **User intent:** perform the README's first read-only useful operation after installation.
- **Public path followed:** CLI README quickstart: `net-mesh snapshot show`.
- **First divergence:** both binaries reject `show`; `snapshot --help` lists only `get` and `status`.
- **Minimal runnable reproductions:** released npm at `.../cli-npm-release`; candidate at `.../cli-candidate-install`.

Released command/result:

```sh
./node_modules/.bin/net-mesh snapshot show
```

```text
error: unrecognized subcommand 'show'
Usage: net-mesh snapshot [OPTIONS] <COMMAND>
COMMAND_EXIT=2
ELAPSED_SECONDS=0.229
```

Candidate command/result:

```sh
C:/Users/chief/AppData/Local/Temp/net-usability-300deeee-v2/cli-candidate-install/bin/net-mesh.exe snapshot show
```

```text
error: unrecognized subcommand 'show'
Usage: net-mesh snapshot [OPTIONS] <COMMAND>
COMMAND_EXIT=2
ELAPSED_SECONDS=0.030
```

- **Expected:** a copy/pasted quickstart command should execute, or the README should say `snapshot get`/`snapshot status`.
- **Actual:** parse failure before any operation.
- **Workaround (separate from defect):** `net-mesh snapshot --help` identifies `get` and `status`; however, using `get` without an attached runtime exposes CLI-2.
- **Implementation-source inspection required:** No. Public README and `--help` are sufficient.

### CLI-2 — no-config snapshot inspection silently reports a fresh local runtime

- **Severity:** MAJOR
- **Confidence:** HIGH
- **Category:** false success / configuration / diagnostics
- **Versions/environment:** HEAD candidate `net-cli` 0.35.0; Windows 10 x86_64.
- **User intent:** inspect the local or configured Net deployment after installation.
- **Public path followed:** the README says the profile is optional, shows `endpoint = "in-process"`, and describes snapshot as a one-shot `MeshOsSnapshot` read. After CLI-1, `snapshot --help` identifies `get` as the valid spelling.
- **First divergence:** with no config and no surrounding node, the command does not report “not attached” or “no target.” It starts a new runtime, serializes its empty state, and exits 0.
- **Minimal runnable reproduction:** `C:/Users/chief/AppData/Local/Temp/net-usability-300deeee-v2/cli-candidate-install`; evidence `snapshot-get.stdout.txt` and `snapshot-get.stderr.txt`.

Command:

```sh
C:/Users/chief/AppData/Local/Temp/net-usability-300deeee-v2/cli-candidate-install/bin/net-mesh.exe snapshot get
```

Observed in 0.039 s:

```json
{
  "daemons": {},
  "replicas": {},
  "peers": {},
  "avoid_list": {},
  "local_maintenance": "Active",
  "recently_emitted": [],
  "recent_failures": [],
  "freeze_remaining_ms": null,
  "admin_audit": [],
  "log_ring": [],
  "in_flight_migrations": [],
  "runtime_epoch_id": 10703544217438741654
}
```

```text
WARN ... no operator identity configured; using an ephemeral keypair ...
COMMAND_EXIT=0
```

- **Expected:** a cluster-inspection command should read an existing target or explicitly fail/name the missing attachment layer. If “in-process” intentionally means “create a disposable isolated runtime,” stdout/stderr must say so before presenting the result as the freshest snapshot.
- **Actual:** valid-looking empty cluster state and success. The identity warning can misdirect the user into believing identity is the only missing prerequisite.
- **Workaround (separate from defect):** none for `snapshot get` was exposed by its help. Remote-attach flags are available only on selected other command groups; the public reference says a surrounding environment must provide a local node, but the command does not detect its absence.
- **Implementation-source inspection required:** Not to establish the failure; **yes, after freeze, for localization only.** `cli/src/commands/snapshot.rs:61-66` calls `CliContext::build` and immediately reads `ctx.deck().status()`. `cli/src/context.rs:209-216` constructs `MeshOsDaemonSdk::start(...)` and a `DeckClient` from that new runtime. No attachment check precedes output. This diagnosis was made only after the black-box output was frozen.

### C-1 — public quickstart is not a complete strict-C program

- **Severity:** MODERATE
- **Confidence:** HIGH
- **Category:** documentation / interoperability
- **Versions/environment:** HEAD public `net.h` and C quickstart; candidate `net-ffi` 0.35.0; ziglang 0.16.0 C11 frontend on Windows 10.
- **User intent:** copy the published C quickstart into a clean consumer project and compile it.
- **Public path followed:** exact includes and body from `docs/sdk/c/quickstart`, compiled with the public header.
- **First divergence:** the snippet calls `free(cursor)` but includes only `net.h`, `<stdio.h>`, and `<string.h>`.
- **Minimal runnable reproduction:** `C:/Users/chief/AppData/Local/Temp/net-usability-300deeee-v2/c-quickstart/app.c`; frozen compiler output in `compile.stderr.txt`.

Command:

```sh
python -m ziglang cc -std=c11 -Wall -Wextra -Werror \
  -IC:/Users/chief/Documents/git/net/net/crates/net/include \
  -c app.c -o app.o
```

Observed:

```text
app.c:24:9: error: call to undeclared library function 'free' ...
note: include the header <stdlib.h> or explicitly provide a declaration for 'free'
COMMAND_EXIT=1
ELAPSED_SECONDS=0.498
```

- **Expected:** the complete quickstart should compile under a conforming C11 toolchain.
- **Actual:** compile failure caused by a missing standard header.
- **Workaround (separate from defect):** add `#include <stdlib.h>`. The resulting object linked successfully.
- **Implementation-source inspection required:** No.

### C-2 — the C “ingest and poll back” quickstart succeeds with zero events

- **Severity:** MAJOR
- **Confidence:** HIGH
- **Category:** false success / API ergonomics / documentation
- **Versions/environment:** HEAD candidate `net-ffi`/`net.h` 0.35.0; Windows 10 x86_64.
- **User intent:** execute the C quickstart's promised loop: ingest an event and poll it back.
- **Public path followed:** `docs/sdk/c/quickstart` title/intro and its `net_init` → `net_ingest_raw` → `net_poll_ex` → `net_shutdown` code.
- **First divergence:** every API reports success, but the default bus returns no event. The exact quickstart prints nothing and exits 0.
- **Minimal runnable reproduction:** exact program `.../c-quickstart/app.c`; count-instrumented reduction `.../c-quickstart/app_count.c`; evidence `count-run.stdout.txt`.

Compile/run of the reduced witness:

```sh
python -m ziglang cc -std=c11 -Wall -Wextra -Werror \
  -IC:/Users/chief/Documents/git/net/net/crates/net/include app_count.c \
  -LC:/Users/chief/AppData/Local/Temp/net-usability-300deeee-v2/cli-candidate-target/release \
  -lnet -o app_count.exe
PATH=/c/Users/chief/AppData/Local/Temp/net-usability-300deeee-v2/cli-candidate-target/release:$PATH \
  ./app_count.exe
```

Observed:

```text
ingest_rc=0 poll_rc=0 count=0
shutdown_rc=0
RUN_EXIT=0
```

- **Expected:** based on “Ingest events and poll them back,” “A NULL cursor starts from the earliest buffered event,” and the included poll loop, either the event should be printed or the sample should explicitly assert/explain a zero count.
- **Actual:** accepted-and-discarded data, empty poll, no diagnostic, success exit.
- **Conflicting public disclosure:** the shared install and core Rust quickstart do explain that the memory/default adapter discards events. The C quickstart's promise and comments do not carry that constraint into the copied program, so a reader following the boundary-native page gets false success despite the disclosure elsewhere.
- **Workaround (separate from defect):** treat the sample only as an acceptance/lifecycle smoke test and check stats, or configure a retaining adapter/real mesh before promising poll-back behavior.
- **Implementation-source inspection required:** No. Public docs disclose the default adapter semantics; execution proves the contradictory C outcome.

### C-3 — Windows runtime loader setup is absent from the linking guide

- **Severity:** MODERATE
- **Confidence:** HIGH
- **Category:** installation / configuration / interoperability
- **Versions/environment:** candidate `net-ffi` 0.35.0; Windows 10 x86_64; ziglang 0.16.0.
- **User intent:** build, link, and run a C consumer against the advertised Windows `net.dll`.
- **Public path followed:** build `cargo build --release -p net-ffi`; compile/link with `-L ... -lnet`; then follow the guide's runtime loader section.
- **First divergence:** the runtime section provides only `LD_LIBRARY_PATH` and `DYLD_LIBRARY_PATH`. With the executable in a clean external directory and the DLL left in `target/release`, Windows cannot load it.
- **Minimal runnable reproduction:** `.../c-quickstart/app.exe`; candidate artifacts under `.../cli-candidate-target/release`.

Observed without a loader workaround:

```sh
./app.exe
```

```text
app.exe: error while loading shared libraries: net.dll: cannot open shared object file: No such file or directory
COMMAND_EXIT=127
ELAPSED_SECONDS=0.109
```

After placing the external target's release directory on the MSYS-visible `PATH`, the same executable ran and exited 0.

- **Expected:** the Windows row should include its runtime loader/import-library instructions just as Linux and macOS do.
- **Actual:** Windows is listed as a produced artifact, but its required run step is omitted.
- **Workaround (separate from defect):** put `net.dll` beside the executable or add its directory to Windows `PATH` before launch.
- **Implementation-source inspection required:** No.

### DOC-1 — “current published release” is one release behind every queried registry

- **Severity:** MODERATE
- **Confidence:** HIGH
- **Category:** documentation / installation / interoperability
- **Versions/environment:** pinned docs at HEAD; registry queries on 2026-08-09.
- **User intent:** pin a mutually compatible Rust/CLI release as directed.
- **Public path followed:** `start/install/_shared.md` and `start/install/rust.md`, both saying 0.33 is the current published release and warning that mismatched layers are untested.
- **First divergence:** ordinary registry queries resolve 0.34.0, while HEAD package manifests are already 0.35.0 candidates.
- **Minimal deterministic commands:**

```sh
cargo info net-mesh-sdk
cargo info net-cli
npm view @net-mesh/cli version versions --json
python -m pip index versions net-mesh-cli
```

Observed heads:

```text
net-mesh-sdk version: 0.34.0
net-cli version: 0.34.0
@net-mesh/cli version: 0.34.0
net-mesh-cli (0.34.0)
```

- **Expected:** a page that labels a version “current” and uses it as a cross-layer compatibility instruction should match the registries or avoid a hard-coded current version.
- **Actual:** 0.33 is presented as current while 0.34 is available across all queried release surfaces.
- **Workaround (separate from defect):** query each registry and pin matching 0.34.0 packages; for this candidate audit, use all 0.35.0 path artifacts from the exact HEAD.
- **Implementation-source inspection required:** No.

### C-4 — the public C artifact map contradicts itself

- **Severity:** MINOR
- **Confidence:** HIGH
- **Category:** discovery / documentation
- **Versions/environment:** public docs/headers at pinned HEAD.
- **User intent:** determine how many headers/libraries must be packaged and linked.
- **Public path followed:** `include/net.h`, `include/README.md`, and `docs/sdk/c/headers-and-linking`.
- **First divergence:** mutually incompatible public statements appear before any code is written:
  - `net.h`: “One header, one shared library. This is the entire C SDK.”
  - C overview heading: “Ten headers, five libraries,” followed by eleven rows all mapped to `libnet`.
  - linking page: “eleven, spread across five shared libraries,” followed by “Every header resolves out of the same `libnet`.”
- **Minimal verification command:** `cargo build --release -p net-ffi` produced one `net.dll` plus its import library, confirming the latter one-library model for this candidate.
- **Expected:** one canonical count/topology across header banner, overview, and linking page.
- **Actual:** header count and library count disagree in the first-screen discovery material.
- **Workaround (separate from defect):** for 0.35.0, package the eleven public headers and link the single `libnet`/`net.dll` artifact.
- **Implementation-source inspection required:** No.

## Clean/no-finding areas

- **Rust local lifecycle:** the candidate Rust bus quickstart built and executed from a clean external crate. `emit` returned a receipt and `shutdown().await` exited cleanly; no hang or shutdown diagnostic defect was reproduced.
- **Rust useful operation with explicit prerequisites:** after enabling `macros` and adding `schemars`, a two-node loop completed handshake, started both nodes, announced a tool, waited for the fold, discovered it, invoked it, and exited 0.
- **CLI release packaging:** npm 0.34.0 installed its Windows platform package and exposed `net-mesh`; the PyPI 0.34.0 Windows wheel installed without compiling and exposed the same executable. `--help` and `version` exited 0 on both.
- **CLI candidate Cargo packaging:** `cargo install --path .../cli --root <external>` completed and installed candidate 0.35.0 outside the repository. `version` reported CLI and SDK 0.35.0.
- **CLI offline identity authoring:** `identity generate --out <disposable path>` created the file and exited 0. No private key material is reproduced in this report.
- **CLI parse diagnostics:** the stale `snapshot show` spelling fails promptly with exit 2 and names the unrecognized subcommand; it does not hang.
- **C candidate build/link:** `cargo build --release -p net-ffi --locked` succeeded in an external target directory. After adding the missing standard header and Windows loader setup, a consumer linked and ran.
- **C lifecycle:** `net_init`, `net_ingest_raw`, `net_poll_ex`, `net_free_poll_result`, and `net_shutdown` completed without a crash or shutdown hang in the reduced witness.
- **Scope exclusion:** no named-channel behavior was audited or re-reported.

## Reproduction asset index

All consumer assets are outside the repository under:

`C:/Users/chief/AppData/Local/Temp/net-usability-300deeee-v2`

- `rust-sdk-quickstart/` — passing local bus journey and cold build/run logs.
- `rust-announce/` — frozen failing Rust announce compile.
- `rust-tool-calling/` — passing two-node workaround journey.
- `cli-npm-release/` — npm 0.34.0 install/help/version/identity/snapshot logs.
- `cli-pypi-release/` — PyPI 0.34.0 wheel install/help/version logs.
- `cli-candidate-install/` — Cargo-installed candidate 0.35.0 and snapshot false-success logs.
- `cli-candidate-target/` — external Cargo target containing candidate `net.dll`/import library.
- `c-toolchain/` — disposable Zig 0.16.0 toolchain venv.
- `c-quickstart/` — exact C quickstart, frozen strict-compile failure, loader failure, and count-instrumented lifecycle witness.

## Final repository-state note

The only file intentionally created by this scan is:

`docs/internal/misc/SDK_USABILITY_RUST_CLI_C_2026_08_09.md`

The unrelated untracked Go/Python/TypeScript reports listed above were left untouched.
