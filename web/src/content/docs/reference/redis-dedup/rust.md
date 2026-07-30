## The helper in Rust

```rust
use net_sdk::RedisStreamDedup;

let mut dedup = RedisStreamDedup::with_capacity(600_000);

for entry in stream {
    let id = entry.fields["dedup_id"].as_str();
    if !dedup.is_duplicate(id) {
        process(entry);
    }
}
```

`with_capacity(n)`, `new()` (default capacity), `is_duplicate(&str) -> bool`
(test-and-insert), `len()`, `capacity()`, `is_empty()`, `clear()`. Re-exported
as `net_sdk::RedisStreamDedup`; the canonical implementation is
`net::adapter::RedisStreamDedup`.
