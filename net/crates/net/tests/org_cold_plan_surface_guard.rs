//! CI guard: the OLB cold-plan bridge is not supported product API.
//!
//! OLB-2B.3d-pre needs a coherent authority/discovery capture produced by
//! `MeshNode` in this crate and consumed by the cold plan in the separate
//! `net-mesh-sdk` crate. Rust has no "visible to one other crate" visibility, so
//! the three types the SDK names and the three node methods it calls have to be
//! `pub`. That is a workspace-internal bridge, and the frozen 2B.3d-pre boundary
//! forbids premature product surface — so being `pub` must not make it LOOK
//! supported (independent review HOLD-5, 2026-08-29).
//!
//! This guard is the enforcement, in the crate-source style the repo already uses
//! for invariants a type system cannot state (`doc_link_guard`,
//! `capability_schema_doc_guard`, the MCP `dependency_boundary` test):
//!
//!   1. every `pub` bridge item carries `#[doc(hidden)]`, which is what keeps it
//!      out of the generated public documentation;
//!   2. the two items the SDK does NOT need are not `pub` at all;
//!   3. nothing re-exports any bridge name — not from this crate's root, and not
//!      into the SDK's public surface inventory;
//!   4. the module's own docs say plainly that it is unstable, workspace-internal
//!      and not semver-covered.
//!
//! Run: `cargo test --test org_cold_plan_surface_guard`.

use std::path::{Path, PathBuf};

/// The crate root (`net/crates/net`), which is CWD for `cargo test`.
fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    // Normalized to LF: this repo is checked out with CRLF on Windows, and every
    // structural assertion below anchors on line shapes.
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .replace("\r\n", "\n")
}

/// The line that declares `item`, plus the attribute lines immediately above it.
fn declaration_with_attributes<'a>(src: &'a str, item: &str) -> (usize, Vec<&'a str>) {
    let lines: Vec<&str> = src.lines().collect();
    let index = lines
        .iter()
        .position(|line| line.trim_start().starts_with(item))
        .unwrap_or_else(|| panic!("declaration not found: {item}"));
    let mut attrs = Vec::new();
    for line in lines[..index].iter().rev() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#[") || trimmed.starts_with("///") || trimmed.starts_with("//") {
            attrs.push(trimmed);
            continue;
        }
        break;
    }
    (index, attrs)
}

fn assert_doc_hidden(src: &str, path: &str, item: &str) {
    let (_, attrs) = declaration_with_attributes(src, item);
    assert!(
        attrs.contains(&"#[doc(hidden)]"),
        "{path}: `{item}` is part of the OLB cold-plan bridge and must carry \
         #[doc(hidden)] — it is workspace-internal, not product API"
    );
}

/// Every bridge name, for the re-export scan.
const BRIDGE_NAMES: &[&str] = &[
    "OrgColdRefusal",
    "OrgColdAuthority",
    "OrgColdDiscovery",
    "OrgColdAuthorityStamp",
    "OrgColdGrantAuthority",
    "org_cold_discovery",
    "org_cold_authority",
    "org_cold_authority_is_current",
];

#[test]
fn the_cold_plan_bridge_is_doc_hidden() {
    let bridge = crate_root().join("src/adapter/net/behavior/org_cold_plan.rs");
    let src = read(&bridge);
    let name = "src/adapter/net/behavior/org_cold_plan.rs";
    for item in [
        "pub enum OrgColdRefusal {",
        "pub struct OrgColdAuthority {",
        "pub struct OrgColdDiscovery {",
    ] {
        assert_doc_hidden(&src, name, item);
    }

    let module = crate_root().join("src/adapter/net/behavior/mod.rs");
    assert_doc_hidden(
        &read(&module),
        "src/adapter/net/behavior/mod.rs",
        "pub mod org_cold_plan;",
    );

    let mesh = crate_root().join("src/adapter/net/mesh.rs");
    let mesh_src = read(&mesh);
    for item in [
        "pub fn org_cold_discovery(",
        "pub fn org_cold_authority(",
        "pub fn org_cold_authority_is_current(",
    ] {
        assert_doc_hidden(&mesh_src, "src/adapter/net/mesh.rs", item);
    }
}

#[test]
fn the_bridge_exposes_nothing_the_sdk_does_not_need() {
    let src = read(&crate_root().join("src/adapter/net/behavior/org_cold_plan.rs"));
    // The SDK never inspects the stamp or a grant pin — it hands the capture back
    // to the node's comparison — so neither type may cross the crate boundary.
    for internal in ["OrgColdAuthorityStamp", "OrgColdGrantAuthority"] {
        assert!(
            src.contains(&format!("pub(crate) struct {internal} {{")),
            "`{internal}` must stay crate-internal: the SDK does not name it, so \
             exporting it would widen the bridge past its consumer"
        );
        assert!(
            !src.contains(&format!("\npub struct {internal} {{")),
            "`{internal}` must not be exported"
        );
    }
    // Exactly the accessors the SDK calls, and no more.
    let public_fns: Vec<&str> = src
        .lines()
        .map(str::trim_start)
        .filter(|line| line.starts_with("pub fn "))
        .collect();
    let expected = [
        "pub fn now_secs(&self) -> u64 {",
        "pub fn authority_org(&self) -> OrgId {",
        "pub fn now_secs(&self) -> u64 {",
        "pub fn authority(&self) -> &OrgColdAuthority {",
        "pub fn authority_org(&self) -> OrgId {",
        "pub fn owner_providers(&self) -> &[PrivateCapabilityProvider] {",
        "pub fn granted_providers(&self, grant_id: &[u8; 32]) -> &[PrivateCapabilityProvider] {",
    ];
    assert_eq!(
        public_fns.len(),
        expected.len(),
        "the bridge's public accessor count changed ({:?}); every one of them is \
         API the SDK must actually call, so widening it needs its own review",
        public_fns
    );
    for accessor in &expected {
        assert!(
            public_fns.contains(accessor),
            "expected bridge accessor missing: {accessor}"
        );
    }
}

#[test]
fn nothing_re_exports_the_bridge() {
    let mut roots = vec![crate_root().join("src")];
    roots.push(crate_root().join("sdk/src"));
    let mut files = Vec::new();
    for root in &roots {
        walk(root, &mut files);
    }
    assert!(
        files.len() > 100,
        "the scan found only {} files; a broken walk would make this guard \
         vacuous",
        files.len()
    );
    for file in &files {
        let src = read(file);
        for (number, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            let is_reexport =
                trimmed.starts_with("pub use ") || trimmed.starts_with("pub(crate) use ");
            if !is_reexport {
                continue;
            }
            for name in BRIDGE_NAMES {
                assert!(
                    !trimmed.contains(name),
                    "{}:{}: re-exports the OLB cold-plan bridge name `{name}`. The \
                     bridge is reachable only through its own hidden module; a \
                     re-export would put it into a supported surface inventory.",
                    file.display(),
                    number + 1
                );
            }
        }
    }
}

#[test]
fn the_bridge_declares_itself_unstable() {
    let src = read(&crate_root().join("src/adapter/net/behavior/org_cold_plan.rs"));
    for phrase in [
        "unstable workspace-internal OLB implementation bridge",
        "not application API",
        "not covered by semver",
    ] {
        assert!(
            src.to_lowercase().contains(&phrase.to_lowercase()),
            "the bridge module must state plainly that it is {phrase}"
        );
    }
}

/// The body of `MeshNode::capture_cold`, from its signature to the line that
/// closes it at the same indentation.
fn capture_cold_body(src: &str) -> String {
    let start = src
        .find("    fn capture_cold(")
        .expect("MeshNode::capture_cold not found — this guard is anchored on it");
    let rest = &src[start..];
    let end = rest
        .find("\n    }\n")
        .expect("capture_cold's closing brace not found");
    rest[..end].to_string()
}

/// F1 (independent review): the cold capture takes the scoped store EXACTLY ONCE,
/// through the instrumented acquisition, with BOTH plane queries inside it.
///
/// The runtime witness
/// `one_cold_capture_acquisition_spans_the_owner_and_grant_queries` compares the
/// section identity across the owner plane and a real grant query, which kills a
/// split that reacquires through `lock_cold_section`. This is the other half: a
/// split that BYPASSES the helper with a bare `scoped_discovery.lock()` would
/// stamp no new identity, so only a structural check can see it.
///
/// Non-vacuous by construction: the function must be found, and both production
/// plane queries must appear inside the section, or the assertions below cannot
/// pass.
#[test]
fn the_cold_capture_holds_exactly_one_store_acquisition() {
    let body = capture_cold_body(&read(&crate_root().join("src/adapter/net/mesh.rs")));
    let instrumented = body.matches("self.lock_cold_section()").count();
    assert_eq!(
        instrumented, 1,
        "capture_cold must open EXACTLY ONE store section, through \
         lock_cold_section; found {instrumented}"
    );
    let bare = body.matches("scoped_discovery.lock()").count();
    assert_eq!(
        bare, 0,
        "capture_cold must not take the scoped store outside lock_cold_section: \
         a bare acquisition stamps no section identity, so the runtime witness \
         could not see the split ({bare} found)"
    );
    // The anchors that make the two assertions above mean something.
    for query in [
        "find_owner_private_providers(",
        "find_capabilities_for_grant(",
    ] {
        assert!(
            body.contains(query),
            "capture_cold must contain the production `{query}` call inside its \
             single section, or this guard is vacuous"
        );
    }
    let section = body
        .find("self.lock_cold_section()")
        .expect("checked above");
    for query in [
        "find_owner_private_providers(",
        "find_capabilities_for_grant(",
    ] {
        let at = body.find(query).expect("checked above");
        assert!(
            at > section,
            "`{query}` must run AFTER the section is opened, inside it"
        );
    }
}

/// F2 (independent review): the proof intent is constructed only AFTER the final
/// currentness comparison, on both cold-plan paths.
///
/// The runtime evidence is the thread-local intent counter (see
/// `a_superseded_private_attempt_constructs_no_intent`). This is the structural
/// leg: in each attempt, the `intent_for` call must appear after the
/// `org_cold_authority_is_current` call. It exists because the first repair
/// satisfied every behavioural assertion while minting BEFORE the comparison —
/// the sequence was wrong, not the outcome.
#[test]
fn both_cold_plan_attempts_mint_after_the_comparison() {
    let src = read(&crate_root().join("sdk/src/org/call.rs"));
    for attempt in ["fn plan_attempt(", "fn plan_exported_attempt("] {
        let start = src
            .find(attempt)
            .unwrap_or_else(|| panic!("{attempt} not found — this guard is anchored on it"));
        let rest = &src[start..];
        let end = rest
            .find("\n    }\n")
            .unwrap_or_else(|| panic!("{attempt} closing brace not found"));
        let body = &rest[..end];
        let compare = body
            .find("org_cold_authority_is_current")
            .unwrap_or_else(|| panic!("{attempt} does not compare currentness at all"));
        let mint = body
            .find("self.intent_for(")
            .unwrap_or_else(|| panic!("{attempt} never constructs the intent"));
        assert!(
            mint > compare,
            "{attempt} constructs the proof intent BEFORE the final currentness \
             comparison; §10's sequence is select -> compare -> mint"
        );
        assert!(
            body.contains("select_candidate("),
            "{attempt} must select through the shared rule, or the ordering above \
             is about the wrong code"
        );
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}
