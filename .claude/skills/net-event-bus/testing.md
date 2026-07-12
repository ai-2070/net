# Testing — Patterns for Net Integrations

How to write tests that are fast, deterministic, and don't flake.

---

## The default: memory transport, single process, two nodes

For 90% of tests, do this:

1. Use the **memory transport** (no constructor flags = memory by default).
2. Run two `NetNode` instances in the same process — one publisher, one subscriber.
3. Subscribe **before** publishing.
4. Use a small ring buffer (`shards: 1, buffer_capacity: 64`) so issues surface fast.
5. Always `shutdown` in a teardown hook, even on test failure.

Memory transport gives you the full SDK surface (channels, typed events, backpressure semantics) without UDP, NAT, or peer discovery — all the things that make tests flaky.

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

```python
def test_roundtrip(node):
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

Set ring buffer small, emit in a tight loop, assert `events_dropped > 0`:

```python
import time
node = NetNode(shards=1, buffer_capacity=8, backpressure='drop_oldest')
for i in range(1000):
    node.emit({'i': i})
time.sleep(0.1)
assert node.stats().events_dropped > 0
```

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
