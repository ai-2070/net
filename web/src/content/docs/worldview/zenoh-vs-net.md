---
title: Using Zenoh with Net
description: "Zenoh can provide the distributed data plane behind a capability published through Net."
---

# Using Zenoh with Net

Zenoh can sit beneath Net in the same way that Redis or vLLM can. A provider may use Zenoh to distribute and query data across its deployment, then publish operations over that data as Net capabilities.

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

Zenoh gives a provider a distributed data space built around key expressions. The provider can publish, subscribe to, store, or query data through that space. Net makes the provider's operations available to the rest of the logical machine under a stable identity and explicit authority.

## A concrete composition

Consider a robotics deployment that uses Zenoh to distribute camera frames, telemetry, and local world-model updates.

A service in that deployment can:

1. read and query the required data through Zenoh;
2. implement an operation such as `inspect_region` or `get_local_model`;
3. publish that operation through Net as a capability;
4. return the result through the Net invocation, stream, or artifact associated with the work.

Callers address the capability and its provider. The provider remains free to use Zenoh internally, change its key layout, or move work between edge and cloud without exposing those details as the public invocation contract.

## Where the boundary sits

Zenoh remains responsible for the distributed data plane chosen by the provider: key expressions, publication, subscription, queries, and storage integration.

Net remains responsible for making provider-held work available across the mesh: capability discovery, provider identity, visibility, invocation authority, selection, and the streams or artifacts attached to an invocation.

When they are used together, Zenoh implements the provider's data plane and Net connects that provider to the wider logical machine.
