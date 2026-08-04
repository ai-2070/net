---
title: Quickstart
description: Build a node, put something on it, and prove the node accepted it — the smallest loop that tells you the SDK is wired up correctly.
capability: Event bus — ingest + poll
boundary: /docs/sdk/c/quickstart
boundaryLabel: C — Quickstart
---
# Quickstart

Build a node, put something on it, and prove the node accepted it. That is the
whole of this page. It is deliberately smaller than a first program usually is,
because the first thing worth knowing about Net is which of two node types you
are holding.

## Two node types, and picking the wrong one costs an afternoon

Every binding gives you both. They are not layers of each other and the names are
close enough to be worth stating plainly:

- **A bus node** is in-process. It ingests events into a local ring buffer, counts
  them, and hands them back when you poll or subscribe. No network, no peers, no
  keys. This is what you build to check your install and what most tests use.
- **A mesh node** speaks encrypted UDP to other nodes. It carries capabilities,
  tools and nRPC, and it is the node the rest of this spine
  ([announce](/docs/sdk/announce) onward) is about.

If you are here to move events inside one process, you want a bus node and you can
stop after the first snippet. If you are here to have two programs find and call
each other, you want a mesh node, and you need the next two sections.

## A mesh node needs a shared key and a handshake

Two facts that are easy to skip and expensive to skip:

**Every mesh node in a mesh uses the same pre-shared key.** It is 32 bytes. The
bindings disagree about whether you hand them raw bytes or a 64-character hex
string, and that disagreement is real rather than cosmetic — the fragment below
says which one yours takes.

**Peers do not find each other by magic.** A mesh node has to be connected to
another mesh node before anything folds between them: one side accepts, the other
connects, and both start. Until that happens, `announce` announces to nobody and
`discover` returns an empty list — which looks exactly like a bug in your
capability filter and is not one.

This is the single most common way a first mesh program fails: it is correct, and
it is talking to itself.

## Acceptance is not delivery

Whatever you put on a node, the call that accepts it is telling you one thing:
**the local node took it.** It is not telling you a subscriber ran, a peer
received it, or the work is done. That distinction has a page of its own —
[Submitted Is Not Completed](/docs/guides/submitted-is-not-completed) — and it is
the model, not an implementation detail to be optimized away later.

The first program therefore verifies a local counter rather than claiming a
round trip. Every fragment below ends by asserting the node's own ingested count,
because that is the strongest claim the smallest program can actually make.

## Why your first program appears to hang

**The default transport is memory, and memory discards events after counting
them.** A first program that publishes and then waits to read the same event back
will not error. It will sit there, waiting for something that is never coming.

Reading events back needs either an adapter that retains them (Redis, JetStream)
or the mesh transport between two nodes. That is a deliberate second decision, not
a default you were supposed to have found.

## Where this goes next

The seven pages of this spine are one journey, in this order: build a node →
[announce](/docs/sdk/announce) a capability → [discover](/docs/sdk/discover) it
from somewhere else → [invoke](/docs/sdk/invoke) it → [watch](/docs/sdk/watch)
what it emits → [move artifacts](/docs/sdk/artifacts) too large for the bus → and
handle [the ways it fails](/docs/sdk/errors).

Those links are language-neutral: each one states the objective and hands you the
four lenses. Once you are inside a language the *Next* control keeps you there —
it will not walk a Python reader into Go.
