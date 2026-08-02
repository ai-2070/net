//! Hierarchical subnet identifier — a **topology coordinate, not an
//! authority**.
//!
//! Encodes a 4-level hierarchy into a `u32`. Each level gets 8 bits
//! (256 values). Parent/child/sibling relationships are resolved with
//! bitwise operations at wire speed.
//!
//! ```text
//! subnet_id (u32):
//!   [level_0: 8 bits] [level_1: 8 bits] [level_2: 8 bits] [level_3: 8 bits]
//! ```
//!
//! # Topology is not authority
//!
//! A `SubnetId` says *where a node sits* in one installation's local
//! hierarchy (a vehicle, machine, or site — not a fleet directory;
//! fleet membership lives in org identity). Holding, deriving, or
//! claiming a `SubnetId` grants nothing: channel access needs a
//! channel token, provider effects need provider admission, and
//! protected transport rights (`ATTACH`/`ROUTE`/`EXPORT`) need a
//! `SubnetGrant` under an authority-qualified `SubnetRef`
//! (SUBNET_AUTH_PLAN.md). The security-facing alias for this type is
//! [`TopologySubnetId`]; the plain name `SubnetId` remains for the
//! existing topology/visibility surface.

/// Maximum number of hierarchy levels.
pub const MAX_DEPTH: u8 = 4;

/// Longest possible ancestor chain: every level plus the global root.
///
/// This is the constant that makes protected-forwarding cost a
/// function of the hierarchy rather than of an operator's grant count
/// — see [`SubnetId::ancestor_path`].
pub const MAX_ANCESTOR_PATH: usize = MAX_DEPTH as usize + 1;

/// Hierarchical subnet identifier.
///
/// Zero (`0x00000000`) means global / no subnet — as a *grant scope*
/// under an authority it is the authority-local root, covering every
/// path (see [`Self::is_ancestor_or_self_of`]). Trailing zeros mean
/// "no sub-level specified" — `SubnetId::new(&[3, 7])` restricts the
/// first two levels and leaves the deeper two unrestricted. Level
/// meanings are operator-chosen within one installation's hierarchy;
/// fleet, customer, and geography populations belong in org identity,
/// not in these four levels.
///
/// `Ord` is derived on the inner `u32` representation. The order
/// has no semantic meaning for the hierarchy (it does NOT match
/// ancestor/descendant relationships); it exists purely as a
/// deterministic tiebreaker for callers that need a total order
/// over `SubnetId`s — e.g. `correlation.rs::analyze_subnet_correlation`
/// needs ties at the same depth to resolve consistently across runs
/// rather than depending on `HashMap` iteration order.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Default,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct SubnetId(u32);

impl SubnetId {
    /// Global / no subnet.
    pub const GLOBAL: Self = Self(0);

    /// Maximum hierarchy depth supported by the encoding — same
    /// value as the module-level `MAX_DEPTH` constant, exposed
    /// as an associated const so operator tooling and the SDK can
    /// reach it through the type without an extra `use`.
    pub const MAX_DEPTH: u8 = MAX_DEPTH;

    /// Create a subnet ID from hierarchy levels (up to 4).
    ///
    /// Levels are packed MSB-first: `&[3, 7]` becomes `0x03_07_00_00`.
    ///
    /// # Panics
    /// Panics if more than 4 levels are provided. For untrusted
    /// input (config / FFI / JSON) prefer [`Self::try_new`].
    #[expect(
        clippy::expect_used,
        reason = "documented panicking variant; try_new is the fallible alternative for untrusted input"
    )]
    pub fn new(levels: &[u8]) -> Self {
        Self::try_new(levels).expect("SubnetId::new: too many levels (use try_new for fallible)")
    }

    /// Fallible variant of [`Self::new`].
    ///
    /// Pre-existing `new` panics on `levels.len() >
    /// MAX_DEPTH`. Returns [`super::SubnetError::TooManyLevels`]
    /// instead so a malformed config doesn't crash the daemon
    /// loader.
    pub fn try_new(levels: &[u8]) -> Result<Self, super::SubnetError> {
        if levels.len() > MAX_DEPTH as usize {
            return Err(super::SubnetError::TooManyLevels {
                got: levels.len(),
                max: MAX_DEPTH,
            });
        }
        let mut val = 0u32;
        for (i, &level) in levels.iter().enumerate() {
            val |= (level as u32) << (24 - i * 8);
        }
        Ok(Self(val))
    }

    /// Create from raw u32 value.
    #[inline]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Get the raw u32 value.
    #[inline]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Extract a specific level (0-3). Returns 0 for unset levels.
    #[inline]
    pub const fn level(self, n: u8) -> u8 {
        if n >= MAX_DEPTH {
            return 0;
        }
        ((self.0 >> (24 - n * 8)) & 0xFF) as u8
    }

    /// Number of non-zero hierarchy levels.
    ///
    /// `SubnetId::new(&[3, 7, 0, 0])` has depth 2.
    pub fn depth(self) -> u8 {
        for d in (0..MAX_DEPTH).rev() {
            if self.level(d) != 0 {
                return d + 1;
            }
        }
        0
    }

    /// Check if this is the global (zero) subnet.
    #[inline]
    pub const fn is_global(self) -> bool {
        self.0 == 0
    }

    /// Get the parent subnet (zero out the deepest non-zero level).
    ///
    /// `SubnetId::new(&[3, 7, 2])` → `SubnetId::new(&[3, 7])`.
    /// `SubnetId::GLOBAL` → `SubnetId::GLOBAL`.
    pub fn parent(self) -> Self {
        let d = self.depth();
        if d == 0 {
            return Self::GLOBAL;
        }
        let mask = Self::mask_for_depth(d - 1);
        Self(self.0 & mask)
    }

    /// Check if `self` is an ancestor of `other` (prefix match).
    ///
    /// Global is ancestor of everything. A subnet is its own ancestor.
    /// Same relation as [`Self::is_ancestor_or_self_of`]; that name is
    /// the canonical one for security-facing scope containment.
    #[inline]
    pub fn is_ancestor_of(self, other: Self) -> bool {
        self.is_ancestor_or_self_of(other)
    }

    /// Canonical fixed-width scope-containment operation
    /// (SUBNET_AUTH_PLAN.md D1): `true` iff `self` is `target` or an
    /// ancestor of `target` in the 4-level prefix hierarchy.
    ///
    /// One masked `u32` comparison — no allocation, no string
    /// parsing, no graph walk, no policy lookup. `SubnetGrant` scope
    /// checks and the compiled forwarding context reduce to this
    /// operation, so its truth table is pinned by test before any
    /// caller lands:
    ///
    /// ```text
    /// scope A     contains target A       true
    /// scope A     contains target A/B     true
    /// scope A/B   contains target A       false
    /// scope A/B   contains target A/C     false
    /// scope A/B   contains target A/B/C   true
    /// scope 0     contains target 0       true
    /// scope 0     contains target A       true   (authority-local root)
    /// scope A     contains target 0       false
    /// ```
    ///
    /// Path `0` ([`Self::GLOBAL`]) is the authority-local root, not an
    /// absent or wildcard value: a scope of `0` covers every present
    /// and future path under its authority, and a target of `0` is
    /// contained only by scope `0`. Authority qualification (equal
    /// paths under different authorities are unrelated) lives one
    /// layer up on `SubnetRef`; this method compares paths only.
    #[inline]
    pub fn is_ancestor_or_self_of(self, target: Self) -> bool {
        if self.is_global() {
            return true;
        }
        let d = self.depth();
        let mask = Self::mask_for_depth(d);
        (self.0 & mask) == (target.0 & mask)
    }

    /// Walk this path and every ancestor of it, deepest first, ending
    /// at [`Self::GLOBAL`].
    ///
    /// `3.7.2` yields `3.7.2`, `3.7`, `3`, `global`. At most
    /// [`MAX_ANCESTOR_PATH`] items, always.
    ///
    /// This is the enumeration that lets scope questions be answered
    /// by *lookup* rather than by scan. Every scope containing a given
    /// path is on that path's ancestor chain and nowhere else, so a
    /// caller holding an index keyed by scope can probe the four-odd
    /// candidates directly instead of testing containment against each
    /// scope it happens to hold. See
    /// `VerifiedGatewayContextSet::authorize_transition`.
    #[inline]
    pub fn ancestor_path(self) -> AncestorPath {
        AncestorPath { next: Some(self) }
    }

    /// The deepest path that contains both `self` and `other`.
    ///
    /// Their longest common prefix: `3.7.2` and `3.7.9` share `3.7`;
    /// `3.7` and `4.1` share `global`. A path contains both `self` and
    /// `other` **iff** it is an ancestor-or-self of this result, which
    /// is what bounds a two-endpoint containment question to one
    /// ancestor chain instead of two.
    ///
    /// A zero level terminates the comparison rather than matching,
    /// because zero means "level unset", not "level equal to zero".
    pub fn common_ancestor(self, other: Self) -> Self {
        let mut d = 0u8;
        while d < MAX_DEPTH {
            let (a, b) = (self.level(d), other.level(d));
            if a == 0 || b == 0 || a != b {
                break;
            }
            d += 1;
        }
        Self(self.0 & Self::mask_for_depth(d))
    }

    /// Check if two IDs are in the same subnet (identical values).
    #[inline]
    pub const fn is_same_subnet(self, other: Self) -> bool {
        self.0 == other.0
    }

    /// Check if two IDs share the same parent.
    pub fn is_sibling(self, other: Self) -> bool {
        let d1 = self.depth();
        let d2 = other.depth();
        if d1 != d2 || d1 == 0 {
            return false;
        }
        let mask = Self::mask_for_depth(d1 - 1);
        (self.0 & mask) == (other.0 & mask) && self.0 != other.0
    }

    /// Get the bitmask for a given depth.
    ///
    /// depth=0 → 0x00000000 (global)
    /// depth=1 → 0xFF000000
    /// depth=2 → 0xFFFF0000
    /// depth=3 → 0xFFFFFF00
    /// depth=4 → 0xFFFFFFFF
    #[inline]
    pub const fn mask_for_depth(depth: u8) -> u32 {
        match depth {
            0 => 0x00000000,
            1 => 0xFF000000,
            2 => 0xFFFF0000,
            3 => 0xFFFFFF00,
            _ => 0xFFFFFFFF,
        }
    }
}

/// Iterator over a path and its ancestors, deepest first
/// ([`SubnetId::ancestor_path`]).
///
/// Yields at most [`MAX_ANCESTOR_PATH`] items and never allocates, so
/// a packet path can walk it on the stack.
#[derive(Debug, Clone, Copy)]
pub struct AncestorPath {
    next: Option<SubnetId>,
}

impl Iterator for AncestorPath {
    type Item = SubnetId;

    fn next(&mut self) -> Option<SubnetId> {
        let current = self.next?;
        // GLOBAL is its own parent, so termination has to be explicit
        // rather than falling out of the walk.
        self.next = (!current.is_global()).then(|| current.parent());
        Some(current)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = match self.next {
            None => 0,
            Some(id) => id.depth() as usize + 1,
        };
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for AncestorPath {}

impl std::fmt::Display for SubnetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_global() {
            write!(f, "global")
        } else {
            let d = self.depth();
            for i in 0..d {
                if i > 0 {
                    write!(f, ".")?;
                }
                write!(f, "{}", self.level(i))?;
            }
            Ok(())
        }
    }
}

/// Inverse of [`std::fmt::Display`]: parses `"global"`
/// (case-insensitive) or a dotted decimal form like `"3.7.2"`
/// (each level a `u8`).
impl std::str::FromStr for SubnetId {
    type Err = super::SubnetError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.eq_ignore_ascii_case("global") {
            return Ok(Self::GLOBAL);
        }
        if trimmed.is_empty() {
            return Err(super::SubnetError::ParseFailed {
                input: s.to_string(),
                reason: "empty".into(),
            });
        }
        let parts: Vec<&str> = trimmed.split('.').collect();
        if parts.len() > MAX_DEPTH as usize {
            return Err(super::SubnetError::TooManyLevels {
                got: parts.len(),
                max: MAX_DEPTH,
            });
        }
        let mut levels: Vec<u8> = Vec::with_capacity(parts.len());
        for p in parts {
            match p.parse::<u8>() {
                Ok(level) => levels.push(level),
                Err(e) => {
                    return Err(super::SubnetError::ParseFailed {
                        input: s.to_string(),
                        reason: format!("level `{p}` not a u8: {e}"),
                    })
                }
            }
        }
        Self::try_new(&levels)
    }
}

/// Security-facing name for the compact topology coordinate
/// (SUBNET_AUTH_PLAN.md D1).
///
/// The subnet-auth surface (`SubnetRef`, `SubnetGrant`,
/// `VerifiedSubnetContext`) names this type `TopologySubnetId` to keep
/// the distinction audible at call sites: a `TopologySubnetId` is a
/// *path* inside one installation's hierarchy, and only an
/// authority-qualified `SubnetRef { authority, path }` is a security
/// target. Equal path bits under two authorities are unrelated.
///
/// Same type, same wire encoding — the alias changes no behavior.
pub type TopologySubnetId = SubnetId;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_global() {
        assert!(SubnetId::GLOBAL.is_global());
        assert_eq!(SubnetId::GLOBAL.depth(), 0);
        assert_eq!(SubnetId::GLOBAL.raw(), 0);
    }

    #[test]
    fn test_new() {
        let id = SubnetId::new(&[3, 7]);
        assert_eq!(id.level(0), 3);
        assert_eq!(id.level(1), 7);
        assert_eq!(id.level(2), 0);
        assert_eq!(id.level(3), 0);
        assert_eq!(id.depth(), 2);
        assert!(!id.is_global());
    }

    #[test]
    fn test_full_depth() {
        let id = SubnetId::new(&[1, 2, 3, 4]);
        assert_eq!(id.depth(), 4);
        assert_eq!(id.level(0), 1);
        assert_eq!(id.level(1), 2);
        assert_eq!(id.level(2), 3);
        assert_eq!(id.level(3), 4);
        assert_eq!(id.raw(), 0x01020304);
    }

    #[test]
    fn test_parent() {
        let id = SubnetId::new(&[3, 7, 2]);
        let parent = id.parent();
        assert_eq!(parent, SubnetId::new(&[3, 7]));

        let grandparent = parent.parent();
        assert_eq!(grandparent, SubnetId::new(&[3]));

        let root = grandparent.parent();
        assert_eq!(root, SubnetId::GLOBAL);

        assert_eq!(SubnetId::GLOBAL.parent(), SubnetId::GLOBAL);
    }

    #[test]
    fn test_is_ancestor_of() {
        let region = SubnetId::new(&[3]);
        let fleet = SubnetId::new(&[3, 7]);
        let vehicle = SubnetId::new(&[3, 7, 2]);
        let other_fleet = SubnetId::new(&[3, 8]);
        let other_region = SubnetId::new(&[4]);

        // Global is ancestor of everything
        assert!(SubnetId::GLOBAL.is_ancestor_of(region));
        assert!(SubnetId::GLOBAL.is_ancestor_of(vehicle));

        // Region is ancestor of its fleets and vehicles
        assert!(region.is_ancestor_of(fleet));
        assert!(region.is_ancestor_of(vehicle));

        // Fleet is ancestor of its vehicles
        assert!(fleet.is_ancestor_of(vehicle));

        // But not the other way
        assert!(!vehicle.is_ancestor_of(fleet));
        assert!(!fleet.is_ancestor_of(region));

        // Not ancestor of different branch
        assert!(!region.is_ancestor_of(other_region));
        assert!(!fleet.is_ancestor_of(other_fleet));

        // Self is ancestor of self
        assert!(fleet.is_ancestor_of(fleet));
    }

    /// The canonical scope-containment truth table from
    /// SUBNET_AUTH_PLAN.md D1, pinned before any security caller
    /// lands. Every row is stated explicitly rather than derived in a
    /// loop so a semantic change breaks a named, greppable line.
    #[test]
    fn ancestor_or_self_truth_table() {
        let a = SubnetId::new(&[3]);
        let a_b = SubnetId::new(&[3, 7]);
        let a_c = SubnetId::new(&[3, 8]);
        let a_b_c = SubnetId::new(&[3, 7, 2]);
        let zero = SubnetId::GLOBAL;

        // scope A contains target A
        assert!(a.is_ancestor_or_self_of(a));
        // scope A contains target A/B
        assert!(a.is_ancestor_or_self_of(a_b));
        // scope A/B does not contain target A
        assert!(!a_b.is_ancestor_or_self_of(a));
        // scope A/B does not contain target A/C
        assert!(!a_b.is_ancestor_or_self_of(a_c));
        // scope A/B contains target A/B/C
        assert!(a_b.is_ancestor_or_self_of(a_b_c));
        // scope 0 contains target 0
        assert!(zero.is_ancestor_or_self_of(zero));
        // scope 0 contains target A (authority-local root)
        assert!(zero.is_ancestor_or_self_of(a));
        assert!(zero.is_ancestor_or_self_of(a_b_c));
        // scope A does not contain target 0
        assert!(!a.is_ancestor_or_self_of(zero));
        assert!(!a_b_c.is_ancestor_or_self_of(zero));

        // `is_ancestor_of` is the same relation under the legacy name.
        assert_eq!(a.is_ancestor_of(a_b), a.is_ancestor_or_self_of(a_b));
        assert_eq!(a_b.is_ancestor_of(zero), a_b.is_ancestor_or_self_of(zero));
    }

    /// The ancestor chain is the candidate set for every scope
    /// question, so its length bound is what makes forwarding cost
    /// independent of how many grants a gateway holds.
    #[test]
    fn ancestor_path_is_deepest_first_and_bounded() {
        let full: Vec<SubnetId> = SubnetId::new(&[1, 2, 3, 4]).ancestor_path().collect();
        assert_eq!(
            full,
            vec![
                SubnetId::new(&[1, 2, 3, 4]),
                SubnetId::new(&[1, 2, 3]),
                SubnetId::new(&[1, 2]),
                SubnetId::new(&[1]),
                SubnetId::GLOBAL,
            ],
        );
        assert_eq!(full.len(), MAX_ANCESTOR_PATH);

        // GLOBAL is its own parent; the walk must still terminate.
        assert_eq!(
            SubnetId::GLOBAL.ancestor_path().collect::<Vec<_>>(),
            vec![SubnetId::GLOBAL],
        );

        // Every path is bounded, and `size_hint` matches what is
        // actually produced — a caller sizing a stack buffer from it
        // must not be lied to.
        for raw in [
            0x00000000u32,
            0x03000000,
            0x03070000,
            0x03070200,
            0x01020304,
        ] {
            let id = SubnetId::from_raw(raw);
            let path = id.ancestor_path();
            let predicted = path.len();
            let walked: Vec<SubnetId> = path.collect();
            assert_eq!(predicted, walked.len(), "size_hint must be exact for {id}");
            assert!(walked.len() <= MAX_ANCESTOR_PATH);
            assert_eq!(walked[0], id, "the path starts at self");
            assert_eq!(*walked.last().unwrap(), SubnetId::GLOBAL, "and ends global");
        }
    }

    /// Containment against two endpoints reduces to containment
    /// against one: a scope holds both iff it holds their common
    /// ancestor. Stated exhaustively over a small hierarchy so the
    /// reduction is pinned, not assumed.
    #[test]
    fn common_ancestor_is_the_meet_of_the_containment_order() {
        let cases = [
            (&[3u8, 7, 2][..], &[3u8, 7, 9][..], &[3u8, 7][..]),
            (&[3, 7, 2], &[3, 8, 2], &[3]),
            (&[3, 7], &[4, 7], &[]),
            (&[3, 7], &[3, 7], &[3, 7]),
            (&[3, 7], &[3], &[3]),
            (&[3], &[], &[]),
            (&[], &[], &[]),
        ];
        for (a, b, expected) in cases {
            let (a, b) = (SubnetId::new(a), SubnetId::new(b));
            let meet = SubnetId::new(expected);
            assert_eq!(a.common_ancestor(b), meet, "{a} ∧ {b}");
            assert_eq!(b.common_ancestor(a), meet, "commutative: {b} ∧ {a}");
        }

        // The property the index depends on: over every pair in a
        // small hierarchy, "contains both" and "contains the meet"
        // are the same predicate.
        let universe: Vec<SubnetId> = [
            &[][..],
            &[3][..],
            &[4][..],
            &[3, 7][..],
            &[3, 8][..],
            &[3, 7, 2][..],
            &[3, 7, 9][..],
        ]
        .iter()
        .map(|l| SubnetId::new(l))
        .collect();
        for &a in &universe {
            for &b in &universe {
                let meet = a.common_ancestor(b);
                for &scope in &universe {
                    let contains_both =
                        scope.is_ancestor_or_self_of(a) && scope.is_ancestor_or_self_of(b);
                    assert_eq!(
                        contains_both,
                        scope.is_ancestor_or_self_of(meet),
                        "scope {scope} vs {a}/{b} (meet {meet})",
                    );
                }
                // And the meet is always reachable by walking either
                // endpoint's chain, which is what the probe loop does.
                assert!(a.ancestor_path().any(|s| s == meet));
                assert!(b.ancestor_path().any(|s| s == meet));
            }
        }
    }

    #[test]
    fn test_is_sibling() {
        let fleet_a = SubnetId::new(&[3, 7]);
        let fleet_b = SubnetId::new(&[3, 8]);
        let fleet_c = SubnetId::new(&[4, 7]);
        let region = SubnetId::new(&[3]);

        assert!(fleet_a.is_sibling(fleet_b));
        assert!(!fleet_a.is_sibling(fleet_c)); // different region
        assert!(!fleet_a.is_sibling(fleet_a)); // self is not sibling
        assert!(!fleet_a.is_sibling(region)); // different depth
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", SubnetId::GLOBAL), "global");
        assert_eq!(format!("{}", SubnetId::new(&[3])), "3");
        assert_eq!(format!("{}", SubnetId::new(&[3, 7])), "3.7");
        assert_eq!(format!("{}", SubnetId::new(&[1, 2, 3, 4])), "1.2.3.4");
    }

    #[test]
    fn test_from_raw() {
        let id = SubnetId::from_raw(0x03070000);
        assert_eq!(id, SubnetId::new(&[3, 7]));
    }

    #[test]
    fn test_mask_for_depth() {
        assert_eq!(SubnetId::mask_for_depth(0), 0x00000000);
        assert_eq!(SubnetId::mask_for_depth(1), 0xFF000000);
        assert_eq!(SubnetId::mask_for_depth(2), 0xFFFF0000);
        assert_eq!(SubnetId::mask_for_depth(3), 0xFFFFFF00);
        assert_eq!(SubnetId::mask_for_depth(4), 0xFFFFFFFF);
    }

    /// Too many levels must surface as `Err(...)`, not
    /// panic. SubnetId values typically come from config / FFI /
    /// JSON; a malformed entry must not crash the daemon loader.
    #[test]
    fn try_new_rejects_too_many_levels() {
        use super::super::error::SubnetError;
        let err = SubnetId::try_new(&[1, 2, 3, 4, 5]).unwrap_err();
        assert!(
            matches!(err, SubnetError::TooManyLevels { got: 5, max: 4 }),
            "expected TooManyLevels{{got: 5, max: 4}}, got {:?}",
            err
        );
    }

    #[test]
    fn try_new_accepts_max_depth() {
        // Boundary: exactly 4 levels must succeed.
        let id = SubnetId::try_new(&[1, 2, 3, 4]).expect("4 levels must be accepted (boundary)");
        assert_eq!(id, SubnetId::new(&[1, 2, 3, 4]));
    }

    #[test]
    fn try_new_accepts_empty() {
        let id = SubnetId::try_new(&[]).expect("0 levels (GLOBAL) must be accepted");
        assert_eq!(id, SubnetId::GLOBAL);
    }

    #[test]
    fn from_str_round_trips_global_and_dotted_levels() {
        use std::str::FromStr;
        assert_eq!(SubnetId::from_str("global").unwrap(), SubnetId::GLOBAL);
        assert_eq!(SubnetId::from_str("GLOBAL").unwrap(), SubnetId::GLOBAL);
        assert_eq!(SubnetId::from_str("3").unwrap(), SubnetId::new(&[3]));
        assert_eq!(SubnetId::from_str("3.7").unwrap(), SubnetId::new(&[3, 7]));
        assert_eq!(
            SubnetId::from_str("1.2.3.4").unwrap(),
            SubnetId::new(&[1, 2, 3, 4])
        );
        // Display ↔ FromStr round-trip.
        let id = SubnetId::new(&[3, 7, 2]);
        assert_eq!(SubnetId::from_str(&id.to_string()).unwrap(), id);
    }

    #[test]
    fn from_str_rejects_garbage() {
        use super::super::error::SubnetError;
        use std::str::FromStr;
        assert!(matches!(
            SubnetId::from_str("").unwrap_err(),
            SubnetError::ParseFailed { .. }
        ));
        assert!(matches!(
            SubnetId::from_str("256").unwrap_err(),
            SubnetError::ParseFailed { .. }
        ));
        assert!(matches!(
            SubnetId::from_str("1.2.3.4.5").unwrap_err(),
            SubnetError::TooManyLevels { got: 5, max: 4 }
        ));
        assert!(matches!(
            SubnetId::from_str("not-a-number").unwrap_err(),
            SubnetError::ParseFailed { .. }
        ));
    }
}
