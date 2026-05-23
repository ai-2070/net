//! Hierarchical subnet identifier.
//!
//! Encodes a 4-level hierarchy (region/fleet/vehicle/subsystem) into a `u32`.
//! Each level gets 8 bits (256 values). Parent/child/sibling relationships
//! are resolved with bitwise operations at wire speed.
//!
//! ```text
//! subnet_id (u32):
//!   [level_0: 8 bits] [level_1: 8 bits] [level_2: 8 bits] [level_3: 8 bits]
//!    ^region (256)     ^fleet (256)       ^vehicle (256)     ^subsystem (256)
//! ```

/// Maximum number of hierarchy levels.
pub const MAX_DEPTH: u8 = 4;

/// Hierarchical subnet identifier.
///
/// Zero (`0x00000000`) means global / no subnet. Trailing zeros mean
/// "no sub-level specified" — `SubnetId::new(&[3, 7])` represents
/// region=3, fleet=7, with no vehicle or subsystem restriction.
///
/// `Ord` is derived on the inner `u32` representation. The order
/// has no semantic meaning for the hierarchy (it does NOT match
/// ancestor/descendant relationships); it exists purely as a
/// deterministic tiebreaker for callers that need a total order
/// over `SubnetId`s — e.g. `correlation.rs::analyze_subnet_correlation`
/// needs ties at the same depth to resolve consistently across runs
/// rather than depending on `HashMap` iteration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct SubnetId(u32);

impl SubnetId {
    /// Global / no subnet.
    pub const GLOBAL: Self = Self(0);

    /// Maximum hierarchy depth supported by the encoding — same
    /// value as the module-level [`MAX_DEPTH`] constant, exposed
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
    #[inline]
    pub fn is_ancestor_of(self, other: Self) -> bool {
        if self.is_global() {
            return true;
        }
        let d = self.depth();
        let mask = Self::mask_for_depth(d);
        (self.0 & mask) == (other.0 & mask)
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
}
