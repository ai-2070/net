# REST vs Net

REST and webhooks are not the enemy, and they're not the model. They're the
**dirty edge** — how Net meets systems that only speak HTTP.

The rule:

> **Use REST/webhooks when integrating legacy SaaS, browser-only apps, dashboards,
> or systems that only speak HTTP. Do not model Net internally as REST.**

## Where REST fits in the hierarchy

There is a deliberate ordering of integration surfaces, from first-class to edge:

1. **Native Net — SDK / daemon / nRPC.** First-class. Typed capabilities,
   discovery, events, streams, artifacts, recovery. This is the substrate.
2. **The MCP bridge.** The ecosystem wedge — the fastest way to bring existing
   tool supply onto the mesh ([MCP vs Net](/docs/worldview/mcp-vs-net)).
3. **REST / webhooks.** The edge. For legacy SaaS, browser-only apps, dashboards,
   and anything that will only ever speak HTTP.

REST belongs at the boundary, translating between an HTTP-only system and the
mesh. It does not belong in the middle: Net's internals are events, capabilities,
and causal state — not resources and verbs. Modeling the mesh *as* a REST API is
the mistake that makes people mistake Net for an API gateway. It isn't one.

## Why not model Net as REST

- **REST is request/response.** The mesh is evented — work has stages, streams,
  and failures that a resource-and-verb model flattens into a status code (see
  [Submitted Is Not Completed](/docs/guides/submitted-is-not-completed)).
- **REST assumes a fixed endpoint.** The mesh discovers capabilities by what they
  do, not by URL. There's no canonical host to point at.
- **REST terminates trust at every hop** (load balancers, proxies, gateways see
  plaintext). The mesh is encrypted end-to-end between the actual endpoints.

## Practical shape

When you must integrate an HTTP-only system, put a thin adapter at the edge that
speaks REST/webhooks on one side and announces a capability (or publishes/consumes
events) on the other. The HTTP system stays in its world; the mesh stays in
 its. The adapter is a translator, not the architecture.

> This page is a positioning note, not an integration guide — there is no
> first-class REST adapter shipped today (unlike the MCP, Redis, and JetStream
> adapters). If you're building an edge adapter, model it as a capability
> announcer, and keep the HTTP surface at the boundary.
