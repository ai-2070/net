---
title: Invoke
description: Invoke a selected provider or service, apply provider-side admission, and handle typed results and ambiguous execution.
capability: nRPC — typed request/response + streaming
boundary: /docs/sdk/c/headers-and-linking
boundaryLabel: C — nRPC in net_rpc.h
---

# Invoke a Capability

Discovery returns candidate providers visible to the caller. Invocation addresses
one provider or a service name, applies provider-side admission, and returns a
typed result or failure.

## Two ways to address a call

**By node id.** You pin a specific provider. Use this when the provider holds a
session or other state the caller already selected.

**By service or tool name.** The mesh selects from providers that advertise it.
This allows another eligible provider to be selected after the current provider
becomes unavailable.

Prefer name-addressed calls unless you have a reason to pin. The reason is usually
state — a provider holding a session you already started.

## The tool call and the RPC call are the same call

`call_tool` and its equivalents are sugar over nRPC. A tool call is a
name-addressed, JSON-coded nRPC request whose service name is the tool id. When you
want request/response without the tool abstraction — your own service name, your
own codec — use nRPC directly. Nothing is lost by dropping down and nothing extra
is gained by staying up.

Both paths carry deadlines. Both surface the same typed failures.

## Deadlines are a caller-side promise about waiting, not a cancel

A deadline bounds how long _you_ wait. When it elapses the caller does emit a
cancel for that call id, so a cooperating provider can drop the in-flight handler —
but "can drop" is not "did not run." When a deadline elapses you learn that no
answer arrived in time. You do not learn whether the work happened.

That distinction is the whole of **ambiguous execution**, and it is the reason a
retry is not free: retrying a call whose deadline elapsed may run the work a second
time. Make the operation idempotent, or carry an idempotency key, or accept the
duplicate. This is not a Net peculiarity; it is what a network deadline means
everywhere, stated here because the typed API makes it easy to forget.

## Provider admission happens at invocation

Seeing a capability does not grant the right to invoke it. Discovery may itself be
scoped, but provider admission is still evaluated for the call.

The provider enforces scope **at call time**, against the authenticated caller
origin. An owner-only capability refuses a caller outside its scope regardless of
who can see it in the fold. A grant that has expired or been revoked stops working
at the next call, not at the next announcement — there is no cached permission to
go stale.

What a caller gets back is a **typed denial**, not a silence and not a generic
error: a distinct failure that says the authority check refused, so your code can
tell "you may not" apart from "nobody answered." The four bindings name it
differently; [Errors](/docs/sdk/errors) has each one and demonstrates the refusal.

For wrapped MCP tools this is the owner-scope and consent model in
[Wrap an MCP Server](/docs/guides/wrap-mcp-server) and
[Expose Net as MCP](/docs/guides/expose-net-as-mcp). Deadlines, cancellation and
streaming in depth are in [Typed RPC with nRPC](/docs/guides/nrpc).
