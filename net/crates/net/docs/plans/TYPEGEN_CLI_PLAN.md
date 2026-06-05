# Typegen CLI plan — TypeScript and Python bindings from discovered tool descriptors

Branch: `typegen-cli`.
Predecessor work: `ToolDescriptor` (`net/crates/net/src/adapter/net/cortex/tool.rs:95`), `ToolCapability` with `input_schema` / `output_schema` JSON Schema fields (`net/crates/net/src/adapter/net/behavior/capability.rs:482-499`), `MeshNode::list_tools` aggregation across the capability fold, the metadata-key conventions for tool descriptions / streaming / tags (`description_metadata_key` / `streaming_metadata_key` / `tags_metadata_key` in `cortex/tool.rs`).
Scope: ship `net-mesh typegen` CLI commands that generate language-specific typed bindings (TypeScript `.d.ts` + runtime helpers, Python `.pyi` stubs + Pydantic models) from `ToolDescriptor`s retrieved through `list_tools` or read from a pinned snapshot file.

**Why this exists.** `ToolDescriptor::input_schema` / `output_schema` carry JSON Schema (draft 2020-12) as strings. Today every consumer that wants typed access has to write its own schema → language-type translation. Two well-supported language ecosystems (TS + Python) deserve first-class codegen so developers writing code against discovered tools get IDE support — autocomplete, type checking, hover docs — instead of dynamic `dict` / `object` access.

**What this is not.**
- Not a redesign of `ToolDescriptor` or the schema fields it carries.
- Not a new schema language. The substrate already commits to JSON Schema draft 2020-12 (`tool.rs:109`); typegen consumes that.
- Not a runtime invocation API. The CLI emits source files; calling the tools is still through the existing SDK methods (`call_service`, `call_service_streaming`, `TypedMeshRpc::call`).
- Not a Rust binding generator. Rust users already get types through `sdk-macros` at compile time; typegen targets the dynamic languages where there's no equivalent path.

Tagged `[A | B | C | D | E]`:

- A — Command structure + descriptor fetch + snapshot format
- B — JSON Schema → TypeScript codegen
- C — JSON Schema → Python codegen
- D — Snapshot / regenerate / diff workflows
- E — Tests + docs + tidy

---

## Status

| ID   | Pri | Area              | Title                                                                                |
|------|-----|-------------------|--------------------------------------------------------------------------------------|
| A-1  | H   | command skeleton  | `commands/typegen.rs` — `TypegenCommand` enum + clap subcommand wiring               |
| A-2  | H   | descriptor fetch  | `--query` flag + `MeshNode::list_tools` call + descriptor filtering                  |
| A-3  | H   | snapshot format   | `TypegenSnapshot` struct + JSON serialization + version field for forward-compat     |
| A-4  | M   | output            | per-language writer interface + `--out` directory layout convention                  |
| B-1  | H   | TS codegen        | JSON Schema → TS interface translation (objects, primitives, enums, unions, arrays)  |
| B-2  | H   | TS module shape   | `.d.ts` file per tool + index re-export + `tools.ts` runtime helpers                 |
| B-3  | M   | TS edge cases     | `additionalProperties`, `oneOf` / `anyOf` / `allOf`, `$ref`, nullable patterns       |
| B-4  | M   | TS tests          | snapshot tests against fixture descriptors with golden TS outputs                    |
| C-1  | H   | Python codegen    | JSON Schema → Pydantic v2 model translation                                          |
| C-2  | H   | Python stubs      | `.pyi` stub generation for non-Pydantic consumers                                    |
| C-3  | M   | Python module     | per-tool module + `__init__.py` re-export + typed call helpers                       |
| C-4  | M   | Python tests      | snapshot tests against fixture descriptors with golden Python outputs                |
| D-1  | M   | snapshot verb     | `net-mesh typegen snapshot --query ... --out tools.snapshot`                              |
| D-2  | M   | regenerate verb   | `net-mesh typegen --language ts --from-snapshot tools.snapshot --out ./generated/`        |
| D-3  | M   | diff              | `net-mesh typegen diff --from old.snapshot --to new.snapshot` — schema-evolution view     |
| E-1  | H   | integration tests | `tests/typegen_cli_ts.rs`, `tests/typegen_cli_python.rs` — end-to-end CLI runs       |
| E-2  | M   | downstream check  | TS test: generated `.d.ts` imported in a tsc-strict project, compiles               |
| E-3  | M   | downstream check  | Python test: generated Pydantic models pass mypy --strict                            |
| E-4  | L   | docs              | `docs/cli/TYPEGEN.md` operator/developer guide                                       |
| E-5  | L   | tidy              | clippy + rustfmt; CI gating on the downstream type-checks                            |

---

## Gap A — Command structure, descriptor fetch, snapshot

### A-1 — `commands/typegen.rs` skeleton

Follows the existing CLI convention (see `commands/aggregator.rs`, `commands/cap.rs`).

```rust
#[derive(Subcommand, Debug)]
pub enum TypegenCommand {
    /// Generate typed bindings for tools matching a query.
    Generate(GenerateArgs),
    /// Pin currently discoverable tools into a snapshot file
    /// for reproducible later regeneration.
    Snapshot(SnapshotArgs),
    /// Show schema-evolution diff between two snapshots.
    Diff(DiffArgs),
}

#[derive(Args, Debug)]
pub struct GenerateArgs {
    /// Language target. Required.
    #[arg(long, value_enum)]
    pub language: Language,

    /// Tag query. Mutually exclusive with `--from-snapshot` and
    /// `--tools`.
    #[arg(long = "tag", num_args = 1.., value_name = "TAG")]
    pub tags: Vec<String>,

    /// Explicit tool IDs to generate for. Mutually exclusive
    /// with `--query` and `--from-snapshot`.
    #[arg(long = "tool", num_args = 1..)]
    pub tools: Vec<String>,

    /// Read descriptors from a snapshot instead of live discovery.
    /// Mutually exclusive with `--tag` and `--tool`.
    #[arg(long)]
    pub from_snapshot: Option<PathBuf>,

    /// Output directory. Created if missing.
    #[arg(long, default_value = "./generated")]
    pub out: PathBuf,

    /// Regenerate into an existing output directory; produces a
    /// summary of added / removed / changed tools.
    #[arg(long)]
    pub update: bool,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

#[derive(Clone, Debug, ValueEnum)]
pub enum Language {
    Ts,
    Python,
}
```

**Files touched (A-1).**
- `cli/src/commands/typegen.rs` — new file, ~150 lines for enum + args + dispatch shell.
- `cli/src/commands/mod.rs` — register module + top-level `Typegen(TypegenCommand)` variant.
- `cli/src/main.rs` — dispatch arm.

### A-2 — Descriptor fetch via `list_tools`

The CLI either fetches live or reads a snapshot.

**Live fetch path.**
1. Resolve `CliContext::mesh_node()` (reuses the accessor established by the aggregator remote-attach work).
2. Call `MeshNode::list_tools(query)`. The query layer is the existing `CapabilityQuery` shape used by `cap query` (`commands/cap.rs:64-79`); typegen passes its `--tag` values through unchanged.
3. Filter by `--tool` if explicit IDs were provided.
4. Result: `Vec<ToolDescriptor>` ready for codegen.

**Snapshot path.**
1. Read snapshot file (JSON).
2. Deserialize into `TypegenSnapshot::descriptors: Vec<ToolDescriptor>`.
3. Filter as above if `--tool` overrides the snapshot contents.

**Descriptor sanity check.** A descriptor without `input_schema` cannot generate input types (`tool.rs:112` notes this happens when the schema exceeds the fold's per-entry budget — the descriptor's metadata says "fetch via the future `tool.metadata.fetch` RPC"). Until that fetch RPC ships, the CLI prints a warning + skips that tool with a clear message:

```
warning: tool `image_processing/v2` has no inline input schema (size > fold budget);
         binding skipped. Re-run after `tool.metadata.fetch` ships.
```

`None` `output_schema` is fine — many tools don't require strict output validation (`tool.rs:113-114`). The codegen emits an `unknown` (TS) or `Any` (Python) for the response type with a comment noting the descriptor didn't carry a schema.

### A-3 — Snapshot format

```rust
#[derive(Serialize, Deserialize)]
pub struct TypegenSnapshot {
    /// Snapshot format version. Bump when the shape changes
    /// non-additively. Today: 1.
    pub format_version: u32,
    /// Timestamp when the snapshot was taken (RFC 3339).
    pub captured_at: String,
    /// Query that produced this snapshot (tags + tool IDs, if
    /// any). For audit / re-execution; not used at regenerate.
    pub source_query: SnapshotQuery,
    /// The descriptors, in the order returned by `list_tools`.
    /// `node_count` is preserved so consumers can see the
    /// pre-snapshot population, but typegen itself ignores it.
    pub descriptors: Vec<ToolDescriptor>,
}
```

Snapshots are intended for source-control commit. They're deterministic given the same `list_tools` query against the same mesh state, and reproducible at regenerate time.

### A-4 — Output directory layout

Each language has its own conventional layout. The CLI doesn't try to unify these because TS and Python ecosystems have different expectations.

**TypeScript layout (`--out ./generated/`):**
```
generated/
  tools/
    <tool_id>.d.ts        # one file per tool: request/response interfaces
    <tool_id>.ts          # runtime helpers (currently: type-tagged call wrapper)
  index.ts                # re-exports every tool module
  meta.json               # generator metadata (format_version, source, …)
```

**Python layout (`--out ./generated/`):**
```
generated/
  __init__.py             # re-exports every tool's models + helpers
  <tool_id>/
    __init__.py
    models.py             # Pydantic models (request / response)
    models.pyi            # type stubs for non-Pydantic consumers
    call.py               # typed call helper
  _meta.json
```

Tool IDs typically contain slashes (`vendor/tool_name`); the writers map `/` → `_` in filenames and preserve the original ID in the module's exported metadata.

---

## Gap B — TypeScript codegen

### B-1 — JSON Schema → TypeScript translation

Targets JSON Schema draft 2020-12 (`tool.rs:109`).

**Approach.** Don't reinvent the schema translator. The ecosystem has two well-maintained options:

- `json-schema-to-typescript` (npm) — Node-based, mature, widely used. Downside: invoking it from a Rust CLI means shelling out to Node, which adds an external dependency operators have to install.
- `quicktype-core` (npm + Rust bindings via `quicktype-rust`, though that crate is less actively maintained) — also Node-based.
- Pure-Rust JSON Schema → TS: `typify` (cargo) supports JSON Schema → Rust types primarily, but the underlying schema parser can be reused; TS emission would need to be written.

**Recommendation.** Ship the first version with an embedded translator written in Rust over `schemars` / `serde_json` parsing, scoped to the JSON Schema constructs actually used by tool schemas in practice. The full draft 2020-12 spec is large but tool schemas tend to be a narrow subset (objects with primitive properties, occasional unions, occasional arrays). Cover the common subset, surface clear errors for unsupported constructs, expand coverage iteratively as real tools expose new patterns.

This avoids the Node dependency and keeps the CLI self-contained. The trade-off is more upfront translator code (~500–800 lines for the common subset) versus an external dependency. Worth it for distribution simplicity.

**Translation rules (common subset):**

| JSON Schema construct                 | TypeScript output                                |
|---------------------------------------|--------------------------------------------------|
| `{ "type": "string" }`                | `string`                                         |
| `{ "type": "integer" }`               | `number` (no separate int type in TS)            |
| `{ "type": "number" }`                | `number`                                         |
| `{ "type": "boolean" }`               | `boolean`                                        |
| `{ "type": "null" }`                  | `null`                                           |
| `{ "type": "array", "items": T }`     | `T[]`                                            |
| `{ "type": "object", "properties": …, "required": [...] }` | `interface { foo: T; bar?: U }` |
| `{ "enum": [...] }`                   | union of literals: `"a" \| "b" \| "c"`           |
| `{ "oneOf": [...] }`                  | union: `A \| B`                                  |
| `{ "anyOf": [...] }`                  | union: `A \| B` (semantically `oneOf`-ish in TS) |
| `{ "allOf": [...] }`                  | intersection: `A & B`                            |
| `{ "$ref": "#/$defs/Foo" }`           | named type ref; emit `$defs` as sibling interfaces |
| `nullable: true` (OpenAPI dialect)    | `T \| null`                                      |
| `additionalProperties: T`             | indexed signature `[key: string]: T`             |

**Unsupported constructs (initial release):**
- Recursive `$ref` cycles → error with the cycle path.
- `not`, `if/then/else`, `dependentSchemas`, `unevaluatedProperties` → error message naming the construct + the tool ID.
- String formats (`format: "date-time"` etc.) → fall back to `string`; emit a doc comment noting the format.

### B-2 — Module shape

For tool `acme/web_search` with input `{ "query": string, "max_results"?: integer }` and output `{ "results": array of { "url": string, "title": string } }`:

`generated/tools/acme_web_search.d.ts`:
```typescript
// Auto-generated. Do not edit by hand.
// Source: tool `acme/web_search` v1.2.0
// Generated from snapshot: tools.snapshot @ 2026-06-04T10:00:00Z

/** Request body for `acme/web_search`. */
export interface AcmeWebSearchRequest {
  query: string;
  max_results?: number;
}

/** A single search result. */
export interface AcmeWebSearchResult {
  url: string;
  title: string;
}

/** Response body for `acme/web_search`. */
export interface AcmeWebSearchResponse {
  results: AcmeWebSearchResult[];
}

/** Descriptor metadata captured at generation time. */
export const AcmeWebSearchMeta = {
  toolId: "acme/web_search",
  version: "1.2.0",
  description: "Search the web for query terms",
  streaming: false,
  stateless: true,
  estimatedTimeMs: 800,
  tags: ["search", "io"],
} as const;
```

`generated/tools/acme_web_search.ts`:
```typescript
import type {
  AcmeWebSearchRequest,
  AcmeWebSearchResponse,
} from "./acme_web_search";
import { AcmeWebSearchMeta } from "./acme_web_search";

// Runtime call helper. Imports the user's mesh client lazily so
// the codegen doesn't dictate which SDK entry point is used.
export async function callAcmeWebSearch(
  mesh: { call: (tool: string, input: unknown) => Promise<unknown> },
  input: AcmeWebSearchRequest,
): Promise<AcmeWebSearchResponse> {
  return (await mesh.call(AcmeWebSearchMeta.toolId, input)) as AcmeWebSearchResponse;
}
```

The mesh-client interface is kept structural rather than imported from `@net/sdk` so the generated code doesn't pin to a specific SDK version. Users wire their own SDK instance.

`generated/index.ts` re-exports everything:
```typescript
export * from "./tools/acme_web_search";
// … one per generated tool
```

### B-3 — TS edge cases

Worth being explicit because these are where codegen tends to subtly break:

- **`additionalProperties: false`** on an object → emit a TS interface and don't add an index signature. Validation is at runtime (the substrate's schema validator handles it); TS doesn't enforce additionalProperties at the type level natively.
- **`additionalProperties: true`** or absent → emit `[key: string]: unknown` index signature unless the object has no other properties (then just `Record<string, unknown>`).
- **`oneOf` vs `anyOf`** → TS unions don't distinguish; both emit as union, with a doc comment noting if the source was `oneOf`.
- **`allOf` with a base + extension** → emit as intersection type. Common pattern for OpenAPI-style schemas.
- **`$ref` to `#/$defs/...`** → emit each `$def` as a named interface, reference by name.
- **`$ref` to external URI** → error in initial release; warn and skip the tool. External refs need network fetch which complicates determinism.
- **Empty object `{}` schema** → `Record<string, unknown>` (matches the "anything goes" semantics).
- **String enums vs const** → `enum: ["a", "b"]` becomes a literal union; `const: "a"` becomes `"a"`.
- **Tuple arrays** (`prefixItems` in 2020-12) → emit as TS tuple type `[T, U, V]`.

### B-4 — TS tests

`crates/net/cli/tests/typegen_ts_snapshots.rs` — table-driven test taking fixture `ToolDescriptor`s through the translator and asserting the emitted `.d.ts` matches golden files in `tests/fixtures/typegen/ts/`. Fixtures cover each translation rule plus the edge cases above.

---

## Gap C — Python codegen

### C-1 — Pydantic v2 model translation

Targets Pydantic v2 because that's the current major version (v1 is in maintenance). For non-Pydantic consumers, `.pyi` stubs are emitted alongside (C-2).

**Approach.** Same trade-off as TS: existing tools like `datamodel-code-generator` produce excellent output but require Python in the toolchain. Initial release uses an embedded translator in Rust, scoped to the common subset, with the same iterative-coverage approach.

**Translation rules:**

| JSON Schema construct                 | Pydantic v2 output                                |
|---------------------------------------|---------------------------------------------------|
| `{ "type": "string" }`                | `str`                                             |
| `{ "type": "integer" }`               | `int`                                             |
| `{ "type": "number" }`                | `float`                                           |
| `{ "type": "boolean" }`               | `bool`                                            |
| `{ "type": "null" }`                  | `None`                                            |
| `{ "type": "array", "items": T }`     | `list[T]`                                         |
| `{ "type": "object", "properties": …, "required": [...] }` | `class Foo(BaseModel): foo: T; bar: U \| None = None` |
| `{ "enum": [...] }`                   | `Literal["a", "b", "c"]`                          |
| `{ "oneOf": [...] }`                  | `Union[A, B]` (also `A \| B` on Py 3.10+; emit the `Union[]` form for broader compat) |
| `{ "allOf": [...] }`                  | inheritance: `class C(A, B): ...` when the components are objects |
| `{ "$ref": "#/$defs/Foo" }`           | named class reference                             |
| `nullable: true`                      | `T \| None` (`Optional[T]`)                       |
| `additionalProperties: T`             | not directly expressible as a model field; emit as `model_config = ConfigDict(extra="allow")` + doc comment |

### C-2 — `.pyi` stubs

Pydantic models give runtime validation + typed access. Stubs (`.pyi`) give static-analysis-only types for consumers using `mypy` / `pyright` without taking on Pydantic as a runtime dependency.

Emit both. Pydantic in `models.py`, equivalent type definitions in `models.pyi`. Consumers pick which to import.

### C-3 — Module shape

For the same `acme/web_search` example:

`generated/acme_web_search/models.py`:
```python
"""
Auto-generated. Do not edit by hand.
Source: tool `acme/web_search` v1.2.0
Generated from snapshot: tools.snapshot @ 2026-06-04T10:00:00Z
"""

from __future__ import annotations
from pydantic import BaseModel


class AcmeWebSearchRequest(BaseModel):
    """Request body for `acme/web_search`."""
    query: str
    max_results: int | None = None


class AcmeWebSearchResult(BaseModel):
    """A single search result."""
    url: str
    title: str


class AcmeWebSearchResponse(BaseModel):
    """Response body for `acme/web_search`."""
    results: list[AcmeWebSearchResult]
```

`generated/acme_web_search/call.py`:
```python
from __future__ import annotations
from typing import Protocol
from .models import AcmeWebSearchRequest, AcmeWebSearchResponse


class _MeshLike(Protocol):
    async def call(self, tool_id: str, input: dict) -> dict: ...


TOOL_ID = "acme/web_search"
VERSION = "1.2.0"


async def call_acme_web_search(
    mesh: _MeshLike,
    input: AcmeWebSearchRequest,
) -> AcmeWebSearchResponse:
    raw = await mesh.call(TOOL_ID, input.model_dump(exclude_none=True))
    return AcmeWebSearchResponse.model_validate(raw)
```

`generated/__init__.py` re-exports the per-tool modules.

### C-4 — Python tests

Same pattern as B-4. `crates/net/cli/tests/typegen_python_snapshots.rs` runs fixture descriptors through the translator, asserts output equals golden files in `tests/fixtures/typegen/python/`.

---

## Gap D — Snapshot / regenerate / diff

### D-1 — `net-mesh typegen snapshot`

Captures the current `list_tools` result for later reproducible regeneration.

```
net-mesh typegen snapshot \
    --tag tool \
    --out ./tools.snapshot
```

**Behaviour.**
1. Run the descriptor fetch (A-2).
2. Wrap in `TypegenSnapshot` (A-3).
3. Write JSON to `--out`. Format with stable key ordering so snapshots diff cleanly in source control.
4. Print summary: tool count, total schema bytes, write path.

### D-2 — Regenerate from snapshot

The `generate --from-snapshot` path described in A-1. Useful for CI / reproducible builds where the substrate's live tool population shouldn't influence the build output.

### D-3 — `net-mesh typegen diff`

Compares two snapshots, surfaces schema-evolution information.

```
net-mesh typegen diff --from old.snapshot --to new.snapshot
```

**Output:**

```
Added tools (2):
  - vendor/new_tool v1.0.0
  - vendor/another_new v0.9.0

Removed tools (1):
  - vendor/deprecated v1.0.0

Schema changes (3):
  vendor/existing/v1.2.0 → v1.3.0
    - input.query: type unchanged (string)
    - input.max_results: optional → required          [BREAKING]
    - input.filter: added (optional)

  vendor/another/v2.0.0 → v2.1.0
    - output.score: number → integer                  [BREAKING]

  vendor/third/v1.5.0 → v1.5.0
    - description text changed
    - tags: ["a"] → ["a", "b"]

3 changes total, 2 marked BREAKING.
```

**Why it matters.** Developers regenerating types against an evolved substrate population want to see breaking changes before they propagate into their codebases. The diff verb is the surface that surfaces them.

**Heuristics for BREAKING.** Conservative — flag anything that could plausibly break callers:
- A required field is added.
- An optional field is made required.
- A field type changes (string → integer, number → integer, etc.).
- An enum value is removed.
- A `oneOf` / `anyOf` branch is removed.
- A nullable field becomes non-nullable.

Non-breaking changes (description text, tags, estimated_time_ms) get listed but unflagged.

---

## Gap E — Tests, downstream checks, docs

### E-1 — Integration tests

`tests/typegen_cli_ts.rs` and `tests/typegen_cli_python.rs`. Spawn a daemon subprocess, register a few test tools with known schemas via the existing test fixtures, run `net-mesh typegen generate` end-to-end, validate the output directory contents.

### E-2 — Downstream TS type-check

The strongest evidence the codegen works: take the generated `.d.ts` files and import them into a tsc-strict project, assert it compiles.

`tests/fixtures/typegen/ts_consumer/`:
- `tsconfig.json` with `strict: true`, `noImplicitAny: true`, `strictNullChecks: true`.
- `index.ts` that imports a few generated tools, constructs valid request shapes, asserts the response types compile.
- CI runs `npx tsc --noEmit` against this project after typegen runs.

If tsc fails, the codegen has a bug. The test surfaces it.

### E-3 — Downstream Python type-check

Same idea for Python. `tests/fixtures/typegen/python_consumer/`:
- `pyproject.toml` with mypy config: `strict = true`.
- `consumer.py` that imports generated Pydantic models, builds request instances, asserts response types.
- CI runs `mypy --strict consumer.py` against the directory after typegen runs.

### E-4 — Docs

`docs/cli/TYPEGEN.md`. Sections:

1. **Quick start** — generate types for one tool, import in TS / Python, call it.
2. **Generate verbs** — flags, query semantics, output layout per language.
3. **Snapshot workflow** — when to snapshot, how to commit snapshots, how regenerate works.
4. **Diff** — reading the output, what BREAKING means, suggested workflow when breaking changes appear.
5. **Schema coverage** — which JSON Schema constructs are supported, what happens to unsupported ones.
6. **Generated code conventions** — file layout, naming, the mesh-client interface used by the call helpers.
7. **Editing generated code** — don't. Regenerate instead. (Mention the header comment that marks files as generated.)

### E-5 — Tidy

- `clippy::pedantic` over the new code paths.
- rustfmt.
- CI job that runs the typegen integration tests + the downstream type-checks on every PR.

---

## Schema coverage — initial vs. iterative

The translator ships with coverage of the common JSON Schema subset. Constructs that don't translate cleanly get explicit error messages naming the construct + tool ID, so the operator knows exactly what's not supported.

**Common subset (initial release):**
- All primitive types + null
- Arrays with homogeneous items
- Objects with `properties` + `required` + `additionalProperties`
- `enum`, `const`
- `oneOf` / `anyOf` (as unions)
- `allOf` (as intersection / inheritance for object combinations)
- Local `$ref` to `$defs`
- Optional/required field handling via `required` array
- `nullable: true` (OpenAPI dialect, common in practice)
- Doc strings from `description` / `title`

**Out of initial scope (emit clear error):**
- External `$ref` URIs (need network fetch).
- Recursive `$ref` cycles.
- `not`, `if/then/else`, `dependentSchemas`, `unevaluatedProperties`, `dependentRequired`.
- Schema composition where the result isn't expressible in the target language's type system without runtime checks.
- Custom format keywords beyond doc comments.

Coverage expands iteratively as real tool schemas surface unsupported constructs. The plan ships with explicit "the operator will see this exact message" error text rather than silently skipping or producing broken output.

---

## Estimated effort

Rough breakdown assuming uninterrupted focused work:

- Gap A (skeleton + fetch + snapshot + output writer interface): 1.5 days.
- Gap B (TS codegen — translator + module shape + edge cases + tests): 2 days.
- Gap C (Python codegen — same shape as B + stubs): 2 days.
- Gap D (snapshot verb + regenerate + diff): 1 day. Snapshot and regenerate are mostly mechanical; diff has the most interesting logic (the BREAKING heuristics).
- Gap E (integration + downstream type-checks + docs + tidy): 1.5 days.

**Total: ~8 days of focused work** to ship the full surface.

Could compress to 5–6 days by deferring Gap D (snapshot/diff) to a follow-up PR. The minimum useful release is generate-from-live + TS + Python; snapshot/diff is quality-of-life that earns its existence quickly but isn't strictly required for the first version.

---

## Out of scope (explicitly)

- **Rust codegen.** `sdk-macros` already provides typed binding at compile time for Rust consumers.
- **C / C++ codegen.** Type-system limits make ergonomic generation harder; not pursued without specific need.
- **Go codegen.** Plausible future work; deferred from this plan because the Go SDK consumer pattern is less established than TS / Python.
- **OpenAPI / Swagger emission.** The substrate uses JSON Schema directly; there's no need for the OpenAPI envelope.
- **Schema validation at codegen time.** Already handled by the substrate's `validate_capabilities` (`behavior/schema.rs`). The CLI doesn't re-validate.
- **Custom template support.** Users who want different output shapes can post-process the generated files or contribute language-specific options upstream. Templating the generator surface complicates maintenance for unclear gain.
- **Per-language packaging** (npm publish, PyPI publish). The CLI emits source files; packaging is the user's responsibility.
- **Watch mode** that regenerates on substrate changes. Useful eventually; not required for initial release. Snapshot + manual regenerate covers reproducibility, which is the harder property.

---

## Open questions for implementation

These are unknowns that the plan flags but doesn't resolve. The implementer addresses them during the work.

1. **Translator dependency choice.** Embedded Rust translator (recommended above) versus shelling to Node tools. The recommendation favors embedding for distribution simplicity; revisit if the translator code grows beyond ~1000 LOC.
2. **Field naming.** Tool IDs and JSON Schema property names can contain characters that aren't valid TS / Python identifiers. Snake-case is the natural Python convention; camelCase is the natural TS convention. The codegen probably mirrors the source schema's casing rather than rewriting, with sanitization only for hard violations (leading digits, reserved words). Worth confirming with the first real tools that exercise edge cases.
3. **Multi-version handling.** When `list_tools` returns multiple versions of the same `tool_id`, the codegen needs a strategy: generate each version separately (verbose), generate only the latest (silent data loss), or generate with version suffixes (`AcmeWebSearchRequestV1_2` vs `V1_3`). Initial proposal: separate modules per `(tool_id, version)` with the version segment in the filename / module path. Confirm during implementation.
4. **Streaming tools.** `ToolDescriptor::streaming: bool` (`tool.rs:129`) marks server-streaming handlers. The call helper for streaming tools needs a different signature than the unary one — likely an async iterator (TS) / async generator (Python) over response items. Worth specifying the exact shape during B-2 / C-3 implementation rather than ahead of time.
5. **Output for tools without schemas.** Tools where `input_schema` is `None` get skipped with a warning. Tools where only `output_schema` is `None` could either skip output type generation or emit `unknown` / `Any`. Initial proposal: emit `unknown` / `Any` + doc comment so the tool is still callable, just without response typing. Confirm.

---

## Connection to existing CLI

`net-mesh typegen` slots into the existing `commands/` directory alongside `cap`, `aggregator`, `channel`, `subnet`, etc. The dispatch pattern, output formatting, and context construction reuse the established conventions. Nothing about typegen requires a new CLI subsystem — it's another verb family on the same surface.
