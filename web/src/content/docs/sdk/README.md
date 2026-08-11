---
title: SDKs and C ABI
description: "Choose a language, build a capability provider and caller, then follow the shared announce, discover, invoke, watch, artifact, and error path."
---

# SDKs and C ABI

Net has ergonomic SDKs for Rust, TypeScript, Python, and Go, plus a C ABI for
embedding the runtime or building another language binding.

Choose the language used by your application:

| Surface    | Package or entry point                 | Runtime model                                 | Start here                                               |
| ---------- | -------------------------------------- | --------------------------------------------- | -------------------------------------------------------- |
| Rust       | `net-mesh-sdk` / `net_sdk`             | native async Rust                             | [Rust quickstart](/docs/sdk/rust/quickstart)             |
| TypeScript | `@net-mesh/sdk`                        | Node.js with explicit async shutdown          | [TypeScript quickstart](/docs/sdk/typescript/quickstart) |
| Python     | `net-mesh-sdk` / `net_sdk`             | Python async API over the native runtime      | [Python quickstart](/docs/sdk/python/quickstart)         |
| Go         | `github.com/ai-2070/net/go`            | cgo binding with polling for bus events       | [Go quickstart](/docs/sdk/go/quickstart)                 |
| C ABI      | generated headers over one `libnet`    | explicit ownership, polling, and return codes | [C quickstart](/docs/sdk/c/quickstart)                   |

The language selector keeps the task sequence in one language where that surface
exists:

```text
quickstart → announce → discover → invoke → watch → artifacts → errors
```

The concepts are shared, but call shapes and feature coverage are not identical.
Each language landing page states its runtime lifecycle and known gaps. The C ABI
has a separate four-page annex because memory ownership, linking, and error handling
need explicit treatment.

Most applications should begin with a language SDK. Use the lower-level event bus
when embedding the bus, writing an adapter, or controlling ingestion and
consumption directly.
