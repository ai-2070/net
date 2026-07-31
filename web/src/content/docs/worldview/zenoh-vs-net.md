---
title: Net and Zenoh
description: "Zenoh gives distributed data a key space. Net gives provider-held work a capability and authority model."
---

# Net and Zenoh

Zenoh and Net both support distributed systems that span edge and cloud without
putting every interaction behind one application server. They differ in what the
network makes addressable.

> **Zenoh addresses distributed data through key expressions. Net addresses
> capabilities offered under identity and authority.**

A Zenoh application publishes, subscribes to, stores, or queries data through a
key space. A Net application discovers a provider that can perform an operation
and invokes it under explicit authority.

|                  | Zenoh                                                      | Net                                                                 |
| ---------------- | ---------------------------------------------------------- | ------------------------------------------------------------------- |
| Root abstraction | key expression                                             | capability offered by a provider                                    |
| Normal question  | where can I read, write, query, or subscribe to this data? | who can perform this work, and may this caller use them?            |
| Topology         | peer, client, and router modes                             | symmetric peers with routed fallback                                |
| Discovery        | multicast scouting, gossip, or configured endpoints        | signed capability announcements folded into a local index           |
| Authority        | deployment ACLs and TLS/mTLS configuration                 | provider identity plus separate visibility and invocation authority |
| Strong domains   | distributed data, edge/cloud, embedded and robotics        | provider-held work crossing runtime or organization boundaries      |

## Choose Zenoh for a distributed data space

Zenoh unifies data in motion, data at rest, and queryable computation through one
key space. Its transport breadth, embedded support, ROS 2 integration, and field
history make it a natural fit for telemetry, robotics, and edge-to-cloud data.

If the application is well described as data that lives at keys and can be
published, queried, or subscribed to, Zenoh already matches that model.

## Choose Net for work under authority

Net's capability model asks a different set of questions:

```text
Who offers this operation?
Who owns the provider?
Who may know the capability exists?
Who may invoke it?
Which provider is available and admissible for this request?
Which streams, state, and artifacts belong to the work?
What outcome did the provider report, and what evidence verifies success?
```

Those questions matter when work crosses contractor machines, customer hardware,
partner organizations, personal devices, or any environment where the network
operator is not automatically the owner of every capability.

## Discovery and authority

Both systems can discover beyond direct peers. Zenoh propagates routing and
liveliness information through scouting, gossip, and routers. Net propagates signed
capability announcements with bounded forwarding and folds them into a local
provider index.

A Net provider's resource properties remain assertions unless backed by an
attestation. Separately, organization-scoped capability descriptors are encrypted
for their audience, and provider-local policy controls invocation. Selection truth
and authority are different concerns.

## Deployment differences

Zenoh supports TCP, UDP, QUIC, TLS, serial, and shared memory. Net's mesh transport
is UDP-only. Networks that block UDP are therefore a direct deployment constraint
for Net.

Zenoh can use a CA, certificates, and ACL rules. Net uses Noise `NKpsk0` sessions
with a shared PSK and peer static key. Each model has operational costs: PKI needs
issuance and renewal; PSKs need secure distribution and rotation.

Zenoh reliable delivery over TCP or QUIC is ordered. Net reliability preserves
gap-free eventual delivery without imposing order; consumers use sequence numbers
when strict ordering is required.

## Using both

A system can use Zenoh for its distributed data plane and Net for selected
capabilities whose invocation needs provider identity, authority, task state, or
artifacts. The boundary is whether the application is addressing **data at a key**
or **work offered by a provider**.
