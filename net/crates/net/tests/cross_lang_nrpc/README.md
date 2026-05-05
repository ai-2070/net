# Cross-binding nRPC compatibility — Phase B7

The canonical wire-format contract every binding's nRPC implementation must satisfy. The shared fixture in [`golden_vectors.json`](./golden_vectors.json) is the single source of truth — every binding loads it and runs the same test matrix.

## The service

**Name:** `cross_lang_echo_sum`

**Purpose:** Two trivial behaviors — string echo + integer sum — chosen specifically because they exercise both string and numeric JSON encoding without requiring any external state. Any binding that implements this service correctly proves it can:

- Encode a typed request to JSON wire bytes.
- Decode an incoming request from JSON wire bytes.
- Dispatch through the runtime to a handler.
- Encode the response back to JSON wire bytes.
- Decode the response on the caller side.

**Request shape (JSON):**

```jsonc
{
  "text": "string to echo",
  "numbers": [1, 2, 3]    // array of int64-compatible values
}
```

**Response shape (JSON):**

```jsonc
{
  "echo": "string from text field",
  "sum": 6                 // int64 (sum of numbers)
}
```

**Behavior:**

- Echo `text` as-is — no normalization, no whitespace trimming.
- Sum `numbers` left-to-right; integer wraparound on overflow is permitted but not required (no test case crosses int64 limits).
- Empty `numbers` ⇒ `sum = 0`.
- Missing or wrong-type `text` / `numbers` ⇒ handler error with status `0x8000` (`NRPC_TYPED_BAD_REQUEST`, application-defined range `0x8000..=0xFFFF`); the caller observes this as `nrpc:server_error: status=0x8000 message=…`.

## Error model

Errors propagate through the wire as `RpcResponse { status, body }` where `status` is one of:

| Status hex | Constant                       | Trigger                                    |
| ---------- | ------------------------------ | ------------------------------------------ |
| `0x0000`   | `RpcStatus::Ok`                | Normal response.                           |
| `0x8000`   | `NRPC_TYPED_BAD_REQUEST`       | Handler couldn't decode request (this svc). |
| `0x8001`   | `NRPC_TYPED_HANDLER_ERROR`     | Handler ran but returned an error (unused). |

Each binding's user-facing error type carries the stable `nrpc:` prefix:

| Kind segment    | Source                                   |
| --------------- | ---------------------------------------- |
| `no_route`      | No session to target / capability gone   |
| `timeout`       | Deadline elapsed before reply            |
| `server_error`  | Handler returned a non-OK status         |
| `transport`     | Wire-level send/receive failure          |
| `codec_encode`  | Caller-side encode failure               |
| `codec_decode`  | Caller-side decode failure               |

## Test matrix

The fixture `golden_vectors.json` declares two arrays:

- `ok_cases` — request → expected response. Every binding asserts:
  1. Round-trip through its own runtime: encode request, dispatch through a handler that implements the spec above, decode response, compare to `expected_response_json`.
  2. The response decodes to a value semantically equal to `expected_response_json` (allowing for ordering of object keys; the JSON encoders may differ in canonicalization).

- `error_cases` — request → expected error. Every binding asserts:
  1. The call surfaces an error whose kind matches `expected_error_kind_prefix`.
  2. The status code (when applicable) matches `expected_status`.

## Implementation per binding

| Binding | Test file                                                  | Pattern                                                    |
| ------- | ---------------------------------------------------------- | ---------------------------------------------------------- |
| Rust    | `tests/integration_nrpc_cross_lang.rs`                     | Boots an in-process loopback handler against the spec.     |
| Node    | `bindings/node/test/cross_lang_compat.test.ts`             | Loads the fixture, runs against `TypedMeshRpc` stubs.      |
| Python  | `bindings/python/tests/test_cross_lang_compat.py`          | Loads the fixture, runs against `TypedMeshRpc` stubs.      |
| Go      | (downstream — reference consumer at `bindings/go/net/`)    | Same shape; downstream fixture-driven test once Go ships.  |

These are **wire-format compat tests**, not subprocess-based interop tests. Cargo can't easily orchestrate Node + Python subprocesses portably (see "future work" below); the fixture-driven approach catches the same bugs at lower cost.

The fixture and contract are versioned via the top-level `abi_version_expected` field, which mirrors `NET_RPC_ABI_VERSION` from `bindings/go/rpc-ffi/src/lib.rs`. Bumping the ABI version invalidates the fixture and forces every binding's compat test to update.

## Future work

True subprocess-based interop tests (Node caller → Rust server, Python caller → Rust server, Node ↔ Python, etc.) are out of scope for B7. The blockers:

- Cross-platform process orchestration from cargo is fragile (PATH discovery for `node`, `python`, `bun`).
- Both bindings' native modules must be built first (`napi build`, `maturin develop`) — a non-cargo dependency that breaks `cargo test` discoverability.
- Windows toolchain stability for the bindings is incomplete.

When those constraints relax, add a `tests/cross_lang_nrpc.rs` driver that gates on `CROSS_LANG_NRPC=1` + `NET_NODE_BUILT=1` / `NET_PYTHON_BUILT=1` and spawns binding-side caller scripts via `Command::new`.
