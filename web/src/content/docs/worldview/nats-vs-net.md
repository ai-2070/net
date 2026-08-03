---
title: Using NATS with Net
description: "NATS can provide publish-subscribe and request-reply messaging behind a capability published through Net."
---

# Using NATS with Net

NATS is a publish-subscribe and request-reply messaging system built around subjects. It can sit beneath Net inside a provider or deployment, just as Redis can provide state and vLLM can provide inference.

The two systems occupy different positions in an application:

```text
Applications, agents, Hermes, OpenClaw
                    │
                    ▼
        Net capability/authority plane
                    │
                    ▼
     Provider implementations and adapters
                    │
   ┌────────┬───────┼────────┐
   ▼        ▼       ▼        ▼
 Zenoh    NATS     Redis     vLLM
  data  messaging  state   inference
```

A service can use NATS subjects to exchange messages within its own system, then publish selected operations through Net as capabilities. Net callers do not need the subject names or the NATS topology. They address the capability and the provider offering it.

## A concrete composition

Suppose several inference workers already accept requests on a NATS subject and send their results through request-reply.

A Net provider can:

1. publish an `embed` or `generate` capability;
2. receive an authorized invocation from another Net node;
3. translate the invocation into a NATS request on the internal subject;
4. return the reply through the Net invocation, stream, or artifact associated with the work.

NATS continues to carry the internal messages. Net connects the provider-held operation to callers across machines, runtimes, or authority boundaries.

## Where the boundary sits

NATS remains responsible for publish-subscribe and request-reply messaging over subjects.

Net remains responsible for making provider-held work available across the mesh: capability discovery, provider identity, visibility, invocation authority, selection, and the streams or artifacts attached to an invocation.

When they are used together, NATS carries messages within the provider's implementation and Net publishes the resulting capability to the wider logical machine.
