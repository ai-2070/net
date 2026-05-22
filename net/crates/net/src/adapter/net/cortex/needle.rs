//! Case-insensitive substring needle shared by the cortex query
//! builders (memories, tasks). Per perf #81 — the legacy per-item
//! matcher called `field.to_lowercase()` once per item per filter
//! check, allocating a fresh `String` and Unicode-case-folding every
//! byte of the haystack on every match attempt.
//!
//! The fast path here is "needle is pure ASCII": ASCII
//! `str::to_lowercase` is `eq_ignore_ascii_case` byte-for-byte (no
//! Turkic dotless-I edge cases), and bytes ≥ 0x80 in the haystack
//! never `eq_ignore_ascii_case` to any ASCII byte, so a byte-windowed
//! `eq_ignore_ascii_case` scan over the haystack produces the same
//! verdict as the legacy `haystack.to_lowercase().contains(needle)`
//! — without allocating, without Unicode folding. ASCII-ness is a
//! property of the needle (post-lowercase) so we precompute it once
//! at filter-construction; the matcher reads a `bool`.
//!
//! Non-ASCII needles still flow through the legacy
//! `to_lowercase().contains(...)` path because the Unicode
//! case-folding tables are the only correct way to handle non-ASCII
//! inputs — but those queries are rare in practice (filter strings
//! are typically `"GROCERY"`, `"tag"`, an email fragment, etc.).

#[derive(Debug, Clone)]
pub(super) struct AsciiInsensitiveNeedle {
    lowercased: String,
    is_ascii: bool,
}

impl AsciiInsensitiveNeedle {
    pub(super) fn new(needle: impl Into<String>) -> Self {
        let lowercased = needle.into().to_lowercase();
        let is_ascii = lowercased.is_ascii();
        Self {
            lowercased,
            is_ascii,
        }
    }

    /// True if `haystack` contains the needle case-insensitively.
    /// Fast-paths pure-ASCII needles via `eq_ignore_ascii_case` over
    /// haystack byte windows (zero allocation, no Unicode folding);
    /// falls back to the legacy `to_lowercase().contains(...)` shape
    /// for non-ASCII needles.
    pub(super) fn matches(&self, haystack: &str) -> bool {
        if self.is_ascii {
            let h = haystack.as_bytes();
            let n = self.lowercased.as_bytes();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `matches` must produce the SAME verdict as the legacy
    /// `haystack.to_lowercase().contains(&needle.to_lowercase())`
    /// shape across both the ASCII fast path and the Unicode
    /// fallback. Behavior drift here is observable as "search box
    /// stopped finding rows it used to."
    #[test]
    fn matches_legacy_to_lowercase_contains() {
        let cases: &[(&str, &str)] = &[
            ("GROCERY", "Grocery shopping list"),
            ("grocery", "Grocery shopping list"),
            ("Grocery", "grocery shopping list"),
            ("xyz", "Grocery shopping list"),
            ("", "anything"),
            ("longer than haystack", "short"),
            ("a", ""),
            ("hello", "héllo world"),
            ("world", "héllo world"),
            ("CAFÉ", "let's grab café tonight"),
            ("café", "let's grab CAFÉ tonight"),
            ("naïve", "a NAÏVE approach"),
            ("Ω", "math symbols: Ω ω"),
            ("DEPLOY", "Deploy to production"),
        ];
        for (needle, haystack) in cases {
            let reference = haystack.to_lowercase().contains(&needle.to_lowercase());
            let actual = AsciiInsensitiveNeedle::new(*needle).matches(haystack);
            assert_eq!(
                actual, reference,
                "AsciiInsensitiveNeedle({needle:?}).matches({haystack:?}) diverged from legacy",
            );
        }
    }
}
