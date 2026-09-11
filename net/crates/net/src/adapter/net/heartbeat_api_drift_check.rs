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
/// sibling path in THIS workspace; for a consumer who took
/// `net-mesh` from the registry, the wire crate is a versioned
/// dependency unpacked somewhere else entirely and the path does not
/// exist. Rather than fail there (a guard that breaks other people's
/// builds) or pass silently (a guard that is inert exactly where no
/// one is looking), the test detects the Net workspace by its
/// **layout** and prints why it skipped otherwise.
///
/// "Is there a `.git` somewhere above me" is not that test: an
/// unpacked package sitting under a consumer's own repository
/// (`<consumer>/target/package/net-mesh-0.36.0`) answers yes, and
/// then the guard demands a `wire/` that a packaged crate cannot
/// have. [`is_net_workspace`] asks for the three markers only the
/// real checkout carries.
#[test]
fn wire_session_still_defines_the_heartbeat_helper() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = manifest_dir.join("wire").join("src").join("session.rs");

    if !is_net_workspace(manifest_dir) {
        println!(
            "SKIPPED wire_session_still_defines_the_heartbeat_helper: {manifest_dir:?} \
             is not the Net workspace root (no `wire/Cargo.toml` member declared \
             beside a `net-mesh-wire = {{ path = \"wire\" }}` dependency), so the \
             sibling path {path:?} is not the wire crate this build links — a \
             registry dependency is unpacked elsewhere. This guard is enforced in \
             CI, which always builds from a checkout."
        );
        return;
    }

    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "this IS the Net workspace, so net-mesh-wire's session.rs must be \
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

/// Is `manifest_dir` the Net workspace root, with the wire crate as a
/// path member?
///
/// Three markers, all of which a `cargo package` tarball loses and a
/// foreign repository never has:
///
/// 1. `wire/Cargo.toml` exists — the sibling crate is present as
///    source, not as a registry dependency;
/// 2. this crate's own manifest declares the workspace and lists
///    `"wire"` as a member;
/// 3. …and depends on it **by path**, so the `wire/src/session.rs`
///    read below is the code this build actually links.
///
/// Pure so it can be witnessed directly: see
/// `net_workspace_detection_needs_every_layout_marker`.
fn is_net_workspace(manifest_dir: &std::path::Path) -> bool {
    if !manifest_dir.join("wire").join("Cargo.toml").is_file() {
        return false;
    }
    let Ok(manifest) = std::fs::read_to_string(manifest_dir.join("Cargo.toml")) else {
        return false;
    };
    let declares_member = manifest.contains("members = [") && manifest.contains("\"wire\",");
    let depends_by_path =
        manifest.contains("net-mesh-wire = { version") && manifest.contains("path = \"wire\"");
    declares_member && depends_by_path
}

/// The classification itself, over synthetic trees.
///
/// This is the part Kyra's three placements exercise from the
/// outside; here it is pinned without depending on where the test
/// itself happens to be compiled. The positive case is a **controlled
/// complete fixture** — a temp tree carrying all three markers — not
/// this crate's own `CARGO_MANIFEST_DIR`: an earlier revision asserted
/// `is_net_workspace(env!("CARGO_MANIFEST_DIR"))` unconditionally, so a
/// packaged core's test binary failed here instead of at the callee
/// test it had just made package-safe (Kyra, closure review of
/// `bdcd47125`).
///
/// The real checkout is still verified where it matters: under CI
/// (`CI=true`, which every GitHub job sets) the manifest directory
/// MUST classify as the workspace, so the callee guard cannot skip
/// itself into uselessness on the one machine that enforces it. A
/// packaged build never runs with `CI` set by this repository, and if
/// a consumer's CI does, the message says exactly which assumption
/// broke.
///
/// The interesting negative is the third: a `.git` above the crate is
/// exactly what an unpacked package under a consumer's repository has,
/// and it must NOT read as the Net workspace.
#[test]
fn net_workspace_detection_needs_every_layout_marker() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path();

    // A packaged crate: manifest, sources, no `wire/` member.
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"net-mesh\"\n\n[dependencies]\nnet-mesh-wire = { version = \"0.36.0\" }\n",
    )
    .expect("write manifest");
    assert!(
        !is_net_workspace(root),
        "an unpacked package has no wire/ member and must not be mistaken for the \
         workspace"
    );

    // …and the same tree beneath a Git checkout — a consumer's repo
    // with our package under `target/package/`. The old `.git`-
    // ancestor test said yes here and then demanded `wire/src`.
    std::fs::create_dir_all(root.join(".git")).expect("fake .git");
    assert!(
        !is_net_workspace(root),
        "a Git ancestor is not evidence of the NET workspace: an unpacked package \
         under a consumer's repository has one"
    );

    // A tree that has `wire/Cargo.toml` but neither manifest marker
    // is still not us (someone else's `wire` crate).
    std::fs::create_dir_all(root.join("wire")).expect("wire dir");
    std::fs::write(
        root.join("wire").join("Cargo.toml"),
        "[package]\nname = \"wire\"\n",
    )
    .expect("write wire manifest");
    assert!(
        !is_net_workspace(root),
        "a sibling directory named `wire` is not the Net layout without the member \
         and path-dependency declarations"
    );

    // The member declaration alone is not enough either: the path
    // dependency is what ties `wire/src/session.rs` to the code this
    // build links.
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\n    \".\",\n    \"wire\",\n]\n\n[package]\nname = \"net-mesh\"\n\n\
         [dependencies]\nnet-mesh-wire = { version = \"0.36.0\" }\n",
    )
    .expect("write member-only manifest");
    assert!(
        !is_net_workspace(root),
        "a workspace member entry without a path dependency is not the Net layout"
    );

    // The controlled positive: every marker present, and nothing else
    // about the location matters (it is a temp dir under a fake `.git`).
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\n    \".\",\n    \"wire\",\n]\n\n[package]\nname = \"net-mesh\"\n\n\
         [dependencies]\nnet-mesh-wire = { version = \"0.36.0\", path = \"wire\" }\n",
    )
    .expect("write complete manifest");
    assert!(
        is_net_workspace(root),
        "a tree with wire/Cargo.toml, the workspace member entry and the path \
         dependency IS the Net layout, wherever it sits"
    );

    // Where the guard is enforced, the real checkout must classify —
    // otherwise the callee test skips on the only machine that runs
    // it. Outside CI (a developer's packaged build, a consumer's
    // `cargo test` on the registry crate) this is not asserted, and
    // the callee test's printed skip is the behaviour.
    if std::env::var_os("CI").is_some() {
        let real = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(
            is_net_workspace(real),
            "CI is set, so this must be the Net checkout, but {real:?} does not \
             classify as the workspace: the callee drift guard would skip itself \
             into uselessness here"
        );
    }
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
