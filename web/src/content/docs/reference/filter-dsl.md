# Filter DSL

The filter DSL is the predicate language for event consumption. Filters are JSON predicates evaluated against event payloads after retrieval from the adapter, and they're the primary way you narrow down what a consumer sees on a busy channel.

The grammar is small. There are three boolean operators and one equality primitive — nothing else — and the simplicity is intentional. The bus's filter is for hot-path event matching where every microsecond counts; if you need richer predicates (numeric comparison, semver, capability matching), they live in the capability subsystem, not the bus.

## Grammar

A filter is one of five JSON shapes:

```json
// 1. Equality, shorthand form
{ "path": "level", "value": "error" }

// 2. Equality, explicit form
{ "$eq": { "path": "level", "value": "error" } }

// 3. Logical AND — all children must match
{ "$and": [ <filter>, <filter>, ... ] }

// 4. Logical OR — at least one child must match
{ "$or": [ <filter>, <filter>, ... ] }

// 5. Logical NOT — the inner filter must not match
{ "$not": <filter> }
```

That's the whole language. Filters compose recursively; you can nest `$and` inside `$or` inside `$not` to any depth.

## The Rust builder

In Rust, filters are constructed through the `Filter` enum or the `FilterBuilder` fluent helper:

```rust
use net::{Filter, FilterBuilder};
use serde_json::json;

// Direct construction
let f = Filter::and(vec![
    Filter::eq("level", json!("error")),
    Filter::eq("service", json!("api")),
]);

// Builder form (AND of equality conditions)
let f = FilterBuilder::new()
    .eq("level", json!("error"))
    .eq("service", json!("api"))
    .build_and();
```

The builder's `build_and()` and `build_or()` collapse the `Vec<Filter>` into the right boolean shape. A single-element AND or OR is unwrapped to the inner filter, so you don't pay for unnecessary wrapping.

## Paths

The path is a dot-separated sequence of object keys and array indices, evaluated left to right against the event's JSON tree:

| Path                    | Selects                                            |
|-------------------------|----------------------------------------------------|
| `"level"`               | Top-level field `level`                            |
| `"user.email"`          | Nested field `email` inside `user`                 |
| `"items.0"`             | First element of an array at `items`               |
| `"errors.0.code"`       | Field `code` of the first element of `errors`      |
| `""`                    | The root value itself                              |

Numeric segments dereference arrays; non-numeric segments dereference objects. A path that doesn't exist evaluates to "no match" — the filter is not erroneous, the equality is just false.

## Values

The value side of an equality is any JSON value: string, number, boolean, null, object, or array. Comparison is structural — `{"a": 1, "b": 2}` matches `{"b": 2, "a": 1}` because objects are unordered. The bus uses `serde_json::Value::eq`, which is the deep-equality semantics most callers expect.

```rust
// String equality
Filter::eq("status", json!("running"))

// Numeric equality
Filter::eq("retry_count", json!(3))

// Boolean
Filter::eq("verified", json!(true))

// Null
Filter::eq("deleted_at", json!(null))

// Nested
Filter::eq("config", json!({"mode": "fast", "timeout_ms": 100}))
```

There is no fuzzy match, no range, no regex. If you need range or regex, fold over the events at the application layer or use a capability predicate at subscription time.

## Evaluation semantics

Two edge cases worth knowing about:

- **Empty `$and` matches nothing.** A literal `{"$and": []}` is treated as "the predicate has no satisfiable form," not as "matches everything." This is deliberate: an externally-supplied filter with an accidentally empty AND would otherwise silently pass through every event. If you want "match everything," omit the filter from the `ConsumeRequest` entirely.
- **Empty `$or` matches nothing.** Same shape as Rust's `Iterator::any` on an empty iterator — there's no element to match, so the predicate is false.

## Serialization

Filters round-trip through JSON. The same filter you build in Rust can be serialized, transmitted, and reconstructed on another node (or in another language) without semantic drift:

```rust
let filter = Filter::and(vec![
    Filter::eq("level", json!("error")),
    Filter::eq("service", json!("api")),
]);

let json = filter.to_json()?;
// {"$and":[{"path":"level","value":"error"},{"path":"service","value":"api"}]}

let parsed = Filter::from_json(&json)?;
```

The serialized form is what travels on the wire when a subscriber attaches a filter to a subscription, and it's what rides in nRPC's `net-where` header for capability-targeted calls.

## Performance

Filter evaluation is single-pass over the event payload. The dot-path accessor traverses the JSON tree once, the equality check is a `serde_json::Value::eq`, and the boolean composition short-circuits on the first decisive child. On modern hardware, evaluating a depth-3 path against a 1 KB JSON event runs in single-digit microseconds.

Filtering is post-retrieval — the adapter doesn't push predicates down. If you have an adapter-side prefiltering need (Redis Streams' `XREAD COUNT`, JetStream's subject filtering), apply that at the adapter level, then use the bus's filter for the in-memory pass.

## Comparison with capability predicates

The bus filter and the capability predicate AST are different languages with different jobs. The bus filter is equality-only because that's what's fast and what 99% of bus subscribers need. The capability predicate adds existence, numeric comparison, semver, and string matching because capability decisions need that vocabulary.

The two compose: an nRPC call carries a capability predicate (`net-where`) to target receivers and a filter (on the response stream) to narrow what comes back. Use whichever fits the question.
