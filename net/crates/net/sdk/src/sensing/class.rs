//! The Rust-owned classification of [`SensingError`], the one every language
//! binding forwards (SENSING_OLB_COMPLETION_PLAN C5).
//!
//! # Why classification lives here and not in each binding
//!
//! [`SensingError`] is `#[non_exhaustive]`. Inside this crate a `match` on it
//! can be exhaustive; in every other crate (the napi and pyo3 bindings, the C
//! shim) it cannot, and each must carry a `_ =>` arm. A binding that derived
//! its own codes would therefore classify a newly added variant through that
//! wildcard, silently, and no test in this crate could notice. So the
//! bindings do not match variants at all: they surface [`SensingError::kind`],
//! [`SensingError::is_terminal`] and the message, or [`SensingError::to_wire`]
//! whole. The exhaustive `match`es below are the guard. Adding a variant does
//! not compile until it is classified here.
//!
//! # The wire form
//!
//! ```text
//! sensing:<kind>: <message>
//! ```
//!
//! - `<kind>` is a stable snake_case token: what a binding branches on.
//! - `<message>` is the human-facing remedy, the variant's `Display` text. It
//!   is never parsed for meaning.
//!
//! The shared fixture `tests/cross_lang_sensing/error_vectors.json` lists one
//! canonical instance of every variant with its kind, terminal flag, wire
//! string and message. It is generated from this module (see the drift guard
//! in this module's tests), and every binding's suite asserts its surfaced
//! errors against it.

use super::SensingError;

/// The prefix every sensing wire string carries.
pub const ERR_SENSING_PREFIX: &str = "sensing:";

impl SensingError {
    /// The stable classification token, for a caller (or a binding) to branch
    /// on. Snake-case, never renamed once shipped.
    ///
    /// Exhaustive by construction: a new variant fails to compile until it is
    /// given a kind here.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::IncarnationRequired => "incarnation_required",
            Self::AlreadyProviding { .. } => "already_providing",
            Self::DurableIdentityRequired => "durable_identity_required",
            Self::RegistrationIdentityExhausted => "registration_identity_exhausted",
            Self::EmptyCapability => "empty_capability",
            Self::UnsatisfiableBudget => "unsatisfiable_budget",
            Self::NoOrganizationAuthority => "no_organization_authority",
            Self::ObserverNotQualified => "observer_not_qualified",
            Self::UnsatisfiableStartBound => "unsatisfiable_start_bound",
            Self::WatchesAtCapacity => "watches_at_capacity",
            Self::ObservationIdentityUnavailable => "observation_identity_unavailable",
            Self::WatchClosed => "watch_closed",
            Self::NoSensingLeader => "no_sensing_leader",
            Self::UnsatisfiableCoalescing => "unsatisfiable_coalescing",
            Self::CapabilityTooLong => "capability_too_long",
        }
    }

    /// Whether this refusal is a **permanent state of the node or handle**
    /// that refused. If it is, nothing the caller later does through that
    /// node or handle makes the same operation succeed: the mesh must be
    /// rebuilt, or a new handle opened. If it is not, the operation can
    /// succeed after the caller acts (fix an argument, close the incumbent,
    /// install authority) or after node state moves (a membership renews, a
    /// rotation settles).
    ///
    /// A binding uses this to decide between "fix and retry" and "give up on
    /// this object". It is not a retry policy: a non-terminal refusal still
    /// needs the action its message names.
    ///
    /// Exhaustive by construction, like [`Self::kind`].
    pub fn is_terminal(&self) -> bool {
        match self {
            // Builder-time facts of THIS mesh: only a rebuilt mesh changes them.
            Self::Disabled | Self::IncarnationRequired | Self::DurableIdentityRequired => true,
            // Identity spaces that never refill on this node.
            Self::RegistrationIdentityExhausted | Self::ObservationIdentityUnavailable => true,
            // A closed watch is inert forever; a new watch is a new handle.
            Self::WatchClosed => true,
            // Fixable by the caller's own action.
            Self::AlreadyProviding { .. }
            | Self::EmptyCapability
            | Self::UnsatisfiableBudget
            | Self::UnsatisfiableStartBound
            | Self::UnsatisfiableCoalescing
            | Self::CapabilityTooLong => false,
            // Node state that can move back: authority installed or rotated,
            // membership renewed, a view that settles.
            Self::NoOrganizationAuthority | Self::ObserverNotQualified => false,
            // A member able to lead the capability can come back.
            Self::NoSensingLeader => false,
            // Per observation ROOT, and every watch mints its own root: a new
            // watch is not refused by this one's exhaustion.
            Self::WatchesAtCapacity => false,
        }
    }

    /// The `sensing:<kind>: <message>` wire string a binding surfaces.
    pub fn to_wire(&self) -> String {
        format!("{ERR_SENSING_PREFIX}{}: {self}", self.kind())
    }
}

/// The reference parse every binding mirrors: the kind token of a sensing wire
/// string, or `None` when `wire` is not one.
///
/// Only the kind is recovered. The message is human-facing and never parsed
/// for meaning.
pub fn parse_sensing_wire(wire: &str) -> Option<&str> {
    let rest = wire.strip_prefix(ERR_SENSING_PREFIX)?;
    let (kind, _message) = rest.split_once(": ")?;
    let portable = !kind.is_empty()
        && kind
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    portable.then_some(kind)
}

/// The Rust-owned classification of one sensing wire string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SensingWireClass {
    /// The stable kind token ([`SensingError::kind`]).
    pub kind: &'static str,
    /// Whether the refusal is terminal ([`SensingError::is_terminal`]).
    pub terminal: bool,
}

/// Classify a sensing wire string the way Rust classifies the error that
/// produced it, or `None` when `wire` is not a sensing wire string or names a
/// kind this build does not know.
///
/// This is what a binding calls instead of keeping its own kind→terminal
/// table: the answer comes from [`SensingError::is_terminal`], so a binding
/// can never disagree with Rust about it.
pub fn classify_sensing_wire(wire: &str) -> Option<SensingWireClass> {
    let kind = parse_sensing_wire(wire)?;
    canonical_instances()
        .into_iter()
        .find(|error| error.kind() == kind)
        .map(|error| SensingWireClass {
            kind: error.kind(),
            terminal: error.is_terminal(),
        })
}
/// One canonical instance of every variant, in declaration order: the
/// fixture's population.
///
/// This list is checked for completeness against the enum's own source by
/// `the_canonical_population_covers_every_declared_variant`, so a new variant
/// absent from it fails a test even though the list itself is not
/// compiler-checked.
pub(crate) fn canonical_instances() -> Vec<SensingError> {
    vec![
        SensingError::Disabled,
        SensingError::IncarnationRequired,
        SensingError::AlreadyProviding {
            capability: "fixture.capability".to_owned(),
        },
        SensingError::DurableIdentityRequired,
        SensingError::RegistrationIdentityExhausted,
        SensingError::EmptyCapability,
        SensingError::UnsatisfiableBudget,
        SensingError::NoOrganizationAuthority,
        SensingError::ObserverNotQualified,
        SensingError::UnsatisfiableStartBound,
        SensingError::WatchesAtCapacity,
        SensingError::ObservationIdentityUnavailable,
        SensingError::WatchClosed,
        SensingError::NoSensingLeader,
        SensingError::UnsatisfiableCoalescing,
        SensingError::CapabilityTooLong,
    ]
}

/// Render the fixture from the classifier. The checked-in file must equal
/// this byte for byte.
#[cfg(test)]
pub(crate) fn render_error_vectors() -> String {
    let vectors: Vec<serde_json::Value> = canonical_instances()
        .iter()
        .map(|error| {
            serde_json::json!({
                "kind": error.kind(),
                "terminal": error.is_terminal(),
                "wire": error.to_wire(),
                "message": error.to_string(),
            })
        })
        .collect();
    let doc = serde_json::json!({
        "description": "The canonical `sensing:` error vocabulary. Rust owns the classification (net_sdk::sensing::SensingError::kind / is_terminal / to_wire); every language binding surfaces it verbatim and asserts against these vectors. A binding MUST recover `kind` from `wire` (prefix `sensing:`, the kind runs to the first `: `), MUST report `terminal` exactly as listed, and MUST NOT derive either from the message. GENERATED - do not hand-edit; regenerate with: NET_REGENERATE_SENSING_FIXTURE=1 cargo test -p net-mesh-sdk --features net --lib sensing::class::tests::the_checked_in_fixture_matches_the_classifier",
        "version": 1,
        "prefix": ERR_SENSING_PREFIX,
        "vectors": vectors,
    });
    let mut text = serde_json::to_string_pretty(&doc).expect("fixture serializes");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("tests")
            .join("cross_lang_sensing")
            .join("error_vectors.json")
    }

    /// The drift guard. A new or reclassified variant changes the render,
    /// which fails this until the fixture is regenerated. The regenerated
    /// fixture then fails every binding suite that has not caught up.
    #[test]
    fn the_checked_in_fixture_matches_the_classifier() {
        let path = fixture_path();
        if std::env::var_os("NET_REGENERATE_SENSING_FIXTURE").is_some() {
            std::fs::create_dir_all(path.parent().expect("fixture dir")).expect("mkdir");
            std::fs::write(&path, render_error_vectors()).expect("write fixture");
        }
        let checked_in = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert_eq!(
            checked_in,
            render_error_vectors(),
            "error_vectors.json is stale - regenerate with:\n  \
             NET_REGENERATE_SENSING_FIXTURE=1 cargo test -p net-mesh-sdk --features net --lib \
             sensing::class::tests::the_checked_in_fixture_matches_the_classifier"
        );
    }

    /// The canonical population names every variant the enum declares. Read
    /// from `sensing.rs`'s own source, so a variant added there and forgotten
    /// here fails this test, not a binding suite three languages away.
    #[test]
    fn the_canonical_population_covers_every_declared_variant() {
        let source = include_str!("../sensing.rs");
        let body = source
            .split("pub enum SensingError {")
            .nth(1)
            .expect("enum declaration present");
        let body = &body[..body.find("\n}\n").expect("enum body closes")];
        let declared: Vec<&str> = body
            .lines()
            .filter_map(|line| {
                let line = line.strip_prefix("    ")?;
                let first = line.chars().next()?;
                if !first.is_ascii_uppercase() {
                    return None;
                }
                let name_end = line
                    .find(|c: char| !c.is_ascii_alphanumeric())
                    .unwrap_or(line.len());
                Some(&line[..name_end])
            })
            .collect();
        assert!(
            declared.len() >= 13,
            "the source scan found {} variants; it is not reading the enum",
            declared.len()
        );

        let canonical: Vec<String> = canonical_instances()
            .iter()
            .map(|e| {
                let debug = format!("{e:?}");
                debug
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            canonical, declared,
            "canonical_instances() must list every SensingError variant, in declaration order"
        );
    }

    /// Kinds are distinct, portable tokens, and every wire string parses back
    /// to its own kind through the reference parser.
    #[test]
    fn every_vector_parses_back_to_its_kind_and_kinds_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for error in canonical_instances() {
            let kind = error.kind();
            assert!(seen.insert(kind), "kind `{kind}` is used twice");
            let wire = error.to_wire();
            assert_eq!(parse_sensing_wire(&wire), Some(kind), "parse of {wire}");
            assert!(wire.ends_with(&error.to_string()), "message rides verbatim");
        }
    }

    /// The reference parser refuses what is not a sensing wire string rather
    /// than inventing a kind for it.
    #[test]
    fn the_parser_refuses_non_sensing_strings() {
        for wire in [
            "",
            "sensing:",
            "sensing:disabled",
            "sensing:: message",
            "sensing:Not_Portable: message",
            "org:credentials:node_authority_required: message",
            "capability sensing is disabled on this mesh",
        ] {
            assert_eq!(parse_sensing_wire(wire), None, "{wire:?}");
        }
    }

    /// Every message names its remedy, or says plainly that the state is
    /// terminal. The needle table is an exhaustive `match`, so a new variant
    /// does not compile here either until someone states what its message
    /// must tell the caller. This replaces the earlier hand-kept sample of
    /// five provider messages (consumer-SDK review §3).
    #[test]
    fn every_message_names_its_remedy() {
        fn needle(error: &SensingError) -> &'static str {
            match error {
                SensingError::Disabled => "MeshBuilder::enable_sensing",
                SensingError::IncarnationRequired => "next_incarnation",
                SensingError::AlreadyProviding { .. } => "provide_replacing",
                SensingError::DurableIdentityRequired => "MeshBuilder::identity",
                SensingError::RegistrationIdentityExhausted => "exhausted",
                SensingError::EmptyCapability => "capability name",
                SensingError::UnsatisfiableBudget => "SensingQuery::within",
                SensingError::NoOrganizationAuthority => "install it before watching",
                SensingError::ObserverNotQualified => "snapshots resume",
                SensingError::UnsatisfiableStartBound => "SensingQuery::start_within",
                SensingError::WatchesAtCapacity => "exhausted",
                SensingError::ObservationIdentityUnavailable => "no further capability watch",
                SensingError::WatchClosed => "SensingClient::watch",
                SensingError::NoSensingLeader => "open an exact watch",
                SensingError::UnsatisfiableCoalescing => "Coalesced::Top(k)",
                SensingError::CapabilityTooLong => "1024 bytes",
            }
        }
        for error in canonical_instances() {
            let message = error.to_string();
            assert!(
                message.contains(needle(&error)),
                "`{}` no longer names its remedy `{}`: {message}",
                error.kind(),
                needle(&error),
            );
        }
        assert!(SensingError::AlreadyProviding {
            capability: "gpu.infer".into()
        }
        .to_string()
        .contains("gpu.infer"));
    }
    /// The binding-facing classifier answers every fixture vector with the
    /// fixture's own kind and terminal flag, and refuses what is not a known
    /// sensing wire string. A binding that surfaces this answer cannot drift
    /// from Rust's `is_terminal`.
    #[test]
    fn classify_sensing_wire_reproduces_every_fixture_vector() {
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(fixture_path()).expect("fixture"))
                .expect("json");
        let vectors = doc["vectors"].as_array().expect("vectors");
        assert_eq!(vectors.len(), canonical_instances().len());
        for v in vectors {
            let wire = v["wire"].as_str().expect("wire");
            let class = classify_sensing_wire(wire).expect("a fixture wire classifies");
            assert_eq!(class.kind, v["kind"].as_str().unwrap(), "{wire}");
            assert_eq!(class.terminal, v["terminal"].as_bool().unwrap(), "{wire}");
        }
        assert_eq!(classify_sensing_wire("sensing:not_a_kind: x"), None);
        assert_eq!(classify_sensing_wire("org:rpc:timeout: x"), None);
    }
}
