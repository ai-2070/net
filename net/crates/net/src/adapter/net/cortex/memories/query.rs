//! Query builder over `MemoriesState`.
//!
//! Fluent filters compose in any order. Terminal methods (`collect`,
//! `count`, `first`, `exists`) execute against the borrowed state.
//!
//! Tag predicates come in three flavors:
//!
//! - [`MemoriesQuery::where_tag`] — memory must have this one tag.
//! - [`MemoriesQuery::where_any_tag`] — memory must have at least one
//!   tag from the given set (logical OR).
//! - [`MemoriesQuery::where_all_tags`] — memory must have every tag
//!   in the given set (logical AND).

use std::collections::HashSet;
use std::sync::Arc;

use super::state::MemoriesState;
use super::types::{Memory, MemoryId};

/// Ordering for query results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderBy {
    /// By `id`, ascending.
    IdAsc,
    /// By `id`, descending.
    IdDesc,
    /// By `created_ns`, ascending (oldest first).
    CreatedAsc,
    /// By `created_ns`, descending (newest first).
    CreatedDesc,
    /// By `updated_ns`, ascending.
    UpdatedAsc,
    /// By `updated_ns`, descending.
    UpdatedDesc,
}

/// Case-insensitive content-substring needle paired with the ASCII
/// classification of the lowercased form. Per perf #81 — pre-fix
/// the per-memory matcher called `m.content.to_lowercase()`, which
/// allocates a fresh `String` and Unicode-case-folds every byte of
/// the haystack on every match attempt. For a state with 100K
/// memories and 4 KiB avg content that's ~400 MB of allocations
/// plus 400 MB of case-folding per content search.
///
/// The fast path here is "needle is pure ASCII": ASCII
/// [`str::to_lowercase`] is `eq_ignore_ascii_case` byte-for-byte
/// (no Turkic dotless-I edge cases), and bytes ≥ 0x80 in the
/// haystack never `eq_ignore_ascii_case` to any ASCII byte, so a
/// byte-windowed `eq_ignore_ascii_case` scan over the haystack
/// produces the same verdict as the legacy
/// `haystack.to_lowercase().contains(needle)` — without
/// allocating, without Unicode folding. ASCII-ness is a property
/// of the needle (post-lowercase) so we precompute it once at
/// filter-construction; the matcher reads a `bool`.
///
/// Non-ASCII needles still flow through the legacy
/// `to_lowercase().contains(...)` path because the Unicode
/// case-folding tables are the only correct way to handle
/// non-ASCII inputs — but those queries are rare in practice
/// (filter strings are typically `"GROCERY"`, `"tag"`, an email
/// fragment, etc.). Same fast-path treatment is applied to
/// `cortex/tasks/query.rs` by the same rationale.
#[derive(Debug, Clone)]
pub(super) struct ContentNeedle {
    /// Lowercased form of the user-provided needle.
    lowercased: String,
    /// `true` iff `lowercased.is_ascii()`. Read by [`Self::matches`]
    /// to choose the zero-alloc byte-windowed path.
    is_ascii: bool,
}

impl ContentNeedle {
    pub(super) fn new(needle: impl Into<String>) -> Self {
        let lowercased = needle.into().to_lowercase();
        let is_ascii = lowercased.is_ascii();
        Self {
            lowercased,
            is_ascii,
        }
    }

    /// True if `haystack` contains the needle case-insensitively.
    /// Fast-paths pure-ASCII needles via `eq_ignore_ascii_case`
    /// over haystack byte windows (zero allocation, no Unicode
    /// folding); falls back to the legacy
    /// `to_lowercase().contains(...)` shape for non-ASCII needles.
    pub(super) fn matches(&self, haystack: &str) -> bool {
        if self.is_ascii {
            let h = haystack.as_bytes();
            let n = self.lowercased.as_bytes();
            // Empty needle matches every haystack — preserves the
            // legacy `"".to_lowercase().contains("")` shape.
            if n.is_empty() {
                return true;
            }
            if h.len() < n.len() {
                return false;
            }
            h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
        } else {
            haystack.to_lowercase().contains(&self.lowercased)
        }
    }
}

/// Filter / order / limit configuration. Shared by [`MemoriesQuery`]
/// and (future) reactive watchers.
#[derive(Debug, Clone, Default)]
pub(super) struct MemoriesFilterSpec {
    pub id_in: Option<HashSet<MemoryId>>,
    pub source: Option<String>,
    pub content_contains: Option<ContentNeedle>,
    pub require_tag: Option<String>,
    pub require_any_tag: Option<Vec<String>>,
    pub require_all_tags: Option<Vec<String>>,
    pub only_pinned: Option<bool>,
    pub created_after_ns: Option<u64>,
    pub created_before_ns: Option<u64>,
    pub updated_after_ns: Option<u64>,
    pub updated_before_ns: Option<u64>,
    pub order_by: Option<OrderBy>,
    pub limit: Option<usize>,
}

impl MemoriesFilterSpec {
    pub(super) fn matches(&self, m: &Memory) -> bool {
        if let Some(ids) = &self.id_in {
            if !ids.contains(&m.id) {
                return false;
            }
        }
        if let Some(src) = &self.source {
            if &m.source != src {
                return false;
            }
        }
        if let Some(needle) = &self.content_contains {
            if !needle.matches(&m.content) {
                return false;
            }
        }
        if let Some(tag) = &self.require_tag {
            if !m.tags.iter().any(|t| t == tag) {
                return false;
            }
        }
        // Treat `Some(vec![])` as "no constraint" rather than as a
        // pathological matcher. Pre-fix `require_any_tag(empty)`
        // excluded everything (`any` over an empty list is false),
        // while `require_all_tags(empty)` included everything
        // (`all` over an empty list is true) — asymmetric and
        // trap-prone for callers building filters from UI multi-
        // select widgets that emit empty vectors.
        if let Some(tags) = &self.require_any_tag {
            if !tags.is_empty() && !tags.iter().any(|want| m.tags.iter().any(|t| t == want)) {
                return false;
            }
        }
        if let Some(tags) = &self.require_all_tags {
            if !tags.is_empty() && !tags.iter().all(|want| m.tags.iter().any(|t| t == want)) {
                return false;
            }
        }
        if let Some(want_pinned) = self.only_pinned {
            if m.pinned != want_pinned {
                return false;
            }
        }
        // Inclusive bounds. Strict `>` / `<` bounds would drop
        // events at the cutoff, breaking pagination using "last
        // sync ns" and dropping one of two events written in the
        // same ns. See `cortex/tasks/query.rs` for context.
        if let Some(ns) = self.created_after_ns {
            if m.created_ns < ns {
                return false;
            }
        }
        if let Some(ns) = self.created_before_ns {
            if m.created_ns > ns {
                return false;
            }
        }
        if let Some(ns) = self.updated_after_ns {
            if m.updated_ns < ns {
                return false;
            }
        }
        if let Some(ns) = self.updated_before_ns {
            if m.updated_ns > ns {
                return false;
            }
        }
        true
    }

    /// Execute and return matching memories as `Vec<Arc<Memory>>`.
    /// Per perf #96 — each match is one atomic refcount bump
    /// instead of the legacy deep `Memory` clone (which carries
    /// three heap-allocated fields). The watcher path benefits
    /// most because `tx.send(initial.clone())` becomes a Vec
    /// clone of Arcs, not Memory deep-clones.
    pub(super) fn execute(&self, state: &MemoriesState) -> Vec<Arc<Memory>> {
        let mut out: Vec<Arc<Memory>> = state
            .memories
            .values()
            .filter(|a| self.matches(a.as_ref()))
            .cloned()
            .collect();
        if let Some(order) = self.order_by {
            sort_memories(&mut out, order);
        }
        if let Some(limit) = self.limit {
            out.truncate(limit);
        }
        out
    }
}

/// Fluent query over `MemoriesState`. Created via [`MemoriesState::query`].
pub struct MemoriesQuery<'a> {
    state: &'a MemoriesState,
    spec: MemoriesFilterSpec,
}

impl MemoriesState {
    /// Start a fluent query over this state snapshot.
    pub fn query(&self) -> MemoriesQuery<'_> {
        MemoriesQuery {
            state: self,
            spec: MemoriesFilterSpec::default(),
        }
    }
}

impl<'a> MemoriesQuery<'a> {
    /// Restrict to memories whose id is in the provided collection.
    pub fn where_id_in(mut self, ids: impl IntoIterator<Item = MemoryId>) -> Self {
        self.spec.id_in = Some(ids.into_iter().collect());
        self
    }

    /// Restrict to memories from this source.
    pub fn where_source(mut self, source: impl Into<String>) -> Self {
        self.spec.source = Some(source.into());
        self
    }

    /// Restrict to memories whose content contains `needle`
    /// (case-insensitive).
    pub fn content_contains(mut self, needle: impl Into<String>) -> Self {
        self.spec.content_contains = Some(ContentNeedle::new(needle));
        self
    }

    /// Restrict to memories tagged with `tag`.
    pub fn where_tag(mut self, tag: impl Into<String>) -> Self {
        self.spec.require_tag = Some(tag.into());
        self
    }

    /// Restrict to memories that have AT LEAST ONE of the given tags.
    pub fn where_any_tag(mut self, tags: impl IntoIterator<Item = String>) -> Self {
        self.spec.require_any_tag = Some(tags.into_iter().collect());
        self
    }

    /// Restrict to memories that have EVERY tag in the given set.
    pub fn where_all_tags(mut self, tags: impl IntoIterator<Item = String>) -> Self {
        self.spec.require_all_tags = Some(tags.into_iter().collect());
        self
    }

    /// Restrict to pinned (`true`) or unpinned (`false`) only.
    pub fn where_pinned(mut self, pinned: bool) -> Self {
        self.spec.only_pinned = Some(pinned);
        self
    }

    /// Restrict to `created_ns >= ns` (inclusive).
    pub fn created_after(mut self, ns: u64) -> Self {
        self.spec.created_after_ns = Some(ns);
        self
    }

    /// Restrict to `created_ns <= ns` (inclusive).
    pub fn created_before(mut self, ns: u64) -> Self {
        self.spec.created_before_ns = Some(ns);
        self
    }

    /// Restrict to `updated_ns >= ns` (inclusive).
    pub fn updated_after(mut self, ns: u64) -> Self {
        self.spec.updated_after_ns = Some(ns);
        self
    }

    /// Restrict to `updated_ns <= ns` (inclusive).
    pub fn updated_before(mut self, ns: u64) -> Self {
        self.spec.updated_before_ns = Some(ns);
        self
    }

    /// Order results.
    pub fn order_by(mut self, order: OrderBy) -> Self {
        self.spec.order_by = Some(order);
        self
    }

    /// Truncate to `n` results after ordering.
    pub fn limit(mut self, n: usize) -> Self {
        self.spec.limit = Some(n);
        self
    }

    /// Execute and collect matching memories. Per perf #96 each
    /// result is `Arc<Memory>` — refcount bump, not deep clone.
    pub fn collect(self) -> Vec<Arc<Memory>> {
        self.spec.execute(self.state)
    }

    /// Count matches. Ignores `limit`.
    pub fn count(self) -> usize {
        self.state
            .memories
            .values()
            .filter(|a| self.spec.matches(a.as_ref()))
            .count()
    }

    /// Return the first match (after `order_by` if set). Returns
    /// `Arc<Memory>` per perf #96.
    pub fn first(mut self) -> Option<Arc<Memory>> {
        self.spec.limit = Some(1);
        self.collect().into_iter().next()
    }

    /// True if any match exists. Short-circuits.
    pub fn exists(self) -> bool {
        self.state
            .memories
            .values()
            .any(|a| self.spec.matches(a.as_ref()))
    }
}

fn sort_memories(memories: &mut [Arc<Memory>], order: OrderBy) {
    match order {
        OrderBy::IdAsc => memories.sort_by_key(|m| m.id),
        OrderBy::IdDesc => memories.sort_by_key(|m| std::cmp::Reverse(m.id)),
        OrderBy::CreatedAsc => memories.sort_by_key(|m| m.created_ns),
        OrderBy::CreatedDesc => memories.sort_by_key(|m| std::cmp::Reverse(m.created_ns)),
        OrderBy::UpdatedAsc => memories.sort_by_key(|m| m.updated_ns),
        OrderBy::UpdatedDesc => memories.sort_by_key(|m| std::cmp::Reverse(m.updated_ns)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(id: MemoryId, content: &str, tags: &[&str], pinned: bool, created: u64) -> Memory {
        Memory {
            id,
            content: content.to_string(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            source: "test".into(),
            created_ns: created,
            updated_ns: created,
            pinned,
        }
    }

    fn sample() -> MemoriesState {
        let mut s = MemoriesState::new();
        for m in [
            mk(1, "Meeting notes", &["work", "notes"], true, 100),
            mk(2, "Grocery list", &["personal", "todo"], false, 200),
            mk(3, "Reading list", &["personal", "reading"], true, 300),
            mk(4, "Sprint plan", &["work", "planning"], false, 400),
            mk(5, "Birthday ideas", &["personal"], false, 500),
        ] {
            s.memories.insert(m.id, Arc::new(m));
        }
        s
    }

    #[test]
    fn test_where_tag_single() {
        let s = sample();
        let mut ids: Vec<_> = s
            .query()
            .where_tag("work")
            .collect()
            .iter()
            .map(|m| m.id)
            .collect();
        ids.sort();
        assert_eq!(ids, vec![1, 4]);
    }

    #[test]
    fn test_where_any_tag_is_or() {
        let s = sample();
        // Any of {reading, planning} → ids 3 (reading), 4 (planning).
        let mut ids: Vec<_> = s
            .query()
            .where_any_tag(["reading".into(), "planning".into()])
            .collect()
            .iter()
            .map(|m| m.id)
            .collect();
        ids.sort();
        assert_eq!(ids, vec![3, 4]);
    }

    #[test]
    fn test_where_all_tags_is_and() {
        let s = sample();
        // All of {personal, reading} → only id 3.
        let ids: Vec<_> = s
            .query()
            .where_all_tags(["personal".into(), "reading".into()])
            .collect()
            .iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, vec![3]);

        // All of {personal, work} → no memory has both → empty.
        let none: Vec<_> = s
            .query()
            .where_all_tags(["personal".into(), "work".into()])
            .collect();
        assert!(none.is_empty());
    }

    /// Pin: passing an empty `Vec` to `require_any_tag` /
    /// `require_all_tags` is treated as "no constraint" — the
    /// filter is skipped. Pre-fix `Some(vec![])` was a
    /// pathological matcher: `require_any_tag(empty)` rejected
    /// every memory (`any` over empty = false), while
    /// `require_all_tags(empty)` accepted every memory (`all`
    /// over empty = true). UI multi-select widgets that emit
    /// empty vectors would silently flip query semantics.
    #[test]
    fn empty_tag_filters_are_treated_as_no_constraint() {
        let s = sample();
        let total = s.memories.len();

        // `require_any_tag(empty)` → no constraint, returns all.
        let any_empty: Vec<_> = s.query().where_any_tag(Vec::<String>::new()).collect();
        assert_eq!(
            any_empty.len(),
            total,
            "require_any_tag(empty) must be treated as no constraint \
             (got {}/{}); pre-fix this rejected every memory",
            any_empty.len(),
            total,
        );

        // `require_all_tags(empty)` → also no constraint, returns
        // all (this branch was already accepting all pre-fix; the
        // assertion ensures the new semantics keep the same
        // result for callers).
        let all_empty: Vec<_> = s.query().where_all_tags(Vec::<String>::new()).collect();
        assert_eq!(
            all_empty.len(),
            total,
            "require_all_tags(empty) must return all memories"
        );
    }

    #[test]
    fn test_where_pinned_toggles() {
        let s = sample();
        let mut pinned_ids: Vec<_> = s
            .query()
            .where_pinned(true)
            .collect()
            .iter()
            .map(|m| m.id)
            .collect();
        pinned_ids.sort();
        assert_eq!(pinned_ids, vec![1, 3]);

        assert_eq!(s.query().where_pinned(false).count(), 3);
    }

    #[test]
    fn test_content_contains_case_insensitive() {
        let s = sample();
        let ids: Vec<_> = s
            .query()
            .content_contains("GROCERY")
            .collect()
            .iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, vec![2]);
    }

    /// Pin perf #81: `ContentNeedle::matches` must produce the
    /// SAME verdict as the legacy
    /// `haystack.to_lowercase().contains(&needle.to_lowercase())`
    /// shape for every (needle, haystack) pair, regardless of
    /// which branch (`is_ascii` fast path vs Unicode-folding
    /// fallback) the matcher takes. A regression that splits the
    /// two paths' semantics would silently break the public
    /// `content_contains` builder for a subset of inputs — these
    /// adapters back operator-visible search surfaces (the
    /// Prisma-ish `MemoriesFilter`), so behavior drift here is
    /// observable as "search box stopped finding rows it used to."
    #[test]
    fn content_needle_matches_legacy_to_lowercase_contains() {
        // (needle, haystack) cases covering both the ASCII fast
        // path and the Unicode-folding fallback. Each pair is
        // checked against the legacy shape inline as the
        // reference. The Unicode cases are deliberately the ones
        // where `to_lowercase` produces real case-folds (ASCII
        // mapping for the haystack body + ASCII needle still
        // exercises the fast path; non-ASCII NEEDLE exercises the
        // fallback).
        let cases: &[(&str, &str)] = &[
            // ASCII fast-path cases — needle is pure ASCII so the
            // byte-windowed eq_ignore_ascii_case scan runs.
            ("GROCERY", "Grocery shopping list"),
            ("grocery", "Grocery shopping list"),
            ("Grocery", "grocery shopping list"),
            ("xyz", "Grocery shopping list"),
            ("", "anything"),
            ("longer than haystack", "short"),
            // Empty haystack with non-empty needle → no match.
            ("a", ""),
            // ASCII needle, non-ASCII haystack. Bytes >= 0x80 in
            // the haystack never `eq_ignore_ascii_case` to ASCII
            // bytes, so non-ASCII positions are naturally
            // rejected — matches legacy.
            ("hello", "héllo world"),
            ("world", "héllo world"),
            // Fallback (non-ASCII needle) cases — needle is
            // Unicode-folded by the slow path.
            ("CAFÉ", "let's grab café tonight"),
            ("café", "let's grab CAFÉ tonight"),
            ("naïve", "a NAÏVE approach"),
            ("Ω", "math symbols: Ω ω"),
        ];
        for (needle, haystack) in cases {
            let reference = haystack
                .to_lowercase()
                .contains(&needle.to_lowercase());
            let actual = ContentNeedle::new(*needle).matches(haystack);
            assert_eq!(
                actual, reference,
                "ContentNeedle({needle:?}).matches({haystack:?}) diverged from legacy",
            );
        }
    }

    #[test]
    fn test_order_by_created_desc_limit() {
        let s = sample();
        let ids: Vec<_> = s
            .query()
            .order_by(OrderBy::CreatedDesc)
            .limit(2)
            .collect()
            .iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, vec![5, 4]);
    }

    #[test]
    fn test_composed_tag_and_pinned() {
        let s = sample();
        // Pinned AND tagged "personal" → id 3.
        let ids: Vec<_> = s
            .query()
            .where_tag("personal")
            .where_pinned(true)
            .collect()
            .iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, vec![3]);
    }

    #[test]
    fn test_where_source() {
        let mut s = sample();
        Arc::make_mut(s.memories.get_mut(&1).unwrap()).source = "llm".into();
        assert_eq!(s.query().where_source("llm").count(), 1);
        assert_eq!(s.query().where_source("test").count(), 4);
    }

    #[test]
    fn test_where_id_in() {
        let s = sample();
        let mut ids: Vec<_> = s
            .query()
            .where_id_in([2, 4, 99])
            .collect()
            .iter()
            .map(|m| m.id)
            .collect();
        ids.sort();
        assert_eq!(ids, vec![2, 4]);
    }

    #[test]
    fn test_first_and_exists() {
        let s = sample();
        let first = s
            .query()
            .where_pinned(true)
            .order_by(OrderBy::CreatedDesc)
            .first()
            .unwrap();
        assert_eq!(first.id, 3);

        assert!(s.query().where_tag("work").exists());
        assert!(!s.query().where_tag("unicorn").exists());
    }

    #[test]
    fn test_empty_state_queries_empty() {
        let s = MemoriesState::new();
        assert_eq!(s.query().count(), 0);
        assert!(s.query().first().is_none());
        assert!(!s.query().exists());
    }
}
