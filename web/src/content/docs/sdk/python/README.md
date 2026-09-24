---
title: Python
description: "Use net-mesh-sdk from Python and understand where the ergonomic tool API differs from the lower-level capability handle."
---

# Python SDK

Install the Python SDK for capability discovery, tool calls, event streams, and
artifact transfer:

```bash
pip install net-mesh-sdk
```

The package installs as `net-mesh-sdk` and imports as `net_sdk`. The native
`net-mesh` binding is installed transitively.

## Choose the entry point

- **`NetNode`** is the event bus. Transport is selected in the constructor; memory
  is the default.
- **`MeshNode`** provides the ergonomic tool surface through `serve_tool`,
  `call_tool`, and `list_tools`.

Python exposes the raw capability announce/query surface through the node's native
handle rather than the same `MeshNode` methods used for tools. The pages below use
the ergonomic path and identify the places that require the native handle.

## Protected services

Both authority surfaces live on the native `net` package, with the organization
half also wrapped by `net_sdk.org` (`OrgClient` / `AsyncOrgClient`,
`serve_org`). Organization auth is `serve_org_typed` / `serve_org_streaming` /
`serve_org_client_stream` / `serve_org_duplex` on the provider, and `OrgClient`
with `call` / `call_streaming` / `call_client_stream` / `call_duplex` on the
caller — `AsyncOrgClient` carries the three streaming verbs, not the unary `call`
([concepts](/docs/concepts/organizations)). The subnet
authority plane ([concepts](/docs/concepts/subnets)) —
`mesh.serve_subnet_exported(service, export_name, handler)`
for a provider inside a protected subnet, `client.call_exported(service,
request)` for the caller, and `net.subnet.admin.*` for runtime gateway
administration. Named exports and trust anchors are constructor kwargs
(`subnet_exports=`, `subnet_authorities=`, …) validated before the node exists;
`subnet:<kind>` failures classify through `net.subnet.classify_subnet_error`
([reference](/docs/reference/error-codes)).

## Follow the capability path

1. [Quickstart](/docs/sdk/python/quickstart)
2. [Announce](/docs/sdk/python/announce)
3. [Discover](/docs/sdk/python/discover)
4. [Invoke](/docs/sdk/python/invoke)
5. [Watch](/docs/sdk/python/watch)
6. [Artifacts](/docs/sdk/python/artifacts)
7. [Errors](/docs/sdk/python/errors)
