//! Heartbeat-unification drift check (moved here in Stage 2).

//! Tripwire for the heartbeat-unification invariant: every
//! production-side caller in `mod.rs` and `mesh.rs` that
//! constructs a heartbeat must go through
//! `NetSession::build_heartbeat`. See
//! `docs/internal/plans/HEARTBEAT_UNIFICATION_PLAN.md` Step 4.
//!
//! The module lives in the core, not beside `NetSession`: Stage 2
//! moved `session.rs` into `net-mesh-wire`, and the files this
//! tripwire scans (`mod.rs`, `mesh.rs`) are core files. It reads the
//! wire crate's `session.rs` through `CARGO_MANIFEST_DIR` so a
//! rename of the helper itself is caught too.
//!
//! `PacketBuilder::new` used to be `pub(crate)`, which made external
//! callers unrepresentable; the crate split cannot express that
//! ceiling, so this tripwire is now the whole enforcement. Within the
//! workspace a contributor could legitimately add a new production
//! caller that reaches into the pool directly
//! (`session.thread_local_pool().get().build_heartbeat()`)
//! and bypass the session helper — that pattern was the bug
//! shape behind #97/#106. This test counts the approved
//! production call sites and fails if a new one appears
//! without an explicit allowlist update, forcing the
//! contributor to confirm the design choice.
//!
//! The test scans only the *production* prefixes of each
//! file (everything before the first column-0
//! `#[cfg(test)]`), which excludes the test modules where
//! `PacketBuilder::new(&keys.tx_key, ...)` is the canonical
//! way to build a heartbeat for a manually-constructed
//! peer session.

/// Everything before the first column-0 `#[cfg(test)] mod`.
///
/// Top-level test modules are tagged with a column-0 `#[cfg(test)]`
/// immediately followed by `mod`. Nested `#[cfg(test)]` mods (indented
/// inside an `impl` or inline `mod` block) are deliberately NOT cut here, so
/// production code following a nested test mod is still checked. False
/// positives from nested-test-mod content are unlikely because none of the
/// nested test mods in this codebase reference `build_heartbeat`.
///
/// **Scanned line by line, not by substring.** This used to search for the
/// literal `"\n#[cfg(test)]\nmod "`, which silently fails on CRLF: the
/// needle never matches, the whole file is treated as production, and the
/// allowlist assertion then reports every TEST caller as a drifted
/// production one. That is a confusing failure a long way from its cause —
/// it cost a real debugging detour during OLB-2B.3c-pre, where an editor
/// rewrote `mesh.rs` with CRLF and this guard blamed eight test call sites.
/// `str::lines` strips a trailing `\r`, so a line-based scan cannot regress
/// that way. Witnessed by `production_prefix_is_line_ending_agnostic`.
fn production_prefix(src: &str) -> String {
    let mut prefix = String::with_capacity(src.len());
    let mut lines = src.lines().peekable();
    while let Some(line) = lines.next() {
        // Column 0 for BOTH lines, matching the original substring form:
        // `line == "#[cfg(test)]"` rejects an indented attribute, and
        // `starts_with("mod ")` rejects an indented or re-exported module.
        let opens_test_mod =
            line == "#[cfg(test)]" && lines.peek().is_some_and(|next| next.starts_with("mod "));
        if opens_test_mod {
            break;
        }
        prefix.push_str(line);
        prefix.push('\n');
    }
    prefix
}

fn count_build_heartbeat_callers(src: &str) -> Vec<String> {
    src.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            // Skip comments / doc-comments.
            if trimmed.starts_with("//") {
                return false;
            }
            line.contains(".build_heartbeat(")
        })
        .map(|line| line.trim().to_string())
        .collect()
}

/// The prefix scan must not care about line endings.
///
/// This is the regression that actually happened. `production_prefix`
/// searched for the literal `"\n#[cfg(test)]\nmod "`; an editor rewrote
/// `mesh.rs` with CRLF during OLB-2B.3c-pre, the needle stopped matching,
/// the whole file was treated as production, and this guard reported eight
/// TEST call sites as drifted production callers. The real change was a
/// line ending, and the failure pointed at `build_heartbeat`.
///
/// A guard whose false-positive mode is that confusing has to prove it
/// cannot do that again. Both fixtures below carry the SAME code, so both
/// must yield the same single production caller.
#[test]
fn production_prefix_is_line_ending_agnostic() {
    const SRC: &str = "\
fn production() {
let a = session.build_heartbeat();
}

#[cfg(test)]
mod tests {
fn t() {
    let b = builder.build_heartbeat();
}
}
";
    let lf = production_prefix(SRC);
    let crlf = production_prefix(&SRC.replace('\n', "\r\n"));

    let expected = vec!["let a = session.build_heartbeat();".to_string()];
    assert_eq!(
        count_build_heartbeat_callers(&lf),
        expected,
        "LF: the test-module caller must be cut"
    );
    assert_eq!(
        count_build_heartbeat_callers(&crlf),
        expected,
        "CRLF: the same source with CRLF endings must cut the same test \
         module. Leaking `builder.build_heartbeat()` here means the prefix \
         scan is substring-based again, and the allowlist assertion will \
         blame test call sites for a line-ending change"
    );
}

/// The cut is column-0-only, in both endings.
///
/// Pinned because the line-based rewrite could easily have loosened it: a
/// `trim()` on either line would start cutting at NESTED `#[cfg(test)] mod`
/// blocks, silently shrinking the production surface this guard inspects.
/// That failure is invisible — the assertion just stops seeing callers.
#[test]
fn production_prefix_cuts_only_column_zero_test_mods() {
    const SRC: &str = "\
impl Thing {
    #[cfg(test)]
    mod nested {
        fn t() {}
    }
}

fn still_production() {
    let a = session.build_heartbeat();
}
";
    for (label, src) in [("LF", SRC.to_string()), ("CRLF", SRC.replace('\n', "\r\n"))] {
        let prod = production_prefix(&src);
        assert_eq!(
            count_build_heartbeat_callers(&prod),
            vec!["let a = session.build_heartbeat();".to_string()],
            "{label}: an INDENTED `#[cfg(test)] mod` must not cut the scan — \
             production code after a nested test mod is still checked"
        );
    }
}

#[test]
fn mod_rs_production_callers_match_allowlist() {
    let prod = production_prefix(include_str!("mod.rs"));
    let callers = count_build_heartbeat_callers(&prod);
    // The only approved production caller in mod.rs:
    //   `let packet = session.build_heartbeat();`
    // inside `spawn_heartbeat`. Pre-fix this read
    //   `let packet = pooled.build_heartbeat();`
    // — that pattern is the regression we want to catch.
    let approved = ["let packet = session.build_heartbeat();"];
    assert_eq!(
        callers,
        approved.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        "mod.rs production callers of `.build_heartbeat()` drifted from the \
         approved allowlist. If you intentionally added a new caller, route it \
         through `Session::build_heartbeat` and update this allowlist. \
         See docs/internal/plans/HEARTBEAT_UNIFICATION_PLAN.md."
    );
}

#[test]
fn mesh_rs_production_callers_match_allowlist() {
    let prod = production_prefix(include_str!("mesh.rs"));
    let callers = count_build_heartbeat_callers(&prod);
    let approved = ["let packet = session.build_heartbeat();"];
    assert_eq!(
        callers,
        approved.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        "mesh.rs production callers of `.build_heartbeat()` drifted from the \
         approved allowlist. If you intentionally added a new caller, route it \
         through `Session::build_heartbeat` and update this allowlist. \
         See docs/internal/plans/HEARTBEAT_UNIFICATION_PLAN.md."
    );
}

/// The wire crate still owns the helper the allowlist points at.
///
/// `mod.rs` / `mesh.rs` are scanned above for *callers*; this reads
/// the callee. After Stage 2 the definition lives one crate away, so
/// `include_str!` cannot reach it — the path is resolved from
/// `CARGO_MANIFEST_DIR` instead. A rename or removal of
/// `build_heartbeat` in `net-mesh-wire` would otherwise leave the
/// allowlist asserting about a method that no longer exists, and the
/// tripwire would pass vacuously.
///
/// **Repository-only, and says so.** `wire/src/session.rs` is a
/// sibling path in this workspace; for a consumer who took
/// `net-mesh` from the registry, the wire crate is a versioned
/// dependency unpacked somewhere else entirely and the path does not
/// exist. Rather than fail there (a guard that breaks other people's
/// builds) or pass silently (a guard that is inert exactly where no
/// one is looking), the test detects a checkout by the workspace
/// root's `.git` and **prints why it skipped** otherwise. CI is
/// always a checkout, so the guard never goes vacuous where it
/// matters.
#[test]
fn wire_session_still_defines_the_heartbeat_helper() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = manifest_dir.join("wire").join("src").join("session.rs");

    // `.git` is a directory in a normal clone and a file in a
    // worktree or submodule; `exists()` covers both.
    let in_checkout = manifest_dir
        .ancestors()
        .any(|dir| dir.join(".git").exists());
    if !in_checkout {
        println!(
            "SKIPPED wire_session_still_defines_the_heartbeat_helper: no repository \
             checkout above {manifest_dir:?}, so the sibling path {path:?} is not the \
             wire crate this build links (a registry dependency is unpacked elsewhere). \
             This guard is enforced in CI, which is always a checkout."
        );
        return;
    }

    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "this IS a repository checkout, so net-mesh-wire's session.rs must be \
             readable at {path:?}: {e}"
        )
    });
    assert!(
        src.contains("pub fn build_heartbeat(&self)"),
        "net-mesh-wire's NetSession must still define `build_heartbeat`; the \
         allowlist in this module names it as the ONLY approved way to \
         construct a production heartbeat."
    );
}

/// Negative witness: the scan actually fails on a planted call site.
///
/// Cardinality tests that only ever see compliant input pass whether
/// or not the detector works. This plants an unapproved caller into
/// the real `mesh.rs` production prefix and asserts the comparison
/// the two allowlist tests perform would reject it.
#[test]
fn a_planted_production_caller_breaks_the_allowlist() {
    let approved = ["let packet = session.build_heartbeat();"];

    let clean = production_prefix(include_str!("mesh.rs"));
    let clean_callers = count_build_heartbeat_callers(&clean);
    assert_eq!(
        clean_callers,
        approved.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        "precondition: the unplanted scan matches the allowlist"
    );

    // The bug shape behind #97/#106: a production caller that reaches
    // into the pool instead of the session helper.
    let planted = format!(
        "{clean}\nfn smuggled_heartbeat(session: &NetSession) {{\n    \
         let packet = session.thread_local_pool().get().build_heartbeat();\n}}\n"
    );
    let planted_callers = count_build_heartbeat_callers(&production_prefix(&planted));
    assert_eq!(
        planted_callers.len(),
        clean_callers.len() + 1,
        "the scan must see the planted caller"
    );
    assert_ne!(
        planted_callers,
        approved.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        "regression: a production `.build_heartbeat()` call site outside the \
         allowlist must make this comparison fail — if it does not, the \
         tripwire is inert and #97/#106 can come back unnoticed."
    );
}
