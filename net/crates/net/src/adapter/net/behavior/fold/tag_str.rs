//! [`TagStr`]: a capability tag string whose bytes a fold can share.
//!
//! CAPABILITY_FOLD_SCALE_PLAN.md Slice 6 (Track B1). A fold of 1M
//! capability entries holds ~31 tags per entry, and most tag strings
//! repeat across the fleet. `CapabilityMembership::tags` stores
//! `TagStr` handles: the capability fold's tag dictionary
//! (`TagDictionary`) swaps each admitted tag for its one canonical
//! allocation, so the fold keeps each distinct tag's bytes once.
//!
//! `TagStr` is a string to everything outside the fold:
//!
//! - **Wire and signature.** It serializes and deserializes exactly as
//!   `String` does, so the postcard encoding, the signing transcript and
//!   the snapshot form are byte-identical to the `Vec<String>` it
//!   replaced. The golden test `tag_str_encoding_matches_the_string_oracle`
//!   pins that.
//! - **Readers.** It derefs to `str`. `Eq`, `Ord` and `Hash` are by
//!   content, and `Borrow<str>` makes a `TagStr`-keyed map answer a
//!   `&str` lookup. Two handles with the same text are equal whether or
//!   not they share an allocation.
//! - **Immutable.** No method mutates the text.
//!
//! Sharing is an allocation detail, not ownership. A caller that clones
//! a payload out of the fold (a query result, a snapshot) holds handles
//! that keep their bytes alive after the fold drops the tag. The fold's
//! tag budget counts only the fold's own canonical storage.

use std::borrow::Borrow;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An immutable, cheaply cloned capability tag string. See the module
/// doc.
#[derive(Clone)]
pub struct TagStr(Arc<str>);

impl TagStr {
    /// The tag's text.
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `self` and `other` share one allocation. Content equality
    /// is `==`; this is for tests and accounting.
    pub fn shares_allocation_with(&self, other: &TagStr) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Deref for TagStr {
    type Target = str;

    #[inline]
    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for TagStr {
    #[inline]
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for TagStr {
    #[inline]
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl PartialEq for TagStr {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0
    }
}

impl Eq for TagStr {}

impl PartialEq<str> for TagStr {
    #[inline]
    fn eq(&self, other: &str) -> bool {
        &*self.0 == other
    }
}

impl PartialEq<&str> for TagStr {
    #[inline]
    fn eq(&self, other: &&str) -> bool {
        &*self.0 == *other
    }
}

impl PartialEq<String> for TagStr {
    #[inline]
    fn eq(&self, other: &String) -> bool {
        &*self.0 == other.as_str()
    }
}

impl PartialEq<TagStr> for str {
    #[inline]
    fn eq(&self, other: &TagStr) -> bool {
        self == &*other.0
    }
}

impl PartialEq<TagStr> for &str {
    #[inline]
    fn eq(&self, other: &TagStr) -> bool {
        *self == &*other.0
    }
}

impl PartialEq<TagStr> for String {
    #[inline]
    fn eq(&self, other: &TagStr) -> bool {
        self.as_str() == &*other.0
    }
}

impl Hash for TagStr {
    /// Hashes as the `str` it holds, which `Borrow<str>` requires.
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl PartialOrd for TagStr {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TagStr {
    #[inline]
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl fmt::Debug for TagStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for TagStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}

impl From<String> for TagStr {
    #[inline]
    fn from(s: String) -> Self {
        Self(Arc::from(s))
    }
}

impl From<&str> for TagStr {
    #[inline]
    fn from(s: &str) -> Self {
        Self(Arc::from(s))
    }
}

impl From<&String> for TagStr {
    #[inline]
    fn from(s: &String) -> Self {
        Self(Arc::from(s.as_str()))
    }
}

impl From<TagStr> for String {
    #[inline]
    fn from(t: TagStr) -> Self {
        t.as_str().to_owned()
    }
}

impl Serialize for TagStr {
    /// Exactly `String`'s serialization: the wire and signing form of a
    /// tag does not depend on how the receiver stores it.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TagStr {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equality_hash_and_borrow_are_by_content() {
        use std::collections::HashMap;
        let a = TagStr::from("hardware.gpu");
        let b = TagStr::from(String::from("hardware.gpu"));
        assert!(!a.shares_allocation_with(&b));
        assert_eq!(a, b);
        assert_eq!(a, "hardware.gpu");
        let mut map: HashMap<TagStr, u32> = HashMap::new();
        map.insert(a.clone(), 1);
        assert_eq!(map.get("hardware.gpu"), Some(&1), "lookup by &str");
        assert_eq!(map.get(&b), Some(&1), "lookup by an equal handle");
        assert!(a.clone().shares_allocation_with(&a));
    }

    #[test]
    fn serde_form_is_string_form() {
        let tags: Vec<TagStr> = vec!["a".into(), "héllo".into(), "".into()];
        let strings: Vec<String> = vec!["a".into(), "héllo".into(), "".into()];
        let a = postcard::to_allocvec(&tags).expect("encode tags");
        let b = postcard::to_allocvec(&strings).expect("encode strings");
        assert_eq!(a, b);
        let back: Vec<TagStr> = postcard::from_bytes(&b).expect("decode");
        assert_eq!(back, tags);
        assert_eq!(
            serde_json::to_string(&tags).expect("json"),
            serde_json::to_string(&strings).expect("json")
        );
    }
}
