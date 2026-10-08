//! The capability fold's tag dictionary: one canonical [`TagStr`] per
//! distinct tag, with fold-owned use counts and a fail-closed budget.
//!
//! CAPABILITY_FOLD_SCALE_PLAN.md Slice 6, under "Owner rulings and
//! corrected B1 contract".
//!
//! - **Fold-owned liveness.** A tag's use count is the number of tag
//!   occurrences across the fold's stored entries, moved only by
//!   [`TagDictionary::admit`] and [`TagDictionary::release`]. When the
//!   last stored use goes, the tag leaves the dictionary, whatever
//!   handles a caller still holds (an `Arc` count would not say that: a
//!   query result or snapshot clone keeps it above one).
//! - **Accepted mutations only.** `admit` is called after merge has
//!   decided Insert or Replace, under the fold's write guards. It is
//!   all-or-nothing: it decides first, touching nothing, and commits
//!   only when the result fits.
//! - **Net budget.** A replacement whose outgoing payload holds a tag's
//!   last use frees that tag's room for the incoming payload.
//! - **Warm refresh allocates nothing.** When every incoming tag is
//!   already canonical, admission is lookups and counter moves: no
//!   insert, no scratch growth.

use std::collections::{HashMap, HashSet};

use super::state::PayloadRejection;
use super::tag_str::TagStr;

/// Most tags one capability advertisement may carry, duplicates
/// included.
///
/// 8,192, raised from the first ruling's 256: a tool contributes 4-7 tags
/// (its `software.tool.<i>.*` encoding, `ai-tool:` and a served `nrpc:`
/// tag), so 256 capped a node at ~40-50 tools or ~250 public services,
/// which broke tool-heavy nodes (CAPABILITY_FOLD_SCALE_PLAN.md, "Tag cap
/// raised"). With [`MAX_CAPABILITY_TAG_LEN`] this bounds one
/// advertisement's tag data at 2 MiB.
pub const MAX_CAPABILITY_TAGS: usize = 8192;

/// Most UTF-8 bytes one capability tag may hold.
pub const MAX_CAPABILITY_TAG_LEN: usize = 256;

/// Check one advertisement's tags against [`MAX_CAPABILITY_TAGS`] and
/// [`MAX_CAPABILITY_TAG_LEN`]. Counts the vector as given, duplicates
/// included, and measures UTF-8 bytes. Nothing is truncated or dropped:
/// a violation rejects the whole advertisement.
pub fn validate_capability_tags<T: AsRef<str>>(tags: &[T]) -> Result<(), PayloadRejection> {
    if tags.len() > MAX_CAPABILITY_TAGS {
        return Err(PayloadRejection::TooManyTags {
            count: tags.len(),
            max: MAX_CAPABILITY_TAGS,
        });
    }
    for (index, tag) in tags.iter().enumerate() {
        let len = tag.as_ref().len();
        if len > MAX_CAPABILITY_TAG_LEN {
            return Err(PayloadRejection::TagTooLong {
                index,
                len,
                max: MAX_CAPABILITY_TAG_LEN,
            });
        }
    }
    Ok(())
}

/// The capability fold's canonical-tag budget, fixed at fold creation.
///
/// The defaults bound fold-owned canonical tag storage. They are not a
/// process-memory bound or a capacity guarantee. Dictionary and table
/// overhead is reported separately and not counted here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TagBudget {
    /// Most distinct canonical tags.
    pub max_tags: usize,
    /// Most UTF-8 bytes across distinct canonical tags.
    pub max_bytes: usize,
}

impl TagBudget {
    /// Default distinct-tag ceiling.
    pub const DEFAULT_MAX_TAGS: usize = 1_000_000;
    /// Default canonical-byte ceiling: 64 MiB.
    pub const DEFAULT_MAX_BYTES: usize = 64 * 1024 * 1024;
}

impl Default for TagBudget {
    fn default() -> Self {
        Self {
            max_tags: Self::DEFAULT_MAX_TAGS,
            max_bytes: Self::DEFAULT_MAX_BYTES,
        }
    }
}

/// Dictionary counters, for `FoldStats`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TagDictionaryStats {
    /// Distinct canonical tags held.
    pub tags: u64,
    /// UTF-8 bytes across them: what the budget counts.
    pub bytes: u64,
    /// Estimated dictionary overhead beyond those bytes: the table and
    /// each canonical allocation's header. Not counted by the budget.
    pub overhead_bytes: u64,
    /// Advertisements refused for the budget.
    pub budget_rejections: u64,
}

/// See the module doc.
#[derive(Debug, Default)]
pub struct TagDictionary {
    /// Canonical tag → number of stored occurrences.
    ///
    /// `u64`: an occurrence count must stay exact for its tag to retire
    /// on the last release. A tag can occur up to `MAX_CAPABILITY_TAGS`
    /// times in each of up to `u32::MAX` entries, past `u32`; a saturated
    /// count would never return to zero (cubic, PR #1210).
    uses: HashMap<TagStr, u64>,
    /// Sum of `len()` over `uses`' keys.
    bytes: usize,
    budget: TagBudget,
    budget_rejections: u64,
}

impl TagDictionary {
    /// An empty dictionary with `budget`.
    pub fn with_budget(budget: TagBudget) -> Self {
        Self {
            budget,
            ..Self::default()
        }
    }

    /// The configured budget.
    pub fn budget(&self) -> TagBudget {
        self.budget
    }

    /// Drop every tag, keeping the budget and the counters.
    pub fn clear(&mut self) {
        self.uses.clear();
        self.bytes = 0;
    }

    /// Admit an accepted mutation: `incoming` is about to be stored, and
    /// `outgoing` (a Replace's old payload) is about to go.
    ///
    /// On success every incoming tag is swapped for its canonical
    /// allocation, incoming uses are counted and outgoing uses released.
    /// On refusal nothing has changed: not the dictionary, not
    /// `incoming`.
    pub fn admit(
        &mut self,
        incoming: &mut [TagStr],
        outgoing: Option<&[TagStr]>,
    ) -> Result<(), PayloadRejection> {
        self.check(incoming, outgoing)?;
        for tag in incoming.iter_mut() {
            let canonical = self
                .uses
                .get_key_value(tag.as_str())
                .map(|(k, _)| k.clone());
            match canonical {
                Some(canonical) => {
                    // A second lookup to bump the count: neither allocates.
                    if let Some(count) = self.uses.get_mut(canonical.as_str()) {
                        *count = count.saturating_add(1);
                    }
                    *tag = canonical;
                }
                None => {
                    self.bytes += tag.len();
                    self.uses.insert(tag.clone(), 1);
                }
            }
        }
        if let Some(outgoing) = outgoing {
            self.release(outgoing);
        }
        Ok(())
    }

    /// Release one stored payload's uses: removal by eviction, expiry or
    /// a restore unwind. A tag whose last use this was leaves the
    /// dictionary.
    pub fn release(&mut self, tags: &[TagStr]) {
        for tag in tags {
            let gone = match self.uses.get_mut(tag.as_str()) {
                Some(count) => {
                    *count = count.saturating_sub(1);
                    *count == 0
                }
                None => {
                    debug_assert!(false, "released a tag the dictionary does not hold");
                    false
                }
            };
            if gone {
                self.uses.remove(tag.as_str());
                self.bytes -= tag.len();
            }
        }
    }

    /// Decide whether admitting `incoming` (and releasing `outgoing`)
    /// fits the budget, without changing anything.
    ///
    /// Linear in the payload sizes: it runs under the fold's write locks,
    /// and at [`MAX_CAPABILITY_TAGS`] a pairwise scan would be ~10⁸
    /// comparisons per announcement (PR #1210 review).
    fn check(
        &mut self,
        incoming: &[TagStr],
        outgoing: Option<&[TagStr]>,
    ) -> Result<(), PayloadRejection> {
        if incoming
            .iter()
            .all(|tag| self.uses.contains_key(tag.as_str()))
        {
            // Nothing new, and the release can only shrink the
            // dictionary. The warm refresh stops here, allocating nothing.
            return Ok(());
        }

        // New distinct tags, deduplicated within the payload. Only a
        // payload that brings new tags builds these sets.
        let mut fresh: HashSet<&str> = HashSet::new();
        let mut new_bytes = 0usize;
        for tag in incoming {
            if !self.uses.contains_key(tag.as_str()) && fresh.insert(tag.as_str()) {
                new_bytes += tag.len();
            }
        }
        let new_tags = fresh.len();

        // Room the outgoing payload frees: tags whose every stored use
        // is in `outgoing` and that `incoming` does not carry again.
        let (mut freed_tags, mut freed_bytes) = (0usize, 0usize);
        if let Some(outgoing) = outgoing {
            let mut occurrences: HashMap<&str, u64> = HashMap::with_capacity(outgoing.len());
            for tag in outgoing {
                *occurrences.entry(tag.as_str()).or_insert(0) += 1;
            }
            let carried: HashSet<&str> = incoming.iter().map(TagStr::as_str).collect();
            for (tag, occurrences) in occurrences {
                let stored = self.uses.get(tag).copied().unwrap_or(0);
                if stored == occurrences && !carried.contains(tag) {
                    freed_tags += 1;
                    freed_bytes += tag.len();
                }
            }
        }

        let tags_after = self.uses.len() - freed_tags + new_tags;
        let bytes_after = self.bytes - freed_bytes + new_bytes;
        if tags_after > self.budget.max_tags || bytes_after > self.budget.max_bytes {
            self.budget_rejections += 1;
            return Err(PayloadRejection::TagBudget {
                new_tags,
                new_bytes,
                live_tags: self.uses.len(),
                live_bytes: self.bytes,
                max_tags: self.budget.max_tags,
                max_bytes: self.budget.max_bytes,
            });
        }
        Ok(())
    }

    /// Whether a restore of `payloads` (the effective restored state,
    /// into an emptied dictionary) fits the budget.
    pub fn preflight<'a>(
        &self,
        payloads: impl IntoIterator<Item = &'a [TagStr]>,
    ) -> Result<(), PayloadRejection> {
        let mut distinct: HashSet<&str> = HashSet::new();
        let mut bytes = 0usize;
        for tags in payloads {
            for tag in tags {
                if distinct.insert(tag.as_str()) {
                    bytes += tag.len();
                }
            }
        }
        if distinct.len() > self.budget.max_tags || bytes > self.budget.max_bytes {
            return Err(PayloadRejection::TagBudget {
                new_tags: distinct.len(),
                new_bytes: bytes,
                live_tags: 0,
                live_bytes: 0,
                max_tags: self.budget.max_tags,
                max_bytes: self.budget.max_bytes,
            });
        }
        Ok(())
    }

    /// Current counters.
    pub fn stats(&self) -> TagDictionaryStats {
        // hashbrown: one (TagStr, u64) slot plus one control byte per
        // bucket; buckets ≈ capacity · 8/7. Each canonical allocation
        // carries an `Arc` header of two counts.
        let slot = std::mem::size_of::<(TagStr, u64)>() + 1;
        let table = self.uses.capacity() * 8 / 7 * slot;
        let headers = self.uses.len() * 2 * std::mem::size_of::<usize>();
        TagDictionaryStats {
            tags: self.uses.len() as u64,
            bytes: self.bytes as u64,
            overhead_bytes: (table + headers) as u64,
            budget_rejections: self.budget_rejections,
        }
    }

    /// The canonical handle for `tag`, if held. For tests.
    #[cfg(test)]
    pub(crate) fn canonical(&self, tag: &str) -> Option<&TagStr> {
        self.uses.get_key_value(tag).map(|(k, _)| k)
    }

    /// The stored use count for `tag`. For tests.
    #[cfg(test)]
    pub(crate) fn uses_of(&self, tag: &str) -> u64 {
        self.uses.get(tag).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(list: &[&str]) -> Vec<TagStr> {
        list.iter().map(|t| TagStr::from(*t)).collect()
    }

    #[test]
    fn caps_count_duplicates_and_measure_utf8_bytes() {
        let at_cap: Vec<String> = (0..MAX_CAPABILITY_TAGS).map(|_| "t".into()).collect();
        assert_eq!(validate_capability_tags(&at_cap), Ok(()));
        let over: Vec<String> = (0..=MAX_CAPABILITY_TAGS).map(|_| "t".into()).collect();
        assert_eq!(
            validate_capability_tags(&over),
            Err(PayloadRejection::TooManyTags {
                count: MAX_CAPABILITY_TAGS + 1,
                max: MAX_CAPABILITY_TAGS
            }),
            "duplicates count"
        );
        // 'é' is two UTF-8 bytes: 128 of them is exactly 256 bytes.
        let exact = "é".repeat(128);
        assert_eq!(exact.chars().count(), 128);
        assert_eq!(validate_capability_tags(&[exact.as_str()]), Ok(()));
        let one_over = format!("{exact}a");
        assert_eq!(
            validate_capability_tags(&["ok", one_over.as_str()]),
            Err(PayloadRejection::TagTooLong {
                index: 1,
                len: 257,
                max: 256
            })
        );
    }

    #[test]
    fn admit_canonicalizes_and_counts_fold_owned_uses() {
        let mut dict = TagDictionary::default();
        let mut a = tags(&["gpu", "cpu", "gpu"]);
        dict.admit(&mut a, None).expect("admit");
        let mut b = tags(&["gpu"]);
        dict.admit(&mut b, None).expect("admit");
        assert!(b[0].shares_allocation_with(&a[0]), "one allocation per tag");
        assert_eq!(dict.uses_of("gpu"), 3);
        assert_eq!(dict.stats().tags, 2);
        assert_eq!(dict.stats().bytes, 6);

        // A reader pins a handle; the fold's last use still retires it.
        let pinned = a[1].clone();
        dict.release(&a);
        assert_eq!(dict.uses_of("cpu"), 0);
        assert!(dict.canonical("cpu").is_none(), "retired despite the pin");
        assert_eq!(pinned, "cpu");
        dict.release(&b);
        let stats = dict.stats();
        assert_eq!((stats.tags, stats.bytes), (0, 0), "nothing held");
    }

    #[test]
    fn a_refused_admission_changes_nothing() {
        let mut dict = TagDictionary::with_budget(TagBudget {
            max_tags: 2,
            max_bytes: 1024,
        });
        let mut held = tags(&["a", "b"]);
        dict.admit(&mut held, None).expect("fits");
        let mut over = tags(&["a", "c"]);
        let before = over.clone();
        let err = dict.admit(&mut over, None).expect_err("third tag");
        assert!(matches!(
            err,
            PayloadRejection::TagBudget { new_tags: 1, .. }
        ));
        assert_eq!(dict.uses_of("a"), 1, "no use counted");
        assert!(dict.canonical("c").is_none(), "no tag inserted");
        assert!(
            !over[0].shares_allocation_with(&held[0]),
            "payload untouched"
        );
        assert_eq!(over, before);
        assert_eq!(dict.stats().budget_rejections, 1);
    }

    #[test]
    fn a_replacement_frees_its_last_use_for_the_new_tag() {
        let mut dict = TagDictionary::with_budget(TagBudget {
            max_tags: 2,
            max_bytes: 1024,
        });
        let mut other = tags(&["shared"]);
        dict.admit(&mut other, None).expect("other publisher");
        let mut old = tags(&["shared", "old"]);
        dict.admit(&mut old, None).expect("old");
        // Full. "old" is this entry's last use, so replacing it with
        // "new" nets to the same count.
        let mut new = tags(&["shared", "new"]);
        dict.admit(&mut new, Some(&old)).expect("net fit");
        assert!(dict.canonical("old").is_none());
        assert_eq!(dict.uses_of("shared"), 2);
        assert_eq!(dict.uses_of("new"), 1);

        // "shared" is still used by `other`, so dropping it frees nothing.
        let mut third = tags(&["new", "third"]);
        assert!(dict.admit(&mut third, Some(&new)).is_err());
    }

    /// A use count past `u32::MAX` stays exact: no saturation that would
    /// keep the tag in the dictionary after its last release.
    #[test]
    fn use_counts_stay_exact_past_u32() {
        let mut dict = TagDictionary::default();
        let mut first = tags(&["t"]);
        dict.admit(&mut first, None).expect("admit");
        if let Some(count) = dict.uses.get_mut("t") {
            *count = u64::from(u32::MAX);
        }
        let mut more = tags(&["t"]);
        dict.admit(&mut more, None).expect("one more use");
        assert_eq!(
            dict.uses_of("t"),
            u64::from(u32::MAX) + 1,
            "exact, not saturated"
        );
        if let Some(count) = dict.uses.get_mut("t") {
            *count = 2;
        }
        dict.release(&more);
        dict.release(&first);
        assert!(dict.canonical("t").is_none(), "the last release retires it");
    }

    #[test]
    fn duplicates_within_a_payload_are_one_new_tag() {
        let mut dict = TagDictionary::with_budget(TagBudget {
            max_tags: 1,
            max_bytes: 1024,
        });
        let mut dup = tags(&["x", "x", "x"]);
        dict.admit(&mut dup, None).expect("one distinct tag");
        assert_eq!(dict.uses_of("x"), 3);
    }

    #[test]
    fn preflight_counts_the_effective_state_once() {
        let dict = TagDictionary::with_budget(TagBudget {
            max_tags: 2,
            max_bytes: 1024,
        });
        let rows = [tags(&["a", "b"]), tags(&["b", "a"])];
        assert!(dict.preflight(rows.iter().map(Vec::as_slice)).is_ok());
        let rows = [tags(&["a", "b"]), tags(&["c"])];
        assert!(dict.preflight(rows.iter().map(Vec::as_slice)).is_err());
    }

    /// The pairwise budget arithmetic `check` used before it went linear
    /// (PR #1210 review): `(tags_after, bytes_after)`, or `None` when the
    /// payload brings nothing new.
    fn pairwise_oracle(
        dict: &TagDictionary,
        incoming: &[TagStr],
        outgoing: Option<&[TagStr]>,
    ) -> Option<(usize, usize)> {
        let mut fresh: Vec<usize> = Vec::new();
        for (at, tag) in incoming.iter().enumerate() {
            if dict.uses.contains_key(tag.as_str()) {
                continue;
            }
            if fresh.iter().any(|&seen| incoming[seen] == *tag) {
                continue;
            }
            fresh.push(at);
        }
        if fresh.is_empty() {
            return None;
        }
        let new_tags = fresh.len();
        let new_bytes: usize = fresh.iter().map(|&at| incoming[at].len()).sum();
        let (mut freed_tags, mut freed_bytes) = (0usize, 0usize);
        if let Some(outgoing) = outgoing {
            for (at, tag) in outgoing.iter().enumerate() {
                if outgoing[..at].contains(tag) {
                    continue;
                }
                let occurrences = outgoing.iter().filter(|t| *t == tag).count();
                let stored = dict.uses.get(tag.as_str()).copied().unwrap_or(0) as usize;
                if stored == occurrences && !incoming.contains(tag) {
                    freed_tags += 1;
                    freed_bytes += tag.len();
                }
            }
        }
        Some((
            dict.uses.len() - freed_tags + new_tags,
            dict.bytes - freed_bytes + new_bytes,
        ))
    }

    /// The linear `check` decides exactly as the pairwise one did, at the
    /// budget boundary, over replacements mixing duplicates, tags shared
    /// with other payloads, last uses and brand-new tags.
    #[test]
    fn linear_check_matches_the_pairwise_oracle() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move |bound: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % bound
        };
        for _ in 0..200 {
            let mut dict = TagDictionary::default();
            let mut stored: Vec<Vec<TagStr>> = Vec::new();
            for _ in 0..1 + next(4) {
                let mut payload: Vec<TagStr> = (0..next(12))
                    .map(|_| TagStr::from(format!("t{}", next(16))))
                    .collect();
                dict.admit(&mut payload, None).expect("unbounded");
                stored.push(payload);
            }
            let incoming: Vec<TagStr> = (0..next(12))
                .map(|_| TagStr::from(format!("t{}", next(24))))
                .collect();
            let outgoing = if next(3) == 0 {
                None
            } else {
                Some(stored[next(stored.len() as u64) as usize].clone())
            };
            let outgoing = outgoing.as_deref();
            match pairwise_oracle(&dict, &incoming, outgoing) {
                None => {
                    dict.budget = TagBudget {
                        max_tags: 0,
                        max_bytes: 0,
                    };
                    assert!(dict.check(&incoming, outgoing).is_ok(), "nothing new");
                }
                Some((tags_after, bytes_after)) => {
                    dict.budget = TagBudget {
                        max_tags: tags_after,
                        max_bytes: bytes_after,
                    };
                    assert!(dict.check(&incoming, outgoing).is_ok(), "at the boundary");
                    dict.budget.max_tags = tags_after - 1;
                    assert!(dict.check(&incoming, outgoing).is_err(), "one tag over");
                    dict.budget.max_tags = tags_after;
                    dict.budget.max_bytes = bytes_after - 1;
                    assert!(dict.check(&incoming, outgoing).is_err(), "one byte over");
                }
            }
        }
    }
}
