//! The DARK-BOUNDARY guards for the organization exact-provider sensing
//! composition bridge.
//!
//! These are source-text guards, deliberately. The composition witness in
//! `sensing_org_exact_seam.rs` proves the seams work; these prove the plumbing
//! that lets a witness reach them cannot become a production surface:
//!
//! * every `pub` item in the bridge file carries the fixtures gate, is hidden
//!   from docs, and says in words that it is unsupported test plumbing;
//! * the rule is by MODULE BOUNDARY, not a name list: the guard iterates the
//!   bridge file's own declarations, so an eighth, differently named bridge is
//!   covered the moment it is declared;
//! * no bridge identifier appears in `mesh.rs`, so the module cannot quietly
//!   become a re-export of private node state;
//! * the external fixtures-off probe names exactly the bridge's declarations -
//!   neither fewer (which would shrink the darkness claim) nor more (which
//!   would not compile);
//! * the bridge module is the ONLY declaration the slice adds under `src/`
//!   that is `pub` without a fixtures gate on its module.
//!
//! Run: `cargo nextest run --features "cortex tool fixtures" --test
//! sensing_org_exact_guards`
#![cfg(feature = "net")]

use std::collections::BTreeSet;
use std::path::PathBuf;

/// The exact sentence every fixtures-only bridge must carry, byte for byte.
/// The same constant the in-crate evaluator inventory uses, so the two cannot
/// drift into different vocabularies.
const FIXTURES_ONLY: &str = "Unstable fixtures-only test bridge; not supported core API.";

/// The cfg gate a fixtures-only bridge must keep, byte for byte.
const FIXTURES_CFG: &str = "#[cfg(any(test, feature = \"fixtures\"))]";

const BRIDGE_PATH: &str = "src/adapter/net/org_exact_sensing_bridge.rs";
const MANIFEST_PATH: &str = "guards/fixtures_off_probe/MANIFEST";
const PROBE_MAIN: &str = "guards/fixtures_off_probe/src/main.rs";

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Read a repository file with line endings normalized, so a CRLF checkout
/// cannot change what these guards see.
fn read(relative: &str) -> String {
    let path = crate_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("{} is required by this guard: {err}", path.display()))
        .replace("\r\n", "\n")
}

/// Every `pub fn` / `pub struct` / `pub enum` declaration in the bridge file,
/// as the declaration NAME. Derived from the source, never from a list.
fn bridge_declarations(source: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for line in source.lines() {
        let line = line.trim();
        for prefix in [
            "pub fn ",
            "pub struct ",
            "pub enum ",
            "pub const ",
            "pub type ",
        ] {
            if let Some(rest) = line.strip_prefix(prefix) {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                assert!(
                    !name.is_empty(),
                    "could not read a declaration name from {line:?}"
                );
                found.insert(name);
            }
        }
    }
    found
}

/// The contiguous doc/attribute block immediately above a declaration - the
/// same shape the in-crate inventory guard uses.
fn attribute_block(source: &str, declaration: &str) -> Option<String> {
    let before = source.split(declaration).next()?;
    if before.len() == source.len() {
        return None;
    }
    let block: Vec<&str> = before
        .lines()
        .rev()
        .map(str::trim)
        .skip_while(|line| line.is_empty())
        .take_while(|line| line.starts_with("///") || line.starts_with("#["))
        .collect();
    Some(block.join("\n"))
}

/// Every `pub` item in the bridge is gated, hidden and says so in words.
#[test]
fn every_bridge_declaration_is_gated_hidden_and_marked_unstable() {
    let bridge = read(BRIDGE_PATH);
    let declarations = bridge_declarations(&bridge);
    assert!(
        !declarations.is_empty(),
        "the bridge file declares nothing - deleting its contents must fail this \
         guard, not satisfy it"
    );

    for name in &declarations {
        // Match the declaration exactly, so a prefix cannot borrow another
        // item's attribute block.
        let needle = bridge
            .lines()
            .find(|line| {
                let line = line.trim();
                [
                    "pub fn ",
                    "pub struct ",
                    "pub enum ",
                    "pub const ",
                    "pub type ",
                ]
                .iter()
                .any(|prefix| {
                    line.strip_prefix(prefix).is_some_and(|rest| {
                        rest.starts_with(name.as_str())
                            && !rest[name.len()..]
                                .starts_with(|c: char| c.is_alphanumeric() || c == '_')
                    })
                })
            })
            .map(str::trim)
            .unwrap_or_else(|| panic!("{name} vanished between two reads of the same source"));
        let block = attribute_block(&bridge, needle)
            .unwrap_or_else(|| panic!("no attribute block above `{needle}`"));
        assert!(
            block.contains(FIXTURES_CFG),
            "`{name}` must carry {FIXTURES_CFG} - an ungated item in this file would \
             ship in production builds. Block:\n{block}"
        );
        assert!(
            block.contains("#[doc(hidden)]"),
            "`{name}` must be #[doc(hidden)] - this is test plumbing, not API. \
             Block:\n{block}"
        );
        assert!(
            block.contains(FIXTURES_ONLY),
            "`{name}` must carry the exact sentence {FIXTURES_ONLY:?}, so vague prose \
             cannot satisfy this guard. Block:\n{block}"
        );
    }
}

/// The module declaration itself is fixtures-gated and hidden, and NO bridge
/// identifier appears in `mesh.rs`.
#[test]
fn the_bridge_is_declared_once_and_is_absent_from_mesh() {
    let module = read("src/adapter/net/mod.rs");
    let declaration = "pub mod org_exact_sensing_bridge;";
    assert_eq!(
        module.matches(declaration).count(),
        1,
        "the bridge module must be declared exactly once"
    );
    let block = attribute_block(&module, declaration).expect("attribute block above the module");
    assert!(
        block.contains(FIXTURES_CFG),
        "the module declaration must carry {FIXTURES_CFG}. Block:\n{block}"
    );
    assert!(
        block.contains("#[doc(hidden)]"),
        "the module declaration must be #[doc(hidden)]. Block:\n{block}"
    );

    // Containment: the bridge is a facade over `pub(crate)` seams, so nothing
    // in `mesh.rs` may name it. A bridge declared THERE instead would be
    // reachable only through a re-export, which is exactly the production
    // exposure this keeps out.
    let mesh = read("src/adapter/net/mesh.rs");
    assert!(
        mesh.len() > 100_000,
        "mesh.rs read back as {} bytes - a truncated read would make the check \
         below vacuous",
        mesh.len()
    );
    assert!(
        !mesh.contains("org_exact_sensing_bridge"),
        "mesh.rs names the bridge module; it must stay a separate, fixtures-gated \
         module with no production re-export"
    );
    // And none of the bridge's declarations may be DECLARED there either - a
    // same-named `pub(crate)` seam is fine and expected (that is what the
    // bridge is a facade over), a `pub` twin in mesh.rs is not.
    for name in bridge_declarations(&read(BRIDGE_PATH)) {
        let twin = format!("pub fn {name}(");
        assert!(
            !mesh.contains(&twin),
            "mesh.rs declares `{twin}`; the bridge's surface must exist in exactly \
             one place, behind the fixtures gate"
        );
    }
}

/// The external fixtures-off probe covers exactly the bridge's declarations.
#[test]
fn the_fixtures_off_probe_covers_every_bridge_declaration() {
    let bridge = read(BRIDGE_PATH);
    let declared = bridge_declarations(&bridge);
    let manifest: BTreeSet<String> = read(MANIFEST_PATH)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    assert_eq!(
        manifest, declared,
        "the fixtures-off probe MANIFEST must equal the bridge's declaration set: \
         fewer names would shrink the darkness claim, more would not compile"
    );

    // And the probe source really names them, so the MANIFEST cannot be a
    // detached list.
    let probe = read(PROBE_MAIN);
    for name in &declared {
        assert!(
            probe.contains(name.as_str()),
            "the probe must name `{name}`, or the negative compile proves nothing \
             about it"
        );
    }

    // The probe is its own workspace and does NOT enable the feature by
    // default - both are what make the negative leg meaningful.
    let probe_manifest = read("guards/fixtures_off_probe/Cargo.toml");
    assert!(
        probe_manifest.contains("[workspace]"),
        "the probe must be its own workspace root, or the parent feature graph \
         could turn `fixtures` on for it"
    );
    assert!(
        !probe_manifest.contains("\"net/fixtures\"]\n") || probe_manifest.contains("[features]"),
        "the probe may forward the core fixtures gate ONLY through its own \
         optional feature, never through its dependency's default set"
    );
    let dependency_block = probe_manifest
        .split("[dependencies.net]")
        .nth(1)
        .expect("the probe must depend on the core crate")
        .split("\n[")
        .next()
        .expect("dependency block");
    assert!(
        !dependency_block.contains("fixtures"),
        "the probe's core dependency must NOT enable `fixtures`; the negative leg \
         is the whole point. Block:\n{dependency_block}"
    );
}
