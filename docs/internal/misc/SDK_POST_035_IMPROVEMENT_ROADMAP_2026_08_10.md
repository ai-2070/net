# Post-0.35 SDK Improvement Roadmap

**Status:** Proposed follow-on work after the 0.35 usability round

**Date:** 2026-08-10

**Repository base inspected:** `b7139a6b591c72d050bc7719c7011f9c66912052` (`sdk-usability-2`)

**Related plan:** `docs/internal/misc/SDK_USABILITY_RESIDUALS_PLAN_2026_08_10.md`

## 1. Purpose

The 0.35 usability round moved Net’s SDKs from source-adjacent wrappers to credible public entry points. Installation, basic lifecycle, package pairing, declarations, CLI/Deck distribution, native-linking guidance, and first-use examples are materially better.

The next round should not be another undirected wrapper-polish exercise or an all-pairs API-parity campaign. The remaining leverage is in proving complete developer journeys, making configuration and errors semantically consistent, hardening language-runtime boundaries, and recording support honestly.

The governing principle is:

> Improve SDKs as complete, independently verifiable developer journeys—not as collections of similarly named methods.

## 2. Current state and exclusions

Several earlier audit findings are already repaired in the current source and must not be reopened without a fresh reproduction:

- Python’s ergonomic `MeshNode` exposes capability announcement and discovery methods.
- Go exposes channel token roots rather than allowing `RequireToken` to construct an unusable permanently closed channel.
- Invalid Go reliability strings are rejected rather than becoming fire-and-forget.
- Node, Python, and the C/Go FFI reject unknown capability modalities rather than mapping or dropping them.
- Node nRPC serving enters its captured Tokio runtime around synchronous registration.
- The TypeScript tool metadata-service lifetime repair is being implemented under the residual plan.

This roadmap also does not require identical API shapes. Rust, TypeScript, Python, Go, and C have different native runtime and ownership conventions. Semantic equivalence matters; cosmetic homogeneity does not.

### Explicit non-goals

- No universal generated API database.
- No requirement that every Rust symbol appear in every binding.
- No hidden downloads during package import, `go build`, or `go generate`.
- No network dependency in ordinary pull-request CI unless the job is explicitly a released-artifact conformance job.
- No historical release-note rewrite.
- No parity work without a complete provider/caller or producer/consumer journey.
- No governance ceremony, compatibility council, or broad ownership framework.

## 3. Priority summary

| Priority | Workstream | Why it matters | Exit condition |
|---|---|---|---|
| P0 | Released-artifact conformance | Source CI cannot prove public packages install and execute together. | Every supported binding completes the same external two-node journey from released artifacts. |
| P0 | Go callback containment | A Go panic escaping through cgo can abort the node. | Every exported callback contains panic, preserves diagnostics, and coordinates handle teardown. |
| P0 | Cross-SDK strictness contract | Silent fallback can widen authority or downgrade reliability. | Invalid/unknown configuration rejects consistently across bindings. |
| P1 | Lifecycle contract | Cleanup obligations remain type-specific and easy to miss. | Every resource-bearing public type has explicit, tested deterministic teardown. |
| P1 | Error semantics | Generic/string errors make retry and denial logic binding-specific. | Stable canonical categories map to native typed errors in every binding. |
| P1 | Security-sensitive journey parity | Provider verbs without compatible callers are not shipped features. | Each claimed protected feature has complete provisioning-through-teardown evidence. |
| P1 | Capability records | Symbol presence does not establish support maturity or semantics. | Each subsystem publishes a curated binding/status/evidence record. |
| P2 | Go native acquisition | cgo is legitimate, but current setup has avoidable operator tax. | One explicit checksum-verified operation installs artifacts and emits environment settings. |
| P2 | Static external-consumer gates | Internal builds miss released declarations, stubs, tags, and headers. | Python, Go, and C get native equivalents of the strict TS consumer gate. |
| P2 | Canonical runnable examples | Hand-copied snippets drift and contaminate bindings. | One executed end-to-end project per language is reused by docs and skills. |

---

## 4. Workstream A — Released-artifact conformance pack

### Goal

Prove what an external developer receives, not merely what the repository can build.

### Shape

Create one disposable project per public surface. Each project must use only published or release-attached artifacts—no workspace path dependency, local `file:` package, Cargo path dependency, Go `replace`, repository-relative header, or unreleased native binary.

Each binding executes the same conceptual journey:

```text
install exact matching release
→ create two nodes
→ connect
→ announce one capability
→ discover the provider
→ register one typed RPC/tool
→ invoke it
→ open and terminate one stream
→ exercise cancellation
→ close every child handle
→ shut both nodes down
```

### Required surfaces

- Rust from crates.io.
- TypeScript from npm SDK/core and the matching platform package.
- Python from PyPI wheels.
- Go from the tagged Go module and released native archive.
- C from released headers and library archive.
- CLI and Deck from npm/PyPI/binary release where relevant.

### Required inverses

Every language must cover the same failure classes where applicable:

- malformed or wrong-length PSK;
- unknown enum/configuration string;
- discovery with no matching provider;
- invalid or expired credential;
- invocation after serve-handle closure;
- cancellation before dispatch and during streaming;
- shutdown with an outstanding child reference;
- second close/shutdown call;
- native/library version mismatch.

### Evidence format

Each run records:

- package names and exact versions;
- downloaded artifact filenames and checksums;
- target OS/architecture/runtime versions;
- exact commands and exit codes;
- positive milestones reached;
- expected failure category for every inverse;
- process/task leak status after shutdown.

### Release integration

This should be a release or post-publication gate, not a dependency of every source PR. It may query public registries because validating publication is its purpose. A source-only PR job should run the same projects against locally packed artifacts.

### Acceptance

A release cannot be called SDK-complete until every claimed binding passes its external journey on the release’s supported platform matrix. Missing artifacts and unavailable platforms are explicit failures, not skipped success.

---

## 5. Workstream B — Cross-SDK configuration strictness

### Goal

Eliminate silent fallback, widening, and reliability downgrade at SDK boundaries.

### Canonical contract

| Input class | Required behavior |
|---|---|
| Unknown enum/string | Reject before runtime construction or state mutation. |
| Empty selector where broad matching is possible | Require an explicit `any`/unscoped form; never widen accidentally. |
| Missing security prerequisite | Reject configuration with the missing field named. |
| Unsupported feature | Return `unsupported`; never degrade to a weaker mode. |
| Unknown remote wire value | Fail closed without mutating authoritative state. |
| Case/whitespace variant | Reject unless normalization is explicitly part of the protocol. |
| Numeric overflow/truncation | Reject before crossing the ABI; never narrow silently. |

### Fixture families

Create canonical malformed-input vectors for:

- reliability;
- backpressure mode;
- capability modality;
- scoped selector kind;
- channel name and reserved tags;
- token roots and token generation;
- capability numeric fields;
- stream reliability/window values;
- organization/subnet access modes.

Bindings consume the same neutral vectors and assert language-native errors with equivalent canonical categories.

### Acceptance

No binding may turn an invalid value into a valid default, remove it from a filter, or reinterpret it as another modality. The inverse tests must prove no node, channel, service, announcement, or policy was installed after rejection.

---

## 6. Workstream C — Lifecycle as a public contract

### Goal

Make ownership and deterministic cleanup obvious and testable for every resource-bearing type.

### Lifecycle record

For each public type, record:

```text
type
→ native/runtime reference owned?
→ child handles issued?
→ explicit close/shutdown operation
→ sync or async
→ idempotent?
→ outstanding-child behavior
→ post-close behavior
→ drain/cancellation semantics
```

At minimum cover:

- bus nodes;
- mesh nodes;
- RPC handles;
- unary and streaming serve handles;
- streams and iterators;
- watchers/subscriptions;
- tool handles;
- daemon/MeshOS handles;
- storage/query iterators;
- migration/transfer handles.

### Native language forms

- Rust: `Drop` plus explicit async shutdown when draining matters.
- TypeScript: deterministic `close()`/`shutdown()`; consider `Symbol.asyncDispose` only where supported without compatibility distortion.
- Python: context manager and async-context-manager support where the underlying operation warrants it.
- Go: idempotent `Close`/`Shutdown` suitable for `defer`.
- C: explicit paired `*_free`/`*_shutdown`, with ownership transfer stated in headers.

### Required inverses

- double close;
- parent close before child;
- child close after parent refusal;
- operation after close;
- cancellation during close;
- partial-construction rollback;
- shutdown with outstanding iterator/watch/RPC/tool;
- no task, goroutine, callback, or native reference surviving successful shutdown.

### Acceptance

Ordinary successful cleanup cannot depend on V8 finalizers, Python finalization, Go finalizers, Rust process exit, forced sleeps, or eventual garbage collection.

---

## 7. Workstream D — Canonical error semantics

### Goal

Give every binding equivalent machine-actionable meaning without forcing identical classes or syntax.

### Canonical categories

- `invalid_argument`
- `unsupported`
- `unavailable` / `no_route`
- `unauthorized`
- `forbidden`
- `timeout`
- `cancelled`
- `backpressure`
- `closed`
- `version_mismatch` / `abi_mismatch`
- `callback_failed`
- `internal`
- `unknown(code)`

### Native mapping

- Rust enum variants with source chains.
- TypeScript subclasses with stable `code` and structured detail.
- Python exception classes/attributes.
- Go sentinels and typed errors supporting `errors.Is`/`errors.As`.
- C stable integer codes plus owned diagnostic text.

### Required properties

1. The same substrate failure maps to the same canonical category.
2. Binding-native context is retained rather than flattened into one string.
3. Future unknown codes preserve the numeric/string code as `unknown`, never masquerade as an existing category.
4. Retryability is explicit where it is actually part of the contract.
5. Security denial never becomes availability failure or generic internal error.
6. Errors never become default success values.

### Acceptance

Cross-language conformance vectors prove each category and its inverse. Documentation names canonical meaning first and binding-native expression second.

---

## 8. Workstream E — Security-sensitive provider/caller journey parity

### Goal

Expose or document only complete usable security journeys.

### Priority controls

- channel-prefix registration;
- subscriber-origin binding;
- queue-group admission policy;
- token-root installation;
- explicit issuer/signer generations;
- capability/scoped discovery with authority preservation;
- protected nRPC registration and invocation.

### Complete journey requirement

For every claimed protected feature:

```text
operator provisions authority
→ provider installs policy
→ provider registers service/channel
→ caller constructs proof
→ caller discovers coherent provider identity/authority
→ caller invokes or subscribes
→ provider denial reaches caller with the right category
→ policy replacement/revocation takes effect
→ both sides tear down cleanly
```

A provider registration method without a compatible caller operation is not a shipped high-level feature. A low-level method used only by tests does not establish an ergonomic SDK journey.

### Binding policy

Do not add methods merely to equalize counts. If a binding cannot safely complete the journey, mark the feature `core-only`, `partial`, or `not exposed` and route users to the supported alternative.

### Generation-aware token requirement

High-level SDKs should expose explicit generation-bearing issuance and delegation. Generation-zero conveniences may remain temporarily for compatibility, clearly marked as legacy. Legacy delegation must use the signer’s generation-zero identity rather than copying the parent token’s generation.

### Acceptance

Each binding claiming support proves both provider and caller directions against the canonical implementation, plus malformed proof, wrong identity, wrong scope, expiry, revocation, policy replacement, and teardown inverses.

---

## 9. Workstream F — Go callback containment and teardown safety

### Goal

Prevent a Go callback panic or handle race from terminating or corrupting the native node.

### Scope

Inventory every `//export` trampoline and callback registration, including:

- Compute `Process`;
- snapshot/restore;
- migration;
- MeshOS daemon callbacks;
- RPC handlers and observers;
- logging and control callbacks;
- callback-owned `cgo.Handle` teardown.

### Required implementation properties

Every callback boundary must:

1. establish `defer`/`recover` before invoking user code;
2. capture panic value and stack;
3. convert it to `callback_failed` with operation identity;
4. return across C/Rust without unwinding;
5. coordinate with shutdown so an in-flight callback cannot use a deleted `cgo.Handle`;
6. delete each handle exactly once after callbacks have drained or been fenced out.

### Witness strategy

Use subprocess tests. A child process deliberately panics while handling peer-controlled input. The parent asserts:

- child remains alive long enough to return a typed callback error, or exits normally after the expected test;
- no SIGABRT/access violation/heap corruption occurs;
- node shutdown completes;
- no callback executes after handle deletion.

Include races between callback entry, graceful shutdown, forced close, and registration replacement. Run under `go test -race` where meaningful, but do not treat the race detector as proof across the C/Rust boundary.

### Acceptance

Every exported callback is accounted for in an inventory and has positive, panic, teardown, and concurrent-close evidence. No uncontained callback remains merely because it is “unlikely” to panic.

---

## 10. Workstream G — Curated capability records

### Goal

Publish honest support status and evidence without pretending symbol presence proves semantic parity.

### Record shape

Maintain one small canonical record per domain:

```text
feature | binding | status | mode | evidence | reason | alternative
```

Statuses:

- `supported`
- `partial`
- `experimental`
- `not exposed`
- `n/a`

Modes where needed:

- `core-only`
- `poll`
- `verify-only`

### Evidence obligations

- Positive rows name a public symbol, source path, compile-checked example, or runtime conformance test.
- Negative rows include an editorial reason and supported alternative.
- Critical negative rows may carry a cheap enforceable absence check.
- Generated copies in agent skills are committed and equality-checked against the canonical record.

### Guardrails

- Do not infer maturity from generated declarations.
- Do not infer global absence from one package tree.
- Keep ergonomic wrapper, raw binding, and C ABI status distinct.
- Keep availability separate from access mode.

### Acceptance

Docs, skills, and release claims use the record, and every positive support claim resolves to current evidence.

---

## 11. Workstream H — Deterministic Go native acquisition

### Goal

Keep Go’s native/cgo boundary explicit while eliminating avoidable setup reconstruction.

### Proposed operator surface

Provide one explicit release-aware command, for example:

```text
net-mesh sdk env --language go --shell bash
net-mesh sdk env --language go --shell powershell
net-mesh sdk install-native --version 0.36.0
```

The final naming may differ; the contract matters.

### Required behavior

- select exact OS/architecture/toolchain artifact;
- distinguish MSVC and MinGW compatibility;
- download only on explicit invocation;
- verify checksum/signature/provenance;
- install headers, import/static library, and runtime library as one versioned unit;
- emit exact `CGO_CFLAGS`, `CGO_LDFLAGS`, and loader-path configuration;
- report the native ABI and package version;
- support offline use with a predownloaded archive;
- never mutate global environment silently.

### Acceptance

A clean external Go module can install the tagged module, run one explicit native-setup operation, build, execute the conformance journey, and remove the installed artifact without repository-relative paths.

---

## 12. Workstream I — Native external-consumer gates

### Goal

Give Python, Go, and C the same consumer-boundary protection that strict TypeScript checking now provides.

### Python gate

From a freshly built wheel in a clean environment:

- install only wheel artifacts;
- import every supported public module;
- run Pyright or mypy against realistic consumer code;
- compare stubs/type-visible members with the built extension where practical;
- execute construction, error, and lifecycle paths;
- verify distribution/import naming and extras.

### Go gate

From a clean module without `replace`:

- resolve the exact nested Go tag;
- compile against released headers/library;
- run one native lifecycle;
- verify `errors.Is`/`errors.As` behavior;
- prove Go module version, native version, and ABI agree.

### C gate

From the release archive only:

- compile C11 and C++ consumers;
- cover GCC/Clang, MSVC, and MinGW where supported;
- use warnings as errors;
- link and run dynamic builds, plus static builds if shipped;
- exercise version, ABI, allocation/free, and shutdown.

### Acceptance

The gates fail if they accidentally consume workspace source, repository-relative libraries, generated files absent from the package, or a different version than the artifact under test.

---

## 13. Workstream J — Canonical runnable language journeys

### Goal

Stop maintaining hand-copied “almost examples.”

### Proposed structure

```text
examples/sdk-spine/
  rust/
  typescript/
  python/
  go/
  c/
```

Each implementation follows the same conceptual milestones:

```text
connected
announced
discovered
invoked
streamed
cancelled
closed
```

### Publishing model

- CI executes the source-build version.
- Release conformance executes the same project against published artifacts.
- Documentation includes or links to the canonical files.
- Agent skills route to exactly one language project.
- Universal semantics remain in shared prose; runtime/import/error/lifecycle differences stay in the language project and binding companion.

### Acceptance

A competent developer or coding agent can start from the canonical example and produce a conventional customer-owned integration without repository-private knowledge, founder interpretation, or cross-language API guessing.

---

## 14. Recommended execution order

### Phase 0 — Finish current residual closure

Complete `SDK_USABILITY_RESIDUALS_PLAN_2026_08_10.md`, including exact-head CI. Do not mix this broader roadmap into the active residual repair chain.

### Phase 1 — Safety floor

1. Go callback containment and teardown inventory.
2. Cross-SDK strictness fixtures.
3. Canonical error categories for the failures touched by those repairs.

These prevent process aborts, authority widening, and reliability downgrade.

### Phase 2 — Release truth

4. Released-artifact conformance pack.
5. Python/Go/C external-consumer gates.
6. Version/ABI/artifact identity evidence.

These prove that what ships is usable independently of the source tree.

### Phase 3 — Complete protected journeys

7. Security-sensitive provider/caller parity.
8. Generation-aware token APIs.
9. Lifecycle records and remaining deterministic teardown gaps.

### Phase 4 — Self-serve adoption

10. Curated capability records.
11. Canonical runnable examples.
12. Deterministic Go native acquisition.

## 15. Release-blocking policy

The roadmap is not uniformly release-blocking.

### Block release

- Public artifact cannot install or load on a claimed platform.
- Invalid configuration silently widens scope or weakens reliability/security.
- Callback panic can abort the node.
- Claimed protected provider/caller journey is unusable or bypasses policy.
- Successful shutdown leaves native tasks/references that prevent process exit.
- Module/package/native ABI versions disagree.

### Block only the affected support claim

- Binding lacks an ergonomic method but a documented `core-only` route works.
- Feature is intentionally partial/experimental and the capability record says so.
- Language-specific convenience is absent without changing semantics.

### Do not block

- Cosmetic method-name differences.
- Historical release-note wording.
- Full all-pairs parity with no concrete user journey.
- Optional convenience helpers whose absence does not force private API access or unsafe behavior.

## 16. Completion claim

When the prioritized workstreams are complete, the bounded claim is:

> Net’s supported SDKs install from released artifacts, complete a shared two-node capability and invocation journey, reject invalid configuration without silent downgrade, expose deterministic language-native cleanup, preserve stable error meaning, and document security-sensitive support with executable evidence.

Do not expand this into identical feature coverage, proof of every subsystem through every binding, or universal production readiness for all deployment topologies.