# The Agentic Mesh

AI agents need work done. The work lives somewhere else — in tools, APIs, other
agents, files, GPUs, browsers, enterprise systems, and humans. Those capabilities
are distributed across machines and organizations, they come and go, and no
single agent owns them. So an agent needs four things it mostly doesn't have
today: to **discover** capabilities live, **invoke** them safely, **observe** what
actually happened, and **recover** when work fails.

That is the gap Net fills.

> **Net is a discovery mesh for agentic capability.**

## Why request/response and hand-wired tools run out

The dominant way to give an agent a tool is to wire it in by hand: a fixed list
of endpoints, hard-coded in a config, each reached by an HTTP or MCP call. That
works right up until the moment the world stops being fixed:

- The tool you need is on a **different machine** — a colleague's workstation, a
  GPU box, a service in another org — and it wasn't in your config.
- The provider's **availability changes**: the GPU is busy, the model isn't
  loaded, the service moved.
- The work has **live state** — it streams, it fails partway, it needs a retry, it
  produces an artifact — and a single `200 OK` can't express any of that (see
  [Submitted Is Not Completed](/docs/guides/submitted-is-not-completed)).
- **Credentials** for the tool shouldn't leave the machine that owns them.

A hand-wired call surface has no answer to "what can I use *right now*, and did the
work actually happen?" It only knows the endpoints someone typed in, and it only
learns of failure if the callee bothers to report it.

## What a capability is

On Net, a **capability** is a typed, discoverable unit of work a node can do —
a tool, an API, a model, a GPU, an agent, a service. A node **announces** its
capabilities (name, input/output schema, policy, availability); every peer folds
that announcement into a local index; and any node can **query** the index by
what it needs rather than by who has it:

```
net-mesh cap query --tag gpu --tag vram:24     # who can do this, right now?
net-mesh cap nodes                             # everything the local index knows
```

Announcements propagate across the mesh, not just to direct neighbors — a node
several hops away learns the same capability fingerprint (bounded by a hop count,
so reach is finite and predictable, not a broadcast storm). Discovery is by
**capability**, and location is incidental: the answer might be a laptop, a rack
server, or a Jetson on a factory floor, and your code doesn't care which.

## Discover → invoke → observe → recover

The agent loop Net is built for:

1. **Discover** — ask the mesh who can do the work (`net-mesh cap query …`, or the SDK
   `find_nodes`). You get back nodes you can talk to directly.
2. **Describe** — read a capability's schema, risk, and provider before you commit
   to it. Display never implies permission to invoke.
3. **Invoke** — make a typed call and get a typed result. On Net this is nRPC
   (`call_typed` in the SDK); through an MCP host it's the `net_invoke_capability`
   meta-tool. Deadlines, retries, and cancellation are built in.
4. **Observe** — subscribe to the events the work emits; replay them later from a
   durable log if you need to.
5. **Recover** — when a call fails, retry, hedge to another provider, or trip a
   circuit breaker — because failure is a first-class, typed outcome, not silence.

Each step is a real primitive, not a diagram. The
[claims audit](/docs/worldview/right-and-wrong-use-cases) behind these pages maps
every one of them to shipped code.

## Credentials stay local; policy is local authority

Discovery is not authorization. A capability can be **visible** to the mesh while
being **invocable** only by its owner — the default for anything credentialed.
The node that holds a secret runs the work; the caller never sees the credential.
Widening who may invoke is an explicit, local decision, and consent for anything
sensitive is granted out-of-band by a human, not inferred by a model. This is what
makes it safe to expose a capability to agents you don't fully trust.

## The substrate underneath

The agentic mesh is the flagship use case, not the whole system. Underneath,
Net is a latency-first encrypted mesh — the same substrate that runs vehicular
sensor fusion, factory-floor robotics, and edge inference. Capability discovery,
typed RPC (nRPC), durable logs (RedEX), folded state (CortEX), and content-
addressed artifacts (Dataforts) are all layers on one encrypted, brokerless
transport. That's why the agentic story holds up: it's not a thin coordination
API over someone else's cloud — it's discovery, presence, policy, events, and
artifacts on infrastructure you own.
