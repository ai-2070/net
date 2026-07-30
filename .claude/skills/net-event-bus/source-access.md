# Reading Net's real source

Every chapter in this skill cites source paths — `net/crates/net/sdk/src/mesh.rs`,
`x402/mod.rs`, `bindings/python/src/lib.rs:1402`. Those citations are the evidence
behind the claims, and they are only useful if you can open them.

**If you are working inside the Net repository**, they are repo-relative. Read and
grep them directly; skip the rest of this page.

**If you are not** — the normal case, because this skill's job is helping someone
build an *application* against Net — the source is not on your disk. Fetch it:

```bash
npx -y opensrc@latest path ai-2070/net
```

That prints an absolute path to a cached checkout (~46 MB). First call fetches;
every later call returns the path instantly. [`opensrc`](https://github.com/vercel-labs/opensrc)
is a small tool for exactly this: giving a coding agent a package's real source
instead of its type signatures.

```bash
SRC=$(npx -y opensrc@latest path ai-2070/net)

rg 'fn emit' "$SRC/net/crates/net/sdk/src"
sed -n '640,700p' "$SRC/net/crates/net/sdk/src/mesh.rs"
ls "$SRC/net/crates/net/include"
```

Without `opensrc`, a shallow clone is equivalent and needs no new tool:

```bash
git clone --depth 1 https://github.com/ai-2070/net /tmp/net
```

If the fetch fails — no network, a locked-down sandbox, a proxy — **say so and
work from this skill's text**. Do not guess a signature and present it as read
from source.

## One fetch covers all five bindings

`opensrc` reads the registry metadata and checks out the repository, rather than
unpacking the registry tarball. So these three commands return the *same*
directory:

```bash
opensrc path crates:net-mesh-sdk     # Rust
opensrc path @net-mesh/sdk           # Node / TS
opensrc path pypi:net-mesh-sdk       # Python
```

There is nothing per-language to fetch. Go (`go/`) and C
(`net/crates/net/include/`) are in the same tree, and neither is published to a
registry `opensrc` would query on its own.

This also means you get **real TypeScript source**. The npm package publishes only
its built `dist` directory — compiled JS plus `.d.ts` — so reading
`node_modules/@net-mesh/sdk` shows you build output. The checkout has
`net/crates/net/sdk-ts/src/`.

## Three things the cache will not have

1. **Anything newer than the last release.** The checkout is the *published* tag,
   not `master`. This skill's frontmatter carries the version it documents; when
   that is ahead of the published one, surfaces added in between are simply
   absent. Currently that includes the org-capability-auth module
   (`net/crates/net/sdk/src/org/`, `go/org.go`). Pin explicitly if you need an
   older one: `opensrc path crates:net-mesh-sdk@0.32.0`.

2. **`net/crates/net/bindings/node/index.d.ts`.** It is napi-generated and
   git-ignored, so no checkout has it — including this one. The equivalent, and
   the actual declaration site, is the `#[napi]` attributes in
   `net/crates/net/bindings/node/src/*.rs`. An installed
   `node_modules/@net-mesh/core/index.d.ts` also carries it, generated at
   publish time.

3. **Anything a build produces.** Rust target directories, the TypeScript bundle
   under `sdk-ts/dist`, the compiled `.node` and `.so` artifacts. If a citation
   looks like build output, it is — read the source it is generated from.

## Rooting a shorthand citation

Most citations are repo-rooted and need no work. The rest are relative to the
subsystem the chapter is about, which reads naturally in context and resolves to
nothing if you paste it into a shell. Prepend the matching root:

The `…` marks where the citation continues — prepend everything to its left.

| Root | Holds | A citation that needs it |
|---|---|---|
| *(none — repo root)* | the majority | `net/crates/net/sdk/src/mesh.rs` |
| `net/crates/net/…` | `sdk/`, `sdk-ts/`, `sdk-py/`, `bindings/`, `include/`, `tests/` | `bindings/python/src/lib.rs` |
| `net/crates/net/src/…` | core internals | `bus.rs`, `config.rs`, `adapter/noop.rs` |
| `net/crates/net/src/adapter/net/…` | mesh behaviour | `behavior/fold/island.rs`, `channel/config.rs`, `cortex/workflow/` |
| `net/crates/net/cli/src/…` | the `net-mesh` command tree | `commands/node.rs` |
| `net/crates/net/payments/src/…` | payments modules | `core/quote.rs`, `x402/mod.rs` |
| `net/crates/net/bindings/…` | FFI layers | `python/src/lib.rs`, `node/src/payment_provider.rs` |

Skill-internal references (`bindings/coverage.md`, `examples/hello.ts`) are
relative to this skill directory, not the repo.

**This table is enforced.** `.github/scripts/check-skill-source-paths.py` resolves
all 204 cited paths against exactly these roots in CI, so a citation that stops
resolving fails the build rather than silently rotting.

## Line anchors are a hint, not an address

Citations like `net/crates/net/src/config.rs:660-664` are the sharpest evidence in
this skill and the most fragile: any edit above line 660 moves them. CI checks
only that the file exists and the line is inside it — it cannot check that the
line still holds what the text says.

So **navigate by the symbol, confirm with the line number.** If they disagree,
the symbol is right and the anchor has drifted.

## What the source does and does not settle

Reach for it to resolve a *specific factual question*: an exact signature, a
field name, an enum's variants, whether a symbol exists in the binding the user
is actually writing.

Do not reach for it instead of this skill. The source will not tell you that
`.memory()` selects an adapter that discards events, that backpressure shows up
as silence rather than an error, or which of five bindings the person you are
helping is in. That is what `concepts.md` and `bindings/coverage.md` are for, and
a signature read correctly out of that context still produces wrong code.

Two specific traps:

- **Never generalize from one binding's source to another's.** The names differ
  on purpose. `bindings/coverage.md` is the record of what each binding actually
  exposes; a symbol's presence in `sdk/src/` says nothing about Go or C.
- **Internal crate source is not public API.** `net/crates/net/src/` is the
  implementation; `net/crates/net/sdk/` is what an application calls. Reading the
  former to explain behaviour is right; copying from it into user code is not.
