---
title: Guides
description: "Task-oriented guides: how to publish and consume, make a channel durable, run a daemon that survives node failure."
---
# Guides

The pages in this section are task-oriented. Each one answers a "how do I…" question — how do I publish and consume events, how do I make a channel durable, how do I run a daemon that survives a node failure — and gives you the code, the model behind it, and the gotchas to know about.

Guides assume you've read the [Concepts](/docs/concepts) section, or are willing to flip back when something doesn't make sense. They're meant to be read in any order; pick the one closest to what you're trying to do.

Everything here is grounded in real Net APIs. If you need the exhaustive signature of a method or the full grammar of a configuration object, the [Reference](/docs/reference) section is where to look.

## About the language switcher

Guides are **Rust-first**. The core crate is Rust, and the guides go deep enough
that showing every knob in four languages would cost more clarity than it buys.
Switching the sidebar language changes which **SDK** pages you see; it does not
translate the guides.

That's a real gap, not a preference — so where a guide covers something you'd
reach for from another binding, it carries a side-by-side section instead. [Using
the Event Bus](/docs/guides/event-bus#the-same-loop-four-ways) is the worked
example. For per-language idioms end to end, each SDK has its own spine:
[Rust](/docs/sdk/rust/quickstart) · [TypeScript](/docs/sdk/typescript/quickstart) ·
[Python](/docs/sdk/python/quickstart) · [Go](/docs/sdk/go/quickstart) ·
[C](/docs/sdk/c/quickstart).
