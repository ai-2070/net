# Source access — reading the real tree

This skill is a shadow copy of `@net-mesh/browser` and `net-mesh-leaf`. The
source is ground truth, and it is one command away.

## Fetch the tree

```bash
npx -y opensrc@latest path ai-2070/net
```

That prints the path of a checkout containing the whole repository: the Rust
core, the SDKs, the leaf, and the browser package. **Nothing here is
build-generated** — the package's compiled output and the leaf's wasm-bindgen
output are artifacts you produce (see `session.md` § Build it) and are not in the
tree.

## Roots for the citations this skill makes

| Citation | Root |
|---|---|
| `net/crates/net/browser-ts/src/` | the page package: the node surface, the leader session, streams, events, errors, the UDP probe |
| `net/crates/net/browser-ts/src/store/` | the networked store: definition, owner, replica, ledger, chunking and assembly |
| `net/crates/net/browser-ts/src/three/` | the scene-graph binding (`bindEntities`) |
| `net/crates/net/browser-ts/README.md` | the package's own reference — the measured tables and the reasoning behind every rule this skill states |
| `net/crates/net/leaf/src/` | the wasm half: the node, dispatch, sessions, streams, establishment, identity, storage |
| `net/crates/net/leaf/tests/` | the leaf's native and wasm witness suites |
| `net/crates/net/tests/rtc_browser/` | the Playwright matrix that drives the built package in a real browser |
| `net/crates/net/examples/browser-demo/` | the worked two-tab demo: the page, the anchor host, and its assertions |

## Navigate by the symbol, not the line

A line number in prose is a hint; the symbol is the address. `player` moved
because a field was inserted above it. If a citation looks wrong, search the
symbol (`hostStore`, `openSession`, `peerIdHex`, `StoreError`) — it will be where
the name is, not where a number said.

## The tests are the most direct statement of behaviour

- `net/crates/net/browser-ts/src/store/` — the protocol halves; the replica's
  state machine and the owner's dispatch have the sharpest comments in the
  package.
- `net/crates/net/browser-ts/src/node.ts` and
  `net/crates/net/browser-ts/src/leader/session.ts` — the two entry points, side
  by side, including which methods differ between them and why.
- `net/crates/net/leaf/src/leader_session.rs` — what a follower can and cannot
  do through the leader, in the Rust that answers it.
- `net/crates/net/leaf/src/error.rs` — the variant list every `.kind` in
  `errors.md` mirrors.
- `net/crates/net/leaf/src/wasm_witnesses.rs` and
  `net/crates/net/browser-ts/src/store/` — the witnesses behind the store's
  lifecycle claims, including the ones the package documents as *not* yet
  established.

## If a mechanism is not here

The leaf is a **deliberate subset** of the Rust core. If a mechanism has no
counterpart in `net/crates/net/leaf/src/` or the browser package, treat it as
absent rather than assuming the native spelling works in a page.
