# Documentation — SDK Spine (Phase 3 sub-plan of `DOCS_STRATEGY_PLAN.md`)

**Status:** IN PROGRESS. Phase 3a — the **Rust reference spine** —
(`web/src/content/docs/sdk/rust/`) is the deliverable of this commit. Phase 3b
(TypeScript / Python / Go / C) is pending review of the Rust spine.

**Goal:** One conceptual spine, five language bindings. Every SDK page teaches the
same agentic step in the same order, so a reader who learns the concept in one
language can map it to any other. Not five unrelated manuals.

---

## The 7-page skeleton (every language)

| Page | Teaches | Verified Rust primitive |
|---|---|---|
| `quickstart` | install + a node + a first runnable loop | `Net::builder().memory().build()`, `emit`, `subscribe_typed`, `shutdown` |
| `announce` | publish a capability / tool | `#[tool]` macro + `Mesh::announce_capabilities`, or `CapabilitySet` |
| `discover` | find capabilities on the mesh | `Mesh::find_nodes(&CapabilityFilter)` (sync `Vec<u64>`), `list_tools` |
| `invoke` | call a capability, get a typed result | `call_tool`, `serve_rpc_typed` / `call_typed` |
| `watch` | consume the event stream | `subscribe_typed::<T>(SubscribeOpts::default())` + `.next().await` |
| `artifacts` | move a blob / dir over the mesh | `net_sdk::transport::fetch_blob` / `fetch_dir`, `net-mesh transfer` |
| `errors` | classify + recover | `SdkError`, `RpcError`, retry/hedge/breaker |

Each page: purpose → verified code → cross-links → a "same spine across bindings"
line. Grounded against the SDK examples (`sdk/examples/{hello,channels,tool_calling}.rs`)
and the resilience/transport source — not invented.

## Binding asymmetry (stated, not faked)

The spine is one shape, but the bindings are not identical, and the pages say so:

- **Rust / TS / Python** — full surface: named channels / typed firehose, transport
  is a runtime choice, async streams.
- **Go / C** — poll-based, transport-coupled constructors, no named-channel/typed-
  firehose sugar. Their `discover`/`watch`/`invoke` pages show the poll idiom and
  say plainly where the ergonomic surface isn't there yet, rather than pretending
  parity.

## Language gating (the mechanism)

The docs system already supports per-language gating: `docs-language.ts` defines
`LANGUAGES = ["rust","ts","python","go","c"]`, `DEFAULT_LANGUAGE = "rust"`, and
`DocsOrderConfig.languages` keys a slug → the languages it's visible under. Phase
3a gates `sdk/rust` to `["rust"]` (visible by default, since Rust is default). Phase
3b adds `sdk/typescript` → `["ts"]`, etc., so the switcher shows exactly one
language's spine at a time.

## Phase 3b — fan-out (pending)

Generate `sdk/{typescript,python,go,c}/` against the Rust spine as the template,
adjusting for binding asymmetry and each SDK's real API (verify against
`sdk-ts/`, `sdk-py/`, `go/`, `include/net.h`). Best done as one language at a time
with a build check between each, or as a generator seeded by the Rust spine.

## Acceptance

- Phase 3a: `sdk/rust/` renders (gated to `rust`), `cd web && npm run build`
  passes, every code block is grounded in verified SDK source, nav wired in
  `docs.order.ts` (`sdk` section + `sdk/rust` folder + labels + `languages`).
- Phase 3b (each language): same skeleton, real API for that binding, asymmetry
  stated, build green.
