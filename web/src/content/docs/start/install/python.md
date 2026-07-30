## Install it — Python

```sh
pip install net-mesh          # the native binding
pip install net-mesh-sdk      # the ergonomic SDK, if you want typed channels
```

Both publish at **0.33**. The distribution is named `net-mesh` but **the import
stays `net`** — symmetry with the Rust crate, and so existing code keeps working.
The SDK installs as `net-mesh-sdk` and imports as `net_sdk`. **Python 3.10 or
newer.**

Built with `maturin`, shipping prebuilt wheels for the same targets as the Node
binding, with a source build as fallback.

### What is peculiar about Python here

**Two names per layer, and they do not match.** `pip install net-mesh` →
`import net`; `pip install net-mesh-sdk` → `import net_sdk`. Four strings for two
packages is a lot to keep straight, and a `ModuleNotFoundError` here usually means
the right package is installed under the other name.

**The exported bus class is `Net`, not `EventBus`.** `EventBus` is the internal
Rust type the binding is built from, not a Python export.

**Several surfaces live only on `net`, never on `net_sdk`** — payments is the
clearest case. Check the lower layer before concluding a feature is absent.

**A stale binding fails as a missing attribute, not a missing module.** If `net`
imports but a symbol `net_sdk` needs is gone, you have two versions installed;
check both before reading anything else.

### Verify it worked

```python
from dataclasses import dataclass

from net_sdk import NetNode


@dataclass
class Hello:
    msg: str


def main() -> None:
    with NetNode(shards=1) as node:
        ch = node.channel("hello/world", Hello)
        ch.publish(Hello(msg="hello, mesh"))

        # Counts at the PRODUCER boundary: accepted, not received or stored.
        stats = node.stats()
        assert stats.events_ingested == 1, "the bus did not accept the event"
        print(f"accepted: ingested={stats.events_ingested}")


if __name__ == "__main__":
    main()
```

Expect one line, `accepted: ingested=1`, and a clean exit. The context manager
handles shutdown; leaving it out is how a Python program ends up with a drain
worker still holding events.

Next: [the Python SDK spine](/docs/sdk/python/quickstart).
