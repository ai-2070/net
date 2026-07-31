---
title: Net and NATS
description: "NATS organizes messaging around subjects. Net organizes distributed work around capabilities, providers, and authority."
---

# Net and NATS

NATS and Net both move work across distributed systems, but their root abstractions
are different.

> **NATS addresses subjects. Net addresses capabilities under authority.**

A NATS application publishes or requests on a known subject. Publishers and
subscribers meet through that name, with the server fabric handling routing,
accounts, and delivery semantics.

A Net application asks for a capability. It discovers providers that announce the
operation, evaluates which providers are visible and admissible, and invokes one
under the same identity model used for related streams, state, and artifacts.

|                   | NATS                                               | Net                                                            |
| ----------------- | -------------------------------------------------- | -------------------------------------------------------------- |
| Root abstraction  | subject                                            | capability offered by a provider                               |
| Normal deployment | server fabric, clusters, superclusters, leaf nodes | peer library with routed fallback                              |
| Selection         | subscriptions and queue groups                     | capability predicates, availability, locality, and authority   |
| Authority         | accounts, credentials, subject permissions         | provider identity; separate discovery and invocation authority |
| Durability        | JetStream                                          | RedEX and folded state                                         |
| Default delivery  | core at-most-once                                  | fire-and-forget                                                |

## Choose NATS for messaging

NATS is the simpler choice for conventional messaging and request/reply services
inside an operational domain. Its subject model is compact, JetStream provides a
mature durable messaging subsystem, and its ecosystem includes established clients,
tooling, managed offerings, and operational practice.

Leaf nodes also make NATS suitable for edge sites. They connect outward to a hub,
retain local operation during disconnection, and reconcile when the link returns.
NATS should not be reduced to a single central broker.

## Choose Net for provider-held capabilities

Net becomes relevant when the application needs to know more than which subject to
use:

- which provider currently offers the operation;
- who owns that provider;
- who may discover the capability;
- who may invoke it;
- whether selection depends on resources, locality, or organization;
- which streams, state, and artifacts belong to the work;
- how those relationships change when the provider or route changes.

This is additional machinery. It is useful only when the application needs those
answers.

## Authority and provider claims

Capability predicates match provider announcements. A provider that claims to
have a GPU is not automatically attested to have one. Treat resource properties as
selection input unless the deployment supplies external evidence.

Visibility and invocation are separate authority decisions. An organization-scoped
capability descriptor is encrypted for its audience, while the provider still
applies local admission policy when a caller invokes it.

## Deployment differences

NATS uses TCP and fits ordinary corporate networks naturally. Net's mesh transport
is UDP-only and requires the relevant inbound and outbound UDP path.

NATS deployments manage servers, accounts, NKeys or JWTs, credential files, and
subject permissions. Net links use Noise `NKpsk0` with a shared PSK and the peer's
static public key. That removes the server fabric but creates its own distribution
and rotation responsibilities.

Delivery also divides differently. JetStream can provide durable, ordered streams.
Net reliability provides gap-free eventual delivery but does not reorder packets;
consumers that require strict order use sequence numbers to reassemble them.

## Using both

The systems can occupy different scopes in one architecture. NATS may carry
internal service traffic while Net exposes selected capabilities across machines,
field devices, runtimes, or organizations.

Use NATS when subjects describe the system cleanly. Evaluate Net when the unit of
work must remain attached to provider identity, authority, availability, and
artifacts as it crosses boundaries.
