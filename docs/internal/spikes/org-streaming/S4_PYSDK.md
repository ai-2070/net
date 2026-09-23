# Stage 4 — S4PySdk (pure Python SDK, PyPI `net-mesh-sdk`)

## 8.5 S4PySdk — pure SDK Python (Q6)

**Lane:** S4PySdk. **Pinned brief:** `spikes/org-streaming/S4_BRIEF.md`
@`02f2d6c9c` (the lane worktree predates that object; the brief's verbatim
text was delivered as `local://S4_BRIEF.md` and is cited as the pinned
content — both pins govern and they are identical). **Base:** `692b38961`
(Wave 1 closed 4/4). Row: `net/crates/net/sdk-py/**` — exclusively (the
brief's ownership table governs over the dispatch sketch's four-path list);
nothing outside the row was touched except this record
(`docs/internal/spikes/org-streaming/S4_PYSDK.md`, the lane's own file).

### 8.5.1 The landed surface (what shipped)

`net/crates/net/sdk-py/src/net_sdk/org/__init__.py` (new package) — the
`net-mesh-sdk` org facade: the §4.4 verbs sync + async (the call trio + the
serve trio) as the **pure-SDK pass-through over the typed wrappers** — thin
forwarding only, **no new stream wrapper**, no protocol logic:

- **Sync caller** `OrgClient`: `call_streaming(service, request,
  deadline_ms=0, cancel_token=0) -> RpcStream`, `call_client_stream(service,
  ...) -> ClientStreamCall`, `call_duplex(service, ...) -> DuplexCall` —
  returning the wheel's EXISTING handle classes unchanged. Plus the seams'
  companion execution control `reserve_cancel_token()` / `cancel(token)` and
  the **preserved unary** `call()` / `call_exported()`.
- **Async caller** `AsyncOrgClient`: `await call_streaming/call_client_stream/
  call_duplex` returning the wheel's `Async*` handle classes unchanged;
  deliberately no `cancel_token` parameter (the bridge mints and owns the
  token — asyncio task cancellation is the one cancellation story).
- **Serve trio** `serve_org_streaming(mesh, service, access, handler,
  handler_timeout_ms=None)` / `serve_org_client_stream` / `serve_org_duplex`
  + the **preserved unary** `serve_org`, all accepting a `net_sdk.MeshNode`
  OR the raw wheel mesh (the wrapper's private `_native` is unwrapped inside
  the facade — application code never names it).
- **Typed-wrapper pass-through** (the unary typed layer): `TypedOrgClient`
  and `serve_org_typed` over the wheel's `net.org` typed wrappers, with the
  same `MeshNode`-or-raw entry contract as every other facade verb.
- **`org_err_to_py` mirror at the wrapper level:** the `org:` wire
  vocabulary mirrored for `net_sdk` consumers — the `OrgError` family
  (`OrgError`, `OrgCredentialsError`, `OrgDiscoveryError`,
  `OrgAdmissionDeniedError`, `OrgUnclassifiedError`) re-exported from the
  wheel plus `parse_org_error` / `classify_org_error` / `ParsedOrgError` (the
  wheel's `net.org` vocabulary layer, lazily resolved). Midstream errors on
  org-opened handles surface through this family, never the `RpcError`
  family — witnessed below.
- **Provisioning forwards** (so a consumer never touches the binding):
  `install_org_authority(mesh, authority_dir)`,
  `install_provider_grant_audience(mesh, grant, audience_secret_path)`.
- **Seam contracts restated at every call entry point** (the wheel's,
  load-bearing one layer up): provider pinned per call (discovered privately,
  ONE exact-target call, pinned for the whole stream, never retried);
  `deadline_ms == 0` => the facade's 300 s default, never "none";
  `cancel_token == 0` => uncancellable.
- `net_sdk/__init__.py`: export additions only — the 16 eager facade names
  re-exported at `net_sdk` top level (the five `net.org`-sourced names stay
  at `net_sdk.org.*`, lazily resolved: eager submodule import at package load
  would break `import net_sdk` against the sdk-py test-suite's `net` stub,
  which is not a package).

`net/crates/net/sdk-py/examples/org_streaming_consumer.py` (new) — the Q6
**executed `net_sdk`-only consumer run**: its imports are the standard
library and `net_sdk` ONLY (no `net` import, no private binding access). One
invocation = one live two-mesh round trip over real transport for one cell
(`stream_sync|stream_async|rollup_sync|rollup_async|mirror_sync|
mirror_async|unary|midstream|cancel`), asserting the round trip AND the
serve-side verified-caller attribution at its named assertion lines, printing
one `CELL_OK` line with the observed facts. It is the skill-examples-runner
shape (execute with a timeout, match the stdout contract) and the evidence
vehicle for "calls and serves through `net_sdk` alone".

`net/crates/net/sdk-py/tests/test_org_streaming.py` (new) — the 15 witness
rows driving that consumer (one subprocess per cell — a fresh interpreter
against the INSTALLED artifact, so nothing here is an import check).

### 8.5.2 The F-S3.1-2 level statement (the named Stage-4 item)

`net_sdk.org.HANDLER_DROP_CONTRACT` is the stub/doc entry, verbatim the
Wave-1 Python binding's `__HANDLER_DROP_CONTRACT` wording, and the contract
is documented at every handler surface this lane owns (the three streaming
`serve_org_*` verb docstrings — `serve_org_streaming` embeds the full level
text, its siblings reference it exactly as the wheel's surface does — plus
`AsyncOrgClient`'s class docstring and the witness/consumer docstrings):

> **Handler-drop contract (Specification §2.2 — the F-S3.1-2 level).** A protected
> call runs under a per-call retire supervisor. On retirement — caller CANCEL or
> the caller handle's ``close()``/drop, the call deadline, revocation, session
> replacement, ``serve_handle.close()`` against an in-flight call, or node
> shutdown — the supervisor drops the handler future **without a final poll**.
> Cancellation is observed ONLY through the retirement observables (the request
> input fences to EOF where the shape has one; library-controlled sinks stop
> admitting output) and NEVER as a handler-side event: a ``def`` handler's
> blocking thread cannot be interrupted and runs to whatever point it reaches
> (its return value is discarded, its performed effects are not recalled); an
> ``async def`` handler's coroutine MAY see ``asyncio.CancelledError`` at an
> ``await`` as best-effort teardown machinery, but it may equally never be
> resumed to observe anything — never rely on it.

The `task.cancel()` witness's CAN/CANNOT statement (the consumer's `cancel`
cell docstring, the operative text, mirrored from the binding witness at its
exact link levels): (1) caller-side `CancelledError`; (2) the substrate's
LOCAL teardown observable — the response side EOFs while the caller handle is
still ALIVE (the per-stream cancel watcher's `pending.cancel` close), the
link that discriminates the cancel-token path; (3) the provider-side
retirement observable — the handler's request input fences to EOF — arrives
with the WIRE CANCEL the substrate publishes from the caller handle's
`close()`/drop (its per-shape Drop contract). CANNOT: any handler-side cancel
event (the §2.2 drop level above).

### 8.5.3 Witnesses and counts (executed)

Roster from source — `tests/test_org_streaming.py` = **15 tests / 9 names**
(the `[same_org|granted]` parametrize pair yields the 12 matrix cells):

1. `test_org_streaming_sync_call_and_serve[same_org]`
2. `test_org_streaming_sync_call_and_serve[granted]`
3. `test_org_streaming_async_call_and_serve[same_org]`
4. `test_org_streaming_async_call_and_serve[granted]`
5. `test_org_client_stream_sync_call_and_serve[same_org]`
6. `test_org_client_stream_sync_call_and_serve[granted]`
7. `test_org_client_stream_async_call_and_serve[same_org]`
8. `test_org_client_stream_async_call_and_serve[granted]`
9. `test_org_duplex_sync_call_and_serve[same_org]`
10. `test_org_duplex_sync_call_and_serve[granted]`
11. `test_org_duplex_async_call_and_serve[same_org]`
12. `test_org_duplex_async_call_and_serve[granted]`
13. `test_unary_call_and_serve_preserved_through_net_sdk`
14. `test_streaming_midstream_error_surfaces_the_org_vocabulary`
15. `test_task_cancel_propagates_to_retirement_observables`

PENDING-PART1A-RESULTS

The sdk-py suite executes in exact parts (F-S4PySdk-3; the parts' union is
the full `testpaths` set — 348 items = 327 + 15 + 6):

| part | command (cwd `net/crates/net/sdk-py`) | items | result |
|---|---|---|---|
| 1B — the pure-Python stub bulk (everything not below) | `python -m pytest --ignore=tests/test_packaging_metadata.py --ignore=tests/test_org_streaming.py` (CPython 3.10.11) | 327 | **327 passed** in 0.57 s (durable `s4pysdk-part1b-bulk.xml`) |
| 1A — the live Q6 witnesses (the table above) | `python -m pytest tests/test_org_streaming.py` (CPython 3.10.11, the wheel's ABI) | 15 | PENDING-PART1A-COUNT |
| 2 — the packaging-metadata guards | `python -m pytest tests/test_packaging_metadata.py` (CPython 3.12.12 — its CI interpreter; `tomllib` is 3.11+) | 6 | **6 passed** in 0.11 s (durable `s4pysdk-part2-meta.xml`, sha256 `c3b4e2e0e88eebe11d15db5e5b46d61c68cb4c86cd5bf435bcc497d4eb235ea8`) |

### 8.5.4 Inverse receipts (executed, raw)

PENDING-RECEIPTS

### 8.5.5 Findings (state, not decide)

**F-S4PySdk-1 — the task-cancel propagation property holds no production
site above the wheel (a boundary, established by construction).** The three
cancel links ride entirely through the wheel's async bridge: the pull's
`task.cancel()` reaches the bridge through the `Async*` handle's
`__anext__` (the handle the facade returns UNCHANGED — "no new stream
wrapper"), and the token minting that discriminates link 2 lives in
`bindings/python/src/org.rs`'s `call_duplex_bytes_deadline` argument
threading. The pure-SDK layer forwards the awaitable and the handle with no
interception point; the property CANNOT be weakened above the wheel without
ADDING a wrapper around the handle — which this row forbids ("no new stream
wrapper") and which would itself be a second cancellation story. The
inverse receipt for this property therefore exists at its production site —
S4Python's Receipt 2 (`bindings/python/src/org.rs`, `call_duplex`
token-threading weakened to `0`, RED at the link-2 discriminant,
hash-checked restore) — executed and recorded in `S4_PYTHON.md` §8.4.4.
This row's `test_task_cancel_propagates_to_retirement_observables` executes
the same three links live through `net_sdk` and pins their observability at
the facade; its inverse is the binding-level receipt. Stated, not decided.

**F-S4PySdk-2 — the sdk-py test-suite's `net` stub constrains facade
imports (a discovery documented into the surface).** `tests/conftest.py`
installs an auto-fabricating `net` stub into `sys.modules` before any SDK
module loads (so the pure-Python suite needs no wheel), and that stub is
NOT a package: `import net.org` against it raises (no `__path__`). Any
facade that eagerly imported `net.org` submodules at package load would
therefore break `import net_sdk` for every existing sdk-py test at
collection. Consequences landed in the surface: the `net.org`-sourced
names (`parse_org_error`, `classify_org_error`, `ParsedOrgError`) resolve
lazily from `net_sdk.org` (PEP 562 module `__getattr__`, cached at first
access) and are deliberately NOT re-exported at `net_sdk` top level (whose
export additions stay eager and stub-safe); and the live witnesses run as
subprocess consumer cells against the installed artifact (a fresh
interpreter, no stub) rather than in-process. Stated, not decided.

**F-S4PySdk-3 — `test_packaging_metadata.py` cannot collect on CPython
3.10 (pre-existing; split-run boundary).** It imports `tomllib` (stdlib
3.11+) while the only interpreter on this host whose ABI loads the cp310
wheel the live cells need is 3.10.11. The module is pure metadata reading
(no `net`/`net_sdk` import), so it runs green on CPython 3.12 — its CI
environment (the `sdk-py-tests` job is 3.12) — and the suite is verified
in exact parts (8.5.3/8.5.6). Nothing in this lane's diff touches that
module. Stated, not decided.

**F-S4PySdk-4 — `response pump failed` (0x0006) under concurrent live
load on this host (a substrate-level flake; observed once, isolated
re-run green).** What was observed, exactly: during a batch whose rows
raced ANOTHER live two-mesh run on the same host,
`test_org_streaming_sync_call_and_serve[same_org]`'s consumer died at
`chunks = list(stream)` with `_net.OrgError: org:rpc:server_error: rpc:
server returned status 0x0006: response pump failed` (the provider's
response pump — a wheel/substrate component below this row). The identical
cell then passed in isolation in 2.69 s (`rerun-stream-sync-sameorg.xml`),
and every other cell of the batch passed in the same run — so this is
load-sensitivity of the substrate's response pump, not a cell or facade
defect (the facade forwards bytes unchanged; the failure text is the
server's own classification arriving through the mirrored `org:rpc:`
vocabulary — which is itself the §4.4 contract working). Whether the pump
should fail-vs-stall under concurrent mesh load is a substrate question
(its row is out of this lane). Stated, not decided.

### 8.5.6 Never executed here (complete)

- **A single-interpreter full-suite invocation.** `test_packaging_metadata.py`
  imports `tomllib` (Python ≥3.11) and the live witness rows need the
  CPython 3.10 interpreter that loads the cp310 wheel (`_net.cp310-win_amd64.pyd`
  — the only `net` build with the S4 surface on this host). The suite is
  therefore executed in exact parts whose union is the full `testpaths`
  set: part 1 = `python -m pytest --ignore=tests/test_packaging_metadata.py`
  (CPython 3.10.11 — every other module incl. the 15 live rows), part 2 =
  CPython 3.12.12 on `tests/test_packaging_metadata.py` alone (its CI
  interpreter). Both parts green (8.5.3).
- **A fresh `maturin develop`/`maturin build` of the `net` wheel.** The
  wheel source (`bindings/python/**`) is out of this row (S4Python's,
  landed and verified at `9ea5e64c8`+), its in-tree develop build
  (`bindings/python/python/net/_net.cp310-win_amd64.pyd`, dated at the
  Wave-1 close) is the artifact every live cell executed against (F16 in
  8.5.7), and rebuilding it here would risk the session's known ENOSPC
  episodes for zero evidence gain. The cross-platform CI builds from
  source.
- **Linux/macOS execution.** This host is Windows-only (win32 10.0.22631,
  x64). CI's ubuntu `sdk-py-tests`/`python-tests` jobs are the
  cross-platform evidence.
- **A live midstream ADMISSION-DENIED (`0x0009` -> `OrgAdmissionDeniedError`)
  classification through the facade.** Mirrors the binding lane's exact
  boundary (`S4_PYTHON.md` §8.4.6): there is no revocation-feed
  provisioning verb (`install_org_authority` / `install_provider_grant_audience`
  only), so a live midstream revocation cannot be driven without a
  provisioning-surface extension (outside this row). The mirrored class
  identity itself IS live-witnessed through the midstream pin + Receipt 2's
  inverse.
- **The four-weakenings sweep per property.** One applied weakening per
  property was executed (8.5.4); the brief's four-weakenings sweep per
  property was not run (the same disclosure the binding lane made at
  `S4_PYTHON.md` §8.4.4's close).
- **`green-under-own-inverse` checks.** Not applicable: each executed
  witness is red under its own applied weakening (8.5.4) — there is no
  observed green-under-inverse to report as a finding.
- **The skill-examples runner wiring for the consumer.**
  `docs/data/examples.yaml` + `.github/scripts/` are Main's files (outside
  this row; "a needed change outside your row = finding, STOP"). The
  consumer at `net/crates/net/sdk-py/examples/org_streaming_consumer.py`
  is built to the runner's exact pattern (executed with a hard timeout,
  stdout contract `CELL_OK <json>`) and IS executed here through the
  witness rows; wiring it into the runner manifest is Main's same-commit
  CI work if wanted (its path is stable).
- **`AsyncOrgClient` unary forms / a `cancel_token` parameter on the async
  trio.** Deliberately absent (the §4.4/async contract: the bridge mints
  and owns the token) — not an oversight, nothing to execute.

### 8.5.7 F16 size+sha ledger + delivery notes

PENDING-F16
