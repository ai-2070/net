# Testing — Patterns for Net Integrations

How to write tests that are fast, deterministic, and don't flake.

---

## Read this before choosing a transport for a test

**Memory transport does not deliver events.** It selects the Noop adapter, which
counts batches and discards them — `adapter/noop.rs` says "Just count, don't
store", and its `poll_shard` returns an empty result. Events flow producer →
ring buffer → drain worker → adapter, so with Noop there is nothing to read:
`subscribe()` never yields and `poll()` always returns zero.

A test that publishes on a memory node and waits for a subscriber **hangs**. It
does not fail with a useful message; it waits forever.

That rules memory transport out for delivery tests and rules it *in* for
everything up to the adapter boundary:

| Testing this | Transport |
|---|---|
| Construction, config validation, shutdown | memory |
| Ingestion accepted / refused, backpressure behaviour | memory |
| Serialization of your payload type | memory |
| **Delivery — a subscriber actually receives** | mesh (two nodes), Redis, or JetStream |
| Ordering, replay, retention | Redis, JetStream, or RedEX |

For delivery tests, bind two nodes to `127.0.0.1` on different UDP ports over
the mesh transport — see § Two-node mesh below. That is the cheapest transport
that actually round-trips in one process.

### Rules that still apply to every test

1. Run each node in the same process; there is no broker to start.
2. Subscribe **before** publishing — subscriptions are hot (§ The race trap).
3. Set `buffer_capacity` explicitly if you are exercising backpressure. It must
   be a **power of two and at least 1024**, enforced at construction and caught
   by no compiler. The default is 1,048,576 per shard, so a few thousand events
   will not fill it.
4. Always `shutdown` in a teardown hook, even on test failure.

## The race trap: subscribe before publish

The single most common test bug. Subscriptions are hot — the subscriber sees what arrives *after* it joined.

```typescript
// WRONG — race: publish may run before subscribe is ready
publisher.channel('topic').publish(event);
for await (const ev of subscriber.channel('topic').subscribe()) { ... }

// RIGHT — start the subscriber first, await its readiness, then publish
const stream = subscriber.channel('topic').subscribe();
await new Promise(r => setTimeout(r, 10));   // give the iterator a tick
publisher.channel('topic').publish(event);
```

The 10ms sleep is a smell but it's the simplest fix. For zero-flake tests, build a small helper that subscribes, awaits the first poll loop iteration, then signals "ready" via a Promise / Event / Channel — the publisher waits on that signal before emitting.

## Deterministic shutdown in tests

Tests that don't `shutdown` cleanly leak:
- File descriptors (per node)
- Tokio task handles (Rust)
- Timer handles (Node)
- Background threads (Python, Go)

Test runner appears to hang or report leaked handles. Use the per-language teardown:

```typescript
// vitest / jest
let node: NetNode;
beforeEach(async () => { node = await NetNode.create({ shards: 1 }); });
afterEach(async () => { await node.shutdown(); });
```

```python
# pytest
import pytest
from net_sdk import NetNode

@pytest.fixture
def node():
    n = NetNode(shards=1)
    yield n
    n.shutdown()
```

```rust
// rstest or manual
#[tokio::test]
async fn my_test() {
    let node = Net::builder().shards(1).memory().build().await.unwrap();
    // ... test body ...
    node.shutdown().await.unwrap();
}
```

```go
func TestMyThing(t *testing.T) {
    bus, _ := net.New(&net.Config{NumShards: 1})
    defer bus.Shutdown()
    // ... test body ...
}
```

## Assertion patterns

### "I emit X, the subscriber sees X"

**This needs a delivering transport.** On a memory node it hangs forever — the
Noop adapter stores nothing, so the subscriber never yields. Build `node` over
the mesh transport (§ In-process mesh), Redis, or JetStream.

```python
def test_roundtrip(node):   # node must NOT be a memory-transport node
    ch = node.channel('test', dict)
    received = []

    import threading, time
    def consume():
        for ev in ch.subscribe():
            received.append(ev)
            if len(received) == 3: return

    t = threading.Thread(target=consume); t.start()
    time.sleep(0.05)   # let consumer start
    for i in range(3):
        ch.publish({'i': i})
    t.join(timeout=2.0)
    assert len(received) == 3
```

### "I emit fast enough to trigger backpressure"

Two things make the obvious version of this test wrong.

**`buffer_capacity` must be a power of two and at least 1024.** A smaller value
is rejected at construction, not silently rounded.

**`drop_oldest` never increments `events_dropped`.** It evicts to make room, so
the producer always succeeds and the counter stays at zero however far past
capacity you push — and it is the default. Both counters sit at the *producer*
boundary: they record what the bus accepted or refused from you, not what
survived to an adapter. Assert against a mode that refuses the producer.

```python
node = NetNode(shards=1, buffer_capacity=1024, backpressure='fail_producer')
refused = 0
for i in range(20_000):
    try:
        node.emit({'i': i})
    except Exception:
        refused += 1

assert refused > 0
assert node.stats().events_dropped > 0
```

The exact counts vary run to run — the drain worker races the producer — so
assert on "> 0", never on a specific number. `examples/observe.*` prints all
three modes side by side if you want to see the difference.

### "Shutdown is clean and idempotent"

```rust
#[tokio::test]
async fn shutdown_is_idempotent() {
    let node = Net::builder().memory().build().await.unwrap();
    node.shutdown().await.unwrap();
    node.shutdown().await.unwrap();   // should not panic
}
```

## Multi-node integration tests (mesh transport)

When you need to actually exercise the mesh — encryption, NAT, rerouting — write integration tests, not unit tests. Two patterns:

### In-process mesh

Bind two nodes to `127.0.0.1` on different UDP ports, point one at the other as a peer. Slower than memory transport, faster than process boundaries.

### Multi-process mesh in CI

Spawn two processes via `std::process::Command` / `subprocess` / `child_process`. Each is a node. Coordinate via stdin/stdout for "ready" signals. Slow but real.

For both, **avoid hard-coded ports** — assign port 0 and read back the bound address, or use a port reservation helper. CI runners share ports across parallel jobs.

### Org-protected services

Don't hand-roll org fixtures. The SDK ships two generators behind the `fixtures` Cargo feature — deliberately off by default so they never compile into a release binding library:

```bash
# A full cross-org scenario: adopted authorities, credential bytes,
# 0600 audience-secret files, and a manifest.json a harness in ANY
# language loads. Builds no mesh.
cargo run -p net-mesh-sdk --features net,fixtures --example gen_org_scenario

# The canonical `org:` error-vocabulary fixture every binding parses.
cargo run -p net-mesh-sdk --features net,cortex,fixtures --example gen_org_error_fixtures
```

Test the **error domain**, not the message. `org:<domain>:<kind>[: <detail>]` is frozen and fixture-pinned; the detail is human-facing and will change. Asserting on it is the org equivalent of asserting on a timestamp.

Two setup mistakes produce a confusing "service not found" rather than an auth error, so check them first: a `Granted` provider that never called `install_provider_grant_audience` registers fine but stays encrypted and undiscoverable, and a mesh built without an explicit identity seed is refused at bind with `persistent_identity_required`. Full model: `org.md`.

## CI-specific gotchas

- **No NAT in CI.** Disable the `nat-traversal` feature in test builds — its probes add latency and may emit warnings.
- **UDP firewall.** Most CI runners allow loopback UDP, but sandboxed runners (some macOS CI) may not. Test the memory transport path; only run the mesh path on hosts you control.
- **Parallel test isolation.** Memory transport instances are isolated per-process; safe to run tests in parallel. Mesh transport instances on the same host need unique ports — serialize or use ephemeral ports.
- **Container networking.** If running tests in Docker, the mesh transport needs the right network mode (`host` or a shared user-defined network). Bridge mode + NAT is solvable but not for tests.

## What not to test

- **Don't test the SDK.** Net's own test suite (thousands of unit + integration tests in the Rust core, plus SDK smoke tests) covers the SDK. Test *your application's behavior* against the SDK, not the SDK itself.
- **Don't mock the bus.** Use the real SDK with memory transport. Mocking pub/sub leads to tests that pass but ship broken integrations — exactly the failure mode the SDK's "small enough to use everywhere" property is designed to prevent.
- **Don't assert on timestamps.** They're nanosecond-resolution and machine-dependent. Assert on ordering and content, not absolute time.
- **Don't assert on shard IDs.** Shard assignment is hash-based and may change between SDK versions.

## Further reading

- [Running in Production](https://ai2070.net/docs/guides/production-deployment)
