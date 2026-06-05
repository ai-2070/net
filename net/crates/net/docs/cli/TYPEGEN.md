# `net-mesh typegen` — typed bindings from tool descriptors

`net-mesh typegen` generates language-specific typed bindings (TypeScript,
Python) from the `ToolDescriptor`s a mesh advertises — so code written
against discovered tools gets autocomplete, type-checking, and hover docs
instead of dynamic `dict` / `object` access. It consumes the JSON Schema
(draft 2020-12) the descriptors already carry; it does not invent a schema
language or a runtime API (you still call tools through the SDK).

> Binary name: the CLI ships as `net-mesh`.

---

## 1. Quick start

```sh
# 1. Pin the currently-discoverable tools into a snapshot (live, needs a
#    mesh target — same attach flags as `net aggregator` / `net transfer`).
$ net-mesh typegen snapshot \
    --node-addr <ip:port> --node-pubkey <hex> --node-id <N> --psk-hex <hex> \
    --out ./tools.snapshot

# 2. Generate bindings from the snapshot (offline, reproducible).
$ net-mesh typegen generate --language ts --from-snapshot ./tools.snapshot --out ./generated
$ net-mesh typegen generate --language python --from-snapshot ./tools.snapshot --out ./py_generated
```

```ts
// TypeScript: import the generated module and call the tool.
import { callAcmeWebSearch, AcmeWebSearchRequest } from "./generated";
const req: AcmeWebSearchRequest = { query: "net mesh", max_results: 5 };
const res = await callAcmeWebSearch(mesh, req); // res is typed
```

```python
# Python: build a validated request, call, get a validated response.
from py_generated.acme_web_search import AcmeWebSearchRequest, call_acme_web_search
res = await call_acme_web_search(mesh, AcmeWebSearchRequest(query="net mesh", max_results=5))
```

---

## 2. Verbs

| Verb | What it does |
|------|--------------|
| `generate --language <ts\|python>` | Emit bindings for matching tools into `--out`. |
| `snapshot` | Pin discoverable tools into a JSON snapshot for reproducible regeneration. |
| `diff --from <a> --to <b>` | Show the schema-evolution diff between two snapshots. |

### Sources

`generate` and `snapshot` get descriptors from one of two places:

- **Live discovery** — pass the remote-attach flags (`--node-addr`,
  `--node-pubkey`, `--node-id`, `--psk-hex`, each defaultable in the
  profile). The CLI joins the mesh, lets the capability fold populate, then
  reads `list_tools`. A short discovery poll (≤ 5 s) avoids racing an empty
  result.
- **Snapshot** — `generate --from-snapshot <file>` regenerates from a pinned
  capture. Offline and deterministic; this is the path CI and tests use.

### Filtering

`--tag <T>...` keeps a tool if ANY of its tags match; `--tool <ID>...` keeps
exact tool ids. Both apply to live and snapshot sources.

---

## 3. Output layout

**TypeScript (`--out ./generated/`):**
```
generated/
  tools/<tool_id>.ts   # request/response interfaces, $defs, a Meta const, a call helper
  index.ts             # re-exports every tool module
  meta.json            # generator metadata
```
> One `.ts` per tool (not a `.d.ts` + `.ts` split): a `foo.d.ts` is the
> ambient declaration file *for* `foo.ts` — the same module — so the runtime
> `Meta` const can't live in a `.d.ts`. A single `.ts` is correct and
> tsc-strict-clean.

**Python (`--out ./generated/`):**
```
generated/
  __init__.py          # re-exports each tool package
  <tool_id>/
    __init__.py
    models.py          # Pydantic v2 models (request / response / $defs)
    models.pyi         # type stubs
    call.py            # typed call helper
  _meta.json
```

Tool ids usually contain `/`; writers map every character outside
`[A-Za-z0-9_]` to `_` for file/module names and keep the original id in the
generated metadata + call helper.

The mesh-client interface used by the call helpers is **structural** (TS:
`{ call(tool, input): Promise<unknown> }`; Python: a `Protocol`), so the
generated code never pins to a specific SDK version — you wire your own
client.

---

## 4. Snapshot workflow

Snapshots are JSON, pretty-printed with stable key ordering so they diff
cleanly in source control. Commit them alongside the code that depends on
the generated types; regenerate from the committed snapshot in CI so the
build output doesn't drift with the live mesh population.

```sh
$ net-mesh typegen diff --from old.snapshot --to new.snapshot
Added tools (1):
  - vendor/new_tool v1.0.0

Schema changes (1):
  vendor/search v1.2.0 → v1.3.0
    - input.max_results: optional → required          [BREAKING]
    - input.filter: added (optional)

1 changed tool(s), 1 marked BREAKING.
```

`diff` emits the structured report under `--output json` / `yaml`.

**What `[BREAKING]` means** (conservative — anything that could plausibly
break a caller): a required field added, an optional field made required, a
field's type changed, an enum value removed, a nullable field made
non-nullable, or an output field removed. Enum *widening* and
required→optional are listed but not flagged.

---

## 5. Schema coverage

Supported (common subset): primitives + null, arrays, tuples
(`prefixItems`), objects (`properties` / `required` / `additionalProperties`),
`enum`, `const`, `oneOf` / `anyOf` (unions), `allOf` (TS intersection /
Python inheritance for object combinations), local `$ref` to `$defs`,
`nullable: true` (OpenAPI dialect), and doc strings from `description` /
`title`.

`additionalProperties` is treated strictly: **absent** means "no extra
properties in the generated type" (most tool schemas omit the keyword
without intending an open object); only an explicit `true` / typed schema
opens it (TS index signature, Python `ConfigDict(extra="allow")`), and
`false` closes it.

Out of initial scope — a tool with one of these is **skipped with a
warning**, not silently mis-generated: external `$ref` URIs, `not`,
`if`/`then`/`else`, `dependentSchemas` / `dependentRequired`,
`unevaluatedProperties`. A tool whose `input_schema` is `None` (schema
exceeded the fold's per-entry budget) is also skipped until the
`tool.metadata.fetch` RPC ships. A tool with no `output_schema` still
generates — its response type is `unknown` (TS) / `Any` (Python).

---

## 6. Editing generated code

Don't. Every file starts with an "Auto-generated … Do not edit by hand"
header. Regenerate instead; commit the snapshot so regeneration is
reproducible.

---

## 7. Not yet wired

- **Downstream type-check CI** (run `tsc --noEmit` / `mypy --strict` against
  the generated output) — needs Node / Python toolchains in CI; tracked as a
  follow-up.
- **Streaming-tool call helpers** — `ToolDescriptor::streaming` is captured
  in the metadata, but the helpers are currently the unary shape.
- **Go / other languages**, custom templates, and per-language packaging are
  out of scope (see `TYPEGEN_CLI_PLAN.md`).
