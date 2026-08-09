## Install it — Go

```sh
go get github.com/ai-2070/net/go
```

One unified module, imported as package `net`. There is no separate core-and-SDK
split here: the Go module wraps the core crate's shared library directly.

### What is peculiar about Go here

**cgo, which means a C toolchain on every build machine** — gcc or clang on
Linux and macOS, MSVC on Windows. Not just yours: anywhere this builds, including
CI and any container that compiles rather than copies a binary. A pure-Go
cross-compile will not work.

Check it before anything else, because Go turns cgo *off* by default when it
cannot find a C compiler and says nothing:

```sh
go env CGO_ENABLED   # must print 1
```

If it prints `0`, install a compiler and set `CGO_ENABLED=1`. Building without
it fails inside the binding with a message naming this requirement — the
package deliberately refuses to compile rather than presenting a hollowed-out
API and blaming your call to `net.New`.

**The module needs the Rust shared libraries present to link.** `go vet`
type-checks without them, which is why it is the fastest way to catch a mistake
before dealing with linking; a `go build` needs the real `libnet`.

**Errors come back as values, and the constructor is `net.New`.** Naming is Go's,
not Rust's — `IngestRaw`, `Stats`, `Shutdown` — so do not carry method names over
from another binding's documentation.

**Some surfaces are not exposed in Go at all.** Agent-to-agent task handoff and
the consumer-side filter DSL are the notable gaps. The
[binding coverage matrix](/docs/reference/glossary) records which; a missing method
is sometimes a real absence rather than a different name.

### Verify it worked

```go
package main

import (
	"fmt"
	"log"

	"github.com/ai-2070/net/go"
)

func main() {
	bus, err := net.New(&net.Config{NumShards: 1})
	if err != nil {
		log.Fatal(err)
	}
	defer bus.Shutdown()

	if err := bus.IngestRaw(`{"msg":"hello, mesh"}`); err != nil {
		log.Fatal(err)
	}

	// Counts at the PRODUCER boundary: accepted, not received or stored.
	stats, err := bus.Stats()
	if err != nil {
		log.Fatal(err)
	}
	if stats.EventsIngested != 1 {
		log.Fatalf("the bus did not accept the event: ingested=%d", stats.EventsIngested)
	}
	fmt.Printf("accepted: ingested=%d\n", stats.EventsIngested)
}
```

Expect one line, `accepted: ingested=1`, and a clean exit. This is the example CI
builds and runs against the real shared libraries on every commit.

Next: [the Go SDK spine](/docs/sdk/go/quickstart).
