# Python SDK / Python-distributed CLI and Deck usability scan — 2026-08-09

## Result

Discovery-only black-box scan of pinned HEAD `300deeee6b2ceb55507cb8101289a9c992b0a37f` found one BLOCKER, two MAJOR findings, one MODERATE finding, and one MINOR finding.

| ID | Severity | Category | Confidence | Summary |
|---|---|---|---|---|
| PY-01 | BLOCKER | installation | High | The 0.35.0 repository candidate `net-mesh-sdk` has an empty dependency range and cannot be installed. |
| PY-02 | MAJOR | lifecycle / documentation | High | The published Python mesh handshake is sequential but `accept()` blocks, so the documented first two-node journey times out before `connect()` can run. |
| PY-03 | MAJOR | diagnostics / discovery | High | `net-deck` ignores help, version, and invalid arguments, enters the TUI even without a TTY, and waits indefinitely. |
| PY-04 | MODERATE | documentation / configuration | High | The live install docs say Python packages are at 0.33 although PyPI installs 0.34.0, undermining the same-version guidance. |
| PY-05 | MINOR | diagnostics | High | `net-mesh --help` exposes an internal plan name and source-code archaeology instead of user-facing help. |

No production code was repaired. No package source, commit, branch, issue, PR, release, deployment, or external communication was changed or created. The only repository file written by this scan is this report.

## Audited revision, scope, and boundary

- Repository: `C:/Users/chief/Documents/git/net`
- Audited HEAD: `300deeee6b2ceb55507cb8101289a9c992b0a37f`
- HEAD subject: merge of PR #797, `SDK Bugs`
- Starting state: `git status --short` was empty.
- Primary package metadata and public surfaces:
  - `net/crates/net/bindings/python/{README.md,pyproject.toml,python/net/_net.pyi}` — candidate `net-mesh==0.35.0`, import `net`
  - `net/crates/net/sdk-py/{README.md,pyproject.toml}` — candidate `net-mesh-sdk==0.35.0`, import `net_sdk`
  - `net/crates/net/cli/python/{README.md,pyproject.toml}` — candidate `net-mesh-cli==0.35.0`, executable `net-mesh`
  - `net/crates/net/deck/python/{README.md,pyproject.toml}` — candidate `net-deck==0.35.0`, executable `net-deck`
  - `README.md`, public website pages/fragments under `web/src/content/docs`, live `https://ai2070.net/docs`, PyPI metadata, installed package metadata, public signatures, stubs, and CLI help
- Public documentation captured on 2026-08-09:
  - `https://ai2070.net/docs/start/install/`
  - `https://ai2070.net/docs/sdk/python/quickstart`
  - `https://ai2070.net/docs/sdk/python/announce`
  - `https://ai2070.net/docs/sdk/python/errors`
  - `https://pypi.org/project/net-mesh/`
  - `https://pypi.org/project/net-mesh-sdk/`
  - `https://pypi.org/project/net-mesh-cli/`
  - `https://pypi.org/project/net-deck/`
- Explicitly excluded: internal plans, prior audit/review packets, internal tests, named-channel semantic audit scope already frozen elsewhere, and implementation source until the corresponding black-box witnesses were frozen.
- There are no dedicated tracked Python example files under the four package directories at this HEAD; the public runnable surface is primarily README and generated website snippets.

After PY-01 through PY-03 were frozen, implementation source was inspected only to localize those failures. That later inspection is separated under each finding.

## Environment and clean consumer state

- Host: Microsoft Windows `10.0.26100.8875`, x86_64
- Shell layer: MSYS2 `MINGW64_NT-10.0-26100`
- Clean-test interpreter selected by uv: CPython `3.11.14` x86_64 at `C:/Users/chief/AppData/Roaming/uv/python/cpython-3.11.14-windows-x86_64-none/python.exe`
- Host agent interpreter (not used for final isolated package results): CPython `3.11.15`
- `uv 0.9.7`
- `pip 26.1.2` on the host; final isolated journeys used `uv pip`, not host pip
- `rustc 1.97.1`, `cargo 1.97.1`
- `git 2.55.0.windows.3`
- Test root outside the repository: `C:/Users/chief/AppData/Local/Temp/net-python-usability-300deeee`
- Every reported journey used a separate uv-created virtual environment under that test root.
- Candidate Rust builds set `CARGO_TARGET_DIR` under the external test project, so Cargo output did not write into the checkout.

Wrong turn preserved: the first pass used `python -m venv` from the Hermes-hosted interpreter. Its pip reported the Hermes environment as an outside installation and `pip freeze` exposed host packages, so it was not accepted as a clean consumer environment. Those environments were deleted and all reported package results were rerun from fresh `uv venv --python 3.11` environments using explicit `.venv/Scripts/python.exe` paths.

## Attempted journeys

| Journey | Version / provenance | End-to-end result |
|---|---|---|
| Install the repository candidate ergonomic SDK | HEAD candidate `net-mesh-sdk==0.35.0` | Failed deterministically in dependency resolution (PY-01). |
| Install the released SDK and run the public one-node acceptance check | PyPI `net-mesh-sdk==0.34.0`, `net-mesh==0.34.0` | Passed: printed `accepted: ingested=1`, clean context-manager exit. |
| Run the raw binding README example | PyPI `net-mesh==0.34.0` | Passed: two ingests, zero drops; poll returned zero events as the docs warn for the no-op adapter. |
| Run the public two-node Python handshake and shutdown | PyPI 0.34.0 plus live/pinned public docs | Failed at `host.accept(...)` before `agent.connect(...)` (PY-02). A threaded workaround completed and shut down. |
| Install and inspect the released CLI | PyPI `net-mesh-cli==0.34.0` | Wheel install, `--help`, `version`, and isolated identity generation passed. Help quality has PY-05. |
| Install the repository candidate CLI from its Python package | HEAD candidate `net-mesh-cli==0.35.0` | Source build succeeded in 555 s; `net-mesh version` returned CLI/SDK 0.35.0. |
| Install and probe released Deck | PyPI `net-deck==0.33.0` | Wheel install passed; help/version/invalid-argument probes all entered the TUI and timed out (PY-03). |
| Install and probe candidate Deck | HEAD candidate `net-deck==0.35.0` | Source build completed; `--help` again entered the TUI and timed out (PY-03). |
| Compare public version guidance to registry versions | Live docs, PyPI on 2026-08-09 | Docs report 0.33; core/SDK/CLI latest are 0.34.0 (PY-04). |

## Findings

### PY-01 — Candidate `net-mesh-sdk` cannot be installed

- Severity: **BLOCKER**
- Category: **installation**
- Confidence: **High**
- Versions/environment: HEAD `300deeee6b2ceb55507cb8101289a9c992b0a37f`; candidate `net-mesh-sdk==0.35.0`; Windows x86_64; CPython 3.11.14; uv 0.9.7.
- User intent: install the ergonomic Python SDK from the explicitly supplied repository candidate and import `net_sdk`.
- Public path followed: the top-level README and `net/crates/net/sdk-py/README.md` say `pip install net-mesh-sdk`; for the supplied candidate, install its public PEP 517 project directory.
- First divergence: dependency resolution, before a wheel is installed or an import can be attempted.

Minimal runnable reproduction:

```bash
cd C:/Users/chief/AppData/Local/Temp/net-python-usability-300deeee/candidate_sdk
bash repro.sh
```

`repro.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
ROOT='C:/Users/chief/Documents/git/net'
uv venv --python 3.11 .venv
uv pip install --python .venv/Scripts/python.exe "$ROOT/net/crates/net/sdk-py"
```

Observed result (1 s):

```text
Resolved 1 package in 2ms
× No solution found when resolving dependencies:
╰─▶ Because only net-mesh-sdk==0.35.0 is available and net-mesh-sdk==0.35.0
    depends on net-mesh ∅, we can conclude that all versions of net-mesh-sdk
    cannot be used.
```

- Exit code: `1`
- Expected: install `net-mesh-sdk==0.35.0` together with its matching `net-mesh==0.35.0` candidate, after which `import net_sdk` is possible.
- Actual: the candidate metadata declares `net-mesh>=0.35.0,<0.35.0`, an empty set. No version can satisfy both bounds.
- Workaround (separate from defect): no ordinary resolver-supported workaround exists. `--no-deps` could bypass metadata only if the caller separately builds and installs the exact native 0.35.0 candidate; that is unsupported manual assembly and was not treated as installation success.
- Implementation-source inspection required: **No.** The failure and empty range are visible in PEP 621 package metadata (`net/crates/net/sdk-py/pyproject.toml:22`) and resolver diagnostics. No implementation source was needed.
- Evidence assets:
  - `C:/Users/chief/AppData/Local/Temp/net-python-usability-300deeee/candidate_sdk/repro.sh`
  - `C:/Users/chief/AppData/Local/Temp/net-python-usability-300deeee/candidate_sdk/transcript.txt`
  - `C:/Users/chief/AppData/Local/Temp/net-python-usability-300deeee/candidate_sdk/result.txt`

### PY-02 — Published two-node handshake cannot run sequentially

- Severity: **MAJOR**
- Category: **lifecycle / documentation**
- Confidence: **High**
- Versions/environment: public docs at HEAD and live URL on 2026-08-09; released `net-mesh-sdk==0.34.0` + `net-mesh==0.34.0`; Windows x86_64; CPython 3.11.14.
- User intent: perform the first useful distributed Python operation: connect two local `MeshNode` instances, start them, then shut them down.
- Public path followed literally: `https://ai2070.net/docs/sdk/python/quickstart`, mirrored at `web/src/content/docs/sdk/quickstart/python.md:49-60`:

```python
host.accept(agent.node_id)
agent.connect(HOST_ADDR, host.public_key, host.node_id)
host.start()
agent.start()
```

- First divergence: the first line, `host.accept(agent.node_id)`, blocks waiting for a peer handshake. The next line that initiates that handshake cannot execute in the same thread.

Minimal runnable reproduction:

```bash
cd C:/Users/chief/AppData/Local/Temp/net-python-usability-300deeee/released_sdk
uv venv --python 3.11 .venv
uv pip install --python .venv/Scripts/python.exe net-mesh-sdk==0.34.0
.venv/Scripts/python.exe mesh_lifecycle.py
```

Observed result after approximately 20 s:

```text
Traceback (most recent call last):
  File ".../mesh_lifecycle.py", line 7, in <module>
    host.accept(agent.node_id)
  File ".../site-packages/net_sdk/mesh.py", line 183, in accept
    return self._native.accept(peer_node_id)
RuntimeError: accept: connection error: handshake timeout
```

- Exit code: `1`
- Expected: both calls complete, both receive loops start, and explicit `shutdown()` calls terminate cleanly.
- Actual: the responder times out before the initiator call is reached. This prevents the documented first distributed journey and blames a handshake timeout rather than explaining that the responder call is blocking and requires concurrency.
- Workaround (validated, separate from defect): run `host.accept(...)` in a second thread, then call `agent.connect(...)` from the main thread. `mesh_lifecycle_workaround.py` printed `handshake and start succeeded` and exited `0` in 1 s, including both shutdowns.
- Implementation-source inspection required: **No for the finding. Yes only for localization after freezing.** Later inspection showed `net/crates/net/sdk-py/src/net_sdk/mesh.py:188-198` presents synchronous `connect`, `accept`, and `start` methods and directly forwards `accept` into the blocking native call; its public docstring does not warn that concurrency is required.
- Evidence assets:
  - original witness: `C:/Users/chief/AppData/Local/Temp/net-python-usability-300deeee/released_sdk/mesh_lifecycle.py`
  - original transcript/result: `C:/Users/chief/AppData/Local/Temp/net-python-usability-300deeee/released_sdk/transcript.txt`, `result.txt`
  - validated workaround: `C:/Users/chief/AppData/Local/Temp/net-python-usability-300deeee/released_sdk/mesh_lifecycle_workaround.py` and its `.stdout.txt` / `.stderr.txt`

### PY-03 — Deck silently ignores argv and hangs in automation

- Severity: **MAJOR**
- Category: **diagnostics / discovery**
- Confidence: **High**
- Versions/environment: PyPI `net-deck==0.33.0` and HEAD candidate `net-deck==0.35.0`; Windows x86_64; CPython 3.11.14; stdin redirected from `/dev/null`; stdout captured (non-TTY).
- User intent: discover usage/version or receive a diagnostic for an invalid option without entering an interactive TUI.
- Public path followed: install from the Python-distributed surface, then use conventional executable discovery flags.
- First divergence: argument processing. `--help`, `-h`, `--version`, and `--bogus` are all silently ignored; each invocation initializes the alternate-screen TUI and keeps running.

Released-wheel probe:

```bash
cd C:/Users/chief/AppData/Local/Temp/net-python-usability-300deeee/deck
uv venv --python 3.11 .venv
uv pip install --python .venv/Scripts/python.exe net-deck==0.33.0
timeout 3s .venv/Scripts/net-deck.exe --help </dev/null
```

All four probes:

```text
--help    EXIT_CODE=124 ELAPSED_SECONDS=3 STDOUT_BYTES=5204 STDERR_BYTES=0
-h        EXIT_CODE=124 ELAPSED_SECONDS=4 STDOUT_BYTES=5204 STDERR_BYTES=0
--version EXIT_CODE=124 ELAPSED_SECONDS=3 STDOUT_BYTES=5204 STDERR_BYTES=0
--bogus   EXIT_CODE=124 ELAPSED_SECONDS=3 STDOUT_BYTES=5174 STDERR_BYTES=0
```

The candidate reproduced it after a successful source build:

```text
+ net-deck==0.35.0 (from file:///.../net/crates/net/deck/python)
<alternate-screen ANSI TUI output showing v0.35.0>
EXIT_CODE=124
```

- Expected: `--help`/`-h` prints usage and exits `0`; `--version` prints a machine-readable or plain version and exits `0`; an unknown option prints a usage diagnostic and exits `2`. At minimum, a non-TTY invocation should not wait forever without a diagnostic.
- Actual: every argument is a silent no-op. The process emits 5+ KiB of terminal control sequences to captured stdout and remains live until killed. This is both false argument acceptance and an unexplained automation hang.
- Workaround (separate from defect): obtain package version from PyPI/installed metadata; enter the TUI interactively and use `?` for in-app help or `q` to quit. There is no non-interactive executable help/version workaround.
- Implementation-source inspection required: **No for the finding. Yes for localization after freezing.** Later inspection of `net/crates/net/deck/src/main.rs` showed no argv parser or `std::env::args` use; `main()` immediately creates the runtime and initializes ratatui.
- Evidence assets:
  - minimal witness: `C:/Users/chief/AppData/Local/Temp/net-python-usability-300deeee/deck/minimal_help_repro.sh`
  - released probes: `deck_help.stdout.txt`, `deck_help.stderr.txt`, `deck_h.*`, `deck_version.*`, and `deck_bogus.*` in that directory
  - candidate witness: `C:/Users/chief/AppData/Local/Temp/net-python-usability-300deeee/candidate_deck/{repro.sh,transcript.txt,result.txt}`

### PY-04 — Public Python version guidance is stale

- Severity: **MODERATE**
- Category: **documentation / configuration**
- Confidence: **High**
- Versions/environment: live docs captured 2026-08-09; PyPI queried with pip 26.1.2.
- User intent: follow the public requirement to keep the native core and ergonomic SDK on the same published version.
- Public path followed:
  - `https://ai2070.net/docs/start/install/` says: “Every package publishes at the same version from the same commit. The current published release is 0.33.”
  - `web/src/content/docs/start/install/python.md:8` says both Python packages publish at 0.33.
  - query the package index before pinning.
- First divergence: registry discovery. Current core, SDK, and CLI are 0.34.0, while the docs still instruct 0.33.

Reproduction:

```bash
python -m pip index versions net-mesh-sdk
python -m pip index versions net-mesh
python -m pip index versions net-mesh-cli
python -m pip index versions net-deck
```

Observed, all exit `0`:

```text
net-mesh-sdk (0.34.0)
net-mesh (0.34.0)
net-mesh-cli (0.34.0)
net-deck (0.33.0)
```

- Expected: the live install page either derives the current release from a canonical release record or avoids a hard-coded “current” version; same-version instructions identify versions users can actually install.
- Actual: a time-sensitive hard-coded value is at least one release behind for both Python SDK layers and the Python-distributed CLI. The separately distributed Deck also trails those surfaces, despite the page presenting it alongside the install family.
- Workaround (separate from defect): query PyPI and explicitly pin `net-mesh-sdk==0.34.0` and `net-mesh==0.34.0`; treat Deck as independently versioned in practice.
- Implementation-source inspection required: **No.** Public docs and registry metadata are sufficient.
- Evidence: package-index output is preserved in this scan's tool transcript; released install transcripts under `released_sdk`, `cli`, and `deck` independently confirm the resolved versions.

### PY-05 — CLI help exposes internal implementation notes

- Severity: **MINOR**
- Category: **diagnostics**
- Confidence: **High**
- Versions/environment: released `net-mesh-cli==0.34.0`; candidate source at HEAD retains the same text.
- User intent: use `net-mesh --help` to discover supported operations and global flags.
- Public path followed: `pip install net-mesh-cli`, then `net-mesh --help` as documented by the package README.
- First divergence: the opening long help directs users to `NET_CLI_PLAN.md`, an internal repository plan not linked or shipped as CLI help. The `--no-color` entry then prints source-maintainer history including `Deliberately NOT env = "NO_COLOR"`, a former bug narrative, and source-level `main` behavior.

Reproduction:

```bash
cd C:/Users/chief/AppData/Local/Temp/net-python-usability-300deeee/cli
.venv/Scripts/net-mesh.exe --help
```

- Exit code: `0`
- Expected: self-contained operator-facing description of the full surface and concise behavior of `--no-color` / `$NO_COLOR`.
- Actual: help leaks an internal plan name and implementation archaeology, while using examples such as `net org keygen` even though the installed executable is `net-mesh`.
- Workaround: ignore the internal references and use nested `--help` output.
- Implementation-source inspection required: **No for the user-visible defect. Yes only for localization.** The emitted strings localize to `net/crates/net/cli/src/main.rs:49-54` and `:83-91`.
- Evidence assets: `C:/Users/chief/AppData/Local/Temp/net-python-usability-300deeee/cli/{repro.sh,transcript.txt,result.txt}`.

## Clean / no-finding areas

- Released wheel installation was fast and compilation-free on Windows x86_64:
  - `net-mesh-sdk==0.34.0` resolved with `net-mesh==0.34.0` and installed successfully.
  - `net-mesh-cli==0.34.0` and `net-deck==0.33.0` each installed as a single platform wheel.
- Package/import naming is unusual but clearly and repeatedly documented: `net-mesh` → `net`, `net-mesh-sdk` → `net_sdk`, `net-mesh-cli` → `net-mesh`, and `net-deck` → `net-deck`.
- The documented one-node `NetNode` acceptance check executed end to end and shut down cleanly through its context manager.
- The raw `net.Net` README example executed: `events_ingested==2`, `events_dropped==0`, and polling returned zero events. The public docs explicitly explain that the default no-op adapter counts and discards, so this was not classified as false success.
- The concurrency workaround proved that two released `MeshNode` instances can handshake, start, and shut down locally once the blocking accept is scheduled concurrently.
- Released CLI wheel behavior was otherwise strong in sampled paths: `net-mesh version` returned structured JSON with matching CLI/SDK 0.34.0; nested help was discoverable; isolated `identity generate --output json` exited `0` and created an identity. The generated secret file was not read or included in this report.
- Candidate CLI packaging worked when the documented source-build prerequisite was supplied: with Rust 1.97.1 and external `CARGO_TARGET_DIR`, it built/installed `net-mesh-cli==0.35.0` and reported matching CLI/SDK versions. The 555 s source-build duration is recorded but not itself classified as a defect because the README warns that source installs require Rust and released wheels avoid compilation.
- Candidate Deck packaging also built successfully with the same external-target discipline. Its argv behavior remains PY-03.
- Public Python docs explicitly call out several otherwise dangerous semantics: memory transport is a no-op for delivery, acceptance is not completion, `MeshNode` requires explicit shutdown, the PSK representation is hex, tool serving and announcement are separate, and some tool/nRPC operations require the native handle.

## Reproduction asset index

All assets are outside the repository under:

`C:/Users/chief/AppData/Local/Temp/net-python-usability-300deeee`

Key directories:

- `candidate_sdk/` — PY-01 minimal script, resolver transcript, exit/elapsed record
- `released_sdk/` — released install, one-node/raw examples, failing sequential handshake, passing concurrent workaround, stdout/stderr records
- `cli/` — released CLI install/help/version/identity journey
- `candidate_cli/` — 0.35.0 source-build and version journey, with external Cargo target
- `deck/` — released Deck install and four argv probes
- `candidate_deck/` — 0.35.0 source-build and failing help probe, with external Cargo target

The reproductions use no customer data or credentials. A disposable identity was generated only to exercise the CLI path; its secret was never read into the report.

## Final repository-state note

The checkout was clean at scan start and remained untouched during package builds. Near finalization, two unrelated untracked files appeared concurrently:

- `docs/internal/misc/SDK_USABILITY_GO_2026_08_09.md`
- `docs/internal/misc/SDK_USABILITY_TYPESCRIPT_2026_08_09.md`

They were not created, opened, edited, staged, or deleted by this scan. Verification below is intentionally limited to this Python report, as requested.
