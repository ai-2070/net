# Filter DSL — Narrowing What a Subscriber Sees

Read this when the user wants a consumer to receive **only some** of a channel's events (`level == "error"`, `service == "api"` AND `region == "eu"`), asks "how do I filter on the bus," or is attaching a predicate to a subscription / `ConsumeRequest`. This is the **bus-side** filter — equality only, hot-path. For "route to the GPU node" / numeric / semver matching, that's the **capability predicate** surface in `capabilities.md`, not this.

The two are different languages with different jobs; the last section here says when to use which.

---

## What it is

A filter is a JSON predicate evaluated against each event payload **after retrieval from the adapter**, in memory, on the consumer side. It is the primary way you narrow a busy channel down to the events a given consumer cares about.

The grammar is deliberately tiny: **three boolean operators and one equality primitive, nothing else.** No range, no regex, no numeric comparison. That's what keeps it single-pass and microsecond-cheap on the hot path. If you need more vocabulary, it lives in the capability subsystem or in your own fold, not here.

## Grammar

A filter is one of five JSON shapes, composable recursively to any depth:

```json
// 1. Equality, shorthand
{ "path": "level", "value": "error" }

// 2. Equality, explicit
{ "$eq": { "path": "level", "value": "error" } }

// 3. AND — all children must match
{ "$and": [ <filter>, <filter>, ... ] }

// 4. OR — at least one child must match
{ "$or": [ <filter>, <filter>, ... ] }

// 5. NOT — the inner filter must not match
{ "$not": <filter> }
```

That is the whole language. `$and` inside `$or` inside `$not` nests freely.

## Paths

`path` is a dot-separated walk of object keys and array indices, left to right, over the event's JSON tree:

| Path | Selects |
|---|---|
| `"level"` | top-level field `level` |
| `"user.email"` | nested `email` inside `user` |
| `"items.0"` | first element of array `items` |
| `"errors.0.code"` | `code` of the first element of `errors` |
| `""` | the root value itself |

Numeric segments dereference arrays; non-numeric segments dereference objects. **A path that doesn't exist is not an error — the equality is just false** ("no match"). So a filter never throws at evaluation time for a missing field; it simply doesn't match.

## Values

The value side is any JSON value — string, number, boolean, null, object, array. Comparison is **structural deep-equality** (`serde_json::Value::eq`), so object key order doesn't matter (`{"a":1,"b":2}` matches `{"b":2,"a":1}`).

```rust
Filter::eq("status", json!("running"))          // string
Filter::eq("retry_count", json!(3))             // number (exact — no range)
Filter::eq("verified", json!(true))             // boolean
Filter::eq("deleted_at", json!(null))           // null
Filter::eq("config", json!({"mode":"fast"}))    // nested object
```

There is no fuzzy match, no range, no regex. If you need range/regex, fold over events at the application layer, or use a capability predicate at subscription time (`capabilities.md`).

## The Rust builder

In Rust, build filters with the `Filter` enum or the `FilterBuilder` fluent helper:

```rust
use net::{Filter, FilterBuilder};
use serde_json::json;

// Direct
let f = Filter::and(vec![
    Filter::eq("level", json!("error")),
    Filter::eq("service", json!("api")),
]);

// Builder (AND of equalities)
let f = FilterBuilder::new()
    .eq("level", json!("error"))
    .eq("service", json!("api"))
    .build_and();   // .build_or() for OR
```

`build_and()` / `build_or()` collapse the `Vec<Filter>` into the right shape, and **a single-element AND/OR is unwrapped** to the inner filter — no wasted wrapping.

In TS / Python / Go / C you hand the filter as JSON (the shapes above) on the subscribe / consume call; there's no per-language builder object — the JSON *is* the portable representation.

## Two evaluation edge cases

- **Empty `$and` matches nothing.** `{"$and": []}` is "no satisfiable form," **not** "matches everything." This is deliberate — an externally-supplied filter with an accidentally empty AND would otherwise silently pass every event through. To mean "match everything," **omit the filter entirely** from the request.
- **Empty `$or` matches nothing.** Same as `Iterator::any` on an empty iterator — no element to satisfy, so false.

If a user reports "my filter matches nothing," check for an empty `$and`/`$or` first; and remember a missing path is a silent non-match, so a typo'd `path` looks identical to "no events qualified."

## Serialization

Filters round-trip through JSON with no semantic drift — build in one language, serialize, reconstruct in another:

```rust
let json = filter.to_json()?;
// {"$and":[{"path":"level","value":"error"},{"path":"service","value":"api"}]}
let parsed = Filter::from_json(&json)?;
```

The serialized form is what travels when a subscriber attaches a filter to a subscription. A malformed filter surfaces as `ConsumerError::InvalidFilter` at consume time (message includes a parse position) — see `error-codes.md`.

## Performance

Evaluation is **single-pass** over the payload: the dot-path accessor traverses the JSON tree once, equality is a `Value::eq`, and boolean composition short-circuits on the first decisive child. A depth-3 path against a ~1 KB event runs in single-digit microseconds.

Filtering is **post-retrieval** — the adapter does *not* push predicates down. If you have adapter-side prefiltering available (Redis Streams `XREAD COUNT`, JetStream subject filtering), apply that at the adapter first, then use the bus filter for the in-memory pass. Don't expect the bus filter to reduce what the adapter reads off disk/network.

## Bus filter vs. capability predicate — which to use

| You're asking… | Surface | Where |
|---|---|---|
| "of the events on this channel, which payloads have `level == error`?" | **bus filter** (equality only) | this file |
| "which *nodes* have a GPU with ≥24 GB / model X loaded / semver ≥ 2.1?" | **capability predicate** (existence, numeric, semver, string match) | `capabilities.md` |

They **compose**: an nRPC call can carry a capability predicate (`net-where`) to pick *which receivers* answer, and a bus filter on the response stream to narrow *what comes back* (`nrpc.md`). Use the bus filter for content matching, the capability predicate for placement.

## Cross-references

- `capabilities.md` — the richer predicate language (numeric/semver/existence) for node selection.
- `error-codes.md` — `ConsumerError::InvalidFilter` and where it fires.
- `apis.md` — where the filter argument goes on each SDK's subscribe/consume call.
- `payloads.md` — shape your event so the fields you filter on are top-level and cheap to reach.
