---
title: Organizations
description: Identity answers who is this entity. Permission tokens answer what is this entity allowed to do on a channel.
---
# Organizations

[Identity](/docs/concepts/identity) answers *who is this entity*. Permission tokens answer *what is this entity allowed to do on a channel*. Organizations answer a third question that neither one can: **which company is this caller acting for, and did my company authorize them?**

That distinction matters as soon as a mesh spans more than one commercial party. A partner's inference service, a customer's data plane, and your own internal tooling can all be peers on the same mesh, but "peer on the mesh" is not "may call my billing service." An organization is the unit of authority that closes that gap — and unlike every other authorization surface in Net, it governs *visibility* as well as access.

## An organization is a key, and belonging is a certificate

An organization is an ed25519 root keypair, kept offline. It never runs on a node and it never appears on the wire. Everything an organization asserts, it asserts by signing a small, structured, time-bound object with that key.

There are three such objects, and the most common mistake is to read any of them as permission to invoke something.

An **organization membership certificate** says "this entity belongs to this organization." It is issued by the org root to a specific node identity, carries a revocation generation, and defaults to roughly a year of validity with a hard two-year ceiling. It establishes belonging and nothing else.

A **dispatcher grant** says "this entity may act *for* this organization," over one named capability or over all of them. It is also issued by the org root, and it is what lets a caller speak in its organization's name. It is not permission to invoke anything either — it says *whose* authority the caller is drawing on, not that the authority extends anywhere in particular.

A **capability grant** says "this other organization holds these rights on this capability, over these providers." Crucially, it is issued by the **provider's** organization, not the caller's. If org B wants org A to be able to reach one of B's services, B signs a grant naming A as grantee, and the rights are explicit: `INVOKE` to call it, `DISCOVER` to find it, or both.

Even holding all three is not authority to invoke. Every protected call carries a freshly constructed proof, and the provider verifies that proof against its own installed authority on every single call. The grants are what make a valid proof *constructible*; the provider's verification is what makes it *accepted*. Nothing is cached into a session, and no credential is ever a bearer token that skips the check.

## Access implies visibility

The property that separates org auth from a conventional access-control layer is that a protected service is not merely refused to outsiders — it is **invisible** to them.

When a node serves a capability under organization protection, it picks one of two access modes, and each one implies its own form of encrypted announcement:

- **Same-org** access admits members of the serving node's own organization, and the capability is announced only inside that organization's encrypted audience.
- **Granted** access admits members of another organization holding a capability grant, and the capability is announced only inside the encrypted per-grant audiences.

Neither ever appears on the plaintext capability-announcement plane. A peer outside the audience cannot decrypt the announcement, so the service simply does not exist as far as its capability index is concerned. There is no separate visibility setting to configure, and therefore no way to get access and visibility out of sync — a mismatch that is a recurring source of leaks in systems that model the two independently.

This has a direct consequence for how you read a query result: an empty discovery result is the correct, expected outcome for an unauthorized caller, and it is deliberately indistinguishable from "nobody serves this."

## Denials are coarse, on purpose

When a provider's admission engine does refuse a call, the caller learns one of three things: `denied`, `not_supported`, or `unavailable`. That is the entire vocabulary, and it carries no detail.

This is not an oversight, and it will not be refined. A precise remote reason — "your dispatcher grant expired," "no grant covers this capability," "your membership was revoked at generation 4" — is a credential oracle. An attacker holding a partial or stale credential set could walk those responses to map exactly which piece to forge or replay. The detailed reason is recorded on the provider side for audit, where the party that already knows everything can see it, and never crosses the wire.

The caller-side errors are correspondingly organized around a more useful question than *why*: **did anything leave this process?** Credential and discovery failures are local — the call was never sent. Admission denials and transport errors are remote. Every language binding exposes that distinction directly rather than making you parse a message. The full vocabulary is in the [error-code reference](/docs/reference/error-codes).

## Secrets that never enter your process

A capability grant carrying `DISCOVER` rights mints an **audience secret**: the raw key that decrypts that grant's encrypted announcements. It is the one piece of org credential material that is not designed to transit.

The signed credentials — membership, dispatcher grant, capability grants — are public objects meant to be handed around, and they cross language boundaries as bytes. The audience secret does not. In every language binding, always, it is supplied as a **filesystem path** and never as a buffer, and there is deliberately no bytes-accepting constructor anywhere in the API.

The reason is specific rather than ceremonial. Handing a raw key to a garbage-collected runtime puts it in memory that is never zeroized, is freely copied by the collector, and shows up in a heap dump. Instead the file is opened by Rust through a checked loader — it must be a regular file, not a symlink, owner-only, and exactly the right size — read into scrub-on-drop storage, and never handed back to the caller. The key's entire lifetime stays on one side of the boundary.

## Revocation is a floor, not a list

Certificates carry a revocation *generation*. To revoke, the organization signs a floor bundle: "for this member, every certificate below generation N is now invalid." Nodes merge bundles monotonically — a lower floor never rolls back a higher one, and the state survives a restart.

The practical consequence is that v1 renewal is re-issue plus a raised floor, rather than extension in place. Grants are correspondingly short-lived: seven days by default, thirty at the absolute ceiling, rejected both at issue time and at every verifier. Long-lived org credentials are not a configuration you can opt into.

## What you actually do with this

Issuance is an offline ceremony against the org root key, run through [`net-mesh org`](/docs/reference/cli). Nothing in the SDK issues credentials, and the org key never needs to be on a node.

In application code you touch two verbs — bind a credential set to your mesh and call, or serve a capability under an access mode — and one startup step that installs what the ceremony produced. [Private capabilities](/docs/guides/private-capabilities) walks the whole path, in every supported language.
