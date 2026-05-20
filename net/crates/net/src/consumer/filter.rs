//! JSON predicate filtering for event consumption.
//!
//! The filter engine supports:
//! - Logical operators: `$and`, `$or`, `$not`
//! - Dot-path field access: `"foo.bar.baz"`
//! - Equality matching (values must match exactly)
//!
//! Filtering is performed **after retrieval** from the adapter,
//! not pushed down to the storage layer.

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// Inner equality condition (path + value).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EqCondition {
    /// Dot-separated path to the field (e.g., "foo.bar.baz").
    pub path: String,
    /// Value to match against.
    pub value: JsonValue,
}

/// A filter predicate for matching events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Filter {
    /// Logical AND: all filters must match.
    And {
        /// List of filters that must all match.
        #[serde(rename = "$and")]
        filters: Vec<Filter>,
    },
    /// Logical OR: at least one filter must match.
    Or {
        /// List of filters where at least one must match.
        #[serde(rename = "$or")]
        filters: Vec<Filter>,
    },
    /// Logical NOT: the inner filter must not match.
    Not {
        /// The filter to negate.
        #[serde(rename = "$not")]
        filter: Box<Filter>,
    },
    /// Equality match with $eq wrapper: `{ "$eq": { "path": "...", "value": ... } }`
    EqWrapped {
        /// The equality condition.
        #[serde(rename = "$eq")]
        condition: EqCondition,
    },
    /// Equality match (shorthand): `{ "path": "...", "value": ... }`
    Eq {
        /// Dot-separated path to the field (e.g., "foo.bar.baz").
        path: String,
        /// Value to match against.
        value: JsonValue,
    },
}

impl Filter {
    /// Create an AND filter.
    pub fn and(filters: Vec<Filter>) -> Self {
        Self::And { filters }
    }

    /// Create an OR filter.
    pub fn or(filters: Vec<Filter>) -> Self {
        Self::Or { filters }
    }

    /// Create a NOT filter.
    #[allow(clippy::should_implement_trait)]
    pub fn not(filter: Filter) -> Self {
        Self::Not {
            filter: Box::new(filter),
        }
    }

    /// Create an equality filter.
    pub fn eq(path: impl Into<String>, value: JsonValue) -> Self {
        Self::Eq {
            path: path.into(),
            value,
        }
    }

    /// Check if an event matches this filter.
    ///
    /// Empty `And` children are rejected as "matches nothing" rather
    /// than "matches everything" — `.all()` on an empty iterator
    /// returns `true`, which would silently turn an externally-
    /// supplied filter JSON like `{"and": []}` into a universal
    /// pass-through. Empty `Or` naturally returns `false` via
    /// `.any()` on an empty iterator and keeps its documented
    /// "matches nothing" behavior.
    #[inline]
    pub fn matches(&self, event: &JsonValue) -> bool {
        match self {
            // Single-element fast path: skip the iterator + closure
            // setup and recurse directly. `And { filters: [f] }` and
            // `Or { filters: [f] }` are common after deserializing
            // small filter trees and were otherwise paying iter+all/any
            // overhead per event.
            Self::And { filters } if filters.len() == 1 => filters[0].matches(event),
            Self::Or { filters } if filters.len() == 1 => filters[0].matches(event),
            Self::And { filters } => {
                !filters.is_empty() && filters.iter().all(|f| f.matches(event))
            }
            Self::Or { filters } => filters.iter().any(|f| f.matches(event)),
            Self::Not { filter } => !filter.matches(event),
            Self::EqWrapped { condition } => {
                json_path_get(event, &condition.path) == Some(&condition.value)
            }
            Self::Eq { path, value } => json_path_get(event, path) == Some(value),
        }
    }

    /// Parse a filter from JSON.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Convert the filter to JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

/// Efficient dot-path accessor for JSON values.
///
/// Given a path like `"foo.bar.baz"`, returns `value["foo"]["bar"]["baz"]`.
///
/// # Examples
///
/// ```
/// use serde_json::json;
/// use net::consumer::filter::json_path_get;
///
/// let value = json!({"user": {"name": "Alice", "age": 30}});
/// assert_eq!(json_path_get(&value, "user.name"), Some(&json!("Alice")));
/// assert_eq!(json_path_get(&value, "user.age"), Some(&json!(30)));
/// assert_eq!(json_path_get(&value, "user.missing"), None);
/// ```
#[inline]
pub fn json_path_get<'a>(value: &'a JsonValue, path: &str) -> Option<&'a JsonValue> {
    if path.is_empty() {
        return Some(value);
    }

    let mut current = value;
    for segment in path.split('.') {
        current = match current {
            JsonValue::Object(map) => map.get(segment)?,
            JsonValue::Array(arr) => {
                // Support numeric indexing for arrays
                let idx: usize = segment.parse().ok()?;
                arr.get(idx)?
            }
            _ => return None,
        };
    }
    Some(current)
}

/// Filter builder for fluent API.
#[derive(Debug, Default)]
pub struct FilterBuilder {
    filters: Vec<Filter>,
}

impl FilterBuilder {
    /// Create a new filter builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an equality condition.
    pub fn eq(mut self, path: impl Into<String>, value: JsonValue) -> Self {
        self.filters.push(Filter::eq(path, value));
        self
    }

    /// Build an AND filter from accumulated conditions.
    #[expect(
        clippy::unwrap_used,
        reason = "len == 1 branch guarantees the iterator yields exactly one element"
    )]
    pub fn build_and(self) -> Filter {
        if self.filters.len() == 1 {
            self.filters.into_iter().next().unwrap()
        } else {
            Filter::and(self.filters)
        }
    }

    /// Build an OR filter from accumulated conditions.
    #[expect(
        clippy::unwrap_used,
        reason = "len == 1 branch guarantees the iterator yields exactly one element"
    )]
    pub fn build_or(self) -> Filter {
        if self.filters.len() == 1 {
            self.filters.into_iter().next().unwrap()
        } else {
            Filter::or(self.filters)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_eq_filter() {
        let filter = Filter::eq("type", json!("token"));

        assert!(filter.matches(&json!({"type": "token", "value": "hello"})));
        assert!(!filter.matches(&json!({"type": "message", "value": "hello"})));
        assert!(!filter.matches(&json!({"value": "hello"}))); // Missing field
    }

    /// `Filter::from_json` is reachable from any FFI / SDK path
    /// that accepts an externally-supplied filter. A deeply
    /// nested adversarial input must NOT crash the consumer
    /// thread via stack overflow. We rely on `serde_json`'s
    /// recursion limit (default 128) to reject the JSON form;
    /// this test pins that the limit is in force, so a future
    /// switch to a non-recursive deserializer doesn't silently
    /// open a DoS vector. Constructed depth (10_000) is well
    /// past any plausible user filter and well past serde_json's
    /// limit.
    #[test]
    fn from_json_rejects_adversarially_nested_filter() {
        let depth = 10_000usize;
        let mut json = String::with_capacity(depth * 8 + 32);
        for _ in 0..depth {
            json.push_str(r#"{"$not":"#);
        }
        json.push_str(r#"{"path":"x","value":1}"#);
        for _ in 0..depth {
            json.push('}');
        }

        let parsed = Filter::from_json(&json);
        assert!(
            parsed.is_err(),
            "depth-{depth} filter JSON must be rejected by serde_json's recursion limit"
        );
    }

    /// Programmatic construction bypasses `from_json` and can
    /// nest arbitrarily — but that's a Rust-API-only path, not a
    /// DoS surface. We verify here that `matches` handles a
    /// modest depth (256 — the same `recursion_limit` set in
    /// `lib.rs:55`) without overflow even on a small thread
    /// stack. A future change that materially deepens recursion
    /// per frame (e.g. wrapping in `Box::pin`) would surface
    /// here.
    #[test]
    fn matches_handles_modest_depth_on_small_stack() {
        const DEPTH: usize = 256;
        // Build (depth-many) `Not` wrappers around an Eq leaf.
        let mut f = Filter::eq("x", json!(1));
        for _ in 0..DEPTH {
            f = Filter::not(f);
        }

        // 256 KiB is well below typical defaults; if `matches`
        // were to use materially more than ~1 KiB per frame this
        // would overflow.
        let result = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || f.matches(&json!({"x": 1})))
            .expect("spawn small-stack thread")
            .join()
            .expect("matches() must not panic at depth 256 on a small stack");

        // Even number of `Not` wraps → unchanged truth value.
        assert!(result, "depth-256 nested Not over true Eq should be true");
    }

    #[test]
    fn test_nested_path() {
        let filter = Filter::eq("user.profile.name", json!("Alice"));

        assert!(filter.matches(&json!({
            "user": {
                "profile": {
                    "name": "Alice",
                    "age": 30
                }
            }
        })));

        assert!(!filter.matches(&json!({
            "user": {
                "profile": {
                    "name": "Bob"
                }
            }
        })));
    }

    #[test]
    fn test_array_indexing() {
        let filter = Filter::eq("items.0.name", json!("first"));

        assert!(filter.matches(&json!({
            "items": [
                {"name": "first"},
                {"name": "second"}
            ]
        })));

        assert!(!filter.matches(&json!({
            "items": [
                {"name": "other"}
            ]
        })));
    }

    #[test]
    fn test_and_filter() {
        let filter = Filter::and(vec![
            Filter::eq("type", json!("token")),
            Filter::eq("index", json!(0)),
        ]);

        assert!(filter.matches(&json!({"type": "token", "index": 0})));
        assert!(!filter.matches(&json!({"type": "token", "index": 1})));
        assert!(!filter.matches(&json!({"type": "message", "index": 0})));
    }

    #[test]
    fn test_or_filter() {
        let filter = Filter::or(vec![
            Filter::eq("type", json!("token")),
            Filter::eq("type", json!("message")),
        ]);

        assert!(filter.matches(&json!({"type": "token"})));
        assert!(filter.matches(&json!({"type": "message"})));
        assert!(!filter.matches(&json!({"type": "error"})));
    }

    #[test]
    fn test_not_filter() {
        let filter = Filter::not(Filter::eq("type", json!("error")));

        assert!(filter.matches(&json!({"type": "token"})));
        assert!(filter.matches(&json!({"type": "message"})));
        assert!(!filter.matches(&json!({"type": "error"})));
    }

    #[test]
    fn test_complex_filter() {
        // Match tokens that are either "hello" or "world" but not from user "bot"
        let filter = Filter::and(vec![
            Filter::eq("type", json!("token")),
            Filter::or(vec![
                Filter::eq("value", json!("hello")),
                Filter::eq("value", json!("world")),
            ]),
            Filter::not(Filter::eq("user", json!("bot"))),
        ]);

        assert!(filter.matches(&json!({
            "type": "token",
            "value": "hello",
            "user": "alice"
        })));

        assert!(!filter.matches(&json!({
            "type": "token",
            "value": "hello",
            "user": "bot"  // Excluded by NOT
        })));

        assert!(!filter.matches(&json!({
            "type": "token",
            "value": "other",  // Not hello or world
            "user": "alice"
        })));
    }

    #[test]
    fn test_filter_builder() {
        let filter = FilterBuilder::new()
            .eq("type", json!("token"))
            .eq("active", json!(true))
            .build_and();

        assert!(filter.matches(&json!({"type": "token", "active": true})));
        assert!(!filter.matches(&json!({"type": "token", "active": false})));
    }

    #[test]
    fn test_filter_serialization() {
        let filter = Filter::and(vec![
            Filter::eq("type", json!("token")),
            Filter::not(Filter::eq("error", json!(true))),
        ]);

        let json = filter.to_json().unwrap();
        let parsed: Filter = Filter::from_json(&json).unwrap();

        // Should behave the same after round-trip
        let event = json!({"type": "token", "error": false});
        assert_eq!(filter.matches(&event), parsed.matches(&event));
    }

    #[test]
    fn test_json_path_get() {
        let value = json!({
            "a": {
                "b": {
                    "c": 42
                }
            },
            "arr": [1, 2, 3],
            "nested_arr": [{"x": 10}, {"x": 20}]
        });

        assert_eq!(json_path_get(&value, "a.b.c"), Some(&json!(42)));
        assert_eq!(json_path_get(&value, "arr.1"), Some(&json!(2)));
        assert_eq!(json_path_get(&value, "nested_arr.0.x"), Some(&json!(10)));
        assert_eq!(json_path_get(&value, "missing"), None);
        assert_eq!(json_path_get(&value, "a.b.missing"), None);
        assert_eq!(json_path_get(&value, ""), Some(&value));
    }

    #[test]
    fn test_json_path_get_primitive() {
        // Trying to access path on primitive value
        let value = json!(42);
        assert_eq!(json_path_get(&value, "foo"), None);

        let value = json!("string");
        assert_eq!(json_path_get(&value, "bar"), None);

        let value = json!(true);
        assert_eq!(json_path_get(&value, "baz"), None);

        let value = json!(null);
        assert_eq!(json_path_get(&value, "qux"), None);
    }

    #[test]
    fn test_json_path_get_invalid_array_index() {
        let value = json!({"arr": [1, 2, 3]});
        // Non-numeric index on array
        assert_eq!(json_path_get(&value, "arr.foo"), None);
        // Out of bounds
        assert_eq!(json_path_get(&value, "arr.100"), None);
    }

    #[test]
    fn test_filter_builder_single() {
        // Single filter should not wrap in AND/OR
        let filter = FilterBuilder::new().eq("type", json!("token")).build_and();

        assert!(matches!(filter, Filter::Eq { .. }));

        let filter = FilterBuilder::new().eq("type", json!("token")).build_or();

        assert!(matches!(filter, Filter::Eq { .. }));
    }

    #[test]
    fn test_filter_builder_multiple_or() {
        let filter = FilterBuilder::new()
            .eq("type", json!("a"))
            .eq("type", json!("b"))
            .build_or();

        assert!(filter.matches(&json!({"type": "a"})));
        assert!(filter.matches(&json!({"type": "b"})));
        assert!(!filter.matches(&json!({"type": "c"})));
    }

    #[test]
    fn test_filter_clone() {
        let filter = Filter::and(vec![
            Filter::eq("a", json!(1)),
            Filter::not(Filter::eq("b", json!(2))),
        ]);

        let cloned = filter.clone();
        let event = json!({"a": 1, "b": 3});
        assert_eq!(filter.matches(&event), cloned.matches(&event));
    }

    #[test]
    fn test_filter_debug() {
        let filter = Filter::eq("type", json!("token"));
        let debug = format!("{:?}", filter);
        assert!(debug.contains("Eq"));
        assert!(debug.contains("type"));
    }

    #[test]
    fn test_filter_partial_eq() {
        let f1 = Filter::eq("type", json!("token"));
        let f2 = Filter::eq("type", json!("token"));
        let f3 = Filter::eq("type", json!("other"));

        assert_eq!(f1, f2);
        assert_ne!(f1, f3);
    }

    #[test]
    fn test_empty_and_filter() {
        // Regression (LOW, BUGS.md): empty `And` used to match
        // everything via `.all()` on an empty iterator returning
        // `true`. A filter JSON like `{"and": []}` reaching the
        // matcher would silently become a universal pass-through.
        // Now empty `And` matches nothing, consistent with the
        // conservative "an empty filter isn't a filter" choice.
        let filter = Filter::and(vec![]);
        assert!(
            !filter.matches(&json!({"any": "value"})),
            "empty And must not match — was silently universal-pass before"
        );
    }

    #[test]
    fn test_empty_or_filter() {
        let filter = Filter::or(vec![]);
        // Empty OR should match nothing
        assert!(!filter.matches(&json!({"any": "value"})));
    }

    /// Single-element `And` / `Or` must produce the same result as
    /// the inner filter alone — the fast path in `matches()` recurses
    /// directly without the iterator+closure setup, but it has to be
    /// semantically identical to the iter-based path.
    #[test]
    fn test_single_element_and_or_match_inner_filter() {
        let inner = Filter::eq("k", json!("v"));
        let single_and = Filter::and(vec![inner.clone()]);
        let single_or = Filter::or(vec![inner.clone()]);

        let yes = json!({"k": "v"});
        let no = json!({"k": "other"});

        for ev in &[yes, no] {
            assert_eq!(
                single_and.matches(ev),
                inner.matches(ev),
                "single-element And must match inner: {ev}",
            );
            assert_eq!(
                single_or.matches(ev),
                inner.matches(ev),
                "single-element Or must match inner: {ev}",
            );
        }
    }

    /// Fast path must recurse correctly when the single child is
    /// itself a composite filter (Not, nested And/Or, Eq, etc.) — the
    /// straight-line `filters[0].matches(event)` call has to dispatch
    /// the same way the slow path's closure would.
    #[test]
    fn test_single_element_fast_path_recurses_into_composite() {
        let leaf = Filter::eq("k", json!("v"));
        let yes = json!({"k": "v"});
        let no = json!({"k": "other"});

        // And{[Not{leaf}]}
        let nested_not = Filter::and(vec![Filter::not(leaf.clone())]);
        assert!(!nested_not.matches(&yes));
        assert!(nested_not.matches(&no));

        // Or{[And{[leaf]}]} — both layers hit the fast path.
        let nested_double = Filter::or(vec![Filter::and(vec![leaf.clone()])]);
        assert!(nested_double.matches(&yes));
        assert!(!nested_double.matches(&no));

        // Or{[And{[leaf, leaf2]}]} — outer hits the fast path, inner
        // falls through to the iterator path. Verifies the two paths
        // compose correctly.
        let leaf2 = Filter::eq("x", json!(1));
        let mixed = Filter::or(vec![Filter::and(vec![leaf.clone(), leaf2.clone()])]);
        assert!(mixed.matches(&json!({"k": "v", "x": 1})));
        assert!(!mixed.matches(&json!({"k": "v", "x": 2})));
        assert!(!mixed.matches(&json!({"k": "other", "x": 1})));
    }

    /// Regression: multi-element `And` / `Or` must keep using the
    /// iterator path (not silently fall into the single-element
    /// shortcut). Guards against a future refactor of the fast-path
    /// guard.
    #[test]
    fn test_multi_element_and_or_uses_slow_path() {
        let f1 = Filter::eq("k", json!("v"));
        let f2 = Filter::eq("x", json!(1));

        let and = Filter::and(vec![f1.clone(), f2.clone()]);
        assert!(and.matches(&json!({"k": "v", "x": 1})));
        assert!(!and.matches(&json!({"k": "v", "x": 2})));
        assert!(!and.matches(&json!({"k": "other", "x": 1})));

        let or = Filter::or(vec![f1.clone(), f2.clone()]);
        assert!(or.matches(&json!({"k": "v", "x": 99})));
        assert!(or.matches(&json!({"k": "nope", "x": 1})));
        assert!(!or.matches(&json!({"k": "nope", "x": 2})));
    }

    #[test]
    fn test_filter_builder_default() {
        let builder = FilterBuilder::default();
        let debug = format!("{:?}", builder);
        assert!(debug.contains("FilterBuilder"));
    }

    #[test]
    fn test_eq_wrapped_filter_deserialization() {
        // Test $eq wrapper format: { "$eq": { "path": "type", "value": "token" } }
        let json_str = r#"{"$eq": {"path": "type", "value": "token"}}"#;
        let filter: Filter = serde_json::from_str(json_str).unwrap();

        assert!(filter.matches(&json!({"type": "token", "data": "hello"})));
        assert!(!filter.matches(&json!({"type": "message", "data": "hello"})));
    }

    #[test]
    fn test_eq_wrapped_with_nested_path() {
        // Test $eq with nested path
        let json_str = r#"{"$eq": {"path": "user.role", "value": "admin"}}"#;
        let filter: Filter = serde_json::from_str(json_str).unwrap();

        assert!(filter.matches(&json!({"user": {"role": "admin"}})));
        assert!(!filter.matches(&json!({"user": {"role": "user"}})));
    }

    #[test]
    fn test_eq_wrapped_with_numeric_value() {
        // Test $eq with numeric value
        let json_str = r#"{"$eq": {"path": "count", "value": 42}}"#;
        let filter: Filter = serde_json::from_str(json_str).unwrap();

        assert!(filter.matches(&json!({"count": 42})));
        assert!(!filter.matches(&json!({"count": 41})));
    }

    #[test]
    fn test_eq_wrapped_with_boolean_value() {
        // Test $eq with boolean value
        let json_str = r#"{"$eq": {"path": "active", "value": true}}"#;
        let filter: Filter = serde_json::from_str(json_str).unwrap();

        assert!(filter.matches(&json!({"active": true})));
        assert!(!filter.matches(&json!({"active": false})));
    }

    #[test]
    fn test_eq_wrapped_in_and() {
        // Test $eq wrapped inside $and
        let json_str = r#"{"$and": [{"$eq": {"path": "type", "value": "token"}}, {"$eq": {"path": "index", "value": 0}}]}"#;
        let filter: Filter = serde_json::from_str(json_str).unwrap();

        assert!(filter.matches(&json!({"type": "token", "index": 0})));
        assert!(!filter.matches(&json!({"type": "token", "index": 1})));
        assert!(!filter.matches(&json!({"type": "message", "index": 0})));
    }

    #[test]
    fn test_eq_wrapped_in_or() {
        // Test $eq wrapped inside $or
        let json_str = r#"{"$or": [{"$eq": {"path": "type", "value": "token"}}, {"$eq": {"path": "type", "value": "message"}}]}"#;
        let filter: Filter = serde_json::from_str(json_str).unwrap();

        assert!(filter.matches(&json!({"type": "token"})));
        assert!(filter.matches(&json!({"type": "message"})));
        assert!(!filter.matches(&json!({"type": "error"})));
    }

    #[test]
    fn test_eq_wrapped_in_not() {
        // Test $eq wrapped inside $not
        let json_str = r#"{"$not": {"$eq": {"path": "type", "value": "error"}}}"#;
        let filter: Filter = serde_json::from_str(json_str).unwrap();

        assert!(filter.matches(&json!({"type": "token"})));
        assert!(filter.matches(&json!({"type": "message"})));
        assert!(!filter.matches(&json!({"type": "error"})));
    }

    #[test]
    fn test_both_eq_formats_work() {
        // Test that both shorthand and wrapped formats work
        let shorthand = r#"{"path": "type", "value": "token"}"#;
        let wrapped = r#"{"$eq": {"path": "type", "value": "token"}}"#;

        let filter1: Filter = serde_json::from_str(shorthand).unwrap();
        let filter2: Filter = serde_json::from_str(wrapped).unwrap();

        let event = json!({"type": "token"});
        assert!(filter1.matches(&event));
        assert!(filter2.matches(&event));

        let event2 = json!({"type": "other"});
        assert!(!filter1.matches(&event2));
        assert!(!filter2.matches(&event2));
    }
}
